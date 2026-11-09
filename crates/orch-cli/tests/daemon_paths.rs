//! Endpoint selection by a real daemon child (T1; design D4, D12
//! `daemon_paths::long_root_override_and_wrong_project`; spec R2, R33).
//!
//! Each test owns its project directory and `TMPDIR` under a short private
//! `/tmp/dp-*` base, passes everything to the child through `env_clear`ed
//! environments, and confines the client with `ORCH_TEST_SOCKET_ROOT`. No
//! test touches the developer's `TMPDIR`, home, projects or daemons.

mod daemon_support;

use daemon_support::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `len("/.orchestry/run/orch.sock")`.
const SUFFIX: usize = 25;
const BUDGET: usize = 100;

/// A private base with a project root of an exact byte length and its own
/// `TMPDIR`, everything canonical.
struct PathProject {
    _base: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    tmp: PathBuf,
}

impl PathProject {
    /// `root_len` is the byte length of the canonical project root.
    fn new(root_len: usize) -> PathProject {
        let base = tempfile::Builder::new()
            .prefix("dp-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        let canonical = base.path().canonicalize().unwrap();
        let pad = root_len
            .checked_sub(canonical.as_os_str().len() + 1)
            .filter(|n| *n >= 1)
            .expect("base path is already longer than the requested root");
        let root = canonical.join("x".repeat(pad));
        fs::create_dir(&root).unwrap();
        assert_eq!(root.as_os_str().len(), root_len);
        let tmp = canonical.join("tmp");
        fs::create_dir(&tmp).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        let project = PathProject {
            base: canonical,
            root,
            tmp,
            _base: base,
        };
        let out = project.run(
            &project.cmd(),
            &["init", "--name", "proj", "--adapter", "shell"],
        );
        assert!(out.status.success(), "{}", text(&out.stderr));
        project
    }

    /// A child command with no endpoint override: discovery, default
    /// endpoint and `TMPDIR` fallback decide, as for a real owner.
    fn cmd(&self) -> Command {
        let mut cmd = orch_command(&self.root, Path::new("/unused"), &self.base);
        cmd.env_remove("ORCH_SOCKET").env("TMPDIR", &self.tmp);
        cmd
    }

    fn run(&self, cmd: &Command, args: &[&str]) -> std::process::Output {
        let mut next = Command::new(cmd.get_program());
        // `get_envs` lists only explicit changes: without `env_clear` the child
        // would inherit this test process's environment (which may belong to
        // an agent run), so every copy starts cleared.
        next.env_clear()
            .args(args)
            .current_dir(cmd.get_current_dir().unwrap());
        for (key, value) in cmd.get_envs() {
            match value {
                Some(value) => next.env(key, value),
                None => next.env_remove(key),
            };
        }
        next.process_group(0);
        output_with_deadline(next, CALL)
    }

    fn start(&self, cmd: Command, tag: &str) -> DaemonChild {
        let mut cmd = cmd;
        cmd.args(["daemon", "start"]);
        DaemonChild::spawn(cmd, self.base.join(format!("{tag}.err")))
    }

    fn state_dir(&self) -> PathBuf {
        self.root.join(".orchestry")
    }
}

use std::os::unix::process::CommandExt;

fn listening(daemon: &mut DaemonChild) -> PathBuf {
    match daemon.outcome(READY) {
        Outcome::Ready(line) => PathBuf::from(line.strip_prefix("listening ").unwrap()),
        Outcome::Exited(status) => panic!("start exited early ({status}): {}", daemon.stderr()),
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

// -- the 100/101 byte boundary ---------------------------------------------

/// A default endpoint of exactly 100 bytes is used as is, inside the project,
/// in a private run directory; nothing is created under `TMPDIR`.
#[test]
fn a_default_endpoint_of_exactly_100_bytes_stays_in_the_project() {
    let project = PathProject::new(BUDGET - SUFFIX);
    let mut daemon = project.start(project.cmd(), "d");
    let socket = listening(&mut daemon);
    assert_eq!(socket, project.state_dir().join("run/orch.sock"));
    assert_eq!(socket.as_os_str().len(), BUDGET);
    assert_eq!(mode(&project.state_dir().join("run")), 0o700);
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(
        fs::read_dir(&project.tmp).unwrap().count(),
        0,
        "TMPDIR was used"
    );
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(project.state_dir().join("daemon.json")).unwrap())
            .unwrap();
    assert_eq!(record["socket"]["utf8"], socket.to_str().unwrap());
}

/// One byte more and the default moves to `$TMPDIR/orch-<hash>/orch.sock` in a
/// private directory; clients still find it through `daemon.json`, whatever
/// their own `TMPDIR` is.
#[test]
fn a_default_endpoint_of_101_bytes_falls_back_to_a_private_tmpdir_hash_dir() {
    let project = PathProject::new(BUDGET - SUFFIX + 1);
    let mut daemon = project.start(project.cmd(), "d");
    let socket = listening(&mut daemon);

    assert_eq!(socket.parent().unwrap().parent().unwrap(), project.tmp);
    let dir = socket
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let hash = dir.strip_prefix("orch-").expect("orch-<hash>");
    assert_eq!(hash.len(), 16);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{hash}"
    );
    assert_eq!(socket.file_name().unwrap(), "orch.sock");
    assert_eq!(mode(socket.parent().unwrap()), 0o700);
    assert_eq!(mode(&socket), 0o600);
    assert!(
        !project.state_dir().join("run/orch.sock").exists(),
        "a socket was also bound in the project"
    );

    // A client with an unrelated TMPDIR reads the published endpoint.
    let mut client = project.cmd();
    client.env("TMPDIR", "/nonexistent-tmpdir");
    let out = project.run(&client, &["--json", "daemon", "status"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["pid"], daemon.pid);
    assert_eq!(status["socket"]["utf8"], socket.to_str().unwrap());
}

/// The fallback name depends only on the project root: the same root maps to
/// the same directory on every start, and different roots to different ones.
#[test]
fn the_fallback_directory_is_a_stable_function_of_the_root() {
    let one = PathProject::new(BUDGET - SUFFIX + 1);
    let two = PathProject::new(BUDGET - SUFFIX + 1);
    let name = |project: &PathProject| {
        let mut daemon = project.start(project.cmd(), "d");
        let socket = listening(&mut daemon);
        let out = project.run(&project.cmd(), &["daemon", "stop"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        // Reaps the child; a bare liveness probe would see its zombie.
        assert!(daemon.wait_exit(READY).success());
        socket.parent().unwrap().file_name().unwrap().to_owned()
    };
    let first = name(&one);
    assert_eq!(first, name(&one), "same root, different directory");
    assert_ne!(first, name(&two), "different roots collided");
}

/// When even the fallback would exceed 100 bytes the start fails closed with
/// the `ORCH_SOCKET` hint (exit 2), truncating nothing and creating nothing.
#[test]
fn a_tmpdir_too_long_for_the_fallback_fails_with_the_override_hint() {
    let mut project = PathProject::new(BUDGET - SUFFIX + 1);
    let long_tmp = project
        .tmp
        .join("y".repeat(BUDGET - project.tmp.as_os_str().len() - 6));
    fs::create_dir(&long_tmp).unwrap();
    project.tmp = long_tmp;
    let out = project.run(
        &{
            let mut c = project.cmd();
            c.args(["daemon", "start"]);
            c
        },
        &["daemon", "start"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("ORCH_SOCKET"), "{err}");
    assert!(!text(&out.stdout).contains("listening"));
    assert_eq!(
        fs::read_dir(&project.tmp).unwrap().count(),
        0,
        "a fallback dir was created"
    );
    assert!(!project.state_dir().join("daemon.json").exists());
}

// -- ORCH_SOCKET -----------------------------------------------------------

/// An explicit endpoint beats the in-project default and the fallback, at
/// both sides of the boundary, and neither default location is created.
#[test]
fn orch_socket_overrides_both_defaults() {
    for root_len in [BUDGET - SUFFIX, BUDGET - SUFFIX + 1] {
        let project = PathProject::new(root_len);
        let socket = project.base.join("s");
        let mut cmd = project.cmd();
        cmd.env("ORCH_SOCKET", &socket);
        let mut daemon = project.start(cmd, "d");
        assert_eq!(listening(&mut daemon), socket, "root length {root_len}");
        assert!(!project.state_dir().join("run/orch.sock").exists());
        assert_eq!(fs::read_dir(&project.tmp).unwrap().count(), 0);
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(project.state_dir().join("daemon.json")).unwrap())
                .unwrap();
        assert_eq!(record["socket"]["utf8"], socket.to_str().unwrap());
    }
}

/// Unusable overrides are errors, never silently replaced by a default.
#[test]
fn unusable_orch_socket_values_are_refused() {
    let project = PathProject::new(40);
    let too_long = project
        .base
        .join("z".repeat(BUDGET + 1 - project.base.as_os_str().len() - 1));
    assert_eq!(too_long.as_os_str().len(), BUDGET + 1);
    for (label, value) in [
        ("relative", "relative/orch.sock".to_owned()),
        ("empty", String::new()),
        ("101 bytes", too_long.to_str().unwrap().to_owned()),
    ] {
        let mut cmd = project.cmd();
        cmd.env("ORCH_SOCKET", &value);
        cmd.args(["daemon", "start"]);
        let out = output_with_deadline(cmd, CALL);
        assert_eq!(out.status.code(), Some(2), "{label}: {}", text(&out.stderr));
        assert!(!text(&out.stdout).contains("listening"), "{label}");
        assert!(!project.state_dir().join("daemon.json").exists(), "{label}");
        assert!(
            !project.state_dir().join("run").exists(),
            "{label}: fell back to the default"
        );
    }
}

// -- wrong project ---------------------------------------------------------

/// Pointing project B's client at project A's endpoint is `wrong_project`: both
/// roots are named, exit 7, and neither a status nor a stop touches A.
#[test]
fn a_client_for_another_project_is_told_wrong_project_and_stops_nothing() {
    let a = Sandbox::new();
    a.init_project();
    let owner = start_ready(&a, &[], "a");
    let b = Sandbox::new();
    b.init_project();

    for args in [&["daemon", "status"][..], &["daemon", "stop"][..]] {
        let mut cmd = b.orch();
        cmd.env("ORCH_SOCKET", a.socket())
            .env("ORCH_TEST_SOCKET_ROOT", "/tmp")
            .args(args);
        let out = output_with_deadline(cmd, CALL);
        assert_eq!(
            out.status.code(),
            Some(7),
            "{args:?}: {}",
            text(&out.stderr)
        );
        let err = text(&out.stderr);
        assert!(err.contains("wrong_project"), "{err}");
        assert!(
            err.contains(b.root().to_str().unwrap()),
            "expected root missing: {err}"
        );
        assert!(
            err.contains(a.root().to_str().unwrap()),
            "served root missing: {err}"
        );
        assert!(
            !err.contains("kill") && !err.contains("rm "),
            "no destructive hint: {err}"
        );
    }
    assert!(is_alive(owner.pid), "a wrong-project stop reached A");
    assert_eq!(a.status().unwrap().pid, owner.pid);
}

/// With no override, B's client uses B's own discovery file: A's daemon is
/// invisible to it, and the failure is B's NoDaemon, not an answer from A.
#[test]
fn without_an_override_a_client_never_reaches_another_projects_daemon() {
    let a = Sandbox::new();
    a.init_project();
    let owner = start_ready(&a, &[], "a");
    let b = Sandbox::new();
    b.init_project();
    let mut cmd = b.orch();
    cmd.env_remove("ORCH_SOCKET").args(["daemon", "status"]);
    let out = output_with_deadline(cmd, CALL);
    assert_eq!(out.status.code(), Some(7), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("No orch daemon is running"), "{err}");
    assert!(err.contains(b.root().to_str().unwrap()), "{err}");
    assert!(is_alive(owner.pid));
}

// -- collision simulation ---------------------------------------------------

/// Learn the fallback endpoint a project would use by running it once.
fn learn_fallback(project: &PathProject) -> PathBuf {
    let mut daemon = project.start(project.cmd(), "probe");
    let socket = listening(&mut daemon);
    let out = project.run(&project.cmd(), &["daemon", "stop"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(daemon.wait_exit(READY).success());
    assert!(!socket.exists());
    socket
}

/// Two roots mapping to one fallback directory cannot be forced (the hash is
/// 64-bit), so the outcome of a collision is simulated: a different owner's
/// live listener already sits on the endpoint this project would bind. The
/// start must refuse and leave that listener alone.
#[test]
fn a_foreign_live_listener_at_the_fallback_endpoint_is_never_replaced() {
    let project = PathProject::new(BUDGET - SUFFIX + 1);
    let socket = learn_fallback(&project);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    fs::set_permissions(socket.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let inode = fs::metadata(&socket).unwrap().ino();

    let out = project.run(
        &{
            let mut c = project.cmd();
            c.args(["daemon", "start"]);
            c
        },
        &["daemon", "start"],
    );
    assert_ne!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("listening"));
    assert_eq!(
        fs::metadata(&socket).unwrap().ino(),
        inode,
        "the foreign socket was replaced"
    );
    assert!(UnixListener::bind(&socket).is_err(), "still occupied");
    assert!(!project.state_dir().join("daemon.json").exists());
    drop(listener);
}

/// The other collision outcome: the directory exists but is not private (or
/// not ours). It is neither used nor chmodded; the start fails with exit 2.
#[test]
fn a_fallback_directory_that_is_not_private_is_refused_and_left_unchanged() {
    let project = PathProject::new(BUDGET - SUFFIX + 1);
    let socket = learn_fallback(&project);
    let dir = socket.parent().unwrap();
    fs::create_dir_all(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();

    let out = project.run(
        &{
            let mut c = project.cmd();
            c.args(["daemon", "start"]);
            c
        },
        &["daemon", "start"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(!text(&out.stdout).contains("listening"));
    assert_eq!(mode(dir), 0o755, "the directory was chmodded");
    assert!(!socket.exists());
    assert!(!project.state_dir().join("daemon.json").exists());
}
