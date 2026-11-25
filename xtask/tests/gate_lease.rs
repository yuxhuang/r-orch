//! Lease processes use private lease directories and explicitly controlled workers.
//! No test signals a discovered PID or touches the cooperative host lease.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use witness::fixture::{
    CARGO_SUBCOMMAND_PY, COMMAND_BUDGET, HANDSHAKE_BUDGET, OwnedChild, POLL_INTERVAL, Sandbox,
    field, row, wait_for,
};

const PAYLOAD: &str = r#"#!/usr/bin/python3
# This is a bounded inert boundary worker, never an AI CLI or host workload.
import json, os, signal, socket, subprocess, sys, time
mode, address = sys.argv[1:3]
if mode in ('resistant', 'detached'):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
if mode == 'resistant':
    # INT must not run finally and kill the detached leaf on F's behalf.
    # Lifecycle execution stays held until independent cleanup is reviewed.
    signal.signal(signal.SIGINT, lambda *_: os._exit(130))
connection = socket.socket(socket.AF_UNIX)
connection.settimeout(30)
connection.connect(address)
if mode == 'resistant-direct':
    # No expiry and no automatic exit on a signal: only gate KILL can reap it.
    def forwarded(number, frame):
        connection.sendall((json.dumps({'forwarded': number}) + '\n').encode())
    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, forwarded)
reader = connection.makefile('rb')
descriptors = []
for fd in range(3, 1024):
    try: metadata = os.fstat(fd)
    except OSError: continue
    descriptors.append({'fd': fd, 'device': metadata.st_dev & ((1 << 64) - 1), 'inode': metadata.st_ino})
connection.sendall((json.dumps({'role': mode, 'pid': os.getpid(), 'stdin_inode': os.fstat(0).st_ino, 'stdin_device': os.fstat(0).st_dev & ((1 << 64) - 1), 'descriptors': descriptors, 'token': os.environ.get('XTASK_GATE_LEASE_TOKEN', '')}) + '\n').encode())
child = None
try:
    if mode == 'resistant':
        child = subprocess.Popen([sys.executable, __file__, 'detached', address], start_new_session=True)
    while True:
        deadline = time.monotonic() + 30
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            connection.sendall(b'{"expired":true}\n')
            break
        connection.settimeout(None if mode in ('holder', 'resistant-direct') else remaining)
        try:
            line = reader.readline(65536)
        except (TimeoutError, socket.timeout):
            connection.sendall(b'{"expired":true}\n')
            break
        if not line: break
        request = json.loads(line)
        if request['op'] == 'ping':
            connection.sendall((json.dumps({'alive': os.getpid()}) + '\n').encode())
            continue
        if request['op'] == 'release': break
        if request['op'] == 'gate':
            env = dict(os.environ)
            if request.get('token') == 'remove': env.pop('XTASK_GATE_LEASE_TOKEN', None)
            if request.get('token') == 'wrong': env['XTASK_GATE_LEASE_TOKEN'] = 'wrong-private-token'
            # The runtime bound is separate from handshake/idle budgets. A timeout
            # kills only this still-unreaped owned group, never a discovered PID.
            nested = subprocess.Popen(request['argv'], env=env, start_new_session=True)
            try:
                code = nested.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(nested.pid, signal.SIGKILL)
                nested.wait(timeout=2)
                code = 124
            connection.settimeout(2)
            connection.sendall((json.dumps({'exit': code}) + '\n').encode())
finally:
    if child is not None and child.poll() is None:
        child.kill()
        child.wait(timeout=2)
    connection.close()
"#;

// Observe the child itself: no fixed descriptor number and no marker reopen.
// Sandbox launches with /dev/null stdin; the Runner must preserve that input.
fn require_extra_marker_and_normal_stdin(peer: &Peer, marker: &Path) {
    let marker = fs::metadata(marker).unwrap();
    let matches: Vec<_> = peer.ready["descriptors"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fd| {
            fd["device"].as_u64() == Some(marker.dev())
                && fd["inode"].as_u64() == Some(marker.ino())
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one inherited extra marker: {}",
        peer.ready
    );
    let stdin = fs::metadata("/dev/null").unwrap();
    assert_eq!(peer.ready["stdin_device"].as_u64(), Some(stdin.dev()));
    assert_eq!(peer.ready["stdin_inode"].as_u64(), Some(stdin.ino()));
    assert_ne!((stdin.dev(), stdin.ino()), (marker.dev(), marker.ino()));
}

struct Boundary {
    listener: UnixListener,
    path: std::path::PathBuf,
}

struct Peer {
    input: BufReader<UnixStream>,
    output: UnixStream,
    ready: serde_json::Value,
}

impl Boundary {
    fn new(sandbox: &Sandbox) -> Self {
        let path = sandbox.temporary.join("s");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        sandbox.script(&sandbox.temporary.join("payload.py"), PAYLOAD);
        Self { listener, path }
    }
    fn accept(&self, holder: &mut OwnedChild) -> Peer {
        let deadline = Instant::now() + HANDSHAKE_BUDGET;
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = holder.child.try_wait().unwrap() {
                        panic!(
                            "lease payload handshake failed: holder exited with {status}; {}",
                            holder.diagnostic()
                        );
                    }
                    assert!(
                        Instant::now() < deadline,
                        "setup: payload never reached acquired handshake"
                    );
                    std::thread::park_timeout(POLL_INTERVAL);
                }
                Err(error) => panic!("setup: payload handshake: {error}"),
            }
        };
        // Darwin accepts inherit the listener nonblocking flag. Handshakes and
        // later responses use the bounded blocking stream contract below.
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(HANDSHAKE_BUDGET)).unwrap();
        stream.set_write_timeout(Some(HANDSHAKE_BUDGET)).unwrap();
        let mut peer = Peer {
            input: BufReader::new(stream.try_clone().unwrap()),
            output: stream,
            ready: serde_json::Value::Null,
        };
        peer.ready = peer.response();
        peer
    }
    fn exec(&self, sandbox: &Sandbox, mode: &str) -> OwnedChild {
        let mut command = sandbox.gate(&[
            "lease",
            "exec",
            "--purpose",
            "private-contract-holder",
            "--lease-wait",
            "0",
            "--",
            "/usr/bin/python3",
        ]);
        command
            .arg(sandbox.temporary.join("payload.py"))
            .arg(mode)
            .arg(&self.path);
        sandbox.spawn(&mut command)
    }
}

impl Peer {
    fn response(&mut self) -> serde_json::Value {
        let mut line = String::new();
        assert!(
            self.input.read_line(&mut line).unwrap() > 0,
            "worker closed before response"
        );
        serde_json::from_str(&line).unwrap()
    }
    fn request(&mut self, request: serde_json::Value) {
        writeln!(self.output, "{request}").unwrap();
        self.output.flush().unwrap();
    }
    fn alive(&mut self) {
        self.request(serde_json::json!({"op": "ping"}));
        assert_eq!(self.response()["alive"], self.ready["pid"]);
    }
    fn release(&mut self) {
        self.request(serde_json::json!({"op": "release"}));
    }
    fn nested(&mut self, token: &str, args: &[&str]) -> i64 {
        let argv: Vec<_> = [env!("CARGO_BIN_EXE_xtask"), "gate"]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        // Separate budgets: runtime 30 s + timeout cleanup 2 s + socket send
        // 2 s + scheduling allowance 1 s. This is observation, not a larger
        // miniature runtime or a change to any production budget.
        self.input
            .get_ref()
            .set_read_timeout(Some(COMMAND_BUDGET + Duration::from_secs(5)))
            .unwrap();
        self.request(serde_json::json!({"op": "gate", "argv": argv, "token": token}));
        let code = self.response()["exit"].as_i64().unwrap();
        self.input
            .get_ref()
            .set_read_timeout(Some(HANDSHAKE_BUDGET))
            .unwrap();
        code
    }
    #[allow(
        dead_code,
        reason = "historical EOF diagnostic retained; D41 controls use real wait/live oracles"
    )]
    fn require_closed_without_self_expiry(&mut self) {
        // Diagnostic only: EOF is NOT authoritative process death (FS2/I3).
        // A self-expired worker is an explicit failure, never INV-T evidence.
        self.input
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut line = String::new();
        let count = self
            .input
            .read_line(&mut line)
            .expect("worker channel stayed open past teardown observation");
        assert_eq!(
            count, 0,
            "worker self-expired or sent data instead of being stopped: {line}"
        );
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Closing both halves releases workers even when an assertion unwinds.
        // Holder/direct workers have no idle expiry; nested command runtimes are bounded.
        // Death assertions use process observations and real status/lock results.
        let _ = self.output.shutdown(std::net::Shutdown::Both);
    }
}

#[test]
fn exclusive_wait_zero_names_holder_and_never_starts_losing_command() {
    let sandbox = Sandbox::new();
    let boundary = Boundary::new(&sandbox);
    let mut holder = boundary.exec(&sandbox, "holder");
    let mut peer = boundary.accept(&mut holder);
    let marker = sandbox.temporary.join("loser-started");
    let mut command = sandbox.gate(&[
        "lease",
        "exec",
        "--purpose",
        "private-loser",
        "--lease-wait",
        "0",
        "--",
        "/usr/bin/touch",
    ]);
    command.arg(&marker);
    let output = sandbox.run(&mut command);
    output.code(75);
    assert!(
        output.text().contains("private-contract-holder")
            || output.text().contains(&holder.child.id().to_string()),
        "holder not named: {}",
        output.text()
    );
    assert!(!marker.exists());
    peer.release();
    holder.finish(HANDSHAKE_BUDGET).code(0);
}

