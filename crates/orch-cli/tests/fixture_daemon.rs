//! The shared test-daemon helper (`tests/common/daemon.rs`) keeps its promises:
//! isolated short socket, readiness, clean stop, and a teardown that catches a
//! leaked process. If these fail, every fixture built on the helper is suspect.

#[path = "common/daemon.rs"]
mod daemon;

use std::os::unix::process::CommandExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use daemon::{TestDaemon, process_table};

/// An initialised project plus private HOME, both under one tempdir.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::Builder::new()
            .prefix("orch-fixture-daemon-")
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(dir.path().join("home")).expect("home");
        std::fs::create_dir_all(dir.path().join("p")).expect("project");
        let output = Command::new(env!("CARGO_BIN_EXE_orch"))
            .env_clear()
            .current_dir(dir.path().join("p"))
            .env("HOME", dir.path().join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                dir.path().join("home/global.yml"),
            )
            .args(["init", "--name", "fx", "--adapter", "shell"])
            .output()
            .expect("run orch init");
        assert!(
            output.status.success(),
            "init: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Fixture { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn start(&self) -> TestDaemon {
        TestDaemon::start(&self.root(), &self.home())
    }
}

fn alive(pid: u32) -> bool {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        None,
    )
    .is_ok()
}

#[test]
fn the_socket_is_a_short_private_path_under_tmp() {
    let fx = Fixture::new();
    let daemon = fx.start();
    let socket = daemon.socket().to_string_lossy().into_owned();
    assert!(socket.len() <= 100, "{socket} is {} bytes", socket.len());
    assert!(socket.starts_with("/tmp/otd-"), "{socket}");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(daemon.socket_dir())
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o700, "socket dir must be private");
}

#[test]
fn a_started_daemon_answers_status_on_its_own_socket() {
    let fx = Fixture::new();
    let daemon = fx.start();
    let out = daemon.control(&["daemon", "status", "--json"]);
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status json");
    assert_eq!(value["pid"].as_u64(), Some(u64::from(daemon.pid())));
}

#[test]
fn a_client_with_the_applied_env_reaches_the_daemon_and_one_without_it_does_not() {
    let fx = Fixture::new();
    let daemon = fx.start();
    let mut with = Command::new(env!("CARGO_BIN_EXE_orch"));
    with.current_dir(fx.root())
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .args(["daemon", "status"]);
    daemon.apply(&mut with);
    assert!(with.output().unwrap().status.success());

    let mut without = Command::new(env!("CARGO_BIN_EXE_orch"));
    without
        .current_dir(fx.root())
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env(daemon::GATE_ENV, "1")
        .env("ORCH_SOCKET", "/tmp/otd-nonexistent/s")
        .args(["daemon", "status"]);
    assert!(
        !without.output().unwrap().status.success(),
        "a wrong socket must not reach the test daemon"
    );
}

#[test]
fn shutdown_stops_the_daemon_and_leaves_no_process_behind() {
    let fx = Fixture::new();
    let daemon = fx.start();
    let pid = daemon.pid();
    let marker = daemon.socket_dir().to_string_lossy().into_owned();
    daemon.shutdown();
    assert!(!alive(pid), "daemon pid {pid} still alive");
    assert!(
        !process_table().iter().any(|p| p.command.contains(&marker)),
        "a process still names {marker}"
    );
}

#[test]
fn dropping_without_shutdown_still_stops_the_daemon() {
    let fx = Fixture::new();
    let pid = {
        let daemon = fx.start();
        daemon.pid()
    };
    assert!(!alive(pid), "daemon pid {pid} outlived its guard");
}

#[test]
fn teardown_of_an_already_crashed_daemon_is_clean() {
    let fx = Fixture::new();
    let daemon = fx.start();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(daemon.pid()).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    daemon.shutdown();
}

#[test]
fn teardown_fails_loudly_when_a_process_naming_the_socket_dir_survives() {
    let fx = Fixture::new();
    let daemon = fx.start();
    let marker = daemon.socket_dir().to_string_lossy().into_owned();
    // A leak: its own process group, so only the command-line check can see it.
    let mut leak = Command::new("/bin/sh")
        .args(["-c", &format!("sleep 300; : {marker}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn leak");
    let leaked = catch_unwind(AssertUnwindSafe(|| daemon.shutdown()));
    // The teardown killed it, but only after reporting it.
    let reaped = leak.wait().expect("reap leak");
    assert!(leaked.is_err(), "a survivor must fail the teardown");
    assert!(!reaped.success(), "survivor was killed: {reaped}");
}
