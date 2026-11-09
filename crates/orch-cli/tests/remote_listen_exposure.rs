//! The peer listener has no authentication, so binding it beyond loopback
//! must be an explicit choice and must say so (security review F1).
//! Exercised through the real binary: the warning is user-facing output.
//!
//! Since S2a-cli the listener belongs to the daemon: `remote listen` edits the
//! daemon's configuration (so those cases run against a test daemon), and the
//! engine-start warning is printed by the foreground start (`orch serve`,
//! `orch run --watch`, `orch daemon start`), which these tests run on an
//! isolated socket and terminate once the line they wait for has appeared.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;

#[path = "common/daemon.rs"]
mod daemon;
use daemon::TestDaemon;

const EXPOSURE: &str = "no authentication";

fn orch(project: &Path, home: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(project)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", home.join("global.yml"))
        .env("NO_COLOR", "1")
        // No agent CLIs on PATH: nothing here may spawn a real one.
        .env("PATH", "/usr/bin:/bin")
        .env_remove("ORCHESTRY_DEBUG");
    command
}

struct Project {
    // Dropped first: the daemon (if any) stops before its directories go.
    daemon: Option<TestDaemon>,
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    /// `orch init` only: no daemon, for the cases that start one themselves.
    fn new() -> Self {
        let project = Self {
            daemon: None,
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        project
            .run(&["init", "--name", "p", "--adapter", "shell"])
            .assert()
            .success();
        project
    }

    /// An initialised project with its explicit test daemon (dispatch off).
    fn with_daemon() -> Self {
        let mut project = Self::new();
        project.daemon = Some(TestDaemon::start(project.dir.path(), project.home.path()));
        project
    }

    fn run(&self, args: &[&str]) -> Command {
        let mut command = orch(self.dir.path(), self.home.path());
        command.args(args);
        if let Some(daemon) = &self.daemon {
            daemon.apply(&mut command);
        }
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.run(args).output().unwrap()
    }

    fn configured_listen(&self) -> String {
        let output = self.output(&["--json", "config", "get", "peer.listen"]);
        assert!(output.status.success(), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["value"]
            .as_str()
            .expect("peer.listen is set")
            .to_owned()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn remote_listen_on_without_an_address_binds_loopback_only_and_stays_quiet() {
    let project = Project::with_daemon();
    let output = project.output(&["remote", "listen", "on"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(project.configured_listen(), "127.0.0.1:7690");
    let all = text(&output.stdout) + &text(&output.stderr);
    assert!(!all.contains(EXPOSURE), "loopback warned: {all}");
    assert!(all.contains("127.0.0.1:7690"), "{all}");
}

#[test]
fn remote_listen_on_a_wildcard_address_persists_it_and_warns_on_stderr() {
    for addr in ["0.0.0.0:7690", "[::]:7690"] {
        let project = Project::with_daemon();
        let output = project.output(&["remote", "listen", "on", addr]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(project.configured_listen(), addr);
        let stderr = text(&output.stderr);
        for needle in [addr, "ALL network interfaces", EXPOSURE, "127.0.0.1"] {
            assert!(stderr.contains(needle), "{needle} missing: {stderr}");
        }
    }
}

#[test]
fn remote_listen_on_a_specific_lan_address_warns_on_stderr() {
    let project = Project::with_daemon();
    let output = project.output(&["remote", "listen", "on", "192.168.1.5:7690"]);
    assert!(output.status.success(), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("192.168.1.5:7690") && stderr.contains(EXPOSURE),
        "{stderr}"
    );
}

/// The first foreground `orch <args>` run: an explicit daemon start on an
/// isolated socket, in its own process group. It is terminated (SIGTERM, then
/// SIGKILL of the group) as soon as `done(stdout, stderr)` holds, the child
/// exits, or the budget is spent, and its output so far is returned.
fn foreground_start(
    project: &Project,
    args: &[&str],
    done: impl Fn(&str, &str) -> bool,
) -> (String, String) {
    const BUDGET: Duration = Duration::from_secs(30);
    // The shared helper's 0700 directory: the daemon refuses a group- or
    // world-accessible socket directory.
    let (short, socket) = daemon::short_socket();
    let mut command = orch(project.dir.path(), project.home.path());
    command
        .args(args)
        .env("ORCH_SOCKET", &socket)
        .env("ORCH_TEST_SOCKET_ROOT", short.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().expect("spawn the foreground start");
    let pgid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).expect("pid"));
    let collect = |mut pipe: Box<dyn Read + Send>| {
        let seen = Arc::new(Mutex::new(String::new()));
        let writer = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                writer
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        seen
    };
    let stdout = collect(Box::new(child.stdout.take().expect("stdout")));
    let stderr = collect(Box::new(child.stderr.take().expect("stderr")));
    let deadline = Instant::now() + BUDGET;
    loop {
        let (out, err) = (
            stdout.lock().unwrap().clone(),
            stderr.lock().unwrap().clone(),
        );
        if done(&out, &err)
            || child.try_wait().expect("poll the child").is_some()
            || Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGTERM);
    let stop = Instant::now() + Duration::from_secs(5);
    while child.try_wait().expect("poll the child").is_none() && Instant::now() < stop {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
    let _ = child.wait();
    // Let the reader threads drain what the child wrote before it died.
    std::thread::sleep(Duration::from_millis(100));
    let (out, err) = (
        stdout.lock().unwrap().clone(),
        stderr.lock().unwrap().clone(),
    );
    (out, err)
}

fn exposure_seen(_stdout: &str, stderr: &str) -> bool {
    stderr.contains(EXPOSURE)
}

fn listening(stdout: &str, _stderr: &str) -> bool {
    stdout.lines().any(|line| line.starts_with("listening "))
}

#[test]
#[ignore = "production bug tsk_MdWfPP2: daemon start/serve prints no exposure warning before binding"]
fn a_daemon_started_with_a_network_listen_address_warns_before_binding() {
    // 192.0.2.0/24 is TEST-NET-1: never a local address, so the bind cannot
    // succeed and nothing is ever exposed by this test.
    for args in [
        vec!["serve", "--listen", "192.0.2.1:7690"],
        vec!["daemon", "start", "--listen", "192.0.2.1:7690"],
    ] {
        let project = Project::new();
        let (_stdout, stderr) = foreground_start(&project, &args, exposure_seen);
        assert!(
            stderr.contains("192.0.2.1:7690") && stderr.contains(EXPOSURE),
            "orch {}: no exposure warning: {stderr}",
            args.join(" ")
        );
    }
}

#[test]
#[ignore = "production bug tsk_MdWfPP2: daemon start/run --watch prints no exposure warning before binding"]
fn a_daemon_started_with_a_configured_network_peer_listen_warns_before_binding() {
    let project = Project::with_daemon();
    project
        .run(&["remote", "listen", "on", "192.0.2.1:7690"])
        .assert()
        .success();
    // Stop the daemon that applied the setting, then start (explicitly) from
    // the stored config alone.
    let mut project = project;
    project.daemon.take().expect("daemon").shutdown();
    let (_stdout, stderr) = foreground_start(&project, &["run", "--watch"], exposure_seen);
    assert!(
        stderr.contains("192.0.2.1:7690") && stderr.contains(EXPOSURE),
        "configured peer.listen did not warn: {stderr}"
    );
}

#[test]
fn a_daemon_with_loopback_listening_does_not_warn() {
    let project = Project::new();
    let (stdout, stderr) =
        foreground_start(&project, &["serve", "--listen", "127.0.0.1:0"], listening);
    assert!(
        listening(&stdout, &stderr),
        "never became ready: {stdout} {stderr}"
    );
    assert!(!stderr.contains(EXPOSURE), "{stderr}");
}
