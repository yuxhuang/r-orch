//! Shared adapter machinery, ported from
//! `src/infrastructure/adapters/utils.ts` (`docs/parity/adapters.md` §1.3–1.6).

use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use orch_core::adapter::{AdapterError, AgentEvent, AgentEventKind};
use orch_core::clock::{Clock, iso_from_epoch_ms};
use orch_core::process::{ProcessManager, SpawnOptions};
use orch_domain::{AdapterErrorKind, TokenUsage, TokenUsageOpts, create_token_usage};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::process::line_reader::read_lines;
use crate::process::spawn::ChildIo;

/// The real clock. Adapters mint `AgentEvent.timestamp` at parse time, exactly
/// where the TS calls `new Date().toISOString()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        jiff::Timestamp::now().as_millisecond()
    }

    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}

pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Combine the system and user prompts. Adapters without a native
/// system-prompt flag use this.
///
/// The separator is **exactly** two newlines, and an empty system prompt is
/// falsy in TS, so it returns the user prompt unchanged.
pub fn build_full_prompt(system_prompt: Option<&str>, user_prompt: &str) -> String {
    match system_prompt {
        Some(system) if !system.is_empty() => format!("{system}\n\n{user_prompt}"),
        _ => user_prompt.to_owned(),
    }
}

/// Append a captured stderr tail to an adapter process error.
pub fn append_stderr_tail(message: &str, adapter_name: &str, tail: &str) -> String {
    if tail.is_empty() {
        return message.to_owned();
    }
    format!("{message}\n--- {adapter_name} stderr (tail) ---\n{tail}")
}

/// The first of the two keys whose value is a number.
fn usage_number(usage: &Value, primary: &str, fallback: &str) -> Option<u64> {
    number_field(usage, primary).or_else(|| number_field(usage, fallback))
}

/// `typeof value[key] === 'number'`, as a `u64`.
///
/// JSON numbers are arbitrary; [`TokenUsage`] counts are `u64`, so a negative
/// or fractional count is clamped at zero and truncated. No provider emits
/// either, and the alternative is a panic.
pub fn number_field(value: &Value, key: &str) -> Option<u64> {
    let Some(Value::Number(number)) = value.get(key) else {
        return None;
    };
    if let Some(exact) = number.as_u64() {
        return Some(exact);
    }
    let approx = number.as_f64().unwrap_or(0.0);
    Some(if approx.is_finite() && approx > 0.0 {
        approx as u64
    } else {
        0
    })
}

/// Extract token usage from a parsed JSON event.
///
/// `stats_fallback` also looks at `parsed.stats.usage`, and is claude-only.
/// A `usage` object without a numeric `input_tokens`/`inputTokens` is
/// **discarded whole** — the TS returns `undefined` rather than defaulting the
/// input to 0 (`docs/parity/adapters.md` §10.7).
pub fn extract_tokens(parsed: &Value, stats_fallback: bool) -> Option<TokenUsage> {
    let mut usage = parsed.get("usage");
    if usage.is_none() && stats_fallback {
        usage = parsed.get("stats").and_then(|stats| stats.get("usage"));
    }
    // `!usage` in TS: `null` is falsy too.
    let usage = usage.filter(|value| !value.is_null())?;

    let input = usage_number(usage, "input_tokens", "inputTokens")?;
    Some(create_token_usage(
        input,
        usage_number(usage, "output_tokens", "outputTokens").unwrap_or(0),
        Some(TokenUsageOpts {
            reasoning: usage_number(usage, "reasoning_tokens", "reasoningTokens").or(Some(0)),
            cache_read: usage_number(usage, "cache_read_input_tokens", "cacheReadTokens")
                .or(Some(0)),
            cache_write: usage_number(usage, "cache_creation_input_tokens", "cacheWriteTokens")
                .or(Some(0)),
        }),
    ))
}

