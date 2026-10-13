//! Ported from `test/unit/application/orchestrator-dispatch-worktree.test.ts`
//! (bug-dom-001).
//!
//! Dispatch persists `proof.branch` immediately so a later failure cannot lose
//! the branch. The bug was that it wrote a **stale local copy** of the task,
//! reverting the `in_progress` status and the attempt counter it had just set;
//! the fix re-reads first.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::{FakeAdapter, FakeTaskStore, done_event, make_agent, make_task};
use orch_core::workspace::PrepareResult;
use orch_domain::{Agent, Task, TaskProof, TaskStatus};

use super::harness::{Harness, NOW_ISO};

fn done_adapter() -> Arc<FakeAdapter> {
    let adapter = FakeAdapter::new();
    adapter.set_events([done_event(NOW_ISO)]);
    Arc::new(adapter)
}

/// Dispatch one worktree task and return the harness.
async fn dispatch_worktree(branch: &str, workspace_path: &str, task: Task) -> Harness {
    let h = Harness::builder()
        .tasks([task])
        .agents([Agent {
            id: "agt_wt1".into(),
            ..make_agent()
        }])
        .adapter(done_adapter())
        .build();
    h.workspace.set_prepare_result(PrepareResult {
        path: workspace_path.to_owned(),
        branch: Some(branch.to_owned()),
    });

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;
    h
}

/// The save that carried `proof.branch` — the one the fix is about.
fn branch_save(h: &Harness, task_id: &str, branch: &str) -> Task {
    h.tasks
        .saved()
        .into_iter()
        .find(|task| {
            task.id == task_id
                && task.proof.as_ref().and_then(|p| p.branch.as_deref()) == Some(branch)
        })
        .expect("a save carrying proof.branch")
}

#[tokio::test]
async fn the_branch_save_carries_in_progress_not_the_stale_todo() {
    let h = dispatch_worktree(
        "orch/tsk_wt1",
        "/tmp/worktree/tsk_wt1",
        Task {
            id: "tsk_wt1".into(),
            ..make_task()
        },
    )
    .await;

    assert_eq!(
        branch_save(&h, "tsk_wt1", "orch/tsk_wt1").status,
        TaskStatus::InProgress
    );
}

#[tokio::test]
async fn the_branch_name_lands_on_the_proof() {
    let h = dispatch_worktree(
        "orch/tsk_wt1",
        "/tmp/worktree/tsk_wt1",
        Task {
            id: "tsk_wt1".into(),
            ..make_task()
        },
    )
    .await;

    assert_eq!(
        branch_save(&h, "tsk_wt1", "orch/tsk_wt1")
            .proof
            .unwrap()
            .branch
            .as_deref(),
        Some("orch/tsk_wt1")
    );
}

#[tokio::test]
async fn the_worktree_path_lands_on_the_task() {
    let path = "/tmp/worktrees/orch-tsk_wt1";
    let h = dispatch_worktree(
        "orch/tsk_wt1",
        path,
        Task {
            id: "tsk_wt1".into(),
            ..make_task()
        },
    )
    .await;

    assert_eq!(
        branch_save(&h, "tsk_wt1", "orch/tsk_wt1").workspace.as_deref(),
        Some(path)
    );
}

#[tokio::test]
async fn the_branch_save_carries_the_incremented_attempt_count() {
    let h = dispatch_worktree(
        "orch/tsk_wt1",
        "/tmp/worktree/tsk_wt1",
        Task {
            id: "tsk_wt1".into(),
            attempts: 0,
            ..make_task()
        },
    )
    .await;

    assert_eq!(branch_save(&h, "tsk_wt1", "orch/tsk_wt1").attempts, 1);
}

#[tokio::test]
async fn an_existing_files_changed_list_survives_the_branch_save() {
    let h = dispatch_worktree(
        "orch/tsk_wt1",
        "/tmp/worktree/tsk_wt1",
        Task {
            id: "tsk_wt1".into(),
            proof: Some(TaskProof {
                files_changed: vec!["README.md".into()],
                ..TaskProof::default()
            }),
            ..make_task()
        },
    )
    .await;

    assert_eq!(
        branch_save(&h, "tsk_wt1", "orch/tsk_wt1")
            .proof
            .unwrap()
            .files_changed,
        vec!["README.md".to_owned()]
    );
}

#[tokio::test]
async fn a_non_worktree_dispatch_writes_no_branch_at_all() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_nwt".into(),
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_nwt".into(),
            ..make_agent()
        }])
        .adapter(done_adapter())
        .build();
    // `prepare` returns no branch — a shared or isolated workspace.
    h.workspace.set_prepare_result(PrepareResult {
        path: "/tmp/ws/tsk_nwt".into(),
        branch: None,
    });

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    assert!(
        !h.tasks
            .saved()
            .into_iter()
            .any(|task| task.proof.as_ref().is_some_and(|p| p.branch.is_some()))
    );
}

/// Returns `None` from `get` once the call count passes a threshold, so the
/// post-dispatch re-read sees a task that has been deleted underneath it.
/// The TS test does the same with a counting `mockImplementation`.
struct VanishingTaskStore {
    inner: Arc<FakeTaskStore>,
    gets: std::sync::atomic::AtomicUsize,
    vanish_after: usize,
    vanishing_id: String,
}

#[async_trait]
impl TaskStore for VanishingTaskStore {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        let count = self
            .gets
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if count > self.vanish_after && id == self.vanishing_id {
            return Ok(None);
        }
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[tokio::test]
async fn a_task_deleted_mid_dispatch_does_not_break_the_tick() {
    // The re-read is a `let Some(…) = … else`, so a vanished task is skipped
    // rather than unwrapped.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_missing".into(),
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_missing".into(),
            ..make_agent()
        }])
        .adapter(done_adapter())
        .wrap_task_store(|inner| {
            Arc::new(VanishingTaskStore {
                inner,
                gets: std::sync::atomic::AtomicUsize::new(0),
                vanish_after: 6,
                vanishing_id: "tsk_missing".into(),
            }) as Arc<dyn TaskStore>
        })
        .build();
    h.workspace.set_prepare_result(PrepareResult {
        path: "/tmp/ws/missing".into(),
        branch: Some("orch/tsk_missing".into()),
    });

    h.load_state().await.unwrap();
    // The tick completes; the dispatch either lands or is skipped, but it
    // never panics and never leaves the task claimed.
    h.tick().await.unwrap();
    h.settle().await;
    assert!(h.persisted_state().claimed.is_empty());
}
