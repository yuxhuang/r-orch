//! Storage port traits.
//!
//! All persistence goes through these contracts. Implementations use atomic
//! writes (temp file + rename); services depend on the traits, never on a
//! concrete store.
//!
//! Every method takes `&self`, not `&mut self`: stores are shared as
//! `Arc<dyn …>` and own their interior mutability (the index mutex), exactly
//! as the shared store instances always have.
//!
//! # Three rules every implementation owes the contract
//!
//! These are not implementation taste. Each is observable, each is invisible
//! in review, and the test fakes in [`crate::test_support`] already implement
//! all three — so a fake-backed test will not catch a store that skips one.
//!
//! 1. **`save` pushes to the end of `_index.json`.** Remove the entity by id,
//!    then append. Index order is therefore *last-write*
//!    order, not creation order, and it is observable: `findBestAgent`
//!    resolves equal-scoring agents by position in [`AgentStore::list`], and
//!    that order is kept rather than fixed.
//!    An in-place update that preserves position silently changes which agent
//!    runs a task.
//!
//! 2. **Sorts over index order must be stable.** ECMAScript mandates a stable
//!    `Array.prototype.sort`; Rust's `sort_unstable*` family is not. The
//!    workspace `clippy.toml` denies those three methods for this reason —
//!    see rule 1 for what breaks.
//!
//! 3. **Every YAML read must apply merge keys.** YAML merge keys (`<<`)
//!    are part of the `config.yml` format, and `serde_yaml_ng` leaves them as a
//!    literal `"<<"` key, so a reader that forgets `Value::apply_merge()` silently falls back to
//!    `DEFAULT_CONFIG` for everything a hand-authored `config.yml` supplied
//!    through a merge — while both trees still compare equal under the Rust
//!    parser.

use async_trait::async_trait;
use futures::stream::BoxStream;
use indexmap::IndexMap;
use orch_domain::{
    Agent, GlobalConfig, Goal, GoalStatus, Message, OrchestratorConfig, OrchestratorState, Run,
    RunEvent, Task, TaskStatus, Team,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::Result;

/// A struct rather than two `Option` parameters, so adding a filter dimension
/// is not a signature break for five packages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFilter {
    pub status: Option<TaskStatus>,
    /// Matches `Task::goal_id` — the camelCase `goalId` on the wire.
    pub goal_id: Option<String>,
}

impl TaskFilter {
    pub fn with_status(status: TaskStatus) -> Self {
        TaskFilter {
            status: Some(status),
            goal_id: None,
        }
    }

    pub fn with_goal(goal_id: impl Into<String>) -> Self {
        TaskFilter {
            status: None,
            goal_id: Some(goal_id.into()),
        }
    }

