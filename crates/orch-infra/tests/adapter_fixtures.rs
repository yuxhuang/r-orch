//! V15 — recorded stdout streams → expected `AgentEvent` sequences
//! (ARCHITECTURE §8.3).
//!
//! Each case is a pair in `fixtures/adapters/<adapter>/`: a recorded stdout
//! stream, and the events it must produce. These pin the **whole**
//! `data` shape rather than the single field each assertion looks at — which
//! is what catches a renamed or reordered key.
//!
//! Regenerate with `ORCH_BLESS_FIXTURES=1 cargo test -p orch-infra
//! --test adapter_fixtures`, then read the diff: a change here is a change to
//! the wire contract.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::ProcessManager;
use orch_domain::AgentConfig;
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use serde_json::{Value, json};
use support::{Script, ScriptedProcessManager, collect, fixed_clock, fixture_dir, params};

fn adapter_for(kind: &str, pm: Arc<dyn ProcessManager>) -> Box<dyn Adapter> {
    match kind {
        "claude" => Box::new(ClaudeAdapter::new(pm).with_clock(fixed_clock())),
        "codex" => Box::new(CodexAdapter::new(pm).with_clock(fixed_clock())),
        "cursor" => Box::new(CursorAdapter::new(pm).with_clock(fixed_clock())),
        "opencode" => Box::new(OpenCodeAdapter::new(pm).with_clock(fixed_clock())),
        "pi" => Box::new(PiAdapter::new(pm).with_clock(fixed_clock())),
        "grok" => Box::new(GrokAdapter::new(pm).with_clock(fixed_clock())),
        "antigravity" => Box::new(AntigravityAdapter::new(pm).with_clock(fixed_clock())),
        "shell" => Box::new(ShellAdapter::new(pm).with_clock(fixed_clock())),
        other => panic!("unknown adapter {other}"),
    }
}

fn execute_params(kind: &str) -> ExecuteParams {
    let mut execute_params = params("fixture prompt", "/ws");
    if kind == "shell" {
        execute_params.config = AgentConfig {
            command: Some("fixture".to_owned()),
            ..Default::default()
        };
    }
    execute_params
}

/// The events a recorded stream produces, with the timestamp dropped: it is
/// the clock's, and the per-adapter suites already assert that.
async fn observed(kind: &str, stdout: Vec<u8>, exit: i32) -> Value {
    let pm = ScriptedProcessManager::new(Script::new().stdout(stdout).exit(exit));
    let adapter = adapter_for(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
    let handle = adapter.execute(execute_params(kind)).expect("execute");
    let collected = collect(handle).await;

    let events: Vec<Value> = collected
        .events
        .iter()
        // Rebuilt rather than serialized-and-stripped, so the fixture shows
        // the wire field order (`serde_json::Map::remove` swaps the last entry
        // into the hole).
        .map(|event| {
            let mut value = serde_json::Map::new();
            value.insert("type".to_owned(), json!(event.kind));
            value.insert("data".to_owned(), event.data.clone());
            if let Some(tokens) = event.tokens {
                value.insert("tokens".to_owned(), json!(tokens));
            }
            if let Some(kind) = event.error_kind {
                value.insert("errorKind".to_owned(), json!(kind));
            }
            Value::Object(value)
        })
        .collect();

    json!({
        "events": events,
        "error": collected.error.map(|err| json!({
            "message": err.message,
            "kind": err.kind,
            "exitCode": err.exit_code,
        })),
    })
}

async fn check(kind: &str, case: &str, stream: &str, exit: i32) {
    let dir = fixture_dir(kind);
    let stream_path = dir.join(stream);
    let expected_path = dir.join(format!("{case}.expected.json"));

    let stdout = std::fs::read(&stream_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", stream_path.display()));
    let actual = observed(kind, stdout, exit).await;

    if std::env::var_os("ORCH_BLESS_FIXTURES").is_some() {
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        std::fs::write(
            &expected_path,
            format!("{}\n", serde_json::to_string_pretty(&actual).expect("json")),
        )
        .expect("write fixture");
        // Write, then **fail** (code m8).
        //
        // This used to `return`, so a blessing run reported every fixture
        // test as passing — which means the variable being set anywhere, in
        // CI or a stray shell export, would silently disarm the entire wire
        // contract *and* overwrite the expectations with whatever the code
        // currently produces. The one thing a fixture suite must never do is
        // pass without comparing.
        //
        // Each case is its own `#[tokio::test]`, so every fixture is still
        // regenerated in one run; they simply all report failure.
        panic!(
            "blessed {kind}/{case} — fixtures regenerated, not verified. Review the diff \
             (a change here is a change to the wire contract), then re-run without \
             ORCH_BLESS_FIXTURES to actually compare."
        );
    }

    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|err| panic!("read {}: {err}", expected_path.display())),
    )
    .expect("parse fixture");

    assert_eq!(
        actual,
        expected,
        "{kind}/{case} drifted from its fixture\n\
         actual:\n{}\n",
        serde_json::to_string_pretty(&actual).expect("json")
    );
}

#[tokio::test]
async fn claude_session() {
    check("claude", "session", "session.jsonl", 0).await;
}

#[tokio::test]
async fn claude_errors_and_garbage() {
    check("claude", "errors", "errors.jsonl", 0).await;
}

#[tokio::test]
async fn codex_session() {
    check("codex", "session", "session.jsonl", 0).await;
}

#[tokio::test]
async fn codex_duplicate_provider_error() {
    check("codex", "duplicate-error", "duplicate-error.jsonl", 0).await;
}

#[tokio::test]
async fn cursor_session() {
    check("cursor", "session", "session.jsonl", 0).await;
}

#[tokio::test]
async fn opencode_session() {
    check("opencode", "session", "session.jsonl", 0).await;
}

#[tokio::test]
async fn opencode_tool_error() {
    check("opencode", "tool-error", "tool-error.jsonl", 0).await;
}

#[tokio::test]
async fn pi_session() {
    check("pi", "session", "session.jsonl", 0).await;
}

#[tokio::test]
async fn pi_auto_retry() {
    check("pi", "auto-retry", "auto-retry.jsonl", 0).await;
}

#[tokio::test]
async fn grok_session() {
    check("grok", "session", "session.jsonl", 0).await;
}

#[tokio::test]
async fn antigravity_session() {
    check("antigravity", "session", "session.txt", 0).await;
}

#[tokio::test]
async fn shell_session() {
    check("shell", "session", "session.txt", 0).await;
}

/// A non-zero exit is part of the contract too: the fixture records the
/// terminal error alongside the events that preceded it.
#[tokio::test]
async fn claude_failed_exit() {
    check("claude", "failed-exit", "errors.jsonl", 1).await;
}
