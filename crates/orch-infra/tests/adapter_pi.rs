//! Ported from `test/unit/infrastructure/pi-adapter.test.ts`, plus the
//! adapter-only half of `test/integration/pi-adapter.e2e.test.ts` (its
//! orchestrator assertions belong to WP-4).

mod support;

use std::sync::Arc;
use std::time::Duration;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, StdioMode};
use orch_domain::{AdapterErrorKind, AgentConfig, ReasoningEffort, TokenUsage};
use orch_infra::adapters::PiAdapter;
use serde_json::{Value, json};
use support::{
    STDIN_EOF_MARKER, Script, ScriptedProcessManager, collect, fixed_clock, params,
    with_system_prompt,
};

/// The grace period pi uses for its own terminal kill.
const TERMINAL_GRACE: Duration = Duration::from_millis(1_000);

fn adapter(pm: &Arc<ScriptedProcessManager>) -> PiAdapter {
    PiAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn run_params() -> ExecuteParams {
    params("pi prompt", "/tmp/pi-ws")
}

async fn run_with(script: Script) -> (Arc<ScriptedProcessManager>, u32, support::Collected) {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    let pid = handle.pid;
    let collected = collect(handle).await;
    (pm, pid, collected)
}

async fn run(script: Script) -> support::Collected {
    run_with(script).await.2
}

#[test]
fn the_kind_is_pi() {
    assert_eq!(adapter(&ScriptedProcessManager::silent()).kind(), "pi");
}

#[tokio::test]
async fn spawns_pi_in_rpc_mode_with_a_piped_stdin_and_sessions_enabled() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "pi");
    assert_eq!(record.args, ["--mode", "rpc"]);
    assert_eq!(record.cwd, std::path::Path::new("/tmp/pi-ws"));
    assert_eq!(record.stdin, StdioMode::Pipe);
    assert!(!record.has_flag("--no-session"));
}

#[tokio::test]
async fn passes_the_model_and_the_effort_as_thinking() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            run_params(),
            AgentConfig {
                model: Some("openai-codex/gpt-5.5".to_owned()),
                effort: Some(ReasoningEffort::High),
                ..Default::default()
            },
        ))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.value_after("--model"), Some("openai-codex/gpt-5.5"));
    assert_eq!(record.value_after("--thinking"), Some("high"));
}

#[tokio::test]
async fn appends_the_orchestrator_system_prompt_without_replacing_pis_own() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(with_system_prompt(run_params(), "system instructions"))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(
        record.value_after("--append-system-prompt"),
        Some("system instructions")
    );
    assert!(!record.has_flag("--system-prompt"));
}

#[tokio::test]
async fn writes_one_jsonl_prompt_frame_and_keeps_stdin_open() {
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin().hang());
    let handle = adapter(&pm)
        .execute(params("do work", "/tmp/pi-ws"))
        .expect("execute");

    let captured = pm.stdin_capture().await.expect("stdin was written");
    assert!(captured.ends_with('\n'), "the frame is newline-terminated");
    assert!(
        !captured.contains(STDIN_EOF_MARKER),
        "stdin must stay open for the session: {captured:?}"
    );

    let frame: Value = serde_json::from_str(captured.trim_end()).expect("one JSONL frame");
    assert_eq!(frame["type"], json!("prompt"));
    assert_eq!(frame["message"], json!("do work"));
    assert!(
        frame["id"].as_str().expect("id").starts_with("orch-"),
        "{:?}",
        frame["id"]
    );

    // Dropping the handle releases stdin and reaps the session.
    drop(handle);
    adapter(&pm).stop(0).await.ok();
}

#[tokio::test]
async fn extension_ui_requests_are_ignored() {
    let collected = run(Script::lines([
        json!({ "type": "extension_ui_request", "method": "setTitle", "title": "noise" })
            .to_string(),
    ]))
    .await;
    collected.expect_ok();
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn unknown_event_types_are_dropped_rather_than_becoming_output() {
    let collected = run(Script::lines([
        json!({ "type": "brand_new_pi_event", "payload": 1 }).to_string(),
    ]))
    .await;
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn text_deltas_are_aggregated_into_one_output_on_text_end() {
    let collected = run(Script::lines([
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "hello" } }).to_string(),
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": " world" } }).to_string(),
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_end", "content": "hello world" } }).to_string(),
    ]))
    .await;

    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!({ "text": "hello world" }));
}

