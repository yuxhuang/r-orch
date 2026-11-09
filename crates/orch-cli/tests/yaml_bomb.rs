//! A planted YAML bomb is refused, and the process exits.
//!
//! The security re-review's C0 finding was not that the parse was slow but
//! that the mitigation made it *non-terminating*: a 2 s deadline printed the
//! right error, then `spawn_blocking` carried on allocating ~0.55 GiB/s —
//! 19.3 GiB RSS at 28 s, 180 GiB at three minutes — and tokio waited for that
//! task at shutdown, so the process never exited.
//!
//! These tests go through the real binary because that is where the failure
//! lived. A unit test on `read_yaml_value` returns an `Err` and tells you
//! nothing about whether the process it belonged to could die: the old build
//! passed the equivalent unit test while `orch status` ran forever.
//!
//! Every YAML read goes through the same chokepoint, so a bomb is planted in
//! each of the three kinds of file ORCH reads on a plain command: the project
//! config, a task and an agent.
//!
//! Since S2a-cli every one of those reads (`config_store.read()`,
//! `task_service` / `agent_service` lookups) happens **inside the daemon
//! process** answering the RPC, not inside the short-lived client the test
//! spawns: the client is a thin socket client with nothing of its own to
//! balloon. `project()` therefore starts an explicit test daemon right after
//! `orch init` (`tests/common/daemon.rs`), and the RSS sampler below watches
//! the daemon's pid while it waits for the client to get its answer: a
//! regression of the C0 bug would now mean the daemon's RPC handler never
//! returns (its request task is stuck on the still-ballooning
//! `spawn_blocking`), so the client hangs waiting on the socket exactly as
//! the single binary used to hang waiting on its own stuck task.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;

#[path = "common/daemon.rs"]
mod daemon;
use daemon::TestDaemon;

/// `shapeD_6000_30` from gate-security-rereview.md §1: a 6,000-element leaf,
/// then 30 levels of `[*prev, *prev]`. 60 aliases — under any sane alias cap —
/// and 2^30 expansion from 84 KB of text.
fn shape_d() -> String {
    let mut doc = String::from("b0: &b0 [");
    for i in 0..6_000 {
        if i > 0 {
            doc.push(',');
        }
        doc.push_str("\"yyyyyyyyyy\"");
    }
    doc.push_str("]\n");
    for level in 1..=30 {
        doc.push_str(&format!(
            "b{level}: &b{level} [*b{}, *b{}]\n",
            level - 1,
            level - 1
        ));
    }
    doc
}

fn orch(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(dir)
        .env("HOME", dir.join("home"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", dir.join("home/global.yml"))
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG");
    daemon::apply_for(dir, &mut command);
    command
}

/// A throwaway initialised project with its own explicit test daemon.
struct Project {
    // Dropped first: the daemon stops before its project directory goes.
    daemon: TestDaemon,
    dir: tempfile::TempDir,
}

impl Project {
    fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        daemon::unregister_project(self.dir.path());
    }
}

fn project() -> Project {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("home")).unwrap();
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "claude"])
        .assert()
        .success();
    // Every command that can trip the bomb (`status`, `task show`, `agent
    // list`) is a daemon RPC: the explicit test daemon runs before any of
    // them is issued.
    let daemon = TestDaemon::start(dir.path(), &dir.path().join("home"));
    daemon::register_project(dir.path(), &daemon);
    Project { daemon, dir }
}

