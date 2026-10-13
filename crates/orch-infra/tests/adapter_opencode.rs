//! Ported from `test/unit/infrastructure/opencode-adapter.test.ts`.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager};
use orch_domain::{AdapterErrorKind, AgentConfig, TokenUsage};
use orch_infra::adapters::OpenCodeAdapter;
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, with_system_prompt};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> OpenCodeAdapter {
    OpenCodeAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn run_params() -> ExecuteParams {
    params("test prompt", "/tmp/workspace")
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_opencode() {
    assert_eq!(
        adapter(&ScriptedProcessManager::silent()).kind(),
        "opencode"
    );
}

#[tokio::test]
async fn spawns_opencode_with_the_contract_argv_and_no_workspace_flag() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "opencode");
    assert_eq!(record.args, ["run", "--format", "json", "test prompt"]);
    // The workspace reaches the child only through the cwd.
    assert!(!record.has_flag("--dir"));
    assert!(!record.has_flag("--workspace"));
    assert_eq!(record.cwd, std::path::Path::new("/tmp/workspace"));
}

#[tokio::test]
async fn includes_the_model_before_the_prompt() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            run_params(),
            AgentConfig {
                model: Some("anthropic/claude-sonnet-4".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(
        record.value_after("--model"),
        Some("anthropic/claude-sonnet-4")
    );
    assert_eq!(record.last_arg(), "test prompt");
}

#[tokio::test]
async fn the_system_prompt_is_prepended_to_the_prompt() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(with_system_prompt(
            params("user task", "/tmp/workspace"),
            "system instructions",
        ))
        .expect("execute");
    assert_eq!(
        pm.spawn_record().last_arg(),
        "system instructions\n\nuser task"
    );
}

#[tokio::test]
async fn a_text_event_becomes_output_carrying_the_bare_text() {
    let collected = run(Script::lines([json!({
        "type": "text",
        "timestamp": 1234,
        "sessionID": "ses_1",
        "part": { "type": "text", "text": "hello world" },
    })
    .to_string()]))
    .await;

    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!("hello world"));
}

#[tokio::test]
async fn a_text_event_without_text_falls_back_to_the_whole_part() {
    let collected = run(Script::lines([
        json!({ "type": "text", "part": { "type": "text" } }).to_string(),
    ]))
    .await;
    assert_eq!(collected.first().data, json!({ "type": "text" }));
}

#[tokio::test]
async fn step_start_is_skipped() {
    let collected = run(Script::lines([
        json!({ "type": "step_start", "part": { "type": "step-start", "snapshot": "abc123" } })
            .to_string(),
        json!({ "type": "text", "part": { "text": "hello" } }).to_string(),
    ]))
    .await;

    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.first().data, json!("hello"));
}