#[tokio::test]
async fn text_end_without_content_falls_back_to_the_aggregated_deltas() {
    let collected = run(Script::lines([
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "from deltas" } }).to_string(),
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_end" } }).to_string(),
    ]))
    .await;
    assert_eq!(collected.first().data, json!({ "text": "from deltas" }));
}

#[tokio::test]
async fn an_empty_text_end_emits_nothing() {
    let collected = run(Script::lines([
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_end", "content": "" } }).to_string(),
    ]))
    .await;
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn a_message_update_error_becomes_an_error_event() {
    let collected = run(Script::lines([json!({
        "type": "message_update",
        "assistantMessageEvent": {
            "type": "error",
            "error": { "errorMessage": "  provider exploded  " },
        },
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(
        collected.first().data["message"],
        json!("provider exploded")
    );
}

#[tokio::test]
async fn tool_execution_update_progress_is_dropped() {
    let collected = run(Script::lines([json!({
        "type": "tool_execution_update",
        "toolName": "bash",
        "result": { "content": [{ "type": "text", "text": "partial output…" }] },
    })
    .to_string()]))
    .await;
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn tool_execution_maps_to_a_tool_call_then_an_output() {
    let collected = run(Script::lines([
        json!({ "type": "tool_execution_start", "toolName": "read", "args": { "path": "a.ts" } })
            .to_string(),
        json!({
            "type": "tool_execution_end",
            "toolName": "read",
            "result": { "content": [{ "type": "text", "text": "ok" }] },
            "isError": false,
        })
        .to_string(),
    ]))
    .await;

    assert_eq!(
        collected.kinds(),
        [AgentEventKind::ToolCall, AgentEventKind::Output]
    );
    assert_eq!(collected.data()[0]["name"], json!("read"));
    assert_eq!(collected.data()[0]["input"], json!({ "path": "a.ts" }));
    assert_eq!(collected.data()[1]["text"], json!("ok"));
}

#[tokio::test]
async fn a_bash_tool_execution_maps_to_a_command_event() {
    let collected = run(Script::lines([
        json!({
            "type": "tool_execution_end",
            "toolName": "bash",
            "args": { "command": "npm test" },
            "result": { "content": [{ "type": "text", "text": "passed" }], "details": { "exitCode": 0 } },
            "isError": false,
        })
        .to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Command]);
    assert_eq!(collected.first().data["command"], json!("npm test"));
    assert_eq!(collected.first().data["result"], json!("passed"));
}

#[tokio::test]
async fn write_and_edit_tool_executions_map_to_file_changes() {
    for tool in ["write", "edit", "Write"] {
        let collected = run(Script::lines([json!({
            "type": "tool_execution_end",
            "toolName": tool,
            "args": { "path": "src/new.ts" },
            "result": { "content": [{ "type": "text", "text": "written" }] },
            "isError": false,
        })
        .to_string()]))
        .await;

        assert_eq!(collected.kinds(), [AgentEventKind::FileChange], "{tool}");
        assert_eq!(collected.first().data["paths"], json!(["src/new.ts"]));
    }
}

#[tokio::test]
async fn a_file_path_argument_is_accepted_too() {
    let collected = run(Script::lines([json!({
        "type": "tool_execution_end",
        "toolName": "edit",
        "args": { "file_path": "src/old.ts" },
        "isError": false,
    })
    .to_string()]))
    .await;
    assert_eq!(collected.first().data["paths"], json!(["src/old.ts"]));
}

#[tokio::test]
async fn a_failed_tool_execution_becomes_an_error() {
    let collected = run(Script::lines([json!({
        "type": "tool_execution_end",
        "toolName": "bash",
        "args": { "command": "false" },
        "result": { "content": [{ "type": "text", "text": "exit status 1" }] },
        "isError": true,
    })
    .to_string()]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data["message"], json!("exit status 1"));
}

#[tokio::test]
async fn a_generic_tool_execution_without_a_result_reports_completion() {
    let collected = run(Script::lines([
        json!({ "type": "tool_execution_end", "toolName": "ls", "isError": false }).to_string(),
    ]))
    .await;
    assert_eq!(collected.first().data["text"], json!("ls completed"));
}

#[tokio::test]
async fn agent_end_becomes_done_with_the_final_text_and_tokens() {
    let (pm, pid, collected) = run_with(Script::lines([
        json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "done text" } }).to_string(),
        json!({
            "type": "agent_end",
            "messages": [{
                "role": "assistant",
                "content": [{ "type": "text", "text": "done text" }],
                "usage": { "input": 10, "output": 4, "totalTokens": 14 },
            }],
        })
        .to_string(),
    ]))
    .await;

    collected.expect_ok();
    assert_eq!(collected.last().kind, AgentEventKind::Done);
    assert_eq!(collected.last().data["result"], json!("done text"));
    assert_eq!(
        collected.last().tokens,
        Some(TokenUsage {
            input: 10,
            output: 4,
            reasoning: 0,
            total: 14,
            cache_read: 0,
            cache_write: 0,
        })
    );
    // The long-lived session is reaped as soon as the run is over.
    assert_eq!(pm.killed_with_grace(), [(pid, TERMINAL_GRACE)]);
}

#[tokio::test]
async fn a_large_agent_end_frame_is_not_truncated() {
    // pi opts out of the shared 16 KB line cap for exactly this reason.
    let large_text = "x".repeat(20_000);
    let collected = run(Script::lines([json!({
        "type": "agent_end",
        "messages": [{
            "role": "assistant",
            "content": [{ "type": "text", "text": large_text }],
            "usage": { "input": 10, "output": 4 },
        }],
    })
    .to_string()]))
    .await;

    assert_eq!(collected.last().kind, AgentEventKind::Done);
    assert_eq!(
        collected.last().data["result"]
            .as_str()
            .expect("result")
            .len(),
        20_000
    );
}

#[tokio::test]
async fn a_response_failure_is_a_terminal_error() {
    let (pm, pid, collected) = run_with(Script::lines([
        json!({ "type": "response", "command": "prompt", "success": false, "error": "bad request" })
            .to_string(),
    ]))
    .await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::Unknown)
    );
    assert!(!collected.has_kind(AgentEventKind::Done));
    assert_eq!(collected.error_message(), "bad request");
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::Unknown
    );
    assert_eq!(pm.killed_with_grace(), [(pid, TERMINAL_GRACE)]);
}

#[tokio::test]
async fn a_successful_response_frame_emits_nothing() {
    let collected = run(Script::lines([
        json!({ "type": "response", "command": "prompt", "success": true }).to_string(),
    ]))
    .await;
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn a_terminal_rate_limit_becomes_an_error_rather_than_done() {
    let message = "Permission denied: Reached overall message rate limit. Please try again later.";
    let (pm, pid, collected) = run_with(Script::lines([json!({
        "type": "agent_end",
        "messages": [{
            "role": "assistant",
            "content": [],
            "stopReason": "error",
            "errorMessage": message,
        }],
    })
    .to_string()]))
    .await;

    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.first().kind, AgentEventKind::Error);
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::RateLimit)
    );
    assert_eq!(collected.first().data["message"], json!(message));
    assert!(!collected.has_kind(AgentEventKind::Done));
    assert_eq!(collected.error_message(), message);
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::RateLimit
    );
    assert_eq!(pm.killed_with_grace(), [(pid, TERMINAL_GRACE)]);
}