#[test]
fn killed_holder_releases_kernel_lease_while_descendant_with_token_is_still_live() {
    let sandbox = Sandbox::new();
    let boundary = Boundary::new(&sandbox);
    let mut holder = boundary.exec(&sandbox, "holder");
    let mut peer = boundary.accept(&mut holder);
    let old_inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(sandbox.temporary.join("lease/r-orch-gate.lease"))
            .unwrap()
            .ino()
    };
    assert!(!peer.ready["token"].as_str().unwrap().is_empty());
    holder.signal("KILL");
    assert!(!holder.finish(HANDSHAKE_BUDGET).status.success());
    // This live peer is the descendant needed to test close-on-exec. No process
    // is signalled to obtain the successor's lease, including this peer.
    peer.request(serde_json::json!({"op": "gate", "argv": ["/usr/bin/true"], "token": "keep"}));
    assert_eq!(peer.response()["exit"], 0);
    let marker = sandbox.temporary.join("successor-started");
    let mut command = sandbox.gate(&[
        "lease",
        "exec",
        "--purpose",
        "private-successor",
        "--lease-wait",
        "0",
        "--",
        "/usr/bin/touch",
    ]);
    command.arg(&marker);
    sandbox.run(&mut command).code(0);
    assert!(marker.exists());
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(sandbox.temporary.join("lease/r-orch-gate.lease"))
            .unwrap()
            .ino(),
        old_inode,
        "recovery must not unlink the shared lock"
    );
    peer.release();
    require_process_gone(&sandbox, peer.ready["pid"].as_u64().unwrap());
}

#[test]
fn live_inherited_token_proceeds_and_removed_or_wrong_token_waits() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n", true);
    for (token, expected) in [("keep", 0), ("remove", 75), ("wrong", 75)] {
        let boundary = Boundary::new(&sandbox);
        let mut holder = boundary.exec(&sandbox, "holder");
        let mut peer = boundary.accept(&mut holder);
        assert_eq!(
            peer.nested(token, &["pr", "--part", "suite", "--lease-wait", "0"]),
            expected
        );
        if token == "keep" {
            assert_eq!(field(&sandbox.result(), "lease"), "inherited");
        }
        peer.release();
        holder.finish(COMMAND_BUDGET).code(0);
        drop(peer);
        drop(boundary);
        fs::remove_file(sandbox.temporary.join("s")).unwrap();
    }
}

#[test]
fn stale_inherited_token_after_holder_death_cannot_grant_inherited_authority() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n", true);
    let boundary = Boundary::new(&sandbox);
    let mut holder = boundary.exec(&sandbox, "holder");
    let mut peer = boundary.accept(&mut holder);
    holder.signal("KILL");
    assert!(!holder.finish(HANDSHAKE_BUDGET).status.success());
    assert_eq!(
        peer.nested("keep", &["pr", "--part", "suite", "--lease-wait", "0"]),
        0
    );
    assert_eq!(field(&sandbox.result(), "lease"), "exclusive");
    peer.release();
    require_process_gone(&sandbox, peer.ready["pid"].as_u64().unwrap());
}

#[test]
fn every_lease_entry_refuses_symlinks_without_touching_owned_outside_canary() {
    for name in [
        "r-orch-gate.lease",
        "r-orch-gate.turnstile",
        "r-orch-gate.holder",
    ] {
        let sandbox = Sandbox::new();
        let outside = sandbox.temporary.join("outside-canary");
        fs::write(&outside, "untouched\n").unwrap();
        symlink(&outside, sandbox.temporary.join("lease").join(name)).unwrap();
        let marker = sandbox.temporary.join("unsafe-command-started");
        let mut command = sandbox.gate(&[
            "lease",
            "exec",
            "--purpose",
            "private-symlink",
            "--lease-wait",
            "0",
            "--",
            "/usr/bin/touch",
        ]);
        command.arg(&marker);
        let output = sandbox.run(&mut command);
        output.code(1);
        output.contains("REFUSED");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched\n");
        assert!(!marker.exists());
    }
}

#[test]
fn created_lease_files_are_private_regular_entries() {
    let sandbox = Sandbox::new();
    let boundary = Boundary::new(&sandbox);
    let mut holder = boundary.exec(&sandbox, "holder");
    let mut peer = boundary.accept(&mut holder);
    let mut found = 0;
    for name in [
        "r-orch-gate.lease",
        "r-orch-gate.turnstile",
        "r-orch-gate.holder",
    ] {
        match fs::symlink_metadata(sandbox.temporary.join("lease").join(name)) {
            Ok(metadata) => {
                found += 1;
                assert!(metadata.is_file());
                assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                assert_ne!(name, "r-orch-gate.lease", "live holder has no lease entry");
            }
            Err(error) => panic!("private lease entry: {error}"),
        }
    }
    assert!(found > 0);
    peer.release();
    holder.finish(COMMAND_BUDGET).code(0);
    // Turnstile creation/fairness requires PR2 queued-observation coverage;
    // lazy creation and post-exit removal are implementer choices.
}

#[test]
fn replaced_lock_inode_is_lost_and_cannot_qualify() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n", true);
    // A gate-tier result, not lease-exec's unspecified status, is the oracle.
    sandbox.suite().code(0);
    let boundary = Boundary::new(&sandbox);
    parked_build(&sandbox, &boundary, "holder");
    let mut holder =
        sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    let _peer = boundary.accept(&mut holder);
    let lock = sandbox.temporary.join("lease/r-orch-gate.lease");
    fs::rename(&lock, sandbox.temporary.join("lease/old-owned-lock")).unwrap();
    fs::write(&lock, "").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    // Lease loss may stop the worker before a release request reaches it.
    // Leave it parked so the gate's teardown is the only cause of completion.
    let output = holder.finish(COMMAND_BUDGET);
    output.code(1);
    let result = sandbox.result();
    assert_eq!(field(&result, "lease"), "lost");
    let qualifying = field(&result, "qualifying");
    assert!(
        qualifying.starts_with("no") && qualifying.contains("lease"),
        "lost lease not attributed in qualification: {result}"
    );
}

/// A fake cargo whose first `build` spawns the parked holder as a **child** and
/// then exits 0, so the gate's direct child completes while a descendant still
/// holds the gate's own output pipe. Later invocations exec the real cargo, so
/// the rest of the suite is real.
///
/// This is the vehicle for `steps.rs`'s "capture EOF deadline; descendant may
/// hold output; target kept", and it is independent of the suite runner: the
/// gate's build command is the gate's own child whatever runs the tests.
fn completed_build_with_holder(sandbox: &Sandbox, boundary: &Boundary) {
    let cargo = sandbox.temporary.join("bin/cargo");
    let real = sandbox.cargo_forwarder();
    let once = sandbox.temporary.join("first-build-holder");
    sandbox.script(
        &cargo,
        &format!(
            r#"#!/usr/bin/python3
import os, subprocess, sys
from pathlib import Path
{parser}
if cargo_subcommand(sys.argv[1:]) == 'build':
    try:
        Path({once:?}).mkdir()
    except FileExistsError:
        pass
    else:
        subprocess.Popen(['/usr/bin/python3', {payload:?}, 'holder', {socket:?}])
        sys.exit(0)
os.execv({real:?}, [{real:?}] + sys.argv[1:])
"#,
            parser = CARGO_SUBCOMMAND_PY,
            once = once.to_str().unwrap(),
            payload = sandbox.temporary.join("payload.py").to_str().unwrap(),
            socket = boundary.path.to_str().unwrap(),
            real = real.to_str().unwrap(),
        ),
    );
}

fn parked_build(sandbox: &Sandbox, boundary: &Boundary, mode: &str) {
    let cargo = sandbox.temporary.join("bin/cargo");
    let real = sandbox.cargo_forwarder();
    let once = sandbox.temporary.join("first-build-boundary");
    let second = sandbox.temporary.join("second-build-attempt");
    sandbox.script(
        &cargo,
        &format!(
            r#"#!/usr/bin/python3
import os, sys
from pathlib import Path
{parser}
if cargo_subcommand(sys.argv[1:]) in ('build', 'test'):
    try:
        Path({once:?}).mkdir()
    except FileExistsError:
        Path({second:?}).write_text('second\n')
        sys.exit(95)
    # Report selected target rather than assuming when F creates it.
    Path({target_report:?}).write_text(os.environ.get('CARGO_TARGET_DIR', ''))
    Path({cwd_report:?}).write_text(os.getcwd())
    os.execv('/usr/bin/python3', ['/usr/bin/python3', {payload:?}, {mode:?}, {socket:?}])
os.execv({real:?}, [{real:?}] + sys.argv[1:])
"#,
            parser = CARGO_SUBCOMMAND_PY,
            once = once.to_str().unwrap(),
            second = second.to_str().unwrap(),
            target_report = sandbox.temporary.join("selected-target").to_str().unwrap(),
            cwd_report = sandbox.temporary.join("selected-cwd").to_str().unwrap(),
            real = real.to_str().unwrap(),
            payload = sandbox.temporary.join("payload.py").to_str().unwrap(),
            socket = boundary.path.to_str().unwrap()
        ),
    );
}

fn prepare_owned_target(sandbox: &Sandbox, relative: &str) -> PathBuf {
    assert!(matches!(relative, "target/gate-warm" | "target/gate"));
    let owner = fs::symlink_metadata(&sandbox.temporary).unwrap().uid();
    let target = sandbox.root.join(relative);
    // Only these known fixture paths may be created, never the reported path.
    // Refuse symlink/foreign/non-directory components before descending.
    let parent = sandbox.root.join("target");
    for path in [&sandbox.root, &parent, &target] {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(path).unwrap();
                fs::symlink_metadata(path).unwrap()
            }
            Err(error) => panic!("setup: owned target {}: {error}", path.display()),
        };
        assert!(
            metadata.is_dir() && !metadata.file_type().is_symlink() && metadata.uid() == owner,
            "setup: unsafe owned target component {}",
            path.display()
        );
    }
    target
}

