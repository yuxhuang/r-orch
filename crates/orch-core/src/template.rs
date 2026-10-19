//! Template engine port (the interface and `PromptContext` only).
//!
//! **Nothing here uses `skip_serializing_if`.** That is deliberate and is the
//! first of the two ARCHITECTURE §4.3 mitigations: the Rust `liquid` crate
//! errors on a missing key where liquidjs renders empty, so every documented
//! variable is always present and an absent optional is an explicit `nil`.
//! Liquid truthiness treats `nil` exactly like liquidjs treats `undefined`,
//! so the rendered bytes are unchanged.

use async_trait::async_trait;
use orch_domain::GoalStatus;
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectContext {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContext {
    pub id: String,
    pub title: String,
    pub description: String,
    pub priority: i64,
    pub labels: Vec<String>,
    pub scope: Option<Vec<String>>,
    /// `labels` contains `"autonomous"`.
    pub is_autonomous: bool,
    /// From `Task::goal_id` (the camelCase `goalId` on disk).
    pub goal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentContext {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
}

/// An entry of `agents[]`. The **current** agent's own `role` is set to `nil`
/// here, because it is already in the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub adapter: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub remote: Option<orch_domain::RemoteRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryContext {
    pub previous_error: String,
    pub previous_output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageContext {
    pub id: String,
    /// The sender's **name** when resolvable, else the raw agent id.
    pub from: String,
    pub subject: String,
    pub body: String,
    pub sent_at: String,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalContext {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    /// Each entry formatted `[<status>] <title>`.
    pub task_names: Vec<String>,
    /// From the context key `<goalId>-progress`.
    pub progress: Option<String>,
}

/// One shared-context entry, rendered through `entry[0]` / `entry[1]`.
///
/// An **ordered pair array** rather than a map: this is the second
/// ARCHITECTURE §4.3 mitigation. The Rust `liquid` crate leaves object
/// iteration order unspecified, which would reorder the `shared_context` loop;
/// a pair array renders identically through `entry[0]`/`entry[1]` and pins the
/// order to `ContextStore::list`'s (key-ascending).
pub type SharedContextEntry = (String, String);

/// The variables the prompt templates see. Names and nesting
/// are contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptContext {
    pub project: ProjectContext,
    pub task: TaskContext,
    pub agent: AgentContext,
    pub agents: Vec<AgentInfo>,
    /// `None` for the first attempt — **never `Some(0)`**, because `0` is
    /// truthy in Liquid.
    pub attempt: Option<i64>,
    pub workspace_path: String,
    /// Populated only when `attempt > 1`, even if a retry context is supplied.
    pub retry: Option<RetryContext>,
    pub feedback: Option<String>,
    /// `None` (not an empty list) when there is no relevant context.
    pub shared_context: Option<Vec<SharedContextEntry>>,
    /// `None` (not an empty list) when there are no messages.
    pub messages: Option<Vec<MessageContext>>,
    pub goal: Option<GoalContext>,
}

#[async_trait]
pub trait TemplateEngine: Send + Sync {
    async fn render(&self, template: &str, context: &PromptContext) -> Result<String>;
}

/// Default render timeout.
pub const DEFAULT_RENDER_TIMEOUT_MS: u64 = 5_000;

/// Max shared-context entries injected into a single prompt
/// (`MAX_CONTEXT_ENTRIES`).
pub const MAX_CONTEXT_ENTRIES: usize = 15;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> PromptContext {
        PromptContext {
            project: ProjectContext {
                name: "proj".into(),
                description: None,
            },
            task: TaskContext {
                id: "tsk_1".into(),
                title: "Do it".into(),
                description: String::new(),
                priority: 3,
                labels: vec![],
                scope: None,
                is_autonomous: false,
                goal_id: None,
            },
            agent: AgentContext {
                id: "agt_1".into(),
                name: "shelly".into(),
                role: Some("role".into()),
            },
            agents: vec![AgentInfo {
                id: "agt_1".into(),
                name: "shelly".into(),
                role: None,
                adapter: "shell".into(),
                capabilities: Vec::new(),
                remote: None,
            }],
            attempt: None,
            workspace_path: "/tmp/ws".into(),
            retry: None,
            feedback: None,
            shared_context: None,
            messages: None,
            goal: None,
        }
    }

    #[test]
    fn every_documented_variable_is_present_even_when_absent() {
        // ARCHITECTURE §4.3 mitigation 1: nothing is omitted.
        let value = serde_json::to_value(context()).unwrap();
        for key in [
            "project",
            "task",
            "agent",
            "agents",
            "attempt",
            "workspace_path",
            "retry",
            "feedback",
            "shared_context",
            "messages",
            "goal",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["attempt"], serde_json::Value::Null);
        assert_eq!(value["retry"], serde_json::Value::Null);
        assert_eq!(value["shared_context"], serde_json::Value::Null);
        assert_eq!(value["messages"], serde_json::Value::Null);
        assert_eq!(value["goal"], serde_json::Value::Null);
        assert_eq!(value["project"]["description"], serde_json::Value::Null);
        assert_eq!(value["task"]["scope"], serde_json::Value::Null);
        assert_eq!(value["task"]["goal_id"], serde_json::Value::Null);
        assert_eq!(value["agents"][0]["role"], serde_json::Value::Null);
    }

    #[test]
    fn shared_context_is_an_ordered_pair_array() {
        // ARCHITECTURE §4.3 mitigation 2: renders through entry[0]/entry[1].
        let mut ctx = context();
        ctx.shared_context = Some(vec![
            ("b-key".into(), "b-value".into()),
            ("a-key".into(), "a-value".into()),
        ]);
        let value = serde_json::to_value(&ctx).unwrap();
        assert_eq!(
            value["shared_context"],
            json!([["b-key", "b-value"], ["a-key", "a-value"]])
        );
    }

    #[test]
    fn goal_context_carries_the_status_string() {
        let mut ctx = context();
        ctx.goal = Some(GoalContext {
            id: "goal_1".into(),
            title: "Ship".into(),
            description: "d".into(),
            status: GoalStatus::Active,
            task_names: vec!["[todo] Do it".into()],
            progress: None,
        });
        let value = serde_json::to_value(&ctx).unwrap();
        assert_eq!(value["goal"]["status"], "active");
        assert_eq!(value["goal"]["task_names"][0], "[todo] Do it");
        assert_eq!(value["goal"]["progress"], serde_json::Value::Null);
    }

    #[test]
    fn context_round_trips() {
        let ctx = context();
        let json = serde_json::to_string(&ctx).unwrap();
        assert_eq!(serde_json::from_str::<PromptContext>(&json).unwrap(), ctx);
    }
}
