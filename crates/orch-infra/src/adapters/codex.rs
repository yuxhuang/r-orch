//! Codex CLI adapter — `src/infrastructure/adapters/codex.ts`,
//! `kind = "codex"` (`docs/parity/adapters.md` §2.2).
//!
//! Spawns `codex exec --json -` and pipes the prompt on stdin, which avoids
//! `ARG_MAX` limits on long prompts.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use orch_domain::{TokenUsage, extract_error_message, is_model_metadata_warning};
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

use super::util::{
    build_full_prompt, classify, extract_tokens, probe_version, streaming_events, system_clock,
    version_probe_failure,
};
use crate::process::spawn::{ChildWriter, StderrPolicy, spawn_child};

const INSTALL_HINT: &str = "Codex CLI not found. Install: npm i -g @openai/codex";

pub struct CodexAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl CodexAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        CodexAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for CodexAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexAdapter").finish_non_exhaustive()
    }
}

fn build_args(params: &ExecuteParams) -> Vec<String> {
    let mut args = vec![
        "exec".to_owned(),
        "--json".to_owned(),
        "--sandbox".to_owned(),
        // Autonomous agents cannot answer an approval prompt.
        "danger-full-access".to_owned(),
    ];

    if let Some(model) = &params.config.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }

    // Read the prompt from stdin.
    args.push("-".to_owned());
    args
}

/// A single write of the combined prompt with **no trailing newline**, then
/// close stdin.
fn write_prompt(stdin: Option<ChildWriter>, prompt: String) {
    let Some(mut stdin) = stdin else {
        return;
    };
    tokio::spawn(async move {
        if let Err(err) = stdin.write_all(prompt.as_bytes()).await {
            tracing::warn!(error = %err, "failed to write the codex prompt to stdin");
            return;
        }
        // `proc.stdin.end()` — codex waits for EOF before it starts.
        if let Err(err) = stdin.shutdown().await {
            tracing::debug!(error = %err, "failed to close codex stdin");
        }
    });
}

/// Codex reports the same provider failure twice (once as an `error` record,
/// once inside `turn.failed`). The dedup is **stateful across lines**, resets
/// on any non-error event, and compares the *unwrapped* message rather than
/// the raw line.
fn event_parser(clock: Arc<dyn Clock>) -> impl FnMut(&str) -> Option<AgentEvent> + Send + 'static {
    let mut last_error_message: Option<String> = None;

    move |line| {
        let event = parse_event(line, clock.as_ref())?;
        if event.kind != AgentEventKind::Error {
            last_error_message = None;
            return Some(event);
        }

        let message = extract_error_message(&event.data);
        if !message.is_empty() && last_error_message.as_deref() == Some(message.as_str()) {
            return None;
        }
        last_error_message = Some(message);
        Some(event)
    }
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
    match parsed.get("type").and_then(Value::as_str).unwrap_or("") {
        // Provider lifecycle noise carries no user-facing information.
        "thread.started" | "turn.started" | "item.started" => None,

        "turn.completed" => {
            let tokens = extract_tokens(&parsed, false);
            let mut data = Map::new();
            // A falsy (empty) result is omitted, as `result ? { result } : {}`.
            if let Some(result) = parsed.get("result").and_then(Value::as_str)
                && !result.is_empty()
            {
                data.insert("result".to_owned(), Value::String(result.to_owned()));
            }
            data.insert("raw".to_owned(), parsed);
            let mut event = AgentEvent::new(AgentEventKind::Done, timestamp, Value::Object(data));
            event.tokens = tokens;
            Some(event)
        }

        "turn.failed" => {
            let tokens = extract_tokens(&parsed, false);
            let value = coalesce_error(&parsed);
            Some(error_or_warning(timestamp, parsed, &value, tokens))
        }

        "item.completed" => {
            let item = match parsed.get("item") {
                Some(item) if !item.is_null() => item.clone(),
                _ => Value::Object(Map::new()),
            };
            parse_item(timestamp, item)
        }

        "error" => {
            let value = coalesce_error(&parsed);
            Some(error_or_warning(timestamp, parsed, &value, None))
        }

        _ => text_output(timestamp, &parsed, parsed.clone()),
    }
}

