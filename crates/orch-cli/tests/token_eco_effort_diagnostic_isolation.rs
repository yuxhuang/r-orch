//! review-b2 §F1 (condition 1) — effort diagnostics must never be mistaken for
//! the agent's own output.
//!
//! Contract (review-b2.md §F1, design-v3 D2-1 / D5.2): a **normal-mode** agent
//! that carries a *legacy* `effort: high` on an adapter whose argv is
//! byte-identical to the `d8c2319` baseline (Claude) must see no change in
//! * the retry prompt after a fast failure (`retry.previous_output`), and
//! * the task result (`proof.agent_summary`) of a run that ends without an
//!   agent message,
//!
//! compared with the same agent carrying no effort at all. Effort diagnostics
//! (`request-only: …`, `legacy-ignored: …`, `effort preparation: …`) are
//! operator information, "not provider output", so they must not feed either
//! surface. Both runs go through the real `orch` binary and the repository's
//! fake `claude` CLI; when `ORCH_T1_OLD_BIN` names the `d8c2319` binary the
//! same scenario is also run on it and the results must be byte-identical.
//!
//! The oracle is the no-effort run of the same binary (and the baseline
//! binary), never the implementation's own bookkeeping.
//!
//! Verified both ways (tests-t1.md): with the pre-fix adapters (`784169c`,
//! commit `72125b1` reverted) the three contract tests FAIL; at `72125b1` they
//! pass, including byte-identity with `d8c2319`.

mod token_eco_support;

use serde_json::Value;
use token_eco_support::{Reader, Sandbox, new_reader, old_reader};

/// What a scenario leaves behind for comparison.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    /// The `**Last output:**` block of the retry prompt.
    previous_output: String,
    /// `proof.agent_summary` of the task after a silent successful run.
    silent_summary: Option<String>,
}

/// The `**Last output:**` fence of a normal-mode retry prompt: exactly what
/// the agent is shown as "previous output".
fn previous_output(prompt: &str) -> String {
    let after = prompt
        .split("**Last output:**\n```\n")
        .nth(1)
        .unwrap_or_else(|| panic!("retry prompt has no Last output fence:\n{prompt}"));
    let (output, _) = after.split_once("\n```\n").expect("fence closes");
    output.to_owned()
}

fn task_json(sb: &Sandbox, reader: &Reader, task: &str) -> Value {
    let out = sb.run(reader, &["--json", "task", "show", task], None);
    assert!(out.ok(), "task show failed: {}", out.all());
    serde_json::from_str(&out.stdout).expect("task json")
}

fn add_task(sb: &Sandbox, reader: &Reader, title: &str, assignee: &str) -> String {
    let out = sb.run(
        reader,
        &["task", "add", title, "--assignee", assignee, "--quiet"],
        None,
    );
    assert!(out.ok(), "task add: {}", out.all());
    out.stdout.trim().to_owned()
}

/// Run the whole scenario on `reader` with a Claude agent carrying `effort`.
/// The init, seeding and dispatch all use the same binary so each reader is
/// judged on its own store.
fn observe(reader: &Reader, effort: Option<&str>) -> Observed {
    let sb = Sandbox::new(reader);
    // The model is the same literal in every run, so the only variable is
    // the stored effort.
    let first = sb.seed_agent(reader, "First", "claude", Some("claude-sonnet-4-6"), effort);
    let second = sb.seed_agent(
        reader,
        "Second",
        "claude",
        Some("claude-sonnet-4-6"),
        effort,
    );

    // 1. A fast failure: the fake claude prints one assistant message, then
    //    fails on stderr (the marker in the title selects that scenario).
    let task = add_task(&sb, reader, "flaky job ORCH_FAKE_FAIL", &first);
    sb.run(reader, &["run", &task], None);
    let runs = sb.run(reader, &["--json", "logs", "--task", &task], None);
    assert!(runs.ok(), "logs --task: {}", runs.all());
    let runs: Vec<Value> = serde_json::from_str(&runs.stdout).expect("runs json");
    assert_eq!(runs.len(), 1, "exactly one attempt so far: {runs:?}");
    assert_eq!(runs[0]["status"], "failed", "the first attempt must fail");

    // 2. The retry goes to the second agent; the fake records the prompt it
    //    receives on stdin.
    let assign = sb.run(reader, &["task", "assign", &task, &second], None);
    assert!(assign.ok(), "assign: {}", assign.all());
    sb.run(reader, &["run", &task], None);
    let prompt = sb
        .recorded_stdin("claude", 1)
        .expect("the retry attempt reached the fake claude");
    let previous_output = previous_output(&prompt);

    // 3. A run that ends without any agent message or `done.result`.
    let quiet = add_task(&sb, reader, "quiet job ORCH_FAKE_SILENT", &first);
    let run = sb.run(reader, &["run", &quiet], None);
    assert!(run.ok(), "silent run failed: {}", run.all());
    let silent_summary = task_json(&sb, reader, &quiet)["proof"]["agent_summary"]
        .as_str()
        .map(str::to_owned);

    Observed {
        previous_output,
        silent_summary,
    }
}

#[test]
fn legacy_effort_adds_no_diagnostic_text_to_the_normal_retry_previous_output() {
    let reader = new_reader();
    let without = observe(&reader, None);
    let with = observe(&reader, Some("high"));
    assert_eq!(
        with.previous_output, without.previous_output,
        "a legacy effort changed the retry previous_output"
    );
}

#[test]
fn legacy_effort_does_not_become_the_result_of_a_run_that_said_nothing() {
    let reader = new_reader();
    let without = observe(&reader, None);
    let with = observe(&reader, Some("high"));
    assert_eq!(
        with.silent_summary, without.silent_summary,
        "a legacy effort diagnostic became the task result"
    );
}

#[test]
fn legacy_effort_scenario_is_byte_identical_to_the_baseline_binary() {
    let Some(old) = old_reader() else {
        return;
    };
    let baseline = observe(&old, Some("high"));
    let current = observe(&new_reader(), Some("high"));
    assert_eq!(
        current, baseline,
        "retry prompt / result differ from d8c2319 for identical stored inputs"
    );
}

#[test]
fn the_scenario_itself_discriminates_the_failure_output_from_the_silent_run() {
    // Guards the harness, not the product: the no-effort retry carries the
    // fake agent's own failure output and the silent run has no summary, so
    // the comparisons above cannot pass vacuously on two empty values.
    let seen = observe(&new_reader(), None);
    assert!(
        seen.previous_output.contains("ORCH_FAKE_OK"),
        "the previous output must carry the failed attempt's own message: {:?}",
        seen.previous_output
    );
    assert_eq!(
        seen.silent_summary, None,
        "a silent run without effort has no result"
    );
}
