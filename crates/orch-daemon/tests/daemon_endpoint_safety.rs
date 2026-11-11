#![cfg(feature = "testing")]
//! What an explicit start may do to whatever already sits at its socket path
//! (design D4, spec R15; board 1 code C2 = security F4).
//!
//! Only a probe that proves nobody listens (`ECONNREFUSED` on a same-owner
//! socket) authorises the start to unlink the path. A listener the probe
//! cannot reach (here: a live same-uid listener chmodded to 000, so the
//! connect fails with `EACCES`) is indeterminate and must be left alone.
//!
//! Each case runs a real `orch_daemon::prepare_start` + `run` in a child
//! process (this test binary re-executed) so the start reads a private HOME
//! and TMPDIR and no agent identity, never the caller's environment.

use std::io::{BufRead, BufReader};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const CHILD_ROOT: &str = "ORCH_ENDPOINT_SAFETY_CHILD_ROOT";
const CHILD_SOCKET: &str = "ORCH_ENDPOINT_SAFETY_CHILD_SOCKET";
/// Generous: a refusing start still builds the engine and runs recovery
/// before it classifies the endpoint.
const START_BUDGET: Duration = Duration::from_secs(60);

/// The child half: an explicit foreground start of the project in
/// `CHILD_ROOT` on the endpoint `CHILD_SOCKET`, exiting with the start's own
/// code. A no-op when run as an ordinary test.
#[test]
fn endpoint_safety_child() {
    let (Some(root), Some(socket)) = (std::env::var_os(CHILD_ROOT), std::env::var_os(CHILD_SOCKET))
    else {
        return;
    };
    let options = orch_daemon::StartOptions {
        project_root: PathBuf::from(root),
        socket: Some(PathBuf::from(socket)),
        detach: false,
        dispatch: orch_daemon::DispatchMode::Off,
        from_agent: false,
        listen: None,
        peer_name: None,
    };
    let code = match orch_daemon::prepare_start(options) {
        Err(error) => {
            eprintln!("start failed: {error}");
            error.exit_code()
        }
        Ok(prepared) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            match runtime.block_on(prepared.run()) {
                Ok(exit) => exit.code,
                Err(error) => {
                    eprintln!("start failed: {error}");
                    error.exit_code()
                }
            }
        }
    };
    std::process::exit(code);
}

/// A temp dir with a short path (the socket must stay under 100 bytes) and
/// mode 0700, as a socket parent must be.
fn short_private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oes-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}

struct Case {
    project: tempfile::TempDir,
    home: tempfile::TempDir,
    endpoint_dir: tempfile::TempDir,
}

impl Case {
    fn new() -> Case {
        let project = tempfile::tempdir().unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(project.path().join(".orchestry"))
            .unwrap();
        Case {
            project,
            home: tempfile::tempdir().unwrap(),
            endpoint_dir: short_private_dir(),
        }
    }

    fn socket(&self) -> PathBuf {
        self.endpoint_dir.path().join("s")
    }

    /// Start the child with a cleared environment: only PATH, a private
    /// HOME/TMPDIR and the two case variables.
    fn spawn_start(&self) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "endpoint_safety_child", "--nocapture"])
            .args(["--test-threads", "1"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("TMPDIR", self.home.path())
            .env(CHILD_ROOT, self.project.path())
            .env(CHILD_SOCKET, self.socket())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().unwrap()
    }
}

fn inode(path: &Path) -> (u64, u64) {
    let meta = std::fs::symlink_metadata(path).unwrap();
    (meta.dev(), meta.ino())
}

