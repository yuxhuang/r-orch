//! Nullable run usage for storage v2 (spec R86 and R87).
//!
//! This deliberately does not convert `TokenUsage`: its legacy extractors
//! replace absent counters with zero. Read the original JSON instead. The
//! raw-output entry point must run before an adapter drops lifecycle records;
//! the event entry point can recover only metadata retained in `AgentEvent`.
//!
//! The record lives here, not in `orch-infra`, because the orchestrator's
//! collector observes through `orch-core` types only (the crate-graph check
//! limits which engine files may name `orch_infra`).

use crate::adapter::{AdapterTestResult, AgentEvent, AgentEventKind};
use serde_json::Value;

/// Values for the seven nullable usage columns of a run, independent of outcome.
/// Counts fit SQLite's signed INTEGER without rounding or saturation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunUsage {
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub tokens_reasoning: Option<i64>,
    pub tokens_cache_read: Option<i64>,
    pub tokens_cache_write: Option<i64>,
    pub model: Option<String>,
    pub adapter_version: Option<String>,
}

impl RunUsage {
    /// Seed metadata from the prepared config and an already cached version
    /// probe. A failed probe may still supply a version; neither failure nor
    /// missing metadata changes the run's outcome. This performs no I/O.
    pub fn new(requested_model: Option<&str>, probe: Option<&AdapterTestResult>) -> Self {
        Self {
            model: metadata_text(requested_model),
            adapter_version: metadata_text(probe.and_then(|probe| probe.version.as_deref())),
            ..Self::default()
        }
    }

    /// Fill the CLI version from a cached probe when no version is known yet.
    /// A version already held is never replaced, and a probe without one
    /// changes nothing. This performs no I/O.
    pub fn fill_adapter_version(&mut self, probe: Option<&AdapterTestResult>) {
        if self.adapter_version.is_none() {
            self.adapter_version = metadata_text(probe.and_then(|probe| probe.version.as_deref()));
        }
    }

