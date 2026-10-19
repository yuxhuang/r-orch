//! Claude adapter tests.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind};
use orch_core::process::{DEFAULT_KILL_GRACE, StdioMode};
use orch_domain::{AdapterErrorKind, AgentConfig, ReasoningEffort, TokenUsage};
use orch_infra::adapters::ClaudeAdapter;
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, with_system_prompt};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> ClaudeAdapter {
    ClaudeAdapter::new(Arc::clone(pm) as Arc<dyn orch_core::process::ProcessManager>)
        .with_clock(fixed_clock())
}

fn config() -> AgentConfig {
    AgentConfig {
        max_turns: Some(10),
        ..Default::default()
    }
}

fn run_params() -> orch_core::adapter::ExecuteParams {
    support::with_config(params("test prompt", "/tmp/workspace"), config())
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_claude() {
    let pm = ScriptedProcessManager::silent();
    assert_eq!(adapter(&pm).kind(), "claude");
}

#[tokio::test]
async fn spawns_claude_with_the_contract_argv() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "claude");
    assert_eq!(
        record.args,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--input-format",
            "text",
            "--max-turns",
            "10",
            "--verbose",
            "--dangerously-skip-permissions",
        ]
    );
    assert_eq!(record.cwd, std::path::Path::new("/tmp/workspace"));
    assert_eq!(record.stdin, StdioMode::Pipe);
}

#[tokio::test]
async fn max_turns_defaults_to_fifty() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(params("p", "/tmp/workspace"))
        .expect("execute");
    assert_eq!(pm.spawn_record().value_after("--max-turns"), Some("50"));
}

#[tokio::test]
async fn includes_the_model_when_configured() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            params("p", "/tmp/workspace"),
            AgentConfig {
                model: Some("sonnet".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");
    assert_eq!(pm.spawn_record().value_after("--model"), Some("sonnet"));
}

#[tokio::test]
async fn includes_the_effort_only_when_configured() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            params("p", "/tmp/workspace"),
            AgentConfig {
                effort: Some(ReasoningEffort::High),
                ..Default::default()
            },
        ))
        .expect("execute");
    assert_eq!(pm.spawn_record().value_after("--effort"), Some("high"));

    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(params("p", "/tmp/workspace"))
        .expect("execute");
    assert!(!pm.spawn_record().has_flag("--effort"));
}

/// Spawn and keep the handle alive: the system-prompt file lives exactly as
/// long as the event stream.
fn execute_keeping(
    pm: &Arc<ScriptedProcessManager>,
    execute_params: orch_core::adapter::ExecuteParams,
) -> orch_core::adapter::ExecuteHandle {
    adapter(pm).execute(execute_params).expect("execute")
}

fn system_prompt_file_of(record: &support::SpawnRecord) -> std::path::PathBuf {
    record
        .value_after("--system-prompt-file")
        .expect("--system-prompt-file")
        .into()
}

#[tokio::test]
async fn the_orchestrator_system_prompt_wins_over_the_agent_one() {
    let pm = ScriptedProcessManager::silent();
    let _handle = execute_keeping(
        &pm,
        with_system_prompt(
            support::with_config(
                params("p", "/tmp/workspace"),
                AgentConfig {
                    system_prompt: Some("Agent config system prompt".to_owned()),
                    ..Default::default()
                },
            ),
            "Orchestrator system prompt",
        ),
    );
    let record = pm.spawn_record();
    assert!(!record.has_flag("--system-prompt"));
    assert_eq!(
        std::fs::read_to_string(system_prompt_file_of(&record)).expect("read"),
        "Orchestrator system prompt"
    );
}

#[tokio::test]
async fn falls_back_to_the_agent_system_prompt() {
    let pm = ScriptedProcessManager::silent();
    let _handle = execute_keeping(
        &pm,
        support::with_config(
            params("p", "/tmp/workspace"),
            AgentConfig {
                system_prompt: Some("Agent config prompt".to_owned()),
                ..Default::default()
            },
        ),
    );
    assert_eq!(
        std::fs::read_to_string(system_prompt_file_of(&pm.spawn_record())).expect("read"),
        "Agent config prompt"
    );
}