fn parse_item(timestamp: String, item: Value) -> Option<AgentEvent> {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    match item_type.as_str() {
        "agent_message" => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return None;
            }
            Some(AgentEvent::new(
                AgentEventKind::Output,
                timestamp,
                json_with_raw([("text", Value::String(text.to_owned()))], &item),
            ))
        }

        "reasoning" => None,

        "command_execution" => {
            let command = item.get("command").and_then(Value::as_str).unwrap_or("");
            if command.is_empty() {
                return None;
            }
            let mut fields = vec![("command", Value::String(command.to_owned()))];
            if let Some(result) = item.get("aggregated_output").and_then(Value::as_str) {
                fields.push(("result", Value::String(result.to_owned())));
            }
            Some(AgentEvent::new(
                AgentEventKind::Command,
                timestamp,
                json_with_raw(fields, &item),
            ))
        }

        "file_change" => {
            let paths: Vec<Value> = item
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|change| change.get("path").and_then(Value::as_str))
                        .filter(|path| !path.is_empty())
                        .map(|path| Value::String(path.to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            Some(AgentEvent::new(
                AgentEventKind::FileChange,
                timestamp,
                json_with_raw([("paths", Value::Array(paths))], &item),
            ))
        }

        "tool_use" | "mcp_tool_call" => {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| item.get("tool").and_then(Value::as_str))
                .unwrap_or(&item_type)
                .to_owned();
            let mut fields = vec![("name", Value::String(name))];
            // `'input' in item` — presence, so an explicit null survives.
            if let Some(input) = item.get("input") {
                fields.push(("input", input.clone()));
            }
            Some(AgentEvent::new(
                AgentEventKind::ToolCall,
                timestamp,
                json_with_raw(fields, &item),
            ))
        }

        "web_search" => {
            let mut input = Map::new();
            if let Some(query) = item.get("query").and_then(Value::as_str) {
                input.insert("query".to_owned(), Value::String(query.to_owned()));
            }
            if let Some(action) = item.get("action") {
                input.insert("action".to_owned(), action.clone());
            }
            Some(AgentEvent::new(
                AgentEventKind::ToolCall,
                timestamp,
                json_with_raw(
                    [
                        ("name", Value::String("web_search".to_owned())),
                        ("input", Value::Object(input)),
                    ],
                    &item,
                ),
            ))
        }

        "tool_result" => text_output(timestamp, &item, item.clone()),

        "error" => Some(error_or_warning(timestamp, item.clone(), &item, None)),

        _ => text_output(timestamp, &item, item.clone()),
    }
}

/// `{ …fields, raw }` — `raw` last, matching the TS object literals.
fn json_with_raw<I>(fields: I, raw: &Value) -> Value
where
    I: IntoIterator<Item = (&'static str, Value)>,
{
    let mut data = Map::new();
    for (key, value) in fields {
        data.insert(key.to_owned(), value);
    }
    data.insert("raw".to_owned(), raw.clone());
    Value::Object(data)
}

/// `parsed.error ?? parsed`.
fn coalesce_error(parsed: &Value) -> Value {
    match parsed.get("error") {
        Some(error) if !error.is_null() => error.clone(),
        _ => parsed.clone(),
    }
}

/// `item.text`, else `item.content`, else `item.message`, if a string.
fn extract_item_text(item: &Value) -> Option<&str> {
    for key in ["text", "content", "message"] {
        if let Some(Value::String(text)) = item.get(key) {
            return Some(text);
        }
    }
    None
}

fn text_output(timestamp: String, source: &Value, raw: Value) -> Option<AgentEvent> {
    let text = extract_item_text(source).filter(|text| !text.is_empty())?;
    Some(AgentEvent::new(
        AgentEventKind::Output,
        timestamp,
        json_with_raw([("text", Value::String(text.to_owned()))], &raw),
    ))
}

/// Codex reports a recoverable model-metadata cache miss as an error item;
/// that one case becomes an `output` event with a `⚠` prefix instead.
fn error_or_warning(
    timestamp: String,
    raw: Value,
    value: &Value,
    tokens: Option<TokenUsage>,
) -> AgentEvent {
    let message = extract_error_message(value);
    if is_model_metadata_warning(&message) {
        return AgentEvent::new(
            AgentEventKind::Output,
            timestamp,
            json_with_raw([("text", Value::String(format!("⚠ {message}")))], &raw),
        );
    }

    let mut event = AgentEvent::new(
        AgentEventKind::Error,
        timestamp,
        json_with_raw([("message", Value::String(message.clone()))], &raw),
    )
    .with_error_kind(classify(&message));
    event.tokens = tokens;
    event
}

#[async_trait]
impl Adapter for CodexAdapter {
    fn kind(&self) -> &'static str {
        "codex"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "codex").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let args = build_args(&params);
        let mut io = spawn_child(
            &self.process_manager,
            "codex",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                // stdin must be a pipe to carry the prompt.
                stdin: StdioMode::Pipe,
                inherit_env: params.inherit_env,
            },
            StderrPolicy::Tail,
            &params.cancel,
        )?;

        // Codex has no native `--system-prompt`, so it is prepended.
        write_prompt(
            io.stdin.take(),
            build_full_prompt(params.system_prompt.as_deref(), &params.prompt),
        );

        let pid = io.pid;
        let events = streaming_events(
            io,
            event_parser(Arc::clone(&self.clock)),
            "Codex",
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