    /// Observe a decoded CLI record before legacy parsing drops information.
    ///
    /// Each supplied, valid counter replaces its previous observation, including
    /// explicit zero. Missing/invalid fields leave previous observations intact.
    /// Reports are snapshots, never added together: replaying one is idempotent.
    /// No success check is made, so error and aborted records retain usage too.
    /// Only known metadata carriers are inspected, never tool inputs or text.
    pub fn observe_output(&mut self, adapter: &str, output: &Value) {
        let kind = output.get("type").and_then(Value::as_str);
        let source = match (adapter, kind) {
            ("claude" | "cursor", Some("assistant")) => {
                output.get("message").unwrap_or(&Value::Null)
            }
            ("claude" | "cursor", Some("system" | "result" | "error"))
            | (
                "codex",
                Some(
                    "thread.started" | "turn.started" | "turn.completed" | "turn.failed" | "error",
                ),
            )
            | ("grok", Some("end" | "error")) => output,
            ("opencode", Some("step_start" | "step_finish")) => {
                output.get("part").unwrap_or(&Value::Null)
            }
            ("pi", Some("agent_end")) => output
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| {
                    messages.iter().rev().find(|message| {
                        message.get("role").and_then(Value::as_str) == Some("assistant")
                    })
                })
                .unwrap_or(&Value::Null),
            ("pi", Some("message_end" | "turn_end" | "response")) => output,
            _ => return,
        };
        self.observe_source(adapter, source);
        // Some wrappers report the model outside the part/message. The more
        // specific source wins when both are present.
        if metadata_text(source.get("model").and_then(Value::as_str)).is_none()
            && let Some(model) = metadata_text(output.get("model").and_then(Value::as_str))
        {
            self.model = Some(model);
        }
    }

    /// Observe metadata that survived the adapter's event conversion. Never use
    /// `event.tokens` as a fallback: it cannot distinguish unknown from zero.
    pub fn observe_event(&mut self, adapter: &str, event: &AgentEvent) {
        if matches!(
            event.kind,
            AgentEventKind::Command | AgentEventKind::FileChange | AgentEventKind::ToolCall
        ) {
            return;
        }
        let source = match adapter {
            "codex" | "pi" | "grok" => event.data.get("raw").unwrap_or(&event.data),
            _ => &event.data,
        };
        // OpenCode emits the part itself, without the outer event type.
        if adapter == "opencode"
            && matches!(event.kind, AgentEventKind::Done | AgentEventKind::Error)
        {
            self.observe_source(adapter, source);
        } else if matches!(adapter, "claude" | "cursor")
            && event.kind == AgentEventKind::Output
            && source.get("role").and_then(Value::as_str) == Some("assistant")
        {
            // These adapters emit the message itself for assistant records.
            self.observe_source(adapter, source);
        } else {
            self.observe_output(adapter, source);
        }
    }

    fn observe_source(&mut self, adapter: &str, source: &Value) {
        if let Some(model) = metadata_text(source.get("model").and_then(Value::as_str)) {
            self.model = Some(model);
        }
        let usage = match adapter {
            "opencode" => source.get("tokens"),
            "claude" => source
                .get("usage")
                .or_else(|| source.get("stats")?.get("usage")),
            _ => source.get("usage"),
        };
        let Some(usage) = usage.filter(|usage| usage.is_object()) else {
            return;
        };
        let aliases: [&[&str]; 5] = match adapter {
            "pi" => [
                &["input", "input_tokens"],
                &["output", "output_tokens"],
                &["reasoning", "reasoning_tokens"],
                &["cacheRead", "cache_read", "cache_read_input_tokens"],
                &["cacheWrite", "cache_write", "cache_creation_input_tokens"],
            ],
            "opencode" => [&["input"], &["output"], &["reasoning"], &[], &[]],
            // Codex includes cached input in input_tokens and reasoning output
            // in output_tokens. Keep these detail counters as reported; never
            // add them to, or subtract them from, the input/output totals.
            "codex" => [
                &["input_tokens", "inputTokens"],
                &["output_tokens", "outputTokens"],
                &[
                    "reasoning_output_tokens",
                    "reasoning_tokens",
                    "reasoningTokens",
                ],
                &[
                    "cached_input_tokens",
                    "cache_read_input_tokens",
                    "cacheReadTokens",
                ],
                &["cache_creation_input_tokens", "cacheWriteTokens"],
            ],
            _ => [
                &["input_tokens", "inputTokens"],
                &["output_tokens", "outputTokens"],
                &["reasoning_tokens", "reasoningTokens"],
                &["cache_read_input_tokens", "cacheReadTokens"],
                &["cache_creation_input_tokens", "cacheWriteTokens"],
            ],
        };
        for (destination, keys) in [
            &mut self.tokens_input,
            &mut self.tokens_output,
            &mut self.tokens_reasoning,
            &mut self.tokens_cache_read,
            &mut self.tokens_cache_write,
        ]
        .into_iter()
        .zip(aliases)
        {
            if let Some(count) = keys.iter().find_map(|key| count(usage.get(*key)?)) {
                *destination = Some(count);
            }
        }
        if adapter == "opencode"
            && let Some(cache) = usage.get("cache")
        {
            if let Some(read) = cache.get("read").and_then(count) {
                self.tokens_cache_read = Some(read);
            }
            if let Some(write) = cache.get("write").and_then(count) {
                self.tokens_cache_write = Some(write);
            }
        }
    }
}

/// Receives a run's usage once its event stream has ended, whatever the
/// outcome: success, failure, cancellation or shutdown. Storage-independent:
/// the durable run finish (spec R36) replaces this as the consumer.
pub trait RunUsageObserver: Send + Sync {
    fn run_usage_observed(&self, run_id: &str, usage: &RunUsage);
}

