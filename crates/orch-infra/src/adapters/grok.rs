//! Grok CLI adapter — `src/infrastructure/adapters/grok.ts`,
//! `kind = "grok"` (`docs/parity/adapters.md` §2.6).
//!
//! Grok streams text/thought deltas; this adapter aggregates text deltas into
//! bounded output chunks and emits a terminal `done` at session end,
//! synthesising one if the CLI exited cleanly without an `end` record.
//!
//! Its stderr is **drained and discarded** rather than tail-captured: the TS
//! never reads it at all and deadlocks on a chatty child, and appending a tail
//! here would change the error text, its classification and the persisted
//! `agent.last_error` (PARITY.md §14.12).

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::StreamExt;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::util::{error_event, probe_version, stringify, system_clock, version_probe_failure};
use crate::process::line_reader::read_lines;
use crate::process::spawn::{ChildIo, StderrPolicy, spawn_child};

const INSTALL_HINT: &str =
    "Grok CLI not found. Install and authenticate the grok CLI, then ensure `grok` is on PATH.";

/// Aggregated text is flushed once it reaches this many UTF-16 code units.
const OUTPUT_CHUNK_LEN: usize = 240;

pub struct GrokAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl GrokAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        GrokAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for GrokAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokAdapter").finish_non_exhaustive()
    }
}

fn build_args(params: &ExecuteParams) -> Vec<String> {
    let mut args = vec![
        // The prompt is argv[1], raw.
        "-p".to_owned(),
        params.prompt.clone(),
        "--output-format".to_owned(),
        "streaming-json".to_owned(),
        "--permission-mode".to_owned(),
        "bypassPermissions".to_owned(),
        "--always-approve".to_owned(),
        // Passed in addition to the spawn cwd.
        "--cwd".to_owned(),
        params.workspace.to_string_lossy().into_owned(),
    ];

    if let Some(model) = &params.config.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = params.config.effort {
        args.push("--effort".to_owned());
        args.push(effort.as_str().to_owned());
    }
    // Truthiness: `max_turns: 0` omits the flag.
    if let Some(max_turns) = params.config.max_turns.filter(|turns| *turns != 0) {
        args.push("--max-turns".to_owned());
        args.push(max_turns.to_string());
    }

    if let Some(system_prompt) = params
        .system_prompt
        .as_deref()
        .or(params.config.system_prompt.as_deref())
    {
        args.push("--system-prompt-override".to_owned());
        args.push(system_prompt.to_owned());
    }
    args
}

/// Aggregated assistant text: `text_buffer` is the unflushed chunk,
/// `final_text` the whole turn, which the `done` event reports.
#[derive(Default)]
struct TextState {
    buffer: String,
    buffer_units: usize,
    final_text: String,
}

impl TextState {
    fn append(&mut self, text: &str) {
        self.buffer.push_str(text);
        self.buffer_units += text.encode_utf16().count();
        self.final_text.push_str(text);
    }

    fn is_chunk_ready(&self) -> bool {
        self.buffer_units >= OUTPUT_CHUNK_LEN
    }

    /// `flushOutput()` — nothing to emit for an empty buffer.
    fn flush(&mut self, clock: &dyn Clock) -> Option<AgentEvent> {
        if self.buffer.is_empty() {
            return None;
        }
        let chunk = std::mem::take(&mut self.buffer);
        self.buffer_units = 0;
        Some(AgentEvent::new(
            AgentEventKind::Output,
            clock.now_iso(),
            serde_json::json!({ "text": chunk }),
        ))
    }
}

fn parse_event(line: &str, clock: &dyn Clock, state: &mut TextState) -> Option<AgentEvent> {
    if line.trim().is_empty() {
        return None;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        return Some(AgentEvent::new(
            AgentEventKind::Output,
            clock.now_iso(),
            serde_json::json!({ "text": line }),
        ));
    };

    let timestamp = clock.now_iso();
    match parsed.get("type").and_then(Value::as_str).unwrap_or("") {
        "thought" => None,

        "text" => {
            if let Some(text) = parsed.get("data").and_then(Value::as_str) {
                state.append(text);
            }
            None
        }

        "tool_call" | "tool_use" => {
            Some(AgentEvent::new(AgentEventKind::ToolCall, timestamp, parsed))
        }

        "tool_result" => Some(AgentEvent::new(AgentEventKind::Output, timestamp, parsed)),

        "error" => {
            let message = match parsed.get("data") {
                Some(Value::String(data)) => data.clone(),
                _ => stringify(&parsed),
            };
            Some(error_event(timestamp, parsed, &message))
        }

        "end" => {
            let mut data = Map::new();
            data.insert("result".to_owned(), Value::String(state.final_text.clone()));
            data.insert("raw".to_owned(), parsed);
            Some(AgentEvent::new(
                AgentEventKind::Done,
                timestamp,
                Value::Object(data),
            ))
        }

        _ => Some(AgentEvent::new(AgentEventKind::Output, timestamp, parsed)),
    }
}

