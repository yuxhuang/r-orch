//! Workspace manager port.

use std::path::Path;

use async_trait::async_trait;
use orch_domain::{Agent, OrchestratorConfig, Task};
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareResult {
    pub path: String,
    /// Set only in `worktree` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// The outcome of a merge: success, or a conflict carrying the raw output.
///
/// A non-conflict failure (missing branch, hook failure) also lands in
/// `Conflict`; the two are not distinguished in the result type, only in
/// whether `git merge --abort` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    Success,
    Conflict { conflict_info: String },
}

impl MergeResult {
    pub fn success(&self) -> bool {
        matches!(self, MergeResult::Success)
    }

    pub fn conflict_info(&self) -> Option<&str> {
        match self {
            MergeResult::Success => None,
            MergeResult::Conflict { conflict_info } => Some(conflict_info),
        }
    }
}

#[async_trait]
pub trait WorkspaceManager: Send + Sync {
    /// Resolve the mode (task override → agent config → project default),
    /// then create the worktree / isolated copy / reuse the shared root.
    async fn prepare(
        &self,
        task: &Task,
        agent: &Agent,
        config: &OrchestratorConfig,
    ) -> Result<PrepareResult>;

    /// `git merge --no-ff <branch> -m "Merge <branch>"`, aborting on conflict.
    async fn merge_back(&self, branch: &str) -> Result<MergeResult>;

    async fn cleanup(&self, task_id: &str, branch: Option<&str>) -> Result<()>;

    /// Path-containment check. Sync and fallible: returns `Ok(())` when the path
    /// is contained, an error otherwise.
    fn validate(&self, workspace_path: &Path, project_root: &Path) -> Result<()>;

    /// Files changed on a worktree branch relative to its merge-base.
    async fn get_changed_files(&self, branch: &str) -> Result<Vec<String>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_result_omits_an_absent_branch() {
        let result = PrepareResult {
            path: "/tmp/ws".into(),
            branch: None,
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"path":"/tmp/ws"}"#
        );
    }

    #[test]
    fn merge_result_accessors() {
        assert!(MergeResult::Success.success());
        assert_eq!(MergeResult::Success.conflict_info(), None);

        let conflict = MergeResult::Conflict {
            conflict_info: "CONFLICT (content)".into(),
        };
        assert!(!conflict.success());
        assert_eq!(conflict.conflict_info(), Some("CONFLICT (content)"));
    }
}