/// Wait for a start that must refuse. Fails at once if the endpoint's inode
/// changes underneath (the original was replaced) or the budget runs out,
/// killing the child either way.
fn expect_refusal(mut child: Child, socket: &Path, original: (u64, u64)) -> (ExitStatus, String) {
    let deadline = Instant::now() + START_BUDGET;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
            return (status, stderr);
        }
        let replaced = std::fs::symlink_metadata(socket)
            .map(|meta| (meta.dev(), meta.ino()) != original)
            .unwrap_or(true);
        if replaced || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the start did not refuse: endpoint {} (original inode {:?})",
                if replaced {
                    "was replaced"
                } else {
                    "kept, but start never exited"
                },
                original
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// C2/F4: a live same-uid listener whose socket is mode 000 refuses the
/// probe with `EACCES`. That proves nothing about staleness: the start must
/// exit 2 naming the path, leave the inode in place, and the original
/// listener must still accept once its mode is restored.
#[test]
fn a_live_listener_the_probe_cannot_reach_is_left_alone() {
    if is_root() {
        // root bypasses the socket's mode bits, so connect succeeds and the
        // EACCES path cannot be reached; the live-listener case covers root.
        eprintln!("skipped: euid 0 ignores socket permissions (EACCES is unreachable)");
        return;
    }
    let case = Case::new();
    let socket = case.socket();
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o000)).unwrap();
    let original = inode(&socket);
    assert_eq!(
        UnixStream::connect(&socket).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "precondition: the probe sees EACCES on this platform"
    );

    let (status, stderr) = expect_refusal(case.spawn_start(), &socket, original);
    assert_eq!(
        status.code(),
        Some(2),
        "exit 2 (invalid arguments); stderr: {stderr}"
    );
    assert!(
        stderr.contains(&socket.display().to_string()),
        "names the path: {stderr}"
    );
    assert!(
        stderr.contains("left alone"),
        "says it was left alone: {stderr}"
    );
    assert_eq!(inode(&socket), original, "the endpoint was never unlinked");

    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let _client = UnixStream::connect(&socket).unwrap();
    listener.set_nonblocking(false).unwrap();
    listener
        .accept()
        .expect("the original listener still accepts");
}

/// A connectable same-uid listener is live and never replaced.
#[test]
fn a_live_reachable_listener_is_left_alone() {
    let case = Case::new();
    let socket = case.socket();
    let listener = UnixListener::bind(&socket).unwrap();
    let original = inode(&socket);

    let (status, stderr) = expect_refusal(case.spawn_start(), &socket, original);
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("live listener"), "{stderr}");
    assert_eq!(inode(&socket), original);
    let _client = UnixStream::connect(&socket).unwrap();
    listener
        .accept()
        .expect("the original listener still accepts");
}

/// A same-uid socket nobody listens on (`ECONNREFUSED`) is stale: the start
/// replaces it and serves on the path.
#[test]
fn a_stale_socket_is_replaced() {
    let case = Case::new();
    let socket = case.socket();
    drop(UnixListener::bind(&socket).unwrap());
    // Keep the stale inode allocated so the replacement provably differs,
    // even on file systems that reuse a freed inode at once (Linux ext4).
    let keep = case.endpoint_dir.path().join("keep");
    std::fs::hard_link(&socket, &keep).unwrap();
    let stale = inode(&socket);
    assert_eq!(
        UnixStream::connect(&socket).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused,
        "precondition: nobody listens"
    );

    let mut child = case.spawn_start();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    let ready = loop {
        line.clear();
        if stdout.read_line(&mut line).unwrap() == 0 {
            break false;
        }
        // libtest prints `test endpoint_safety_child ... ` before the
        // child's own first line, on the same line.
        if line.contains("listening ") {
            break true;
        }
    };
    let replaced = std::fs::symlink_metadata(&socket).map(|meta| (meta.dev(), meta.ino()));
    let connected = UnixStream::connect(&socket).is_ok();
    let _ = child.kill();
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
    assert!(
        ready,
        "the start never became ready ({status}); stderr: {stderr}"
    );
    assert_ne!(
        replaced.unwrap(),
        stale,
        "a fresh socket replaced the stale one"
    );
    assert!(connected, "the new daemon accepts on the path");
}
