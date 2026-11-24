//! The check against sources made by the tests: links, special files, hostile names,
//! recorded processes that are alive, and the clean generated corpus (R90, R103, R104,
//! R117, R119, R99). Every source is a private temporary directory of invented content.
use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::Mutex;

struct Src {
    dir: tempfile::TempDir,
}

impl Src {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".orchestry")).unwrap();
        Self { dir }
    }
    fn project(&self) -> &Path {
        self.dir.path()
    }
    fn orch(&self) -> std::path::PathBuf {
        self.dir.path().join(".orchestry")
    }
    fn put(&self, rel: &str, bytes: impl AsRef<[u8]>) {
        let path = self.orch().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn task(&self, id: &str) {
        self.put(&format!("tasks/{id}.yml"), task_yaml(id));
    }
}

fn task_yaml(id: &str) -> String {
    format!(
        "id: {id}\ntitle: t\ndescription: d\nstatus: todo\npriority: 1\n\
         created_at: '2025-01-01T00:00:00.000Z'\nupdated_at: '2025-01-01T00:00:00.000Z'\n\
         attempts: 0\nmax_attempts: 3\n"
    )
}

/// What reading it would show: a depth bomb that no limit lets through.
fn bomb() -> String {
    format!("{}0{}", "[".repeat(100), "]".repeat(100))
}

async fn run(src: &Src) -> Report {
    run_with(src, Options::default(), Arc::new(SystemProbe)).await
}

async fn run_with(src: &Src, options: Options, probe: Arc<dyn ProcessProbe>) -> Report {
    preflight(src.project().to_owned(), options, probe)
        .await
        .unwrap()
}

fn refusals(report: &Report) -> Vec<(&str, &str)> {
    report
        .refusals
        .listed
        .iter()
        .map(|f| (f.path.as_str(), f.rule))
        .collect()
}

fn notes(report: &Report) -> Vec<(&str, &str)> {
    report
        .notes
        .listed
        .iter()
        .map(|f| (f.path.as_str(), f.rule))
        .collect()
}

/// Content, kind, mode and times of every entry, so that a read-only check can be shown
/// to have changed nothing at all.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    use std::os::unix::fs::MetadataExt;
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let content = if meta.is_file() {
                format!("{:x}", sha2::Sha256::digest(fs::read(&path).unwrap()))
            } else {
                String::new()
            };
            out.insert(
                rel,
                format!(
                    "{:o} {} {} {} {content}",
                    meta.mode(),
                    meta.len(),
                    meta.mtime_nsec(),
                    meta.mtime()
                ),
            );
            if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    out
}
use sha2::Digest;

