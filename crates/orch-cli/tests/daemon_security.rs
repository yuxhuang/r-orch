//! Binary-level permission and environment vectors (T1; design D11, D12
//! `daemon_security::uid_permissions_and_symlinks`; spec R7, R8, R21). The
//! in-process half (real-socket modes, discovery fields, log hygiene) is in
//! `orch-daemon/tests/daemon_security.rs`.

mod daemon_support;

use daemon_support::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

/// Re-run `cmd` under `/bin/sh` with the given umask. Keeps `unsafe` (and
/// `pre_exec`) out of the test crate: the shell sets the umask, then `exec`s
/// the very same program with the very same cleared environment.
fn under_umask(cmd: &Command, umask: &str) -> Command {
    let mut wrapped = Command::new("/bin/sh");
    wrapped
        .env_clear()
        .arg("-c")
        .arg(format!("umask {umask}; exec \"$0\" \"$@\""))
        .arg(cmd.get_program())
        .args(cmd.get_args())
        .current_dir(cmd.get_current_dir().unwrap());
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            wrapped.env(key, value);
        }
    }
    std::os::unix::process::CommandExt::process_group(&mut wrapped, 0);
    wrapped
}

/// Whatever the owner's umask, the run directory is 0700, the socket 0600 and
/// the discovery record 0600; `.orchestry` keeps the mode its owner gave it
/// (the daemon never chmods a directory it did not create).
#[test]
fn files_are_private_under_any_umask_and_the_state_dir_is_not_chmodded() {
    for umask in ["077", "022", "000"] {
        let sandbox = Sandbox::new();
        sandbox.init_project();
        // Default endpoint (no ORCH_SOCKET): `<root>/.orchestry/run/orch.sock`.
        let state_mode = mode(&sandbox.state_dir());
        fs::set_permissions(sandbox.state_dir(), fs::Permissions::from_mode(0o750)).unwrap();
        let mut cmd = sandbox.start_cmd(&[]);
        cmd.env_remove("ORCH_SOCKET");
        let mut daemon =
            DaemonChild::spawn(under_umask(&cmd, umask), sandbox.short_dir().join("d.err"));
        let socket = match daemon.outcome(READY) {
            Outcome::Ready(line) => {
                std::path::PathBuf::from(line.strip_prefix("listening ").unwrap())
            }
            Outcome::Exited(status) => {
                panic!("umask {umask}: exited ({status}): {}", daemon.stderr())
            }
        };
        assert_eq!(
            socket,
            sandbox.state_dir().join("run/orch.sock"),
            "umask {umask}"
        );
        assert_eq!(
            mode(&sandbox.state_dir().join("run")),
            0o700,
            "umask {umask}: run dir"
        );
        assert_eq!(mode(&socket), 0o600, "umask {umask}: socket");
        assert_eq!(
            mode(&sandbox.state_dir().join("daemon.json")),
            0o600,
            "umask {umask}: daemon.json"
        );
        assert_eq!(
            mode(&sandbox.state_dir().join(".daemon-owner.lock")),
            0o600,
            "umask {umask}: guard"
        );
        assert_eq!(
            mode(&sandbox.state_dir()),
            0o750,
            "umask {umask}: .orchestry was chmodded (was {state_mode:o})"
        );
        // And a client with an ordinary environment still gets in.
        let mut client = sandbox.orch();
        client
            .env_remove("ORCH_SOCKET")
            .env("ORCH_TEST_SOCKET_ROOT", sandbox.root())
            .args(["--json", "daemon", "status"]);
        let out = output_with_deadline(client, CALL);
        assert!(out.status.success(), "umask {umask}: {}", text(&out.stderr));
    }
}

/// Start `cmd` under umask 000 and return the endpoint it reports.
fn start_under_open_umask(
    cmd: &Command,
    err: std::path::PathBuf,
) -> (DaemonChild, std::path::PathBuf) {
    let mut daemon = DaemonChild::spawn(under_umask(cmd, "000"), err);
    let socket = match daemon.outcome(READY) {
        Outcome::Ready(line) => std::path::PathBuf::from(line.strip_prefix("listening ").unwrap()),
        Outcome::Exited(status) => panic!("exited ({status}): {}", daemon.stderr()),
    };
    (daemon, socket)
}

