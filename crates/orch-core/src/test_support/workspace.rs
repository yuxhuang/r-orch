//! A workspace manager that touches no git repository.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orch_domain::{Agent, OrchestratorConfig, Task};

use crate::error::Result;
use crate::test_support::faults::Faults;
use crate::test_support::recorder::Recorder;
use crate::workspace::{MergeResult, PrepareResult, WorkspaceManager};

/// The path the fake workspace manager returns from `prepare`.
pub const FAKE_WORKSPACE: &str = "/tmp/ws";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeWorkspaceManagerCall {
    Prepare {
        task_id: String,
        agent_id: String,
    },
    MergeBack(String),
    Cleanup {
        task_id: String,
        branch: Option<String>,
    },
    Validate {
        workspace_path: PathBuf,
        project_root: PathBuf,
    },
    GetChangedFiles(String),
}

#[derive(Debug, Clone)]
pub struct FakeWorkspaceManager {
    prepare_result: Arc<Mutex<PrepareResult>>,
    merge_result: Arc<Mutex<MergeResult>>,
    changed_files: Arc<Mutex<Vec<String>>>,
    pub calls: Recorder<FakeWorkspaceManagerCall>,
    pub faults: Faults,
}

impl Default for FakeWorkspaceManager {
    fn default() -> Self {
        FakeWorkspaceManager::new()
    }
}

impl FakeWorkspaceManager {
    /// `prepare → { path: "/tmp/ws" }`, `merge_back → success`,
    /// `get_changed_files → []`.
    pub fn new() -> Self {
        FakeWorkspaceManager {
            prepare_result: Arc::new(Mutex::new(PrepareResult {
                path: FAKE_WORKSPACE.to_owned(),
                branch: None,
            })),
            merge_result: Arc::new(Mutex::new(MergeResult::Success)),
            changed_files: Arc::new(Mutex::new(Vec::new())),
            calls: Recorder::new(),
            faults: Faults::new(),
        }
    }

    /// Make `prepare` report a worktree, so the merge-back path is exercised.
    pub fn set_prepare_result(&self, result: PrepareResult) {
        *self
            .prepare_result
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = result;
    }

    pub fn set_merge_result(&self, result: MergeResult) {
        *self.merge_result.lock().unwrap_or_else(|e| e.into_inner()) = result;
    }

    pub fn set_changed_files(&self, files: impl IntoIterator<Item = String>) {
        *self.changed_files.lock().unwrap_or_else(|e| e.into_inner()) = files.into_iter().collect();
    }

