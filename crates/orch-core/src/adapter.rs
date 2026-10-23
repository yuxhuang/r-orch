//! Agent adapter port.
//!
//! Every AI tool (Claude, Codex, Shell, …) implements [`Adapter`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use indexmap::IndexMap;
use orch_domain::{AdapterErrorKind, AgentConfig, DomainError, RunEventType, TokenUsage};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterTestResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "errorKind", default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<AdapterErrorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl AdapterTestResult {
    pub fn ok(version: impl Into<String>) -> Self {
        AdapterTestResult {
            ok: true,
            version: Some(version.into()),
            ..Default::default()
        }
    }

    pub fn failed(error: impl Into<String>, kind: AdapterErrorKind) -> Self {
        AdapterTestResult {
            ok: false,
            error: Some(error.into()),
            error_kind: Some(kind),
            ..Default::default()
        }
    }
}

/// The adapter-level event kind. **Distinct from [`RunEventType`]** — the
/// orchestrator maps between them with [`AgentEventKind::to_run_event_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    Output,
    FileChange,
    Command,
    ToolCall,
    Error,
    Done,
}

impl AgentEventKind {
    pub const ALL: [AgentEventKind; 6] = [
        AgentEventKind::Output,
        AgentEventKind::FileChange,
        AgentEventKind::Command,
        AgentEventKind::ToolCall,
        AgentEventKind::Error,
        AgentEventKind::Done,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentEventKind::Output => "output",
            AgentEventKind::FileChange => "file_change",
            AgentEventKind::Command => "command",
            AgentEventKind::ToolCall => "tool_call",
            AgentEventKind::Error => "error",
            AgentEventKind::Done => "done",
        }
    }

    /// `output → agent_output`, `file_change → file_changed`,
    /// `command → command_run`, `tool_call → tool_call`, `error → error`,
    /// anything else → `done`.
    pub fn to_run_event_type(self) -> RunEventType {
        match self {
            AgentEventKind::Output => RunEventType::AgentOutput,
            AgentEventKind::FileChange => RunEventType::FileChanged,
            AgentEventKind::Command => RunEventType::CommandRun,
            AgentEventKind::ToolCall => RunEventType::ToolCall,
            AgentEventKind::Error => RunEventType::Error,
            AgentEventKind::Done => RunEventType::Done,
        }
    }
}

impl std::fmt::Display for AgentEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentEventKind {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        AgentEventKind::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// One event pulled from a running adapter.
///
/// Canonical `data` shape per kind, which downstream consumers (TUI logs,
/// `orch logs`, serve mode) render without knowing adapter internals:
///
/// | kind        | data                                                  |
/// |-------------|-------------------------------------------------------|
/// | output      | `{ text: string, raw?: unknown }`                     |
/// | tool_call   | `{ name: string, input?: unknown, raw?: unknown }`    |
/// | command     | `{ command: string, result?: unknown, raw?: unknown }`|
/// | file_change | `{ paths: string[], raw?: unknown }`                  |
/// | error       | `{ message: string, raw?: unknown }`                  |
/// | done        | `{ result?: string, raw?: unknown }`                  |
///
/// Existing adapters predate this contract and emit a variety of shapes
/// (claude/cursor: the full `parsed.message`; codex: `item`), so renderers
/// stay defensive. `raw` is an escape hatch for the full provider payload and
/// must not appear in a default log summary. One `output` per logical
/// assistant message, not per delta; adapters aggregate streaming deltas
/// themselves and drop in-flight progress pings at the boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    #[serde(rename = "type")]
    pub kind: AgentEventKind,
    /// ISO-8601 with millisecond precision, minted at parse time.
    pub timestamp: String,
    pub data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    #[serde(rename = "errorKind", default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<AdapterErrorKind>,
}

impl AgentEvent {
    pub fn new(
        kind: AgentEventKind,
        timestamp: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        AgentEvent {
            kind,
            timestamp: timestamp.into(),
            data,
            tokens: None,
            error_kind: None,
        }
    }

    pub fn with_tokens(mut self, tokens: TokenUsage) -> Self {
        self.tokens = Some(tokens);
        self
    }