/// An `ORCH_SOCKET` whose parent does not exist yet: the daemon creates that
/// parent 0700, binds, and leaves the socket 0600 and the record 0600 even
/// under umask 000 (sec-eng F2 equivalence, conformance condition: every
/// endpoint parent passes `prepare_private_dir` before bind).
#[test]
fn an_orch_socket_override_in_a_new_parent_is_private_under_umask_000() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let parent = sandbox.short_dir().join("fresh");
    let wanted = parent.join("s");
    let mut cmd = sandbox.start_cmd(&[]);
    cmd.env("ORCH_SOCKET", &wanted);
    assert!(!parent.exists());
    let (_daemon, socket) = start_under_open_umask(&cmd, sandbox.short_dir().join("d.err"));
    assert_eq!(socket, wanted);
    assert_eq!(mode(&parent), 0o700, "override parent");
    assert_eq!(mode(&socket), 0o600, "override socket");
    assert_eq!(
        mode(&sandbox.state_dir().join("daemon.json")),
        0o600,
        "record"
    );
}

/// A root too long for the in-project default falls back to
/// `$TMPDIR/orch-<hash>/orch.sock`: the fallback parent is created 0700, the
/// socket ends 0600 and the record 0600 under umask 000.
#[test]
fn a_tmpdir_fallback_endpoint_is_private_under_umask_000() {
    let base = tempfile::Builder::new()
        .prefix("dq-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap();
    let canonical = base.path().canonicalize().unwrap();
    // One byte past the in-project budget: `<root>/.orchestry/run/orch.sock` > 100.
    let root_len = 100 - "/.orchestry/run/orch.sock".len() + 1;
    let pad = root_len - (canonical.as_os_str().len() + 1);
    let root = canonical.join("x".repeat(pad));
    let tmp = canonical.join("tmp");
    for dir in [&root, &tmp, &root.join("home")] {
        fs::create_dir(dir).unwrap();
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    let mut cmd = orch_command(&root, Path::new("/unused"), &canonical);
    cmd.env_remove("ORCH_SOCKET").env("TMPDIR", &tmp);
    let mut init = Command::new(cmd.get_program());
    init.env_clear()
        .current_dir(&root)
        .args(["init", "--name", "proj", "--adapter", "shell"]);
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            init.env(key, value);
        }
    }
    let out = output_with_deadline(init, CALL);
    assert!(out.status.success(), "{}", text(&out.stderr));

    cmd.args(["daemon", "start"]);
    let (_daemon, socket) = start_under_open_umask(&cmd, canonical.join("d.err"));
    let parent = socket.parent().unwrap();
    assert_eq!(parent.parent().unwrap(), tmp, "not the TMPDIR fallback");
    assert_eq!(mode(parent), 0o700, "fallback parent");
    assert_eq!(mode(&socket), 0o600, "fallback socket");
    assert_eq!(mode(&root.join(".orchestry/daemon.json")), 0o600, "record");
}