#[tokio::test]
async fn waits_through_an_auto_retry_and_emits_only_the_settled_completion() {
    let (pm, pid, collected) = run_with(Script::lines([
        json!({
            "type": "agent_end",
            "willRetry": true,
            "messages": [{
                "role": "assistant",
                "content": [],
                "stopReason": "error",
                "errorMessage": "Rate limit exceeded; retrying",
            }],
        })
        .to_string(),
        json!({ "type": "auto_retry_start", "attempt": 1 }).to_string(),
        json!({
            "type": "agent_end",
            "willRetry": false,
            "messages": [{
                "role": "assistant",
                "content": [{ "type": "text", "text": "Recovered after retry." }],
                "stopReason": "stop",
                "usage": { "input": 12, "output": 5 },
            }],
        })
        .to_string(),
        json!({ "type": "agent_settled" }).to_string(),
    ]))
    .await;

    collected.expect_ok();
    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.first().kind, AgentEventKind::Done);
    assert_eq!(
        collected.first().data["result"],
        json!("Recovered after retry.")
    );
    assert_eq!(
        collected.first().tokens,
        Some(TokenUsage {
            input: 12,
            output: 5,
            reasoning: 0,
            total: 17,
            cache_read: 0,
            cache_write: 0,
        })
    );
    assert_eq!(pm.killed_with_grace(), [(pid, TERMINAL_GRACE)]);
}

