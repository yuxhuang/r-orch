//! V37 / ARCHITECTURE A2: what happens when both binaries mutate
//! `_index.json` at once.
//!
//! A2 claims **single writer**: the PID lock means one process mutates
//! orchestrator state, and the index read-modify-write is mutex-protected only
//! *within* a process. One-shot commands like `task add` do **not** take the
//! lock, so two of them racing is reachable — and the outcome is a lost
//! update, not corruption.
//!
//! This test **documents** that rather than asserting it away. Three
//! properties hold regardless of how the race lands, and they are what is
//! asserted:
//!
//! 1. Every task **file** survives. Each is an independent atomic write, so
//!    nothing is lost on disk.
//! 2. `_index.json` is always valid JSON and always a **subset** of the files.
//!    A lost update drops entries; it never invents or corrupts one.
//! 3. Deleting the index recovers everything, because a missing index triggers
//!    a rebuild from the files.
//!
//! The consequence worth stating plainly: after a race, `task list` can
//! under-report while the task's own file sits on disk. Nothing in the system
//! notices until the index is rebuilt.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

const TASKS: usize = 12;

struct Harness {
    project: tempfile::TempDir,
    home: PathBuf,
    rust: PathBuf,
    oracle: PathBuf,
}

impl Harness {
    /// `None` when the Rust binary or the vendored oracle is missing, so the
    /// suite still runs without them.
    fn new() -> Option<Harness> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let rust = repo_root.join("target/debug/orch");
        let oracle = repo_root.join("parity/orch-ts/dist/cli.js");
        if !rust.exists() {
            eprintln!("skipping: run `cargo build -p orch-cli` first");
            return None;
        }
        if !oracle.exists() {
            eprintln!("skipping: run parity/setup.sh to build the oracle");
            return None;
        }

        let project = tempfile::Builder::new()
            .prefix("r-orch-a2-")
            .tempdir()
            .expect("tempdir");
        let home = project.path().join(".harness-home");
        std::fs::create_dir_all(home.join(".orchestry")).expect("harness home");
        // A warm update-check cache, so the oracle makes no network call.
        std::fs::write(
            home.join(".orchestry/update-check.json"),
            format!(
                "{{\"latest\":\"1.0.34\",\"checked_at\":{}}}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            ),
        )
        .expect("seed update cache");

        let harness = Harness {
            project,
            home,
            rust,
            oracle,
        };
        harness.git(&["init", "--initial-branch=main"]);
        harness.git(&["config", "user.email", "a2@r-orch.test"]);
        harness.git(&["config", "user.name", "A2"]);
        harness.git(&["commit", "--allow-empty", "-m", "Initial commit"]);
        harness.run_rust(&["init", "--name", "a2", "--adapter", "shell"]);
        Some(harness)
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(self.project.path())
            .output()
            .expect("git");
        assert!(status.status.success(), "git {args:?} failed");
    }

    fn command(&self, rust: bool) -> Command {
        let mut command = if rust {
            Command::new(&self.rust)
        } else {
            let mut command = Command::new("node");
            command.arg(&self.oracle);
            command
        };
        command
            .current_dir(self.project.path())
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.home.join(".orchestry/global.yml"),
            )
            .env("CI", "1")
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null());
        command
    }

    fn run_rust(&self, args: &[&str]) -> std::process::Output {
        self.command(true).args(args).output().expect("run orch")
    }

    fn tasks_dir(&self) -> PathBuf {
        self.project.path().join(".orchestry/tasks")
    }

    /// Task ids that have their own file on disk.
    fn task_files(&self) -> BTreeSet<String> {
        std::fs::read_dir(self.tasks_dir())
            .expect("tasks dir")
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".yml").map(str::to_owned)
            })
            .collect()
    }

    /// Task ids present in `_index.json`.
    fn index_ids(&self) -> BTreeSet<String> {
        let path = self.tasks_dir().join("_index.json");
        let raw = std::fs::read_to_string(&path).expect("index readable");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("index is valid JSON after a race");
        value
            .as_array()
            .expect("index is an array")
            .iter()
            .map(|task| task["id"].as_str().expect("id is a string").to_owned())
            .collect()
    }

    /// Ids reported by `task list --json`, which reads the index.
    fn listed_ids(&self) -> BTreeSet<String> {
        let output = self.run_rust(&["task", "list", "--json"]);
        assert!(output.status.success(), "task list failed");
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("list emitted JSON");
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|task| task["id"].as_str().expect("id").to_owned())
            .collect()
    }
}