fn same_selected_target(
    sandbox: &Sandbox,
    expected: &Path,
    child_cwd: &Path,
    reported: impl AsRef<Path>,
) -> std::io::Result<bool> {
    let reported = reported.as_ref();
    if reported.as_os_str().is_empty() || !child_cwd.is_absolute() {
        return Ok(false);
    }
    let cwd = fs::canonicalize(child_cwd)?;
    if !cwd.starts_with(&sandbox.temporary) {
        return Ok(false);
    }
    // Canonicalization is read-only identity observation, never permission to
    // create, enumerate, remove or plant a canary through an untrusted report.
    let selected = match fs::canonicalize(cwd.join(reported)) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(selected.starts_with(&sandbox.temporary) && selected == fs::canonicalize(expected)?)
}

fn require_selected_target(sandbox: &Sandbox, expected: &Path) {
    let reported = fs::read_to_string(sandbox.temporary.join("selected-target")).unwrap();
    let cwd = fs::read_to_string(sandbox.temporary.join("selected-cwd")).unwrap();
    assert!(
        same_selected_target(sandbox, expected, Path::new(&cwd), &reported).unwrap(),
        "wrong selected target {reported:?} at child cwd {cwd:?}; expected {}",
        expected.display()
    );
}

#[test]
fn selected_target_accepts_relative_spelling_against_the_actual_child_cwd() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    let cwd = sandbox.root.join("fixture");
    fs::create_dir(&cwd).unwrap();
    assert!(same_selected_target(&sandbox, &expected, &cwd, "../target/gate-warm").unwrap());
}

#[test]
fn selected_target_accepts_noncanonical_absolute_spelling() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    let selected = sandbox.root.join("target/../target/./gate-warm");
    assert!(same_selected_target(&sandbox, &expected, &sandbox.root, &selected).unwrap());
}

#[test]
fn selected_target_accepts_an_equivalent_owned_parent_alias() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    let alias = sandbox.temporary.join("repo-alias");
    symlink(&sandbox.root, &alias).unwrap();
    let selected = alias.join("target/gate-warm");
    assert!(same_selected_target(&sandbox, &expected, &alias, &selected).unwrap());
}

#[test]
fn selected_target_rejects_a_different_existing_owned_directory() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    prepare_owned_target(&sandbox, "target/gate");
    assert!(!same_selected_target(&sandbox, &expected, &sandbox.root, "target/gate").unwrap());
}

#[test]
fn selected_target_rejects_an_absent_directory_without_creating_it() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    let selected = sandbox.temporary.join("unreported-target/missing");
    assert!(!same_selected_target(&sandbox, &expected, &sandbox.root, &selected).unwrap());
    assert!(!selected.parent().unwrap().exists());
}

#[test]
fn selected_target_rejects_an_empty_report_instead_of_using_the_cwd() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    assert!(!same_selected_target(&sandbox, &expected, &expected, "").unwrap());
}

#[test]
fn selected_target_rejects_an_alias_to_a_different_owned_target() {
    let sandbox = Sandbox::filesystem_only();
    let expected = prepare_owned_target(&sandbox, "target/gate-warm");
    let different = prepare_owned_target(&sandbox, "target/gate");
    let alias = sandbox.temporary.join("different-alias");
    symlink(different, &alias).unwrap();
    assert!(!same_selected_target(&sandbox, &expected, &sandbox.root, &alias).unwrap());
}

