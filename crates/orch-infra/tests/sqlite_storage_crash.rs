//! Public Storage process-crash checks on synthetic config/template rows.
//! Markers establish callback/receipt phases, never interruption inside COMMIT
//! or power-loss durability. A delayed parent can outlive the bounded child wait.
use orch_core::store::{
    Caller, ConfigEntry, Entity, Filter, Lane, PageRequest, Principal, Record, Revision, Storage,
    StorageExt, TemplateEntry,
};
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(10);

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

fn caller() -> Caller {
    Caller {
        project: "synthetic".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn config(id: &str) -> Entity {
    Entity::Config(ConfigEntry {
        key: id.into(),
        value: format!("config-{id}"),
    })
}
fn template(id: &str) -> Entity {
    Entity::Template(TemplateEntry {
        name: id.into(),
        body: format!("template-{id}"),
    })
}
fn open(path: &Path) -> SqliteStorage {
    SqliteStorage::open(Arc::new(acquire(path)), StorageOptions::default()).unwrap()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
async fn pair(storage: &SqliteStorage, id: &'static str) -> Vec<Record> {
    storage
        .transaction(caller(), Lane::Entities, move |u| {
            Ok(vec![u.create(config(id))?, u.create(template(id))?])
        })
        .await
        .unwrap()
}
async fn rows(storage: &SqliteStorage) -> Vec<Vec<Record>> {
    let view = storage.read(caller()).await.unwrap();
    let mut result = Vec::new();
    for filter in [Filter::Config, Filter::Templates] {
        let page = view.list(&filter, &PageRequest::default()).await.unwrap();
        assert!(page.next_cursor.is_none(), "fixture must fit in one page");
        result.push(page.items);
    }
    result
}

#[test]
#[ignore = "private subprocess invoked explicitly by public crash cases"]
fn storage_crash_child() {
    let root = std::env::var_os("STORAGE_CRASH_ROOT").expect("child root missing");
    let boundary = std::env::var("STORAGE_CRASH_BOUNDARY").unwrap();
    let mut stream = UnixStream::from(io::stdin().as_fd().try_clone_to_owned().unwrap());
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.set_write_timeout(Some(LIMIT)).unwrap();
    runtime().block_on(async move {
        let storage = open(&Path::new(&root).join("data"));
        let following = boundary == "following";
        let mut stream = storage
            .transaction(caller(), Lane::Entities, move |u| {
                u.create(config("1-committed"))?;
                if boundary == "first" {
                    pause(&mut stream, b'S');
                }
                u.create(template("1-committed"))?;
                if boundary == "body" {
                    pause(&mut stream, b'B');
                }
                Ok(stream)
            })
            .await
            .unwrap();
        if following {
            storage
                .transaction(caller(), Lane::Entities, move |u| {
                    u.create(config("2-unreceipted"))?;
                    u.create(template("2-unreceipted"))?;
                    pause(&mut stream, b'N');
                    Ok(())
                })
                .await
                .unwrap();
        } else {
            // StorageExt returns only after Storage::write acknowledges COMMIT.
            pause(&mut stream, b'C');
        }
    });
}

fn crash_at(boundary: &str, marker: u8, committed: bool) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    let rt = runtime();
    let controls = rt.block_on(async {
        let storage = open(&path);
        let controls = pair(&storage, "0-control").await;
        storage.close().await.unwrap();
        controls
    });
    let (mut stream, child_stream) = UnixStream::pair().unwrap();
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage_crash_child", "--ignored", "--nocapture"])
            .env_clear()
            .env("HOME", temp.path())
            .env("TMPDIR", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("XDG_RUNTIME_DIR", temp.path().join("run"))
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                temp.path().join("global.yml"),
            )
            .env("ORCH_DATA_HOME", temp.path().join("daemons"))
            .env("STORAGE_CRASH_ROOT", temp.path())
            .env("STORAGE_CRASH_BOUNDARY", boundary)
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
    assert_eq!(child.kill_and_reap().unwrap().signal(), Some(9));
    rt.block_on(async {
        let storage = open(&path);
        let mut expected: Vec<Vec<Record>> = controls.into_iter().map(|r| vec![r]).collect();
        if committed {
            for (family, entity) in [config("1-committed"), template("1-committed")]
                .into_iter()
                .enumerate()
            {
                let position = expected[family][0].position + 1;
                expected[family].push(Record {
                    entity,
                    revision: Revision::ONE,
                    position,
                });
            }
        }
        assert_eq!(rows(&storage).await, expected, "complete reopened records");
        let recovery = pair(&storage, "3-recovery").await;
        for (family, record) in recovery.into_iter().enumerate() {
            expected[family].push(record);
        }
        assert_eq!(
            rows(&storage).await,
            expected,
            "complete records after recovery receipt"
        );
        storage.close().await.unwrap();
    });
}

#[test]
fn public_kill_between_entity_writes_rolls_back_both_families() {
    crash_at("first", b'S', false);
}
#[test]
fn public_kill_before_callback_return_rolls_back_both_families() {
    crash_at("body", b'B', false);
}
#[test]
fn public_kill_after_receipt_preserves_both_families() {
    crash_at("receipt", b'C', true);
}
#[test]
fn public_kill_during_following_write_preserves_only_receipted_records() {
    crash_at("following", b'N', true);
}

#[test]
fn public_callback_panic_rolls_back_and_reopen_keeps_the_following_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    runtime().block_on(async {
        let storage = open(&path);
        let controls = pair(&storage, "0-control").await;
        let other = storage.clone();
        let failure = tokio::spawn(async move {
            other
                .transaction(
                    caller(),
                    Lane::Entities,
                    |u| -> orch_core::store::Result<()> {
                        u.create(config("2-panicked"))?;
                        u.create(template("2-panicked"))?;
                        panic!("synthetic public callback panic");
                    },
                )
                .await
        })
        .await
        .unwrap_err();
        assert!(failure.is_panic(), "caller must observe the callback panic");
        assert!(storage.failure_state().failure().is_none());
        let recovery = pair(&storage, "3-recovery").await;
        storage.close().await.unwrap();
        let reopened = open(&path);
        let expected: Vec<Vec<Record>> = controls
            .into_iter()
            .zip(recovery)
            .map(|(a, b)| vec![a, b])
            .collect();
        assert_eq!(rows(&reopened).await, expected);
        reopened.close().await.unwrap();
    });
}
