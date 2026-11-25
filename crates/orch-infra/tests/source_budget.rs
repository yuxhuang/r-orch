//! The hostile-document corpus of "Import bounds" (spec R109, R111, R116) against the
//! whole-source check. Each entry is made by this file from a size or a seed, and goes
//! into a private source of its own; the check runs on it in a child process under a
//! virtual-memory ceiling and a 30 s clock, and the child reports its peak resident
//! memory, which must stay under 512 MiB whatever the entry says about itself.
//! Nothing here reads a real project.
use orch_infra::import::preflight::{Options, SystemProbe, preflight};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WORKER: &str = "ORCH_SOURCE_BUDGET_WORKER";
const MARK: &str = "SOURCE-BUDGET-REPORT ";
const TIME_LIMIT: Duration = Duration::from_secs(30);
const RSS_LIMIT_KIB: u64 = 512 * 1024;
/// The child's virtual-memory ceiling: far above what the check needs, and low enough
/// that a runaway expansion fails fast instead of taking the machine.
const ADDRESS_SPACE_KIB: u64 = 8 * 1024 * 1024;

/// The child. Ignored so that a plain run does not start it; the parent runs it by name.
#[test]
#[ignore = "worker of the child-process corpus, run by the tests below"]
fn worker() {
    let Ok(dir) = std::env::var(WORKER) else {
        return;
    };
    let report = tokio::runtime::Builder::new_multi_thread()
        // The check's own work must not need more stack than this, whatever the input nests.
        .thread_stack_size(256 * 1024)
        .enable_all()
        .build()
        .unwrap()
        .block_on(preflight(
            dir.into(),
            Options::default(),
            Arc::new(SystemProbe),
        ))
        .unwrap();
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let hwm: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap();
    let out = serde_json::json!({"report": report, "peak_kib": hwm});
    println!("{MARK}{out}");
}

struct Outcome {
    report: Value,
}

impl Outcome {
    fn refusals(&self) -> &Vec<Value> {
        self.report["refusals"]["listed"].as_array().unwrap()
    }
    fn counts(&self, key: &str) -> u64 {
        self.report["counts"][key].as_u64().unwrap()
    }
    fn accepted(&self, what: &str) {
        assert!(self.refusals().is_empty(), "{what}: {:?}", self.refusals());
    }
    fn refused(&self, what: &str, path: &str, rule: &str) -> &Value {
        let r = self.refusals();
        assert_eq!(r.len(), 1, "{what}: {r:?}");
        assert_eq!(r[0]["path"], path, "{what}");
        assert_eq!(r[0]["rule"], rule, "{what}: {:?}", r[0]);
        &r[0]
    }
}

