//! Template engine port (the interface and `PromptContext` only).
//!
//! **Nothing here uses `skip_serializing_if`.** That is deliberate and is the
//! first of the two ARCHITECTURE §4.3 mitigations: the Rust `liquid` crate
//! errors on a missing key where liquidjs renders empty, so every documented
//! variable is always present and an absent optional is an explicit `nil`.
//! Liquid truthiness treats `nil` exactly like liquidjs treats `undefined`,
//! so the rendered bytes are unchanged.
//!
//! Fields added for eco prompt mode must not change what a **normal** template
//! can observe — including object sizes (`{{ task.size }}` is a golden
//! fixture). So `TaskContext::depends_on`, `RetryContext::source_run_id` and
//! `GoalContext::task_rows` are `#[serde(skip)]`: Rust-only inputs to the eco
//! projection, never template variables. `PromptContext::eco` is the one
//! deliberate exception to the rule above: it is omitted when absent, so a
//! normal render's root object is unchanged, and only the eco templates,
//! which always have it, read it.

use async_trait::async_trait;
use orch_domain::{GoalStatus, TaskStatus};
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
    /// The task's direct prerequisites, for the eco goal-task preview order.
    /// Not a template variable (see the module docs).
    #[serde(skip)]
    pub depends_on: Vec<String>,
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
    /// The failed run `previous_error` and `previous_output` were read from,
    /// set only when the engine can vouch that the run's stored event window
    /// is stable (terminal, with no live collector). Eco clips the output
    /// only when this is present; without it the full output is kept.
    /// Not a template variable (see the module docs).
    #[serde(skip)]
    pub source_run_id: Option<String>,
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
    /// The same linked tasks as `task_names`, structured, in store order.
    /// Not a template variable (see the module docs).
    #[serde(skip)]
    pub task_rows: Vec<GoalTaskRow>,
}

/// One task linked to the goal, for the eco preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalTaskRow {
    pub id: String,
    pub status: TaskStatus,
    pub title: String,
}

/// The eco projection: bounded or relocated presentation copies of data
/// already in [`PromptContext`]. Only the eco templates read it. Every
/// string that came from an agent, a store or a remote peer and is placed in
/// the system half is pre-encoded as one JSON object per line, so it renders
/// as data and cannot form headings or Liquid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcoContext {
    /// One row per agent, sorted by agent id.
    pub roster: Vec<EcoRosterRow>,
    /// The loaded skills block, placed after the agent's own role.
    pub skills: Option<String>,
    /// The same selected entries as `shared_context`, full keys and values,
    /// one JSON record each. `None` when there are none.
    pub shared_context: Option<Vec<String>>,
    pub retry: Option<EcoRetry>,
    pub goal_tasks: Option<EcoGoalTasks>,
}

/// One roster line: a JSON record (`id`, `name`, `adapter`, `summary`,
/// optional `capabilities` and `remote`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcoRosterRow {
    pub record: String,
    /// Remote peer metadata: rendered behind the untrusted-data marker.
    pub untrusted_remote: bool,
}

/// The retry section's presentation copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcoRetry {
    /// Always complete.
    pub previous_error: String,
    /// The full previous output, or its head/tail preview when `clipped`.
    pub output: String,
    pub clipped: bool,
    /// Source bytes left out of `output` (0 when not clipped).
    pub omitted_bytes: u64,
    pub source_run_id: Option<String>,
}

/// The goal's linked-task preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcoGoalTasks {
    /// One JSON record (`id`, `status`, `title`) per previewed task.
    pub rows: Vec<String>,
    /// Every linked task, not the preview length.
    pub total: u64,
    /// Linked tasks not in `rows`.
    pub omitted: u64,
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
    /// Present only for an eco render; see [`EcoContext`]. Omitted, not
    /// `nil`, when absent, so a normal render sees the same root object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eco: Option<EcoContext>,
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
                depends_on: Vec::new(),
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
            eco: None,
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
            task_rows: Vec::new(),
        });
        let value = serde_json::to_value(&ctx).unwrap();
        assert_eq!(value["goal"]["status"], "active");
        assert_eq!(value["goal"]["task_names"][0], "[todo] Do it");
        assert_eq!(value["goal"]["progress"], serde_json::Value::Null);
    }

    #[test]
    fn eco_inputs_are_invisible_to_normal_templates() {
        // Object sizes are observable (`{{ task.size }}`), so a normal
        // context must serialize exactly as it did before eco existed.
        let mut ctx = context();
        ctx.task.depends_on = vec!["tsk_0".into()];
        ctx.retry = Some(RetryContext {
            previous_error: "e".into(),
            previous_output: "o".into(),
            source_run_id: Some("run_1".into()),
        });
        ctx.goal = Some(GoalContext {
            id: "goal_1".into(),
            title: "G".into(),
            description: String::new(),
            status: GoalStatus::Active,
            task_names: Vec::new(),
            progress: None,
            task_rows: vec![GoalTaskRow {
                id: "tsk_1".into(),
                status: TaskStatus::Todo,
                title: "t".into(),
            }],
        });
        let value = serde_json::to_value(&ctx).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 11);
        assert!(value.get("eco").is_none());
        assert_eq!(value["task"].as_object().unwrap().len(), 8);
        assert_eq!(value["retry"].as_object().unwrap().len(), 2);
        assert_eq!(value["goal"].as_object().unwrap().len(), 6);
    }

    #[test]
    fn context_round_trips() {
        let ctx = context();
        let json = serde_json::to_string(&ctx).unwrap();
        assert_eq!(serde_json::from_str::<PromptContext>(&json).unwrap(), ctx);
    }
}
