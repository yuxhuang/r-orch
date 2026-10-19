//! Pi coding-agent adapter for `kind = "pi"`.
//!
//! Spawns `pi --mode rpc`, sends the ORCH prompt as one JSONL `prompt` frame
//! on stdin and maps Pi's RPC events onto `AgentEvent`s. Three things make it
//! unlike every other adapter:
//!
//! 1. **stdin stays open.** `--mode rpc` is a long-lived session: it answers
//!    the prompt with a preflight response and then drives the LLM call
//!    asynchronously. Closing stdin stalls it right after the user
//!    `message_end` (verified on pi-coding-agent 0.73.1), so the adapter
//!    terminates the process itself with a **1000 ms** grace once a terminal
//!    event arrives.
//! 2. **Its own line reader** ([`read_pi_lines`]): no 16 KB cap, because
//!    `agent_end` carries the whole transcript, and a trailing `\r` is
//!    stripped.
//! 3. **Unknown event types are dropped**, where every other adapter defaults
//!    to an `output` event — raw RPC envelopes would only pollute the logs.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::StreamExt;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use orch_domain::{AdapterErrorKind, TokenUsage, TokenUsageOpts, create_token_usage};
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::util::{
    append_stderr_tail, classify, copy_present, error_event, number_field, probe_version,
    stringify, system_clock, version_probe_failure,
};
use crate::process::line_reader::read_pi_lines;
use crate::process::spawn::{ChildIo, ChildWriter, StderrPolicy, spawn_child};

const INSTALL_HINT: &str = "Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent";

/// Pi is long-lived, so the adapter reaps it as soon as the run is over.
const TERMINAL_GRACE: Duration = Duration::from_millis(1_000);

/// Pi exposes usage under several key shapes across versions and across the
/// `message_*` and `agent_end` paths: Pi-native names, snake_case shims, and
/// Anthropic-style names when it forwards a Claude usage record unchanged.
/// First match per field wins, in this order.
const TOKEN_ALIASES: [(&str, &[&str]); 5] = [
    ("input", &["input", "input_tokens"]),
    ("output", &["output", "output_tokens"]),
    ("reasoning", &["reasoning", "reasoning_tokens"]),
    (
        "cache_read",
        &["cacheRead", "cache_read", "cache_read_input_tokens"],
    ),
    (
        "cache_write",
        &["cacheWrite", "cache_write", "cache_creation_input_tokens"],
    ),
];

/// Pi lifecycle events that carry no content but may carry usage.
const PASSIVE_TYPES: [&str; 11] = [
    "extension_ui_request",
    "agent_start",
    "turn_start",
    "message_start",
    "message_end",
    "turn_end",
    "queue_update",
    "compaction_start",
    "compaction_end",
    "auto_retry_start",
    "auto_retry_end",
];

pub struct PiAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl PiAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        PiAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for PiAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiAdapter").finish_non_exhaustive()
    }
}

fn build_args(params: &ExecuteParams) -> Vec<String> {
    let mut args = vec!["--mode".to_owned(), "rpc".to_owned()];

    if let Some(model) = &params.config.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = params.config.effort {
        args.push("--thinking".to_owned());
        args.push(effort.as_str().to_owned());
    }

    // Pi's own coding-agent harness prompt is preserved: ORCH's system prompt
    // is *appended* as extra project/task governance, never `--system-prompt`.
    if let Some(system_prompt) = params
        .system_prompt
        .as_deref()
        .or(params.config.system_prompt.as_deref())
    {
        args.push("--append-system-prompt".to_owned());
        args.push(system_prompt.to_owned());
    }
    args
}

/// The stdin handle, shared between the prompt writer and the event stream.
/// Holding it in the stream is what keeps stdin **open** for the session; it
/// closes when the stream is dropped, not after the write.
type SharedStdin = Arc<tokio::sync::Mutex<Option<ChildWriter>>>;

