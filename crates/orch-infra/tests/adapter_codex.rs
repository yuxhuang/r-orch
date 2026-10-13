//! Ported from `test/unit/infrastructure/codex-adapter.test.ts`.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, StdioMode};
use orch_domain::{AdapterErrorKind, AgentConfig, TokenUsage};
use orch_infra::adapters::CodexAdapter;
use serde_json::json;
use support::{
    STDIN_EOF_MARKER, Script, ScriptedProcessManager, collect, fixed_clock, params,
    with_system_prompt,
};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> CodexAdapter {
    CodexAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn run_params() -> ExecuteParams {
    params("codex prompt", "/tmp/codex-ws")
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_codex() {
    assert_eq!(adapter(&ScriptedProcessManager::silent()).kind(), "codex");
}

#[tokio::test]
async fn spawns_codex_with_the_contract_argv_and_a_piped_stdin() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "codex");
    assert_eq!(
        record.args,
        ["exec", "--json", "--sandbox", "danger-full-access", "-"]
    );
    assert_eq!(record.cwd, std::path::Path::new("/tmp/codex-ws"));
    assert_eq!(record.stdin, StdioMode::Pipe);
}

#[tokio::test]
async fn the_model_comes_before_the_stdin_marker() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            run_params(),
            AgentConfig {
                model: Some("gpt-4".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.value_after("--model"), Some("gpt-4"));
    assert_eq!(record.last_arg(), "-");
}

#[tokio::test]
async fn writes_the_prompt_to_stdin_and_closes_it() {
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin().sleep_ms(200).exit(0));
    let handle = adapter(&pm).execute(run_params()).expect("execute");

    let captured = pm.stdin_capture().await.expect("stdin was written");
    assert_eq!(captured, format!("codex prompt{STDIN_EOF_MARKER}"));
    collect(handle).await.expect_ok();
}

#[tokio::test]
async fn the_system_prompt_is_prepended_on_stdin() {
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin().sleep_ms(200).exit(0));
    let handle = adapter(&pm)
        .execute(with_system_prompt(
            params("user task", "/tmp/codex-ws"),
            "system instructions",
        ))
        .expect("execute");

    assert_eq!(
        pm.stdin_capture().await.expect("stdin"),
        format!("system instructions\n\nuser task{STDIN_EOF_MARKER}")
    );
    collect(handle).await.expect_ok();
}

#[tokio::test]
async fn turn_completed_becomes_done_with_tokens() {
    let collected = run(Script::lines([
        json!({ "type": "turn.completed", "usage": { "input_tokens": 200, "output_tokens": 80 } })
            .to_string(),
    ]))
    .await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(
        collected.first().tokens,
        Some(TokenUsage {
            input: 200,
            output: 80,
            reasoning: 0,
            total: 280,
            cache_read: 0,
            cache_write: 0,
        })
    );
    // An absent (or empty) `result` is omitted from the payload.
    assert_eq!(collected.first().data.get("result"), None);
}

#[tokio::test]
async fn turn_completed_keeps_a_non_empty_result_before_raw() {
    let collected = run(Script::lines([
        json!({ "type": "turn.completed", "result": "all done" }).to_string(),
    ]))
    .await;
    assert_eq!(
        serde_json::to_string(&collected.first().data).expect("json"),
        r#"{"result":"all done","raw":{"type":"turn.completed","result":"all done"}}"#
    );
}

