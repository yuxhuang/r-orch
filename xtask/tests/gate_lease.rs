//! Lease processes use only private lease directories and self-bounded workers.
//! No test signals a discovered PID or touches the cooperative host lease.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use witness::fixture::{
    CARGO_SUBCOMMAND_PY, COMMAND_BUDGET, HANDSHAKE_BUDGET, OwnedChild, POLL_INTERVAL, Sandbox,
    field,
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
connection.sendall((json.dumps({'role': mode, 'pid': os.getpid(), 'token': os.environ.get('XTASK_GATE_LEASE_TOKEN', '')}) + '\n').encode())
child = None
try:
    if mode == 'resistant':
        child = subprocess.Popen([sys.executable, __file__, 'detached', address], start_new_session=True)
    while True:
        if mode == 'resistant-direct':
            connection.settimeout(None)
            line = reader.readline(65536)
            if not line or json.loads(line)['op'] == 'release': break
            continue
        deadline = time.monotonic() + 30
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            connection.sendall(b'{"expired":true}\n')
            break
        connection.settimeout(remaining)
        try:
            line = reader.readline(65536)
        except (TimeoutError, socket.timeout):
            connection.sendall(b'{"expired":true}\n')
            break
        if not line: break
        request = json.loads(line)
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
        // The 30 s idle deadline and per-nested 30 s runtime are separate.
        // EOF/self-expiry are not death/cleanup authority; lifecycle runs held.
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
    let mut peer = boundary.accept(&mut holder);
    let lock = sandbox.temporary.join("lease/r-orch-gate.lease");
    fs::rename(&lock, sandbox.temporary.join("lease/old-owned-lock")).unwrap();
    fs::write(&lock, "").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    peer.release();
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
        holder.signal("KILL");
        let _ = holder.finish(HANDSHAKE_BUDGET);
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
        let pid = writer.ready["pid"].as_u64().unwrap().to_string();
        assert!(
            output
                .text()
                .split(|c: char| !c.is_ascii_digit())
                .any(|token| token == pid),
            "missing exact live writer PID: {}",
            output.text()
        );
        writer.release();
    }
}

// Required PR2 obligations: TV61/62 and shared-fast RS1, once fast is real.
// No fake --shared requester or permanent ignore is introduced here.
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
    holder.signal("KILL");
    let _ = holder.finish(HANDSHAKE_BUDGET);
    worker.release();
    worker.require_closed_without_self_expiry();
    fs::remove_file(sandbox.temporary.join("bin/cargo")).unwrap();
    symlink(real_cargo, sandbox.temporary.join("bin/cargo")).unwrap();
    sandbox.suite().code(0);
    assert_eq!(
        field(&sandbox.result(), "target"),
        "reset: previous build did not finish"
    );
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
