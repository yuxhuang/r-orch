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

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;

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
    command
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("home")).unwrap();
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "claude"])
        .assert()
        .success();
    dir
}

/// Peak RSS in MiB while the command runs, sampled from `/proc/<pid>/status`.
///
/// Returned alongside the exit code and the wall time, because the three
/// together are the finding: the broken build exited **non**-zero quickly in
/// one sense (it printed an error) while RSS climbed without bound.
fn run_sampling_rss(command: &mut Command, budget: Duration) -> (Option<i32>, Duration, u64) {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orch");
    let pid = child.id();
    let mut peak_kib = 0u64;

    loop {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
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
                    // The thing this test exists to catch. Kill it so the
                    // suite does not leave a process eating the machine.
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "did not exit within {budget:?} — peak RSS {} MiB. This is the C0 \
                         failure: the error is printed and the process keeps allocating.",
                        peak_kib / 1024
                    );
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn a_bomb_in_config_is_refused_and_the_process_exits() {
    let dir = project();
    std::fs::write(dir.path().join(".orchestry/config.yml"), shape_d()).unwrap();

    let (code, elapsed, peak_mib) =
        run_sampling_rss(orch(dir.path()).arg("status"), Duration::from_secs(20));

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
        "peak RSS {peak_mib} MiB: the document is being expanded"
    );
}

/// Note which command this uses, and why. `task list` reads `_index.json` and
/// never opens the per-task files, so it exits **0** with a bomb planted in
/// one — measured, when this test first used it. The index shields task files
/// from the listing commands, so the read has to be forced with `task show`.
/// Worth knowing in its own right: a corrupt task file is invisible to
/// `task list`.
#[test]
fn a_bomb_in_a_task_file_is_refused_and_the_process_exits() {
    let dir = project();
    let added = orch(dir.path())
        .args(["task", "add", "a task", "--quiet"])
        .output()
        .expect("task add");
    let id = String::from_utf8_lossy(&added.stdout).trim().to_string();
    assert!(
        id.starts_with("tsk_"),
        "expected an id on stdout, got {id:?}"
    );

    std::fs::write(
        dir.path().join(format!(".orchestry/tasks/{id}.yml")),
        shape_d(),
    )
    .unwrap();

    let (code, elapsed, peak_mib) = run_sampling_rss(
        orch(dir.path()).args(["task", "show", &id]),
        Duration::from_secs(20),
    );
    assert_eq!(code, Some(1), "exit code 1");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert!(peak_mib < 200, "peak RSS {peak_mib} MiB");
}

#[test]
fn a_bomb_in_an_agent_file_is_refused_and_the_process_exits() {
    let dir = project();
    std::fs::write(
        dir.path().join(".orchestry/agents/agt_creator.yml"),
        shape_d(),
    )
    .unwrap();

    let (code, elapsed, peak_mib) = run_sampling_rss(
        orch(dir.path()).args(["agent", "list"]),
        Duration::from_secs(20),
    );
    assert_eq!(code, Some(1), "exit code 1");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert!(peak_mib < 200, "peak RSS {peak_mib} MiB");
}

/// The lock is the reason the backstop may not be a `process::exit` inside
/// `orch-infra` (gate-security's dissent): `read_yaml_value` is the chokepoint
/// for every YAML read, so exiting there could strand `orchestry.lock`.
/// Rejecting through the ordinary error path leaves no lock behind.
#[test]
fn refusing_a_bomb_leaves_no_lock_behind() {
    let dir = project();
    std::fs::write(dir.path().join(".orchestry/config.yml"), shape_d()).unwrap();

    let (code, _, _) = run_sampling_rss(orch(dir.path()).arg("status"), Duration::from_secs(20));
    assert_eq!(code, Some(1));
    assert!(
        !dir.path().join(".orchestry/orchestry.lock").exists(),
        "a refused read must not strand the lock (§14.15)"
    );
}

/// T-E2E: Markdown stars in a task description are text, not YAML aliases.
#[test]
fn task_show_prints_markdown_in_a_literal_block_scalar() {
    let dir = project();
    let added = orch(dir.path())
        .args(["task", "add", "a markdown task", "--quiet"])
        .output()
        .expect("task add");
    assert!(added.status.success(), "{added:?}");
    let id = String::from_utf8(added.stdout).unwrap().trim().to_string();
    assert!(id.starts_with("tsk_"), "expected task id, got {id:?}");
    let path = dir.path().join(format!(".orchestry/tasks/{id}.yml"));
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

    let shown = orch(dir.path())
        .args(["task", "show", &id])
        .output()
        .expect("task show");
    assert!(shown.status.success(), "{shown:?}");
    let output = String::from_utf8(shown.stdout).unwrap();
    for line in body {
        assert!(output.contains(&line), "missing {line:?}: {output}");
    }
}
