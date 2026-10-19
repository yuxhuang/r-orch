//! Orchestrator runtime state, persisted in `.orchestry/state.json`.

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::run::TokenUsage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningEntry {
    pub run_id: String,
    pub agent_id: String,
    pub task_id: String,
    pub pid: u32,
    pub started_at: String,
    pub last_event_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<crate::remote::RemoteRunRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryEntry {
    pub task_id: String,
    pub attempt: u32,
    pub due_at: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateStats {
    pub total_runs: u64,
    pub total_tasks_completed: u64,
    pub total_tasks_failed: u64,
    pub total_tokens: TokenUsage,
    pub total_runtime_ms: u64,
}

/// Field order is the serialized field order.
///
/// `claimed` is a JS `Set` in memory and a JSON **array** on disk. The `{}`
/// form seen in `status --json` is a serialization quirk that is
/// kept by the CLI renderer, not by this type.
///
/// `running` and `claimed` use insertion-ordered containers so that a
/// read-modify-write round trip reproduces the iteration order exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorState {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(
        rename = "onboardingCompleted",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub onboarding_completed: Option<bool>,
    #[serde(default)]
    pub running: IndexMap<String, RunningEntry>,
    #[serde(default)]
    pub claimed: IndexSet<String>,
    #[serde(default)]
    pub retry_queue: Vec<RetryEntry>,
    #[serde(default)]
    pub stats: StateStats,
}

/// The default state. A function rather than a `const` because the
/// collections allocate; each call returns a fresh value.
pub fn default_state() -> OrchestratorState {
    OrchestratorState {
        version: 1,
        pid: None,
        started_at: None,
        onboarding_completed: Some(false),
        running: IndexMap::new(),
        claimed: IndexSet::new(),
        retry_queue: Vec::new(),
        stats: StateStats::default(),
    }
}

impl Default for OrchestratorState {
    fn default() -> Self {
        default_state()
    }
}

impl OrchestratorState {
    /// Field-by-field tolerant read of a raw `state.json` value.
    ///
    /// The state read never fails on a malformed file: every field
    /// falls back to its default independently, `onboardingCompleted` is
    /// coerced to `false` unless it is strictly a boolean, `running` must be an
    /// object, `claimed` and `retry_queue` must be arrays, and
    /// `stats.total_tokens` is merged field-wise over the zero default. A plain `serde` derive would reject all of those, so
    /// the coercion is explicit here and shared by every caller.
    pub fn from_raw(raw: Option<&Value>) -> OrchestratorState {
        let defaults = default_state();
        let Some(Value::Object(raw)) = raw else {
            return defaults;
        };

        let stats = raw.get("stats").and_then(Value::as_object);
        let stats_u64 = |key: &str, fallback: u64| {
            stats
                .and_then(|s| s.get(key))
                .and_then(Value::as_u64)
                .unwrap_or(fallback)
        };

        OrchestratorState {
            version: raw
                .get("version")
                .and_then(Value::as_u64)
                .map(|v| v as u32)
                .unwrap_or(defaults.version),
            pid: raw.get("pid").and_then(Value::as_u64).map(|v| v as u32),
            started_at: raw
                .get("started_at")
                .and_then(Value::as_str)
                .map(str::to_owned),
            // Coerced to false unless strictly a boolean.
            onboarding_completed: Some(
                raw.get("onboardingCompleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
            running: raw
                .get("running")
                .filter(|v| v.is_object())
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.running),
            claimed: raw
                .get("claimed")
                .filter(|v| v.is_array())
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.claimed),
            retry_queue: raw
                .get("retry_queue")
                .filter(|v| v.is_array())
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.retry_queue),
            stats: StateStats {
                total_runs: stats_u64("total_runs", defaults.stats.total_runs),
                total_tasks_completed: stats_u64(
                    "total_tasks_completed",
                    defaults.stats.total_tasks_completed,
                ),
                total_tasks_failed: stats_u64(
                    "total_tasks_failed",
                    defaults.stats.total_tasks_failed,
                ),
                total_tokens: merge_token_usage(
                    defaults.stats.total_tokens,
                    stats.and_then(|s| s.get("total_tokens")),
                ),
                total_runtime_ms: stats_u64("total_runtime_ms", defaults.stats.total_runtime_ms),
            },
        }
    }
}

