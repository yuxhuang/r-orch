//! Network-independent peer boundary. A link owns streams across reconnects;
//! losing a socket must never finish an execution or trigger a retry.

use async_trait::async_trait;
use futures::stream::BoxStream;
use orch_domain::{AdapterErrorKind, Message, ReviewCriterion, ReviewResult, TokenUsage};
use serde::{Deserialize, Serialize};

use crate::adapter::AgentEvent;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteOutcome {
    Succeeded {
        summary: String,
        files_changed: Vec<String>,
        branch: Option<String>,
        review_results: Option<Vec<ReviewResult>>,
        tokens: Option<TokenUsage>,
    },
    ReviewFailed {
        summary: String,
        review_results: Vec<ReviewResult>,
    },
    IntegrationFailed {
        summary: String,
        info: String,
    },
    Failed {
        error: String,
        error_kind: Option<AdapterErrorKind>,
    },
    Cancelled {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct RemoteSubmit {
    pub alias: String,
    pub exec_key: String,
    /// Worker-local id, with no `@` suffix.
    pub agent_id: String,
    pub from: String,
    pub prompt: String,
    pub title: String,
    pub priority: i64,
    pub review_criteria: Vec<ReviewCriterion>,
    pub goal_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RemoteRunEvent {
    Event(AgentEvent),
    Finished(RemoteOutcome),
    /// A successful sync proved the worker has lost the execution record.
    /// Unlike a known failure, this must never be retried automatically.
    Unknown,
}

pub struct RemoteExecuteHandle {
    /// Always zero. This is a peer execution, never a local process handle.
    pub pid: u32,
    pub events: BoxStream<'static, RemoteRunEvent>,
}

#[async_trait]
pub trait RemoteBridge: Send + Sync {
    /// Called only after the pid-less running entry is durable. Register and
    /// enqueue synchronously; do not wait for a socket or an acknowledgement.
    /// The stream stays open during link failures. Err means no request was
    /// enqueued. The owner retains its entry for sync rather than risking a
    /// duplicate effect. Submit and cancel must preserve enqueue order.
    fn submit(&self, request: RemoteSubmit) -> Result<RemoteExecuteHandle>;
    /// Enqueue promptly and preserve submit order; never await the network
    /// while the controller is holding its state mutex.
    async fn cancel(&self, alias: &str, exec_key: &str) -> Result<()>;
    /// Sent after the controller's task/run/state writes have completed.
    async fn ack_finished(&self, _alias: &str, _exec_key: &str) -> Result<()> {
        Ok(())
    }
    /// Delivery is acknowledged by the link before it returns Ok.
    async fn send_msg(&self, peer: &str, message: &Message) -> Result<()>;
}

/// Installed until the peer runtime is wired by the engine owner.
pub struct NoopRemoteBridge;

#[async_trait]
impl RemoteBridge for NoopRemoteBridge {
    fn submit(&self, _request: RemoteSubmit) -> Result<RemoteExecuteHandle> {
        Err(Error::invalid_arguments(
            "Remote peer runtime is not installed",
        ))
    }
    async fn cancel(&self, _alias: &str, _exec_key: &str) -> Result<()> {
        Ok(())
    }
    async fn send_msg(&self, _peer: &str, _message: &Message) -> Result<()> {
        Err(Error::invalid_arguments(
            "Remote peer runtime is not installed",
        ))
    }
}

/// In-process worker tap, before JSONL/TUI truncation. Implementations must
/// enqueue promptly; they must not perform network I/O in this callback.
pub trait RunEventTap: Send + Sync {
    fn record(&self, task_id: &str, run_id: &str, event: &AgentEvent);
}
