//! ARCHITECTURE A2: what happens when several `orch` processes mutate
//! `_index.json` at once.
//!
//! Up to 0.13.0 every one-shot `task add` wrote the index itself, so two of
//! them racing lost an update (D10). Since the daemon owns the project, each
//! `orch` process is a client and the daemon is the only writer, so
//! concurrent adds from separate processes now lose nothing. The harness
//! starts an explicit test daemon on a private short socket, as every
//! binary-level test does; the stress test in orch-cli
//! (`tests/stress_single_writer.rs`) is the broader proof.
//!
//! Asserted:
//!
//! 1. Every task **file** survives (each is an independent atomic write).
//! 2. `_index.json` is valid JSON and names exactly the files: no lost update.
//! 3. The offline recovery still works: with the daemon stopped, deleting the
//!    index and starting again rebuilds it from the files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const TASKS: usize = 12;
const ISOLATION_SCENARIOS_COMPLETED: &str = "concurrent-index isolation scenarios completed";

struct Harness {
    project: tempfile::TempDir,
    home: PathBuf,
    rust: PathBuf,
    /// Short directory for `ORCH_SOCKET`: a temp project path can exceed the
    /// 104-byte Unix socket limit on macOS.
    socket_dir: tempfile::TempDir,
}

/// Where cargo put this build's binaries.
///
/// Derived rather than hard-coded as `<repo>/target`: the target directory
/// moves with `CARGO_TARGET_DIR`, and a stale or foreign `target/debug/orch`
/// is the dangerous failure here — the test would spawn a *different build*
/// and report green about code that was never exercised. Build artifacts from
/// two trees hash identically, so nothing else would notice.
fn target_dir(repo_root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => repo_root.join("target"),
    }
}

// Both direct Git and Git launched inside the CLI must use the fixture's
// repository, index and config rather than an ambient hook's overrides.
fn scrub_git_env(command: &mut Command) {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(name);
    }
}

