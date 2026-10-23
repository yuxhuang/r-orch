//! review-b2 §F1 / decision-cp2 ruling 8 — effort diagnostics leave the
//! `AgentOutput` stream, driven through the **real adapters** and the real
//! dispatch path.
//!
//! Every adapter (a real one, over a fake process manager, so no AI CLI ever
//! runs) is dispatched by the real orchestrator on a Forward or an
//! IgnoreLegacy effort. The contract:
//!
//! * the run log (`runs/<id>.jsonl`) holds no event of diagnostic origin, so
//!   retry `previous_output` (which reads it) cannot carry one;
//! * the bus carries no `agent:output` of diagnostic origin, and the
//!   collector's `final_result` (`result_text` or the last agent message)
//!   cannot be a diagnostic;
//! * the diagnostic is reported on the pre-spawn surface, once per dispatch:
//!   a non-fatal `orchestrator:error` whose context starts
//!   `effort preparation`, whose text is the note verbatim.
//!
//! The oracle is the diagnostic vocabulary of design-v3 D5.2 (the five status
//! words), written out here, not read from the implementation.

mod token_eco_support;

use std::sync::Arc;

use orch_core::adapter::Adapter;
use orch_core::process::ProcessManager;
use orch_core::test_support::FakeProcessManager;
use orch_domain::{
    OrchestratorEvent, ReasoningEffort as E, Run, RunEventType, RunStatus, TaskStatus,
};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use serde_json::{Value, json};
use token_eco_support::{Rig, RigSpec, claude_agent, config_with, task};

const STATUSES: [&str; 5] = [
    "request-only",
    "clamp-possible",
    "local-override-possible",
    "capability-unknown",
    "legacy-ignored",
];

fn is_diagnostic_text(text: &str) -> bool {
    text.contains("effort preparation") || STATUSES.iter().any(|s| text.contains(&format!("{s}:")))
}

fn real_adapter(kind: &str, pm: Arc<dyn ProcessManager>) -> Arc<dyn Adapter> {
    match kind {
        "claude" => Arc::new(ClaudeAdapter::new(pm)),
        "codex" => Arc::new(CodexAdapter::new(pm)),
        "cursor" => Arc::new(CursorAdapter::new(pm)),
        "shell" => Arc::new(ShellAdapter::new(pm)),
        "opencode" => Arc::new(OpenCodeAdapter::new(pm)),
        "pi" => Arc::new(PiAdapter::new(pm)),
        "grok" => Arc::new(GrokAdapter::new(pm)),
        "antigravity" => Arc::new(AntigravityAdapter::new(pm)),
        other => panic!("unknown adapter {other}"),
    }
}

struct Case {
    kind: &'static str,
    model: Option<&'static str>,
    effort: E,
    /// A status word the surface must report for this case, when the policy
    /// table in design-v3 D5.1/D5.2 guarantees one (None: only the leak check
    /// applies).
    note: Option<&'static str>,
}

/// Forward and IgnoreLegacy outcomes across all eight adapters.
const CASES: [Case; 12] = [
    Case {
        kind: "claude",
        model: Some("claude-opus-5-5"),
        effort: E::High,
        note: Some("request-only"),
    },
    Case {
        kind: "claude",
        model: None,
        effort: E::High,
        note: Some("request-only"),
    },
    Case {
        kind: "codex",
        model: Some("gpt-6.1-sol"),
        effort: E::High,
        note: None,
    },
    Case {
        kind: "codex",
        model: None,
        effort: E::High,
        note: None,
    },
    Case {
        kind: "pi",
        model: Some("x/y"),
        effort: E::High,
        note: None,
    },
    Case {
        kind: "grok",
        model: Some("grok-build"),
        effort: E::High,
        note: None,
    },
    Case {
        kind: "antigravity",
        model: Some("gemini-4-argon"),
        effort: E::High,
        note: None,
    },
    Case {
        kind: "antigravity",
        model: Some("gpt-oss-120b"),
        effort: E::High,
        note: Some("legacy-ignored"),
    },
    Case {
        kind: "cursor",
        model: Some("auto"),
        effort: E::High,
        note: None,
    },
    Case {
        kind: "shell",
        model: None,
        effort: E::High,
        note: Some("legacy-ignored"),
    },
    Case {
        kind: "opencode",
        model: None,
        effort: E::High,
        note: Some("legacy-ignored"),
    },
    Case {
        kind: "opencode",
        model: Some("openrouter/other/unlisted-model"),
        effort: E::High,
        note: Some("legacy-ignored"),
    },
];