fn count(value: &Value) -> Option<i64> {
    value.as_u64().and_then(|count| i64::try_from(count).ok())
}

fn metadata_text(text: Option<&str>) -> Option<String> {
    // Storage v2's inline-text ceiling. Oversized or blank metadata is unknown;
    // never truncate a model identity into a different identity.
    text.filter(|text| text.len() <= 64 * 1024 && !text.trim().is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn counters(usage: &RunUsage) -> [Option<i64>; 5] {
        [
            usage.tokens_input,
            usage.tokens_output,
            usage.tokens_reasoning,
            usage.tokens_cache_read,
            usage.tokens_cache_write,
        ]
    }

    fn observed(adapter: &str, output: Value) -> RunUsage {
        let mut usage = RunUsage::default();
        usage.observe_output(adapter, &output);
        usage
    }

    #[test]
    fn absent_metadata_is_unknown_in_all_seven_columns() {
        assert_eq!(RunUsage::new(None, None), RunUsage::default());
    }

    #[test]
    fn failed_probe_retains_supplied_version_and_requested_model() {
        let probe = AdapterTestResult {
            ok: false,
            version: Some("synthetic-cli 1.2.3".into()),
            ..Default::default()
        };
        let usage = RunUsage::new(Some("opaque/model#variant"), Some(&probe));
        assert_eq!(
            (usage.model.as_deref(), usage.adapter_version.as_deref()),
            (Some("opaque/model#variant"), Some("synthetic-cli 1.2.3"))
        );
    }

    #[test]
    fn failed_probe_without_version_keeps_unknown_and_does_not_fail() {
        let probe = AdapterTestResult {
            ok: false,
            error: Some("unavailable".into()),
            ..Default::default()
        };
        assert_eq!(RunUsage::new(None, Some(&probe)), RunUsage::default());
    }

    #[test]
    fn a_late_probe_fills_only_an_unknown_version() {
        let probe = |version: Option<&str>| AdapterTestResult {
            ok: version.is_some(),
            version: version.map(str::to_owned),
            ..Default::default()
        };
        let mut usage = RunUsage::default();
        usage.fill_adapter_version(Some(&probe(None)));
        assert_eq!(usage.adapter_version, None);
        usage.fill_adapter_version(Some(&probe(Some("cli 2"))));
        assert_eq!(usage.adapter_version.as_deref(), Some("cli 2"));
        usage.fill_adapter_version(Some(&probe(Some("cli 3"))));
        assert_eq!(usage.adapter_version.as_deref(), Some("cli 2"));
        usage.fill_adapter_version(None);
        assert_eq!(usage.adapter_version.as_deref(), Some("cli 2"));
    }

    #[test]
    fn partial_report_does_not_invent_missing_counters() {
        let usage = observed(
            "claude",
            json!({"type":"result", "usage":{"output_tokens":9}}),
        );
        assert_eq!(counters(&usage), [None, Some(9), None, None, None]);
    }

    #[test]
    fn explicit_zero_survives_for_every_counter() {
        let usage = observed(
            "codex",
            json!({"type":"turn.failed", "usage":{
                "input_tokens":0, "output_tokens":0, "reasoning_tokens":0,
                "cache_read_input_tokens":0, "cache_creation_input_tokens":0
            }}),
        );
        assert_eq!(counters(&usage), [Some(0); 5]);
    }

    #[test]
    fn codex_native_counters_preserve_overlap_and_snapshot_semantics() {
        // Cached input is included in input, and reasoning output in output.
        // Neither the subtotals nor a provider total change the reported totals.
        let output = json!({"type":"turn.completed", "usage":{
            "input_tokens":100, "cached_input_tokens":60,
            "output_tokens":20, "reasoning_output_tokens":7, "total_tokens":120
        }});
        let mut usage = observed("codex", output.clone());
        let expected = [Some(100), Some(20), Some(7), Some(60), None];
        assert_eq!(counters(&usage), expected);
        usage.observe_output("codex", &output);
        assert_eq!(counters(&usage), expected);
        usage.observe_output("codex", &json!({"type":"turn.failed", "usage":{}}));
        assert_eq!(counters(&usage), expected);
        usage.observe_output(
            "codex",
            &json!({"type":"turn.failed", "usage":{
                "cached_input_tokens":0, "reasoning_output_tokens":0
            }}),
        );
        assert_eq!(
            counters(&usage),
            [Some(100), Some(20), Some(0), Some(0), None]
        );
    }

    #[test]
    fn codex_native_zero_and_absence_are_distinct_without_input_or_output() {
        for kind in ["turn.completed", "turn.failed", "error"] {
            assert_eq!(
                counters(&observed(
                    "codex",
                    json!({"type":kind, "usage":{
                        "cached_input_tokens":0, "reasoning_output_tokens":0
                    }})
                )),
                [None, None, Some(0), Some(0), None]
            );
            assert_eq!(
                counters(&observed("codex", json!({"type":kind, "usage":{}}))),
                [None; 5]
            );
        }
    }

    #[test]
    fn codex_native_counters_validate_signed_integer_range() {
        for value in [
            json!(null),
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!("3"),
            json!(true),
            json!(u64::MAX),
        ] {
            assert_eq!(
                counters(&observed(
                    "codex",
                    json!({"type":"turn.completed", "usage":{
                        "cached_input_tokens":value, "reasoning_output_tokens":value
                    }})
                )),
                [None; 5],
                "{value}"
            );
        }
        assert_eq!(
            counters(&observed(
                "codex",
                json!({"type":"turn.completed", "usage":{
                    "cached_input_tokens":i64::MAX, "reasoning_output_tokens":i64::MAX
                }})
            )),
            [None, None, Some(i64::MAX), Some(i64::MAX), None]
        );
    }

    #[test]
    fn codex_native_aliases_take_first_valid_value_and_are_adapter_specific() {
        let output = json!({"type":"turn.completed", "usage":{
            "cached_input_tokens":0, "cache_read_input_tokens":9, "cacheReadTokens":10,
            "reasoning_output_tokens":0, "reasoning_tokens":8, "reasoningTokens":11
        }});
        assert_eq!(
            counters(&observed("codex", output)),
            [None, None, Some(0), Some(0), None]
        );
        let usage = json!({"cached_input_tokens":null, "cache_read_input_tokens":9,
            "reasoning_output_tokens":-1, "reasoningTokens":8});
        assert_eq!(
            counters(&observed(
                "codex",
                json!({"type":"turn.completed", "usage":usage})
            )),
            [None, None, Some(8), Some(9), None]
        );
        for adapter in ["claude", "cursor", "grok"] {
            let kind = if adapter == "grok" { "end" } else { "result" };
            assert_eq!(
                counters(&observed(
                    adapter,
                    json!({"type":kind, "usage":{
                        "cached_input_tokens":6, "reasoning_output_tokens":4
                    }})
                )),
                [None; 5],
                "{adapter}"
            );
        }
    }

    #[test]
    fn cli_model_overrides_requested_model_even_on_failure() {
        let mut usage = RunUsage::new(Some("requested"), Some(&AdapterTestResult::ok("cli 7")));
        usage.observe_output(
            "codex",
            &json!({"type":"turn.failed", "model":"reported", "usage":{"input_tokens":7}}),
        );
        assert_eq!(
            usage,
            RunUsage {
                tokens_input: Some(7),
                model: Some("reported".into()),
                adapter_version: Some("cli 7".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn null_and_malformed_usage_remain_unknown() {
        for value in [
            Value::Null,
            json!({}),
            json!([]),
            json!("usage"),
            json!(true),
        ] {
            assert_eq!(
                observed("claude", json!({"type":"result", "usage":value})),
                RunUsage::default()
            );
        }
    }

    #[test]
    fn invalid_numbers_are_never_coerced_into_zero_or_rounded() {
        for value in [
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!("3"),
            json!(true),
            json!(u64::MAX),
        ] {
            assert_eq!(
                observed(
                    "codex",
                    json!({"type":"turn.completed", "usage":{
                        "input_tokens":value, "output_tokens":value, "reasoning_tokens":value,
                        "cache_read_input_tokens":value, "cache_creation_input_tokens":value
                    }})
                ),
                RunUsage::default(),
                "{value}"
            );
        }
    }

    #[test]
    fn sqlite_integer_maximum_is_preserved_exactly() {
        let usage = observed(
            "codex",
            json!({"type":"turn.completed", "usage":{"input_tokens":i64::MAX}}),
        );
        assert_eq!(usage.tokens_input, Some(i64::MAX));
    }

    #[test]
    fn aliases_use_first_valid_value_and_zero_wins() {
        let usage = observed(
            "cursor",
            json!({"type":"result", "usage":{
                "input_tokens":0, "inputTokens":9, "output_tokens":"invalid", "outputTokens":3,
                "reasoningTokens":4, "cacheReadTokens":5, "cacheWriteTokens":6
            }}),
        );
        assert_eq!(
            counters(&usage),
            [Some(0), Some(3), Some(4), Some(5), Some(6)]
        );
    }

    #[test]
    fn stats_fallback_is_claude_only_and_direct_usage_takes_precedence() {
        let output = json!({"type":"result", "stats":{"usage":{"input_tokens":8}}});
        assert_eq!(observed("claude", output.clone()).tokens_input, Some(8));
        assert_eq!(observed("cursor", output.clone()).tokens_input, None);
        let mut direct = output;
        direct["usage"] = Value::Null;
        assert_eq!(observed("claude", direct).tokens_input, None);
    }

    #[test]
    fn pi_last_assistant_failure_preserves_zero_and_ignores_tool_usage() {
        let usage = observed(
            "pi",
            json!({"type":"agent_end", "messages":[
                {"role":"assistant", "usage":{"input":99}},
                {"role":"assistant", "model":"actual", "stopReason":"aborted", "usage":{
                    "input":0, "output_tokens":4, "reasoning":2, "cacheRead":0, "cache_write":5}},
                {"role":"toolResult", "model":"wrong", "usage":{"input":999}}
            ]}),
        );
        assert_eq!(
            usage,
            RunUsage {
                tokens_input: Some(0),
                tokens_output: Some(4),
                tokens_reasoning: Some(2),
                tokens_cache_read: Some(0),
                tokens_cache_write: Some(5),
                model: Some("actual".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn pi_passive_all_zero_report_is_preserved_before_legacy_parser_drops_it() {
        let usage = observed(
            "pi",
            json!({"type":"message_end", "usage":{"input":0, "output":0}}),
        );
        assert_eq!(counters(&usage), [Some(0), Some(0), None, None, None]);
    }

    #[test]
    fn opencode_error_retains_all_supplied_counters() {
        let usage = observed(
            "opencode",
            json!({"type":"step_finish", "part":{
                "reason":"error", "tokens":{"input":2, "output":3, "reasoning":0, "cache":{"read":4, "write":5}}
            }}),
        );
        assert_eq!(
            counters(&usage),
            [Some(2), Some(3), Some(0), Some(4), Some(5)]
        );
    }

    #[test]
    fn replaying_a_snapshot_does_not_double_count_and_zero_replaces_old_count() {
        let output = json!({"type":"turn.completed", "usage":{"input_tokens":9}});
        let mut usage = observed("codex", output.clone());
        usage.observe_output("codex", &output);
        assert_eq!(usage.tokens_input, Some(9));
        usage.observe_output(
            "codex",
            &json!({"type":"turn.failed", "usage":{"input_tokens":0}}),
        );
        assert_eq!(usage.tokens_input, Some(0));
    }

    #[test]
    fn absent_later_metadata_does_not_erase_known_values() {
        let mut usage = RunUsage::new(Some("requested"), Some(&AdapterTestResult::ok("cli 1")));
        usage.observe_output(
            "codex",
            &json!({"type":"turn.failed", "usage":{"input_tokens":4}}),
        );
        let previous = usage.clone();
        usage.observe_output("codex", &json!({"type":"error", "model":null}));
        assert_eq!(usage, previous);
    }

    #[test]
    fn plain_text_adapters_and_unknown_kinds_never_interpret_json_as_usage() {
        for adapter in ["shell", "antigravity", "custom"] {
            assert_eq!(
                observed(
                    adapter,
                    json!({"type":"result", "model":"spoof", "usage":{"input_tokens":9}})
                ),
                RunUsage::default()
            );
        }
    }

    #[test]
    fn tool_output_and_nested_raw_cannot_supply_metadata() {
        for adapter in ["claude", "cursor", "codex", "opencode", "pi", "grok"] {
            assert_eq!(
                observed(
                    adapter,
                    json!({"type":"tool_result", "model":"spoof", "usage":{"input_tokens":9},
                "raw":{"type":"result", "usage":{"input_tokens":99}}})
                ),
                RunUsage::default()
            );
        }
    }

    #[test]
    fn emitted_error_event_retains_raw_usage_but_legacy_tokens_are_not_a_fallback() {
        let event = AgentEvent::new(
            AgentEventKind::Error,
            "synthetic",
            json!({"message":"failed", "raw":{
                "type":"turn.failed", "usage":{"output_tokens":0}
            }}),
        )
        .with_tokens(orch_domain::create_token_usage(0, 0, None));
        let mut usage = RunUsage::default();
        usage.observe_event("codex", &event);
        assert_eq!(counters(&usage), [None, Some(0), None, None, None]);
    }

    #[test]
    fn lossy_legacy_tokens_alone_remain_unknown() {
        let event = AgentEvent::new(AgentEventKind::Done, "synthetic", json!({}))
            .with_tokens(orch_domain::create_token_usage(0, 0, None));
        let mut usage = RunUsage::default();
        usage.observe_event("codex", &event);
        assert_eq!(usage, RunUsage::default());
    }

    #[test]
    fn command_and_tool_call_events_are_never_metadata() {
        for kind in [
            AgentEventKind::Command,
            AgentEventKind::ToolCall,
            AgentEventKind::FileChange,
        ] {
            let event = AgentEvent::new(
                kind,
                "synthetic",
                json!({"raw":{"type":"result", "model":"spoof", "usage":{"input_tokens":9}}}),
            );
            let mut usage = RunUsage::default();
            usage.observe_event("claude", &event);
            assert_eq!(usage, RunUsage::default());
        }
    }

    #[test]
    fn emitted_assistant_and_opencode_part_shapes_keep_metadata() {
        let mut usage = RunUsage::default();
        usage.observe_event(
            "claude",
            &AgentEvent::new(
                AgentEventKind::Output,
                "synthetic",
                json!({"role":"assistant", "model":"reported"}),
            ),
        );
        usage.observe_event(
            "opencode",
            &AgentEvent::new(
                AgentEventKind::Error,
                "synthetic",
                json!({"type":"step-finish", "tokens":{"output":3}}),
            ),
        );
        assert_eq!(
            usage,
            RunUsage {
                model: Some("reported".into()),
                tokens_output: Some(3),
                ..Default::default()
            }
        );
    }

    #[test]
    fn opencode_emitted_part_does_not_require_a_redundant_type() {
        let event = AgentEvent::new(
            AgentEventKind::Error,
            "synthetic",
            json!({
                "reason":"error", "tokens":{"input":7}
            }),
        );
        let mut usage = RunUsage::default();
        usage.observe_event("opencode", &event);
        assert_eq!(counters(&usage), [Some(7), None, None, None, None]);
    }

    #[test]
    fn outer_model_is_used_when_a_message_model_is_blank() {
        let usage = observed(
            "pi",
            json!({"type":"agent_end", "model":"outer", "messages":[
                {"role":"assistant", "model":"   "}
            ]}),
        );
        assert_eq!(usage.model.as_deref(), Some("outer"));
    }

    #[test]
    fn blank_and_oversized_metadata_are_unknown_without_truncation() {
        let oversized = "x".repeat(64 * 1024 + 1);
        for text in ["", "  \n", oversized.as_str()] {
            assert_eq!(
                RunUsage::new(Some(text), Some(&AdapterTestResult::ok(text))),
                RunUsage::default()
            );
        }
        let exact = "x".repeat(64 * 1024);
        assert_eq!(
            RunUsage::new(Some(&exact), None).model.as_deref(),
            Some(exact.as_str())
        );
    }

    // Existing recorded sessions are held out from the hand-written edge cases
    // above. Expected values come from the source JSON, not legacy totals.
    #[test]
    fn existing_recorded_sessions_preserve_presence_and_values() {
        let cases = [
            (
                "claude",
                include_str!("../../../fixtures/adapters/claude/session.jsonl"),
                [Some(100), Some(50), None, Some(10), None],
            ),
            (
                "cursor",
                include_str!("../../../fixtures/adapters/cursor/session.jsonl"),
                [Some(50), Some(25), None, Some(10), Some(5)],
            ),
            (
                "codex",
                include_str!("../../../fixtures/adapters/codex/session.jsonl"),
                [Some(200), Some(80), None, None, None],
            ),
            (
                "opencode",
                include_str!("../../../fixtures/adapters/opencode/session.jsonl"),
                [Some(12416), Some(2), Some(0), None, None],
            ),
            (
                "pi",
                include_str!("../../../fixtures/adapters/pi/session.jsonl"),
                [Some(42), Some(17), Some(3), Some(5), Some(0)],
            ),
        ];
        for (adapter, recorded, expected) in cases {
            let mut usage = RunUsage::default();
            for line in recorded.lines() {
                usage.observe_output(adapter, &serde_json::from_str::<Value>(line).unwrap());
            }
            assert_eq!(counters(&usage), expected, "{adapter}");
        }
    }

    #[test]
    fn existing_emitted_event_fixtures_recover_the_same_nullable_counters() {
        let cases = [
            (
                "claude",
                include_str!("../../../fixtures/adapters/claude/session.expected.json"),
                [Some(100), Some(50), None, Some(10), None],
            ),
            (
                "cursor",
                include_str!("../../../fixtures/adapters/cursor/session.expected.json"),
                [Some(50), Some(25), None, Some(10), Some(5)],
            ),
            (
                "codex",
                include_str!("../../../fixtures/adapters/codex/session.expected.json"),
                [Some(200), Some(80), None, None, None],
            ),
            (
                "opencode",
                include_str!("../../../fixtures/adapters/opencode/session.expected.json"),
                [Some(12416), Some(2), Some(0), None, None],
            ),
            (
                "pi",
                include_str!("../../../fixtures/adapters/pi/session.expected.json"),
                [Some(42), Some(17), Some(3), Some(5), Some(0)],
            ),
        ];
        for (adapter, recorded, expected) in cases {
            let fixture: Value = serde_json::from_str(recorded).unwrap();
            let mut usage = RunUsage::default();
            for mut value in fixture["events"].as_array().unwrap().iter().cloned() {
                value["timestamp"] = json!("synthetic");
                usage.observe_event(
                    adapter,
                    &serde_json::from_value::<AgentEvent>(value).unwrap(),
                );
            }
            assert_eq!(counters(&usage), expected, "{adapter}");
        }
    }
}
