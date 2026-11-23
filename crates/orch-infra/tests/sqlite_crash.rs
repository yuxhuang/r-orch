//! Process-crash checks of the production writer, using only synthetic private data.
//! These cover statement/body-return/receipt boundaries, not an interruption inside
//! SQLite COMMIT or power loss. Writer does not expose a mutable commit-hook seam.
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{ConnectionRole, Lane, Synchronous, Writer, open_connection};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(10);
type EntityRows = (Vec<(i64, String)>, Vec<(i64, String)>);

fn acquire(path: &Path) -> DataDir {
    DataDir::acquire(
        path,
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap()
}

struct OwnedChild(Child);

impl OwnedChild {
    fn kill_and_reap(&mut self) -> io::Result<ExitStatus> {
        if self.0.try_wait()?.is_none() {
            // Child::kill targets this spawned child only, never a process group
            // or a pid read from disk/environment.
            self.0.kill()?;
        }
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "test child did not exit after kill",
                ));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = self.kill_and_reap() {
            if std::thread::panicking() {
                // Even stderr can fail. Cleanup must not replace the first panic
                // with a second panic and abort the entire integration binary.
                let _ = writeln!(io::stderr(), "crash child cleanup failed: {error}");
            } else {
                panic!("crash child cleanup failed: {error}");
            }
        }
    }
}

fn pause(stream: &mut UnixStream, boundary: u8) {
    stream.write_all(&[boundary]).unwrap();
    let mut release = [0];
    // Parent kills us while blocked here. Timeout/EOF must fail, never allow a
    // delayed parent to mistake a later boundary for the requested crash point.
    stream.read_exact(&mut release).unwrap();
    panic!("crash child unexpectedly released");
}

#[test]
#[ignore = "helper subprocess, invoked explicitly by the crash tests"]
fn writer_crash_child() {
    let root = std::env::var_os("WRITER_CRASH_ROOT").expect("child root missing");
    let boundary = std::env::var("WRITER_CRASH_BOUNDARY").unwrap();
    // Only this helper inherits the private socket endpoint as stdin. Clone the
    // descriptor safely; no global cwd change or pathname-length limit is needed.
    let mut stream = UnixStream::from(io::stdin().as_fd().try_clone_to_owned().unwrap());
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.set_write_timeout(Some(LIMIT)).unwrap();
    let data = acquire(&Path::new(&root).join("data"));
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 4).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| writer.run());
        let receipt = handle
            .submit(Lane::Entities, None, move |tx| {
                tx.execute("INSERT INTO first_entity VALUES(1, 'new-first')", [])?;
                if boundary == "between_statements" {
                    pause(&mut stream, b'S');
                }
                tx.execute("INSERT INTO second_entity VALUES(1, 'new-second')", [])?;
                if boundary == "body_complete" {
                    // Last observable pre-commit boundary: both statements ran,
                    // but this callback has not returned to Writer::submit's
                    // transaction.commit(). No transaction-control SQL is used.
                    pause(&mut stream, b'B');
                }
                Ok(stream)
            })
            .unwrap();
        let mut stream = receipt.wait().unwrap();
        // Production writer sends a successful receipt only after commit returns.
        // Keep writer/connection alive until the abrupt process kill.
        pause(&mut stream, b'C');
        handle.close();
    });
}

fn crash_at(boundary: &str, marker: u8) -> EntityRows {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    {
        let data = acquire(&path);
        let (writer, handle) = Writer::open(&data, Synchronous::Normal, 4).unwrap();
        let setup = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute_batch(
                    "CREATE TABLE first_entity(id INTEGER PRIMARY KEY, value TEXT) STRICT;
                     CREATE TABLE second_entity(id INTEGER PRIMARY KEY, value TEXT) STRICT;
                     INSERT INTO first_entity VALUES(0, 'control-first');
                     INSERT INTO second_entity VALUES(0, 'control-second');",
                )?;
                Ok(())
            })
            .unwrap();
        handle.close();
        writer.run();
        setup.wait().unwrap();
    }
    // An unnamed pair stays confined to these two processes even when TMPDIR
    // exceeds sockaddr_un's pathname limit. Readiness blocks with LIMIT instead
    // of polling for a connection during delayed child startup.
    let (mut stream, child_stream) = UnixStream::pair().unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "writer_crash_child", "--ignored", "--nocapture"])
            .env_clear()
            .env("HOME", temp.path())
            .env("TMPDIR", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                temp.path().join("global.yml"),
            )
            .env("ORCH_DATA_HOME", temp.path().join("daemons"))
            .env("WRITER_CRASH_ROOT", temp.path())
            .env("WRITER_CRASH_BOUNDARY", boundary)
            .current_dir(temp.path())
            .stdin(Stdio::from(OwnedFd::from(child_stream)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut observed = [0];
    stream.read_exact(&mut observed).unwrap();
    assert_eq!(observed, [marker], "wrong crash boundary");
    assert_eq!(
        child.kill_and_reap().unwrap().signal(),
        Some(9),
        "child was not killed"
    );
    // Reacquire the same private root after process death, then open its existing
    // database through T9. Check full row contents on both sides of the unit of
    // work: a count alone would miss corruption of the committed control.
    let data = acquire(&path);
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    let rows = |table: &str| {
        reader
            .prepare(&format!("SELECT id, value FROM {table} ORDER BY id"))
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    (rows("first_entity"), rows("second_entity"))
}

fn controls() -> EntityRows {
    (
        vec![(0, "control-first".into())],
        vec![(0, "control-second".into())],
    )
}

#[test]
fn kill_before_commit_keeps_nothing() {
    assert_eq!(crash_at("between_statements", b'S'), controls());
}

#[test]
fn kill_at_body_return_commit_boundary_keeps_nothing() {
    assert_eq!(crash_at("body_complete", b'B'), controls());
}

#[test]
fn kill_after_commit_receipt_keeps_all_changes() {
    let mut expected = controls();
    expected.0.push((1, "new-first".into()));
    expected.1.push((1, "new-second".into()));
    assert_eq!(crash_at("committed", b'C'), expected);
}