/// Runs the check on `source` in a child of its own and holds it to the ceilings.
fn child(source: &Path, what: &str) -> Outcome {
    let exe = std::env::current_exe().unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let script = format!("ulimit -v {ADDRESS_SPACE_KIB}; exec \"$0\" \"$@\"");
    let started = Instant::now();
    let mut process = Command::new("sh")
        .args(["-c", &script])
        .arg(exe)
        .args([
            "--ignored",
            "--exact",
            "worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(WORKER, source)
        .env("MALLOC_ARENA_MAX", "2")
        .stdin(Stdio::null())
        .stdout(log.reopen().unwrap())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let status = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > TIME_LIMIT {
            process.kill().unwrap();
            process.wait().unwrap();
            panic!("{what}: over {TIME_LIMIT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(status.success(), "{what}: {status}\n{text}");
    let line = text
        .lines()
        .find_map(|l| l.split_once(MARK).map(|(_, rest)| rest))
        .unwrap_or_else(|| panic!("{what}: no report in\n{text}"));
    let out: Value = serde_json::from_str(line).unwrap();
    let peak = out["peak_kib"].as_u64().unwrap();
    eprintln!(
        "{what}: peak resident {peak} KiB in {:?}",
        started.elapsed()
    );
    assert!(peak <= RSS_LIMIT_KIB, "{what}: peak resident {peak} KiB");
    Outcome {
        report: out["report"].clone(),
    }
}

/// A source whose only content is `bytes` at `rel` below `.orchestry/`.
fn source(rel: &str, bytes: impl AsRef<[u8]>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".orchestry").join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    dir
}

// ---- Where a document goes, and the path the report must name.

const CONFIG: (&str, &str) = ("config.yml", "config.yml");
const TASK: (&str, &str) = ("tasks/tsk_a.yml", "tasks/tsk_a.yml");
const STATE: (&str, &str) = ("state.json", "state.json");
const RUN: (&str, &str) = ("runs/run_a.json", "runs/run_a.json");
const INDEX: (&str, &str) = ("tasks/_index.json", "tasks/_index.json");
const CONTEXT_INDEX: (&str, &str) = ("context/_index.json", "context/_index.json");

/// A refused document is refused under the same rule in every slot of its kind, before
/// anything is built from it.
fn refused_everywhere(what: &str, bytes: &[u8], slots: &[(&str, &str)], rule: &str) -> Vec<Value> {
    slots
        .iter()
        .map(|(rel, path)| {
            let dir = source(rel, bytes);
            let outcome = child(dir.path(), &format!("{what} as {rel}"));
            outcome
                .refused(&format!("{what} as {rel}"), path, rule)
                .clone()
        })
        .collect()
}

fn accepted_in(what: &str, bytes: &[u8], slot: (&str, &str)) {
    let dir = source(slot.0, bytes);
    child(dir.path(), &format!("{what} as {}", slot.0)).accepted(what);
}

const YAML_SLOTS: [(&str, &str); 2] = [CONFIG, TASK];
const JSON_SLOTS: [(&str, &str); 4] = [STATE, RUN, INDEX, CONTEXT_INDEX];

// ---- YAML

fn alias_chain(top: usize) -> String {
    let mut text = "a0: &a0 [x]\n".to_owned();
    for k in 1..=top {
        text.push_str(&format!("a{k}: &a{k} [*a{}]\n", k - 1));
    }
    text
}

fn doubling(levels: usize) -> String {
    let mut text = "a0: &a0 [x]\n".to_owned();
    for k in 1..=levels {
        text.push_str(&format!("a{k}: &a{k} [*a{}, *a{}]\n", k - 1, k - 1));
    }
    text
}

fn review_alias() -> String {
    let mut text = "a0: &a0 [x]\n".to_owned();
    for k in 1..=8 {
        let refs = vec![format!("*a{}", k - 1); 10].join(", ");
        text.push_str(&format!("a{k}: &a{k} [{refs}]\n"));
    }
    text
}

fn aliases_of_one_scalar(n: usize) -> String {
    format!("a: &a x\nb: [{}]\n", vec!["*a"; n].join(", "))
}

fn flat_sequence(items: usize) -> String {
    "- x\n".repeat(items)
}

fn repeated_scalar(aliases: usize, extra: bool) -> String {
    let mut text = format!("- &a {}\n", "x".repeat(65_536));
    text.push_str(&"- *a\n".repeat(aliases));
    if extra {
        text.push_str("- y\n");
    }
    text
}

fn merge_bomb() -> String {
    let entries: Vec<String> = (0..4000).map(|i| format!("k{i}: 0")).collect();
    let mut text = format!("big: &big {{{}}}\n", entries.join(", "));
    for i in 0..64 {
        text.push_str(&format!("m{i}: {{<<: *big}}\n"));
    }
    text
}

fn padded(total: usize) -> String {
    let mut text = "a: 1\n".to_owned();
    text.push_str(&"#".repeat(total - text.len()));
    text
}

#[test]
fn the_depth_bomb_is_refused_without_recursion() {
    // The review's document, on a thread with a small stack in the child as well as here.
    let bomb = format!("{}0{}", "[".repeat(20_000), "]".repeat(20_000));
    assert_eq!(bomb.len(), 40_001);
    refused_everywhere("depth bomb", bomb.as_bytes(), &YAML_SLOTS, "depth");
    refused_everywhere("depth bomb", bomb.as_bytes(), &JSON_SLOTS, "depth");
}

#[test]
fn the_alias_bomb_is_refused_without_expansion() {
    let review = review_alias();
    assert!(review.len() < 500, "{}", review.len());
    refused_everywhere(
        "review alias bomb",
        review.as_bytes(),
        &YAML_SLOTS,
        "expanded_nodes",
    );
    refused_everywhere(
        "2^30 document",
        doubling(30).as_bytes(),
        &YAML_SLOTS,
        "expanded_nodes",
    );
    // 70 levels would be 2^70 nodes; the count passes the budget long before it wraps.
    refused_everywhere(
        "2^70 document",
        doubling(70).as_bytes(),
        &YAML_SLOTS,
        "expanded_nodes",
    );
    refused_everywhere(
        "merge bomb",
        merge_bomb().as_bytes(),
        &YAML_SLOTS,
        "expanded_nodes",
    );
}

#[test]
fn each_yaml_budget_passes_at_its_limit_and_refuses_one_over() {
    // bytes
    accepted_in("1,048,576 bytes", padded(1_048_576).as_bytes(), CONFIG);
    let over = refused_everywhere(
        "1,048,577 bytes",
        padded(1_048_577).as_bytes(),
        &[CONFIG],
        "bytes",
    );
    assert_eq!(over[0]["limit"], 1_048_576);
    assert_eq!(over[0]["observed"], "1048577");
    // aliases
    accepted_in("64 aliases", aliases_of_one_scalar(64).as_bytes(), CONFIG);
    let over = refused_everywhere(
        "65 aliases",
        aliases_of_one_scalar(65).as_bytes(),
        &YAML_SLOTS,
        "aliases",
    );
    assert_eq!(over[0]["limit"], 64);
    // nodes as expanded
    // These fixtures pass the parser budget, then stop on config shape.
    let at = refused_everywhere(
        "200,000 nodes",
        flat_sequence(199_999).as_bytes(),
        &[CONFIG],
        "unreadable",
    );
    assert!(
        at.iter()
            .all(|f| f["detail"].as_str().unwrap().contains("expected a mapping"))
    );
    refused_everywhere(
        "200,001 nodes",
        flat_sequence(200_000).as_bytes(),
        &[CONFIG],
        "expanded_nodes",
    );
    // bytes as expanded
    let at = refused_everywhere(
        "4,194,304 bytes expanded",
        repeated_scalar(63, false).as_bytes(),
        &[CONFIG],
        "unreadable",
    );
    assert!(
        at.iter()
            .all(|f| f["detail"].as_str().unwrap().contains("expected a mapping"))
    );
    refused_everywhere(
        "4,194,305 bytes expanded",
        repeated_scalar(63, true).as_bytes(),
        &[CONFIG],
        "expanded_bytes",
    );
    // depth as expanded: 62 aliases is 64 deep as expanded, 63 is one more.
    accepted_in("expanded depth 64", alias_chain(62).as_bytes(), CONFIG);
    refused_everywhere(
        "expanded depth 65",
        alias_chain(63).as_bytes(),
        &YAML_SLOTS,
        "depth",
    );
}

#[test]
fn expanded_depth_counts_through_aliases() {
    // Two deep as written; refused only because of what the anchors expand to.
    let text = alias_chain(63);
    assert!(text.lines().all(|l| l.matches('[').count() == 1));
    refused_everywhere("chain of anchors", text.as_bytes(), &[CONFIG], "depth");
}

#[test]
fn a_merge_key_draws_on_the_same_budget_and_still_merges_inside_it() {
    let ok = b"defaults: &d {a: 1, b: 2}\none: {<<: *d, b: 3}\ntwo: {<<: *d}\n";
    accepted_in("merge keys", ok, CONFIG);
}

#[test]
fn an_alias_inside_its_own_anchor_is_refused() {
    refused_everywhere("cycle", b"a: &a [*a]\n", &YAML_SLOTS, "alias_cycle");
}

#[test]
fn an_explicit_tag_of_any_kind_is_refused() {
    for (what, text) in [
        ("custom", "id: !custom abc\n"),
        ("standard", "id: !!str abc\n"),
        ("binary", "data: !!binary YWJj\n"),
        ("non-specific", "id: ! abc\n"),
        ("on a sequence", "x: !!seq [a]\n"),
        ("on the root", "!!map {a: 1}\n"),
    ] {
        let found = refused_everywhere(what, text.as_bytes(), &YAML_SLOTS, "tag");
        assert!(
            found[0]["detail"]
                .as_str()
                .unwrap()
                .contains("stricter than the 1.0 reader"),
            "{what}: {:?}",
            found[0]
        );
    }
}

#[test]
fn a_second_document_is_refused() {
    let found = refused_everywhere(
        "two documents",
        b"a: 1\n---\nb: 2\n",
        &YAML_SLOTS,
        "documents",
    );
    assert_eq!(
        (&found[0]["limit"], &found[0]["observed"]),
        (&Value::from(1), &Value::from("2"))
    );
}

#[test]
fn a_stream_of_no_document_is_refused() {
    for (what, text) in [
        ("zero bytes", ""),
        ("spaces and newlines", " \n  \n"),
        ("comment only", "# synthetic config\n"),
    ] {
        for found in refused_everywhere(what, text.as_bytes(), &YAML_SLOTS, "documents") {
            assert_eq!(
                (&found["limit"], &found["observed"]),
                (&Value::from(1), &Value::from("0"))
            );
            assert!(
                found["detail"]
                    .as_str()
                    .unwrap()
                    .contains("expected one document, found 0"),
                "{what}: {found:?}"
            );
        }
    }
}

// ---- JSON

fn strings(n: usize) -> String {
    format!("[{}]", vec!["\"a\""; n].join(","))
}

fn nested(n: usize) -> String {
    format!("{}0{}", "[".repeat(n), "]".repeat(n))
}

fn spaced(total: usize) -> String {
    format!("0{}", " ".repeat(total - 1))
}

#[test]
fn each_json_budget_passes_at_its_limit_and_refuses_one_over() {
    // A bounded scalar array still stops on index entry shape after parsing.
    refused_everywhere(
        "999,999 strings",
        strings(999_999).as_bytes(),
        &[INDEX],
        "index_entry",
    );
    refused_everywhere(
        "1,000,000 strings",
        strings(1_000_000).as_bytes(),
        &[INDEX],
        "nodes",
    );
    refused_everywhere("64 deep", nested(64).as_bytes(), &[INDEX], "index_entry");
    accepted_in("64 deep", nested(64).as_bytes(), STATE);
    refused_everywhere("65 deep", nested(65).as_bytes(), &JSON_SLOTS, "depth");
    accepted_in("16 MiB", spaced(16_777_216).as_bytes(), INDEX);
    let over = refused_everywhere(
        "16 MiB and a byte",
        spaced(16_777_217).as_bytes(),
        &JSON_SLOTS,
        "bytes",
    );
    assert_eq!(over[0]["limit"], 16_777_216);
    // An index over a budget stops and is never read as an unreadable index (X-V12).
    assert_eq!(over.len(), JSON_SLOTS.len());
}

// ---- Event lines

fn event_of(len: usize) -> String {
    let head = r#"{"timestamp":"t","type":"done","data":""#;
    let tail = r#""}"#;
    format!("{head}{}{tail}", "x".repeat(len - head.len() - tail.len()))
}

#[test]
fn an_event_line_is_stored_at_its_bound_and_is_a_gap_one_byte_over() {
    let at = event_of(65_536);
    assert_eq!(at.len(), 65_536);
    let dir = source("runs/run_a.jsonl", format!("{at}\n"));
    let out = child(dir.path(), "event at the bound");
    out.accepted("event at the bound");
    assert_eq!(
        (out.counts("run_events"), out.counts("run_event_gaps")),
        (1, 0)
    );
    let dir = source("runs/run_a.jsonl", format!("{}\n", event_of(65_537)));
    let out = child(dir.path(), "event one byte over");
    out.accepted("event one byte over");
    assert_eq!(
        (out.counts("run_events"), out.counts("run_event_gaps")),
        (0, 1)
    );
}

#[test]
fn a_line_without_a_newline_is_not_kept() {
    // 256 MiB with no newline: a sparse file, so that only the reader's work is measured.
    let dir = source("runs/run_a.jsonl", "");
    let path = dir.path().join(".orchestry/runs/run_a.jsonl");
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(256 * 1024 * 1024).unwrap();
    let out = child(dir.path(), "256 MiB line");
    out.accepted("256 MiB line");
    assert_eq!(
        (out.counts("run_events"), out.counts("run_event_gaps")),
        (0, 1)
    );
}

#[test]
fn unusable_event_lines_become_one_gap() {
    let event = event_of(64);
    let long = "x".repeat(70_000);
    let deep = nested(65);
    let text = format!("{event}\n{long}\n\u{0}\u{7f}\n{deep}\nnot json\n{event}\n");
    let dir = source("runs/run_a.jsonl", text);
    let out = child(dir.path(), "three unusable lines");
    out.accepted("three unusable lines");
    assert_eq!(
        (out.counts("run_events"), out.counts("run_event_gaps")),
        (2, 1)
    );
}

#[test]
fn the_report_of_refusals_is_bounded_in_a_child() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join(".orchestry/tasks");
    fs::create_dir_all(&tasks).unwrap();
    for i in 0..2000 {
        fs::write(tasks.join(format!("tsk_{i}.yml")), "id: !custom abc\n").unwrap();
    }
    let out = child(dir.path(), "2,000 refused documents");
    assert_eq!(out.refusals().len(), 1000);
    assert_eq!(out.report["refusals"]["more"], 1000);
}