/// `JSON.stringify(value)` for the error-message paths, where the TS does
/// exactly that to a non-string payload.
pub fn stringify(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The error-event message the claude/cursor/grok paths classify:
/// `typeof data === 'string' ? data : JSON.stringify(data)`.
pub fn error_message_of(value: &Value) -> String {
    stringify(value)
}

/// The stream used by claude, codex, cursor and opencode
/// (`utils.ts:105-163`).
///
/// Lifecycle listeners are attached before this is called — the stderr drain
/// in [`crate::process::spawn_child`] and the [`orch_core::process::ExitSignal`]
/// in the process manager — which is the Rust form of the TS comment about
/// eager `close`/`error`/stderr listeners: a fast-failing child must not be
/// missed while the orchestrator has yet to poll the stream.
pub fn streaming_events<P>(
    io: ChildIo,
    mut parse: P,
    adapter_name: &'static str,
    cancel: CancellationToken,
) -> BoxStream<'static, std::result::Result<AgentEvent, AdapterError>>
where
    P: FnMut(&str) -> Option<AgentEvent> + Send + 'static,
{
    let ChildIo {
        stdout,
        exit,
        stderr_tail,
        ..
    } = io;

    Box::pin(async_stream::try_stream! {
        let mut got_done_event = false;

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
                if let Some(event) = parse(&line) {
                    if event.kind == AgentEventKind::Done {
                        got_done_event = true;
                    }
                    yield event;
                }
            }
            // Release the fd immediately rather than waiting for the process
            // to die — the TS `proc.stdout.destroy()` in its `finally`.
            drop(lines);

            if let Some(err) = read_error {
                // A stdout read error propagates straight out of the TS
                // generator, with neither the stderr tail nor an `errorKind`;
                // `AdapterError::new` classifies it from the message, which is
                // what the orchestrator would otherwise do.
                Err(AdapterError::new(err.to_string(), None))?;
            }
        }

        let outcome = exit.wait().await;

        // A `done` event suppresses every exit-code error, and so does an
        // aborted run. `code == None` (killed by a signal) does not throw.
        if let Some(code) = outcome.code
            && code != 0
            && !cancel.is_cancelled()
            && !got_done_event
        {
            let message = append_stderr_tail(
                &format!("{adapter_name} process exited with code {code}"),
                adapter_name,
                &stderr_tail.tail().await,
            );
            Err(AdapterError::new(message, Some(code)))?;
        }
    })
}

/// `execFileAsync(command, ['--version'])`, reproducing node's error messages
/// so [`orch_domain::classify_adapter_error`] sees the same strings.
///
/// It goes through the injected [`ProcessManager`] rather than spawning
/// directly, which is what makes `test()` assertable without depending on
/// which CLIs happen to be installed. The one invisible consequence is that
/// the probe child gets its own process group, where node's `execFile` would
/// leave it in ours.
pub async fn probe_version(
    process_manager: &dyn ProcessManager,
    command: &str,
) -> std::result::Result<String, String> {
    let mut child = process_manager
        .spawn(command, &["--version".to_owned()], SpawnOptions::default())
        .map_err(|err| spawn_failure_message(command, &err))?;

    // Both pipes are drained together: reading them in sequence would deadlock
    // on a chatty `--version`.
    let mut stdout = String::new();
    let mut stderr = String::new();
    let (out, err) = (child.stdout.take(), child.stderr.take());
    tokio::join!(
        async {
            if let Some(mut out) = out {
                let _ = out.read_to_string(&mut stdout).await;
            }
        },
        async {
            if let Some(mut err) = err {
                let _ = err.read_to_string(&mut stderr).await;
            }
        }
    );

    let outcome = child.exit.wait().await;
    if outcome.success() {
        return Ok(stdout.trim().to_owned());
    }
    // node: `Command failed: <cmd> <args>\n<stderr>`.
    Err(format!("Command failed: {command} --version\n{stderr}"))
}

/// node's `spawn <cmd> <ERRNO>`, which is what makes a missing CLI classify as
/// `spawn_failed` rather than `unknown`.
fn spawn_failure_message(command: &str, err: &orch_core::error::Error) -> String {
    let errno = match err {
        orch_core::error::Error::Io { source, .. } => match source.kind() {
            std::io::ErrorKind::NotFound => "ENOENT",
            std::io::ErrorKind::PermissionDenied => "EACCES",
            _ => "EFAULT",
        },
        _ => "ENOENT",
    };
    format!("spawn {command} {errno}")
}

/// `{ ok: false, error: <fixed text>, errorKind: classifyAdapterError(msg) }`.
pub fn version_probe_failure(
    install_hint: &str,
    message: &str,
) -> orch_core::adapter::AdapterTestResult {
    orch_core::adapter::AdapterTestResult::failed(
        install_hint,
        orch_domain::classify_adapter_error(message, None),
    )
}

