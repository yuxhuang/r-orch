//! `orch daemon *` verbs need no opt-in env. Every child has explicit isolated routing;
//! none of the client error fixtures starts a daemon or inherits an agent CLI.
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output};

fn orch(root: &Path, socket: &Path, test_root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orch"));
    cmd.env_clear()
        .current_dir(root)
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .env("NO_COLOR", "1")
        .env("TERM", "xterm-256color")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("global.yml"))
        .env("ORCH_PROJECT_ROOT", root)
        .env("ORCH_SOCKET", socket)
        .env("ORCH_TEST_SOCKET_ROOT", test_root);
    cmd
}
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir.path().join(".orchestry"))
        .unwrap();
    dir
}
fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dc-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn daemon_verbs_need_no_opt_in_env_and_never_mention_the_removed_gate() {
    let root = project();
    let sock = short_dir();
    for verb in [
        vec!["status"],
        vec!["stop"],
        vec!["dispatch", "on"],
        vec!["dispatch", "off"],
    ] {
        let out = orch(root.path(), &sock.path().join("s"), sock.path())
            .arg("daemon")
            .args(&verb)
            .output()
            .unwrap();
        let err = stderr(&out);
        // The verb runs and reaches the client path: a client verb with no
        // daemon reports the uniform no-daemon failure, never a gate refusal.
        assert!(err.contains("No orch daemon is running"), "{verb:?}: {err}");
        assert!(
            !err.to_lowercase().contains("developmental"),
            "{verb:?}: {err}"
        );
        assert!(
            !err.contains("ORCH_DAEMON_DEVELOPMENTAL"),
            "{verb:?}: {err}"
        );
        assert!(!root.path().join(".orchestry/daemon.json").exists());
    }
}