#[test]
fn expected_target_symlink_is_refused_without_writing_through_it() {
    let sandbox = Sandbox::filesystem_only();
    let outside = sandbox.temporary.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, sandbox.root.join("target")).unwrap();
    let refused = std::panic::catch_unwind(|| {
        prepare_owned_target(&sandbox, "target/gate-warm");
    });
    assert!(refused.is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn direct_holder_int_term_hup_reaps_direct_writer_and_finishes_owned_target() {
    for (tier, signal) in [
        ("full", "INT"),
        ("full", "TERM"),
        ("full", "HUP"),
        ("pr", "INT"),
        ("pr", "TERM"),
        ("pr", "HUP"),
    ] {
        let sandbox = Sandbox::new();
        sandbox.mini("#[test]\nfn sample() {}\n", true);
        let real_cargo = fs::read_link(sandbox.temporary.join("bin/cargo")).unwrap();
        let boundary = Boundary::new(&sandbox);
        // D41 requires the directly started writer to exit; escaped descendants
        // are an owner-accepted residual, not a required teardown assertion.
        parked_build(&sandbox, &boundary, "resistant-direct");
        let mut holder =
            sandbox.spawn(&mut sandbox.gate(&[tier, "--part", "suite", "--lease-wait", "0"]));
        let mut worker = boundary.accept(&mut holder);
        assert_eq!(worker.ready["role"], "resistant-direct");
        let direct_pid = worker.ready["pid"].as_u64().unwrap();
        holder.signal(signal);
        let number = match signal {
            "INT" => 2,
            "TERM" => 15,
            "HUP" => 1,
            _ => unreachable!(),
        };
        assert_eq!(worker.response()["forwarded"], number);
        let output = holder.finish(Duration::from_secs(10));
        assert!(!output.status.success());
        let record = sandbox.result();
        assert!(
            field(&record, "verdict").starts_with("INTERRUPTED"),
            "{record}"
        );
        let observed = sandbox.run(sandbox.command("/bin/ps").args([
            "-p",
            &direct_pid.to_string(),
            "-o",
            "pid=",
        ]));
        assert!(
            observed.stdout.trim().is_empty(),
            "direct writer survived: {}",
            observed.text()
        );
        if tier == "full" {
            assert!(!sandbox.root.join("target/gate").exists());
        } else {
            fs::remove_file(sandbox.temporary.join("bin/cargo")).unwrap();
            symlink(real_cargo, sandbox.temporary.join("bin/cargo")).unwrap();
            sandbox.suite().code(0);
            assert_eq!(field(&sandbox.result(), "target"), "reused");
        }
        sandbox
            .run(&mut sandbox.gate(&[
                "lease",
                "exec",
                "--purpose",
                "after-interrupt",
                "--lease-wait",
                "0",
                "--",
                "/usr/bin/true",
            ]))
            .code(0);
    }
}

#[test]
fn pr1_target_single_writer_is_independent_of_host_serialization() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let boundary = Boundary::new(&sandbox);
    parked_build(&sandbox, &boundary, "holder");
    // C10: two independent configured leases isolate target-marker exclusion.
    // --no-lease is forbidden by D42 and cannot supply this positive.
    let first_lease = sandbox.temporary.join("lease-first");
    let second_lease = sandbox.temporary.join("lease-second");
    for directory in [&first_lease, &second_lease] {
        fs::create_dir(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut holder = sandbox.spawn(
        sandbox
            .gate(&["pr", "--part", "suite", "--lease-wait", "0"])
            .env("XTASK_GATE_LEASE_DIR", &first_lease),
    );
    let mut writer = boundary.accept(&mut holder);
    let target = prepare_owned_target(&sandbox, "target/gate-warm");
    require_selected_target(&sandbox, &target);
    let canary = target.join("first-writer-canary");
    fs::write(&canary, "must survive\n").unwrap();
    // Each process holds its own configured machine lease.
    // A contender cannot reset the first writer's target or enter its cargo boundary.
    let mut contender = sandbox.spawn(
        sandbox
            .gate(&["pr", "--part", "suite", "--lease-wait", "0"])
            .env("XTASK_GATE_LEASE_DIR", &second_lease),
    );
    let deadline = Instant::now() + COMMAND_BUDGET;
    let mut refused = false;
    while Instant::now() < deadline {
        assert!(canary.exists(), "second writer reset a live target");
        assert!(
            !sandbox.temporary.join("second-build-attempt").exists(),
            "second writer reached a cargo build"
        );
        match boundary.listener.accept() {
            Ok(_) => panic!("second writer entered build while first writer held target"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("contender admission observation: {error}"),
        }
        if let Some(status) = contender.child.try_wait().unwrap() {
            assert!(!status.success());
            refused = true;
            break;
        }
        std::thread::park_timeout(POLL_INTERVAL);
    }
    assert!(
        refused,
        "remaining oracle: waiting target contender needs an acquired/queued public observation; timeout absence is not a pass"
    );
    let output = contender.finish(HANDSHAKE_BUDGET);
    output.contains("warm target in use");
    writer.release();
    let _ = holder.finish(HANDSHAKE_BUDGET);
}

#[test]
fn warm_and_cold_orphan_writers_are_not_reset_under_after_direct_holder_kill() {
    for tier in ["pr", "full"] {
        let sandbox = Sandbox::new();
        sandbox.mini("#[test]\nfn sample() {}\n", true);
        let boundary = Boundary::new(&sandbox);
        parked_build(&sandbox, &boundary, "holder");
        let mut holder = sandbox.spawn(&mut sandbox.gate(&[
            tier,
            "--part",
            "suite",
            "--keep",
            "--lease-wait",
            "0",
        ]));
        let mut writer = boundary.accept(&mut holder);
        let target = prepare_owned_target(
            &sandbox,
            if tier == "pr" {
                "target/gate-warm"
            } else {
                "target/gate"
            },
        );
        require_selected_target(&sandbox, &target);
        let canary = target.join("orphan-writer-canary");
        fs::write(&canary, "alive\n").unwrap();
        writer.alive();
        let marker = sandbox.root.join(if tier == "pr" {
            "target/.gate-state/warm.use"
        } else {
            "target/.gate-state/cold.use"
        });
        require_extra_marker_and_normal_stdin(&writer, &marker);
        holder.signal("KILL");
        assert_eq!(holder.finish(HANDSHAKE_BUDGET).status.signal(), Some(9));
        writer.alive();
        marker_lock(&sandbox, &marker, 75);
        // The gate-tier lock FD must not leak into the still-live writer.
        sandbox
            .run(&mut sandbox.gate(&[
                "lease",
                "exec",
                "--purpose",
                "gate-tier-fd-control",
                "--lease-wait",
                "0",
                "--",
                "/usr/bin/true",
            ]))
            .code(0);
        let mut successor = sandbox.spawn(&mut sandbox.gate(&[
            tier,
            "--part",
            "suite",
            "--keep",
            "--lease-wait",
            "0",
        ]));
        let deadline = Instant::now() + COMMAND_BUDGET;
        let mut refused = false;
        while Instant::now() < deadline {
            assert!(canary.exists(), "{tier} reset underneath orphan writer");
            assert!(
                !sandbox.temporary.join("second-build-attempt").exists(),
                "successor reached a build underneath orphan"
            );
            match boundary.listener.accept() {
                Ok(_) => panic!("{tier} admitted a second target writer while orphan lived"),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("successor observation: {error}"),
            }
            if let Some(status) = successor.child.try_wait().unwrap() {
                assert!(!status.success());
                refused = true;
                break;
            }
            std::thread::park_timeout(POLL_INTERVAL);
        }
        assert!(
            refused,
            "remaining oracle: bounded wait branch needs a public queue observation; timeout absence is not a pass"
        );
        let output = successor.finish(HANDSHAKE_BUDGET);
        assert_ne!(
            output.status.code(),
            Some(75),
            "target refusal confused with leaked lease FD: {}",
            output.text()
        );
        let reason = if tier == "pr" {
            "warm target in use"
        } else {
            "build"
        };
        output.contains(reason);
        // The marker records the gate holder, not the cargo worker. Its PID is
        // unvalidated diagnostic data; exclusion comes from the actual lock.
        output.contains("marker diagnostics (unvalidated)");
        output.contains("Do not remove target/");
        assert!(fs::read_to_string(target.join("orphan-writer-canary")).unwrap() == "alive\n");
        writer.alive();
        writer.release();
        require_process_gone(&sandbox, writer.ready["pid"].as_u64().unwrap());
        marker_lock(&sandbox, &marker, 0);
    }
}

// PR2 TV61/62 and shared-fast RS1 controls below use the real fast requester.
// Remaining atomic-fault RS3, RS8/9/10 and attribution/foreign-owner seams are listed in the
// handoff, together with why current public entry points cannot safely plant them.

#[test]
fn unfinished_warm_build_resets_after_owned_orphan_writer_has_exited() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let real_cargo = fs::read_link(sandbox.temporary.join("bin/cargo")).unwrap();
    let boundary = Boundary::new(&sandbox);
    parked_build(&sandbox, &boundary, "holder");
    let mut holder =
        sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    let mut worker = boundary.accept(&mut holder);
    worker.alive();
    let marker = sandbox.root.join("target/.gate-state/warm.use");
    require_extra_marker_and_normal_stdin(&worker, &marker);
    let old_artifact = sandbox.root.join("target/gate-warm/abandoned-artifact");
    fs::write(&old_artifact, "unfinished\n").unwrap();
    let outside = sandbox.temporary.join("outside-recovery-canary");
    fs::write(&outside, "outside\n").unwrap();
    holder.signal("KILL");
    assert_eq!(holder.finish(HANDSHAKE_BUDGET).status.signal(), Some(9));
    worker.alive();
    marker_lock(&sandbox, &marker, 75);
    let refused = sandbox.suite();
    refused.code(1);
    refused.contains("marker lock not granted");
    assert_eq!(fs::read_to_string(&old_artifact).unwrap(), "unfinished\n");
    worker.alive();
    worker.release();
    // No EOF/expiry inference: read-only process absence and a granted lock
    // independently observe the writer's exit and last close.
    require_process_gone(&sandbox, worker.ready["pid"].as_u64().unwrap());
    marker_lock(&sandbox, &marker, 0);
    fs::remove_file(sandbox.temporary.join("bin/cargo")).unwrap();
    symlink(real_cargo, sandbox.temporary.join("bin/cargo")).unwrap();
    sandbox.suite().code(0);
    assert_eq!(
        field(&sandbox.result(), "target"),
        "reset: previous build did not finish"
    );
    assert!(
        !old_artifact.exists(),
        "native reset retained abandoned output"
    );
    assert!(
        !marker.exists(),
        "successful rebuild left the use unfinished"
    );
    assert!(
        sandbox
            .root
            .join("target/.gate-state/warm.record")
            .is_file()
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside\n");
}

#[test]
fn target_runner_preserves_caller_stdin_bytes_through_native_rebuild() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let real = sandbox.cargo_forwarder();
    let observed = sandbox.temporary.join("stdin-observed");
    sandbox.script(
        &sandbox.temporary.join("bin/cargo"),
        &format!(
            r#"#!/bin/sh
for arg do
    if [ "$arg" = build ] && [ ! -f {observed:?} ]; then
        /bin/cat > {observed:?}
    fi
done
exec {real:?} "$@"
"#,
            observed = observed.to_str().unwrap(),
            real = real.to_str().unwrap(),
        ),
    );
    let input = sandbox.temporary.join("caller-input");
    let bytes = b"ordinary caller stdin\nsecond line\0tail";
    fs::write(&input, bytes).unwrap();
    let mut command = sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]);
    sandbox
        .spawn_with_input(&mut command, fs::File::open(input).unwrap())
        .finish(COMMAND_BUDGET)
        .code(0);
    assert_eq!(fs::read(observed).unwrap(), bytes);
    assert!(!sandbox.root.join("target/.gate-state/warm.use").exists());
}

#[test]
fn foreign_build_sampler_boundary_records_yes_without_invalidating_correctness() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    // CONDITIONAL FIXTURE, UNRUN: PATH interception is not a public F promise.
    // I6 remains blocked if F uses absolute tools. F-owned sampler/parser error
    // tests and native controls are still required; these data rows cannot
    // establish process ownership or measurement invalidation.
    sandbox.script(&sandbox.temporary.join("bin/ps"), "#!/bin/sh\nexit 0\n");
    sandbox.suite().code(0);
    assert_eq!(field(&sandbox.result(), "foreign-build-activity"), "no");
    sandbox.script(
        &sandbox.temporary.join("bin/ps"),
        "#!/bin/sh\nprintf '%s\\n' '900000 1 cargo'\n",
    );
    sandbox.suite().code(0);
    assert_eq!(field(&sandbox.result(), "foreign-build-activity"), "yes");
    // A partial run's qualifying=no proves nothing about sampling. The field
    // transition above is only a PATH-boundary fixture; absolute-tool F needs
    // internal sampler tests/native controls (I6/SR-4), still explicitly due.
}

#[cfg(target_os = "macos")]
#[test]
fn unavailable_load_query_is_disclosed_as_unavailable_measurement_data() {
    // CONDITIONAL FIXTURE, UNRUN: no agreed sysctl interception seam (I6).
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    sandbox.script(&sandbox.temporary.join("bin/ps"), "#!/bin/sh\nexit 0\n");
    sandbox.script(
        &sandbox.temporary.join("bin/sysctl"),
        "#!/bin/sh\nprintf '%s\\n' '{ 0.00 0.00 0.00 }'\n",
    );
    sandbox.suite().code(0);
    assert_eq!(field(&sandbox.result(), "foreign-build-activity"), "no");
    sandbox.script(
        &sandbox.temporary.join("bin/sysctl"),
        "#!/bin/sh\nexit 99\n",
    );
    sandbox.suite().code(0);
    assert_eq!(
        field(&sandbox.result(), "foreign-build-activity"),
        "unavailable"
    );
    // A partial run's qualifying=no proves nothing about sampling. The field
    // transition above is only a PATH-boundary fixture; absolute-tool F needs
    // internal sampler tests/native controls (I6/SR-4), still explicitly due.
}

// Safety-held obligations: authoritative worker death and independent cleanup
// (FS2/I3), publication ordered after all writers stop (SR-2), already-reparented
// escapees (SR-4), writer-gone observation before recovery (SR-5), EPIPE/log-failure
// teardown (SR-7), group INT and exact positive wait bounds (SR-8). Channel EOF
// cannot close these. None of the lifecycle cases is executable acceptance.