/// The `stream-json` line dispatch shared by claude and cursor.
///
/// The TS duplicates `parseClaudeEvent` and `parseCursorEvent` verbatim except
/// for claude's `statsFallback` (`cursor.ts:96`: "Cursor stream-json uses the
/// same format as Claude stream-json"), so the two adapters share one
/// implementation here. `data` keeps each adapter's existing shape — the whole
/// `parsed.message`, not the canonical `{ text }` — because consumers are
/// defensive about it and PARITY.md §8.6 says not to normalize.
pub fn parse_stream_json_line(
    line: &str,
    clock: &dyn Clock,
    stats_fallback: bool,
) -> Option<AgentEvent> {
    if line.trim().is_empty() {
        return None;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        // An unparseable line (including one the 16 KB cap truncated) becomes
        // a raw `output` event carrying the line itself.
        return Some(AgentEvent::new(
            AgentEventKind::Output,
            clock.now_iso(),
            Value::String(line.to_owned()),
        ));
    };

    let timestamp = clock.now_iso();
    let event = match parsed.get("type").and_then(Value::as_str) {
        Some("assistant") => AgentEvent::new(
            AgentEventKind::Output,
            timestamp,
            coalesce(&parsed, "message"),
        ),
        Some("tool_use") => AgentEvent::new(AgentEventKind::ToolCall, timestamp, parsed),
        Some("tool_result") => AgentEvent::new(AgentEventKind::Output, timestamp, parsed),
        Some("error") => {
            let data = coalesce(&parsed, "error");
            let message = error_message_of(&data);
            error_event(timestamp, data, &message)
        }
        Some("result") => {
            let tokens = extract_tokens(&parsed, stats_fallback);
            let mut event = AgentEvent::new(AgentEventKind::Done, timestamp, parsed);
            event.tokens = tokens;
            event
        }
        _ => AgentEvent::new(AgentEventKind::Output, timestamp, parsed),
    };
    Some(event)
}

/// Copy `key` from `source` into `target` under `name`, only when it is
/// present — `JSON.stringify` drops `undefined` properties, so an absent
/// source field must stay absent rather than become an explicit `null`, and
/// the run-event payload is compared byte for byte.
pub fn copy_present(
    target: &mut serde_json::Map<String, Value>,
    name: &str,
    source: &Value,
    key: &str,
) {
    if let Some(value) = source.get(key) {
        target.insert(name.to_owned(), value.clone());
    }
}

/// `parsed.<key> ?? parsed` — the nullish coalescing the TS uses to fall back
/// to the whole record.
pub fn coalesce(parsed: &Value, key: &str) -> Value {
    match parsed.get(key) {
        Some(value) if !value.is_null() => value.clone(),
        _ => parsed.clone(),
    }
}

/// `{ type: 'error', data: … , errorKind: classifyAdapterError(message) }`.
pub fn error_event(timestamp: String, data: Value, message: &str) -> AgentEvent {
    AgentEvent::new(AgentEventKind::Error, timestamp, data)
        .with_error_kind(orch_domain::classify_adapter_error(message, None))
}