fn corpus() -> Src {
    let src = Src::new();
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/storage/generated/small/.orchestry");
    copy(&from, &src.orch());
    src
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_generated_corpus_passes_and_the_check_changes_nothing() {
    let src = corpus();
    // A source the check cannot write: it must not need to, and must not try.
    let lock_down = |mode_dir: u32, mode_file: u32| {
        let mut stack = vec![src.orch()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path.clone());
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode_dir)).unwrap();
                } else {
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode_file)).unwrap();
                }
            }
        }
        fs::set_permissions(src.orch(), fs::Permissions::from_mode(mode_dir)).unwrap();
    };
    lock_down(0o555, 0o444);
    let before = snapshot(src.project());
    let report = run(&src).await;
    let after = snapshot(src.project());
    lock_down(0o755, 0o644);
    assert_eq!(before, after);
    assert!(!report.refused(), "{:?}", report.refusals);
    let c = &report.counts;
    assert_eq!(
        (c.tasks, c.agents, c.goals, c.teams, c.messages, c.context),
        (12, 4, 2, 1, 6, 3)
    );
    assert_eq!((c.templates, c.attachments, c.attachment_dirs), (1, 1, 1));
    assert_eq!((c.runs, c.run_event_files, c.run_events), (15, 15, 76));
    assert_eq!(c.indexes, 5);
    assert_eq!((c.claimed, c.retry_entries, c.running), (0, 1, 0));
    // 1.0's own `.index.lock` files have no rule in R99 and are named, not read.
    assert!(notes(&report).iter().all(|(_, rule)| matches!(
        *rule,
        "not_imported" | "unknown_config" | "preapproval_confirmation"
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn every_source_path_is_accounted_for() {
    let src = Src::new();
    src.put("mystery.txt", "x");
    src.put("mystery/deep.yml", bomb());
    src.put("tasks/readme.txt", "x");
    src.put("peer-id", "x");
    src.put("remotes.yml", bomb());
    src.put("workspace-exclude", "x");
    fs::create_dir(src.orch().join("logs")).unwrap();
    fs::create_dir(src.orch().join("run")).unwrap();
    let report = run(&src).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(
        notes(&report),
        [
            ("mystery", "not_imported"),
            ("mystery.txt", "not_imported"),
            ("tasks/_index.json", "index_unreadable"),
            ("tasks/readme.txt", "not_imported"),
            ("config.yml", "config_default")
        ]
    );
    assert_eq!(report.counts.left_in_place, 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn source_links_and_special_files_are_refused_unopened() {
    let outside = tempfile::tempdir().unwrap();
    let canary = outside.path().join("canary.yml");
    fs::write(&canary, bomb()).unwrap();
    let src = Src::new();
    src.task("tsk_ok");
    symlink(&canary, src.orch().join("tasks/tsk_link.yml")).unwrap();
    fs::hard_link(
        src.orch().join("tasks/tsk_ok.yml"),
        src.orch().join("tasks/tsk_hard.yml"),
    )
    .unwrap();
    nix::unistd::mkfifo(
        &src.orch().join("tasks/tsk_fifo.yml"),
        nix::sys::stat::Mode::S_IRWXU,
    )
    .unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(src.orch().join("tasks/tsk_sock.yml")).unwrap();
    // A link as a store directory, and as the index and an attachment.
    fs::create_dir(outside.path().join("agents")).unwrap();
    fs::write(outside.path().join("agents/agt_x.yml"), bomb()).unwrap();
    symlink(outside.path().join("agents"), src.orch().join("agents")).unwrap();
    symlink(&canary, src.orch().join("tasks/_index.json")).unwrap();
    src.put("attachments/tsk_ok/real.bin", "x");
    symlink(&canary, src.orch().join("attachments/tsk_ok/link.bin")).unwrap();
    fs::hard_link(
        src.orch().join("attachments/tsk_ok/real.bin"),
        src.orch().join("attachments/tsk_ok/hard.bin"),
    )
    .unwrap();
    symlink(outside.path(), src.orch().join("attachments/tsk_linkdir")).unwrap();
    // Not a regular file where a directory is, and the state file as a link.
    src.put("messages", "a file where a directory is expected");
    symlink(&canary, src.orch().join("state.json")).unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(20), run(&src))
        .await
        .expect("a FIFO must not hold the check");
    let mut got = refusals(&report);
    got.sort();
    assert_eq!(
        got,
        [
            ("agents", "file_kind"),
            ("attachments/tsk_linkdir", "file_kind"),
            ("attachments/tsk_ok/hard.bin", "file_kind"),
            ("attachments/tsk_ok/link.bin", "file_kind"),
            ("attachments/tsk_ok/real.bin", "file_kind"),
            ("messages", "file_kind"),
            ("state.json", "file_kind"),
            ("tasks/_index.json", "file_kind"),
            ("tasks/tsk_fifo.yml", "file_kind"),
            ("tasks/tsk_hard.yml", "file_kind"),
            ("tasks/tsk_link.yml", "file_kind"),
            ("tasks/tsk_ok.yml", "file_kind"),
            ("tasks/tsk_sock.yml", "file_kind"),
        ]
    );
    // The canary would have been refused as a `depth` bomb had one byte of it been read.
    assert!(report.refusals.listed.iter().all(|f| f.rule == "file_kind"));
    let kinds: Vec<_> = report
        .refusals
        .listed
        .iter()
        .map(|f| (f.path.as_str(), f.detail.as_str()))
        .collect();
    assert!(kinds.contains(&("tasks/tsk_fifo.yml", "a FIFO")));
    assert!(kinds.contains(&("tasks/tsk_sock.yml", "a socket")));
    assert!(kinds.contains(&("tasks/tsk_hard.yml", "a file with more than one hard link")));
    // Both names of a hard-linked inode are refused: each has a link count of two.
    assert_eq!(report.counts.tasks, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_task_ids_that_differ_only_in_case_are_refused_before_any_write() {
    let src = Src::new();
    for id in ["tsk_Case", "tsk_case", "tsk_cas3", "TSK_CASE"] {
        src.task(id);
    }
    let before = snapshot(src.project());
    let report = run(&src).await;
    assert_eq!(snapshot(src.project()), before);
    // Judged in byte order: `TSK_CASE` stands before `tsk_Case`, which stands before `tsk_case`.
    assert_eq!(
        refusals(&report),
        [
            ("tasks/tsk_Case.yml", "id_alias"),
            ("tasks/tsk_case.yml", "id_alias")
        ]
    );
    let first = &report.refusals.listed[0];
    assert_eq!(first.observed.as_deref(), Some("TSK_CASE"));
    assert_eq!(first.limit, None);
    // Neither id was changed: all four are still tasks that read.
    assert_eq!(report.counts.tasks, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_under_another_spelling_of_a_task_id_is_refused_unopened() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("canary"), "x").unwrap();
    let src = Src::new();
    src.task("tsk_a");
    src.task("tsk_b");
    fs::create_dir_all(src.orch().join("workspaces/tsk_a")).unwrap();
    // Not a directory a check may enter, follow or judge: a link, and a file.
    symlink(outside.path(), src.orch().join("workspaces/TSK_A")).unwrap();
    src.put("workspaces/Tsk_B", "a file");
    src.put("workspaces/stray", "a file");
    let report = run(&src).await;
    assert_eq!(
        refusals(&report),
        [
            ("workspaces/TSK_A", "workspace_alias"),
            ("workspaces/Tsk_B", "workspace_alias")
        ]
    );
    assert_eq!(report.refusals.listed[0].observed.as_deref(), Some("tsk_a"));
    assert_eq!(
        notes(&report),
        [
            ("tasks/_index.json", "index_unreadable"),
            ("config.yml", "config_default"),
            ("workspaces/stray", "workspace_unclaimed")
        ]
    );
    assert_eq!(report.counts.workspaces_listed, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_id_must_be_its_files_stem_and_stems_stay_in_the_alphabet() {
    let src = Src::new();
    src.put("tasks/tsk_a.yml", task_yaml("tsk_b"));
    src.put("tasks/tsk a.yml", task_yaml("tsk a"));
    src.put("tasks/tsk\u{e9}.yml", task_yaml("tsk\u{e9}"));
    src.put("tasks/.yml", task_yaml("x"));
    src.put("tasks/tsk\nforged.yml", task_yaml("tsk"));
    // R104: a task id of `.` or `..` stops the import; a dotted id inside the alphabet does not.
    src.put("tasks/..yml", task_yaml("."));
    src.put("tasks/...yml", task_yaml(".."));
    let before = fs::read_dir(src.orch().join("tasks")).unwrap().count();
    let report = run(&src).await;
    assert_eq!(report.refusals.total(), before as u64);
    assert!(report.refusals.listed.iter().all(|f| f.rule == "id"));
    assert_eq!(report.counts.tasks, 0);
    for f in &report.refusals.listed {
        assert!(!f.path.contains('\n'), "{:?}", f.path);
    }
    assert!(refusals(&report).contains(&("tasks/tsk\\u{a}forged.yml", "id")));
    for dot in ["tasks/..yml", "tasks/...yml"] {
        assert!(refusals(&report).contains(&(dot, "id")), "{dot}");
    }
    let src = Src::new();
    src.task("tsk.a..b");
    src.task("..a");
    let report = run(&src).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(report.counts.tasks, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listing_over_the_entry_limit_is_refused_before_any_document_is_read() {
    let src = Src::new();
    src.put("tasks/tsk_a.yml", bomb());
    for i in 0..5 {
        src.task(&format!("tsk_{i}"));
    }
    let options = Options {
        entry_limit: 5,
        ..Options::default()
    };
    let report = run_with(&src, options, Arc::new(SystemProbe)).await;
    // Five names fit under the limit: the top level holds one, `tasks` six.
    assert_eq!(refusals(&report), [("tasks", "entries")]);
    let f = &report.refusals.listed[0];
    assert_eq!((f.limit, f.observed.as_deref()), (Some(5), Some("6")));
    assert_eq!(report.counts.tasks, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_entry_limit_counts_the_second_level_of_attachments_and_ends_the_listing() {
    let src = Src::new();
    src.put("tasks/tsk_a.yml", bomb());
    for i in 0..4 {
        src.put(&format!("attachments/tsk_a/f{i}"), "x");
    }
    // The top level lists 2 names, `attachments` 1, `attachments/tsk_a` 4 and `tasks/` 1: 8.
    let options = Options {
        entry_limit: 6,
        ..Options::default()
    };
    let report = run_with(&src, options, Arc::new(SystemProbe)).await;
    assert_eq!(refusals(&report), [("attachments/tsk_a", "entries")]);
    let options = Options {
        entry_limit: 8,
        ..Options::default()
    };
    let report = run_with(&src, options, Arc::new(SystemProbe)).await;
    // Under the limit the documents are read: the bomb in `tasks/` is now the only refusal.
    assert_eq!(refusals(&report), [("tasks/tsk_a.yml", "depth")]);
    assert_eq!(report.counts.entries, 8);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_report_of_refusals_is_bounded() {
    let src = Src::new();
    for i in 0..2000 {
        src.put(&format!("tasks/tsk_{i}.yml"), "id: !custom abc\n");
    }
    let report = run(&src).await;
    assert_eq!(report.refusals.listed.len(), REPORT_LIMIT);
    assert_eq!(report.refusals.more, 1000);
    assert!(report.refusals.listed.iter().all(|f| f.rule == "tag"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_that_a_parser_quotes_is_cut_and_escaped() {
    let src = Src::new();
    let hostile = format!("id: {}\u{1b}[31m\nstatus: [", "x".repeat(10_000));
    src.put("tasks/tsk_a.yml", hostile);
    let report = run(&src).await;
    let f = &report.refusals.listed[0];
    assert_eq!(f.rule, "unreadable");
    assert!(f.detail.len() <= 1024 + 3, "{}", f.detail.len());
    assert!(!f.detail.contains('\u{1b}'));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_stream_is_counted_and_unusable_lines_are_one_gap() {
    let src = Src::new();
    let event = r#"{"timestamp":"t","type":"done","data":"x"}"#;
    let long = "x".repeat(70_000);
    src.put(
        "runs/run_a.jsonl",
        format!("{event}\n{long}\n\u{0}\n[[[[0]]]]\nnot json\n{event}\n"),
    );
    // One that is only damaged lines ends in a gap, not an error.
    src.put("runs/run_b.jsonl", format!("{long}\n{long}"));
    let report = run(&src).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(report.counts.run_events, 2);
    assert_eq!(report.counts.run_event_gaps, 2);
    assert_eq!(
        notes(&report),
        [
            ("runs/run_a.jsonl", "run_event_gaps"),
            ("runs/run_b.jsonl", "run_event_gaps"),
            ("config.yml", "config_default")
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_check_ends_without_a_report() {
    let src = corpus();
    let options = Options::default();
    options
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let result = preflight(src.project().to_owned(), options, Arc::new(SystemProbe)).await;
    assert!(
        matches!(result, Err(PreflightError::Cancelled)),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_without_orchestry_or_with_a_linked_one_is_not_read() {
    let empty = tempfile::tempdir().unwrap();
    let result = preflight(
        empty.path().to_owned(),
        Options::default(),
        Arc::new(SystemProbe),
    )
    .await;
    assert!(
        matches!(result, Err(PreflightError::NotInitialised)),
        "{result:?}"
    );
    let real = Src::new();
    real.task("tsk_a");
    let linked = tempfile::tempdir().unwrap();
    symlink(real.orch(), linked.path().join(".orchestry")).unwrap();
    let result = preflight(
        linked.path().to_owned(),
        Options::default(),
        Arc::new(SystemProbe),
    )
    .await;
    assert!(
        matches!(result, Err(PreflightError::Source(_))),
        "{result:?}"
    );
}

// ---- R119

fn running(entries: &[serde_json::Value]) -> String {
    let map: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|e| (e["task_id"].as_str().unwrap().to_owned(), e.clone()))
        .collect();
    json!({"version": 1, "running": map}).to_string()
}

fn entry(run: &str, pid: u64) -> serde_json::Value {
    json!({"run_id": run, "agent_id": "agt_a", "task_id": format!("tsk_{run}"), "pid": pid,
           "started_at": "2025-01-01T00:00:00.000Z", "last_event_at": "2025-01-01T00:00:00.000Z"})
}

fn remote_entry(run: &str) -> serde_json::Value {
    let mut e = entry(run, 0);
    e["remote"] = json!({"alias": "peer-a", "exec_key": "k"});
    e
}

/// A fake process table: it answers from a set and records every call it receives.
#[derive(Default)]
struct Table {
    live: Vec<i32>,
    calls: Mutex<Vec<(&'static str, i32)>>,
}
impl ProcessProbe for Table {
    fn process_exists(&self, pid: i32) -> bool {
        self.calls.lock().unwrap().push(("process", pid));
        self.live.contains(&pid)
    }
    fn group_has_member(&self, pgid: i32) -> bool {
        self.calls.lock().unwrap().push(("group", pgid));
        self.live.contains(&pgid)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pid_of_0_or_1_or_over_the_range_is_never_passed_to_a_group_test() {
    let table = Arc::new(Table::default());
    let src = Src::new();
    src.put(
        "state.json",
        running(&[
            entry("zero", 0),
            entry("one", 1),
            entry("big", 2_147_483_648),
            entry("max", u64::from(u32::MAX)),
        ]),
    );
    let report = run_with(&src, Options::default(), table.clone()).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(*table.calls.lock().unwrap(), [("process", 1)]);
    assert_eq!(report.counts.in_flight_to_close, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_every_remote_entry_whatever_the_peer_registry_says() {
    let src = Src::new();
    // A registry that names the peer, one that does not, and one that is unreadable.
    src.put("remotes.yml", "peers: {peer-a: {}}\n");
    src.put("state.json", running(&[remote_entry("r1")]));
    for registry in [Some("peers: {}\n"), Some("{{{ unreadable"), None] {
        match registry {
            Some(text) => src.put("remotes.yml", text),
            None => fs::remove_file(src.orch().join("remotes.yml")).unwrap(),
        }
        let report = run(&src).await;
        assert_eq!(refusals(&report), [("state.json", "in_flight_remote")]);
        assert!(
            report.refusals.listed[0]
                .detail
                .contains("peer peer-a; run r1")
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn assume_stopped_closes_only_the_named_run() {
    let src = Src::new();
    src.put(
        "state.json",
        running(&[remote_entry("r1"), entry("r2", 4242)]),
    );
    let table = Arc::new(Table {
        live: vec![],
        ..Table::default()
    });
    let options = |names: &[&str]| Options {
        assume_stopped: names.iter().map(|s| (*s).to_owned()).collect(),
        ..Options::default()
    };
    // The remote run alone is named: it passes; the other has no process and passes too.
    let report = run_with(&src, options(&["r1"]), table.clone()).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(report.assumed_stopped, ["r1"]);
    assert_eq!(report.counts.in_flight_to_close, 2);
    // Naming a different run leaves the remote one refused.
    let report = run_with(&src, options(&["r2"]), table).await;
    assert_eq!(refusals(&report), [("state.json", "in_flight_remote")]);
    assert_eq!(report.assumed_stopped, ["r2"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_run_whose_pid_or_group_exists_needs_confirmation() {
    let src = Src::new();
    src.put("state.json", running(&[entry("r1", 4242)]));
    let table = Arc::new(Table {
        live: vec![4242],
        ..Table::default()
    });
    let options = Options {
        assume_stopped: ["r1".to_owned()].into(),
        ..Options::default()
    };
    let report = run_with(&src, options, table).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(report.confirmations.len(), 1);
    let c = &report.confirmations[0];
    assert_eq!((c.run_id.as_str(), c.pid), ("r1", 4242));
    assert_eq!(c.found, "the process and its process group were found");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_id_that_names_no_entry_is_refused() {
    let src = Src::new();
    let options = Options {
        assume_stopped: ["nope".to_owned()].into(),
        ..Options::default()
    };
    let report = run_with(&src, options, Arc::new(Table::default())).await;
    assert_eq!(
        refusals(&report),
        [("state.json", "assume_stopped_unknown")]
    );
    assert_eq!(report.refusals.listed[0].observed.as_deref(), Some("nope"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_liveness_check_sends_no_signal() {
    let table = Arc::new(Table {
        live: vec![],
        ..Table::default()
    });
    let src = Src::new();
    src.put("state.json", running(&[entry("r1", 4242), entry("r2", 77)]));
    run_with(&src, Options::default(), table.clone()).await;
    // The trait has two methods and both are existence tests; a call is all that is seen.
    assert_eq!(
        *table.calls.lock().unwrap(),
        [
            ("process", 4242),
            ("group", 4242),
            ("process", 77),
            ("group", 77)
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_set_the_legacy_reader_would_drop_is_not_read_as_empty() {
    for state in [
        json!({"running": {"tsk_a": {"run_id": "r"}}}),
        json!({"running": []}),
        json!({"running": {"tsk_a": {"run_id": "r", "agent_id": "a", "task_id": "tsk_a",
              "pid": -1, "started_at": "x", "last_event_at": "x"}}}),
    ] {
        let src = Src::new();
        src.put("state.json", state.to_string());
        let report = run(&src).await;
        assert_eq!(refusals(&report), [("state.json", "unreadable")], "{state}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn in_flight_facts_survive_long_and_hostile_ids_and_are_escaped_once() {
    let src = Src::new();
    let long = "r".repeat(400);
    let mut hostile = entry("r1", 4242);
    hostile["run_id"] = json!(format!("a\\b\n{long}"));
    hostile["task_id"] = json!("t".repeat(400));
    src.put("state.json", running(&[hostile]));
    let table = Arc::new(Table {
        live: vec![4242],
        ..Table::default()
    });
    let report = run_with(&src, Options::default(), table).await;
    let detail = &report.refusals.listed[0].detail;
    // The pid and the answer come first and are whole; each id is escaped once and cut.
    assert!(
        detail.starts_with(
            "pid 4242: the process and its process group were found; run a\\\\b\\u{a}rrr"
        ),
        "{detail}"
    );
    assert!(detail.len() < 300 && !detail.contains('\n'), "{detail}");
}

/// A pool of one blocking thread: the check holds it, so nothing it needs may queue for
/// another blocking job of the same pool (the guarded YAML reader parks its parse there).
#[test]
fn a_blocking_pool_of_one_is_enough_for_yaml_documents() {
    let src = corpus();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let report = runtime
        .block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                preflight(
                    src.project().to_owned(),
                    Options::default(),
                    Arc::new(SystemProbe),
                ),
            )
            .await
        })
        .expect("the check waited for a blocking thread that it holds")
        .unwrap();
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(report.counts.tasks, 12);
}

/// The flag reaches a read that is already under way, so a cancel does not wait for an
/// event file that has no byte bound (R111) to end.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_during_a_long_read_ends_the_check_without_an_io_finding() {
    let src = Src::new();
    src.put("runs/run_a.jsonl", "");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(src.orch().join("runs/run_a.jsonl"))
        .unwrap();
    file.set_len(64 * 1024 * 1024 * 1024).unwrap();
    let options = Options::default();
    let flag = options.cancel.clone();
    let started = std::time::Instant::now();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let result = preflight(src.project().to_owned(), options, Arc::new(SystemProbe)).await;
    canceller.await.unwrap();
    assert!(
        matches!(result, Err(PreflightError::Cancelled)),
        "{result:?}"
    );
    // 64 GiB would take minutes to read; the flag is seen within a read or two.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}
