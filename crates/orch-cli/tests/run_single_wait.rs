//! `run <id>` must terminate once the target run completes.
//!
//! Since S2a-cli `run <id>` is a daemon client: the daemon dispatches the task
//! and streams its events until the run is terminal. The client must follow
//! that stream to its end and report the run's outcome, not hang after the
//! completion nor return before it.

#[path = "common/daemon.rs"]
mod daemon;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use daemon::TestDaemon;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::Builder::new()
            .prefix("orch-run-wait-")
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(dir.path().join("home")).expect("home");
        std::fs::create_dir_all(dir.path().join("p")).expect("project");
        let fx = Fixture { dir };
        let init = fx.orch(None, &["init", "--name", "run-wait", "--adapter", "shell"]);
        assert!(init.status.success(), "init: {}", stderr(&init));
        fx
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn command(&self, daemon: Option<&TestDaemon>, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
        command
            .env_clear()
            .current_dir(self.root())
            .env("HOME", self.home())
            .env("PATH", "/usr/bin:/bin")
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.home().join("global.yml"),
            )
            .args(args)
            .stdin(Stdio::null());
        if let Some(daemon) = daemon {
            daemon.apply(&mut command);
        }
        command
    }

    fn orch(&self, daemon: Option<&TestDaemon>, args: &[&str]) -> Output {
        self.command(daemon, args).output().expect("run orch")
    }

    fn json(&self, daemon: &TestDaemon, args: &[&str]) -> serde_json::Value {
        let mut argv = vec!["--json"];
        argv.extend(args);
        let out = self.orch(Some(daemon), &argv);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        serde_json::from_slice(&out.stdout).expect("json output")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run `command`, failing the test (and killing the child) if it is still
/// running after `limit`: a hang must not stall the suite.
fn bounded(mut command: Command, limit: Duration) -> Output {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orch");
    let pid = child.id();
    let deadline = Instant::now() + limit;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    loop {
        if let Ok(output) = rx.try_recv() {
            return output.expect("wait orch");
        }
        if Instant::now() >= deadline {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            panic!("`orch run <id>` did not return within {limit:?}: it hung after the run");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn doomed_task(fx: &Fixture, daemon: &TestDaemon) -> String {
    let agent = fx.json(
        daemon,
        &[
            "agent",
            "add",
            "doomed",
            "--adapter",
            "shell",
            "--command",
            "exit 7",
        ],
    );
    let agent_id = agent["id"].as_str().expect("agent id").to_owned();
    let task = fx.json(
        daemon,
        &["task", "add", "doomed work", "--assignee", &agent_id],
    );
    task["id"].as_str().expect("task id").to_owned()
}

fn task_status(fx: &Fixture, daemon: &TestDaemon, id: &str) -> String {
    fx.json(daemon, &["task", "show", id])["status"]
        .as_str()
        .expect("status")
        .to_owned()
}

/// `exit 7` is a failed run, which is exit 1 — so the stream really was
/// followed to its end rather than the command returning early.
#[test]
fn run_id_follows_the_daemon_stream_to_the_failed_run_and_exits_1() {
    let fx = Fixture::new();
    let daemon = TestDaemon::start(&fx.root(), &fx.home());
    let task_id = doomed_task(&fx, &daemon);

    let out = bounded(
        fx.command(Some(&daemon), &["run", &task_id]),
        Duration::from_secs(60),
    );
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_ne!(task_status(&fx, &daemon, &task_id), "todo");
    assert_ne!(task_status(&fx, &daemon, &task_id), "in_progress");
    daemon.shutdown();
}

/// Without a daemon `run <id>` is refused with the uniform no-daemon exit
/// code and never starts one (R4).
#[test]
fn run_id_without_a_daemon_is_refused_with_exit_7() {
    let fx = Fixture::new();
    let socket_dir = tempfile::Builder::new()
        .prefix("otd-")
        .tempdir_in("/tmp")
        .expect("socket dir");
    let socket: &Path = &socket_dir.path().join("s");
    let mut command = fx.command(None, &["run", "tsk_missing"]);
    daemon::apply_env(&mut command, socket, socket_dir.path());
    let out = bounded(command, Duration::from_secs(20));
    assert_eq!(out.status.code(), Some(7), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("orch daemon start"),
        "{}",
        stderr(&out)
    );
    assert!(
        !socket.exists(),
        "a client must never bind or start a daemon"
    );
}
