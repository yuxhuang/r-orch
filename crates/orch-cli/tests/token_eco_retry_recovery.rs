//! D3-9 / D3-10 — retry-evidence recovery through the *real* `orch` binary.
//!
//! A failed run's stored event log is the only place the decisive clue lives
//! once eco mode clips the previous output. These tests prove, end to end and
//! with no real AI CLI, that:
//!
//! * `orch --json logs FAILED_RUN_ID` returns exactly the last 50 stored
//!   events, in stored order, whatever the file size or chunk boundaries;
//! * applying the documented extraction rules to *that CLI output* reproduces,
//!   byte for byte, the previous output the normal-mode prompt carries;
//! * the eco prompt names the same failed run the evidence came from (not a
//!   later or earlier one) and requires the fetch before acting;
//! * the task / list JSON views deliberately do not carry the clue.
//!
//! The oracle (`expected_previous_output`) is written from design-v3 D3, not
//! taken from the implementation.

mod token_eco_support;

use std::fs;

use serde_json::{Value, json};
use token_eco_support::{Reader, Sandbox, new_reader};

const CLUE: &str = "DECISIVE-CLUE-7f3a";
const EARLY_CLUE: &str = "EARLY-OUTSIDE-WINDOW-91bc";

/// Stored events, oldest first. 60 events, ~190 KiB: more than one 64 KiB read
/// chunk, so the tail reader crosses chunk boundaries with multi-byte text in
/// the window.
fn stored_events() -> Vec<Value> {
    let mut events = Vec::new();
    for i in 0..60usize {
        let ts = format!("2026-10-23T01:00:{i:02}.000Z");
        let (kind, data): (&str, Value) = match i {
            // Outside the 50-event window.
            3 => ("agent_output", json!(format!("{EARLY_CLUE} early line"))),
            // Inside the window, ~35 events from the tail, clue after char 80.
            25 => ("agent_output", json!(format!("{}{CLUE}", "x".repeat(100)))),
            // Non-string payloads are compact JSON in the previous output.
            30 => (
                "agent_output",
                json!({"tool": "read", "path": "a/b.rs", "n": 3}),
            ),
            33 => ("error", json!({"code": 17, "retryable": false})),
            34 => ("agent_output", json!(7)),
            35 => ("agent_output", json!(["a", 1, null, true])),
            36 => ("agent_output", Value::Null),
            // Types that never enter the previous output.
            40 => ("tool_call", json!("tool_call payload must not appear")),
            41 => (
                "file_changed",
                json!("file_changed payload must not appear"),
            ),
            50 => ("done", json!("done payload must not appear")),
            // Multi-byte text straddling chunk boundaries somewhere in the file.
            45 => ("agent_output", json!("héllo — 日本語 😀 end")),
            46 => ("error", json!("two\nlines and trailing space ")),
            // Bulk: 4 KiB of text per event so the file spans several chunks.
            _ => (
                "agent_output",
                json!(format!("bulk {i} {}", "é日😀".repeat(500))),
            ),
        };
        events.push(json!({"timestamp": ts, "type": kind, "data": data}));
    }
    events
}

