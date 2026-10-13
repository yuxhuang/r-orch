//! Typed error hierarchy (`src/domain/errors.ts`).
//!
//! The TS `OrchestryError` carries `(message, exitCode, hint?)` and each
//! subclass specializes it. Here it is one enum: the `#[error(...)]` strings
//! **are** the user-facing messages from PARITY.md §3.1, so the contract is
//! hard to drift from, and [`DomainError::exit_code`] / [`DomainError::hint`]
//! are the only two things the CLI error boundary needs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::TaskStatus;

pub type Result<T> = std::result::Result<T, DomainError>;

/// Exit codes (PARITY.md §3):
/// `1` general, `2` invalid arguments, `3` not initialized, `4` lock conflict,
/// `5` agent adapter unavailable, `6` workspace error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("Not initialized")]
    NotInitialized,

    #[error("{0}")]
    InvalidArguments(String),

    #[error("Orchestrator already running (PID: {0})")]
    LockConflict(u32),

    #[error("Agent adapter \"{adapter}\" not available")]
    AgentAdapter { adapter: String, detail: String },

    #[error("No agents configured")]
    NoAgents,

    #[error("Task not found: {0}")]
    TaskNotFound(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Task {task_id} is already running (run: {run_id}, agent: {agent})")]
    TaskAlreadyRunning {
        task_id: String,
        run_id: String,
        agent: String,
    },

    /// The arrow is U+2192 and is **not** ASCII-folded under `--ascii`: it is
    /// produced in the domain layer, which has no knowledge of the flag
    /// (PARITY.md §3.1).
    #[error("Invalid transition for {task_id}: {from} → {to}")]
    InvalidTransition {
        task_id: String,
        from: TaskStatus,
        to: TaskStatus,
    },

    #[error("Goal not found: {0}")]
    GoalNotFound(String),

    #[error("Cannot mark goal {id} as achieved: {count} task(s) still pending — {summary}")]
    GoalHasPendingTasks {
        id: String,
        count: usize,
        summary: String,
    },

    #[error("Team not found: {0}")]
    TeamNotFound(String),

    #[error("Message not found: {0}")]
    MessageNotFound(String),

    #[error("{message}")]
    Workspace {
        message: String,
        hint: Option<String>,
    },

    /// Anything that is a plain `throw new Error(...)` in the original, and
    /// every infrastructure error converted at a service boundary. Exit 1.
    #[error("{0}")]
    Other(String),
}

impl DomainError {
    pub fn exit_code(&self) -> i32 {
        match self {
            DomainError::InvalidArguments(_) => 2,
            DomainError::NotInitialized => 3,
            DomainError::LockConflict(_) => 4,
            DomainError::AgentAdapter { .. } => 5,
            DomainError::Workspace { .. } => 6,
            DomainError::NoAgents
            | DomainError::TaskNotFound(_)
            | DomainError::AgentNotFound(_)
            | DomainError::TaskAlreadyRunning { .. }
            | DomainError::InvalidTransition { .. }
            | DomainError::GoalNotFound(_)
            | DomainError::GoalHasPendingTasks { .. }
            | DomainError::TeamNotFound(_)
            | DomainError::MessageNotFound(_)
            | DomainError::Other(_) => 1,
        }
    }

    /// The second line of the error block (PARITY.md §3.1), or `None` when the
    /// error has no hint.
    pub fn hint(&self) -> Option<String> {
        match self {
            DomainError::NotInitialized => Some("Run: orch init".into()),
            DomainError::LockConflict(_) => Some("Use: orch status".into()),
            DomainError::AgentAdapter { detail, .. } => Some(detail.clone()),
            DomainError::NoAgents => Some("Run: orch agent add <name> --adapter <adapter>".into()),
            DomainError::TaskAlreadyRunning { task_id, .. } => {
                Some(format!("Use: orch logs --task {task_id} --follow"))
            }
            DomainError::GoalHasPendingTasks { .. } => {
                Some("Use --force to cancel pending tasks and mark achieved".into())
            }
            DomainError::Workspace { hint, .. } => hint.clone(),
            DomainError::InvalidArguments(_)
            | DomainError::TaskNotFound(_)
            | DomainError::AgentNotFound(_)
            | DomainError::InvalidTransition { .. }
            | DomainError::GoalNotFound(_)
            | DomainError::TeamNotFound(_)
            | DomainError::MessageNotFound(_)
            | DomainError::Other(_) => None,
        }
    }

