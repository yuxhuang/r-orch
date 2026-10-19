//! OpenCode adapter for `kind = "opencode"`.
//!
//! Spawns `opencode run --format json`. There is **no workspace flag**: the
//! workspace reaches the child only through the spawn cwd.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use orch_domain::{TokenUsage, TokenUsageOpts, create_token_usage};
use serde_json::{Map, Value};

use super::util::{
    build_full_prompt, copy_present, error_event, number_field, probe_version, streaming_events,
    stringify, system_clock, version_probe_failure,
};
use crate::process::spawn::{StderrPolicy, spawn_child};

const INSTALL_HINT: &str = "OpenCode CLI not found. Install: npm i -g opencode";

pub struct OpenCodeAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl OpenCodeAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        OpenCodeAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for OpenCodeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeAdapter").finish_non_exhaustive()
    }
}

fn build_args(params: &ExecuteParams) -> Vec<String> {
    let mut args = vec!["run".to_owned(), "--format".to_owned(), "json".to_owned()];

    if let Some(model) = &params.config.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }

    // No native `--system-prompt`; it is prepended to the user prompt.
    args.push(build_full_prompt(
        params.system_prompt.as_deref(),
        &params.prompt,
    ));
    args
}

/// `part.tokens` with a numeric `input`. Unlike the shared `extract_tokens`
/// this one has no camelCase aliases and no cache counters.
fn extract_opencode_tokens(part: &Value) -> Option<TokenUsage> {
    let tokens = part.get("tokens")?;
    let input = number_field(tokens, "input")?;
    Some(create_token_usage(
        input,
        number_field(tokens, "output").unwrap_or(0),
        Some(TokenUsageOpts {
            reasoning: Some(number_field(tokens, "reasoning").unwrap_or(0)),
            ..Default::default()
        }),
    ))
}

fn parse_event(line: &str, clock: &dyn Clock) -> Option<AgentEvent> {
    if line.trim().is_empty() {
        return None;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        return Some(AgentEvent::new(
            AgentEventKind::Output,
            clock.now_iso(),
            Value::String(line.to_owned()),
        ));
    };

    let timestamp = clock.now_iso();
    let part = match parsed.get("part") {
        Some(part) if !part.is_null() => part.clone(),
        _ => Value::Object(Map::new()),
    };

    match parsed.get("type").and_then(Value::as_str).unwrap_or("") {
        // Lifecycle event — no user-visible content.
        "step_start" => None,

        "text" => Some(AgentEvent::new(
            AgentEventKind::Output,
            timestamp,
            match part.get("text") {
                Some(text) if !text.is_null() => text.clone(),
                _ => part,
            },
        )),

        "tool_use" => {
            let state = match part.get("state") {
                Some(state) if !state.is_null() => state.clone(),
                _ => Value::Object(Map::new()),
            };
            if state.get("status").and_then(Value::as_str) == Some("error") {
                let message = match state.get("error") {
                    Some(Value::String(error)) => error.clone(),
                    _ => stringify(&state),
                };
                return Some(error_event(timestamp, state, &message));
            }
            // The `{ name, input }` shape the TUI's `formatToolInput` expects.
            let mut data = Map::new();
            copy_present(&mut data, "name", &part, "tool");
            copy_present(&mut data, "input", &state, "input");
            Some(AgentEvent::new(
                AgentEventKind::ToolCall,
                timestamp,
                Value::Object(data),
            ))
        }

        "step_finish" => {
            let tokens = extract_opencode_tokens(&part);
            let reason = part.get("reason").and_then(Value::as_str);
            match reason {
                Some("error") => {
                    let message = match part.get("error") {
                        Some(Value::String(error)) => error.clone(),
                        _ => stringify(&part),
                    };
                    let mut event = error_event(timestamp, part, &message);
                    event.tokens = tokens;
                    Some(event)
                }
                // Intermediate lifecycle — the tool_use events carry the
                // actual content.
                Some("tool-calls") => None,
                // `stop`, `max_tokens`, anything else, or absent → done.
                _ => {
                    let mut event = AgentEvent::new(AgentEventKind::Done, timestamp, part);
                    event.tokens = tokens;
                    Some(event)
                }
            }
        }

        _ => Some(AgentEvent::new(AgentEventKind::Output, timestamp, parsed)),
    }
}

#[async_trait]
impl Adapter for OpenCodeAdapter {
    fn kind(&self) -> &'static str {
        "opencode"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "opencode").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let args = build_args(&params);
        let io = spawn_child(
            &self.process_manager,
            "opencode",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                stdin: StdioMode::Ignore,
                inherit_env: params.inherit_env,
            },
            StderrPolicy::Tail,
            &params.cancel,
        )?;

        let pid = io.pid;
        let clock = Arc::clone(&self.clock);
        let events = streaming_events(
            io,
            move |line| parse_event(line, clock.as_ref()),
            "OpenCode",
            params.cancel,
        );
        Ok(ExecuteHandle { pid, events })
    }

    async fn stop(&self, pid: u32) -> orch_core::Result<()> {
        self.process_manager
            .kill_with_grace(pid, DEFAULT_KILL_GRACE)
            .await;
        Ok(())
    }
}