#[tokio::test]
async fn a_tool_use_event_becomes_a_name_and_input_tool_call() {
    let collected = run(Script::lines([json!({
        "type": "tool_use",
        "part": {
            "type": "tool",
            "tool": "write",
            "state": {
                "status": "completed",
                "input": { "filePath": "/tmp/test.txt", "content": "test" },
            },
        },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::ToolCall]);
    assert_eq!(collected.first().data["name"], json!("write"));
    assert_eq!(
        collected.first().data["input"],
        json!({ "filePath": "/tmp/test.txt", "content": "test" })
    );
}

#[tokio::test]
async fn a_tool_use_without_a_tool_or_input_omits_the_keys_entirely() {
    // `JSON.stringify` drops `undefined` properties, so an absent tool must
    // not become an explicit null.
    let collected = run(Script::lines([
        json!({ "type": "tool_use", "part": { "state": { "status": "completed" } } }).to_string(),
    ]))
    .await;
    assert_eq!(collected.first().data, json!({}));
}

#[tokio::test]
async fn a_tool_use_in_the_error_state_becomes_an_error() {
    let collected = run(Script::lines([json!({
        "type": "tool_use",
        "part": {
            "type": "tool",
            "tool": "write",
            "state": { "status": "error", "error": "The user rejected permission" },
        },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(
        collected.first().data,
        json!({ "status": "error", "error": "The user rejected permission" })
    );
}

#[tokio::test]
async fn step_finish_with_reason_stop_becomes_done_and_recomputes_the_total() {
    let collected = run(Script::lines([json!({
        "type": "step_finish",
        "part": {
            "type": "step-finish",
            "reason": "stop",
            // The provider's own total is ignored.
            "tokens": { "total": 14_502, "input": 12_416, "output": 2, "reasoning": 0 },
            "cost": 0,
        },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(
        collected.first().tokens,
        Some(TokenUsage {
            input: 12_416,
            output: 2,
            reasoning: 0,
            total: 12_418,
            cache_read: 0,
            cache_write: 0,
        })
    );
}

#[tokio::test]
async fn step_finish_with_reason_tool_calls_is_skipped() {
    let collected = run(Script::lines([
        json!({
            "type": "step_finish",
            "part": { "reason": "tool-calls", "tokens": { "total": 500, "input": 400, "output": 100 } },
        })
        .to_string(),
        json!({ "type": "text", "part": { "text": "next step" } }).to_string(),
    ]))
    .await;

    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
}

#[tokio::test]
async fn step_finish_with_reason_error_becomes_an_error_with_tokens() {
    let collected = run(Script::lines([json!({
        "type": "step_finish",
        "part": {
            "type": "step-finish",
            "reason": "error",
            "error": "model overloaded",
            "tokens": { "input": 5, "output": 1 },
        },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().tokens.expect("tokens").total, 6);
}

#[tokio::test]
async fn step_finish_with_any_other_reason_becomes_done() {
    let collected = run(Script::lines([
        json!({
            "type": "step_finish",
            "part": { "reason": "max_tokens", "tokens": { "total": 8000, "input": 7000, "output": 1000 } },
        })
        .to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(collected.first().tokens.expect("tokens").total, 8000);
}

#[tokio::test]
async fn step_finish_without_a_reason_becomes_done() {
    let collected = run(Script::lines([
        json!({ "type": "step_finish", "part": {} }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(collected.first().tokens, None);
}

#[tokio::test]
async fn opencode_tokens_need_a_numeric_input() {
    let collected = run(Script::lines([
        json!({ "type": "step_finish", "part": { "reason": "stop", "tokens": { "output": 5 } } })
            .to_string(),
    ]))
    .await;
    assert_eq!(collected.first().tokens, None);
}

#[tokio::test]
async fn an_unparseable_line_becomes_output() {
    let collected = run(Script::new().stdout("not json at all\n")).await;
    assert_eq!(collected.first().data, json!("not json at all"));
}

#[tokio::test]
async fn empty_lines_are_skipped() {
    let collected = run(Script::new().stdout("\n\n").stdout(format!(
        "{}\n",
        json!({ "type": "text", "part": { "text": "hi" } })
    )))
    .await;
    assert_eq!(collected.events.len(), 1);
}

#[tokio::test]
async fn an_unknown_event_type_becomes_output() {
    let collected = run(Script::lines([
        json!({ "type": "unknown_type", "data": "info" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
}

#[tokio::test]
async fn a_non_zero_exit_without_a_done_event_fails_the_run() {
    let collected = run(Script::new().exit(1)).await;
    assert_eq!(
        collected.error_message(),
        "OpenCode process exited with code 1"
    );
}

#[tokio::test]
async fn a_done_event_suppresses_the_exit_error() {
    let collected = run(Script::lines([json!({
        "type": "step_finish",
        "part": { "reason": "stop", "tokens": { "total": 100, "input": 80, "output": 20 } },
    })
    .to_string()])
    .exit(1))
    .await;

    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
}

#[tokio::test]
async fn the_version_probe_classifies_a_missing_binary() {
    let pm = ScriptedProcessManager::silent().without(&["opencode"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("OpenCode CLI not found. Install: npm i -g opencode")
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::SpawnFailed));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(7777).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(7777, DEFAULT_KILL_GRACE)]);
}