/// A group- or world-writable `.orchestry` is refused (exit 2) and left as it
/// was: the daemon will not guess which bits the owner meant.
#[test]
fn a_writable_state_dir_is_refused_and_not_chmodded() {
    for bits in [0o770, 0o757] {
        let sandbox = Sandbox::new();
        sandbox.init_project();
        fs::set_permissions(sandbox.state_dir(), fs::Permissions::from_mode(bits)).unwrap();
        let out = sandbox.run(&["daemon", "start"]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{bits:o}: {}",
            text(&out.stderr)
        );
        assert!(!text(&out.stdout).contains("listening"));
        assert_eq!(mode(&sandbox.state_dir()), bits, "mode changed");
        assert!(!sandbox.state_dir().join("daemon.json").exists());
        assert!(!sandbox.socket().exists());
        fs::set_permissions(sandbox.state_dir(), fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// A symlinked run directory is not followed: the daemon refuses to bind
/// through it and nothing appears at the link's target.
#[test]
fn a_symlinked_run_directory_is_refused_and_its_target_untouched() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let target = sandbox.short_dir().join("elsewhere");
    fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, sandbox.state_dir().join("run")).unwrap();
    let mut cmd = sandbox.start_cmd(&[]);
    cmd.env_remove("ORCH_SOCKET");
    let out = output_with_deadline(cmd, CALL);
    assert_ne!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("listening"));
    assert_eq!(
        fs::read_dir(&target).unwrap().count(),
        0,
        "bound through the symlink"
    );
    assert!(
        fs::symlink_metadata(sandbox.state_dir().join("run"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// Debugger switches and credentials the owner's shell happened to carry never
/// reach an agent child, and the daemon reports only the *names* it removed.
/// The agent sees the daemon's own socket and project, not the ambient ones.
#[test]
fn the_start_environment_is_sanitised_before_any_agent_sees_it() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let out_file = sandbox.short_dir().join("child-env");
    sandbox.seed_agent_task(
        "envdump",
        &format!("/usr/bin/env > '{}'", out_file.display()),
        "dump env",
    );

    let mut cmd = sandbox.start_cmd(&["--dispatch", "on"]);
    cmd.env("DEBUG", "*")
        .env("PWDEBUG", "1")
        .env("NODE_OPTIONS", "--inspect=127.0.0.1:9229")
        .env("ORCH_WEB_PASSWORD", "hunter2-canary")
        .env("KEEP_ME", "visible");
    let mut daemon = DaemonChild::spawn(cmd, sandbox.short_dir().join("d.err"));
    daemon.expect_ready(&sandbox.socket());

    let dumped = wait_for(READY, "the agent to dump its environment", || {
        fs::read_to_string(&out_file)
            .ok()
            .filter(|body| body.contains("PATH="))
    });
    let lines: Vec<&str> = dumped.lines().collect();
    let has = |prefix: &str| lines.iter().any(|l| l.starts_with(prefix));
    assert!(!has("DEBUG="), "DEBUG reached the agent:\n{dumped}");
    assert!(!has("PWDEBUG="), "PWDEBUG reached the agent:\n{dumped}");
    assert!(
        !has("NODE_OPTIONS="),
        "an inspector flag reached the agent:\n{dumped}"
    );
    assert!(
        !has("ORCH_WEB_PASSWORD="),
        "the web password reached the agent:\n{dumped}"
    );
    assert!(!dumped.contains("hunter2-canary"), "{dumped}");
    assert!(
        lines.contains(&"KEEP_ME=visible"),
        "ordinary variables must survive:\n{dumped}"
    );
    assert!(
        lines.contains(&format!("ORCH_SOCKET={}", sandbox.socket().display()).as_str()),
        "the child must be routed to this daemon:\n{dumped}"
    );
    assert!(
        lines.contains(&format!("ORCH_PROJECT_ROOT={}", sandbox.root().display()).as_str()),
        "{dumped}"
    );

    // Names only, never values, in status and in the discovery record.
    let status = sandbox.run(&["--json", "daemon", "status"]);
    assert!(status.status.success(), "{}", text(&status.stderr));
    let body = text(&status.stdout);
    let status: serde_json::Value = serde_json::from_str(&body).unwrap();
    let removed: Vec<&str> = status["removed_env"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for name in ["DEBUG", "PWDEBUG", "NODE_OPTIONS", "ORCH_WEB_PASSWORD"] {
        assert!(removed.contains(&name), "{name} missing from {removed:?}");
    }
    assert!(
        !body.contains("hunter2-canary") && !body.contains("--inspect"),
        "{body}"
    );
    let record = fs::read_to_string(sandbox.state_dir().join("daemon.json")).unwrap();
    assert!(
        !record.contains("hunter2-canary") && !record.contains("--inspect"),
        "{record}"
    );
}