fn grok_events(
    io: ChildIo,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
) -> futures::stream::BoxStream<'static, Result<AgentEvent, AdapterError>> {
    let ChildIo { stdout, exit, .. } = io;

    Box::pin(async_stream::try_stream! {
        let mut got_done_event = false;
        let mut state = TextState::default();

        if let Some(stdout) = stdout {
            let mut lines = Box::pin(read_lines(stdout));
            let mut read_error = None;
            while let Some(line) = lines.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                let line = match line {
                    Ok(line) => line,
                    Err(err) => {
                        read_error = Some(err);
                        break;
                    }
                };

                match parse_event(&line, clock.as_ref(), &mut state) {
                    None => {
                        // Deltas accumulate silently until the chunk is big
                        // enough to be worth an event.
                        if state.is_chunk_ready()
                            && let Some(event) = state.flush(clock.as_ref())
                        {
                            yield event;
                        }
                    }
                    Some(event) => {
                        if event.kind == AgentEventKind::Done {
                            if let Some(flushed) = state.flush(clock.as_ref()) {
                                yield flushed;
                            }
                            got_done_event = true;
                        }
                        yield event;
                    }
                }
            }
            drop(lines);

            if let Some(err) = read_error {
                Err(AdapterError::new(err.to_string(), None))?;
            }
        }

        let outcome = exit.wait().await;

        if !got_done_event
            && !cancel.is_cancelled()
            && let Some(event) = state.flush(clock.as_ref())
        {
            yield event;
        }

        if let Some(code) = outcome.code {
            if code != 0 && !cancel.is_cancelled() && !got_done_event {
                // No stderr tail: see the module docs.
                let message = format!("Grok process exited with code {code}");
                Err(AdapterError::with_kind(
                    message.clone(),
                    orch_domain::classify_adapter_error(&message, Some(code)),
                    Some(code),
                ))?;
            }
            // A clean exit with no `end` record still owes the orchestrator a
            // terminal event.
            if code == 0 && !got_done_event && !cancel.is_cancelled() {
                yield AgentEvent::new(
                    AgentEventKind::Done,
                    clock.now_iso(),
                    serde_json::json!({ "result": state.final_text }),
                );
            }
        }
    })
}

#[async_trait]
impl Adapter for GrokAdapter {
    fn kind(&self) -> &'static str {
        "grok"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "grok").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let args = build_args(&params);
        let io = spawn_child(
            &self.process_manager,
            "grok",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                stdin: StdioMode::Ignore,
            },
            StderrPolicy::Discard,
            &params.cancel,
        )?;

        let pid = io.pid;
        let events = grok_events(io, Arc::clone(&self.clock), params.cancel);
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
    use super::super::util::classify;
    use super::*;

    #[test]
    fn classification_of_the_exit_message_is_a_process_crash() {
        assert_eq!(
            classify("Grok process exited with code 1"),
            orch_domain::AdapterErrorKind::Unknown,
            "without an exit code the message alone is unclassifiable"
        );
        assert_eq!(
            orch_domain::classify_adapter_error("Grok process exited with code 1", Some(1)),
            orch_domain::AdapterErrorKind::ProcessCrash
        );
    }

    #[test]
    fn the_chunk_threshold_counts_utf16_code_units() {
        let mut state = TextState::default();
        // 120 astral characters = 240 UTF-16 units, but 480 bytes.
        state.append(&"\u{1f600}".repeat(119));
        assert!(!state.is_chunk_ready());
        state.append("\u{1f600}");
        assert!(state.is_chunk_ready());
    }
}