/// `{ ...defaults.stats.total_tokens, ...(raw.stats?.total_tokens ?? {}) }`.
fn merge_token_usage(base: TokenUsage, raw: Option<&Value>) -> TokenUsage {
    let Some(Value::Object(raw)) = raw else {
        return base;
    };
    let field = |key: &str, fallback: u64| raw.get(key).and_then(Value::as_u64).unwrap_or(fallback);
    TokenUsage {
        input: field("input", base.input),
        output: field("output", base.output),
        reasoning: field("reasoning", base.reasoning),
        total: field("total", base.total),
        cache_read: field("cache_read", base.cache_read),
        cache_write: field("cache_write", base.cache_write),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_has_the_expected_bytes() {
        // With the stats of a fresh project.
        let state = default_state();
        assert_eq!(
            serde_json::to_string_pretty(&state).unwrap(),
            r#"{
  "version": 1,
  "onboardingCompleted": false,
  "running": {},
  "claimed": [],
  "retry_queue": [],
  "stats": {
    "total_runs": 0,
    "total_tasks_completed": 0,
    "total_tasks_failed": 0,
    "total_tokens": {
      "input": 0,
      "output": 0,
      "reasoning": 0,
      "total": 0,
      "cache_read": 0,
      "cache_write": 0
    },
    "total_runtime_ms": 0
  }
}"#
        );
    }

    #[test]
    fn claimed_serializes_as_an_array_on_disk() {
        let mut state = default_state();
        state.claimed.insert("tsk_b".into());
        state.claimed.insert("tsk_a".into());
        let value = serde_json::to_value(&state).unwrap();
        // Insertion order, like a JS Set.
        assert_eq!(value["claimed"], serde_json::json!(["tsk_b", "tsk_a"]));
    }

    #[test]
    fn missing_file_yields_defaults() {
        assert_eq!(OrchestratorState::from_raw(None), default_state());
    }

    #[test]
    fn non_boolean_onboarding_completed_coerces_to_false() {
        for raw in ["\"yes\"", "1", "null"] {
            let value: Value =
                serde_json::from_str(&format!(r#"{{"onboardingCompleted":{raw}}}"#)).unwrap();
            let state = OrchestratorState::from_raw(Some(&value));
            assert_eq!(state.onboarding_completed, Some(false), "raw={raw}");
        }
        let value: Value = serde_json::from_str(r#"{"onboardingCompleted":true}"#).unwrap();
        assert_eq!(
            OrchestratorState::from_raw(Some(&value)).onboarding_completed,
            Some(true)
        );
    }

    #[test]
    fn non_array_claimed_and_retry_queue_fall_back_to_empty() {
        let value: Value =
            serde_json::from_str(r#"{"claimed":{"a":1},"retry_queue":"nope"}"#).unwrap();
        let state = OrchestratorState::from_raw(Some(&value));
        assert!(state.claimed.is_empty());
        assert!(state.retry_queue.is_empty());
    }

    #[test]
    fn non_object_running_falls_back_to_empty() {
        let value: Value = serde_json::from_str(r#"{"running":[1,2,3]}"#).unwrap();
        assert!(OrchestratorState::from_raw(Some(&value)).running.is_empty());
    }

    #[test]
    fn total_tokens_merges_field_wise() {
        let value: Value =
            serde_json::from_str(r#"{"stats":{"total_tokens":{"input":5,"total":5}}}"#).unwrap();
        let tokens = OrchestratorState::from_raw(Some(&value)).stats.total_tokens;
        assert_eq!(tokens.input, 5);
        assert_eq!(tokens.total, 5);
        assert_eq!(tokens.output, 0);
        assert_eq!(tokens.cache_read, 0);
    }

    #[test]
    fn a_corrupt_running_entry_does_not_lose_the_whole_file() {
        // One malformed entry makes `running` undeserializable, so it falls
        // back to its default — the important property is that the read still
        // succeeds.
        let value: Value = serde_json::from_str(
            r#"{"version":1,"running":{"tsk_1":{"run_id":"run_1"}},"stats":{"total_runs":9}}"#,
        )
        .unwrap();
        let state = OrchestratorState::from_raw(Some(&value));
        assert!(state.running.is_empty());
        assert_eq!(state.stats.total_runs, 9);
    }

    #[test]
    fn full_round_trip_preserves_every_field() {
        let mut state = default_state();
        state.pid = Some(4242);
        state.started_at = Some("2026-10-13T16:37:33.426Z".into());
        state.claimed.insert("tsk_1".into());
        state.running.insert(
            "tsk_1".into(),
            RunningEntry {
                remote: None,
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                pid: 99,
                started_at: "t0".into(),
                last_event_at: "t1".into(),
            },
        );
        state.retry_queue.push(RetryEntry {
            task_id: "tsk_2".into(),
            attempt: 1,
            due_at: "t2".into(),
            error: "boom".into(),
        });
        state.stats.total_runs = 3;

        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(OrchestratorState::from_raw(Some(&value)), state);
    }
}