/// Classification for a message with no exit code, for the adapters that
/// build their own error events.
pub fn classify(message: &str) -> AdapterErrorKind {
    orch_domain::classify_adapter_error(message, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn returns_the_user_prompt_when_there_is_no_system_prompt() {
        assert_eq!(build_full_prompt(None, "do the task"), "do the task");
        assert_eq!(build_full_prompt(Some(""), "do the task"), "do the task");
    }

    #[test]
    fn prepends_the_system_prompt_with_a_double_newline_separator() {
        assert_eq!(
            build_full_prompt(Some("be helpful"), "do the task"),
            "be helpful\n\ndo the task"
        );
    }

    #[test]
    fn preserves_multi_line_prompts() {
        let system = "You are Backend A.\n## Rules\n- No any";
        let user = "## Task: Fix bug\nDescription: crash on startup";
        assert_eq!(
            build_full_prompt(Some(system), user),
            format!("{system}\n\n{user}")
        );
    }

    #[test]
    fn the_separator_is_exactly_two_newlines() {
        let joined = build_full_prompt(Some("sys"), "usr");
        assert_eq!(&joined["sys".len()..joined.len() - "usr".len()], "\n\n");
    }

    #[test]
    fn the_stderr_tail_is_only_appended_when_it_is_non_empty() {
        assert_eq!(append_stderr_tail("boom", "Claude", ""), "boom");
        assert_eq!(
            append_stderr_tail("boom", "Cursor agent", "detail"),
            "boom\n--- Cursor agent stderr (tail) ---\ndetail"
        );
    }

    #[test]
    fn extracts_snake_case_token_usage() {
        let tokens = extract_tokens(
            &json!({ "usage": { "input_tokens": 100, "output_tokens": 50 } }),
            false,
        )
        .expect("usage");
        assert_eq!(
            tokens,
            TokenUsage {
                input: 100,
                output: 50,
                reasoning: 0,
                total: 150,
                cache_read: 0,
                cache_write: 0,
            }
        );
    }

    #[test]
    fn extracts_camel_case_token_usage_including_cache_counters() {
        let tokens = extract_tokens(
            &json!({ "usage": {
                "inputTokens": 50,
                "outputTokens": 25,
                "cacheReadTokens": 10,
                "cacheWriteTokens": 5,
            }}),
            false,
        )
        .expect("usage");
        assert_eq!(
            tokens,
            TokenUsage {
                input: 50,
                output: 25,
                reasoning: 0,
                // Cache counters are informational and excluded from `total`.
                total: 75,
                cache_read: 10,
                cache_write: 5,
            }
        );
    }

    #[test]
    fn the_snake_case_key_wins_over_the_camel_case_one() {
        let tokens = extract_tokens(
            &json!({ "usage": { "input_tokens": 7, "inputTokens": 99 } }),
            false,
        )
        .expect("usage");
        assert_eq!(tokens.input, 7);
    }

    #[test]
    fn a_non_numeric_key_falls_through_to_the_camel_case_one() {
        let tokens = extract_tokens(
            &json!({ "usage": { "input_tokens": "7", "inputTokens": 99 } }),
            false,
        )
        .expect("usage");
        assert_eq!(tokens.input, 99);
    }

    #[test]
    fn usage_without_an_input_count_is_discarded_whole() {
        assert!(extract_tokens(&json!({ "usage": { "output_tokens": 50 } }), false).is_none());
        assert!(extract_tokens(&json!({ "usage": {} }), false).is_none());
        assert!(extract_tokens(&json!({}), false).is_none());
        assert!(extract_tokens(&json!({ "usage": null }), false).is_none());
    }

    #[test]
    fn the_stats_fallback_is_only_consulted_when_asked() {
        let event = json!({ "stats": { "usage": { "input_tokens": 3, "output_tokens": 4 } } });
        assert!(extract_tokens(&event, false).is_none());
        let tokens = extract_tokens(&event, true).expect("usage");
        assert_eq!((tokens.input, tokens.output, tokens.total), (3, 4, 7));
    }

    #[test]
    fn a_direct_usage_object_beats_the_stats_fallback() {
        let tokens = extract_tokens(
            &json!({
                "usage": { "input_tokens": 1 },
                "stats": { "usage": { "input_tokens": 999 } },
            }),
            true,
        )
        .expect("usage");
        assert_eq!(tokens.input, 1);
    }

    #[test]
    fn reasoning_tokens_are_part_of_the_total() {
        let tokens = extract_tokens(
            &json!({ "usage": { "input_tokens": 10, "output_tokens": 4, "reasoning_tokens": 3 } }),
            false,
        )
        .expect("usage");
        assert_eq!((tokens.reasoning, tokens.total), (3, 17));
    }

    #[test]
    fn a_provider_supplied_total_is_ignored_and_recomputed() {
        let tokens = extract_tokens(
            &json!({ "usage": { "input_tokens": 12_416, "output_tokens": 2, "total": 14_502 } }),
            false,
        )
        .expect("usage");
        assert_eq!(tokens.total, 12_418);
    }

    #[test]
    fn stringify_passes_strings_through_and_json_encodes_the_rest() {
        assert_eq!(stringify(&json!("plain")), "plain");
        assert_eq!(stringify(&json!({ "a": 1 })), r#"{"a":1}"#);
        assert_eq!(stringify(&Value::Null), "null");
    }

    #[tokio::test]
    async fn the_version_probe_reports_a_missing_binary_the_way_node_does() {
        let pm = crate::process::SystemProcessManager::new();
        let err = probe_version(&pm, "orch-definitely-not-a-binary")
            .await
            .expect_err("missing binary");
        assert_eq!(err, "spawn orch-definitely-not-a-binary ENOENT");
        assert_eq!(classify(&err), AdapterErrorKind::SpawnFailed);
    }

    #[tokio::test]
    async fn the_version_probe_trims_the_version() {
        // `cargo --version` is not a real adapter probe, but `cargo` is on the
        // PATH wherever this test runs. (`env --version` is GNU-only.)
        let pm = crate::process::SystemProcessManager::new();
        let version = probe_version(&pm, "cargo")
            .await
            .expect("cargo has --version");
        assert!(!version.is_empty());
        assert_eq!(version, version.trim());
    }

    #[tokio::test]
    async fn a_failing_version_probe_reports_the_command_and_its_stderr() {
        let pm = crate::process::SystemProcessManager::new();
        let err = probe_version(&pm, "false").await.expect_err("exit 1");
        assert_eq!(err, "Command failed: false --version\n");
    }

    #[test]
    fn the_system_clock_formats_an_iso_timestamp() {
        let iso = SystemClock.now_iso();
        assert_eq!(iso.len(), 24, "{iso}");
        assert!(iso.ends_with('Z'));
    }
}