// D41 permits escaped descendants; this independently tests the owned direct
// command with no workload self-expiry and no EOF-as-death inference.
#[test]
fn lease_exec_int_term_hup_kills_and_reaps_resistant_direct_child_before_releasing_lease() {
    for (signal, number) in [("INT", 2), ("TERM", 15), ("HUP", 1)] {
        let sandbox = Sandbox::new();
        let boundary = Boundary::new(&sandbox);
        let mut holder = boundary.exec(&sandbox, "resistant-direct");
        let mut peer = boundary.accept(&mut holder);
        let pid = peer.ready["pid"].as_u64().unwrap();
        holder.signal(signal);
        assert_eq!(peer.response()["forwarded"], number, "forwarded {signal}");
        // A live signal-ignoring direct child must still retain the lease.
        sandbox
            .run(&mut sandbox.gate(&[
                "lease",
                "exec",
                "--purpose",
                "during-teardown",
                "--lease-wait",
                "0",
                "--",
                "/usr/bin/true",
            ]))
            .code(75);
        let output = holder.finish(Duration::from_secs(10));
        output.code(128 + number);
        output.contains("unix_wait_status(9)");
        // Read-only observation of the task-owned handshake pid complements
        // the retained wait status. No numeric PID is used as signal authority.
        let observed =
            sandbox.run(
                sandbox
                    .command("/bin/ps")
                    .args(["-p", &pid.to_string(), "-o", "pid="]),
            );
        assert!(
            observed.stdout.trim().is_empty(),
            "direct child still exists: {}",
            observed.text()
        );
        sandbox
            .run(&mut sandbox.gate(&[
                "lease",
                "exec",
                "--purpose",
                "after-teardown",
                "--lease-wait",
                "0",
                "--",
                "/usr/bin/true",
            ]))
            .code(0);
    }
}

fn require_process_gone(sandbox: &Sandbox, pid: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out =
            sandbox.run(
                sandbox
                    .command("/bin/ps")
                    .args(["-p", &pid.to_string(), "-o", "pid="]),
            );
        if out.status.code() == Some(1) && out.stdout.trim().is_empty() {
            return;
        }
        assert!(
            out.status.success(),
            "process observation failed: {}",
            out.text()
        );
        assert!(
            Instant::now() < deadline,
            "task-owned writer {pid} still exists: {}",
            out.text()
        );
        std::thread::park_timeout(POLL_INTERVAL);
    }
}

fn marker_lock(sandbox: &Sandbox, marker: &Path, expected: i32) {
    let mut command = sandbox.command("/usr/bin/python3");
    command
        .args([
            "-c",
            r#"import fcntl,os,sys
fd=os.open(sys.argv[1],os.O_RDONLY|os.O_NOFOLLOW)
try: fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
except BlockingIOError: sys.exit(75)
print(os.fstat(fd).st_ino)
"#,
        ])
        .arg(marker);
    sandbox.run(&mut command).code(expected);
}

#[test]
fn live_marker_or_lease_loss_reaps_direct_writer_and_exports_kept_unfinished_target() {
    for lost in ["marker", "lease"] {
        let sandbox = Sandbox::new();
        sandbox.mini("#[test]\nfn sample() {}\n", true);
        sandbox.suite().code(0);
        let boundary = Boundary::new(&sandbox);
        parked_build(&sandbox, &boundary, "resistant-direct");
        let export = sandbox.temporary.join("lost-export");
        let mut holder = sandbox.spawn(
            sandbox
                .gate(&["pr", "--part", "suite", "--lease-wait", "0", "--evidence"])
                .arg(&export),
        );
        let mut writer = boundary.accept(&mut holder);
        writer.alive();
        let path = if lost == "marker" {
            sandbox.root.join("target/.gate-state/warm.use")
        } else {
            sandbox.temporary.join("lease/r-orch-gate.lease")
        };
        let saved = sandbox.temporary.join("saved-owned-lock");
        fs::rename(&path, &saved).unwrap();
        let output = holder.finish(Duration::from_secs(10));
        require_process_gone(&sandbox, writer.ready["pid"].as_u64().unwrap());
        output.code(1);
        let record = sandbox.result();
        assert!(field(&record, "target").starts_with("kept ("), "{record}");
        assert_eq!(field(&record, "target-finished"), "no");
        assert!(field(&record, "qualifying").starts_with("no"));
        assert!(!sandbox.root.join("target/.gate-state/warm.record").exists());
        assert!(sandbox.root.join("target/gate-warm").is_dir());
        let commands =
            fs::read_to_string(Path::new(field(&record, "run-dir")).join("commands.txt")).unwrap();
        assert!(
            commands.contains("reaped-exit=137"),
            "no direct KILL status: {commands}"
        );
        if lost == "lease" {
            assert_eq!(field(&record, "lease"), "lost");
            assert!(sandbox.root.join("target/.gate-state/warm.use").exists());
        }
        let copied = fs::read_to_string(export.join("result.txt")).unwrap();
        assert_eq!(field(&copied, "target-finished"), "no");
        assert!(field(&copied, "target").starts_with("kept ("));
        marker_lock(&sandbox, &saved, 0);
    }
}

#[test]
fn closed_terminal_pipes_cannot_prevent_signal_reap_or_replace_interrupted_exit() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    sandbox.suite().code(0);
    let boundary = Boundary::new(&sandbox);
    parked_build(&sandbox, &boundary, "resistant-direct");
    let mut holder = sandbox.spawn_with_closed_pipes(&mut sandbox.gate(&[
        "pr",
        "--part",
        "suite",
        "--lease-wait",
        "0",
    ]));
    let mut writer = boundary.accept(&mut holder);
    writer.alive();
    holder.signal("TERM");
    assert_eq!(writer.response()["forwarded"], 15);
    let output = holder.finish(Duration::from_secs(10));
    require_process_gone(&sandbox, writer.ready["pid"].as_u64().unwrap());
    let record = sandbox.result();
    assert!(field(&record, "verdict").starts_with("INTERRUPTED"));
    assert_eq!(field(&record, "target-finished"), "yes");
    assert!(!sandbox.root.join("target/.gate-state/warm.use").exists());
    let commands =
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("commands.txt")).unwrap();
    assert!(
        commands.contains("exit=137"),
        "missing actual direct-child status: {commands}"
    );
    sandbox
        .run(&mut sandbox.gate(&[
            "lease",
            "exec",
            "--purpose",
            "closed-pipe-successor",
            "--lease-wait",
            "0",
            "--",
            "/usr/bin/true",
        ]))
        .code(0);
    // Preserve the signal contract even when the terminal has disconnected.
    output.code(143);
}

#[test]
fn inherited_holder_loss_during_real_use_reaps_writer_without_publishing_a_record() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let outer = Boundary::new(&sandbox);
    let mut holder = outer.exec(&sandbox, "holder");
    let mut client = outer.accept(&mut holder);
    assert_eq!(
        client.nested("keep", &["pr", "--part", "suite", "--lease-wait", "0"]),
        0
    );
    assert_eq!(field(&sandbox.result(), "lease"), "inherited");
    // A second socket distinguishes the gate's direct cargo worker from the
    // still-live client whose inherited token alone confers no authority.
    let address = sandbox.temporary.join("inner-boundary");
    let listener = UnixListener::bind(&address).unwrap();
    listener.set_nonblocking(true).unwrap();
    let inner = Boundary {
        listener,
        path: address,
    };
    parked_build(&sandbox, &inner, "resistant-direct");
    let argv = [
        env!("CARGO_BIN_EXE_xtask"),
        "gate",
        "pr",
        "--part",
        "suite",
        "--lease-wait",
        "0",
    ];
    client.request(serde_json::json!({"op":"gate", "argv":argv, "token":"keep"}));
    let mut writer = inner.accept(&mut holder);
    writer.alive();
    holder.signal("KILL");
    assert_eq!(holder.finish(HANDSHAKE_BUDGET).status.signal(), Some(9));
    assert_eq!(client.response()["exit"], 1);
    require_process_gone(&sandbox, writer.ready["pid"].as_u64().unwrap());
    client.alive();
    let record = sandbox.result();
    assert_eq!(field(&record, "lease"), "lost");
    assert_eq!(field(&record, "target-finished"), "no");
    assert!(field(&record, "target").starts_with("kept ("));
    assert!(!sandbox.root.join("target/.gate-state/warm.record").exists());
    assert!(sandbox.root.join("target/.gate-state/warm.use").exists());
    let commands =
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("commands.txt")).unwrap();
    assert!(
        commands.contains("reaped-exit=137"),
        "missing resistant direct-child KILL status: {commands}"
    );
    client.release();
    require_process_gone(&sandbox, client.ready["pid"].as_u64().unwrap());
}

