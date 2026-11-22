//! `cargo xtask fixture-gen`: the checked-in corpus reproduces byte for byte, drift is detected on
//! a disposable copy, and the corpus reads back through the real 1.0 file stores.
//!
//! Every child process runs with a cleared environment and a private HOME, TMPDIR and XDG tree, and
//! only the checked-in corpus is read in place; everything that is damaged is a copy.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orch_core::storage::{
    AgentStore, ConfigStore, ContextStore, GoalStore, MessageStore, RunStore, StateStore,
    TaskFilter, TaskStore, TeamStore,
};
use orch_domain::{RunStatus, TaskStatus};
use orch_infra::storage::{
    FileAgentStore, FileConfigStore, FileContextStore, FileGoalStore, FileMessageStore,
    FileRunStore, FileStateStore, FileTaskStore, FileTeamStore, Paths,
};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/storage/generated")
}

fn small() -> PathBuf {
    corpus().join("small")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            assert!(
                kind.is_file(),
                "the corpus holds only files: {:?}",
                entry.path()
            );
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A private home and the copies a test damages, removed with it.
struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "home", "tmp", "config", "data", "cache", "state", "runtime", "work",
        ] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        fs::write(root.path().join("global.yml"), "{}\n").unwrap();
        Sandbox { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The xtask binary with nothing inherited: no ORCH_* or ORCHESTRY_* variable, no real home.
    fn xtask(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .env_clear()
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", self.path("global.yml"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .current_dir(self.path("work"))
            .arg("fixture-gen")
            .args(args);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    /// A copy of the checked-in corpus to damage.
    fn copy_of_corpus(&self, name: &str) -> PathBuf {
        let copy = self.path(name);
        copy_tree(&corpus(), &copy);
        copy
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn check(sandbox: &Sandbox, dir: &Path) -> Output {
    sandbox.xtask(&["--check", "--dir", dir.to_str().unwrap()], &[])
}

#[test]
fn check_reproduces_the_checked_in_corpus_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let before = snapshot(&corpus());
    let out = check(&sandbox, &corpus());
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("match a fresh generation byte for byte"));
    assert_eq!(
        snapshot(&corpus()),
        before,
        "--check must not touch the tracked corpus"
    );
}

#[test]
fn inherited_orch_variables_do_not_change_the_generation() {
    let sandbox = Sandbox::new();
    let out = sandbox.xtask(
        &["--check", "--dir", corpus().to_str().unwrap()],
        &[
            ("ORCH_TEST_OS_CAP", "hostile"),
            ("ORCH_PROJECT", "/nonexistent"),
            ("ORCH_SOCKET", "/nonexistent/orch.sock"),
            ("ORCHESTRY_GLOBAL_CONFIG_PATH", "/nonexistent/global.yml"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn a_changed_byte_a_missing_file_and_an_extra_file_are_each_detected_on_a_copy() {
    let sandbox = Sandbox::new();
    let copy = sandbox.copy_of_corpus("copy");
    // Control: an untouched copy passes, so each failure below is caused by its edit.
    assert!(check(&sandbox, &copy).status.success());

    let task = copy.join("small/.orchestry/tasks/_index.json");
    let original = fs::read(&task).unwrap();
    let mut changed = original.clone();
    let last = changed.len() - 2;
    changed[last] ^= 0x01;
    fs::write(&task, &changed).unwrap();
    let out = check(&sandbox, &copy);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("changed: small/.orchestry/tasks/_index.json"),
        "{stderr}"
    );
    assert!(stderr.contains("first difference at byte"), "{stderr}");
    fs::write(&task, &original).unwrap();

    let state = copy.join("small/.orchestry/state.json");
    let state_bytes = fs::read(&state).unwrap();
    fs::remove_file(&state).unwrap();
    let out = check(&sandbox, &copy);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("missing: small/.orchestry/state.json"));
    fs::write(&state, state_bytes).unwrap();

    fs::write(copy.join("small/.orchestry/stray.txt"), "left behind").unwrap();
    let out = check(&sandbox, &copy);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("extra: small/.orchestry/stray.txt"));
    fs::remove_file(copy.join("small/.orchestry/stray.txt")).unwrap();

    assert!(
        check(&sandbox, &copy).status.success(),
        "restoring every edit must pass again"
    );
    assert_eq!(
        snapshot(&copy),
        snapshot(&corpus()),
        "the tracked corpus was never the target"
    );
}

#[test]
fn a_symlink_in_the_corpus_is_refused_not_skipped() {
    let sandbox = Sandbox::new();
    let copy = sandbox.copy_of_corpus("copy");
    std::os::unix::fs::symlink(
        copy.join("README.md"),
        copy.join("small/.orchestry/link.txt"),
    )
    .unwrap();
    let out = check(&sandbox, &copy);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("not a regular file"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn regeneration_repairs_a_damaged_copy_and_refuses_a_directory_that_is_not_a_corpus() {
    let sandbox = Sandbox::new();
    let copy = sandbox.copy_of_corpus("copy");
    fs::write(copy.join("small/.orchestry/stray.txt"), "x").unwrap();
    fs::remove_file(copy.join("small/.orchestry/state.json")).unwrap();
    assert!(!check(&sandbox, &copy).status.success());
    let out = sandbox.xtask(&["--dir", copy.to_str().unwrap()], &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(check(&sandbox, &copy).status.success());
    assert_eq!(snapshot(&copy), snapshot(&corpus()));

    let precious = sandbox.path("precious");
    fs::create_dir(&precious).unwrap();
    fs::write(precious.join("keep.txt"), "keep").unwrap();
    fs::write(precious.join("README.md"), "unrelated project").unwrap();
    let before = snapshot(&precious);
    let out = sandbox.xtask(&["--dir", precious.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("not a generated corpus"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(snapshot(&precious), before);
}

#[test]
fn the_corpus_names_no_machine_path_host_or_account() {
    for (path, bytes) in snapshot(&corpus()) {
        let content = text(&bytes);
        for marker in [
            "/Users/",
            "/home/",
            "/Volumes/",
            "/tmp/",
            "/var/",
            "@",
            "://",
        ] {
            assert!(
                !content.contains(marker),
                "{} contains {marker}",
                path.display()
            );
        }
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// The corpus is a real 1.0 data directory: the file stores read it, in the order their indexes
/// hold, and every entity kind the importer must handle is present. Read on a copy, because a read
/// may rebuild an index.
#[test]
fn the_corpus_reads_back_through_the_real_file_stores() {
    let sandbox = Sandbox::new();
    let root = sandbox.path("project");
    copy_tree(&small(), &root);
    let paths = Paths::new(&root);
    let clock = std::sync::Arc::new(FixedAt(1_735_689_600_000));

    block_on(async {
        let tasks = FileTaskStore::new(paths.clone())
            .list(&TaskFilter::default())
            .await
            .unwrap();
        // The task list sorts by status; the index (last-write order) is what the importer needs, so
        // compare against the file the store was built from.
        let index: Vec<serde_json::Value> = serde_json::from_slice(
            &fs::read(small().join(".orchestry/tasks/_index.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(tasks.len(), index.len());
        assert_eq!(
            tasks.len(),
            12,
            "twelve tasks stay; the two deleted ones are gone"
        );
        for status in [
            TaskStatus::Todo,
            TaskStatus::Retrying,
            TaskStatus::Review,
            TaskStatus::Done,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            assert!(
                tasks.iter().any(|t| t.status == status),
                "no task is {status}"
            );
        }
        // R117: no two task ids differ only in ASCII case.
        let mut folded: Vec<String> = tasks.iter().map(|t| t.id.to_ascii_lowercase()).collect();
        folded.sort();
        folded.dedup();
        assert_eq!(folded.len(), tasks.len());

        let agents = FileAgentStore::new(paths.clone()).list().await.unwrap();
        assert_eq!(agents.len(), 4);
        assert_eq!(
            FileGoalStore::new(paths.clone())
                .list(&Default::default())
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            FileTeamStore::new(paths.clone())
                .list()
                .await
                .unwrap()
                .len(),
            1
        );

        let runs = FileRunStore::new(paths.clone());
        let all = runs.list_all().await.unwrap();
        assert!(all.len() >= 12);
        assert!(all.iter().any(|r| r.status == RunStatus::Succeeded));
        assert!(all.iter().any(|r| r.status == RunStatus::Failed));
        for run in &all {
            assert!(
                tasks.iter().any(|t| t.id == run.task_id),
                "{} has no task",
                run.id
            );
            let events = runs.read_events(&run.id).await.unwrap();
            assert!(events.len() >= 3, "{} has {} events", run.id, events.len());
        }

        let messages = FileMessageStore::new(paths.clone(), clock.clone())
            .list()
            .await
            .unwrap();
        assert_eq!(messages.len(), 6);
        let context = FileContextStore::new(paths.clone(), clock)
            .list()
            .await
            .unwrap();
        assert_eq!(context.len(), 3);

        let state = FileStateStore::new(paths.clone()).read().await.unwrap();
        assert!(
            state.running.is_empty(),
            "a running entry would make the import refuse"
        );
        assert!(state.claimed.is_empty());
        assert!(!state.retry_queue.is_empty());
        assert_eq!(state.stats.total_runs, u64::try_from(all.len()).unwrap());

        let config = FileConfigStore::new(paths.clone());
        assert_eq!(
            config.get("x_synthetic.note").await.unwrap(),
            Some(serde_json::json!("an unknown key, kept as written"))
        );
    });
    assert!(small().join(".orchestry/templates/default.md").is_file());
    assert!(small().join(".orchestry/attachments").is_dir());
}

/// A clock the read-back test never moves; the stores only read it for expiry.
struct FixedAt(i64);

impl orch_core::clock::Clock for FixedAt {
    fn now_ms(&self) -> i64 {
        self.0
    }

    fn now_iso(&self) -> String {
        orch_core::clock::iso_from_epoch_ms(self.0)
    }
}