/// design-v3 D3: from the last 50 stored events keep `agent_output` and
/// `error`; string data literal, anything else compact JSON; join with "\n".
fn expected_previous_output(window: &[Value]) -> String {
    window
        .iter()
        .filter(|e| matches!(e["type"].as_str(), Some("agent_output" | "error")))
        .map(|e| match &e["data"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_events(sb: &Sandbox, run_id: &str, events: &[Value]) {
    let body: String = events.iter().map(|e| format!("{e}\n")).collect();
    fs::write(sb.root().join("runs").join(format!("{run_id}.jsonl")), body).unwrap();
}

struct Scenario {
    sb: Sandbox,
    task: String,
    /// Run ids of the failed runs, oldest first.
    failed: Vec<String>,
    retry_agent: String,
}

fn runs_of(sb: &Sandbox, reader: &Reader, task: &str) -> Vec<Value> {
    let out = sb.run(reader, &["--json", "logs", "--task", task], None);
    assert!(out.ok(), "logs --task failed: {}", out.all());
    let mut runs: Vec<Value> = serde_json::from_str(&out.stdout).expect("runs json");
    runs.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
    runs
}

/// `failures` real failing runs of one task, each given crafted events by
/// `events_for(index)`, then the task is reassigned to a recording codex agent
/// ready for its retry.
fn scenario(mode: &str, failures: usize, events_for: impl Fn(usize) -> Vec<Value>) -> Scenario {
    let new = new_reader();
    let sb = Sandbox::new(&new);
    if mode != "normal" {
        let out = sb.run(
            &new,
            &["config", "set", "defaults.agent.prompt_mode", mode],
            None,
        );
        assert!(out.ok(), "set mode: {}", out.all());
    }
    let failing = sb.seed_agent(&new, "Failing", "shell", None, None);
    let retry_agent = sb.seed_agent(&new, "Retrier", "codex", Some("gpt-6.1-sol"), None);
    let task = sb.run(
        &new,
        &[
            "task",
            "add",
            "flaky job",
            "--assignee",
            &failing,
            "--quiet",
        ],
        None,
    );
    assert!(task.ok(), "{}", task.all());
    let task = task.stdout.trim().to_owned();
    let mut failed = Vec::new();
    for n in 0..failures {
        // The shell command is the agent's `command`: make it fail.
        sb.run(
            &new,
            &["agent", "patch", &failing, "-f", "-"],
            Some(r#"{"config":{"command":"echo placeholder; exit 3"}}"#),
        );
        let run = sb.run(&new, &["run", &task], None);
        assert!(
            !run.stdout.is_empty() || !run.stderr.is_empty(),
            "run printed nothing"
        );
        let runs = runs_of(&sb, &new, &task);
        let run_id = runs.last().unwrap()["id"].as_str().unwrap().to_owned();
        assert_eq!(
            runs.last().unwrap()["status"],
            "failed",
            "run did not fail: {}",
            run.all()
        );
        write_events(&sb, &run_id, &events_for(n));
        failed.push(run_id);
    }
    let assign = sb.run(&new, &["task", "assign", &task, &retry_agent], None);
    assert!(assign.ok(), "assign: {}", assign.all());
    Scenario {
        sb,
        task,
        failed,
        retry_agent,
    }
}

fn dispatch_retry(s: &Scenario) -> String {
    let new = new_reader();
    let out = s.sb.run(&new, &["run", &s.task], None);
    assert!(out.ok(), "retry dispatch failed: {}", out.all());
    s.sb.recorded_stdin("codex", 0)
        .expect("the retry agent received a prompt")
}

/// The first `**Error:**` section of the prompt, the output fence that follows
/// and nothing else: what the agent actually sees as "previous output".
fn normal_previous_output(prompt: &str) -> String {
    let after = prompt
        .split("**Last output:**\n```\n")
        .nth(1)
        .expect("normal prompt has a Last output fence");
    let (output, _) = after.split_once("\n```\n").expect("fence closes");
    output.to_owned()
}

#[test]
fn logs_by_run_id_returns_exactly_the_last_fifty_stored_events_in_order() {
    let events = stored_events();
    let s = scenario("normal", 1, |_| events.clone());
    let out =
        s.sb.run(&new_reader(), &["--json", "logs", &s.failed[0]], None);
    assert!(out.ok(), "{}", out.all());
    let got: Vec<Value> = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        got,
        events[10..].to_vec(),
        "the window is not the last 50 stored events, in stored order"
    );
}

#[test]
fn the_documented_extraction_of_the_cli_output_equals_the_normal_mode_previous_output_byte_for_byte()
 {
    let events = stored_events();
    let s = scenario("normal", 1, |_| events.clone());
    let cli: Vec<Value> = serde_json::from_str(
        &s.sb
            .run(&new_reader(), &["--json", "logs", &s.failed[0]], None)
            .stdout,
    )
    .unwrap();
    let from_cli = expected_previous_output(&cli);

    let prompt = dispatch_retry(&s);
    let in_prompt = normal_previous_output(&prompt);
    assert_eq!(
        in_prompt, from_cli,
        "extraction from `orch --json logs` differs from the normal retry output"
    );
    assert_eq!(
        in_prompt,
        expected_previous_output(&events[10..]),
        "normal retry output differs from the independent oracle"
    );
    assert!(
        in_prompt.contains(CLUE),
        "the decisive clue must be in the normal previous output"
    );
    assert!(
        !in_prompt.contains(EARLY_CLUE),
        "an event outside the 50-event window leaked into the previous output"
    );
    for excluded in ["tool_call payload", "file_changed payload", "done payload"] {
        assert!(
            !in_prompt.contains(excluded),
            "{excluded} must not enter the previous output"
        );
    }
}

#[test]
fn eco_mode_names_the_failed_run_clips_the_preview_and_requires_the_logs_fetch() {
    let events = stored_events();
    let s = scenario("eco", 1, |_| events.clone());
    let full = expected_previous_output(&events[10..]);
    assert!(
        full.len() > 8192,
        "fixture must exceed the 8,192-byte bound"
    );
    let prompt = dispatch_retry(&s);
    let run_id = &s.failed[0];
    assert!(
        prompt.contains(&format!("orch --json logs {run_id}")),
        "eco prompt must name the logs command for the failed run"
    );
    assert!(
        prompt.contains(&format!("failed run `{run_id}`")),
        "preview heading must name the source run"
    );
    assert!(
        prompt.contains("Required before any retry diagnosis, plan or side effect"),
        "fetch must be mandatory before acting"
    );
    assert!(
        prompt.contains("bytes omitted from the middle"),
        "the clip must be disclosed"
    );
    assert!(
        full.find(CLUE)
            .is_some_and(|at| at > 2048 && at < full.len() - 8192),
        "fixture clue must sit in the omitted middle"
    );
    assert!(
        !prompt.contains(CLUE),
        "the decisive clue lies in the clipped middle and must not be in the preview"
    );
    let head: String = full.chars().take(200).collect();
    let tail: String = full
        .chars()
        .rev()
        .take(200)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert!(
        prompt.contains(&head),
        "the preview must start with the output's head"
    );
    assert!(
        prompt.contains(&tail),
        "the preview must end with the output's tail"
    );
    assert!(
        prompt.contains("**Error:** "),
        "the terminal error is carried in full separately"
    );
    // Whatever the preview omitted, the CLI window still carries it.
    let cli = s.sb.run(&new_reader(), &["--json", "logs", run_id], None);
    assert!(cli.stdout.contains(CLUE));
}

#[test]
fn eco_mode_names_the_most_recent_failed_run_not_an_earlier_one() {
    let s = scenario("eco", 2, |n| {
        let mut events = stored_events();
        // Make the two runs distinguishable by content as well as id.
        events[59]["data"] = json!(format!("final line of failure #{n}"));
        events
    });
    assert_eq!(s.failed.len(), 2);
    let prompt = dispatch_retry(&s);
    assert!(
        prompt.contains(&format!("orch --json logs {}", s.failed[1])),
        "must name the latest failed run"
    );
    assert!(
        !prompt.contains(&s.failed[0]),
        "the earlier failed run's id must not appear"
    );
    assert!(prompt.contains("final line of failure #1"));
    assert!(!prompt.contains("final line of failure #0"));
}

#[test]
fn a_failed_run_with_missing_events_is_visible_to_the_agent_and_to_the_logs_command() {
    let s = scenario("eco", 1, |_| stored_events());
    fs::remove_file(
        s.sb.root()
            .join("runs")
            .join(format!("{}.jsonl", s.failed[0])),
    )
    .unwrap();
    let logs =
        s.sb.run(&new_reader(), &["--json", "logs", &s.failed[0]], None);
    let window: Value = serde_json::from_str(&logs.stdout).unwrap_or(Value::Null);
    assert!(
        !logs.ok() || window == json!([]),
        "missing events must be an error or an empty window, never invented data: {}",
        logs.all()
    );
    let prompt = dispatch_retry(&s);
    assert!(
        prompt.contains("Previous attempt failed"),
        "the retry section must still exist"
    );
    assert!(!prompt.contains(CLUE));
}

#[test]
fn task_and_list_json_views_do_not_carry_the_decisive_clue() {
    let s = scenario("normal", 1, |_| stored_events());
    let new = new_reader();
    for args in [
        vec!["--json", "task", "show", &s.task],
        vec!["--json", "task", "list"],
    ] {
        let out = s.sb.run(&new, &args, None);
        assert!(out.ok(), "{args:?}: {}", out.all());
        assert!(
            !out.all().contains(CLUE),
            "{args:?} must not be a substitute for the run events"
        );
    }
    // Keep `retry_agent` live so the scenario is fully exercised.
    assert!(!s.retry_agent.is_empty());
}

/// The tail reader once re-read overlapping bytes when the final (oldest)
/// chunk was short, returning duplicated and missing events for particular
/// file sizes. Sweep file sizes around every 64 KiB multiple, with and
/// without a trailing newline, and require exact ordered equality each time.
#[test]
fn logs_window_is_exact_for_file_sizes_across_chunk_boundaries_with_and_without_trailing_newline() {
    let new = new_reader();
    let s = scenario("normal", 1, |_| stored_events());
    let run_id = &s.failed[0];
    let path = s.sb.root().join("runs").join(format!("{run_id}.jsonl"));
    let mut checked = 0;
    // 60 events; vary the per-event payload so sizes land on both sides of
    // 64 KiB, 128 KiB and 192 KiB.
    for payload in (200..5000).step_by(331) {
        for trailing_newline in [true, false] {
            let events: Vec<Value> = (0..60)
                .map(|i| {
                    json!({"timestamp": format!("2026-10-23T02:00:{i:02}.000Z"), "type": "agent_output",
                           "data": format!("event-{i:03}-{}", "é".repeat(payload / 2))})
                })
                .collect();
            let mut body: String = events.iter().map(|e| format!("{e}\n")).collect();
            if !trailing_newline {
                body.pop();
            }
            fs::write(&path, &body).unwrap();
            let out = s.sb.run(&new, &["--json", "logs", run_id], None);
            assert!(out.ok(), "{}", out.all());
            let got: Vec<Value> = serde_json::from_str(&out.stdout).unwrap();
            assert_eq!(
                got,
                events[10..].to_vec(),
                "window differs at payload {payload} bytes/event, file {} bytes, trailing newline {trailing_newline}",
                body.len()
            );
            checked += 1;
        }
    }
    assert!(checked >= 24, "swept only {checked} sizes");
}
