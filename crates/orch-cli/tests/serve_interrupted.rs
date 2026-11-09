//! `serve --once` interrupted by a signal reports `interrupted`, not success.
//!
//! The verdict must not come from the task list alone: a run killed mid-flight
//! would then report `result="all_done"` and exit 0. With a planted unreadable
//! task file, `serve --once` ticks forever, logging `orchestrator:error` on
//! each dispatch, and on SIGTERM it must not emit
//! `{"event":"serve:finished","result":"all_done","exit_code":0}`.
//!
//! Since S2a-cli `serve --once` is a client of the daemon (it runs one
//! dispatch pass over the socket), so each scenario starts an explicit test
//! daemon on an isolated socket (`tests/common/daemon.rs`). The signal goes to
//! the client; the daemon, which owns `orchestry.lock`, keeps running.
//!
//! This goes through the real binary rather than `run_once` directly, because
//! the three things that matter here are process-level: the exit code the shell
//! sees, the record on stdout, and whether `orchestry.lock` is released. A unit
//! test on `OnceResult` cannot speak to any of them.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;

#[path = "common/daemon.rs"]
mod daemon;
use daemon::TestDaemon;

/// `orch daemon status` against `daemon`: whether it is still serving.
fn status_of(daemon: &TestDaemon, dir: &Path) -> std::process::ExitStatus {
    let mut status = orch(dir);
    daemon.apply(&mut status);
    status
        .args(["daemon", "status"])
        .stdout(Stdio::null())
        .status()
        .expect("daemon status")
}

fn orch(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("HOME", dir.join(".test-home"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            dir.join(".test-home/global.yml"),
        )
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
fn project_with_an_unreadable_task() -> (TestDaemon, tempfile::TempDir) {
    let (dir, daemon) = initialised_project();
    let mut add = orch(dir.path());
    daemon.apply(&mut add);
    let added = add
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
    (daemon, dir)
}

/// `orch init` in a fresh project, then its explicit test daemon (dispatch
/// off). The daemon is returned first so it is dropped (stopped, process
/// table checked) before the project directory is removed.
fn initialised_project() -> (tempfile::TempDir, TestDaemon) {
    let dir = tempfile::tempdir().expect("tempdir");
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "claude"])
        .output()
        .expect("init");
    let daemon = TestDaemon::start(dir.path(), &dir.path().join(".test-home"));
    (dir, daemon)
}

#[test]
fn a_signal_with_work_outstanding_reports_interrupted_and_exits_one() {
    let (daemon, dir) = project_with_an_unreadable_task();

    let mut serve = orch(dir.path());
    daemon.apply(&mut serve);
    let child = serve
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
         achieve. stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The client dies of the signal it was sent (the conventional 128 + 15);
    // it no longer exits 1 as the in-process orchestrator did.
    assert_eq!(
        output.status.code(),
        Some(143),
        "an interrupted --once client exits 128 + the signal"
    );
    // The daemon, not the client, owns `orchestry.lock`: interrupting the
    // client must neither wedge the project nor stop the daemon.
    assert!(
        status_of(&daemon, dir.path()).success(),
        "the daemon stopped when its client was interrupted"
    );
}

/// The other half, and the part that must *not* change: once every task is
/// terminal, a stop is an ordinary completion and the output is unchanged.
#[test]
fn a_finished_project_still_reports_all_done_and_exits_zero() {
    let (dir, daemon) = initialised_project();

    // No tasks at all: `--once` takes its "empty project exits immediately"
    // path, which is the cheapest way to reach a genuine completion without an
    // agent. The interrupted path is unreachable here by construction.
    let mut serve = orch(dir.path());
    daemon.apply(&mut serve);
    let output = serve
        .args(["serve", "--once", "--log-format", "json"])
        .output()
        .expect("serve --once");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains(r#""result":"all_done""#),
        "a completed run must still say all_done. stdout:\n{stdout}"
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(
        status_of(&daemon, dir.path()).success(),
        "the daemon stopped when its client finished"
    );
}
