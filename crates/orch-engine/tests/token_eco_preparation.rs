//! T1 / F4 — effort preparation, failure cleanup and the single production
//! caller (D5-9 dispatch side, D7, production-path audit).
//!
//! Dispatch awaits `Adapter::prepare_effort` before `execute`; every failure
//! goes through the existing run-failure path so no agent is stranded
//! `running`, no claim leaks and no model child is spawned.

mod token_eco_support;

use std::path::{Path, PathBuf};

use orch_domain::{
    AgentStatus, OrchestratorEvent, ReasoningEffort, RunEventType, RunStatus, TaskStatus,
};
use serde_json::json;
use token_eco_support::{Rig, RigSpec, claude_agent, config_with, failed_run, task};

async fn rig_for(effort: Option<ReasoningEffort>, mode: &str) -> Rig {
    let mut agent = claude_agent("agt_a", "Alpha");
    agent.config.effort = effort;
    agent.config.model = Some("claude-opus-5-5".into());
    Rig::new(RigSpec {
        raw_config: config_with(Some(mode), json!({})),
        agents: vec![agent],
        tasks: vec![task("tsk_1", Some("agt_a"))],
        skills: vec![],
        config_store: true,
    })
    .await
}

async fn assert_released(rig: &Rig, why: &str) {
    let state = rig.engine.state.read().await.unwrap();
    assert!(
        state.claimed.is_empty(),
        "{why}: claim leaked {:?}",
        state.claimed
    );
    assert!(
        state.running.is_empty(),
        "{why}: slot leaked {:?}",
        state.running
    );
    assert_eq!(
        rig.agent("agt_a").await.status,
        AgentStatus::Idle,
        "{why}: agent stranded"
    );
    assert_eq!(rig.agent("agt_a").await.current_task, None, "{why}");
}

async fn run_event_text(rig: &Rig, run_id: &str) -> Vec<String> {
    rig.engine
        .runs
        .read_events(run_id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.data.to_string())
        .collect()
}