    pub fn with_error_kind(mut self, kind: AdapterErrorKind) -> Self {
        self.error_kind = Some(kind);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ExecuteParams {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub workspace: PathBuf,
    /// Overlaid on the inherited parent environment. Assembled by the
    /// orchestrator as `{ ...agent.config.env, ORCH_AGENT_ID, ORCH_AGENT_NAME,
    /// ORCH_TASK_ID }` — the three `ORCH_*` keys win.
    pub env: BTreeMap<String, String>,
    /// Filter on the inherited parent environment, passed to
    /// [`SpawnOptions::inherit_env`](crate::process::SpawnOptions). Remote
    /// mirror runs use `WithoutSecrets` (security review F2).
    pub inherit_env: crate::process::EnvInheritance,
    pub config: AgentConfig,
    /// Cancels the run.
    pub cancel: CancellationToken,
}

/// Invocation-local effort preparation; never persisted.
#[derive(Debug, Clone)]
pub struct EffortPreparation {
    /// Only effort may change (cleared for legacy-ignore); all other fields stay identical.
    pub config: AgentConfig,
    /// Request-only and compatibility diagnostics for the pre-spawn run output.
    pub diagnostics: Vec<String>,
}

/// Returned **synchronously** from [`Adapter::execute`]: the PID is available
/// before any event is pulled, because the orchestrator must write
/// `state.running` before it starts consuming the stream.
pub struct ExecuteHandle {
    pub pid: u32,
    /// Stream errors are `Err` *items* rather than a terminated stream,
    /// because the stream distinguishes "an `error` event occurred"
    /// (continue) from "the generator threw" (run failed). The thrown case is
    /// the stream yielding `Err` and then ending.
    pub events: BoxStream<'static, std::result::Result<AgentEvent, AdapterError>>,
}

impl std::fmt::Debug for ExecuteHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecuteHandle")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// An adapter failure, carrying the classification the orchestrator records on
/// the agent and surfaces in `agent:error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AdapterError {
    pub message: String,
    pub kind: AdapterErrorKind,
    pub exit_code: Option<i32>,
}

impl AdapterError {
    /// Classify from the message (and exit code) with
    /// [`orch_domain::classify_adapter_error`].
    pub fn new(message: impl Into<String>, exit_code: Option<i32>) -> Self {
        let message = message.into();
        let kind = orch_domain::classify_adapter_error(&message, exit_code);
        AdapterError {
            message,
            kind,
            exit_code,
        }
    }

    /// Use a pre-determined kind rather than re-classifying.
    pub fn with_kind(
        message: impl Into<String>,
        kind: AdapterErrorKind,
        exit_code: Option<i32>,
    ) -> Self {
        AdapterError {
            message: message.into(),
            kind,
            exit_code,
        }
    }
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn test(&self) -> AdapterTestResult;

    /// Pre-spawn preparation, without model invocation or persistent changes.
    /// Default: identity, no I/O and no diagnostics.
    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> std::result::Result<EffortPreparation, AdapterError> {
        Ok(EffortPreparation {
            config: params.config.clone(),
            diagnostics: Vec::new(),
        })
    }

    /// **Not async**: `execute()` returns `{ pid, events }` synchronously.
    /// Lifecycle listeners (close, error, stderr) must be attached before the
    /// stream is polled, so a fast-failing child is never missed. Spawning is itself non-blocking, so sync costs
    /// nothing.
    fn execute(&self, params: ExecuteParams) -> std::result::Result<ExecuteHandle, AdapterError>;

    async fn stop(&self, pid: u32) -> crate::error::Result<()>;
}

/// Adapter lookup by kind.
///
/// Backed by an insertion-ordered map rather than ARCHITECTURE §5.4's `Vec`:
/// the registry is keyed by `kind`, so re-registering a kind **replaces** it,
/// while `kinds()` still yields registration order. A `Vec` would duplicate instead of replacing. Registration order is contract —
/// `doctor` probes adapters in it.
#[derive(Default, Clone)]
pub struct AdapterRegistry {
    adapters: IndexMap<&'static str, Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        AdapterRegistry::default()
    }