#[tokio::test]
async fn omits_the_system_prompt_when_there_is_none() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(params("p", "/tmp/workspace"))
        .expect("execute");
    let record = pm.spawn_record();
    assert!(!record.has_flag("--system-prompt"));
    assert!(!record.has_flag("--system-prompt-file"));
}

#[tokio::test]
async fn an_empty_system_prompt_is_treated_as_none() {
    let pm = ScriptedProcessManager::silent();
    let _handle = execute_keeping(&pm, with_system_prompt(run_params(), ""));
    let record = pm.spawn_record();
    assert!(!record.has_flag("--system-prompt-file"));
    assert!(!record.has_flag("--system-prompt"));
}

fn run_with_prompts(
    orchestrator: &str,
    agent: Option<&str>,
) -> (
    Arc<ScriptedProcessManager>,
    orch_core::adapter::ExecuteHandle,
) {
    let pm = ScriptedProcessManager::silent();
    let handle = execute_keeping(
        &pm,
        with_system_prompt(
            support::with_config(
                params("p", "/tmp/workspace"),
                AgentConfig {
                    system_prompt: agent.map(str::to_owned),
                    ..Default::default()
                },
            ),
            orchestrator,
        ),
    );
    (pm, handle)
}

#[tokio::test]
async fn a_blank_orchestrator_prompt_falls_through_to_the_agent_one() {
    for blank in ["", "  \n\t "] {
        let (pm, _handle) = run_with_prompts(blank, Some("agent prompt"));
        assert_eq!(
            std::fs::read_to_string(system_prompt_file_of(&pm.spawn_record())).expect("read"),
            "agent prompt"
        );
    }
}

#[tokio::test]
async fn a_blank_agent_prompt_does_not_replace_the_orchestrator_one() {
    let (pm, _handle) = run_with_prompts("orchestrator prompt", Some(" "));
    assert_eq!(
        std::fs::read_to_string(system_prompt_file_of(&pm.spawn_record())).expect("read"),
        "orchestrator prompt"
    );
}

#[tokio::test]
async fn blank_prompts_everywhere_mean_no_system_prompt() {
    for agent in [None, Some(""), Some(" \n")] {
        let (pm, _handle) = run_with_prompts(" ", agent);
        assert!(!pm.spawn_record().has_flag("--system-prompt-file"));
    }
}

#[tokio::test]
async fn the_input_format_is_always_text() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");
    assert_eq!(
        pm.spawn_record().value_after("--input-format"),
        Some("text")
    );
}

#[tokio::test]
async fn neither_prompt_is_in_argv() {
    let pm = ScriptedProcessManager::silent();
    let _handle = execute_keeping(
        &pm,
        with_system_prompt(
            params("user task prompt", "/tmp/workspace"),
            "system instructions",
        ),
    );

    let record = pm.spawn_record();
    assert!(record.has_flag("--system-prompt-file"));
    assert!(
        record
            .args
            .iter()
            .all(|arg| !arg.contains("user task prompt") && !arg.contains("system instructions")),
        "prompts leaked into argv: {:?}",
        record.args
    );
}

#[tokio::test]
async fn the_system_prompt_file_is_private_and_outside_the_workspace() {
    use std::os::unix::fs::PermissionsExt;

    let pm = ScriptedProcessManager::silent();
    let _handle = execute_keeping(&pm, with_system_prompt(run_params(), "secret"));
    let path = system_prompt_file_of(&pm.spawn_record());

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(!path.starts_with("/tmp/workspace"));

    let dir = path.parent().expect("parent");
    let dir_meta = std::fs::symlink_metadata(dir).expect("dir metadata");
    assert!(dir_meta.is_dir());
    assert_eq!(dir_meta.permissions().mode() & 0o7777, 0o700);
}