// ── ordering and the happy path ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn preparation_is_awaited_exactly_once_before_each_execute_in_both_modes() {
    for mode in ["normal", "eco"] {
        let rig = rig_for(Some(ReasoningEffort::High), mode).await;
        rig.dispatch().await;
        assert_eq!(rig.adapter.order(), vec!["prepare", "execute"], "{mode}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_ignore_clears_only_the_invocation_effort_and_records_the_diagnostic() {
    let rig = rig_for(Some(ReasoningEffort::High), "normal").await;
    rig.adapter
        .prepare_diagnostics(&["legacy-ignored: opencode wire check failed"], true);

    rig.dispatch().await;

    let call = rig.adapter.for_task("tsk_1");
    assert_eq!(
        call.config.effort, None,
        "the invocation copy has the effort cleared"
    );
    assert_eq!(
        call.config.model.as_deref(),
        Some("claude-opus-5-5"),
        "model byte-for-byte"
    );
    assert_eq!(
        rig.agent("agt_a").await.config.effort,
        Some(ReasoningEffort::High),
        "the authored definition is never rewritten"
    );
    let run = &rig.runs().await[0];
    assert_eq!(run.status, RunStatus::Succeeded);
    // Ruling 8: the diagnostic is operator information on the pre-spawn
    // surface, never a run event, so it cannot reach retry `previous_output`
    // or `final_result`.
    let events = rig.engine.runs.read_events(&run.id).await.unwrap();
    assert!(
        events
            .iter()
            .all(|e| !e.data.to_string().contains("legacy-ignored")),
        "the diagnostic leaked into the run log: {events:?}"
    );
    let reported: Vec<(String, String)> = rig
        .events()
        .into_iter()
        .filter_map(|e| match e {
            OrchestratorEvent::OrchestratorError {
                error,
                context,
                fatal: false,
            } if context.starts_with("effort preparation") => Some((context, error)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reported,
        vec![(
            format!("effort preparation for run {} (agent agt_a)", run.id),
            "legacy-ignored: opencode wire check failed".to_owned()
        )],
        "exactly one pre-spawn diagnostic, verbatim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn with_no_diagnostics_the_run_log_has_only_model_events() {
    let rig = rig_for(None, "normal").await;
    rig.dispatch().await;
    let run = &rig.runs().await[0];
    let text = run_event_text(&rig, &run.id).await.join("\n");
    assert!(!text.contains("effort preparation"), "{text}");
}

// ── failure cleanup ─────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_preparation_error_fails_the_run_releases_everything_and_spawns_no_child() {
    for mode in ["normal", "eco"] {
        let rig = rig_for(Some(ReasoningEffort::Xhigh), mode).await;
        rig.adapter
            .fail_preparation("opencode wire check: unsupported major 3");

        let _ = rig.dispatch_result().await;

        assert_eq!(
            rig.adapter.order(),
            vec!["prepare"],
            "{mode}: execute must never run"
        );
        assert!(rig.adapter.calls().is_empty(), "{mode}: no model child");
        let runs = rig.runs().await;
        assert_eq!(runs.len(), 1, "{mode}");
        assert_eq!(runs[0].status, RunStatus::Failed, "{mode}");
        let error = runs[0].error.clone().unwrap_or_default();
        assert!(
            error.contains("effort preparation failed") && error.contains("unsupported major 3"),
            "{mode}: {error}"
        );
        assert_released(&rig, mode).await;
        let last = rig
            .agent("agt_a")
            .await
            .last_error
            .expect("last_error recorded");
        assert!(
            last.message.contains("effort preparation failed"),
            "{mode}: {last:?}"
        );
        // Normal retry policy: the task is not left in_progress.
        assert_ne!(
            rig.task("tsk_1").await.status,
            TaskStatus::InProgress,
            "{mode}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_error_after_preparation_is_cleaned_up_the_same_way() {
    let rig = rig_for(Some(ReasoningEffort::High), "normal").await;
    rig.adapter.fail_execute("spawn failed: ENOENT");

    let _ = rig.dispatch_result().await;

    assert_eq!(rig.adapter.order(), vec!["prepare", "execute"]);
    let runs = rig.runs().await;
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert_released(&rig, "execute error").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preparation_that_rewrites_the_model_is_refused_before_execute() {
    let rig = rig_for(Some(ReasoningEffort::High), "normal").await;
    rig.adapter.rewrite_model_in_preparation("some-other-model");

    let _ = rig.dispatch_result().await;

    assert!(
        rig.adapter.calls().is_empty(),
        "a contract violation never reaches execute"
    );
    let runs = rig.runs().await;
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert!(
        runs[0]
            .error
            .clone()
            .unwrap_or_default()
            .contains("changed more than the generic effort"),
        "{:?}",
        runs[0].error
    );
    assert_released(&rig, "contract violation").await;
    assert_eq!(
        rig.agent("agt_a").await.config.model.as_deref(),
        Some("claude-opus-5-5")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_preparation_leaves_the_task_retryable_and_the_next_dispatch_recovers() {
    let rig = rig_for(Some(ReasoningEffort::High), "normal").await;
    rig.adapter.fail_preparation("transient probe failure");
    let _ = rig.dispatch_result().await;
    let after_failure = rig.task("tsk_1").await;
    assert_ne!(after_failure.status, TaskStatus::InProgress);
    assert_eq!(
        after_failure.attempts, 1,
        "the failure consumed one attempt under the normal policy"
    );

    // The operator fixes the environment; the very next dispatch works.
    rig.adapter.fail_preparation_none();
    rig.engine.task_service.retry("tsk_1").await.ok();
    let mut t = rig.task("tsk_1").await;
    t.status = TaskStatus::Todo;
    rig.reseed_task(t).await;
    rig.dispatch().await;
    assert_eq!(
        rig.adapter.order().last().map(String::as_str),
        Some("execute")
    );
    assert_eq!(rig.adapter.calls().len(), 1);
}

// ── D7 engine side: no session, no resume, no shared history ─────────────

#[tokio::test(flavor = "multi_thread")]
async fn prior_runs_with_provider_thread_ids_never_leak_into_the_next_invocation() {
    let rig = rig_for(None, "eco").await;
    // A previous failed run of the same task whose non-output events carry
    // provider session identifiers.
    let mut prior = failed_run(
        "run_prev",
        "tsk_1",
        "agt_a",
        "2025-10-13T16:20:00.000Z",
        "E",
    );
    prior.attempt = 1;
    rig.engine.runs.save(&prior).await.unwrap();
    for (kind, data) in [
        (
            RunEventType::ToolCall,
            json!({"thread_id": "thr_SECRET_123", "session_id": "ses_SECRET_456"}),
        ),
        (
            RunEventType::CommandRun,
            json!({"resume": "--resume ses_SECRET_456"}),
        ),
        (RunEventType::AgentOutput, json!("visible output")),
    ] {
        rig.engine
            .runs
            .append_event(
                "run_prev",
                &orch_domain::RunEvent {
                    timestamp: "2025-10-13T16:00:00.000Z".into(),
                    event_type: kind,
                    data,
                },
            )
            .await
            .unwrap();
    }
    let mut t = task("tsk_1", Some("agt_a"));
    t.attempts = 1;
    rig.reseed_task(t).await;

    rig.dispatch().await;

    let call = rig.adapter.for_task("tsk_1");
    let everything = format!(
        "{}\n{:?}\n{:?}\n{:?}",
        call.all_text(),
        call.env,
        call.config,
        call.workspace
    );
    for forbidden in [
        "thr_SECRET_123",
        "ses_SECRET_456",
        "--resume",
        "--continue",
        "resume",
    ] {
        assert!(
            !everything.contains(forbidden),
            "`{forbidden}` leaked into: {everything}"
        );
    }
    let mut env_keys: Vec<&str> = call.env.keys().map(String::as_str).collect();
    env_keys.sort();
    assert_eq!(
        env_keys,
        ["ORCH_AGENT_ID", "ORCH_AGENT_NAME", "ORCH_TASK_ID"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unrelated_earlier_runs_output_is_not_in_the_next_tasks_prompt() {
    for mode in ["normal", "eco"] {
        let rig = Rig::new(RigSpec {
            raw_config: config_with(Some(mode), json!({})),
            agents: vec![
                claude_agent("agt_a", "Alpha"),
                claude_agent("agt_b", "Beta"),
            ],
            tasks: vec![task("tsk_old", Some("agt_a"))],
            skills: vec![],
            config_store: true,
        })
        .await;
        rig.adapter.push_script(vec![
            Ok(orch_core::test_support::output_event(
                "2025-10-13T16:37:33.426Z",
                "NONCE-7f3a91 remember this",
            )),
            Ok(orch_core::test_support::done_event(
                "2025-10-13T16:37:33.426Z",
            )),
        ]);
        rig.dispatch().await;
        rig.reseed_task(task("tsk_new_a", Some("agt_a"))).await;
        rig.reseed_task(task("tsk_new_b", Some("agt_b"))).await;
        rig.dispatch().await;

        for id in ["tsk_new_a", "tsk_new_b"] {
            let text = rig.adapter.for_task(id).all_text();
            assert!(
                !text.contains("NONCE-7f3a91"),
                "{mode}/{id}: previous conversation leaked"
            );
            assert!(text.contains(&format!("Title of {id}")), "{mode}/{id}");
        }
        // Concurrent unrelated tasks each see only their own task text.
        let a = rig.adapter.for_task("tsk_new_a").prompt;
        let b = rig.adapter.for_task("tsk_new_b").prompt;
        assert!(
            !a.contains("Title of tsk_new_b") && !b.contains("Title of tsk_new_a"),
            "{mode}"
        );
    }
}

// ── production-path audit ───────────────────────────────────────────────

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "tests" || n == "target")
            {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The part of a source file that is not `#[cfg(test)]` code: everything up to
/// the first `#[cfg(test)]` module attribute at column zero or a `mod tests`.
/// (Every file in this workspace keeps its tests in one trailing module.)
fn production_part(source: &str) -> &str {
    let mut cut = source.len();
    for marker in [
        "\n#[cfg(test)]\nmod ",
        "\n#[cfg(test)]\npub(crate) mod ",
        "\n#[cfg(test)]\n#[allow",
    ] {
        if let Some(at) = source.find(marker) {
            cut = cut.min(at);
        }
    }
    &source[..cut]
}

#[test]
fn dispatch_is_the_only_production_caller_of_adapter_execute_and_awaits_preparation_first() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(
        files.len() > 100,
        "scanner found only {} files",
        files.len()
    );

    let mut callers = Vec::new();
    for file in &files {
        // A whole file that is test support (e.g. core/test_support, tests/ modules).
        let rel = file
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("/test_support/") || rel.contains("/orchestrator/tests/") {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap();
        let prod = production_part(&text);
        for (n, line) in prod.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with("///") {
                continue;
            }
            if line.contains(".execute(") && !line.contains("sqlx") && !line.contains("query") {
                callers.push((rel.clone(), n + 1, line.trim().to_owned()));
            }
        }
    }

    assert_eq!(
        callers.len(),
        1,
        "exactly one production Adapter::execute call is allowed; found {callers:#?}"
    );
    let (rel, _line, text) = &callers[0];
    assert!(
        rel.ends_with("orch-engine/src/orchestrator/dispatch.rs"),
        "{rel}"
    );
    assert!(text.contains("adapter.execute("), "{text}");

    // The awaited hook, and the contract guard, sit before that call.
    let dispatch =
        std::fs::read_to_string(crates.join("orch-engine/src/orchestrator/dispatch.rs")).unwrap();
    let execute_at = dispatch
        .find("adapter.execute(params)")
        .expect("the one execute call");
    let prepare_at = dispatch
        .find(".prepare_effort(&params)")
        .expect("the hook call");
    assert!(
        prepare_at < execute_at,
        "prepare_effort must precede execute"
    );
    let between = &dispatch[prepare_at..execute_at];
    assert!(
        between.contains(".await"),
        "the hook result must be awaited"
    );
    assert!(
        between.contains("check_preparation"),
        "the contract guard sits between hook and spawn"
    );
    assert!(
        between.contains("params.config = preparation.config"),
        "the prepared config is what executes"
    );
}

#[test]
fn only_the_opencode_adapter_overrides_the_preparation_hook() {
    let infra = Path::new(env!("CARGO_MANIFEST_DIR")).join("../orch-infra/src/adapters");
    let mut overriders = Vec::new();
    for entry in std::fs::read_dir(&infra).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            if production_part(&text).contains("fn prepare_effort") {
                overriders.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    overriders.sort();
    assert_eq!(
        overriders,
        ["opencode.rs"],
        "ruling 7: only OpenCode overrides the identity hook"
    );
}