#[test]
fn daemon_help_never_mentions_the_removed_gate() {
    let root = tempfile::tempdir().unwrap();
    let sock = short_dir();
    for args in [
        vec!["--help"],
        vec!["daemon", "--help"],
        vec!["daemon", "start", "--help"],
    ] {
        let out = orch(root.path(), &sock.path().join("s"), sock.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
        assert!(
            !text.contains("ORCH_DAEMON_DEVELOPMENTAL"),
            "{args:?}: {text}"
        );
        assert!(
            !text.to_lowercase().contains("developmental"),
            "{args:?}: {text}"
        );
    }
}

#[test]
fn explicit_start_refuses_agent_context_before_project_or_socket_work() {
    let root = tempfile::tempdir().unwrap();
    let sock = short_dir();
    for identity in ["ORCH_TASK_ID", "ORCH_AGENT_ID"] {
        let out = orch(root.path(), &sock.path().join("s"), sock.path())
            .env(identity, "test-id")
            .args(["daemon", "start"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!("✕ {}", orch_proto::error::START_REFUSED_IN_AGENT_CONTEXT)
        );
        assert!(!root.path().join(".orchestry").exists());
    }
}

#[test]
fn absent_stale_file_and_refused_socket_share_nodaemon_exit_and_text() {
    let root = project();
    let short = short_dir();
    let socket = short.path().join("s");
    for state in ["absent", "regular", "refused"] {
        if state == "regular" {
            fs::write(&socket, "stale file").unwrap();
        }
        if state == "refused" {
            fs::remove_file(&socket).unwrap();
            let listener = UnixListener::bind(&socket).unwrap();
            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
            drop(listener);
        }
        for verb in ["status", "stop"] {
            let out = orch(root.path(), &socket, short.path())
                .args(["daemon", verb])
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(7),
                "{state}/{verb}: {}",
                stderr(&out)
            );
            assert!(stderr(&out).contains(&format!(
                "No orch daemon is running for {} (",
                root.path().canonicalize().unwrap().display()
            )));
            assert!(stderr(&out).ends_with("    Start it with: orch daemon start\n"));
        }
        if state == "regular" {
            assert_eq!(fs::read_to_string(&socket).unwrap(), "stale file");
        }
        if state == "absent" {
            assert!(!socket.exists());
        }
    }
}

#[test]
fn agent_nodaemon_text_instructs_report_and_stop() {
    let root = project();
    let short = short_dir();
    let out = orch(root.path(), &short.path().join("s"), short.path())
        .env("ORCH_TASK_ID", "test-task")
        .args(["daemon", "status"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert!(stderr(&out).contains("Do not start it; report this to the owner and stop."));
    assert!(!stderr(&out).contains("Start it with"));
}

#[test]
fn daemon_help_is_offline_and_includes_existing_peer_flags() {
    let root = tempfile::tempdir().unwrap();
    let short = short_dir();
    let out = orch(root.path(), &short.path().join("s"), short.path())
        .args(["daemon", "start", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--detach",
        "--dispatch",
        "--from-agent",
        "--listen",
        "--peer-name",
    ] {
        assert!(stdout.contains(flag), "{stdout}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_dispatch_and_stop_use_the_explicit_harness_socket() {
    let daemon = orch_daemon::testing::DaemonHarness::builder()
        .on_socket()
        .start()
        .await
        .unwrap();
    let root = daemon.project_root();
    let socket = daemon.socket().unwrap();
    let parent = socket.parent().unwrap();
    let out = orch(root, socket, parent)
        .args(["--json", "daemon", "status"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let status: orch_proto::ops::results::DaemonStatus =
        serde_json::from_slice(&out.stdout).unwrap();
    assert!(!status.dispatch.dispatch_on);
    let out = orch(root, socket, parent)
        .args(["daemon", "status"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(human.contains("dispatch OFF"));
    assert!(human.contains(&format!(
        "version: orch {} protocol {}",
        status.binary, status.protocol
    )));
    assert!(human.contains(&status.executable.display_escaped()));
    assert!(human.contains(&status.started_at));

    // Keep automatic dispatch OFF: this binary fixture never admits an
    // agent launch. Both switches are covered with a private fake transport
    // in orch-client/tests/transport.rs.
    let out = orch(root, socket, parent)
        .args(["--json", "daemon", "dispatch", "off"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let state: orch_proto::ops::results::DispatchState =
        serde_json::from_slice(&out.stdout).unwrap();
    assert!(!state.dispatch_on);

    let out = orch(root, socket, parent)
        .args(["daemon", "stop"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("stopped cleanly"));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_skew_names_both_versions_and_frozen_stop_still_works() {
    let daemon = orch_daemon::testing::DaemonHarness::builder()
        .on_socket()
        .versions(orch_proto::LOCAL_PROTOCOL + 1, orch_proto::BINARY_VERSION)
        .start()
        .await
        .unwrap();
    let root = daemon.project_root();
    let socket = daemon.socket().unwrap();
    let parent = socket.parent().unwrap();
    let out = orch(root, socket, parent)
        .args(["daemon", "status"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    let expected = orch_proto::error::ConnectFailure::VersionSkew {
        project_root: root.canonicalize().unwrap().to_string_lossy().into(),
        skew: orch_proto::handshake::VersionSkew {
            client_protocol: orch_proto::LOCAL_PROTOCOL,
            client_binary: orch_proto::BINARY_VERSION.into(),
            daemon_protocol: orch_proto::LOCAL_PROTOCOL + 1,
            daemon_binary: orch_proto::BINARY_VERSION.into(),
        },
    };
    assert_eq!(stderr(&out), format!("{}\n", expected.render(false)));
    let out = orch(root, socket, parent)
        .args(["daemon", "stop"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    daemon.shutdown().await.unwrap();
}

#[test]
fn init_and_rerun_print_start_hint_without_creating_runtime_files() {
    let root = tempfile::tempdir().unwrap();
    let short = short_dir();
    let socket = short.path().join("s");
    for _ in 0..2 {
        let out = orch(root.path(), &socket, short.path())
            .args(["init", "--adapter", "shell"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(String::from_utf8_lossy(&out.stdout).contains("Next: orch daemon start"));
        assert!(!socket.exists());
        assert!(!root.path().join(".orchestry/daemon.json").exists());
        assert!(!root.path().join(".orchestry/orchestry.lock").exists());
    }
}

// Own only this child: a failed readiness/status assertion must still reap it.
struct ForegroundDaemon(std::process::Child);
impl Drop for ForegroundDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn explicit_foreground_start_prints_no_banner_and_sanitizes_the_environment() {
    use std::io::BufRead;
    use std::time::Duration;

    let root = project();
    let short = short_dir();
    let socket = short.path().join("s");
    let errors = short.path().join("stderr");
    let mut daemon = ForegroundDaemon(
        orch(root.path(), &socket, short.path())
            .env("DEBUG", "private-debug-value")
            .args(["daemon", "start", "--peer-name", "test-owner"])
            .stdout(std::process::Stdio::piped())
            .stderr(fs::File::create(&errors).unwrap())
            .spawn()
            .unwrap(),
    );
    let stdout = daemon.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.starts_with("listening ") {
                let _ = ready_tx.send(line);
            }
        }
    });
    assert_eq!(
        ready_rx
            .recv_timeout(Duration::from_secs(15))
            .unwrap_or_else(|error| {
                panic!("{error}: {}", fs::read_to_string(&errors).unwrap())
            }),
        format!("listening {}", socket.display())
    );
    let out = orch(root.path(), &socket, short.path())
        .args(["--json", "daemon", "status"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let status: orch_proto::ops::results::DaemonStatus =
        serde_json::from_slice(&out.stdout).unwrap();
    assert!(!status.dispatch.dispatch_on);
    assert!(status.removed_env.iter().any(|name| name == "DEBUG"));
    let out = orch(root.path(), &socket, short.path())
        .args(["daemon", "stop"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(daemon.0.wait().unwrap().success());
    reader.join().unwrap();
    let errors = fs::read_to_string(errors).unwrap();
    assert!(!errors.to_lowercase().contains("developmental"), "{errors}");
    assert!(!errors.contains("private-debug-value"));
    assert!(!socket.exists());
    assert!(!root.path().join(".orchestry/daemon.json").exists());
}