    /// The `name` of the corresponding TS error class. Asserted by the ported
    /// `errors.test.ts` and used by serve-mode structured logging.
    pub fn name(&self) -> &'static str {
        match self {
            DomainError::NotInitialized => "NotInitializedError",
            DomainError::InvalidArguments(_) => "InvalidArgumentsError",
            DomainError::LockConflict(_) => "LockConflictError",
            DomainError::AgentAdapter { .. } => "AgentAdapterError",
            DomainError::NoAgents => "NoAgentsError",
            DomainError::TaskNotFound(_) => "TaskNotFoundError",
            DomainError::AgentNotFound(_) => "AgentNotFoundError",
            DomainError::TaskAlreadyRunning { .. } => "TaskAlreadyRunningError",
            DomainError::InvalidTransition { .. } => "InvalidTransitionError",
            DomainError::GoalNotFound(_) => "GoalNotFoundError",
            DomainError::GoalHasPendingTasks { .. } => "GoalHasPendingTasksError",
            DomainError::TeamNotFound(_) => "TeamNotFoundError",
            DomainError::MessageNotFound(_) => "MessageNotFoundError",
            DomainError::Workspace { .. } => "WorkspaceError",
            DomainError::Other(_) => "OrchestryError",
        }
    }
}

// ── Adapter error classification ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterErrorKind {
    AdapterNotFound,
    AuthFailed,
    Timeout,
    RateLimit,
    ProcessCrash,
    SpawnFailed,
    Unknown,
}

impl AdapterErrorKind {
    /// All seven variants, in TS enum declaration order.
    pub const ALL: [AdapterErrorKind; 7] = [
        AdapterErrorKind::AdapterNotFound,
        AdapterErrorKind::AuthFailed,
        AdapterErrorKind::Timeout,
        AdapterErrorKind::RateLimit,
        AdapterErrorKind::ProcessCrash,
        AdapterErrorKind::SpawnFailed,
        AdapterErrorKind::Unknown,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AdapterErrorKind::AdapterNotFound => "adapter_not_found",
            AdapterErrorKind::AuthFailed => "auth_failed",
            AdapterErrorKind::Timeout => "timeout",
            AdapterErrorKind::RateLimit => "rate_limit",
            AdapterErrorKind::ProcessCrash => "process_crash",
            AdapterErrorKind::SpawnFailed => "spawn_failed",
            AdapterErrorKind::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for AdapterErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AdapterErrorKind {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        AdapterErrorKind::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterErrorHint {
    pub message: &'static str,
    pub fix: &'static str,
    /// `None` rather than `Some(false)` where the TS object omits the key —
    /// `adapter-error-kind.test.ts` asserts `toBeUndefined()` for `auth_failed`.
    #[serde(
        rename = "doctorHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub doctor_hint: Option<bool>,
}

/// `ERROR_HINTS` (PARITY.md §8.9), in TS key order.
///
/// `timeout.fix` names `agent_timeout`, which is not a real config key — the
/// real path is `defaults.agent.timeout_ms`. The text is user-visible output
/// asserted by tests, so it is replicated verbatim (PARITY.md §14.3).
pub const ERROR_HINTS: [(AdapterErrorKind, AdapterErrorHint); 7] = [
    (
        AdapterErrorKind::AdapterNotFound,
        AdapterErrorHint {
            message: "CLI is not installed.",
            fix: "Install it with: npm i -g @anthropic-ai/claude-code",
            doctor_hint: Some(true),
        },
    ),
    (
        AdapterErrorKind::AuthFailed,
        AdapterErrorHint {
            message: "API key is invalid.",
            fix: "Check it with: claude auth status",
            doctor_hint: None,
        },
    ),
    (
        AdapterErrorKind::Timeout,
        AdapterErrorHint {
            message: "Agent timed out.",
            fix: "Increase the timeout with: orch config set agent_timeout <ms>",
            doctor_hint: None,
        },
    ),
    (
        AdapterErrorKind::RateLimit,
        AdapterErrorHint {
            message: "API rate limit reached.",
            fix: "Wait and retry with: orch task retry <id>",
            doctor_hint: None,
        },
    ),
    (
        AdapterErrorKind::ProcessCrash,
        AdapterErrorHint {
            message: "Agent process crashed.",
            fix: "Try again with: orch task retry <id>",
            doctor_hint: None,
        },
    ),
    (
        AdapterErrorKind::SpawnFailed,
        AdapterErrorHint {
            message: "Failed to start the process.",
            fix: "Check PATH and file permissions",
            doctor_hint: None,
        },
    ),
    (
        AdapterErrorKind::Unknown,
        AdapterErrorHint {
            message: "Unknown error.",
            fix: "Run: orch doctor",
            doctor_hint: Some(true),
        },
    ),
];

/// `ERROR_HINTS[kind]` — total, like the TS `Record`.
pub fn error_hint(kind: AdapterErrorKind) -> &'static AdapterErrorHint {
    ERROR_HINTS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, hint)| hint)
        .expect("ERROR_HINTS covers every AdapterErrorKind")
}