#[tokio::test]
async fn fails_after_agent_settled_when_pi_exhausts_its_retries() {
    let message = "Request timed out after retries";
    let collected = run(Script::lines([
        json!({
            "type": "agent_end",
            "willRetry": false,
            "messages": [{
                "role": "assistant",
                "content": [],
                "stopReason": "error",
                "errorMessage": message,
            }],
        })
        .to_string(),
        json!({ "type": "agent_settled" }).to_string(),
    ]))
    .await;

    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.first().kind, AgentEventKind::Error);
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::Timeout)
    );
    assert!(!collected.has_kind(AgentEventKind::Done));
    assert_eq!(collected.error_message(), message);
}

#[tokio::test]
async fn an_agent_end_without_will_retry_is_itself_terminal() {
    // Older pi versions have neither `willRetry` nor `agent_settled`.
    let collected = run(Script::lines([json!({
        "type": "agent_end",
        "messages": [{
            "role": "assistant",
            "content": [{ "type": "text", "text": "legacy" }],
            "stopReason": "stop",
        }],
    })
    .to_string()]))
    .await;

    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(collected.first().data["result"], json!("legacy"));
}

#[tokio::test]
async fn passive_lifecycle_events_contribute_their_usage() {
    // `message_end` carries the running usage; `agent_end` with no usage of
    // its own falls back to it.
    let collected = run(Script::lines([
        json!({ "type": "message_end", "usage": { "input": 3, "output": 2 } }).to_string(),
        json!({
            "type": "agent_end",
            "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }],
        })
        .to_string(),
    ]))
    .await;

    assert_eq!(collected.first().tokens.expect("tokens").total, 5);
}

#[tokio::test]
async fn an_extension_error_becomes_an_error_event_without_ending_the_run() {
    let collected = run(Script::lines([
        json!({ "type": "extension_error", "message": "failed to load extension foo" }).to_string(),
        json!({
            "type": "agent_end",
            "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }],
        })
        .to_string(),
    ]))
    .await;

    collected.expect_ok();
    assert_eq!(
        collected.kinds(),
        [AgentEventKind::Error, AgentEventKind::Done]
    );
}

#[tokio::test]
async fn an_unparseable_line_becomes_a_text_output() {
    let collected = run(Script::new().stdout("not json\n")).await;
    assert_eq!(collected.data()[0], &json!({ "text": "not json" }));
}

#[tokio::test]
async fn a_carriage_return_is_stripped_but_long_lines_are_not_capped() {
    // pi's own reader, not the shared one (PARITY §14.13).
    let frame = json!({
        "type": "agent_end",
        "messages": [{
            "role": "assistant",
            "content": [{ "type": "text", "text": "y".repeat(20_000) }],
        }],
    })
    .to_string();
    let collected = run(Script::new().stdout(format!("{frame}\r\n"))).await;

    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(
        collected.first().data["result"]
            .as_str()
            .expect("result")
            .len(),
        20_000
    );
}

#[tokio::test]
async fn an_aborted_run_reaps_the_session() {
    let pm = ScriptedProcessManager::new(
        Script::lines([
            json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "partial" } })
                .to_string(),
        ])
        .hang(),
    );
    let execute_params = run_params();
    let cancel = execute_params.cancel.clone();
    let handle = adapter(&pm).execute(execute_params).expect("execute");
    let pid = handle.pid;

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let collected = collect(handle).await;
    collected.expect_ok();
    assert_eq!(pm.killed_with_grace(), [(pid, TERMINAL_GRACE)]);
}

#[tokio::test]
async fn the_stderr_tail_is_appended_on_a_non_zero_exit() {
    let collected = run(Script::new()
        .stderr("error: failed to load extension foo\n")
        .exit(2))
    .await;

    assert_eq!(
        collected.error_message(),
        "Pi process exited with code 2\n\
         --- pi stderr (tail) ---\n\
         error: failed to load extension foo"
    );
}

#[tokio::test]
async fn a_done_event_suppresses_the_exit_error() {
    let collected = run(Script::lines([json!({
        "type": "agent_end",
        "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }],
    })
    .to_string()])
    .exit(1))
    .await;
    collected.expect_ok();
}

#[tokio::test]
async fn the_version_probe_reports_the_version() {
    let pm = ScriptedProcessManager::new(Script::new().stdout("pi 1.0.0\n"));
    let result = adapter(&pm).test().await;
    assert!(result.ok);
    assert_eq!(result.version.as_deref(), Some("pi 1.0.0"));
}

#[tokio::test]
async fn the_version_probe_classifies_a_missing_binary() {
    let pm = ScriptedProcessManager::silent().without(&["pi"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent")
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::SpawnFailed));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(8888).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(8888, DEFAULT_KILL_GRACE)]);
}
