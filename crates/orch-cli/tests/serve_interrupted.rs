//! `serve --once` interrupted by a signal reports `interrupted`, not success.
//!
//! PARITY.md §14.38, a deliberate divergence decided by the user. The oracle
//! computes its verdict from the task list without asking why it stopped
//! waiting, so a run killed mid-flight reports `result="all_done"` and exits 0.
//! Measured on the oracle with a planted unreadable task file: it ticks
//! forever, logging `orchestrator:error` on each dispatch, and on SIGTERM
//! emits `{"event":"serve:finished","result":"all_done","exit_code":0}`.
//!
//! This goes through the real binary rather than `run_once` directly, because
//! the three things that matter here are process-level: the exit code the shell
//! sees, the record on stdout, and whether `orchestry.lock` is released. A unit
//! test on `OnceResult` cannot speak to any of them.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;

fn orch(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(dir)
        .env("NO_COLOR", "1")
        // No agent CLI on PATH: the gate forbids a test exec'ing a real one,
        // and nothing here needs an agent to run.
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG");
    command
}

/// A project with one task whose file cannot be read.
///
/// The index still lists the task as `todo`, so `serve --once` waits for a
/// status that will never arrive — which is the state a signal has to
/// interrupt. Corrupting the file rather than inventing a slow agent keeps the
/// test free of timing assumptions about an adapter.
fn project_with_an_unreadable_task() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "claude"])
        .output()
        .expect("init");
    let added = orch(dir.path())
        .args(["task", "add", "a task", "--quiet"])
        .output()
        .expect("task add");
    let id = String::from_utf8_lossy(&added.stdout).trim().to_string();
    assert!(id.starts_with("tsk_"), "expected a task id, got {id:?}");
    std::fs::write(
        dir.path().join(format!(".orchestry/tasks/{id}.yml")),
        "{{{ not yaml\n",
    )
    .expect("plant the unreadable task");
    dir
}

#[test]
fn a_signal_with_work_outstanding_reports_interrupted_and_exits_one() {
    let dir = project_with_an_unreadable_task();

    let child = orch(dir.path())
        .args(["serve", "--once", "--log-format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve --once");
    let pid = child.id().to_string();

    // Long enough for the orchestrator to come up and tick at least once.
    // `--once` polls on a hard-coded 2 s interval, so this lands inside the
    // first wait rather than racing startup.
    std::thread::sleep(Duration::from_millis(1_200));
    Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("send SIGTERM");

    let started = Instant::now();
    let output = child.wait_with_output().expect("wait for serve");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        elapsed < Duration::from_secs(30),
        "serve did not wind down promptly after SIGTERM: {elapsed:?}"
    );
    assert!(
        stdout.contains(r#""result":"interrupted""#),
        "serve:finished must report `interrupted`, not a success it did not \
         achieve (§14.38). stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "every non-success --once outcome exits 1 (§3)"
    );
    // The lock is the reason the wind-down runs on this path at all: an
    // interrupted run must not wedge the project (§14.15).
    assert!(
        !dir.path().join(".orchestry/orchestry.lock").exists(),
        "orchestry.lock was left behind by an interrupted run"
    );
}

/// The other half of §14.38, and the part that must *not* change: once every
/// task is terminal, a stop is an ordinary completion and the output stays
/// byte-identical to the oracle's.
#[test]
fn a_finished_project_still_reports_all_done_and_exits_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "claude"])
        .output()
        .expect("init");

    // No tasks at all: `--once` takes its "empty project exits immediately"
    // path, which is the cheapest way to reach a genuine completion without an
    // agent. The interrupted path is unreachable here by construction.
    let output = orch(dir.path())
        .args(["serve", "--once", "--log-format", "json"])
        .output()
        .expect("serve --once");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains(r#""result":"all_done""#),
        "a completed run must still say all_done. stdout:\n{stdout}"
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(!dir.path().join(".orchestry/orchestry.lock").exists());
}
