//! `orch task cancel` of a running task must work while another process owns
//! the project.
//!
//! `serve`, `run --watch` and the TUI hold `.orchestry/orchestry.lock`, so the
//! cancelling process cannot take it to kill the agent itself. It cancels the
//! task in the store instead, and the owner's next reconcile kills the child
//! and finishes the run. Before that, the command failed with "Orchestrator
//! already running" and the run went on.
//!
//! Binary-level, because the lock conflict needs two real processes.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn orch_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("orch")
}

fn orch(dir: &Path, args: &[&str]) -> Output {
    Command::new(orch_binary())
        .args(args)
        .current_dir(dir)
        .env("HOME", dir.join("home"))
        .env("NO_COLOR", "1")
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

/// Stops `serve` even when an assertion fails, so no daemon outlives the test.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status();
        let stopped = wait_for(Duration::from_secs(10), || self.0.try_wait().ok().flatten());
        if stopped.is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn cancel_of_a_running_task_while_serve_owns_the_project() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("home")).expect("home");
    stdout_of(&orch(
        dir,
        &["init", "--name", "proj", "--adapter", "shell"],
    ));

    // The owner kills the child on its next tick; a short poll keeps the test
    // from waiting out the 10 s default.
    let config_path = dir.join(".orchestry/config.yml");
    let config = std::fs::read_to_string(&config_path).expect("config.yml");
    assert!(config.contains("poll_interval_ms: 10000"), "{config}");
    std::fs::write(
        &config_path,
        config.replace("poll_interval_ms: 10000", "poll_interval_ms: 300"),
    )
    .expect("write config.yml");

    let agent: Value = serde_json::from_str(&stdout_of(&orch(
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
    let agent_id = agent["id"].as_str().expect("agent id");
    let task_id = stdout_of(&orch(
        dir,
        &["task", "add", "long job", "--assignee", agent_id, "--quiet"],
    ));

    let _daemon = Daemon(
        Command::new(orch_binary())
            .args(["serve", "--log-format", "json"])
            .current_dir(dir)
            .env("HOME", dir.join("home"))
            .env("NO_COLOR", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve"),
    );

    let state_path = dir.join(".orchestry/state.json");
    let (run_id, pid) = wait_for(Duration::from_secs(20), || {
        let entry = read_json(&state_path)?["running"][&task_id].clone();
        Some((entry["run_id"].as_str()?.to_owned(), entry["pid"].as_u64()?))
    })
    .expect("serve never started the run");
    assert!(is_alive(pid), "the agent child is not running");

    let cancel = orch(dir, &["task", "cancel", &task_id]);
    assert!(
        cancel.status.success(),
        "cancel refused while serve owns the project: {}{}",
        String::from_utf8_lossy(&cancel.stdout),
        String::from_utf8_lossy(&cancel.stderr)
    );
    assert!(String::from_utf8_lossy(&cancel.stdout).contains(&format!("Cancelled {task_id}")));
    let task: Value = serde_json::from_str(&stdout_of(&orch(
        dir,
        &["task", "show", &task_id, "--json"],
    )))
    .expect("task json");
    assert_eq!(task["status"], "cancelled");

    // The owner's reconcile does the rest.
    let run_path = dir.join(format!(".orchestry/runs/{run_id}.json"));
    let settled = wait_for(Duration::from_secs(10), || {
        let run = read_json(&run_path)?;
        let state = read_json(&state_path)?;
        (run["status"] == "cancelled" && state["running"].get(&task_id).is_none() && !is_alive(pid))
            .then_some(())
    });
    assert!(
        settled.is_some(),
        "the owner did not stop the run: run={:?} state={:?} child alive={}",
        read_json(&run_path),
        read_json(&state_path),
        is_alive(pid)
    );
}