/// Classify an adapter failure from its message and (optional) exit code.
///
/// The check order is contract (PARITY.md §8.8): the first matching rule wins,
/// so `ENOENT` beats a non-zero exit code and an auth keyword beats a crash.
pub fn classify_adapter_error(error: &str, exit_code: Option<i32>) -> AdapterErrorKind {
    let lower = error.to_lowercase();
    let has = |needle: &str| lower.contains(needle);

    if has("enoent") || has("spawn failed") {
        return AdapterErrorKind::SpawnFailed;
    }
    if has("not found") || has("command not found") || has("no such file") {
        return AdapterErrorKind::AdapterNotFound;
    }
    if has("auth")
        || has("unauthorized")
        || has("401")
        || has("invalid api key")
        || has("authentication")
    {
        return AdapterErrorKind::AuthFailed;
    }
    if has("timeout") || has("timed out") || has("etimedout") {
        return AdapterErrorKind::Timeout;
    }
    if has("rate limit") || has("429") || has("too many requests") {
        return AdapterErrorKind::RateLimit;
    }
    if exit_code.is_some_and(|code| code != 0) {
        return AdapterErrorKind::ProcessCrash;
    }

    AdapterErrorKind::Unknown
}

/// Pull the human-facing message out of a provider error.
///
/// Some CLIs wrap an API error several times, including a JSON document
/// serialized inside a `message` string, so a string that parses as JSON is
/// re-entered. Keyed off `error`, `message`, `detail`, `reason` in that order
/// and bounded at depth 6, exactly like the TS original.
pub fn extract_error_message(value: &Value) -> String {
    extract_at_depth(value, 0)
}

const EXTRACT_KEYS: [&str; 4] = ["error", "message", "detail", "reason"];

