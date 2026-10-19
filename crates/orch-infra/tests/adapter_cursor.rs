//! Cursor adapter tests.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, StdioMode};
use orch_domain::{AdapterErrorKind, AgentConfig, TokenUsage};
use orch_infra::adapters::CursorAdapter;
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, with_system_prompt};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> CursorAdapter {
    CursorAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn run_params() -> ExecuteParams {
    params("cursor prompt", "/tmp/cursor-ws")
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_cursor() {
    assert_eq!(adapter(&ScriptedProcessManager::silent()).kind(), "cursor");
}

#[tokio::test]
async fn spawns_cursor_agent_with_the_contract_argv() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "cursor-agent");
    assert_eq!(
        record.args,
        [
            "-p",
            "--output-format",
            "stream-json",
            "--workspace",
            "/tmp/cursor-ws",
            "--yolo",
            "--trust",
            "cursor prompt",
        ]
    );
    assert_eq!(record.cwd, std::path::Path::new("/tmp/cursor-ws"));
    // Cursor cannot read the prompt from stdin, so stdin stays ignored.
    assert_eq!(record.stdin, StdioMode::Ignore);
}

#[tokio::test]
async fn the_system_prompt_is_prepended_to_the_positional_prompt() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(with_system_prompt(
            params("user task", "/tmp/cursor-ws"),
            "system instructions",
        ))
        .expect("execute");
    assert_eq!(
        pm.spawn_record().last_arg(),
        "system instructions\n\nuser task"
    );
}

#[tokio::test]
async fn the_agent_system_prompt_is_ignored() {
    // Unlike claude, cursor reads only `params.systemPrompt`.
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            params("just the task", "/tmp/cursor-ws"),
            AgentConfig {
                system_prompt: Some("agent prompt".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");
    assert_eq!(pm.spawn_record().last_arg(), "just the task");
}

#[tokio::test]
async fn the_model_comes_before_the_positional_prompt() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            run_params(),
            AgentConfig {
                model: Some("gpt-4o".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.value_after("--model"), Some("gpt-4o"));
    assert_eq!(record.last_arg(), "cursor prompt");
}

#[tokio::test]
async fn an_assistant_event_becomes_output() {
    let collected = run(Script::lines([
        json!({ "type": "assistant", "message": "hi" }).to_string(),
    ]))
    .await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!("hi"));
}

#[tokio::test]
async fn a_tool_use_event_becomes_a_tool_call() {
    let collected = run(Script::lines([
        json!({ "type": "tool_use", "name": "edit" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::ToolCall]);
}

#[tokio::test]
async fn a_tool_result_event_becomes_output() {
    let collected = run(Script::lines([
        json!({ "type": "tool_result", "content": "ok" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
}

#[tokio::test]
async fn an_error_event_carries_the_error_payload() {
    let collected = run(Script::lines([
        json!({ "type": "error", "error": "cursor error" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data, json!("cursor error"));
}

#[tokio::test]
async fn a_result_event_becomes_done_with_camel_case_tokens() {
    let collected = run(Script::lines([json!({
        "type": "result",
        "usage": {
            "inputTokens": 50,
            "outputTokens": 25,
            "cacheReadTokens": 10,
            "cacheWriteTokens": 5,
        },
    })
    .to_string()]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(
        collected.first().tokens,
        Some(TokenUsage {
            input: 50,
            output: 25,
            reasoning: 0,
            total: 75,
            cache_read: 10,
            cache_write: 5,
        })
    );
}

#[tokio::test]
async fn cursor_does_not_read_usage_out_of_stats() {
    // The one difference from claude's otherwise identical parser.
    let collected = run(Script::lines([
        json!({ "type": "result", "stats": { "usage": { "input_tokens": 7 } } }).to_string(),
    ]))
    .await;
    assert_eq!(collected.first().tokens, None);
}

#[tokio::test]
async fn an_unparseable_line_becomes_output() {
    let collected = run(Script::new().stdout("bad line\n")).await;
    assert_eq!(collected.first().data, json!("bad line"));
}

#[tokio::test]
async fn a_non_zero_exit_without_a_done_event_fails_the_run() {
    let collected = run(Script::new().exit(1)).await;
    assert_eq!(
        collected.error_message(),
        "Cursor agent process exited with code 1"
    );
}

#[tokio::test]
async fn a_done_event_suppresses_the_exit_error() {
    let collected = run(Script::lines([json!({ "type": "result" }).to_string()]).exit(1)).await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
}

#[tokio::test]
async fn stderr_written_before_the_stream_is_polled_still_reaches_the_error() {
    // The stderr drain starts inside `spawn_child` at execute() time, not
    // when the stream is first polled.
    let pm = ScriptedProcessManager::new(
        Script::new()
            .stderr("Error: No prompt provided for print mode\n")
            .exit(1),
    );
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let collected = collect(handle).await;
    assert!(
        collected
            .error_message()
            .contains("No prompt provided for print mode"),
        "{}",
        collected.error_message()
    );
}

#[tokio::test]
async fn the_version_probe_prefers_cursor_agent() {
    let pm = ScriptedProcessManager::new(Script::new().stdout("cursor-agent 1.2.3\n"));
    let result = adapter(&pm).test().await;
    assert!(result.ok);
    assert_eq!(result.version.as_deref(), Some("cursor-agent 1.2.3"));
    assert_eq!(pm.spawns().len(), 1);
    assert_eq!(pm.spawn_record().command, "cursor-agent");
}

#[tokio::test]
async fn the_version_probe_falls_back_to_agent_and_execute_then_uses_it() {
    let pm =
        ScriptedProcessManager::new(Script::new().stdout("agent 9.9\n")).without(&["cursor-agent"]);
    let adapter = adapter(&pm);

    let result = adapter.test().await;
    assert!(result.ok);
    assert_eq!(result.version.as_deref(), Some("agent 9.9"));
    assert_eq!(
        pm.spawns()
            .iter()
            .map(|record| record.command.clone())
            .collect::<Vec<_>>(),
        ["cursor-agent", "agent"]
    );

    adapter.execute(run_params()).expect("execute");
    assert_eq!(pm.spawns().last().expect("spawn").command, "agent");
}

#[tokio::test]
async fn execute_before_a_successful_test_still_spawns_cursor_agent() {
    // Quirk: `resolvedCommand` is only ever assigned by `test()`.
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");
    assert_eq!(pm.spawn_record().command, "cursor-agent");
}

#[tokio::test]
async fn a_missing_cli_reports_adapter_not_found_without_classifying() {
    let pm = ScriptedProcessManager::silent().without(&["cursor-agent", "agent"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some(
            "Cursor Agent CLI not found. The headless agent CLI is required (cursor-agent or agent)."
        )
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::AdapterNotFound));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(8888).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(8888, DEFAULT_KILL_GRACE)]);
}