/// One JSONL RPC frame, then **no** `end()`: see the module docs.
fn write_prompt(stdin: SharedStdin, prompt: &str, now_ms: i64) {
    let frame = format!(
        "{}\n",
        serde_json::json!({
            "id": format!("orch-{now_ms}"),
            "type": "prompt",
            "message": prompt,
        })
    );
    tokio::spawn(async move {
        let mut guard = stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return;
        };
        if let Err(err) = stdin.write_all(frame.as_bytes()).await {
            tracing::warn!(error = %err, "failed to write the pi prompt frame");
            return;
        }
        if let Err(err) = stdin.flush().await {
            tracing::debug!(error = %err, "failed to flush pi stdin");
        }
    });
}

#[derive(Debug, Clone)]
struct PiFailure {
    message: String,
    kind: AdapterErrorKind,
}

#[derive(Debug, Clone)]
struct PiCompletion {
    result: String,
    raw: Value,
    tokens: Option<TokenUsage>,
    /// Current Pi emits `agent_settled` after `agent_end` and exposes
    /// `willRetry` on `agent_end`. Keyed off **presence**, not value, so
    /// `willRetry: false` still waits — older Pi versions
    /// have neither field, where `agent_end` is itself terminal.
    wait_for_settled: bool,
    failure: Option<PiFailure>,
}

#[derive(Default)]
struct ParsedPiEvent {
    agent_event: Option<AgentEvent>,
    final_text: Option<String>,
    tokens: Option<TokenUsage>,
    completion: Option<PiCompletion>,
    settled: bool,
}

struct ParseState {
    final_text: String,
    last_tokens: Option<TokenUsage>,
}

fn parse_rpc_event(line: &str, clock: &dyn Clock, state: &ParseState) -> Option<ParsedPiEvent> {
    if line.trim().is_empty() {
        return None;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        return Some(ParsedPiEvent {
            agent_event: Some(AgentEvent::new(
                AgentEventKind::Output,
                clock.now_iso(),
                serde_json::json!({ "text": line }),
            )),
            ..Default::default()
        });
    };

    let timestamp = clock.now_iso();
    let event_type = parsed.get("type").and_then(Value::as_str).unwrap_or("");

    if PASSIVE_TYPES.contains(&event_type) {
        // Passive, but `message_end`/`turn_end` carry the running usage.
        return extract_tokens_from_message(&parsed).map(|tokens| ParsedPiEvent {
            tokens: Some(tokens),
            ..Default::default()
        });
    }

    match event_type {
        "response" => {
            if parsed.get("success") == Some(&Value::Bool(false)) {
                let message = match parsed.get("error") {
                    Some(Value::String(error)) => error.clone(),
                    _ => stringify(&parsed),
                };
                return Some(ParsedPiEvent {
                    completion: Some(PiCompletion {
                        result: String::new(),
                        raw: parsed,
                        tokens: None,
                        wait_for_settled: false,
                        failure: Some(PiFailure {
                            kind: classify(&message),
                            message,
                        }),
                    }),
                    ..Default::default()
                });
            }
            None
        }

        "message_update" => parse_message_update(&parsed, timestamp, state),

        "tool_execution_start" => {
            let mut data = Map::new();
            copy_present(&mut data, "name", &parsed, "toolName");
            copy_present(&mut data, "input", &parsed, "args");
            data.insert("raw".to_owned(), parsed);
            Some(ParsedPiEvent {
                agent_event: Some(AgentEvent::new(
                    AgentEventKind::ToolCall,
                    timestamp,
                    Value::Object(data),
                )),
                ..Default::default()
            })
        }

        // Intermediate progress: one per chunk of tool output. No other
        // adapter surfaces this, so it is dropped.
        "tool_execution_update" => None,

        "tool_execution_end" => Some(parse_tool_execution_end(&parsed, timestamp)),

        "agent_end" => {
            let completion = parse_completion(&parsed, state);
            Some(ParsedPiEvent {
                final_text: Some(completion.result.clone()),
                tokens: completion.tokens,
                completion: Some(completion),
                ..Default::default()
            })
        }

        "agent_settled" => Some(ParsedPiEvent {
            settled: true,
            ..Default::default()
        }),

        "extension_error" => {
            let message = match parsed.get("message") {
                Some(Value::String(message)) => message.clone(),
                _ => stringify(&parsed),
            };
            let data = message_with_raw(&message, &parsed);
            Some(ParsedPiEvent {
                agent_event: Some(error_event(timestamp, data, &message)),
                ..Default::default()
            })
        }

        // Unknown types are dropped; adding a known type above is how a new
        // Pi event gets surfaced.
        _ => None,
    }
}