impl Harness {
    /// A missing binary fails loudly rather than skipping: cargo hides an
    /// `eprintln` for a passing test, so the whole file would report green
    /// while asserting nothing.
    fn new() -> Harness {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let rust = target_dir(&repo_root).join(profile).join("orch");

        assert!(
            rust.exists(),
            "{} is missing — run `cargo build -p orch-cli`",
            rust.display()
        );

        let project = tempfile::Builder::new()
            .prefix("r-orch-a2-")
            .tempdir()
            .expect("tempdir");
        orch_core::test_support::assert_hermetic(project.path());
        let home = project.path().join(".harness-home");
        std::fs::create_dir_all(home.join(".orchestry")).expect("harness home");
        // A warm update-check cache, so no update check goes to the network.
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

        let socket_dir = tempfile::Builder::new()
            .prefix("a2")
            .tempdir()
            .expect("socket dir");
        // The daemon refuses a socket directory others can read.
        std::fs::set_permissions(
            socket_dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("socket dir mode");
        let harness = Harness {
            project,
            home,
            rust,
            socket_dir,
        };
        harness.git(&["init", "--initial-branch=main"]);
        harness.git(&["config", "user.email", "a2@r-orch.test"]);
        harness.git(&["config", "user.name", "A2"]);
        harness.git(&["commit", "--allow-empty", "-m", "Initial commit"]);
        harness.run_rust(&["init", "--name", "a2", "--adapter", "shell"]);
        harness
    }

    fn git(&self, args: &[&str]) {
        let mut git = Command::new("git");
        scrub_git_env(&mut git);
        let status = git
            .args(args)
            .current_dir(self.project.path())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .output()
            .expect("git");
        assert!(status.status.success(), "git {args:?} failed");
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.rust);
        scrub_git_env(&mut command);
        command
            .current_dir(self.project.path())
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.home.join(".orchestry/global.yml"),
            )
            .env("ORCH_PROJECT_ROOT", self.project.path())
            .env("ORCH_SOCKET", self.socket_dir.path().join("s"))
            .env("CI", "1")
            .env("NO_COLOR", "1")
            .env_remove("ORCH_AGENT_ID")
            .env_remove("ORCH_AGENT_NAME")
            .env_remove("ORCH_TASK_ID")
            .stdin(std::process::Stdio::null());
        command
    }

    /// Start the project's daemon explicitly (nothing starts one implicitly).
    fn start_daemon(&self) {
        let output = self.run_rust(&["daemon", "start", "--detach"]);
        assert!(
            output.status.success(),
            "daemon start failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn stop_daemon(&self) {
        let output = self.run_rust(&["daemon", "stop"]);
        assert!(
            output.status.success(),
            "daemon stop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_rust(&self, args: &[&str]) -> std::process::Output {
        self.command().args(args).output().expect("run orch")
    }
}

impl Drop for Harness {
    /// Never leave a daemon behind, even when an assertion failed.
    fn drop(&mut self) {
        let _ = self.run_rust(&["daemon", "stop"]);
        // Do not hide a failed stop, or double-panic during another failure.
        if !std::thread::panicking() {
            assert!(
                !self.project.path().join(".orchestry/daemon.json").exists(),
                "fixture daemon survived Drop"
            );
        }
    }
}

impl Harness {
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

/// Fire `task add` from separate processes at once and check the three
/// invariants. Separate processes, so only the single daemon writer can keep
/// the index complete.
#[test]
fn concurrent_index_mutation_through_the_daemon_loses_nothing() {
    let harness = Harness::new();
    harness.start_daemon();

    let mut children = Vec::new();
    for n in 0..TASKS {
        let title = format!("task {n}");
        let child = harness
            .command()
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

    // 2. One writer: the index is valid and names exactly the files.
    assert_eq!(indexed, files, "an index update was lost or invented");
    assert_eq!(
        harness.listed_ids(),
        indexed,
        "task list must mirror the index"
    );

    // 3. The documented offline recovery: with the daemon stopped, removing
    //    the index and starting again rebuilds it from the files.
    harness.stop_daemon();
    std::fs::remove_file(harness.tasks_dir().join("_index.json")).expect("remove index");
    harness.start_daemon();
    assert_eq!(
        harness.listed_ids(),
        files,
        "a rebuild must recover every task that has a file"
    );
}

/// The same adds run strictly sequentially, as the control for the helpers
/// and the daemon start-up above.
#[test]
fn sequential_adds_lose_nothing() {
    let harness = Harness::new();
    harness.start_daemon();

    for n in 0..6 {
        let title = format!("task {n}");
        let output = harness
            .command()
            .args(["task", "add", &title, "--quiet"])
            .output()
            .expect("run task add");
        assert!(output.status.success());
    }

    let files = harness.task_files();
    assert_eq!(files.len(), 6);
    assert_eq!(harness.index_ids(), files);
    assert_eq!(harness.listed_ids(), files);
}

/// A sanity check on the helpers themselves, so a green result above cannot
/// come from reading the wrong directory.
#[test]
fn the_helpers_read_the_directory_they_claim_to() {
    let harness = Harness::new();
    assert!(
        harness.task_files().is_empty(),
        "a fresh project has no tasks"
    );

    // `init` deliberately creates no index; the daemon builds one from the
    // empty directory, at start-up or on the first list.
    assert!(!harness.tasks_dir().join("_index.json").exists());
    harness.start_daemon();
    assert!(harness.listed_ids().is_empty());
    assert!(harness.tasks_dir().join("_index.json").exists());

    let output = harness.run_rust(&["task", "add", "one", "--quiet"]);
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(id.starts_with("tsk_"), "{id}");
    assert_eq!(harness.task_files(), BTreeSet::from([id.clone()]));
    assert_eq!(harness.index_ids(), BTreeSet::from([id.clone()]));
    assert_eq!(harness.listed_ids(), BTreeSet::from([id]));
}

/// Runs only in a subprocess whose ambient project is a synthetic decoy.
#[test]
fn project_isolation_child() {
    if std::env::var_os("CONCURRENT_INDEX_ISOLATION_CHILD").is_none() {
        return;
    }
    the_helpers_read_the_directory_they_claim_to();
    sequential_adds_lose_nothing();
    concurrent_index_mutation_through_the_daemon_loses_nothing();
    println!("{ISOLATION_SCENARIOS_COMPLETED}");
}

fn project_contents(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read decoy directory") {
            let entry = entry.expect("decoy entry");
            let kind = entry.file_type().expect("decoy file type");
            if kind.is_dir() {
                visit(root, &entry.path(), files);
            } else if kind.is_file() {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(entry.path()).expect("read decoy file"),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn assert_inherited_project_is_untouched(running: bool) {
    let decoy = Harness::new();
    // An unborn repository exposes init's first-commit path; a staged index
    // makes redirected index writes observable even when HEAD already exists.
    if !running {
        std::fs::remove_dir_all(decoy.project.path().join(".git")).expect("remove decoy git");
        decoy.git(&["init", "--initial-branch=main"]);
        decoy.git(&["config", "user.email", "a2@r-orch.test"]);
        decoy.git(&["config", "user.name", "A2"]);
    }
    std::fs::write(
        decoy.project.path().join("staged.txt"),
        "decoy staged sentinel\n",
    )
    .expect("write staged decoy");
    decoy.git(&["add", "staged.txt"]);
    if running {
        decoy.start_daemon();
        let output = decoy.run_rust(&["task", "add", "decoy sentinel", "--quiet"]);
        assert!(output.status.success());
    }
    let before = project_contents(decoy.project.path());
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "project_isolation_child", "--nocapture"])
        .env_clear()
        .envs(std::env::var_os("CARGO_TARGET_DIR").map(|dir| ("CARGO_TARGET_DIR", dir)))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &decoy.home)
        .env("TMPDIR", std::env::temp_dir())
        .env("XDG_CONFIG_HOME", &decoy.home)
        .env("XDG_CACHE_HOME", &decoy.home)
        .env("XDG_DATA_HOME", &decoy.home)
        .env("XDG_RUNTIME_DIR", decoy.socket_dir.path())
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            decoy.home.join("global.yml"),
        )
        .env("ORCH_PROJECT_ROOT", decoy.project.path())
        .env("ORCH_SOCKET", decoy.socket_dir.path().join("s"))
        // Git hooks can export these even when HOME and cwd are private.
        .env("GIT_DIR", decoy.project.path().join(".git"))
        .env("GIT_WORK_TREE", decoy.project.path())
        .env("GIT_INDEX_FILE", decoy.project.path().join(".git/index"))
        .env("CONCURRENT_INDEX_ISOLATION_CHILD", "1")
        .current_dir(decoy.project.path())
        .output()
        .expect("run isolated regression child");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(ISOLATION_SCENARIOS_COMPLETED),
        "isolation child did not complete its scenario: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        project_contents(decoy.project.path()),
        before,
        "fixture setup, commands and Drop must leave decoy files, logs and daemon record unchanged"
    );
    if running {
        assert_eq!(
            decoy.listed_ids(),
            decoy.task_files(),
            "decoy daemon must still answer after fixture teardown"
        );
    } else {
        assert!(!decoy.project.path().join(".orchestry/daemon.json").exists());
        assert!(!decoy.socket_dir.path().join("s").exists());
    }
    assert!(
        output.status.success(),
        "isolation child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn inherited_stopped_decoy_is_untouched_after_fixture_teardown() {
    assert_inherited_project_is_untouched(false);
}

#[test]
fn inherited_running_decoy_is_untouched_after_fixture_teardown() {
    assert_inherited_project_is_untouched(true);
}
