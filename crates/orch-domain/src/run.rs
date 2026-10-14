//! Run domain model — one execution attempt of a task by an agent
//! (`src/domain/run.ts`). Events live in a sibling `.jsonl` file, never in the
//! `Run` record.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Preparing,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

impl RunStatus {
    pub const ALL: [RunStatus; 6] = [
        RunStatus::Preparing,
        RunStatus::Running,
        RunStatus::Succeeded,
        RunStatus::Failed,
        RunStatus::TimedOut,
        RunStatus::Cancelled,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Preparing => "preparing",
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::TimedOut => "timed_out",
            RunStatus::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RunStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunStatus::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// Field order mirrors the TS `Run` interface (PARITY.md §6.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub task_id: String,
    pub agent_id: String,
    pub attempt: u32,
    pub status: RunStatus,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub workspace_path: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
}

/// `total == input + output + reasoning`. The cache counters are informational
/// and are deliberately *not* part of the total (PARITY.md §6.9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub total: u64,
    /// Cache tokens — informational only, NOT added to total (subset of input).
    pub cache_read: u64,
    pub cache_write: u64,
}

/// Optional arguments of [`create_token_usage`], mirroring the TS `opts` bag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsageOpts {
    pub reasoning: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

/// Build a [`TokenUsage`] with `total` always recomputed as
/// `input + output + reasoning` — never accumulated.
pub fn create_token_usage(input: u64, output: u64, opts: Option<TokenUsageOpts>) -> TokenUsage {
    let opts = opts.unwrap_or_default();
    let reasoning = opts.reasoning.unwrap_or(0);
    TokenUsage {
        input,
        output,
        reasoning,
        // Saturating: these three come from an adapter's own usage report,
        // which is attacker-adjacent (a hostile or buggy agent CLI prints
        // whatever it likes) and is also re-read from `state.json`, which can
        // be hand-edited. A debug build would panic and a release build would
        // wrap, and a wrapped total is worse than a clamped one — it reads as
        // a plausible small number.
        total: input.saturating_add(output).saturating_add(reasoning),
        cache_read: opts.cache_read.unwrap_or(0),
        cache_write: opts.cache_write.unwrap_or(0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEventType {
    AgentOutput,
    FileChanged,
    CommandRun,
    ToolCall,
    Error,
    Done,
}

impl RunEventType {
    pub const ALL: [RunEventType; 6] = [
        RunEventType::AgentOutput,
        RunEventType::FileChanged,
        RunEventType::CommandRun,
        RunEventType::ToolCall,
        RunEventType::Error,
        RunEventType::Done,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RunEventType::AgentOutput => "agent_output",
            RunEventType::FileChanged => "file_changed",
            RunEventType::CommandRun => "command_run",
            RunEventType::ToolCall => "tool_call",
            RunEventType::Error => "error",
            RunEventType::Done => "done",
        }
    }
}

impl std::fmt::Display for RunEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RunEventType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunEventType::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// One line of a `<run-id>.jsonl` file. Key order `timestamp, type, data` is
/// the emitted order (PARITY.md §6.9).
///
/// `data` is typed as a JSON value because the on-disk type is TS `unknown`,
/// but the orchestrator pre-serializes it: every line it writes carries a
/// **string** `data`, truncated to 8192 characters with a trailing `…`.
/// Reads must stay tolerant of non-strings because older files and the
/// adapters' own `AgentEvent` payloads are structured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    pub timestamp: String,
    #[serde(rename = "type")]
    pub event_type: RunEventType,
    pub data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(
        reasoning: Option<u64>,
        cache_read: Option<u64>,
        cache_write: Option<u64>,
    ) -> Option<TokenUsageOpts> {
        Some(TokenUsageOpts {
            reasoning,
            cache_read,
            cache_write,
        })
    }

    #[test]
    fn returns_correct_input_and_output() {
        let result = create_token_usage(100, 200, None);
        assert_eq!(result.input, 100);
        assert_eq!(result.output, 200);
    }

    #[test]
    fn computes_total_as_input_plus_output_without_reasoning() {
        let result = create_token_usage(100, 200, None);
        assert_eq!(result.total, 300);
        assert_eq!(result.reasoning, 0);
    }

    #[test]
    fn includes_reasoning_in_total() {
        let result = create_token_usage(100, 200, opts(Some(50), None, None));
        assert_eq!(result.reasoning, 50);
        assert_eq!(result.total, 350);
    }

    #[test]
    fn tracks_cache_tokens_without_adding_to_total() {
        let result = create_token_usage(100, 200, opts(None, Some(80), Some(20)));
        assert_eq!(result.cache_read, 80);
        assert_eq!(result.cache_write, 20);
        assert_eq!(result.total, 300);
    }

    #[test]
    fn zero_case() {
        let result = create_token_usage(0, 0, None);
        assert_eq!(result.total, 0);
        assert_eq!(result.reasoning, 0);
        assert_eq!(result.cache_read, 0);
        assert_eq!(result.cache_write, 0);
    }

    #[test]
    fn total_is_never_accumulated_across_calls() {
        let a = create_token_usage(500, 300, opts(Some(100), None, None));
        assert_eq!(a.total, 900);
        let b = create_token_usage(1, 1, None);
        assert_eq!(b.total, 2);
        assert_eq!(a.total, 900);
    }

    #[test]
    fn defaults_all_optional_fields_to_zero() {
        let result = create_token_usage(10, 20, None);
        assert_eq!(result.reasoning, 0);
        assert_eq!(result.cache_read, 0);
        assert_eq!(result.cache_write, 0);
        assert_eq!(result.total, 30);
    }

    #[test]
    fn run_event_line_matches_the_reference_bytes() {
        let event = RunEvent {
            timestamp: "2026-10-13T16:37:59.169Z".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::Value::String("hello".into()),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"timestamp":"2026-10-13T16:37:59.169Z","type":"agent_output","data":"hello"}"#
        );
    }

    #[test]
    fn run_omits_unset_optionals() {
        let run = Run {
            id: "run_rKQA8ah".into(),
            task_id: "tsk_i4hkLPH".into(),
            agent_id: "agt_veWG_kK".into(),
            attempt: 1,
            status: RunStatus::Succeeded,
            started_at: "2026-10-13T16:37:59.145Z".into(),
            finished_at: Some("2026-10-13T16:37:59.175Z".into()),
            workspace_path: "/abs/path".into(),
            prompt: "p".into(),
            pid: Some(360_135),
            error: None,
            tokens: None,
        };
        let json = serde_json::to_value(&run).unwrap();
        assert!(json.get("error").is_none());
        assert!(json.get("tokens").is_none());
        assert_eq!(json["pid"], 360_135);
    }

    #[test]
    fn run_event_type_strings_round_trip() {
        for t in RunEventType::ALL {
            assert_eq!(
                serde_json::to_string(&t).unwrap(),
                format!("\"{}\"", t.as_str())
            );
            assert_eq!(t.as_str().parse::<RunEventType>(), Ok(t));
        }
    }

    #[test]
    fn run_status_strings_round_trip() {
        for s in RunStatus::ALL {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
            assert_eq!(s.as_str().parse::<RunStatus>(), Ok(s));
        }
    }
}
