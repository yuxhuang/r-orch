//! The daemon's pre-launch workspace-failure recorder (perf F1b) queues the
//! retry in the in-memory state and releases the preparation in the same
//! commit. A step that fails after the retry was queued — listing or
//! finishing the task's runs, or the state write itself — must not let the
//! next operation reload state.json without it: the task is already saved
//! `retrying`, so a lost queue entry means the next tick prepares it again
//! with no back-off.
//!
//! Every failure is armed by the recorder's own save of the task, so it hits
//! exactly the step under test.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use orch_core::error::{Error, Result};
use orch_core::storage::{RunStore, TaskFilter, TaskStore};
use orch_core::test_support::{
    FIXED_NOW_MS, FakeRunStore, FakeTaskStore, FakeWorkspaceManagerCall, make_agent, make_run,
    make_task,
};
use orch_domain::{DomainError, Run, RunEvent, RunStatus, Task, TaskStatus};
use tokio_util::sync::CancellationToken;

use super::harness::Harness;

const TASK: &str = "tsk_test1";
const AGENT: &str = "agt_test1";

type Arm = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

/// Runs `arm` once, right after the recorder saved `TASK` as `retrying`.
struct ArmOnRetry {
    inner: Arc<FakeTaskStore>,
    arm: Arm,
}