async fn rig_for(
    kind: &str,
    model: Option<&str>,
    effort: Option<E>,
    pm: FakeProcessManager,
) -> Rig {
    let mut agent = claude_agent("agt_a", "Alpha");
    agent.adapter = kind.to_owned();
    agent.config.model = model.map(str::to_owned);
    agent.config.effort = effort;
    agent.config.command = Some("true".to_owned());
    let adapter = real_adapter(kind, Arc::new(pm));
    Rig::new_with_adapter(
        RigSpec {
            raw_config: config_with(Some("normal"), json!({})),
            agents: vec![agent],
            tasks: vec![task("tsk_1", Some("agt_a"))],
            skills: vec![],
            config_store: true,
        },
        adapter,
    )
    .await
}

/// `(context, error)` of every effort diagnostic on the bus.
fn reported(rig: &Rig) -> Vec<(String, String)> {
    rig.events()
        .into_iter()
        .filter_map(|e| match e {
            OrchestratorEvent::OrchestratorError {
                error,
                context,
                fatal: false,
            } if context.starts_with("effort preparation") => Some((context, error)),
            _ => None,
        })
        .collect()
}

/// Every `agent:output` text seen on the bus.
fn bus_outputs(rig: &Rig) -> Vec<String> {
    rig.events()
        .into_iter()
        .filter_map(|e| match e {
            OrchestratorEvent::AgentOutput { data, .. } => Some(data),
            _ => None,
        })
        .collect()
}