#[tokio::test]
async fn the_prompt_is_written_to_stdin_and_stdin_is_closed() {
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin().sleep_ms(200).exit(0));
    let handle = execute_keeping(&pm, params("--user task\nprompt", "/tmp/workspace"));
    collect(handle).await.expect_ok();
    assert_eq!(
        pm.stdin_capture().await.expect("stdin was written"),
        format!("--user task\nprompt{}", support::STDIN_EOF_MARKER)
    );
}

#[tokio::test]
async fn the_system_prompt_file_is_removed_after_the_run() {
    let pm = ScriptedProcessManager::silent();
    let handle = execute_keeping(&pm, with_system_prompt(run_params(), "system"));
    let path = system_prompt_file_of(&pm.spawn_record());
    assert!(path.exists());
    collect(handle).await.expect_ok();
    assert!(!path.exists(), "{} was left behind", path.display());
}

#[tokio::test]
async fn the_system_prompt_file_is_removed_when_the_run_is_dropped() {
    let pm = ScriptedProcessManager::new(Script::new().sleep_ms(30_000));
    let handle = execute_keeping(&pm, with_system_prompt(run_params(), "system"));
    let path = system_prompt_file_of(&pm.spawn_record());
    assert!(path.exists());
    drop(handle);
    assert!(!path.exists(), "{} was left behind", path.display());
}

#[tokio::test]
async fn the_system_prompt_file_is_removed_when_spawning_fails() {
    let pm = ScriptedProcessManager::silent().without(&["claude"]);
    let result = adapter(&pm).execute(with_system_prompt(run_params(), "system"));
    assert!(result.is_err());
    let record = pm.spawn_record();
    assert!(!system_prompt_file_of(&record).exists());
}

#[tokio::test]
async fn the_handle_reports_the_pid_before_any_event_is_pulled() {
    let pm = ScriptedProcessManager::silent();
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    assert!(handle.pid > 0, "the pid is available synchronously");
    assert_eq!(pm.spawns().len(), 1);
}

#[tokio::test]
async fn an_assistant_event_becomes_output() {
    let collected = run(Script::lines([
        json!({ "type": "assistant", "message": "hello" }).to_string(),
    ]))
    .await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!("hello"));
    assert_eq!(
        collected.first().timestamp,
        orch_core::test_support::FIXED_NOW_ISO
    );
}