fn extract_at_depth(value: &Value, depth: u32) -> String {
    if depth >= 6 {
        return stringify_error_value(value);
    }

    if let Value::String(s) = value {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(parsed) => extract_at_depth(&parsed, depth + 1),
            // Not JSON — the original (untrimmed) string is the message.
            Err(_) => s.clone(),
        };
    }

    // Arrays are objects in JS, but they carry none of the keys below, so they
    // fall through to the stringify at the end exactly as in the original.
    if let Value::Object(map) = value {
        for key in EXTRACT_KEYS {
            match map.get(key) {
                None | Some(Value::Null) => continue,
                Some(inner) => {
                    let message = extract_at_depth(inner, depth + 1);
                    if !message.is_empty() {
                        return message;
                    }
                }
            }
        }
    }

    stringify_error_value(value)
}

fn stringify_error_value(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Codex reports a recoverable model-metadata cache miss as an error item;
/// it must not fail the run.
///
/// Equivalent to
/// `/^Model metadata for .+ not found\. Defaulting to fallback metadata\b/i`
/// over the trimmed message. Hand-rolled rather than pulling in `regex` for
/// one pattern — `.` excludes line terminators and `\b` requires a non-word
/// character (or end of input) after `metadata`, both of which are checked.
pub fn is_model_metadata_warning(message: &str) -> bool {
    const PREFIX: &str = "Model metadata for ";
    const SUFFIX: &str = " not found. Defaulting to fallback metadata";

    let trimmed = message.trim();
    if trimmed.len() < PREFIX.len() || !trimmed[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        return false;
    }
    let rest = &trimmed[PREFIX.len()..];

    // `.+` is at least one character and may not span a line terminator.
    find_ascii_ci_all(rest, SUFFIX).any(|at| {
        at >= 1
            && !rest[..at].contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            && is_word_boundary_after(&rest[at + SUFFIX.len()..])
    })
}

/// `\b` after a word character: the next character must be a non-word one, or
/// the input must end.
fn is_word_boundary_after(rest: &str) -> bool {
    match rest.chars().next() {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || c == '_'),
    }
}

