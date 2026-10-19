//! Grok adapter tests.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager};
use orch_domain::{AdapterErrorKind, AgentConfig, ReasoningEffort};
use orch_infra::adapters::GrokAdapter;
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, with_system_prompt};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> GrokAdapter {
    GrokAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn run_params() -> ExecuteParams {
    params("grok prompt", "/tmp/grok-ws")
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_grok() {
    assert_eq!(adapter(&ScriptedProcessManager::silent()).kind(), "grok");
}

#[tokio::test]
async fn spawns_grok_with_the_headless_streaming_json_argv() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "grok");
    assert_eq!(
        record.args,
        [
            "-p",
            "grok prompt",
            "--output-format",
            "streaming-json",
            "--permission-mode",
            "bypassPermissions",
            "--always-approve",
            // Passed in addition to the spawn cwd.
            "--cwd",
            "/tmp/grok-ws",
        ]
    );
    assert_eq!(record.cwd, std::path::Path::new("/tmp/grok-ws"));
}

#[tokio::test]
async fn passes_the_model_effort_max_turns_and_system_prompt_override() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(with_system_prompt(
            support::with_config(
                run_params(),
                AgentConfig {
                    model: Some("grok-build".to_owned()),
                    effort: Some(ReasoningEffort::High),
                    max_turns: Some(7),
                    ..Default::default()
                },
            ),
            "system instructions",
        ))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.value_after("--model"), Some("grok-build"));
    assert_eq!(record.value_after("--effort"), Some("high"));
    assert_eq!(record.value_after("--max-turns"), Some("7"));
    assert_eq!(
        record.value_after("--system-prompt-override"),
        Some("system instructions")
    );
    assert!(!record.has_flag("--system-prompt"));
}

#[tokio::test]
async fn max_turns_zero_omits_the_flag() {
    // Grok passes `--max-turns` only when the value is truthy.
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            run_params(),
            AgentConfig {
                max_turns: Some(0),
                ..Default::default()
            },
        ))
        .expect("execute");
    assert!(!pm.spawn_record().has_flag("--max-turns"));
}

#[tokio::test]
async fn aggregates_text_deltas_and_emits_done_on_end() {
    let collected = run(Script::lines([
        json!({ "type": "thought", "data": "skip me" }).to_string(),
        json!({ "type": "text", "data": "ORCH" }).to_string(),
        json!({ "type": "text", "data": "_OK" }).to_string(),
        json!({ "type": "end", "stopReason": "EndTurn" }).to_string(),
    ]))
    .await;

    collected.expect_ok();
    assert_eq!(
        collected.kinds(),
        [AgentEventKind::Output, AgentEventKind::Done]
    );
    assert_eq!(collected.data()[0], &json!({ "text": "ORCH_OK" }));
    assert_eq!(
        collected.data()[1],
        &json!({ "result": "ORCH_OK", "raw": { "type": "end", "stopReason": "EndTurn" } })
    );
}

#[tokio::test]
async fn tool_and_error_events_pass_through() {
    let collected = run(Script::lines([
        json!({ "type": "tool_call", "name": "edit" }).to_string(),
        json!({ "type": "tool_use", "name": "write" }).to_string(),
        json!({ "type": "tool_result", "output": "ok" }).to_string(),
        json!({ "type": "error", "data": "bad" }).to_string(),
    ]))
    .await;

    assert_eq!(
        collected.kinds(),
        [
            AgentEventKind::ToolCall,
            AgentEventKind::ToolCall,
            AgentEventKind::Output,
            AgentEventKind::Error,
            // The clean exit with no `end` record still owes a terminal event.
            AgentEventKind::Done,
        ]
    );
    assert_eq!(
        collected.events[3].error_kind,
        Some(AdapterErrorKind::Unknown)
    );
}

#[tokio::test]
async fn an_unknown_event_type_becomes_output() {
    let collected = run(Script::lines([
        json!({ "type": "whatever", "a": 1 }).to_string()
    ]))
    .await;
    assert_eq!(collected.kinds()[0], AgentEventKind::Output);
}

#[tokio::test]
async fn an_unparseable_line_becomes_a_text_output() {
    let collected = run(Script::new().stdout("plain text\n")).await;
    assert_eq!(collected.data()[0], &json!({ "text": "plain text" }));
}

#[tokio::test]
async fn a_clean_exit_without_an_end_record_synthesises_done() {
    let collected = run(Script::lines([
        json!({ "type": "text", "data": "partial" }).to_string(),
    ]))
    .await;

    collected.expect_ok();
    assert_eq!(
        collected.kinds(),
        [AgentEventKind::Output, AgentEventKind::Done]
    );
    assert_eq!(collected.data()[0], &json!({ "text": "partial" }));
    assert_eq!(collected.data()[1], &json!({ "result": "partial" }));
}

#[tokio::test]
async fn text_is_flushed_in_chunks_once_it_passes_the_threshold() {
    // 240 UTF-16 units is the flush threshold; the flush happens on the next
    // line that produces no event.
    let long = "x".repeat(250);
    let collected = run(Script::lines([
        json!({ "type": "text", "data": long }).to_string(),
        json!({ "type": "thought", "data": "triggers the flush check" }).to_string(),
        json!({ "type": "end" }).to_string(),
    ]))
    .await;

    assert_eq!(
        collected.kinds(),
        [AgentEventKind::Output, AgentEventKind::Done]
    );
    assert_eq!(
        collected.data()[0]["text"].as_str().expect("text").len(),
        250
    );
    // The done event still reports the whole turn.
    assert_eq!(
        collected.data()[1]["result"]
            .as_str()
            .expect("result")
            .len(),
        250
    );
}

#[tokio::test]
async fn a_non_zero_exit_without_a_done_event_fails_the_run() {
    let collected = run(Script::new().exit(1)).await;
    assert_eq!(collected.error_message(), "Grok process exited with code 1");
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::ProcessCrash
    );
}

#[tokio::test]
async fn grok_never_appends_a_stderr_tail_to_its_error() {
    // Stderr is drained and discarded, so a `rate limit` in
    // stderr must not change the message or its classification.
    let collected = run(Script::new().stderr("rate limit exceeded\n").exit(1)).await;
    assert_eq!(collected.error_message(), "Grok process exited with code 1");
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::ProcessCrash
    );
}

#[tokio::test]
async fn the_pending_text_is_flushed_before_the_exit_error() {
    let collected =
        run(
            Script::lines([json!({ "type": "text", "data": "half a thought" }).to_string()])
                .exit(1),
        )
        .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.data()[0], &json!({ "text": "half a thought" }));
    assert_eq!(collected.error_message(), "Grok process exited with code 1");
}

#[tokio::test]
async fn the_version_probe_classifies_a_missing_binary() {
    let pm = ScriptedProcessManager::silent().without(&["grok"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some(
            "Grok CLI not found. Install and authenticate the grok CLI, then ensure `grok` is on PATH."
        )
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::SpawnFailed));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(7777).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(7777, DEFAULT_KILL_GRACE)]);
}
