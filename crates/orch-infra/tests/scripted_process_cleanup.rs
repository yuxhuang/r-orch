//! Fixture lifetime is the cleanup boundary, including outside a Tokio runtime.
mod support;

use std::collections::BTreeMap;
use std::os::unix::{fs::PermissionsExt, net::UnixDatagram, process::CommandExt};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use nix::sys::signal::{Signal as OsSignal, killpg};
use nix::unistd::{Pid, getpgid};
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild, StdioMode};
use support::{Script, ScriptedProcessManager};

/// Independently owned safety net, including when the fixture cleanup is
/// removed for the negative control. Never reap this member before signalling
/// its group: its membership pins group identity even if the leader exited.
struct GroupGuard {
    member: Child,
    pgid: Pid,
}

impl GroupGuard {
    fn new(pgid: Option<Pid>) -> Self {
        let mut command = Command::new("/bin/cat");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(pgid.map_or(0, Pid::as_raw));
        let member = command.spawn().expect("owned group member");
        let pgid = pgid.unwrap_or_else(|| Pid::from_raw(member.id().try_into().unwrap()));
        assert_ne!(pgid, nix::unistd::getpgrp());
        Self { member, pgid }
    }

    fn evidence(&self, label: &str) -> Vec<(u32, String)> {
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,pgid=,stat=,comm="])
            .output()
            .expect("process group evidence");
        assert!(output.status.success());
        let rows: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.get(2)?.parse::<i32>().ok()? == self.pgid.as_raw())
                    .then(|| (fields[0].parse().unwrap(), line.to_owned()))
            })
            .collect();
        eprintln!("{label} pgid={} rows={rows:?}", self.pgid);
        rows
    }

    fn assert_alive(&self) {
        let rows = self.evidence("independent control alive");
        assert!(rows.iter().any(|(pid, row)| {
            *pid == self.member.id() && !row.split_whitespace().nth(3).unwrap().starts_with('Z')
        }));
    }

    fn assert_terminated(&mut self, shell: u32, descendant: u32) {
        // If cleanup worked, this wait is immediately ready. Avoid waiting on
        // the negative path: the independent guard must clean up before panic.
        let rows = self.evidence("after fixture drop, before safety-net cleanup");
        let live: Vec<_> = rows
            .iter()
            .filter(|(_, row)| !row.split_whitespace().nth(3).unwrap().starts_with('Z'))
            .collect();
        let terminated = live.is_empty();
        if !terminated {
            self.cleanup();
        }
        assert!(
            terminated,
            "fixture leaked shell {shell} / descendant {descendant}: {live:?}"
        );
    }

    fn cleanup(&mut self) {
        // No try_wait: reaping the last member before killpg would lose the
        // identity guarantee. Cleanup is once only, even during unwinding.
        if self.pgid.as_raw() > 0 {
            let _ = killpg(self.pgid, OsSignal::SIGKILL);
            let _ = self.member.wait();
            eprintln!(
                "safety-net reaped member={} group={}",
                self.member.id(),
                self.pgid
            );
            self.pgid = Pid::from_raw(0);
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct Hanging {
    pm: Arc<ScriptedProcessManager>,
    child: SpawnedChild,
    group: GroupGuard,
    descendant: u32,
    _dir: tempfile::TempDir,
}

fn hanging() -> Hanging {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready.sock");
    let socket = UnixDatagram::bind(&ready).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let sleep = dir.path().join("sleep");
    // The notification is a readiness barrier, not a timing sleep. exec keeps
    // the descendant PID stable until fixture/safety-net cleanup terminates it.
    std::fs::write(&sleep, "#!/usr/bin/python3\nimport os, socket\ns = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)\ns.sendto(str(os.getpid()).encode(), os.environ['FIXTURE_READY'])\nos.execv('/bin/sleep', ['sleep', '600'])\n").unwrap();
    std::fs::set_permissions(&sleep, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin().hang());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let child = {
        let _entered = runtime.enter();
        pm.spawn(
            "fixture-only",
            &[],
            SpawnOptions {
                stdin: StdioMode::Pipe,
                env: BTreeMap::from([
                    (
                        "PATH".into(),
                        format!("{}:/usr/bin:/bin", dir.path().display()),
                    ),
                    ("FIXTURE_READY".into(), ready.display().to_string()),
                    ("HOME".into(), dir.path().display().to_string()),
                ]),
                ..SpawnOptions::default()
            },
        )
        .unwrap()
    };
    // Install the independent safety net before any readiness assertion.
    let pgid = getpgid(Some(Pid::from_raw(child.pid.try_into().unwrap()))).unwrap();
    let group = GroupGuard::new(Some(pgid));
    let mut bytes = [0; 32];
    let len = socket.recv(&mut bytes).expect("descendant readiness");
    let descendant = std::str::from_utf8(&bytes[..len]).unwrap().parse().unwrap();
    let before = group.evidence("before fixture drop");
    assert!(before.iter().any(|(pid, _)| *pid == child.pid));
    assert!(before.iter().any(|(pid, _)| *pid == descendant));
    drop(runtime);
    Hanging {
        pm,
        child,
        group,
        descendant,
        _dir: dir,
    }
}

#[test]
fn final_owner_drop_terminates_shell_and_descendant_without_a_runtime() {
    let mut control = GroupGuard::new(None);
    let Hanging {
        pm,
        child,
        mut group,
        descendant,
        _dir,
    } = hanging();
    let clone = Arc::clone(&pm);
    drop(pm);
    let rows = group.evidence("non-final owner dropped");
    assert!(rows.iter().any(|(pid, _)| *pid == child.pid));
    drop(clone);
    group.assert_terminated(child.pid, descendant);
    control.assert_alive();
    control.cleanup();
}

#[test]
fn unwinding_the_last_owner_terminates_its_group() {
    let Hanging {
        pm,
        child,
        mut group,
        descendant,
        _dir,
    } = hanging();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owner = pm;
        panic!("deliberate fixture-owner unwind");
    }));
    assert!(result.is_err());
    group.assert_terminated(child.pid, descendant);
}

