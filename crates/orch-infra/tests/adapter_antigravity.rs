//! Antigravity adapter tests.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager};
use orch_domain::{AdapterErrorKind, AgentConfig};
use orch_infra::adapters::AntigravityAdapter;
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, with_system_prompt};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> AntigravityAdapter {
    AntigravityAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn run_params() -> ExecuteParams {
    params("antigravity prompt", "/tmp/agy-ws")
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_antigravity() {
    assert_eq!(
        adapter(&ScriptedProcessManager::silent()).kind(),
        "antigravity"
    );
}

#[tokio::test]
async fn spawns_agy_with_the_prompt_at_argv_index_one() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "agy");
    assert_eq!(
        record.args,
        [
            "-p",
            "antigravity prompt",
            // The prompt is *not* last.
            "--dangerously-skip-permissions",
        ]
    );
    assert_eq!(record.cwd, std::path::Path::new("/tmp/agy-ws"));
}

#[tokio::test]
async fn prepends_the_system_prompt_and_passes_the_model_last() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(with_system_prompt(
            support::with_config(
                params("user task", "/tmp/agy-ws"),
                AgentConfig {
                    model: Some("gemini-3-pro".to_owned()),
                    ..Default::default()
                },
            ),
            "system instructions",
        ))
        .expect("execute");

    let record = pm.spawn_record();
    assert_eq!(
        record.args,
        [
            "-p",
            "system instructions\n\nuser task",
            "--dangerously-skip-permissions",
            "--model",
            "gemini-3-pro",
        ]
    );
}

#[tokio::test]
async fn falls_back_to_the_agent_system_prompt() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            params("user task", "/tmp/agy-ws"),
            AgentConfig {
                system_prompt: Some("agent prompt".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");
    assert_eq!(pm.spawn_record().args[1], "agent prompt\n\nuser task");
}

#[tokio::test]
async fn streams_plain_stdout_lines_and_emits_done() {
    let collected = run(Script::new().stdout("line one\nline two\n")).await;

    collected.expect_ok();
    assert_eq!(
        collected.kinds(),
        [
            AgentEventKind::Output,
            AgentEventKind::Output,
            AgentEventKind::Done
        ]
    );
    assert_eq!(collected.data()[0], &json!({ "text": "line one" }));
    assert_eq!(collected.data()[1], &json!({ "text": "line two" }));
    assert_eq!(
        collected.data()[2],
        &json!({ "result": "line one\nline two" })
    );
}

#[tokio::test]
async fn an_empty_run_still_emits_done() {
    let collected = run(Script::new()).await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);
    assert_eq!(collected.data()[0], &json!({ "result": "" }));
}

#[tokio::test]
async fn a_non_zero_exit_fails_the_run_even_after_output() {
    // Quirk: antigravity's exit check has no done-event guard, so
    // output does not rescue a non-zero exit.
    let collected = run(Script::new().stdout("some output\n").exit(1)).await;
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(
        collected.error_message(),
        "Antigravity process exited with code 1"
    );
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::ProcessCrash
    );
}

#[tokio::test]
async fn a_signal_kill_still_yields_a_synthetic_done() {
    // The other quirk: `exitCode === null` skips the error check
    // and falls through to the terminal event.
    let collected = run(Script::new().stdout("partial\n").killed_by(9)).await;
    collected.expect_ok();
    assert_eq!(
        collected.kinds(),
        [AgentEventKind::Output, AgentEventKind::Done]
    );
}

#[tokio::test]
async fn antigravity_never_appends_a_stderr_tail_to_its_error() {
    let collected = run(Script::new().stderr("ENOENT somewhere\n").exit(1)).await;
    assert_eq!(
        collected.error_message(),
        "Antigravity process exited with code 1"
    );
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::ProcessCrash
    );
}

#[tokio::test]
async fn a_cancelled_run_emits_no_done_event() {
    let pm = ScriptedProcessManager::new(Script::new().sleep_ms(50));
    let execute_params = run_params();
    execute_params.cancel.cancel();
    let handle = adapter(&pm).execute(execute_params).expect("execute");

    let collected = collect(handle).await;
    collected.expect_ok();
    assert!(collected.events.is_empty());
}

#[tokio::test]
async fn the_version_probe_classifies_a_missing_binary() {
    let pm = ScriptedProcessManager::silent().without(&["agy"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some(
            "Antigravity CLI not found. Install Google Antigravity CLI and ensure `agy` is on PATH."
        )
    );
    assert_eq!(result.error_kind, Some(AdapterErrorKind::SpawnFailed));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(8888).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(8888, DEFAULT_KILL_GRACE)]);
}
