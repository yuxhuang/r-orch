//! V40 / PARITY.md §14.12: every adapter completes a run in which the child
//! writes 256 KiB to stderr.
//!
//! A child that writes past one pipe buffer (~64 KiB on Linux) to an unread
//! stderr blocks forever. In the TS original `grok` and `antigravity` never
//! attach a consumer, so such a run hangs until stall detection fires ten
//! minutes later. Draining lives in the shared spawn path here, so these are
//! regression tests for all eight adapters rather than for two.
//!
//! The flood is written **before** the terminal stdout line, so an adapter
//! that only drained stderr after reading stdout would deadlock and the
//! harness timeout would fail the test.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::ProcessManager;
use orch_domain::AgentConfig;
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use serde_json::json;
use support::{Script, ScriptedProcessManager, collect, fixed_clock, params, stderr_flood};

fn flooding_script(terminal_line: Option<String>) -> Script {
    let script = Script::new().stderr(stderr_flood());
    match terminal_line {
        Some(line) => script.stdout(format!("{line}\n")).exit(0),
        None => script.exit(0),
    }
}

async fn assert_completes(
    adapter: &dyn Adapter,
    pm: &Arc<ScriptedProcessManager>,
    execute_params: ExecuteParams,
) {
    let handle = adapter.execute(execute_params).expect("execute");
    let collected = collect(handle).await;
    collected.expect_ok();
    assert!(
        collected.has_kind(AgentEventKind::Done),
        "{} should still reach a terminal event, got {:?}",
        adapter.kind(),
        collected.kinds()
    );
    assert_eq!(pm.spawns().len(), 1);
}

fn pm_for(terminal_line: Option<String>) -> Arc<ScriptedProcessManager> {
    ScriptedProcessManager::new(flooding_script(terminal_line))
}

#[tokio::test]
async fn claude_survives_a_stderr_flood() {
    let pm = pm_for(Some(json!({ "type": "result" }).to_string()));
    let adapter =
        ClaudeAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn codex_survives_a_stderr_flood() {
    let pm = pm_for(Some(json!({ "type": "turn.completed" }).to_string()));
    let adapter =
        CodexAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn cursor_survives_a_stderr_flood() {
    let pm = pm_for(Some(json!({ "type": "result" }).to_string()));
    let adapter =
        CursorAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn opencode_survives_a_stderr_flood() {
    let pm = pm_for(Some(
        json!({ "type": "step_finish", "part": { "reason": "stop" } }).to_string(),
    ));
    let adapter =
        OpenCodeAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn pi_survives_a_stderr_flood() {
    let pm = pm_for(Some(
        json!({
            "type": "agent_end",
            "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }],
        })
        .to_string(),
    ));
    let adapter =
        PiAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn grok_survives_a_stderr_flood() {
    // grok drains and discards; the terminal event is synthesised on exit 0.
    let pm = pm_for(Some(json!({ "type": "end" }).to_string()));
    let adapter =
        GrokAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn antigravity_survives_a_stderr_flood() {
    let pm = pm_for(Some("all done".to_owned()));
    let adapter = AntigravityAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>)
        .with_clock(fixed_clock());
    assert_completes(&adapter, &pm, params("p", "/tmp")).await;
}

#[tokio::test]
async fn shell_survives_a_stderr_flood() {
    // The shell adapter *streams* stderr rather than draining it, so this
    // asserts the two readers really do run concurrently.
    let pm = pm_for(Some("all done".to_owned()));
    let adapter =
        ShellAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());

    let handle = adapter
        .execute(support::with_config(
            params("p", "/tmp"),
            AgentConfig {
                command: Some("noop".to_owned()),
                ..Default::default()
            },
        ))
        .expect("execute");

    let collected = collect(handle).await;
    collected.expect_ok();
    // Every flood line becomes its own error event, which is this adapter's
    // contract; the point is that the run finishes at all.
    assert!(
        collected.of_kind(AgentEventKind::Error).len() > 1_000,
        "got {} error events",
        collected.of_kind(AgentEventKind::Error).len()
    );
    assert_eq!(collected.of_kind(AgentEventKind::Output).len(), 1);
}