    pub fn register(&mut self, adapter: Arc<dyn Adapter>) {
        self.adapters.insert(adapter.kind(), adapter);
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn Adapter>> {
        self.adapters.get(kind)
    }

    /// Fails with
    /// `Unknown adapter: "<kind>". Available: <kinds joined with ", ">`,
    /// a plain `Error` — so exit 1.
    pub fn require(&self, kind: &str) -> std::result::Result<&Arc<dyn Adapter>, DomainError> {
        self.get(kind).ok_or_else(|| {
            DomainError::Other(format!(
                "Unknown adapter: \"{kind}\". Available: {}",
                self.kinds().join(", ")
            ))
        })
    }

    pub fn has(&self, kind: &str) -> bool {
        self.adapters.contains_key(kind)
    }

    /// Registration order.
    pub fn list(&self) -> Vec<&Arc<dyn Adapter>> {
        self.adapters.values().collect()
    }

    /// Registration order.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.adapters.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

impl std::fmt::Debug for AdapterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterRegistry")
            .field("kinds", &self.kinds())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub(&'static str);

    #[async_trait]
    impl Adapter for Stub {
        fn kind(&self) -> &'static str {
            self.0
        }

        async fn test(&self) -> AdapterTestResult {
            AdapterTestResult::ok("1.0.0")
        }

        fn execute(
            &self,
            _params: ExecuteParams,
        ) -> std::result::Result<ExecuteHandle, AdapterError> {
            Err(AdapterError::new("not implemented", None))
        }

        async fn stop(&self, _pid: u32) -> crate::error::Result<()> {
            Ok(())
        }
    }

    fn registry(kinds: &[&'static str]) -> AdapterRegistry {
        let mut registry = AdapterRegistry::new();
        for kind in kinds {
            registry.register(Arc::new(Stub(kind)));
        }
        registry
    }

    #[test]
    fn registration_order_is_preserved() {
        // The doctor probe order.
        let order = [
            "claude",
            "codex",
            "cursor",
            "shell",
            "opencode",
            "pi",
            "grok",
            "antigravity",
        ];
        assert_eq!(registry(&order).kinds(), order.to_vec());
    }

    #[test]
    fn re_registering_a_kind_replaces_it_and_keeps_its_position() {
        let mut registry = registry(&["claude", "codex"]);
        registry.register(Arc::new(Stub("claude")));
        assert_eq!(registry.kinds(), vec!["claude", "codex"]);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn require_reports_the_available_kinds() {
        let registry = registry(&["claude", "shell"]);
        assert!(registry.get("claude").is_some());
        assert!(registry.has("shell"));
        assert!(registry.get("nope").is_none());

        let err = registry.require("nope").err().expect("unknown kind");
        assert_eq!(
            err.to_string(),
            "Unknown adapter: \"nope\". Available: claude, shell"
        );
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn an_empty_registry_is_empty() {
        let registry = AdapterRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.kinds(), Vec::<&str>::new());
        assert_eq!(
            registry
                .require("claude")
                .err()
                .expect("empty registry")
                .to_string(),
            "Unknown adapter: \"claude\". Available: "
        );
    }

    #[test]
    fn event_kind_maps_to_the_run_event_type() {
        use AgentEventKind as K;
        assert_eq!(K::Output.to_run_event_type(), RunEventType::AgentOutput);
        assert_eq!(K::FileChange.to_run_event_type(), RunEventType::FileChanged);
        assert_eq!(K::Command.to_run_event_type(), RunEventType::CommandRun);
        assert_eq!(K::ToolCall.to_run_event_type(), RunEventType::ToolCall);
        assert_eq!(K::Error.to_run_event_type(), RunEventType::Error);
        assert_eq!(K::Done.to_run_event_type(), RunEventType::Done);
    }

    #[test]
    fn event_kind_strings_are_the_wire_values() {
        let names: Vec<&str> = AgentEventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "output",
                "file_change",
                "command",
                "tool_call",
                "error",
                "done"
            ]
        );
        for kind in AgentEventKind::ALL {
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
            assert_eq!(kind.as_str().parse::<AgentEventKind>(), Ok(kind));
        }
    }

    #[test]
    fn agent_event_serializes_with_the_wire_key_names() {
        let event = AgentEvent::new(
            AgentEventKind::Output,
            "2026-10-13T16:37:59.169Z",
            serde_json::json!({ "text": "hi" }),
        )
        .with_error_kind(AdapterErrorKind::Timeout);
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "type": "output",
                "timestamp": "2026-10-13T16:37:59.169Z",
                "data": { "text": "hi" },
                "errorKind": "timeout",
            })
        );
    }

    #[test]
    fn adapter_error_classifies_from_its_message() {
        let err = AdapterError::new("spawn ENOENT", None);
        assert_eq!(err.kind, AdapterErrorKind::SpawnFailed);
        assert_eq!(err.to_string(), "spawn ENOENT");

        let err = AdapterError::new("boom", Some(139));
        assert_eq!(err.kind, AdapterErrorKind::ProcessCrash);

        let err = AdapterError::with_kind("boom", AdapterErrorKind::RateLimit, Some(1));
        assert_eq!(err.kind, AdapterErrorKind::RateLimit);
    }

    #[test]
    fn adapter_error_converts_into_the_core_error_with_exit_1() {
        let err: crate::error::Error = AdapterError::new("boom", None).into();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.to_string(), "boom");
    }

    #[tokio::test]
    async fn test_result_helpers() {
        let registry = registry(&["claude"]);
        let adapter = registry.require("claude").unwrap();
        let result = adapter.test().await;
        assert!(result.ok);
        assert_eq!(result.version.as_deref(), Some("1.0.0"));

        let failed = AdapterTestResult::failed("nope", AdapterErrorKind::AdapterNotFound);
        assert!(!failed.ok);
        assert_eq!(failed.error_kind, Some(AdapterErrorKind::AdapterNotFound));
    }
}