    /// Whether this task passes the filter. Both dimensions are AND-ed and an
    /// unset dimension matches everything.
    pub fn matches(&self, task: &Task) -> bool {
        self.status.is_none_or(|status| task.status == status)
            && self
                .goal_id
                .as_deref()
                .is_none_or(|goal_id| task.goal_id.as_deref() == Some(goal_id))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalFilter {
    pub status: Option<GoalStatus>,
}

impl GoalFilter {
    pub fn with_status(status: GoalStatus) -> Self {
        GoalFilter {
            status: Some(status),
        }
    }

    pub fn matches(&self, goal: &Goal) -> bool {
        self.status.is_none_or(|status| goal.status == status)
    }
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>>;
    async fn get(&self, id: &str) -> Result<Option<Task>>;
    async fn save(&self, task: &Task) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
}

#[async_trait]
pub trait AgentStore: Send + Sync {
    async fn list(&self) -> Result<Vec<Agent>>;
    async fn get(&self, id: &str) -> Result<Option<Agent>>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>>;
    async fn save(&self, agent: &Agent) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
}

#[async_trait]
pub trait RunStore: Send + Sync {
    async fn save(&self, run: &Run) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Run>>;
    async fn list_all(&self) -> Result<Vec<Run>>;
    async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>>;
    async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>>;
    async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()>;
    async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>>;
    async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>>;
    /// Read the events file once, from the beginning, **to EOF**.
    ///
    /// Despite the name this does **not** tail-follow: the stream ends at EOF.
    /// `logs --follow` re-invokes it. The token only lets a consumer stop early.
    fn stream_events(
        &self,
        run_id: &str,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<RunEvent>>;
    /// Release the cached `O_APPEND` handle for this run. Sync — it is a
    /// cache eviction, not I/O.
    fn close_run_events(&self, run_id: &str);
}

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn read(&self) -> Result<OrchestratorState>;
    async fn write(&self, state: &OrchestratorState) -> Result<()>;
}

#[async_trait]
pub trait ConfigStore: Send + Sync {
    async fn read(&self) -> Result<OrchestratorConfig>;
    async fn write(&self, config: &OrchestratorConfig) -> Result<()>;
    /// Dot-path read. `None` is a missing key, or a path
    /// that runs through a non-object.
    async fn get(&self, key_path: &str) -> Result<Option<serde_json::Value>>;
    /// Dot-path write, creating missing intermediate objects.
    ///
    /// Implementations must round-trip through the **raw merged value**, not
    /// through [`OrchestratorConfig`]: the store deep-merges plain objects,
    /// so unknown keys in `config.yml` survive a `config set`
    /// ([`orch_domain::deep_merge`]). `config.yml` is also the one file a user
    /// hand-authors, so it is the one place YAML merge keys are reachable —
    /// rule 3 in the module docs applies to `read` here specifically.
    async fn set(&self, key_path: &str, value: serde_json::Value) -> Result<()>;
}

/// A shared-context entry. Declared here, next to the store that owns it,
/// rather than in `orch-domain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    pub key: String,
    pub value: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[async_trait]
pub trait ContextStore: Send + Sync {
    /// Lazily deletes an expired entry and returns `None` for it.
    async fn get(&self, key: &str) -> Result<Option<ContextEntry>>;
    async fn set(&self, key: &str, value: &str, ttl_ms: Option<u64>) -> Result<()>;
    async fn delete(&self, key: &str) -> Result<()>;
    /// Lazily purges every expired entry (batched, one index rewrite) and
    /// returns the rest sorted by `key` ascending.
    async fn list(&self) -> Result<Vec<ContextEntry>>;
    /// `list()` projected to `{ key: value }`, in `list()` order.
    async fn get_all(&self) -> Result<IndexMap<String, String>>;
}

#[async_trait]
pub trait MessageStore: Send + Sync {
    async fn save(&self, message: &Message) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Message>>;
    async fn list(&self) -> Result<Vec<Message>>;
    async fn list_pending(&self, agent_id: &str) -> Result<Vec<Message>>;
    async fn mark_delivered(&self, id: &str) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
    /// Returns the number of messages purged.
    async fn purge_expired(&self) -> Result<usize>;
}

#[async_trait]
pub trait GoalStore: Send + Sync {
    async fn list(&self, filter: &GoalFilter) -> Result<Vec<Goal>>;
    async fn get(&self, id: &str) -> Result<Option<Goal>>;
    async fn save(&self, goal: &Goal) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
}

#[async_trait]
pub trait TeamStore: Send + Sync {
    async fn save(&self, team: &Team) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Team>>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Team>>;
    async fn list(&self) -> Result<Vec<Team>>;
    async fn delete(&self, id: &str) -> Result<()>;
}

/// `~/.orchestry/global.yml`.
///
/// This trait exists so the TUI — which persists the palette and the
/// activity filter — can be tested without touching the real home directory.
#[async_trait]
pub trait GlobalConfigStore: Send + Sync {
    async fn read(&self) -> Result<GlobalConfig>;
    async fn write(&self, config: &GlobalConfig) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::TaskStatus;

    fn task(id: &str, status: TaskStatus, goal_id: Option<&str>) -> Task {
        Task {
            id: id.into(),
            title: "t".into(),
            description: String::new(),
            status,
            priority: 3,
            assignee: None,
            labels: vec![],
            depends_on: vec![],
            created_at: "t0".into(),
            updated_at: "t0".into(),
            attempts: 0,
            max_attempts: 3,
            workspace_mode: None,
            workspace: None,
            proof: None,
            review_criteria: None,
            review_results: None,
            scope: None,
            feedback: None,
            goal_id: goal_id.map(str::to_owned),
            attachments: None,
        }
    }

    #[test]
    fn an_empty_filter_matches_everything() {
        let filter = TaskFilter::default();
        assert!(filter.matches(&task("tsk_1", TaskStatus::Todo, None)));
        assert!(filter.matches(&task("tsk_2", TaskStatus::Done, Some("goal_1"))));
    }

    #[test]
    fn filter_dimensions_are_anded() {
        let filter = TaskFilter {
            status: Some(TaskStatus::Todo),
            goal_id: Some("goal_1".into()),
        };
        assert!(filter.matches(&task("tsk_1", TaskStatus::Todo, Some("goal_1"))));
        assert!(!filter.matches(&task("tsk_2", TaskStatus::Done, Some("goal_1"))));
        assert!(!filter.matches(&task("tsk_3", TaskStatus::Todo, Some("goal_2"))));
        assert!(!filter.matches(&task("tsk_4", TaskStatus::Todo, None)));
    }

    #[test]
    fn goal_filter_matches_on_status() {
        let filter = GoalFilter::with_status(GoalStatus::Active);
        let goal = Goal {
            id: "goal_1".into(),
            title: "t".into(),
            description: String::new(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: "t0".into(),
            updated_at: None,
        };
        assert!(filter.matches(&goal));
        assert!(!GoalFilter::with_status(GoalStatus::Paused).matches(&goal));
        assert!(GoalFilter::default().matches(&goal));
    }

    #[test]
    fn context_entry_omits_unset_ttl_fields() {
        let entry = ContextEntry {
            key: "mykey".into(),
            value: "myvalue".into(),
            created_at: "2026-10-13T16:42:10.630Z".into(),
            updated_at: "2026-10-13T16:42:10.630Z".into(),
            ttl_ms: None,
            expires_at: None,
        };
        assert_eq!(
            serde_json::to_string_pretty(&entry).unwrap(),
            r#"{
  "key": "mykey",
  "value": "myvalue",
  "created_at": "2026-10-13T16:42:10.630Z",
  "updated_at": "2026-10-13T16:42:10.630Z"
}"#
        );
    }
}
