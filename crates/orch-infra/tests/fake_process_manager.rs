//! What TRAITS-FROZEN Amendment 1 bought: `FakeProcessManager` can now drive
//! a real adapter's event stream and observe its stdin, with no child process
//! anywhere.
//!
//! **WP-4: this is the pattern.** The orchestrator tests need an adapter that
//! produces events on demand; before the amendment the fake could only hand
//! back `None` for all three pipes, so the only option was `FakeAdapter`,
//! which bypasses the parsing under test.
//!
//! The adapter suites still run against scripted *real* children
//! (`tests/support/mod.rs`), because that is what exercises pipes, decoding
//! and reaping. This file covers the seam itself.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind};
use orch_core::process::{ExitOutcome, ProcessManager};
use orch_core::test_support::{FAKE_PID, FakeProcessManager};
use orch_domain::{AdapterErrorKind, AgentConfig};
use orch_infra::adapters::{ClaudeAdapter, CodexAdapter, PiAdapter};
use serde_json::{Value, json};
use support::{collect, fixed_clock, params};

fn fake() -> Arc<FakeProcessManager> {
    Arc::new(FakeProcessManager::new())
}

#[tokio::test]
async fn a_scripted_stdout_drives_a_real_adapter_to_its_events() {
    let pm =
        Arc::new(
            FakeProcessManager::new().with_stdout_lines([
                json!({ "type": "assistant", "message": "hello" }).to_string(),
                json!({ "type": "tool_use", "name": "read" }).to_string(),
                json!({ "type": "result", "usage": { "input_tokens": 3, "output_tokens": 1 } })
                    .to_string(),
            ]),
        );

    let adapter =
        ClaudeAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    let handle = adapter.execute(params("p", "/ws")).expect("execute");

    assert_eq!(handle.pid, FAKE_PID, "the pid is available synchronously");

    let collected = collect(handle).await;
    collected.expect_ok();
    assert_eq!(
        collected.kinds(),
        [
            AgentEventKind::Output,
            AgentEventKind::ToolCall,
            AgentEventKind::Done
        ]
    );
    assert_eq!(collected.first().data, json!("hello"));
    assert_eq!(collected.last().tokens.expect("tokens").total, 4);
    assert_eq!(pm.spawned().len(), 1);
}

#[tokio::test]
async fn a_scripted_exit_and_stderr_produce_the_classified_failure() {
    let pm = Arc::new(
        FakeProcessManager::new()
            .with_stderr("Error: rate limit exceeded\n")
            .with_exit(ExitOutcome {
                code: Some(1),
                signal: None,
            }),
    );

    let adapter = ClaudeAdapter::new(pm as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    let collected = collect(adapter.execute(params("p", "/ws")).expect("execute")).await;

    assert_eq!(
        collected.error_message(),
        "Claude process exited with code 1\n\
         --- Claude stderr (tail) ---\n\
         Error: rate limit exceeded"
    );
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::RateLimit
    );
}

#[tokio::test]
async fn codex_stdin_is_observable() {
    let pm = fake();
    let adapter =
        CodexAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());

    let mut execute_params = params("user task", "/ws");
    execute_params.system_prompt = Some("system instructions".to_owned());
    collect(adapter.execute(execute_params).expect("execute"))
        .await
        .expect_ok();

    // One write of the combined prompt, no trailing newline.
    assert_eq!(pm.stdin_written().await, "system instructions\n\nuser task");
}

#[tokio::test]
async fn pi_stdin_carries_one_jsonl_prompt_frame() {
    let pm = Arc::new(
        FakeProcessManager::new().with_stdout_lines([json!({
            "type": "agent_end",
            "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }],
        })
        .to_string()]),
    );

    let adapter =
        PiAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>).with_clock(fixed_clock());
    let collected = collect(adapter.execute(params("do work", "/ws")).expect("execute")).await;

    collected.expect_ok();
    assert_eq!(collected.kinds(), [AgentEventKind::Done]);

    let written = pm.stdin_written().await;
    assert!(written.ends_with('\n'), "{written:?}");
    let frame: Value = serde_json::from_str(written.trim_end()).expect("one frame");
    assert_eq!(frame["type"], json!("prompt"));
    assert_eq!(frame["message"], json!("do work"));

    // The adapter reaps the long-lived session itself.
    assert_eq!(
        pm.killed_with_grace(),
        [(FAKE_PID, std::time::Duration::from_millis(1_000))]
    );
}

#[tokio::test]
async fn the_shell_adapter_still_needs_no_child_at_all_without_a_command() {
    let pm = fake();
    let adapter =
        orch_infra::adapters::ShellAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>)
            .with_clock(fixed_clock());

    let handle = adapter
        .execute(support::with_config(
            params("p", "/ws"),
            AgentConfig::default(),
        ))
        .expect("execute");
    assert_eq!(handle.pid, 0);
    assert!(pm.spawned().is_empty());

    let collected = collect(handle).await;
    assert_eq!(collected.kinds(), [AgentEventKind::Error]);
}