#[test]
fn native_foreign_cargo_and_periodic_samples_are_observed_while_gate_build_is_live() {
    use witness::fixture::{rust_literal, wait_for};
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    sandbox.suite().code(0);
    let foreign_ready = sandbox.temporary.join("foreign-ready");
    let foreign_release = sandbox.temporary.join("foreign-release");
    let gate_ready = sandbox.temporary.join("gate-ready");
    let gate_release = sandbox.temporary.join("gate-release");
    let build = |ready: &Path, release: &Path| {
        format!(
            r#"fn main() {{
        std::fs::write({ready}, "ready").unwrap();
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(30);
        while !std::path::Path::new({release}).exists() {{
            assert!(std::time::Instant::now()<deadline, "fixture handshake expired");
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }}
    }}"#,
            ready = rust_literal(ready),
            release = rust_literal(release)
        )
    };
    let foreign_root = sandbox.temporary.join("foreign");
    fs::create_dir_all(foreign_root.join("src")).unwrap();
    fs::write(
        foreign_root.join("Cargo.toml"),
        "[package]\nname='foreign-fixture'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        foreign_root.join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .unwrap();
    fs::write(
        foreign_root.join("build.rs"),
        build(&foreign_ready, &foreign_release),
    )
    .unwrap();
    let mut foreign = sandbox.spawn(
        sandbox
            .command("cargo")
            .arg("build")
            .arg("--offline")
            .arg("--manifest-path")
            .arg(foreign_root.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", sandbox.temporary.join("foreign-target")),
    );
    wait_for(&foreign_ready, HANDSHAKE_BUDGET);
    assert!(foreign.child.try_wait().unwrap().is_none());
    // This wrapper records calls but delegates every process observation to the
    // actual native ps. It supplies neither synthetic rows nor load values.
    let samples = sandbox.temporary.join("ps-calls");
    let periodic = sandbox.temporary.join("periodic-observed");
    sandbox.script(
        &sandbox.temporary.join("bin/ps"),
        &format!(
            r#"#!/usr/bin/python3
import os, subprocess, sys
from pathlib import Path
p=Path({samples:?})
n=int(p.read_text())+1 if p.exists() else 1
p.write_text(str(n))
if n>=2: Path({periodic:?}).write_text('observed')
out=subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,comm='],capture_output=True,timeout=2)
Path(str(p)+'-'+str(n)+'.txt').write_bytes(out.stdout)
sys.stdout.buffer.write(out.stdout)
sys.exit(out.returncode)
"#,
            samples = samples.to_str().unwrap(),
            periodic = periodic.to_str().unwrap()
        ),
    );
    sandbox.write("fixture/build.rs", build(&gate_ready, &gate_release));
    let gate = sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    let gate_pid = gate.child.id();
    let foreign_pid = foreign.child.id();
    wait_for(&gate_ready, HANDSHAKE_BUDGET);
    wait_for(&periodic, HANDSHAKE_BUDGET);
    assert!(foreign.child.try_wait().unwrap().is_none());
    fs::write(&gate_release, "finish").unwrap();
    let output = gate.finish(COMMAND_BUDGET);
    fs::write(&foreign_release, "finish").unwrap();
    foreign.finish(COMMAND_BUDGET).code(0);
    output.code(0);
    let record = sandbox.result();
    assert_eq!(field(&record, "foreign-build-activity"), "yes");
    let mut maximum = 0;
    let mut observed_owned_foreign = false;
    for entry in fs::read_dir(&sandbox.temporary).unwrap() {
        let path = entry.unwrap().path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ps-calls-")
        {
            continue;
        }
        let bytes = fs::read_to_string(path).unwrap();
        let rows: Vec<_> = bytes
            .lines()
            .map(|line| {
                let mut fields = line.split_whitespace();
                (
                    fields.next().unwrap().parse::<u32>().unwrap(),
                    fields.next().unwrap().parse::<u32>().unwrap(),
                    fields.collect::<Vec<_>>().join(" "),
                )
            })
            .collect();
        let mut ours = std::collections::BTreeSet::from([gate_pid]);
        loop {
            let previous = ours.len();
            for (pid, parent, _) in &rows {
                if ours.contains(parent) {
                    ours.insert(*pid);
                }
            }
            if ours.len() == previous {
                break;
            }
        }
        let foreign_rows: Vec<_> = rows
            .iter()
            .filter(|(pid, _, command)| {
                !ours.contains(pid)
                    && [
                        "cargo",
                        "rustc",
                        "rustdoc",
                        "clippy-driver",
                        "cargo-nextest",
                    ]
                    .iter()
                    .any(|name| Path::new(command).file_name().is_some_and(|n| n == *name))
            })
            .collect();
        observed_owned_foreign |= foreign_rows.iter().any(|(pid, _, _)| *pid == foreign_pid);
        maximum = maximum.max(foreign_rows.len());
    }
    assert!(
        observed_owned_foreign,
        "native process rows did not identify the task-owned foreign cargo"
    );
    assert_eq!(
        field(&record, "foreign-build-count-max")
            .parse::<usize>()
            .unwrap(),
        maximum
    );
    let commands =
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("commands.txt")).unwrap();
    assert!(
        commands.contains("sample=3"),
        "start, periodic and end samples absent: {commands}"
    );
    assert!(field(&record, "contention-sampling").contains("every 15s"));
}

/// A gate-only TERM while **`cargo nextest run` is the direct child**, with a
/// test child that ignores TERM.
///
/// Measured behaviour this pins (see ADR 0022 Consequences): nextest cancels,
/// signals each running test's process group and exits 0.207 s after the
/// signal, well inside the gate's 2 s grace, so the gate never reaches SIGKILL
/// and never has to. nextest closes its own capture pipes as it goes, so the
/// gate's output is resolved. A test child that ignores TERM therefore
/// **survives**, reparented away from the test, and it holds nextest's capture
/// pipe rather than the gate's — so the gate's "descendant may hold output"
/// check cannot see it, and because the use *finishes* cleanly the gate even
/// publishes a record for the target. `cargo test` had to be killed with its
/// output unresolved, which kept the target; this is the one place where the
/// adoption makes the standing accepted risk for processes the gate did not
/// start (AGENTS.md "What no tier proves", ADR 0015 decision 41) easier to
/// reach rather than harder. ADR 0022 Consequences records it for the owner.
#[test]
fn suite_cancellation_finishes_the_use_and_leaves_a_term_ignoring_test_child() {
    use witness::fixture::rust_literal;
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let ready = sandbox.temporary.join("stubborn.pid");
    let release = sandbox.temporary.join("stubborn.release");
    sandbox.write(
        "fixture/src/lib.rs",
        format!(
            r#"
#[test]
fn stubborn_child_outlives_cancellation() {{
    std::process::Command::new("/bin/sh")
        .args(["-c", concat!("trap '' TERM; echo $$ > ", {ready}, "; while [ ! -f ", {release}, " ]; do sleep 0.05; done")])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}}
"#,
            ready = rust_literal(&ready),
            release = rust_literal(&release),
        ),
    );
    let marker = sandbox.root.join("target/.gate-state/warm.use");
    let mut gate =
        sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    wait_for(&ready, COMMAND_BUDGET);
    let child: u64 = fs::read_to_string(&ready).unwrap().trim().parse().unwrap();
    gate.signal("TERM");
    let output = gate.finish(COMMAND_BUDGET);
    output.code(143);
    let record = sandbox.result();
    assert_eq!(field(&record, "verdict"), "INTERRUPTED (part suite)");
    // THE CHANGE, pinned deliberately. nextest exits cleanly on the gate's
    // TERM with its pipes closed, so the gate's use *finishes* and a record is
    // published — where `cargo test` had to be killed with its output
    // unresolved, which kept the target. A later run therefore trusts a target
    // whose suite was cancelled. ADR 0022 Consequences records this and flags
    // it for the owner; it is not a behaviour this test endorses.
    assert_eq!(field(&record, "target-finished"), "yes", "{record}");
    assert_eq!(
        field(&record, "publication"),
        "record published",
        "{record}"
    );
    // The orphan really outlived the cancellation: nextest signalled its group
    // once and exited without escalating.
    let stat = fs::read_to_string(format!("/proc/{child}/stat")).expect("orphan is alive");
    let fields: Vec<&str> = stat.split_once(") ").unwrap().1.split(' ').collect();
    assert_ne!(
        fields[1],
        std::process::id().to_string(),
        "the orphan must be reparented away from this test: {stat}"
    );
    // And nothing records it: the finished use removed its marker, so the
    // inherited descriptor no longer holds anything a later run would notice.
    assert!(
        !marker.exists(),
        "a finished use leaves no marker, so the orphan is untracked"
    );
    fs::write(&release, "go").unwrap();
    require_process_gone(&sandbox, child);
}

/// The gate's direct child completes while a descendant still holds the gate's
/// output pipe: the target is kept and no record is published.
///
/// Measured, so the vehicle is the build phase rather than the suite: a suite
/// test can no longer reach the gate's pipe at all. nextest captures each
/// test's output and closes its own pipes on exit (TERM to nextest: process
/// exit and pipe EOF both at 0.207 s), and `cargo test --doc` *blocks* until a
/// doctest's leaked child closes its output (child exit and pipe EOF both at
/// 20.2 s), so neither half of the suite step can produce "child completed,
/// output unresolved". The gate's own build command still can, for any runner.
#[test]
fn completed_direct_command_with_live_output_holder_keeps_target_until_fixture_cleanup() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let boundary = Boundary::new(&sandbox);
    completed_build_with_holder(&sandbox, &boundary);
    let mut gate =
        sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    let mut leaf = boundary.accept(&mut gate);
    leaf.alive();
    let output = gate.finish(Duration::from_secs(60));
    // Still live after the gate returned. The fixture releases its own child by
    // protocol, observes process absence, then removes scratch.
    leaf.alive();
    leaf.release();
    require_process_gone(&sandbox, leaf.ready["pid"].as_u64().unwrap());
    output.code(1);
    let record = sandbox.result();
    assert!(field(&record, "target").starts_with("kept ("), "{record}");
    assert_eq!(field(&record, "target-finished"), "no");
    assert!(sandbox.root.join("target/.gate-state/warm.use").exists());
    assert!(!sandbox.root.join("target/.gate-state/warm.record").exists());
    let commands =
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("commands.txt")).unwrap();
    assert!(
        commands.contains("reaped-exit=0"),
        "no actual completed cargo status: {commands}"
    );
}