/// Fire `task add` from both binaries at once and check the three invariants.
#[test]
fn concurrent_cross_binary_index_mutation_loses_updates_without_corrupting_anything() {
    let Some(harness) = Harness::new() else {
        return;
    };

    // Alternate binaries so the writes genuinely interleave.
    let mut children = Vec::new();
    for n in 0..TASKS {
        let title = format!("task {n}");
        let child = harness
            .command(n % 2 == 0)
            .args(["task", "add", &title, "--quiet"])
            .spawn()
            .expect("spawn task add");
        children.push(child);
    }
    for mut child in children {
        let status = child.wait().expect("wait");
        assert!(status.success(), "a `task add` failed outright");
    }

    let files = harness.task_files();
    let indexed = harness.index_ids();

    // 1. Nothing is lost on disk: each task file is its own atomic write.
    assert_eq!(
        files.len(),
        TASKS,
        "a task file went missing — atomic writes should make that impossible"
    );

    // 2. The index is valid and a subset of the files. A lost update drops
    //    entries; it must never invent or corrupt one.
    assert!(
        indexed.is_subset(&files),
        "the index names tasks that have no file: {:?}",
        indexed.difference(&files).collect::<Vec<_>>()
    );

    // What actually happened, recorded rather than asserted — the race may or
    // may not land on any given run.
    let lost = files.len() - indexed.len();
    eprintln!(
        "A2: {} of {TASKS} task files reached the index; {lost} update(s) lost to the race",
        indexed.len()
    );

    // 3. `task list` reads the index, so it reports exactly what the index
    //    holds — under-reporting when updates were lost.
    assert_eq!(
        harness.listed_ids(),
        indexed,
        "task list must mirror the index"
    );

    // And the documented recovery: removing the index rebuilds it from the
    // files, so nothing is permanently invisible.
    std::fs::remove_file(harness.tasks_dir().join("_index.json")).expect("remove index");
    assert_eq!(
        harness.listed_ids(),
        files,
        "a rebuild must recover every task that has a file"
    );
}

/// The same race run strictly sequentially, as the control: with one writer at
/// a time nothing is lost, which is what makes the concurrent result
/// attributable to the race rather than to a bug in either binary.
#[test]
fn sequential_cross_binary_adds_lose_nothing() {
    let Some(harness) = Harness::new() else {
        return;
    };

    for n in 0..6 {
        let title = format!("task {n}");
        let output = harness
            .command(n % 2 == 0)
            .args(["task", "add", &title, "--quiet"])
            .output()
            .expect("run task add");
        assert!(output.status.success());
    }

    let files = harness.task_files();
    assert_eq!(files.len(), 6);
    // Interleaved writers, one at a time: the index keeps up exactly.
    assert_eq!(harness.index_ids(), files);
    assert_eq!(harness.listed_ids(), files);
}

/// Many writers from the **same** binary, which is the case the in-process
/// mutex does cover.
#[test]
fn concurrent_same_binary_adds_in_separate_processes_still_race() {
    let Some(harness) = Harness::new() else {
        return;
    };

    // Separate processes, so the in-process mutex cannot help: this is the
    // same hazard as the cross-binary case and is included so the finding is
    // not mistaken for a TypeScript-versus-Rust incompatibility.
    let mut children = Vec::new();
    for n in 0..TASKS {
        let title = format!("task {n}");
        children.push(
            harness
                .command(true)
                .args(["task", "add", &title, "--quiet"])
                .spawn()
                .expect("spawn"),
        );
    }
    for mut child in children {
        assert!(child.wait().expect("wait").success());
    }

    let files = harness.task_files();
    let indexed = harness.index_ids();
    assert_eq!(files.len(), TASKS);
    assert!(indexed.is_subset(&files));
    eprintln!(
        "A2 (same binary, {TASKS} processes): {} reached the index",
        indexed.len()
    );
}

/// A sanity check on the helpers themselves, so a green result above cannot
/// come from reading the wrong directory.
#[test]
fn the_helpers_read_the_directory_they_claim_to() {
    let Some(harness) = Harness::new() else {
        return;
    };
    assert!(
        harness.task_files().is_empty(),
        "a fresh project has no tasks"
    );

    // `init` deliberately creates no index (PARITY.md §6.11), so the first
    // list builds one from an empty directory.
    assert!(!harness.tasks_dir().join("_index.json").exists());
    assert!(harness.listed_ids().is_empty());
    assert!(harness.tasks_dir().join("_index.json").exists());

    let output = harness.run_rust(&["task", "add", "one", "--quiet"]);
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(id.starts_with("tsk_"), "{id}");
    assert_eq!(harness.task_files(), BTreeSet::from([id.clone()]));
    assert_eq!(harness.index_ids(), BTreeSet::from([id.clone()]));
    assert_eq!(harness.listed_ids(), BTreeSet::from([id]));
}