fn parse_message_update(
    parsed: &Value,
    timestamp: String,
    state: &ParseState,
) -> Option<ParsedPiEvent> {
    let message_event = parsed.get("assistantMessageEvent");
    let update_type = message_event
        .and_then(|event| event.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");

    match update_type {
        // Deltas are aggregated silently: a long response is hundreds of them,
        // and one event per delta would flood the activity feed.
        "text_delta" => {
            let delta = message_event
                .and_then(|event| event.get("delta"))
                .and_then(Value::as_str)
                .unwrap_or("");
            Some(ParsedPiEvent {
                final_text: Some(format!("{}{delta}", state.final_text)),
                ..Default::default()
            })
        }

        "text_end" => {
            let content = message_event
                .and_then(|event| event.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| state.final_text.clone());

            // Reset the delta buffer either way, so a follow-up assistant
            // message in the same session is not concatenated onto this one.
            if content.is_empty() {
                return Some(ParsedPiEvent {
                    final_text: Some(String::new()),
                    ..Default::default()
                });
            }
            Some(ParsedPiEvent {
                final_text: Some(String::new()),
                agent_event: Some(AgentEvent::new(
                    AgentEventKind::Output,
                    timestamp,
                    serde_json::json!({ "text": content }),
                )),
                ..Default::default()
            })
        }

        "error" => {
            let message = message_event
                .and_then(|event| event.get("error"))
                .and_then(pi_message_error)
                .or_else(|| parsed.get("message").and_then(pi_message_error))
                .or_else(|| {
                    message_event
                        .and_then(|event| event.get("reason"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| stringify(parsed));
            let data = message_with_raw(&message, parsed);
            Some(ParsedPiEvent {
                agent_event: Some(error_event(timestamp, data, &message)),
                ..Default::default()
            })
        }

        _ => None,
    }
}

fn parse_tool_execution_end(parsed: &Value, timestamp: String) -> ParsedPiEvent {
    let tool_name = parsed
        .get("toolName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let args = parsed.get("args");
    let result_text = tool_result_text(parsed.get("result"));

    let event = if parsed.get("isError") == Some(&Value::Bool(true)) {
        let message = if result_text.is_empty() {
            stringify(parsed.get("result").unwrap_or(parsed))
        } else {
            result_text.clone()
        };
        error_event(timestamp, message_with_raw(&message, parsed), &message)
    } else if tool_name == "bash" {
        let command = match args.and_then(|args| args.get("command")) {
            Some(Value::String(command)) => command.clone(),
            // `JSON.stringify(args ?? {})`.
            _ => stringify(args.unwrap_or(&Value::Object(Map::new()))),
        };
        let mut data = Map::new();
        data.insert("command".to_owned(), Value::String(command));
        data.insert("result".to_owned(), Value::String(result_text));
        data.insert("raw".to_owned(), parsed.clone());
        AgentEvent::new(AgentEventKind::Command, timestamp, Value::Object(data))
    } else if tool_name.eq_ignore_ascii_case("write") || tool_name.eq_ignore_ascii_case("edit") {
        match extract_path(args) {
            Some(path) => {
                let mut data = Map::new();
                data.insert("paths".to_owned(), serde_json::json!([path]));
                data.insert("raw".to_owned(), parsed.clone());
                AgentEvent::new(AgentEventKind::FileChange, timestamp, Value::Object(data))
            }
            // No path to report: fall through to the generic summary.
            None => generic_tool_output(&tool_name, &result_text, parsed, timestamp),
        }
    } else {
        generic_tool_output(&tool_name, &result_text, parsed, timestamp)
    };

    ParsedPiEvent {
        agent_event: Some(event),
        ..Default::default()
    }
}

/// Generic tool result (read, grep, ls, …) — surface the result text so the
/// logs show what came back rather than the raw RPC envelope.
fn generic_tool_output(
    tool_name: &str,
    result_text: &str,
    parsed: &Value,
    timestamp: String,
) -> AgentEvent {
    let summary = if result_text.is_empty() {
        let name = if tool_name.is_empty() {
            "tool"
        } else {
            tool_name
        };
        format!("{name} completed")
    } else {
        result_text.to_owned()
    };
    let mut data = Map::new();
    data.insert("text".to_owned(), Value::String(summary));
    data.insert("raw".to_owned(), parsed.clone());
    AgentEvent::new(AgentEventKind::Output, timestamp, Value::Object(data))
}

fn parse_completion(parsed: &Value, state: &ParseState) -> PiCompletion {
    let assistant_message = last_assistant_message(parsed);
    let result = assistant_message
        .and_then(|message| text_from_content(message.get("content")))
        .unwrap_or_else(|| state.final_text.clone());
    let tokens = assistant_message
        .and_then(extract_tokens_from_message)
        .or(state.last_tokens);
    let stop_reason = assistant_message
        .and_then(|message| message.get("stopReason"))
        .and_then(Value::as_str);
    let error_message = assistant_message
        .and_then(|message| message.get("errorMessage"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");

    let failure = match stop_reason {
        Some(reason @ ("error" | "aborted")) => {
            let message = if !error_message.is_empty() {
                error_message.to_owned()
            } else if !result.is_empty() {
                result.clone()
            } else {
                format!("Pi agent stopped with reason: {reason}")
            };
            Some(PiFailure {
                kind: classify(&message),
                message,
            })
        }
        _ => None,
    };

    PiCompletion {
        result,
        raw: parsed.clone(),
        tokens,
        wait_for_settled: parsed
            .as_object()
            .is_some_and(|object| object.contains_key("willRetry")),
        failure,
    }
}

/// The terminal event for a completion, plus the error that must be raised
/// after it: a provider failure is deliberately *both* yielded as an `error`
/// event and thrown, so the orchestrator's failure path handles retry rather
/// than seeing a successful run.
fn terminal_event(
    completion: &PiCompletion,
    timestamp: String,
) -> (AgentEvent, Option<AdapterError>) {
    match &completion.failure {
        Some(failure) => {
            let data = message_with_raw(&failure.message, &completion.raw);
            let event = AgentEvent::new(AgentEventKind::Error, timestamp, data)
                .with_error_kind(failure.kind);
            (
                event,
                Some(AdapterError::with_kind(
                    failure.message.clone(),
                    failure.kind,
                    None,
                )),
            )
        }
        None => {
            let mut data = Map::new();
            data.insert(
                "result".to_owned(),
                Value::String(completion.result.clone()),
            );
            data.insert("raw".to_owned(), completion.raw.clone());
            let mut event = AgentEvent::new(AgentEventKind::Done, timestamp, Value::Object(data));
            event.tokens = completion.tokens;
            (event, None)
        }
    }
}

fn message_with_raw(message: &str, raw: &Value) -> Value {
    let mut data = Map::new();
    data.insert("message".to_owned(), Value::String(message.to_owned()));
    data.insert("raw".to_owned(), raw.clone());
    Value::Object(data)
}

fn last_assistant_message(parsed: &Value) -> Option<&Value> {
    parsed
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
}

/// An object with a non-blank string `errorMessage`, trimmed.
fn pi_message_error(value: &Value) -> Option<String> {
    let message = value.get("errorMessage")?.as_str()?.trim();
    (!message.is_empty()).then(|| message.to_owned())
}

/// Pi tool results wrap content arrays in `{ content: [...] }`; messages
/// don't.
fn tool_result_text(result: Option<&Value>) -> String {
    match result {
        Some(Value::String(text)) => text.clone(),
        Some(value) if value.is_object() || value.is_array() => {
            text_from_content(value.get("content")).unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn text_from_content(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let joined: String = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect();
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn extract_path(args: Option<&Value>) -> Option<String> {
    let args = args?;
    for key in ["path", "file_path"] {
        if let Some(Value::String(path)) = args.get(key) {
            return Some(path.clone());
        }
    }
    None
}

/// Unlike the shared extractor, every field defaults to 0; the whole usage is
/// dropped only when all five are 0.
fn extract_tokens_from_message(parsed: &Value) -> Option<TokenUsage> {
    let usage = parsed.get("usage")?;
    if usage.is_null() {
        return None;
    }

    let pick = |field: &str| -> u64 {
        let aliases = TOKEN_ALIASES
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, aliases)| *aliases)
            .unwrap_or(&[]);
        for key in aliases {
            if let Some(found) = number_field(usage, key) {
                return found;
            }
        }
        0
    };

    let (input, output, reasoning, cache_read, cache_write) = (
        pick("input"),
        pick("output"),
        pick("reasoning"),
        pick("cache_read"),
        pick("cache_write"),
    );
    if input == 0 && output == 0 && reasoning == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }
    Some(create_token_usage(
        input,
        output,
        Some(TokenUsageOpts {
            reasoning: Some(reasoning),
            cache_read: Some(cache_read),
            cache_write: Some(cache_write),
        }),
    ))
}

fn pi_events(
    io: ChildIo,
    stdin: SharedStdin,
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
) -> futures::stream::BoxStream<'static, Result<AgentEvent, AdapterError>> {
    let ChildIo {
        pid,
        stdout,
        exit,
        stderr_tail,
        ..
    } = io;

    Box::pin(async_stream::try_stream! {
        // Keeps pi's stdin open until the run ends.
        let _stdin = stdin;
        let mut got_done_event = false;
        let mut stream_error: Option<AdapterError> = None;
        let mut stream_error_yielded = false;
        let mut state = ParseState { final_text: String::new(), last_tokens: None };
        let mut pending_completion: Option<PiCompletion> = None;

        if let Some(stdout) = stdout {
            let mut lines = Box::pin(read_pi_lines(stdout));
            while let Some(line) = lines.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                let line = match line {
                    Ok(line) => line,
                    Err(err) => {
                        // stdout died before a terminal event. Without this the
                        // orchestrator would only see the run hang.
                        let err = AdapterError::new(err.to_string(), None);
                        if !cancel.is_cancelled() && !got_done_event {
                            stream_error_yielded = true;
                            yield AgentEvent::new(
                                AgentEventKind::Error,
                                clock.now_iso(),
                                serde_json::json!({ "message": err.message }),
                            )
                            .with_error_kind(classify(&err.message));
                        }
                        stream_error = Some(err);
                        break;
                    }
                };

                let Some(event) = parse_rpc_event(&line, clock.as_ref(), &state) else {
                    continue;
                };

                if let Some(final_text) = event.final_text {
                    state.final_text = final_text;
                }
                if let Some(tokens) = event.tokens {
                    state.last_tokens = Some(tokens);
                }
                if let Some(completion) = event.completion.clone() {
                    pending_completion = Some(completion);
                }

                if let Some(agent_event) = event.agent_event {
                    let is_done = agent_event.kind == AgentEventKind::Done;
                    if is_done {
                        got_done_event = true;
                    }
                    yield agent_event;
                    if is_done {
                        // One-shot run over a long-lived process: stop pi
                        // rather than waiting for stdin to close.
                        process_manager.kill_with_grace(pid, TERMINAL_GRACE).await;
                        return;
                    }
                }

                let terminal = if event.settled {
                    pending_completion.clone()
                } else {
                    event
                        .completion
                        .filter(|completion| !completion.wait_for_settled)
                };

                if let Some(completion) = terminal {
                    // The run is over either way, so `got_done_event` is not
                    // updated here: nothing reads it past the `return` below.
                    let (event, failure) = terminal_event(&completion, clock.now_iso());
                    yield event;

                    process_manager.kill_with_grace(pid, TERMINAL_GRACE).await;

                    if let Some(failure) = failure {
                        Err(failure)?;
                    }
                    return;
                }
            }
            drop(lines);

            // Pi is long-lived: leaving without a `done` event means the
            // process is still running, so kill it or the exit never resolves.
            if !got_done_event && (cancel.is_cancelled() || stream_error.is_some()) {
                let process_manager = Arc::clone(&process_manager);
                tokio::spawn(async move {
                    process_manager.kill_with_grace(pid, TERMINAL_GRACE).await;
                });
            }
        }

        let outcome = exit.wait().await;

        // The stream error was already surfaced as an event; re-raise it with
        // the exit code folded into the classification.
        if stream_error_yielded
            && let Some(err) = stream_error
        {
            Err(AdapterError::with_kind(
                err.message.clone(),
                orch_domain::classify_adapter_error(&err.message, outcome.code),
                outcome.code,
            ))?;
        }

        if let Some(code) = outcome.code
            && code != 0
            && !cancel.is_cancelled()
            && !got_done_event
        {
            let message = append_stderr_tail(
                &format!("Pi process exited with code {code}"),
                "pi",
                &stderr_tail.tail().await,
            );
            Err(AdapterError::new(message, Some(code)))?;
        }
    })
}

#[async_trait]
impl Adapter for PiAdapter {
    fn kind(&self) -> &'static str {
        "pi"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "pi").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let args = build_args(&params);
        let mut child = spawn_child(
            &self.process_manager,
            "pi",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                stdin: StdioMode::Pipe,
                inherit_env: params.inherit_env,
            },
            // The tail surfaces auth and extension-load failures on a non-zero
            // exit instead of silently discarding them.
            StderrPolicy::Tail,
            &params.cancel,
        )?;

        let stdin: SharedStdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
        write_prompt(Arc::clone(&stdin), &params.prompt, self.clock.now_ms());

        let pid = child.pid;
        let events = pi_events(
            child,
            stdin,
            Arc::clone(&self.process_manager),
            Arc::clone(&self.clock),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> ParseState {
        ParseState {
            final_text: String::new(),
            last_tokens: None,
        }
    }

    #[test]
    fn usage_is_read_through_every_alias_shape() {
        let native = extract_tokens_from_message(&json!({
            "usage": { "input": 10, "output": 4, "reasoning": 3, "cacheRead": 5 }
        }))
        .expect("usage");
        assert_eq!(
            (
                native.input,
                native.output,
                native.reasoning,
                native.cache_read,
                native.total
            ),
            (10, 4, 3, 5, 17)
        );

        let anthropic = extract_tokens_from_message(&json!({
            "usage": { "input_tokens": 1, "cache_creation_input_tokens": 2 }
        }))
        .expect("usage");
        assert_eq!((anthropic.input, anthropic.cache_write), (1, 2));
    }

    #[test]
    fn an_all_zero_usage_is_dropped_but_a_partial_one_is_not() {
        assert!(
            extract_tokens_from_message(&json!({ "usage": { "input": 0, "output": 0 } })).is_none()
        );
        assert!(extract_tokens_from_message(&json!({ "usage": {} })).is_none());
        assert!(extract_tokens_from_message(&json!({})).is_none());

        // Missing input does *not* discard the usage, unlike the shared
        // extractor.
        let tokens =
            extract_tokens_from_message(&json!({ "usage": { "output": 9 } })).expect("usage");
        assert_eq!((tokens.input, tokens.output, tokens.total), (0, 9, 9));
    }

    #[test]
    fn wait_for_settled_keys_off_presence_not_value() {
        let with_false = parse_completion(&json!({ "willRetry": false }), &state());
        assert!(with_false.wait_for_settled);

        let without = parse_completion(&json!({}), &state());
        assert!(!without.wait_for_settled);
    }

    #[test]
    fn a_completion_falls_back_to_the_aggregated_delta_text() {
        let completion = parse_completion(
            &json!({ "messages": [{ "role": "user", "content": "hi" }] }),
            &ParseState {
                final_text: "streamed".to_owned(),
                last_tokens: None,
            },
        );
        assert_eq!(completion.result, "streamed");
    }

    #[test]
    fn an_error_stop_reason_without_a_message_names_the_reason() {
        let completion = parse_completion(
            &json!({ "messages": [{ "role": "assistant", "content": [], "stopReason": "aborted" }] }),
            &state(),
        );
        assert_eq!(
            completion.failure.expect("failure").message,
            "Pi agent stopped with reason: aborted"
        );
    }

    #[test]
    fn unknown_event_types_are_dropped() {
        let clock = orch_core::test_support::FixedClock::default();
        assert!(parse_rpc_event(r#"{"type":"brand_new"}"#, &clock, &state()).is_none());
        assert!(parse_rpc_event("   ", &clock, &state()).is_none());
    }

    #[test]
    fn an_unparseable_line_becomes_a_raw_output_event() {
        let clock = orch_core::test_support::FixedClock::default();
        let parsed = parse_rpc_event("not json", &clock, &state()).expect("event");
        let event = parsed.agent_event.expect("agent event");
        assert_eq!(event.kind, AgentEventKind::Output);
        assert_eq!(event.data, json!({ "text": "not json" }));
    }

    #[test]
    fn tool_result_text_unwraps_the_content_array() {
        assert_eq!(tool_result_text(Some(&json!("plain"))), "plain");
        assert_eq!(
            tool_result_text(Some(&json!({ "content": [{ "type": "text", "text": "a" },
                                                        { "type": "text", "text": "b" }] }))),
            "ab"
        );
        assert_eq!(tool_result_text(Some(&json!({}))), "");
        assert_eq!(tool_result_text(None), "");
    }

    /// A stdout that fails on first read, which is what pi's own `catch` is
    /// there for (ECONNRESET/EPIPE before any terminal event). A real child
    /// cannot be made to do this, so the reader is injected at the `ChildIo`
    /// seam.
    struct FailingReader;

    impl tokio::io::AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("ECONNRESET")))
        }
    }

    #[tokio::test]
    async fn a_stdout_failure_before_a_terminal_event_is_surfaced_and_then_raised() {
        use futures::StreamExt;
        use orch_core::process::{ExitOutcome, ExitSignal};
        use orch_core::test_support::FakeProcessManager;

        let pm = Arc::new(FakeProcessManager::new());
        let io = ChildIo {
            pid: 4242,
            stdout: Some(Box::new(FailingReader)),
            stderr: None,
            stdin: None,
            exit: ExitSignal::resolved(ExitOutcome {
                code: Some(1),
                signal: None,
            }),
            stderr_tail: Default::default(),
        };

        let mut events = pi_events(
            io,
            Arc::new(tokio::sync::Mutex::new(None)),
            Arc::clone(&pm) as Arc<dyn ProcessManager>,
            system_clock(),
            CancellationToken::new(),
        );

        // The failure is reported as an event first, so the orchestrator sees
        // a cause rather than a silent hang.
        let first = events
            .next()
            .await
            .expect("an event")
            .expect("not an error yet");
        assert_eq!(first.kind, AgentEventKind::Error);
        assert!(
            first.data["message"]
                .as_str()
                .expect("message")
                .contains("ECONNRESET"),
            "{:?}",
            first.data
        );

        // …and then raised, with the exit code folded into the class.
        let err = events
            .next()
            .await
            .expect("a second item")
            .expect_err("the run failed");
        assert!(err.message.contains("ECONNRESET"));
        assert_eq!(err.kind, AdapterErrorKind::ProcessCrash);
        assert!(events.next().await.is_none(), "the stream ends after Err");

        // The session is long-lived, so a dead stream must still reap it.
        for _ in 0..50 {
            if !pm.killed_with_grace().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(pm.killed_with_grace(), [(4242, TERMINAL_GRACE)]);
    }

    #[test]
    fn a_write_without_a_path_falls_back_to_a_generic_output() {
        let parsed = parse_tool_execution_end(
            &json!({ "type": "tool_execution_end", "toolName": "write" }),
            "t".to_owned(),
        );
        let event = parsed.agent_event.expect("agent event");
        assert_eq!(event.kind, AgentEventKind::Output);
        assert_eq!(event.data["text"], json!("write completed"));
    }
}
