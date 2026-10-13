//! Ported from `test/unit/infrastructure/shell-adapter.test.ts`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager};
use orch_domain::{AdapterErrorKind, AgentConfig};
use orch_infra::adapters::ShellAdapter;
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, with_system_prompt};

fn adapter(pm: &Arc<ScriptedProcessManager>) -> ShellAdapter {
    ShellAdapter::new(Arc::clone(pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock())
}

fn config(command: &str) -> AgentConfig {
    AgentConfig {
        command: Some(command.to_owned()),
        ..Default::default()
    }
}

fn run_params() -> ExecuteParams {
    support::with_config(params("test prompt", "/tmp"), config("echo hello"))
}

async fn run(script: Script) -> support::Collected {
    let pm = ScriptedProcessManager::new(script);
    let handle = adapter(&pm).execute(run_params()).expect("execute");
    collect(handle).await
}

#[test]
fn the_kind_is_shell() {
    assert_eq!(adapter(&ScriptedProcessManager::silent()).kind(), "shell");
}

#[tokio::test]
async fn spawns_the_configured_command_through_bash_lc() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).execute(run_params()).expect("execute");

    let record = pm.spawn_record();
    assert_eq!(record.command, "bash");
    assert_eq!(record.args, ["-lc", "echo hello"]);
    assert_eq!(record.cwd, std::path::Path::new("/tmp"));
}

#[tokio::test]
async fn passes_the_prompt_in_orchestry_task_prompt() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(support::with_config(
            params("user task", "/tmp"),
            config("echo hello"),
        ))
        .expect("execute");
    assert_eq!(
        pm.spawn_record()
            .env
            .get("ORCHESTRY_TASK_PROMPT")
            .map(String::as_str),
        Some("user task")
    );
}

#[tokio::test]
async fn the_system_prompt_is_joined_with_a_double_newline() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm)
        .execute(with_system_prompt(
            support::with_config(params("user task", "/tmp"), config("echo hello")),
            "be concise",
        ))
        .expect("execute");
    assert_eq!(
        pm.spawn_record()
            .env
            .get("ORCHESTRY_TASK_PROMPT")
            .map(String::as_str),
        Some("be concise\n\nuser task")
    );
}

#[tokio::test]
async fn stdout_lines_become_output_events() {
    let collected = run(Script::new().stdout("hello world\n")).await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Output]);
    assert_eq!(collected.first().data, json!("hello world"));
}

#[tokio::test]
async fn stderr_lines_become_classified_error_events() {
    let collected = run(Script::new().stderr("something went wrong\n")).await;
    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(collected.first().data, json!("something went wrong"));
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::Unknown)
    );
}

#[tokio::test]
async fn a_stderr_line_mentioning_a_rate_limit_classifies_as_one() {
    let collected = run(Script::new().stderr("API rate limit reached\n")).await;
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::RateLimit)
    );
}

#[tokio::test]
async fn both_streams_are_read_concurrently() {
    let collected = run(Script::new()
        .stdout("out one\n")
        .stderr("err one\n")
        .stdout("out two\n"))
    .await;

    collected.expect_ok();
    assert_eq!(collected.of_kind(AgentEventKind::Output).len(), 2);
    assert_eq!(collected.of_kind(AgentEventKind::Error).len(), 1);
}

#[tokio::test]
async fn a_non_zero_exit_fails_the_run_after_its_output_is_drained() {
    let collected = run(Script::new().stderr("intentional failure\n").exit(7)).await;

    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
    assert_eq!(
        collected.error_message(),
        "Shell command exited with code 7"
    );
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::ProcessCrash
    );
}

#[tokio::test]
async fn a_signal_kill_fails_the_run_naming_the_signal() {
    // Unlike `createStreamingEvents`, the shell adapter treats a signal kill
    // as a failure.
    let collected = run(Script::new().killed_by(15)).await;
    assert_eq!(
        collected.error_message(),
        "Shell command terminated by signal SIGTERM"
    );
}

#[tokio::test]
async fn a_zero_exit_with_output_succeeds() {
    let collected = run(Script::new().stdout("done\n").exit(0)).await;
    collected.expect_ok();
}

#[tokio::test]
async fn a_missing_command_yields_one_error_event_and_pid_zero() {
    let pm = ScriptedProcessManager::silent();
    let handle = adapter(&pm)
        .execute(params("test prompt", "/tmp"))
        .expect("execute");
    assert_eq!(handle.pid, 0);

    let collected = collect(handle).await;
    collected.expect_ok();
    assert_eq!(collected.events.len(), 1);
    assert_eq!(collected.first().kind, AgentEventKind::Error);
    assert_eq!(
        collected.first().data,
        json!("Shell adapter requires a command in agent config")
    );
    assert_eq!(
        collected.first().error_kind,
        Some(AdapterErrorKind::SpawnFailed)
    );
    assert!(pm.spawns().is_empty(), "nothing is spawned");
}

#[tokio::test]
async fn a_cancelled_run_reaps_the_child_and_reports_nothing() {
    let pm = ScriptedProcessManager::new(Script::new().stdout("before\n").sleep_ms(30_000));
    let execute_params = run_params();
    let cancel = execute_params.cancel.clone();
    let handle = adapter(&pm).execute(execute_params).expect("execute");
    let pid = handle.pid;

    // Let the first line through, then abort.
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let collected = collect(handle).await;
    collected.expect_ok();
    assert_eq!(
        pm.killed_with_grace(),
        [(pid, Duration::from_millis(5_000))],
        "the abort handler kills with a 5 s grace"
    );
}

#[tokio::test]
async fn the_version_probe_reports_only_the_first_line_of_bash_version() {
    let pm = ScriptedProcessManager::new(
        Script::new().stdout("GNU bash, version 5.2.15(1)-release\nCopyright (C) 2022\n"),
    );
    let result = adapter(&pm).test().await;
    assert!(result.ok);
    assert_eq!(
        result.version.as_deref(),
        Some("GNU bash, version 5.2.15(1)-release")
    );
}

#[tokio::test]
async fn a_missing_bash_reports_bash_not_found() {
    let pm = ScriptedProcessManager::silent().without(&["bash"]);
    let result = adapter(&pm).test().await;
    assert!(!result.ok);
    assert_eq!(result.error.as_deref(), Some("bash not found"));
    // The fixed message is what gets classified, so "not found" wins.
    assert_eq!(result.error_kind, Some(AdapterErrorKind::AdapterNotFound));
}

#[tokio::test]
async fn stop_kills_with_the_default_grace() {
    let pm = ScriptedProcessManager::silent();
    adapter(&pm).stop(9999).await.expect("stop");
    assert_eq!(pm.killed_with_grace(), [(9999, DEFAULT_KILL_GRACE)]);
}