#[async_trait]
impl TaskStore for ArmOnRetry {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.inner.save(task).await?;
        if task.id == TASK && task.status == TaskStatus::Retrying {
            let arm = self.arm.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(arm) = arm {
                arm();
            }
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

/// Fails the first save of a cancelled run (the release's `finish`) and
/// counts the ones that went through. The fake's own faults are store-wide,
/// and the release lists the runs before it finishes one.
struct FailFirstCancel {
    inner: Arc<FakeRunStore>,
    failed: AtomicBool,
    cancelled: Arc<AtomicUsize>,
}

#[async_trait]
impl RunStore for FailFirstCancel {
    async fn save(&self, run: &Run) -> Result<()> {
        if run.status == RunStatus::Cancelled {
            if !self.failed.swap(true, Ordering::SeqCst) {
                return Err(Error::other("transient run write failed"));
            }
            self.cancelled.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.save(run).await
    }
    async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.inner.get(id).await
    }
    async fn list_all(&self) -> Result<Vec<Run>> {
        self.inner.list_all().await
    }
    async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
        self.inner.list_for_task(task_id).await
    }
    async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>> {
        self.inner.list_for_agent(agent_id).await
    }
    async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()> {
        self.inner.append_event(run_id, event).await
    }
    async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
        self.inner.read_events(run_id).await
    }
    async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>> {
        self.inner.read_events_tail(run_id, count).await
    }
    fn stream_events(
        &self,
        run_id: &str,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<RunEvent>> {
        self.inner.stream_events(run_id, token)
    }
    fn close_run_events(&self, run_id: &str) {
        self.inner.close_run_events(run_id);
    }
}

/// A task whose workspace can never be prepared, in daemon mode.
fn rig(cancelled: Option<Arc<AtomicUsize>>) -> (Harness, Arm) {
    let arm: Arm = Arc::new(Mutex::new(None));
    let mut builder = Harness::builder()
        .tasks([Task {
            assignee: Some(AGENT.into()),
            ..make_task()
        }])
        .agents([make_agent()])
        .wrap_task_store({
            let arm = Arc::clone(&arm);
            move |inner| Arc::new(ArmOnRetry { inner, arm }) as Arc<dyn TaskStore>
        });
    if let Some(cancelled) = cancelled {
        builder = builder.wrap_run_store(move |inner| {
            Arc::new(FailFirstCancel {
                inner,
                failed: AtomicBool::new(false),
                cancelled,
            }) as Arc<dyn RunStore>
        });
    }
    let h = builder.build();
    h.workspace.faults.fail_always_with(DomainError::Workspace {
        message: "not a git repository".into(),
        hint: None,
    });
    h.orchestrator.enable_daemon_mode();
    (h, arm)
}

fn arm(arm: &Arm, action: impl FnOnce() + Send + 'static) {
    *arm.lock().unwrap() = Some(Box::new(action));
}

fn prepares(h: &Harness) -> usize {
    h.workspace
        .calls
        .matching(|call| matches!(call, FakeWorkspaceManagerCall::Prepare { .. }))
        .len()
}

/// After the failed launch: one attempt, its back-off on disk, nothing left
/// claimed or preparing, and no extra probe until the back-off is due.
async fn assert_backs_off(h: &Harness) {
    let task = h.task(TASK);
    assert_eq!(task.status, TaskStatus::Retrying);
    assert_eq!(task.attempts, 1);

    // An ordinary tick at the same instant: inside the back-off.
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(
        prepares(h),
        1,
        "the retry was lost: the next tick prepared again without waiting"
    );
    assert_eq!(h.task(TASK).attempts, 1);
    let state = h.persisted_state();
    assert_eq!(state.retry_queue.len(), 1, "{state:?}");
    let retry = &state.retry_queue[0];
    assert_eq!(retry.task_id, TASK);
    assert_eq!(retry.attempt, 1);
    let base = h.orchestrator.config().scheduling.retry_base_delay_ms;
    assert_eq!(
        retry.due_at,
        orch_core::clock::iso_from_epoch_ms(FIXED_NOW_MS + base as i64)
    );
    assert!(state.claimed.is_empty(), "orphan claim: {state:?}");
    assert!(
        !h.runs
            .list_for_task(TASK)
            .await
            .unwrap()
            .iter()
            .any(|run| run.status == RunStatus::Preparing),
        "a preparing run survived the release"
    );

    // Once due, the retry runs and counts again.
    h.clock.advance_ms(base as i64 + 1);
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(prepares(h), 2);
    assert_eq!(h.task(TASK).attempts, 2);
}

#[tokio::test]
async fn a_run_list_error_in_the_release_keeps_the_queued_retry() {
    let (h, armed) = rig(None);
    let runs = Arc::clone(&h.runs);
    arm(&armed, move || {
        runs.faults.fail_once("transient run index read")
    });

    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert!(
        !h.runs.faults.is_armed(),
        "the release never listed the runs"
    );
    assert_backs_off(&h).await;
}

/// A preparing run left behind (a release that failed before) is finished
/// by the next release; a failed `finish` is retried, and the run is
/// cancelled exactly once.
#[tokio::test]
async fn a_run_finish_error_in_the_release_keeps_the_retry_and_cancels_once() {
    let cancelled = Arc::new(AtomicUsize::new(0));
    let (h, _) = rig(Some(Arc::clone(&cancelled)));
    h.runs.seed(Run {
        id: "run_left".into(),
        task_id: TASK.into(),
        status: RunStatus::Preparing,
        ..make_run()
    });

    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert_eq!(
        h.runs.get("run_left").await.unwrap().unwrap().status,
        RunStatus::Cancelled
    );
    assert_backs_off(&h).await;
    assert_eq!(cancelled.load(Ordering::SeqCst), 1, "released twice");
}

/// The recording commit itself failed: the queued retry stays in memory and
/// is written before the next operation reads state.json.
#[tokio::test]
async fn a_state_write_error_in_the_record_is_retried_before_the_next_read() {
    let (h, armed) = rig(None);
    let state = Arc::clone(&h.state);
    arm(&armed, move || state.faults.fail_once("disk full"));

    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert!(!h.state.faults.is_armed(), "the record never wrote state");
    assert!(
        h.persisted_state().retry_queue.is_empty(),
        "the failed write reached the store"
    );
    assert_backs_off(&h).await;
}

/// The success path is unchanged: one state write for the record and the
/// release together.
#[tokio::test]
async fn a_clean_record_still_writes_state_once() {
    let (h, _) = rig(None);
    let before = h.state.calls.len();

    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    let writes = h.state.calls.calls()[before..]
        .iter()
        .filter(|call| matches!(call, orch_core::test_support::StateStoreCall::Write(_)))
        .count();
    assert_eq!(writes, 2, "selection plus one recording write");
    assert_backs_off(&h).await;
}