/// A suite test that leaves a process holding its output behind. nextest
/// captures each test's output, so the leak never reaches the gate; what it
/// does instead is fail the test that caused it
/// (`leak-timeout = { result = "fail" }`). The kept-target path that such a
/// test used to exercise is covered by the test above, on the gate's own build
/// command.
#[test]
fn a_suite_test_that_leaks_an_output_holder_fails_the_suite_and_names_it() {
    use witness::fixture::rust_literal;
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    sandbox.suite().code(0);
    let boundary = Boundary::new(&sandbox);
    sandbox.write(
        "fixture/src/lib.rs",
        format!(
            r#"
#[test]
fn leaves_an_explicitly_controlled_output_holder() {{
    let _owned = std::process::Command::new("/usr/bin/python3")
        .args([{payload}, "holder", {socket}]).spawn().unwrap();
}}
"#,
            payload = rust_literal(&sandbox.temporary.join("payload.py")),
            socket = rust_literal(&boundary.path)
        ),
    );
    let mut gate =
        sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    let mut leaf = boundary.accept(&mut gate);
    leaf.alive();
    let output = gate.finish(Duration::from_secs(60));
    // It really is still live after the gate returned: nextest stopped waiting
    // for the pipe, it did not kill anything. The fixture releases its own
    // child by protocol, observes process absence, then removes scratch.
    leaf.alive();
    leaf.release();
    require_process_gone(&sandbox, leaf.ready["pid"].as_u64().unwrap());
    output.code(1);
    let record = sandbox.result();
    assert_eq!(row(&record, "test")[1], "FAIL", "{record}");
    let inventory =
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("inventory.tsv")).unwrap();
    assert!(
        inventory.contains("leaves_an_explicitly_controlled_output_holder\tfailed"),
        "the leaking test must be named: {inventory}"
    );
    // The gate's own lifecycle is clean: nothing of its output was unresolved.
    assert_eq!(field(&record, "target-finished"), "yes");
    assert!(!sandbox.root.join("target/.gate-state/warm.use").exists());
    let commands =
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("commands.txt")).unwrap();
    assert!(
        commands.contains("exit=100"),
        "no actual completed runner status: {commands}"
    );
}

// Cargo does not forward gate-only TERM to its test process. The test owns and
// waits for the protocol worker; last-close is observed separately on the inode
// opened before TERM, even if a broken finish unlinks the marker pathname.
fn retain_native_control(
    tag: &str,
    output: &witness::fixture::Capture,
    record: Option<&str>,
    details: serde_json::Value,
) {
    let Some(root) = std::env::var_os("XTASK_NATIVE_CONTROL_EVIDENCE") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(root.is_absolute() && root.is_dir());
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let serial = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dest = root.join(format!("{tag}-{}-{serial}", std::process::id()));
    fs::create_dir(&dest).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dest.join("stdout"), &output.stdout_bytes).unwrap();
    fs::write(dest.join("stderr"), &output.stderr_bytes).unwrap();
    fs::write(
        dest.join("observation.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "status": output.status.code(), "details": details,
        }))
        .unwrap(),
    )
    .unwrap();
    if let Some(record) = record {
        fs::write(dest.join("result.txt"), record).unwrap();
        fn copy(from: &Path, to: &Path) {
            fs::create_dir(to).unwrap();
            fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    copy(&entry.path(), &to.join(entry.file_name()));
                } else {
                    assert!(kind.is_file());
                    fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
                }
            }
        }
        copy(Path::new(field(record, "run-dir")), &dest.join("run"));
    }
}

fn require_successful_worker_wait(observed: &str) {
    assert_eq!(
        observed, "Some(0)",
        "actual child.wait status must be success"
    );
}

#[test]
fn output_holder_wait_status_rejects_nonzero_containing_zero() {
    let actual_success = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    require_successful_worker_wait(&format!("{:?}", actual_success.code()));
    let actual_failure = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 101"])
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    let observed = format!("{:?}", actual_failure.code());
    assert!(observed.contains('0'));
    assert!(std::panic::catch_unwind(|| require_successful_worker_wait(&observed)).is_err());
    let signal = std::process::ExitStatus::from_raw(15);
    assert!(
        std::panic::catch_unwind(|| require_successful_worker_wait(&format!(
            "{:?}",
            signal.code()
        )))
        .is_err()
    );
}