/// Run the client `command` to completion, sampling the *daemon's* peak RSS
/// in MiB throughout (sampled from `/proc/<daemon_pid>/status`): the bomb is
/// read inside the daemon process that answers the RPC, not inside the
/// one-shot client this spawns.
///
/// Returned alongside the client's exit code and wall time, because the
/// three together are the finding: a broken build would answer the client
/// quickly in one sense (an error prints, the client exits) while the
/// daemon's RSS climbs without bound in the background, invisible to anyone
/// watching only the one-shot command — or, if the stuck `spawn_blocking` task
/// also wedges the connection handler answering this very request, the client
/// never gets a response at all, which this function treats the same way the
/// original single-process test treated a process that never exited: a
/// panic, not a passing test with a lucky timeout.
fn run_sampling_daemon_rss(
    command: &mut Command,
    daemon_pid: u32,
    budget: Duration,
) -> (Option<i32>, Duration, u64) {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orch");
    let mut peak_kib = 0u64;

    loop {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{daemon_pid}/status")) {
            for line in status.lines() {
                if let Some(kib) = line
                    .strip_prefix("VmRSS:")
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    peak_kib = peak_kib.max(kib);
                }
            }
        }
        match child.try_wait().expect("try_wait") {
            Some(status) => return (status.code(), started.elapsed(), peak_kib / 1024),
            None => {
                if started.elapsed() > budget {
                    // The thing this test exists to catch. Kill the client so
                    // the suite does not hang; the daemon is cleaned up by
                    // `Project`'s drop, which will also flag it as a survivor
                    // if it cannot be stopped.
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "the client never got a response within {budget:?} — daemon peak RSS \
                         {} MiB. This is the C0 failure: the daemon keeps allocating and the \
                         request that hit the bomb never completes.",
                        peak_kib / 1024
                    );
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn a_bomb_in_config_is_refused_and_the_daemon_stays_bounded() {
    let project = project();
    std::fs::write(project.path().join(".orchestry/config.yml"), shape_d()).unwrap();

    let mut command = orch(project.path());
    command.arg("status");
    let (code, elapsed, peak_mib) =
        run_sampling_daemon_rss(&mut command, project.daemon.pid(), Duration::from_secs(20));

    assert_eq!(
        code,
        Some(1),
        "same exit code as the other pre-parse refusals"
    );
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    // The review measured 4.56 GiB at 2 s and 19.3 GiB at 28 s on this input.
    // A rejected document is never built, so RSS stays at ordinary levels.
    assert!(
        peak_mib < 200,
        "peak daemon RSS {peak_mib} MiB: the document is being expanded"
    );
}

/// Note which command this uses, and why. `task list` reads `_index.json` and
/// never opens the per-task files, so it exits **0** with a bomb planted in
/// one — measured, when this test first used it. The index shields task files
/// from the listing commands, so the read has to be forced with `task show`.
/// Worth knowing in its own right: a corrupt task file is invisible to
/// `task list`.
#[test]
fn a_bomb_in_a_task_file_is_refused_and_the_daemon_stays_bounded() {
    let project = project();
    let added = orch(project.path())
        .args(["task", "add", "a task", "--quiet"])
        .output()
        .expect("task add");
    let id = String::from_utf8_lossy(&added.stdout).trim().to_string();
    assert!(
        id.starts_with("tsk_"),
        "expected an id on stdout, got {id:?}"
    );

    std::fs::write(
        project.path().join(format!(".orchestry/tasks/{id}.yml")),
        shape_d(),
    )
    .unwrap();

    let mut command = orch(project.path());
    command.args(["task", "show", &id]);
    let (code, elapsed, peak_mib) =
        run_sampling_daemon_rss(&mut command, project.daemon.pid(), Duration::from_secs(20));
    assert_eq!(code, Some(1), "exit code 1");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert!(peak_mib < 200, "peak daemon RSS {peak_mib} MiB");
}

#[test]
fn a_bomb_in_an_agent_file_is_refused_and_the_daemon_stays_bounded() {
    let project = project();
    std::fs::write(
        project.path().join(".orchestry/agents/agt_creator.yml"),
        shape_d(),
    )
    .unwrap();

    let mut command = orch(project.path());
    command.args(["agent", "list"]);
    let (code, elapsed, peak_mib) =
        run_sampling_daemon_rss(&mut command, project.daemon.pid(), Duration::from_secs(20));
    assert_eq!(code, Some(1), "exit code 1");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert!(peak_mib < 200, "peak daemon RSS {peak_mib} MiB");
}

/// T-E2E: Markdown stars in a task description are text, not YAML aliases.
#[test]
fn task_show_prints_markdown_in_a_literal_block_scalar() {
    let project = project();
    let added = orch(project.path())
        .args(["task", "add", "a markdown task", "--quiet"])
        .output()
        .expect("task add");
    assert!(added.status.success(), "{added:?}");
    let id = String::from_utf8(added.stdout).unwrap().trim().to_string();
    assert!(id.starts_with("tsk_"), "expected task id, got {id:?}");
    let path = project.path().join(format!(".orchestry/tasks/{id}.yml"));
    let task = std::fs::read_to_string(&path).unwrap();
    let body = (0..40)
        .map(|n| format!("- **Role {n}:** be *efficient*"))
        .collect::<Vec<_>>();
    // The task was created without a description; replace its empty field
    // with a literal block while preserving the rest of the generated YAML.
    let mut doc = task
        .lines()
        .filter(|line| !line.starts_with("description:"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    doc.push_str("description: |-\n");
    for line in &body {
        doc.push_str(&format!("  {line}\n"));
    }
    std::fs::write(&path, doc).unwrap();

    let shown = orch(project.path())
        .args(["task", "show", &id])
        .output()
        .expect("task show");
    assert!(shown.status.success(), "{shown:?}");
    let output = String::from_utf8(shown.stdout).unwrap();
    for line in body {
        assert!(output.contains(&line), "missing {line:?}: {output}");
    }
}

/// S2a-cli: `orchestry.lock` is now the daemon's own single-instance marker
/// (`crates/orch-daemon/src/guard.rs`), held for the daemon's whole
/// lifetime — not a transient per-command lock a *client* could strand by
/// hitting the bomb mid-read. With an explicit test daemon running (required
/// for `status` to work at all under S2a-cli), the file exists throughout for
/// reasons unrelated to this test, so the original "a refused read leaves no
/// lock behind" assertion is now unconditionally false and proves nothing.
///
/// The regression this guarded — a rejected read wedging the thing that was
/// holding the lock — plausibly still matters, but now as "the daemon stays
/// responsive after refusing a bomb", which needs a decision on how to probe
/// a specific RPC connection's health (a follow-up request on the same
/// daemon, once its config is restored to something readable?) rather than a
/// file-existence check.
#[test]
#[ignore = "S2a-cli: orchestry.lock is now the daemon's permanent single-instance \
            marker, not a transient per-command lock; the file existing while a test \
            daemon runs is expected and no longer signals a stranded lock. Needs a \
            decision on what \"the daemon stays healthy after refusing a bomb\" should \
            assert instead."]
fn refusing_a_bomb_leaves_no_lock_behind() {
    let project = project();
    std::fs::write(project.path().join(".orchestry/config.yml"), shape_d()).unwrap();

    let mut command = orch(project.path());
    command.arg("status");
    let (code, _, _) =
        run_sampling_daemon_rss(&mut command, project.daemon.pid(), Duration::from_secs(20));
    assert_eq!(code, Some(1));
    assert!(
        !project.path().join(".orchestry/orchestry.lock").exists(),
        "a refused read must not strand the lock"
    );
}