#[tokio::test]
async fn a_tool_use_event_becomes_a_tool_call_carrying_the_whole_record() {
    let collected = run(Script::lines([
        json!({ "type": "tool_use", "name": "read" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::ToolCall]);
    assert_eq!(
        collected.first().data,
        json!({ "type": "tool_use", "name": "read" })
    );
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
async fn an_error_event_carries_the_error_payload_and_its_classification() {
    let collected = run(Script::lines([
        json!({ "type": "error", "error": "something broke" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data, json!("something broke"));
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::Unknown)
    );
}

#[tokio::test]
async fn an_unauthorized_error_classifies_as_auth_failed() {
    let collected = run(Script::lines([
        json!({ "type": "error", "error": "Request unauthorized" }).to_string(),
    ]))
    .await;
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::AuthFailed)
    );
}

#[tokio::test]
async fn an_error_event_without_an_error_field_falls_back_to_the_whole_record() {
    let collected = run(Script::lines([json!({ "type": "error" }).to_string()])).await;
    assert_eq!(collected.first().data, json!({ "type": "error" }));
}

#[tokio::test]
async fn a_result_event_becomes_done_with_tokens() {
    let collected = run(Script::lines([
        json!({ "type": "result", "usage": { "input_tokens": 100, "output_tokens": 50 } })
            .to_string(),
    ]))
    .await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(
        collected.first().tokens,
        Some(TokenUsage {
            input: 100,
            output: 50,
            reasoning: 0,
            total: 150,
            cache_read: 0,
            cache_write: 0,
        })
    );
}

#[tokio::test]
async fn claude_is_the_only_adapter_that_reads_usage_out_of_stats() {
    let collected = run(Script::lines([
        json!({ "type": "result", "stats": { "usage": { "input_tokens": 7, "output_tokens": 1 } } })
            .to_string(),
    ]))
    .await;
    assert_eq!(collected.first().tokens.expect("tokens").total, 8);
}

#[tokio::test]
async fn an_unparseable_line_becomes_output_carrying_the_raw_line() {
    let collected = run(Script::new().stdout("not json at all\n")).await;
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!("not json at all"));
}

#[tokio::test]
async fn empty_lines_are_skipped() {
    let collected = run(Script::new().stdout("\n\n").stdout(format!(
        "{}\n",
        json!({ "type": "assistant", "message": "hi" })
    )))
    .await;
    assert_eq!(collected.events.len(), 1);
}

#[tokio::test]
async fn an_unknown_event_type_becomes_output() {
    let collected = run(Script::lines([
        json!({ "type": "unknown_type", "info": "data" }).to_string(),
    ]))
    .await;
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
}

#[tokio::test]
async fn a_non_zero_exit_without_a_done_event_fails_the_run() {
    let collected = run(Script::new().exit(1)).await;
    assert_eq!(
        collected.error_message(),
        "Claude process exited with code 1"
    );
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::ProcessCrash
    );
}

#[tokio::test]
async fn a_done_event_suppresses_the_exit_error() {
    let collected =
        run(Script::lines([json!({ "type": "result", "data": "ok" }).to_string()]).exit(1)).await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
}

#[tokio::test]
async fn a_signal_kill_is_not_an_exit_error() {
    // `exitCode === null` does not throw.
    let collected = run(Script::new().killed_by(9)).await;
    collected.expect_ok();
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn the_stderr_tail_is_appended_to_the_exit_error_and_then_classified() {
    let collected = run(Script::new().stderr("Error: rate limit exceeded\n").exit(1)).await;
    assert_eq!(
        collected.error_message(),
        "Claude process exited with code 1\n\
         --- Claude stderr (tail) ---\n\
         Error: rate limit exceeded"
    );
    // The tail is appended *before* classification, so stderr decides the
    // kind.
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::RateLimit
    );
}

#[tokio::test]
async fn a_cancelled_run_ends_without_an_exit_error() {
    let pm = ScriptedProcessManager::new(Script::new().sleep_ms(50).exit(1));
    let execute_params = run_params();
    execute_params.cancel.cancel();
    let handle = adapter(&pm).execute(execute_params).expect("execute");
    collect(handle).await.expect_ok();
}

#[tokio::test]
async fn cancelling_a_run_signals_the_child() {
    // The shared spawn path signals the child on abort, for every adapter,
    // rather than relying on the orchestrator to remember.
    let pm = ScriptedProcessManager::new(Script::new().sleep_ms(30_000));
    let execute_params = run_params();
    let cancel = execute_params.cancel.clone();
    let handle = adapter(&pm).execute(execute_params).expect("execute");
    let pid = handle.pid;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();

    // The run ends because the child was signalled, not because it finished.
    collect(handle).await.expect_ok();
    assert_eq!(
        pm.killed(),
        [(pid, orch_core::process::Signal::Term)],
        "cancelling must reach the child"
    );
}

#[tokio::test]
async fn the_version_probe_reports_the_version() {
    let pm = ScriptedProcessManager::new(Script::new().stdout("claude/1.0.0\n"));
    let result = adapter(&pm).test().await;
    assert!(result.ok);
    assert_eq!(result.version.as_deref(), Some("claude/1.0.0"));
    assert_eq!(pm.spawn_record().args, ["--version"]);
}

#[tokio::test]
async fn the_version_probe_classifies_a_missing_binary() {
    let pm = ScriptedProcessManager::silent().without(&["claude"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("Claude Code CLI not found. Install: npm i -g @anthropic-ai/claude-code")
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::SpawnFailed));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(5555).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(5555, DEFAULT_KILL_GRACE)]);
}
