//! V14 — PARITY.md §8.2: every adapter's argv, cwd, stdin mode and
//! self-set environment, snapshotted over a config matrix.
//!
//! The per-adapter suites assert the individual rules ("`--model` is followed
//! by the model", "the prompt is last"); these snapshots pin the whole command
//! line, so a reordering or an extra flag cannot slip through unnoticed.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::ProcessManager;
use orch_domain::{AgentConfig, ReasoningEffort, WorkspaceMode};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use support::{ScriptedProcessManager, SpawnRecord, fixed_clock, params};

/// The two ends of the matrix: nothing configured, and everything configured.
fn matrix() -> Vec<(&'static str, ExecuteParams)> {
    let mut minimal = params("the user prompt", "/ws");
    // Only the shell adapter reads `command`, and it needs one to spawn at
    // all; every other adapter ignores it.
    minimal.config = AgentConfig {
        command: Some("npm test".to_owned()),
        ..Default::default()
    };

    let mut full = params("the user prompt", "/ws");
    full.system_prompt = Some("the system prompt".to_owned());
    full.config = AgentConfig {
        command: Some("npm test".to_owned()),
        model: Some("a-model".to_owned()),
        effort: Some(ReasoningEffort::High),
        max_turns: Some(7),
        system_prompt: Some("the agent prompt".to_owned()),
        workspace_mode: Some(WorkspaceMode::Worktree),
        ..Default::default()
    };

    // Only the agent's own system prompt, to pin the fallback.
    let mut agent_prompt_only = params("the user prompt", "/ws");
    agent_prompt_only.config = AgentConfig {
        command: Some("npm test".to_owned()),
        system_prompt: Some("the agent prompt".to_owned()),
        ..Default::default()
    };

    vec![
        ("minimal", minimal),
        ("full", full),
        ("agent-system-prompt-only", agent_prompt_only),
    ]
}

fn describe(case: &str, record: &SpawnRecord) -> String {
    let mut out = format!("[{case}]\ncommand: {}\n", record.command);
    for (index, arg) in record.args.iter().enumerate() {
        out.push_str(&format!("  argv[{}]: {arg:?}\n", index + 1));
    }
    out.push_str(&format!("cwd: {}\n", record.cwd.display()));
    out.push_str(&format!("stdin: {:?}\n", record.stdin));
    for (key, value) in &record.env {
        out.push_str(&format!("env: {key}={value:?}\n"));
    }
    out
}

/// Run the matrix through one adapter and render every spawn.
fn snapshot_of(build: impl Fn(Arc<dyn ProcessManager>) -> Box<dyn Adapter>) -> String {
    matrix()
        .into_iter()
        .map(|(case, execute_params)| {
            let pm = ScriptedProcessManager::silent();
            let adapter = build(Arc::clone(&pm) as Arc<dyn ProcessManager>);
            adapter.execute(execute_params).expect("execute");
            describe(case, &pm.spawn_record())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn claude_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        ClaudeAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn codex_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        CodexAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn cursor_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        CursorAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn shell_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        ShellAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn opencode_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        OpenCodeAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn pi_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        PiAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn grok_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        GrokAdapter::new(pm).with_clock(fixed_clock())
    )));
}

#[tokio::test]
async fn antigravity_argv() {
    insta::assert_snapshot!(snapshot_of(|pm| Box::new(
        AntigravityAdapter::new(pm).with_clock(fixed_clock())
    )));
}
