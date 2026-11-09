//! `orch task cancel` through the daemon stops a live agent process.
//!
//! Before S2a-cli, `serve`, `run --watch` and the TUI held
//! `.orchestry/orchestry.lock` as a second, separate process from the one
//! running `orch task cancel`; the cancelling process could not take the
//! lock to kill the agent itself, so it cancelled the task in the store
//! instead and waited for the lock-holding owner's next reconcile tick to
//! notice and kill the child.
//!
//! Since S2a-cli every `orch` command — including `run <id>` — is a client
//! of the project's one daemon, and the daemon's own orchestrator is the
//! only thing that ever launches an agent. `task_service::cancel` reaching a
//! live run (`orch_engine::orchestrator::cancel_task_locked`) now kills the
//! agent's child process directly (`kill_with_grace`) as part of handling
//! the RPC. There is no second owning process any more, and so no
//! store-only fallback to fall back to: `task.cancel` and `dispatch_run_task`
//! run on separate coordinator lanes (`Lane::Control` vs `Lane::Ordinary`,
//! `crates/orch-daemon/src/ops/mod.rs`) precisely so a cancel can always get
//! through while a run is still being observed.
//!
//! Binary-level, because the live agent process and the daemon that owns it
//! need to be real: a test daemon (`tests/common/daemon.rs`) launches the
//! real task via a client `orch run <id>` call left running in the
//! background, then a second client issues `orch task cancel` against the
//! same daemon, and the test asserts the agent's child process dies promptly
//! and the run settles to `cancelled` — no reconcile tick, no second
//! process, no `poll_interval_ms` tuning required.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

#[path = "common/daemon.rs"]
mod daemon;
use daemon::TestDaemon;

fn orch_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("orch")
}

fn orch_command(daemon: &TestDaemon, dir: &Path) -> Command {
    let mut command = Command::new(orch_binary());
    command
        .current_dir(dir)
        .env("HOME", dir.join("home"))
        .env("NO_COLOR", "1");
    daemon.apply(&mut command);
    command
}

fn orch(daemon: &TestDaemon, dir: &Path, args: &[&str]) -> Output {
    orch_command(daemon, dir)
        .args(args)
        .output()
        .expect("orch runs")
}

fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "orch failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn wait_for<T>(within: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    while start.elapsed() < within {
        if let Some(value) = probe() {
            return Some(value);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn is_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Reaps the background `orch run <id>` observer even when an assertion
/// fails, so no client outlives the test.
struct Observer(Child);

impl Drop for Observer {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
fn cancel_through_the_daemon_kills_the_live_agent_process() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("home")).expect("home");

    // `init` is one of the two commands that need no daemon.
    let init = Command::new(orch_binary())
        .args(["init", "--name", "proj", "--adapter", "shell"])
        .current_dir(dir)
        .env("HOME", dir.join("home"))
        .env("NO_COLOR", "1")
        .output()
        .expect("orch init runs");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let test_daemon = TestDaemon::start(dir, &dir.join("home"));

    let agent: Value = serde_json::from_str(&stdout_of(&orch(
        &test_daemon,
        dir,
        &[
            "agent",
            "add",
            "sleeper",
            "--adapter",
            "shell",
            "--command",
            "sleep 30",
            "--json",
        ],
    )))
    .expect("agent json");
    let agent_id = agent["id"].as_str().expect("agent id").to_owned();
    let task_id = stdout_of(&orch(
        &test_daemon,
        dir,
        &[
            "task",
            "add",
            "long job",
            "--assignee",
            &agent_id,
            "--quiet",
        ],
    ));

    // `run <id>` is a daemon RPC that launches and observes the run until it
    // settles. Left running in the background so the test can cancel while
    // the agent is still alive.
    let mut run_command = orch_command(&test_daemon, dir);
    run_command
        .args(["run", &task_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _observer = Observer(run_command.spawn().expect("spawn orch run"));

    let state_path = dir.join(".orchestry/state.json");
    let (run_id, pid) = wait_for(Duration::from_secs(20), || {
        let entry = read_json(&state_path)?["running"][&task_id].clone();
        Some((entry["run_id"].as_str()?.to_owned(), entry["pid"].as_u64()?))
    })
    .expect("the daemon never started the run");
    assert!(is_alive(pid), "the agent child is not running");

    let cancel = orch(&test_daemon, dir, &["task", "cancel", &task_id]);
    assert!(
        cancel.status.success(),
        "cancel failed: {}{}",
        String::from_utf8_lossy(&cancel.stdout),
        String::from_utf8_lossy(&cancel.stderr)
    );
    assert!(String::from_utf8_lossy(&cancel.stdout).contains(&format!("Cancelled {task_id}")));

    let task: Value = serde_json::from_str(&stdout_of(&orch(
        &test_daemon,
        dir,
        &["task", "show", &task_id, "--json"],
    )))
    .expect("task json");
    assert_eq!(task["status"], "cancelled");

    // `cancel_task_locked` kills the agent itself as part of handling the
    // RPC: no second process, no reconcile tick to wait on.
    let run_path = dir.join(format!(".orchestry/runs/{run_id}.json"));
    let settled = wait_for(Duration::from_secs(10), || {
        let run = read_json(&run_path)?;
        let state = read_json(&state_path)?;
        (run["status"] == "cancelled" && state["running"].get(&task_id).is_none() && !is_alive(pid))
            .then_some(())
    });
    assert!(
        settled.is_some(),
        "the daemon did not stop the run: run={:?} state={:?} child alive={}",
        read_json(&run_path),
        read_json(&state_path),
        is_alive(pid)
    );
}