#[test]
fn early_return_releases_the_group() {
    fn return_early(pm: Arc<ScriptedProcessManager>) -> Result<(), ()> {
        let _owner = pm;
        Err(())?;
        Ok(())
    }
    let Hanging {
        pm,
        child,
        mut group,
        descendant,
        _dir,
    } = hanging();
    assert_eq!(return_early(pm), Err(()));
    group.assert_terminated(child.pid, descendant);
}

#[test]
fn explicit_kill_then_final_drop_is_idempotent_and_records_the_kill() {
    let Hanging {
        pm,
        child,
        mut group,
        descendant,
        _dir,
    } = hanging();
    pm.kill(child.pid, Signal::Kill);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let exit = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), child.exit.wait())
            .await
            .unwrap()
    });
    assert_eq!(exit.signal, Some(9));
    pm.kill(child.pid, Signal::Kill);
    assert_eq!(pm.killed(), vec![(child.pid, Signal::Kill); 2]);
    drop(runtime);
    drop(pm);
    group.assert_terminated(child.pid, descendant);
}

#[test]
fn an_exited_script_preserves_output_exit_and_unrelated_control() {
    use tokio::io::AsyncReadExt;
    let control = GroupGuard::new(None);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pm = ScriptedProcessManager::new(Script::new().stdout("scripted output").exit(7));
    let (pid, output, exit) = runtime.block_on(async {
        let mut child = pm
            .spawn("fixture-only", &[], SpawnOptions::default())
            .unwrap();
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .await
            .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(3), child.exit.wait())
            .await
            .unwrap();
        (child.pid, output, exit)
    });
    assert_eq!(
        (output.as_str(), exit.code, pm.is_alive(pid)),
        ("scripted output", Some(7), false)
    );
    // This recorded script PID is already reaped. Signalling it must still
    // target only the retained fixture group, never a recycled bare PID.
    pm.kill(pid, Signal::Kill);
    drop(runtime);
    drop(pm);
    control.assert_alive();
}