fn interrupted_output_holder(tier: &str) {
    use witness::fixture::rust_literal;
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let boundary = Boundary::new(&sandbox);
    let waited = sandbox.temporary.join("worker.waited");
    // A doc-test, for the reason given in the completed-holder test above:
    // only `cargo test --doc` still hands the gate's own pipe to what the
    // suite starts.
    sandbox.write(
        "fixture/src/lib.rs",
        format!(
            r#"
#[test]
fn sample() {{}}

/// ```
/// let mut child = std::process::Command::new("/usr/bin/python3")
///     .args([{payload}, "holder", {socket}]).spawn().unwrap();
/// let status = child.wait().unwrap();
/// std::fs::write({waited}, format!("{{:?}}", status.code())).unwrap();
/// assert!(status.success());
/// ```
pub fn controlled_output_holder() {{}}
"#,
            payload = rust_literal(&sandbox.temporary.join("payload.py")),
            socket = rust_literal(&boundary.path),
            waited = rust_literal(&waited),
        ),
    );
    let mut gate =
        sandbox.spawn(&mut sandbox.gate(&[tier, "--part", "suite", "--lease-wait", "0"]));
    let mut worker = boundary.accept(&mut gate);
    let name = if tier == "full" { "cold" } else { "warm" };
    let marker = sandbox.root.join(format!("target/.gate-state/{name}.use"));
    require_extra_marker_and_normal_stdin(&worker, &marker);
    let retained = fs::File::open(&marker).unwrap();
    let target = sandbox.root.join(if tier == "full" {
        "target/gate"
    } else {
        "target/gate-warm"
    });
    gate.signal("TERM");
    let output = gate.finish(Duration::from_secs(10));
    let record = sandbox.result();
    let marker_kept = marker.exists();
    let target_kept = target.is_dir();
    let no_record = !sandbox
        .root
        .join(format!("target/.gate-state/{name}.record"))
        .exists();
    retain_native_control(
        &format!("{tier}-interrupted"),
        &output,
        Some(&record),
        serde_json::json!({
            "marker_kept": marker_kept, "target_kept": target_kept, "no_record": no_record,
            "marker_inode": retained.metadata().unwrap().ino(), "worker": worker.ready,
        }),
    );
    worker.alive();
    assert!(matches!(
        retained.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    // A broken implementation has already unlinked the marker. Do not start
    // another copy of the parked fixture in that case; release before failing.
    let successor = marker_kept
        .then(|| sandbox.run(&mut sandbox.gate(&[tier, "--part", "suite", "--lease-wait", "0"])));
    worker.alive();
    worker.release();
    wait_for(&waited, HANDSHAKE_BUDGET);
    require_successful_worker_wait(&fs::read_to_string(waited).unwrap());
    require_process_gone(&sandbox, worker.ready["pid"].as_u64().unwrap());
    // Blocking acquisition observes the actual last close. Neither PID absence
    // nor EOF authorizes the reset; no retry of a failed nonblocking assertion.
    retained.lock().unwrap();
    drop(retained); // close our independent description, never shared unlock
    output.code(143);
    assert!(marker_kept && target_kept && no_record, "{record}");
    assert!(field(&record, "target").starts_with("kept ("), "{record}");
    assert_eq!(field(&record, "target-finished"), "no");
    let successor = successor.expect("unfinished marker was removed");
    retain_native_control(
        &format!("{tier}-busy"),
        &successor,
        None,
        serde_json::json!({
            "worker_wait_observed": true, "same_inode_last_close_granted": true,
        }),
    );
    successor.code(1);
    successor.contains("marker lock not granted");
    sandbox.write("fixture/src/lib.rs", "#[test]\nfn sample() {}\n");
    let reset = sandbox.run(&mut sandbox.gate(&[tier, "--part", "suite", "--lease-wait", "0"]));
    retain_native_control(
        &format!("{tier}-reset"),
        &reset,
        Some(&sandbox.result()),
        serde_json::json!({}),
    );
    reset.code(0);
    if tier == "pr" {
        assert_eq!(
            field(&sandbox.result(), "target"),
            "reset: previous build did not finish"
        );
    }
    assert!(!marker.exists());
}

#[test]
fn pr_gate_only_term_retains_live_output_holder_until_explicit_release_and_last_close() {
    interrupted_output_holder("pr");
}

#[test]
fn full_gate_only_term_retains_live_output_holder_until_explicit_release_and_last_close() {
    interrupted_output_holder("full");
}

#[test]
fn lease_exec_closed_stderr_keeps_busy_exit_and_does_not_run_command() {
    let sandbox = Sandbox::new();
    let boundary = Boundary::new(&sandbox);
    let mut holder = boundary.exec(&sandbox, "holder");
    let mut peer = boundary.accept(&mut holder);
    let touched = sandbox.temporary.join("must-not-run");
    let mut command = sandbox.gate(&[
        "lease",
        "exec",
        "--purpose",
        "closed-stderr-busy",
        "--lease-wait",
        "0",
        "--",
        "/usr/bin/touch",
    ]);
    command.arg(&touched);
    let output = sandbox
        .spawn_with_closed_pipes(&mut command)
        .finish(COMMAND_BUDGET);
    peer.alive();
    peer.release();
    holder.finish(COMMAND_BUDGET).code(0);
    output.code(75);
    assert!(!touched.exists());
}

#[test]
fn lease_exec_closed_stderr_keeps_signal_exit_and_reaps_resistant_direct_child() {
    let sandbox = Sandbox::new();
    let boundary = Boundary::new(&sandbox);
    let mut command = sandbox.gate(&[
        "lease",
        "exec",
        "--purpose",
        "closed-stderr-term",
        "--lease-wait",
        "0",
        "--",
        "/usr/bin/python3",
    ]);
    command
        .arg(sandbox.temporary.join("payload.py"))
        .arg("resistant-direct")
        .arg(&boundary.path);
    let mut holder = sandbox.spawn_with_closed_pipes(&mut command);
    let mut worker = boundary.accept(&mut holder);
    holder.signal("TERM");
    assert_eq!(worker.response()["forwarded"], 15);
    let output = holder.finish(Duration::from_secs(10));
    require_process_gone(&sandbox, worker.ready["pid"].as_u64().unwrap());
    output.code(143);
    sandbox
        .run(&mut sandbox.gate(&[
            "lease",
            "exec",
            "--purpose",
            "after-closed-stderr-term",
            "--lease-wait",
            "0",
            "--",
            "/usr/bin/true",
        ]))
        .code(0);
}

// PR2 public requesters. Only bootstrap compilation is replaced by copying the
// already-built judge; the actual wrapper, host admission, target lifecycle and
// every miniature suite command run unchanged. No production workload runs.
fn fast_sandbox() -> Sandbox {
    let s = Sandbox::new();
    s.mini("#[test] fn sample() {}\n", true);
    s.write(
        "Cargo.toml",
        "[workspace]\nresolver=\"3\"\nmembers=[\"fixture\",\"xtask\"]\n",
    );
    s.write(
        "xtask/Cargo.toml",
        "[package]\nname=\"xtask\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    );
    s.write("xtask/src/main.rs", "fn main() {}\n");
    s.write("xtask/gate.sh", include_str!("../gate.sh"));
    s.run(s.command("cargo").args(["generate-lockfile", "--offline"]))
        .code(0);
    s.run(s.command("cargo").args(["fmt", "--all"])).code(0);
    s.commit();
    s
}
fn fast_parked_forwarder(s: &Sandbox, boundary: &Boundary) {
    let real = s.cargo_forwarder();
    s.script(&s.temporary.join("bin/cargo"), &format!(r#"#!/usr/bin/python3 -B
import os, pathlib, shutil, subprocess, sys
{parser}
target = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', ''))
sub = cargo_subcommand(sys.argv[1:])
if sub == 'build' and target.name == 'gate-xtask':
    destination=target/'debug/xtask'
    destination.parent.mkdir(parents=True,exist_ok=True)
    source=pathlib.Path({judge:?})
    if not destination.exists() or destination.read_bytes() != source.read_bytes():
        temporary=destination.with_name('xtask.'+str(os.getpid())+'.tmp')
        with temporary.open('xb') as output:
            output.write(source.read_bytes())
        temporary.chmod(0o700)
        temporary.replace(destination)
    sys.exit(0)
if sub == 'build' and target.name == 'gate-warm':
    try: pathlib.Path({once:?}).mkdir()
    except FileExistsError: pass
    else:
        result=subprocess.run(['/usr/bin/python3','-B',{payload:?},'holder',{socket:?}],close_fds=False)
        if result.returncode: sys.exit(result.returncode)
os.execv({real:?},[{real:?}]+sys.argv[1:])
"#, parser=CARGO_SUBCOMMAND_PY, judge=env!("CARGO_BIN_EXE_xtask"), once=s.temporary.join("fast-build-entered").to_str().unwrap(), payload=s.temporary.join("payload.py").to_str().unwrap(), socket=boundary.path.to_str().unwrap(), real=real.to_str().unwrap()));
}
fn wrapped_fast(s: &Sandbox, lease: &Path, wait: &str) -> std::process::Command {
    let mut cmd = s.command("/bin/bash");
    cmd.arg(s.root.join("xtask/gate.sh"))
        .args(["fast", "--base", "HEAD", "--lease-wait", wait])
        .env("XTASK_GATE_LEASE_DIR", lease);
    cmd
}
#[test]
fn real_wrapped_fast_shared_holders_coexist_and_same_target_refuses_tv61_rs1() {
    let a = fast_sandbox();
    let b = fast_sandbox();
    let ba = Boundary::new(&a);
    let bb = Boundary::new(&b);
    fast_parked_forwarder(&a, &ba);
    fast_parked_forwarder(&b, &bb);
    let lease = a.temporary.join("lease");
    let mut first = a.spawn(&mut wrapped_fast(&a, &lease, "0"));
    let mut p = ba.accept(&mut first);
    // Clap accepts global options before the tier, too. That spelling must not
    // silently restore an exclusive bootstrap in front of a shared fast run.
    let mut options_first = b.command("/bin/bash");
    options_first
        .arg(b.root.join("xtask/gate.sh"))
        .args([
            "--lease-wait",
            "0",
            "--host-label",
            "fast",
            "--base",
            "HEAD",
            "--",
            "fast",
        ])
        .env("XTASK_GATE_LEASE_DIR", &lease);
    let mut second = b.spawn(&mut options_first);
    let admitted =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bb.accept(&mut second)));
    let mut q = match admitted {
        Ok(peer) => peer, // Must arrive while first is held, not afterwards.
        Err(error) => {
            p.release();
            first.finish(COMMAND_BUDGET).code(0);
            drop(second);
            std::panic::resume_unwind(error);
        }
    };
    p.alive();
    q.alive();
    let mut labelled_pr = b.command("/bin/bash");
    labelled_pr
        .arg(b.root.join("xtask/gate.sh"))
        .args(["--host-label", "fast", "--lease-wait", "0", "pr"])
        .env("XTASK_GATE_LEASE_DIR", &lease);
    let exclusive = b.run(&mut labelled_pr);
    exclusive.code(75);
    exclusive.contains("bootstrap cooperative lease busy");
    let refused = a.run(&mut wrapped_fast(&a, &lease, "0"));
    refused.code(1);
    refused.contains("warm target in use");
    assert!(a.root.join("target/.gate-state/warm.use").is_file());
    p.alive();
    q.alive();
    p.release();
    q.release();
    first.finish(COMMAND_BUDGET).code(0);
    second.finish(COMMAND_BUDGET).code(0);
    for s in [&a, &b] {
        let record = s.result();
        // A refused same-target request can complete before the first request;
        // the first's completed result is now the latest.
        assert_eq!(field(&record, "lease"), "shared");
        assert_eq!(field(&record, "target-finished"), "yes");
        assert!(!s.root.join("target/.gate-state/warm.use").exists());
    }
}
#[test]
fn real_fast_newcomer_queues_behind_observed_exclusive_waiter_tv62() {
    use std::fs::TryLockError;
    let a = fast_sandbox();
    let b = fast_sandbox();
    let ex = Sandbox::new();
    let ba = Boundary::new(&a);
    let bb = Boundary::new(&b);
    let be = Boundary::new(&ex);
    fast_parked_forwarder(&a, &ba);
    fast_parked_forwarder(&b, &bb);
    let lease = a.temporary.join("lease");
    let mut first = a.spawn(&mut wrapped_fast(&a, &lease, "0"));
    let mut p = ba.accept(&mut first);
    let turnstile = fs::File::open(lease.join("r-orch-gate.turnstile")).unwrap();
    turnstile.try_lock().unwrap();
    drop(turnstile); // Free before exclusive request.
    let mut cmd = ex.gate(&[
        "lease",
        "exec",
        "--purpose",
        "queued-exclusive",
        "--lease-wait",
        "1",
        "--",
        "/usr/bin/python3",
    ]);
    cmd.arg(ex.temporary.join("payload.py"))
        .arg("holder")
        .arg(&be.path)
        .env("XTASK_GATE_LEASE_DIR", &lease);
    let mut exclusive = ex.spawn(&mut cmd);
    let turnstile = fs::File::open(lease.join("r-orch-gate.turnstile")).unwrap();
    let deadline = Instant::now() + HANDSHAKE_BUDGET;
    loop {
        assert!(
            exclusive.child.try_wait().unwrap().is_none(),
            "{}",
            exclusive.diagnostic()
        );
        match turnstile.try_lock() {
            Err(TryLockError::WouldBlock) => break,
            Ok(()) => {
                turnstile.unlock().unwrap();
            }
            Err(e) => panic!("turnstile observation: {e:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "exclusive waiter did not acquire turnstile"
        );
        std::thread::park_timeout(POLL_INTERVAL);
    }
    // A real fast request now reaches and refuses at the held turnstile.
    let probe = b.run(
        b.gate(&["fast", "--lease-wait", "0"])
            .env("XTASK_GATE_LEASE_DIR", &lease),
    );
    probe.code(75);
    probe.contains("stage=admission-turnstile");
    let mut newcomer = b.spawn(&mut wrapped_fast(&b, &lease, "1"));
    // This same request acknowledges an actual EWOULDBLOCK before the first
    // shared holder is released; mere process liveness would not prove arrival.
    wait_bootstrap_stage(&mut newcomer, "admission-turnstile");
    p.alive();
    assert!(exclusive.child.try_wait().unwrap().is_none());
    assert!(newcomer.child.try_wait().unwrap().is_none());
    p.release();
    first.finish(COMMAND_BUDGET).code(0);
    let mut e = be.accept(&mut exclusive); // Actual exclusive payload owns the lease.
    e.alive();
    wait_bootstrap_stage(&mut newcomer, "lease-shared");
    assert!(newcomer.child.try_wait().unwrap().is_none());
    let file = fs::File::open(lease.join("r-orch-gate.lease")).unwrap();
    assert!(matches!(
        file.try_lock_shared(),
        Err(TryLockError::WouldBlock)
    ));
    assert!(
        matches!(bb.listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock),
        "new shared payload overtook exclusive holder"
    );
    e.release();
    exclusive.finish(COMMAND_BUDGET).code(0);
    let mut q = bb.accept(&mut newcomer);
    q.alive();
    q.release();
    newcomer.finish(COMMAND_BUDGET).code(0);
}

fn wait_bootstrap_stage(child: &mut OwnedChild, stage: &str) {
    let deadline = Instant::now() + HANDSHAKE_BUDGET;
    let message = format!("bootstrap: waiting stage={stage}");
    loop {
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "{}",
            child.diagnostic()
        );
        if child.diagnostic().contains(&message) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "missing real wait handshake {message}: {}",
            child.diagnostic()
        );
        std::thread::park_timeout(POLL_INTERVAL);
    }
}