#[tokio::test]
async fn turn_failed_becomes_an_error() {
    let collected = run(Script::lines([
        json!({ "type": "turn.failed", "error": "bad" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data["message"], json!("bad"));
    assert!(collected.first().error_kind.is_some());
}

#[tokio::test]
async fn lifecycle_noise_is_dropped() {
    let collected = run(Script::lines([
        json!({ "type": "thread.started" }).to_string(),
        json!({ "type": "turn.started" }).to_string(),
    ]))
    .await;
    collected.expect_ok();
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn an_agent_message_item_becomes_canonical_output() {
    let item = json!({ "id": "item_0", "type": "agent_message", "text": "hello" });
    let collected = run(Script::lines([
        json!({ "type": "item.completed", "item": item }).to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data["text"], json!("hello"));
    assert_eq!(collected.first().data["raw"], item);
}

#[tokio::test]
async fn an_empty_agent_message_is_dropped() {
    let collected = run(Script::lines([
        json!({ "type": "item.completed", "item": { "type": "agent_message", "text": "" } })
            .to_string(),
    ]))
    .await;
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn a_reasoning_item_is_dropped() {
    let collected = run(Script::lines([
        json!({ "type": "item.completed", "item": { "type": "reasoning", "text": "thinking" } })
            .to_string(),
    ]))
    .await;
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn a_command_execution_is_normalized_and_its_started_event_dropped() {
    let collected = run(Script::lines([
        json!({
            "type": "item.started",
            "item": { "type": "command_execution", "command": "ls", "status": "in_progress" },
        })
        .to_string(),
        json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "command": "ls",
                "aggregated_output": "file.txt\n",
                "exit_code": 0,
            },
        })
        .to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Command]);
    assert_eq!(collected.first().data["command"], json!("ls"));
    assert_eq!(collected.first().data["result"], json!("file.txt\n"));
}

#[tokio::test]
async fn a_file_change_item_extracts_its_paths() {
    let collected = run(Script::lines([json!({
        "type": "item.completed",
        "item": { "type": "file_change", "changes": [{ "path": "a.ts" }, { "path": "b.ts" }] },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::FileChange]);
    assert_eq!(collected.first().data["paths"], json!(["a.ts", "b.ts"]));
}

#[tokio::test]
async fn a_tool_use_item_becomes_a_canonical_tool_call() {
    let collected = run(Script::lines([json!({
        "type": "item.completed",
        "item": { "type": "tool_use", "name": "search", "input": { "query": "orch" } },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::ToolCall]);
    assert_eq!(collected.first().data["name"], json!("search"));
    assert_eq!(collected.first().data["input"], json!({ "query": "orch" }));
}

#[tokio::test]
async fn a_tool_use_item_without_a_name_falls_back_to_tool_then_the_item_type() {
    let collected = run(Script::lines([
        json!({ "type": "item.completed", "item": { "type": "tool_use", "tool": "grep" } })
            .to_string(),
        json!({ "type": "item.completed", "item": { "type": "mcp_tool_call" } }).to_string(),
    ]))
    .await;

    assert_eq!(collected.data()[0]["name"], json!("grep"));
    assert_eq!(collected.data()[1]["name"], json!("mcp_tool_call"));
}

#[tokio::test]
async fn a_web_search_item_becomes_a_tool_call_rather_than_text() {
    let collected = run(Script::lines([json!({
        "type": "item.completed",
        "item": {
            "id": "call_1",
            "type": "web_search",
            "query": "privacy preserving AI",
            "action": { "type": "search", "queries": ["privacy preserving AI"] },
        },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::ToolCall]);
    assert_eq!(collected.first().data["name"], json!("web_search"));
    assert_eq!(
        collected.first().data["input"]["query"],
        json!("privacy preserving AI")
    );
}

#[tokio::test]
async fn an_error_item_becomes_an_error_event() {
    let collected = run(Script::lines([
        json!({ "type": "item.completed", "item": { "type": "error", "message": "oops" } })
            .to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data["message"], json!("oops"));
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::Unknown)
    );
}

#[tokio::test]
async fn an_unknown_item_type_falls_back_to_its_text() {
    let collected = run(Script::lines([
        json!({ "type": "item.completed", "item": { "type": "whatever", "content": "body" } })
            .to_string(),
        json!({ "type": "item.completed", "item": { "type": "whatever" } }).to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data["text"], json!("body"));
}

#[tokio::test]
async fn a_top_level_error_becomes_an_error_event() {
    let collected = run(Script::lines([
        json!({ "type": "error", "error": "fatal" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data["message"], json!("fatal"));
}

#[tokio::test]
async fn a_401_in_the_message_classifies_as_auth_failed() {
    let collected = run(Script::lines([
        json!({ "type": "error", "error": "HTTP 401 unauthorized" }).to_string(),
    ]))
    .await;
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::AuthFailed)
    );
}

#[tokio::test]
async fn nested_provider_json_is_unwrapped_and_the_duplicate_is_suppressed() {
    let provider_error = json!({
        "type": "error",
        "status": 400,
        "error": {
            "type": "invalid_request_error",
            "message": "The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account.",
        },
    })
    .to_string();

    let collected = run(Script::lines([
        json!({ "type": "error", "message": provider_error }).to_string(),
        json!({ "type": "turn.failed", "error": { "message": provider_error } }).to_string(),
    ]))
    .await;

    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(
        collected.first().data["message"],
        json!("The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account.")
    );
}

#[tokio::test]
async fn the_error_dedup_resets_on_any_non_error_event() {
    let collected = run(Script::lines([
        json!({ "type": "error", "error": "same" }).to_string(),
        json!({ "type": "item.completed", "item": { "type": "agent_message", "text": "hi" } })
            .to_string(),
        json!({ "type": "error", "error": "same" }).to_string(),
    ]))
    .await;

    assert_eq!(
        collected.kinds(),
        [
            AgentEventKind::Error,
            AgentEventKind::Output,
            AgentEventKind::Error
        ]
    );
}

#[tokio::test]
async fn a_model_metadata_warning_stays_out_of_the_error_stream() {
    let collected = run(Script::lines([
        json!({
            "type": "item.completed",
            "item": {
                "id": "item_0",
                "type": "error",
                "message": "Model metadata for `gpt-5.6` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.",
            },
        })
        .to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert!(
        collected.first().data["text"]
            .as_str()
            .expect("text")
            .starts_with("⚠ Model metadata for `gpt-5.6` not found"),
        "{:?}",
        collected.first().data["text"]
    );
}

#[tokio::test]
async fn an_unparseable_line_becomes_output() {
    let collected = run(Script::new().stdout("garbage\n")).await;
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!("garbage"));
}

#[tokio::test]
async fn a_non_zero_exit_without_a_done_event_fails_the_run() {
    let collected = run(Script::new().exit(1)).await;
    assert_eq!(
        collected.error_message(),
        "Codex process exited with code 1"
    );
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(7777).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(7777, DEFAULT_KILL_GRACE)]);
}

#[tokio::test]
async fn the_version_probe_classifies_a_missing_binary() {
    let pm = ScriptedProcessManager::silent().without(&["codex"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("Codex CLI not found. Install: npm i -g @openai/codex")
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::SpawnFailed));
}