    /// Branches passed to `merge_back`, in order.
    pub fn merged(&self) -> Vec<String> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeWorkspaceManagerCall::MergeBack(branch) => Some(branch),
                _ => None,
            })
            .collect()
    }

    /// `(task_id, branch)` passed to `cleanup`, in order.
    pub fn cleaned(&self) -> Vec<(String, Option<String>)> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeWorkspaceManagerCall::Cleanup { task_id, branch } => Some((task_id, branch)),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl WorkspaceManager for FakeWorkspaceManager {
    async fn prepare(
        &self,
        task: &Task,
        agent: &Agent,
        _config: &OrchestratorConfig,
    ) -> Result<PrepareResult> {
        self.calls.record(FakeWorkspaceManagerCall::Prepare {
            task_id: task.id.clone(),
            agent_id: agent.id.clone(),
        });
        self.faults.check()?;
        Ok(self
            .prepare_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    async fn merge_back(&self, branch: &str) -> Result<MergeResult> {
        self.calls
            .record(FakeWorkspaceManagerCall::MergeBack(branch.to_owned()));
        self.faults.check()?;
        Ok(self
            .merge_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }

    async fn cleanup(&self, task_id: &str, branch: Option<&str>) -> Result<()> {
        self.calls.record(FakeWorkspaceManagerCall::Cleanup {
            task_id: task_id.to_owned(),
            branch: branch.map(str::to_owned),
        });
        self.faults.check()?;
        Ok(())
    }

    fn validate(&self, workspace_path: &Path, project_root: &Path) -> Result<()> {
        self.calls.record(FakeWorkspaceManagerCall::Validate {
            workspace_path: workspace_path.to_path_buf(),
            project_root: project_root.to_path_buf(),
        });
        self.faults.check()
    }

    async fn get_changed_files(&self, branch: &str) -> Result<Vec<String>> {
        self.calls
            .record(FakeWorkspaceManagerCall::GetChangedFiles(branch.to_owned()));
        self.faults.check()?;
        Ok(self
            .changed_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::factories::{make_agent, make_task};
    use orch_domain::default_config;

    #[tokio::test]
    async fn a_new_fake_workspace_manager_has_its_documented_defaults() {
        let ws = FakeWorkspaceManager::new();
        let result = ws
            .prepare(&make_task(), &make_agent(), &default_config())
            .await
            .unwrap();
        assert_eq!(result.path, "/tmp/ws");
        assert_eq!(result.branch, None);
        assert!(ws.merge_back("orch/tsk_test1").await.unwrap().success());
        assert!(
            ws.get_changed_files("orch/tsk_test1")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(ws.validate(Path::new("/tmp/ws"), Path::new("/tmp")).is_ok());
    }

    #[tokio::test]
    async fn a_worktree_prepare_result_is_configurable() {
        let ws = FakeWorkspaceManager::new();
        ws.set_prepare_result(PrepareResult {
            path: "/tmp/project/.orchestry/workspaces/tsk_test1".into(),
            branch: Some("orch/tsk_test1".into()),
        });
        let result = ws
            .prepare(&make_task(), &make_agent(), &default_config())
            .await
            .unwrap();
        assert_eq!(result.branch.as_deref(), Some("orch/tsk_test1"));
    }

    #[tokio::test]
    async fn a_merge_conflict_is_configurable() {
        let ws = FakeWorkspaceManager::new();
        ws.set_merge_result(MergeResult::Conflict {
            conflict_info: "CONFLICT (content)".into(),
        });
        let result = ws.merge_back("orch/tsk_test1").await.unwrap();
        assert_eq!(result.conflict_info(), Some("CONFLICT (content)"));
        assert_eq!(ws.merged(), vec!["orch/tsk_test1".to_owned()]);
    }

    #[tokio::test]
    async fn records_prepare_cleanup_and_changed_files() {
        let ws = FakeWorkspaceManager::new();
        ws.set_changed_files(["src/a.rs".to_owned()]);
        ws.prepare(&make_task(), &make_agent(), &default_config())
            .await
            .unwrap();
        ws.cleanup("tsk_test1", Some("orch/tsk_test1"))
            .await
            .unwrap();
        ws.cleanup("tsk_2", None).await.unwrap();

        assert_eq!(
            ws.get_changed_files("orch/tsk_test1").await.unwrap(),
            vec!["src/a.rs"]
        );
        assert_eq!(
            ws.cleaned(),
            vec![
                ("tsk_test1".to_owned(), Some("orch/tsk_test1".to_owned())),
                ("tsk_2".to_owned(), None),
            ]
        );
        assert!(ws.calls.contains(&FakeWorkspaceManagerCall::Prepare {
            task_id: "tsk_test1".into(),
            agent_id: "agt_test1".into(),
        }));
    }

    #[tokio::test]
    async fn faults_fail_prepare_and_validate() {
        let ws = FakeWorkspaceManager::new();
        ws.faults.fail_always("worktree add failed");
        assert_eq!(
            ws.prepare(&make_task(), &make_agent(), &default_config())
                .await
                .unwrap_err()
                .to_string(),
            "worktree add failed"
        );
        assert!(
            ws.validate(Path::new("/elsewhere"), Path::new("/tmp"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn works_as_a_trait_object() {
        let ws: Arc<dyn WorkspaceManager> = Arc::new(FakeWorkspaceManager::new());
        assert!(ws.merge_back("b").await.unwrap().success());
    }
}