#[test]
fn full_stderr_does_not_delay_final_owner_cleanup() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;

    const CHILD_SOCKET: &str = "FIXTURE_DROP_BACKPRESSURE_SOCKET";
    if let Some(path) = std::env::var_os(CHILD_SOCKET) {
        let Hanging {
            pm,
            child,
            group,
            descendant,
            _dir,
        } = hanging();
        let mut connection = UnixStream::connect(path).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(connection, "{} {} {}", child.pid, group.pgid, descendant).unwrap();
        let mut command = [0];
        connection.read_exact(&mut command).unwrap();
        assert_eq!(command, *b"G");
        drop(pm);
        writeln!(connection, "DONE").unwrap();
        // Keep the helper and its independent safety net until the supervisor
        // has recorded termination/control survival BEFORE draining stderr.
        BufReader::new(connection)
            .read_until(b'X', &mut Vec::new())
            .unwrap();
        drop(group);
        return;
    }

    // Isolate fd 2 in a helper process; never redirect the test runner's global
    // stderr. Python is already used by this target's readiness fixture.
    let dir = tempfile::tempdir().unwrap();
    let result = Command::new("/usr/bin/python3")
        .args(["-c", FULL_STDERR_SUPERVISOR])
        .arg(std::env::current_exe().unwrap())
        .arg(dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.path())
        .env("TMPDIR", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            dir.path().join("global.yml"),
        )
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&result.stdout));
    assert!(
        result.status.success(),
        "full-stderr supervisor failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

// The pipe is filled to EAGAIN only after READY, then returned to blocking mode.
// No reader drains it until DONE and the process-table/control observations.
// Independently owned unreaped cats pin fixture/control PGIDs before assertions;
// the finally block cleans/reaps them even in the deliberately blocked mutant.
const FULL_STDERR_SUPERVISOR: &str = r#"
import json, os, signal, socket, subprocess, sys
from pathlib import Path
root = Path(sys.argv[2])
env = dict(os.environ, FIXTURE_DROP_BACKPRESSURE_SOCKET=str(root / 'drop.sock'))
control = guard = helper = connection = None
rd, wr = os.pipe()
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(env['FIXTURE_DROP_BACKPRESSURE_SOCKET'])
server.listen(1)
server.settimeout(3)
event = {}

def rows(groups):
    text = subprocess.check_output(['/bin/ps', '-axo', 'pid=,ppid=,pgid=,stat=,comm='], env=env, text=True, timeout=3)
    return [line for line in text.splitlines() if len(line.split()) >= 4 and int(line.split()[2]) in groups]

def live(lines):
    return [line for line in lines if not line.split()[3].startswith('Z')]

def cat(group=0):
    # This supervisor has no threads; setpgid runs before exec on Python 3.9 too.
    return subprocess.Popen(['/bin/cat'], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env, preexec_fn=lambda: os.setpgid(0, group))

def kill_owned(member, group):
    # No poll/try_wait before the final group signal: preserve pinned authority.
    try:
        os.killpg(group, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except PermissionError:
        # macOS can refuse repeat killpg when only dead members remain.
        if live(rows({group})):
            raise
    member.wait(timeout=3)

try:
    control = cat()
    helper = subprocess.Popen([sys.argv[1], '--exact', 'full_stderr_does_not_delay_final_owner_cleanup', '--nocapture'], stdout=subprocess.DEVNULL, stderr=wr, env=env, preexec_fn=lambda: os.setpgid(0, 0))
    connection, _ = server.accept()
    connection.settimeout(3)
    reader = connection.makefile('rb', buffering=0)
    shell, pgid, descendant = map(int, reader.readline().split())
    guard = cat(pgid)
    event.update(shell=shell, pgid=pgid, descendant=descendant, guard=guard.pid, control=control.pid, helper=helper.pid)
    event['before'] = rows({pgid, control.pid})
    os.set_blocking(wr, False)
    filled = 0
    # Fill any sub-PIPE_BUF remainder too: a refused atomic 4096-byte write
    # alone does not prove there is no room for the short Drop diagnostic.
    for size in (4096, 1):
        try:
            while True:
                filled += os.write(wr, b'X' * size)
        except BlockingIOError:
            pass
    os.set_blocking(wr, True)
    event['prefilled_stderr_bytes'] = filled
    connection.sendall(b'G')
    try:
        event['completion_before_drain'] = reader.readline().decode().strip()
    except socket.timeout:
        event['completion_before_drain'] = 'TIMED_OUT'
    event['fixture_before_drain'] = rows({pgid})
    event['control_before_drain'] = rows({control.pid})
finally:
    # Cleanup precedes all verdict assertions, including the causal RED case.
    if guard is not None:
        kill_owned(guard, pgid)
    os.set_blocking(rd, False)
    drained = 0
    try:
        while True:
            data = os.read(rd, 65536)
            if not data:
                break
            drained += len(data)
    except BlockingIOError:
        pass
    event['drained_after_observation'] = drained
    if connection is not None:
        connection.sendall(b'X')
    if helper is not None:
        try:
            event['helper_exit'] = helper.wait(timeout=3)
        except subprocess.TimeoutExpired:
            kill_owned(helper, helper.pid)
            raise
    if control is not None:
        kill_owned(control, control.pid)
    event['final_owned_rows'] = rows({event[k] for k in ('pgid', 'control', 'helper') if k in event})
    os.close(rd)
    os.close(wr)
    if connection is not None:
        connection.close()
    server.close()
    print(json.dumps(event, sort_keys=True), flush=True)

assert event['prefilled_stderr_bytes'] > 0, event
assert event['completion_before_drain'] == 'DONE', event
assert not live(event['fixture_before_drain']), event
assert any(int(line.split()[0]) == event['control'] for line in live(event['control_before_drain'])), event
assert event['helper_exit'] == 0, event
assert not event['final_owned_rows'], event
"#;