/// Byte offsets of every ASCII-case-insensitive occurrence of `needle`.
fn find_ascii_ci_all<'a>(haystack: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    haystack.char_indices().map(|(i, _)| i).filter(move |&i| {
        haystack.len() - i >= needle.len()
            && haystack.is_char_boundary(i + needle.len())
            && haystack[i..i + needle.len()].eq_ignore_ascii_case(needle)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── OrchestryError family ──────────────────────────────────────────────

    #[test]
    fn base_error_stores_message_and_exit_code() {
        let err = DomainError::Other("test error".into());
        assert_eq!(err.to_string(), "test error");
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.hint(), None);
        assert_eq!(err.name(), "OrchestryError");
    }

    #[test]
    fn not_initialized_has_exit_code_3_and_a_helpful_hint() {
        let err = DomainError::NotInitialized;
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.hint().as_deref(), Some("Run: orch init"));
        assert_eq!(err.to_string(), "Not initialized");
        assert_eq!(err.name(), "NotInitializedError");
    }

    #[test]
    fn invalid_arguments_has_exit_code_2() {
        let err = DomainError::InvalidArguments("bad input".into());
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.to_string(), "bad input");
        assert_eq!(err.name(), "InvalidArgumentsError");
    }

    #[test]
    fn lock_conflict_includes_the_pid_and_has_exit_code_4() {
        let err = DomainError::LockConflict(12345);
        assert_eq!(err.exit_code(), 4);
        assert_eq!(err.to_string(), "Orchestrator already running (PID: 12345)");
        assert_eq!(err.hint().as_deref(), Some("Use: orch status"));
        assert_eq!(err.name(), "LockConflictError");
    }

    #[test]
    fn agent_adapter_has_exit_code_5_with_the_detail_as_hint() {
        let err = DomainError::AgentAdapter {
            adapter: "claude".into(),
            detail: "not installed".into(),
        };
        assert_eq!(err.exit_code(), 5);
        assert_eq!(err.to_string(), "Agent adapter \"claude\" not available");
        assert_eq!(err.hint().as_deref(), Some("not installed"));
        assert_eq!(err.name(), "AgentAdapterError");
    }

    #[test]
    fn no_agents_has_exit_code_1_and_guidance() {
        let err = DomainError::NoAgents;
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.to_string(), "No agents configured");
        assert_eq!(
            err.hint().as_deref(),
            Some("Run: orch agent add <name> --adapter <adapter>")
        );
        assert_eq!(err.name(), "NoAgentsError");
    }

    #[test]
    fn task_not_found_includes_the_id() {
        let err = DomainError::TaskNotFound("tsk_abc".into());
        assert_eq!(err.to_string(), "Task not found: tsk_abc");
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.name(), "TaskNotFoundError");
    }

    #[test]
    fn agent_not_found_includes_the_id() {
        let err = DomainError::AgentNotFound("agt_xyz".into());
        assert_eq!(err.to_string(), "Agent not found: agt_xyz");
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.name(), "AgentNotFoundError");
    }

    #[test]
    fn task_already_running_includes_task_run_and_agent() {
        let err = DomainError::TaskAlreadyRunning {
            task_id: "tsk_1".into(),
            run_id: "run_2".into(),
            agent: "claude-1".into(),
        };
        assert_eq!(
            err.to_string(),
            "Task tsk_1 is already running (run: run_2, agent: claude-1)"
        );
        assert_eq!(
            err.hint().as_deref(),
            Some("Use: orch logs --task tsk_1 --follow")
        );
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.name(), "TaskAlreadyRunningError");
    }

    #[test]
    fn invalid_transition_includes_the_task_and_an_unfolded_arrow() {
        let err = DomainError::InvalidTransition {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::Done,
        };
        assert_eq!(
            err.to_string(),
            "Invalid transition for tsk_1: todo \u{2192} done"
        );
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.name(), "InvalidTransitionError");
    }

    #[test]
    fn goal_errors() {
        let err = DomainError::GoalNotFound("goal_1".into());
        assert_eq!(err.to_string(), "Goal not found: goal_1");
        assert_eq!(err.exit_code(), 1);

        let err = DomainError::GoalHasPendingTasks {
            id: "goal_1".into(),
            count: 2,
            summary: "tsk_a, tsk_b".into(),
        };
        assert_eq!(
            err.to_string(),
            "Cannot mark goal goal_1 as achieved: 2 task(s) still pending \u{2014} tsk_a, tsk_b"
        );
        assert_eq!(
            err.hint().as_deref(),
            Some("Use --force to cancel pending tasks and mark achieved")
        );
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn team_and_message_not_found() {
        assert_eq!(
            DomainError::TeamNotFound("team_1".into()).to_string(),
            "Team not found: team_1"
        );
        assert_eq!(
            DomainError::MessageNotFound("msg_1".into()).to_string(),
            "Message not found: msg_1"
        );
        assert_eq!(DomainError::TeamNotFound("t".into()).exit_code(), 1);
        assert_eq!(DomainError::MessageNotFound("m".into()).exit_code(), 1);
    }

    #[test]
    fn workspace_error_has_exit_code_6_and_an_optional_hint() {
        let err = DomainError::Workspace {
            message: "worktree add failed".into(),
            hint: Some("run git worktree prune".into()),
        };
        assert_eq!(err.exit_code(), 6);
        assert_eq!(err.to_string(), "worktree add failed");
        assert_eq!(err.hint().as_deref(), Some("run git worktree prune"));
        assert_eq!(err.name(), "WorkspaceError");

        let err = DomainError::Workspace {
            message: "nope".into(),
            hint: None,
        };
        assert_eq!(err.hint(), None);
    }

    // ── AdapterErrorKind ───────────────────────────────────────────────────

    #[test]
    fn has_7_enum_values() {
        assert_eq!(AdapterErrorKind::ALL.len(), 7);
    }

    #[test]
    fn enum_values_are_lowercase_strings() {
        for kind in AdapterErrorKind::ALL {
            assert!(
                kind.as_str()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "{kind}"
            );
            assert_eq!(kind.as_str().parse::<AdapterErrorKind>(), Ok(kind));
        }
    }

    // ── ERROR_HINTS ────────────────────────────────────────────────────────

    #[test]
    fn has_a_hint_for_every_adapter_error_kind() {
        for kind in AdapterErrorKind::ALL {
            let hint = error_hint(kind);
            assert!(!hint.message.is_empty(), "{kind}");
            assert!(!hint.fix.is_empty(), "{kind}");
        }
        assert_eq!(ERROR_HINTS.len(), AdapterErrorKind::ALL.len());
    }

    #[test]
    fn adapter_not_found_has_doctor_hint() {
        assert_eq!(
            error_hint(AdapterErrorKind::AdapterNotFound).doctor_hint,
            Some(true)
        );
    }

    #[test]
    fn unknown_has_doctor_hint() {
        assert_eq!(
            error_hint(AdapterErrorKind::Unknown).doctor_hint,
            Some(true)
        );
    }

    #[test]
    fn auth_failed_has_no_doctor_hint() {
        assert_eq!(error_hint(AdapterErrorKind::AuthFailed).doctor_hint, None);
        // Serialized, the key is absent — as in the TS object.
        let json = serde_json::to_value(error_hint(AdapterErrorKind::AuthFailed)).unwrap();
        assert!(json.get("doctorHint").is_none());
    }

    #[test]
    fn timeout_fix_keeps_the_wrong_config_key() {
        // PARITY.md §14.3 — replicated verbatim.
        assert_eq!(
            error_hint(AdapterErrorKind::Timeout).fix,
            "Increase the timeout with: orch config set agent_timeout <ms>"
        );
    }

    // ── classifyAdapterError ───────────────────────────────────────────────

    #[test]
    fn classification_conformance_vectors() {
        use AdapterErrorKind::*;
        let cases: &[(&str, Option<i32>, AdapterErrorKind)] = &[
            ("spawn ENOENT", None, SpawnFailed),
            ("Spawn failed: /usr/bin/foo", None, SpawnFailed),
            ("claude: command not found", None, AdapterNotFound),
            ("CLI not found on PATH", None, AdapterNotFound),
            ("Authentication failed", None, AuthFailed),
            ("Request unauthorized", None, AuthFailed),
            ("HTTP 401 error", None, AuthFailed),
            ("Invalid API key provided", None, AuthFailed),
            ("Request timeout after 60s", None, Timeout),
            ("Connection timed out", None, Timeout),
            ("connect ETIMEDOUT 1.2.3.4:443", None, Timeout),
            ("Rate limit exceeded", None, RateLimit),
            ("HTTP 429 Too Many Requests", None, RateLimit),
            ("Too many requests, slow down", None, RateLimit),
            ("Segmentation fault", Some(139), ProcessCrash),
            ("Unknown error occurred", Some(1), ProcessCrash),
            ("Something weird", Some(0), Unknown),
            ("Something else", None, Unknown),
            // Order of the rules is contract.
            ("ENOENT /bin/claude", Some(127), SpawnFailed),
            ("Unauthorized access", Some(1), AuthFailed),
            // Case insensitive.
            ("RATE LIMIT exceeded", None, RateLimit),
            ("AUTHENTICATION FAILED", None, AuthFailed),
            ("TIMEOUT", None, Timeout),
        ];
        for (error, exit_code, expected) in cases {
            assert_eq!(
                classify_adapter_error(error, *exit_code),
                *expected,
                "error={error:?} exit_code={exit_code:?}"
            );
        }
    }

    #[test]
    fn no_such_file_classifies_as_adapter_not_found() {
        assert_eq!(
            classify_adapter_error("no such file or directory", None),
            AdapterErrorKind::AdapterNotFound
        );
    }

    // ── extractErrorMessage / isModelMetadataWarning ───────────────────────

    #[test]
    fn unwraps_provider_messages_nested_inside_serialized_json() {
        let provider_error = serde_json::to_string(&json!({
            "type": "error",
            "status": 400,
            "error": {
                "type": "invalid_request_error",
                "message": "The 'gpt-5.6' model is not supported for this account.",
            },
        }))
        .unwrap();

        let outer = json!({ "type": "turn.failed", "error": { "message": provider_error } });
        assert_eq!(
            extract_error_message(&outer),
            "The 'gpt-5.6' model is not supported for this account."
        );
    }

    #[test]
    fn preserves_plain_error_strings() {
        assert_eq!(
            extract_error_message(&json!("Connection refused")),
            "Connection refused"
        );
    }

    #[test]
    fn empty_and_null_values() {
        assert_eq!(extract_error_message(&json!("   ")), "");
        assert_eq!(extract_error_message(&Value::Null), "null");
    }

    #[test]
    fn key_preference_order_is_error_message_detail_reason() {
        let value = json!({ "reason": "r", "detail": "d", "message": "m", "error": "e" });
        assert_eq!(extract_error_message(&value), "e");
        let value = json!({ "reason": "r", "detail": "d", "message": "m" });
        assert_eq!(extract_error_message(&value), "m");
        let value = json!({ "reason": "r", "detail": "d" });
        assert_eq!(extract_error_message(&value), "d");
        let value = json!({ "reason": "r" });
        assert_eq!(extract_error_message(&value), "r");
    }

    #[test]
    fn a_null_keyed_value_is_skipped() {
        let value = json!({ "error": null, "message": "fallback" });
        assert_eq!(extract_error_message(&value), "fallback");
    }

    #[test]
    fn an_object_without_known_keys_stringifies() {
        let value = json!({ "code": 7 });
        assert_eq!(extract_error_message(&value), r#"{"code":7}"#);
    }

    #[test]
    fn depth_is_bounded_at_six() {
        // Seven nested { error: { ... } } levels: the innermost is stringified
        // rather than unwrapped.
        let mut value = json!("deep");
        for _ in 0..7 {
            value = json!({ "error": value });
        }
        let extracted = extract_error_message(&value);
        assert!(extracted.contains("deep"));
        assert!(extracted.starts_with('{'), "got {extracted:?}");
    }

    #[test]
    fn recognizes_the_codex_fallback_metadata_warning() {
        assert!(is_model_metadata_warning(
            "Model metadata for `gpt-5.6` not found. Defaulting to fallback metadata; this can degrade performance."
        ));
        assert!(!is_model_metadata_warning("Model request failed"));
    }

    #[test]
    fn metadata_warning_matching_details() {
        // Leading/trailing whitespace is trimmed before matching.
        assert!(is_model_metadata_warning(
            "  Model metadata for x not found. Defaulting to fallback metadata  "
        ));
        // Case insensitive.
        assert!(is_model_metadata_warning(
            "MODEL METADATA FOR X NOT FOUND. DEFAULTING TO FALLBACK METADATA"
        ));
        // The `.+` needs at least one character.
        assert!(!is_model_metadata_warning(
            "Model metadata for  not found. Defaulting to fallback metadata"
        ));
        // `\b` fails when a word character follows.
        assert!(!is_model_metadata_warning(
            "Model metadata for x not found. Defaulting to fallback metadatax"
        ));
        // `.` does not span a line terminator.
        assert!(!is_model_metadata_warning(
            "Model metadata for x\ny not found. Defaulting to fallback metadata"
        ));
        // Must be anchored at the start.
        assert!(!is_model_metadata_warning(
            "warn: Model metadata for x not found. Defaulting to fallback metadata"
        ));
        assert!(!is_model_metadata_warning(""));
        // Multi-byte input must not panic or mis-slice.
        assert!(is_model_metadata_warning(
            "Model metadata for «ü» not found. Defaulting to fallback metadata."
        ));
    }
}