async fn the_run(rig: &Rig) -> Run {
    let runs = rig.runs().await;
    assert_eq!(runs.len(), 1, "exactly one dispatch: {runs:?}");
    runs.into_iter().next().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn no_adapter_forward_or_ignore_legacy_path_puts_a_diagnostic_in_the_run_log_or_on_agent_output()
 {
    for case in &CASES {
        let label = format!("{} {:?} {}", case.kind, case.model, case.effort);
        let rig = rig_for(
            case.kind,
            case.model,
            Some(case.effort),
            FakeProcessManager::new(),
        )
        .await;
        rig.dispatch().await;
        let run = the_run(&rig).await;

        let events = rig.engine.runs.read_events(&run.id).await.unwrap();
        let leaked: Vec<_> = events
            .iter()
            .filter(|e| is_diagnostic_text(&e.data.to_string()))
            .collect();
        assert!(
            leaked.is_empty(),
            "{label}: diagnostics in the run log: {leaked:?}"
        );

        let leaked: Vec<_> = bus_outputs(&rig)
            .into_iter()
            .filter(|t| is_diagnostic_text(t))
            .collect();
        assert!(
            leaked.is_empty(),
            "{label}: diagnostics on agent:output: {leaked:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_guaranteed_note_is_reported_exactly_once_on_the_pre_spawn_surface() {
    // The other half of the move: a diagnostic that vanished from every
    // surface would pass the leak sweep above.
    for case in CASES.iter().filter(|c| c.note.is_some()) {
        let label = format!("{} {:?} {}", case.kind, case.model, case.effort);
        let rig = rig_for(
            case.kind,
            case.model,
            Some(case.effort),
            FakeProcessManager::new(),
        )
        .await;
        rig.dispatch().await;
        let run = the_run(&rig).await;

        let wanted = format!("{}:", case.note.unwrap());
        let hits: Vec<_> = reported(&rig)
            .into_iter()
            .filter(|(_, error)| error.starts_with(&wanted))
            .collect();
        assert_eq!(hits.len(), 1, "{label}: {hits:?}");
        assert_eq!(
            hits[0].0,
            format!("effort preparation for run {} (agent agt_a)", run.id),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn opencode_legacy_ignored_is_reported_exactly_once_and_never_as_output() {
    // Without a model OpenCode resolves statically to IgnoreLegacy: the real
    // preparation yields the note, and `execute` (real adapter, fake process)
    // must not repeat it (review-b2 F2).
    let rig = rig_for("opencode", None, Some(E::High), FakeProcessManager::new()).await;
    rig.dispatch().await;

    let notes: Vec<String> = reported(&rig).into_iter().map(|(_, e)| e).collect();
    let ignored: Vec<_> = notes
        .iter()
        .filter(|n| n.starts_with("legacy-ignored"))
        .collect();
    assert_eq!(ignored.len(), 1, "diagnostics surface: {notes:?}");
    assert_eq!(notes.len(), 1, "no other diagnostic either: {notes:?}");

    let run = the_run(&rig).await;
    let events = rig.engine.runs.read_events(&run.id).await.unwrap();
    assert!(
        events
            .iter()
            .filter(|e| e.event_type == RunEventType::AgentOutput)
            .all(|e| !e.data.to_string().contains("legacy-ignored")),
        "legacy-ignored reached an AgentOutput run event: {events:?}"
    );
}

/// A claude child that fails fast: one assistant message, then exit 1.
fn failing_claude() -> FakeProcessManager {
    FakeProcessManager::new()
        .with_stdout_lines([
            json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ORCH_FAKE_OK step one"}]}}).to_string(),
        ])
        .with_stderr("boom: it failed\n")
        .with_exit(orch_core::process::ExitOutcome { code: Some(1), signal: None })
}

async fn failed_context(effort: Option<E>) -> (String, String) {
    let rig = rig_for(
        "claude",
        Some("claude-sonnet-4-6"),
        effort,
        failing_claude(),
    )
    .await;
    rig.dispatch().await;
    let run = the_run(&rig).await;
    assert_eq!(run.status, RunStatus::Failed, "the scenario must fail");
    let context = rig
        .engine
        .run_service
        .get_last_failed_run_context("tsk_1")
        .await
        .unwrap()
        .expect("a failed run context");
    (context.error, context.output)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_high_claude_fast_failure_retries_with_the_same_previous_output_as_no_effort() {
    let (error_none, output_none) = failed_context(None).await;
    let (error_high, output_high) = failed_context(Some(E::High)).await;
    assert_eq!(
        output_high, output_none,
        "previous_output changed with a legacy effort"
    );
    assert_eq!(
        error_high, error_none,
        "previous_error changed with a legacy effort"
    );
    assert!(
        output_none.contains("ORCH_FAKE_OK"),
        "the oracle must carry the agent's own output, else the comparison is vacuous: {output_none:?}"
    );
}

async fn silent_result(effort: Option<E>) -> (TaskStatus, Option<String>) {
    let rig = rig_for(
        "claude",
        Some("claude-sonnet-4-6"),
        effort,
        FakeProcessManager::new(),
    )
    .await;
    rig.dispatch().await;
    let task = rig.task("tsk_1").await;
    let summary: Option<Value> = serde_json::to_value(&task)
        .unwrap()
        .pointer("/proof/agent_summary")
        .cloned();
    (
        task.status,
        summary.and_then(|v| v.as_str().map(str::to_owned)),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_high_claude_run_that_says_nothing_has_the_same_result_as_no_effort() {
    let none = silent_result(None).await;
    let high = silent_result(Some(E::High)).await;
    assert_eq!(high, none, "final_result changed with a legacy effort");
    assert!(
        high.1.as_deref().is_none_or(|s| !is_diagnostic_text(s)),
        "a diagnostic became the task result: {high:?}"
    );
}
