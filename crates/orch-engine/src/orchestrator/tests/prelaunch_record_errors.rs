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

use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Fails cancelled-run saves (the release's `finish`) and
/// counts the ones that went through. The fake's own faults are store-wide,
/// and the release lists the runs before it finishes one.
struct FailCancels {
    inner: Arc<FakeRunStore>,
    remaining: AtomicUsize,
    cancelled: Arc<AtomicUsize>,
}

#[async_trait]
impl RunStore for FailCancels {
    async fn save(&self, run: &Run) -> Result<()> {
        if run.status == RunStatus::Cancelled {
            if self
                .remaining
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
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
    rig_with_cancel_failures(cancelled, 1)
}

fn rig_with_cancel_failures(
    cancelled: Option<Arc<AtomicUsize>>,
    failures: usize,
) -> (Harness, Arm) {
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
            Arc::new(FailCancels {
                inner,
                remaining: AtomicUsize::new(failures),
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

/// Both the recorder's release and its error-path release fail. The launch
/// future has returned: a later tick must own the unfinished cleanup.
#[tokio::test]
async fn two_run_list_errors_are_reconciled_without_losing_the_backoff() {
    let (h, armed) = rig(None);
    let runs = Arc::clone(&h.runs);
    arm(&armed, move || {
        runs.faults.fail_next(2, "run index unavailable")
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert!(!h.runs.faults.is_armed(), "both release attempts must fail");
    assert!(h.in_memory_state().await.claimed.contains(TASK));
    assert_backs_off(&h).await;
}

#[tokio::test]
async fn two_state_errors_retain_the_preparation_until_cleanup_is_durable() {
    let (h, armed) = rig(None);
    let state = Arc::clone(&h.state);
    arm(&armed, move || {
        state.faults.fail_next(2, "state unavailable")
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .contains_key(TASK),
        "a failed state write discarded the preparation owner"
    );
    assert!(
        h.orchestrator.daemon_tick(true).await.is_err(),
        "the second state write must fail too"
    );
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .contains_key(TASK)
    );
    assert!(
        !h.state.faults.is_armed(),
        "both release attempts must fail"
    );
    assert_backs_off(&h).await;
}

#[tokio::test]
async fn two_run_finish_errors_keep_the_run_and_reservation_until_recovery() {
    let cancelled = Arc::new(AtomicUsize::new(0));
    let (h, _) = rig_with_cancel_failures(Some(cancelled.clone()), 2);
    h.runs.seed(Run {
        id: "run_left".into(),
        task_id: TASK.into(),
        status: RunStatus::Preparing,
        ..make_run()
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert_eq!(
        h.runs.peek("run_left").unwrap().status,
        RunStatus::Preparing
    );
    assert!(h.in_memory_state().await.claimed.contains(TASK));
    assert_eq!(cancelled.load(Ordering::SeqCst), 0);
    assert_backs_off(&h).await;
    assert_eq!(cancelled.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_permanent_cleanup_error_remains_visible_and_reserves_the_agent() {
    let (h, armed) = rig(None);
    let runs = h.runs.clone();
    arm(&armed, move || {
        runs.faults.fail_always("run index unavailable")
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    h.clock.advance_ms(60_000);
    for _ in 0..3 {
        h.orchestrator.daemon_tick(true).await.unwrap();
        assert!(h.persisted_state().claimed.contains(TASK));
        assert_eq!(h.task(TASK).attempts, 1);
        assert_eq!(prepares(&h), 1);
    }
    assert!(h.events().iter().any(|event| matches!(event,
        orch_domain::OrchestratorEvent::OrchestratorError { context, .. }
        if context == &format!("pending preparation cleanup {TASK}"))));
    h.runs.faults.clear();
    h.reconcile().await.unwrap();
    assert!(h.persisted_state().claimed.is_empty());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_stale_cleanup_cannot_release_a_superseding_preparation() {
    let (h, armed) = rig(None);
    let runs = h.runs.clone();
    arm(&armed, move || {
        runs.faults.fail_next(2, "run index unavailable")
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    let generation = h.orchestrator.inner.preparing_agents.lock().await[TASK].0;
    h.orchestrator
        .inner
        .preparing_agents
        .lock()
        .await
        .insert(TASK.into(), (generation + 1, AGENT.into()));
    h.runs.seed(Run {
        id: "run_new".into(),
        task_id: TASK.into(),
        status: RunStatus::Preparing,
        ..make_run()
    });
    let token = tokio_util::sync::CancellationToken::new();
    h.orchestrator
        .inner
        .cancels
        .lock()
        .await
        .insert(TASK.into(), token.clone());
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert!(h.persisted_state().claimed.contains(TASK));
    assert_eq!(
        h.orchestrator.inner.preparing_agents.lock().await[TASK].0,
        generation + 1
    );
    assert_eq!(h.runs.peek("run_new").unwrap().status, RunStatus::Preparing);
    assert!(!token.is_cancelled());
    assert_eq!(prepares(&h), 1);
}

#[tokio::test]
async fn cancelling_a_pending_cleanup_never_relaunches_the_cancelled_task() {
    let (h, armed) = rig(None);
    let runs = h.runs.clone();
    arm(&armed, move || {
        runs.faults.fail_next(2, "run index unavailable")
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    h.orchestrator.cancel_task(TASK).await.unwrap();
    h.clock.advance_ms(60_000);
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(h.task(TASK).status, TaskStatus::Cancelled);
    assert_eq!(prepares(&h), 1);
    assert!(h.persisted_state().claimed.is_empty());
    assert!(h.persisted_state().retry_queue.is_empty());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn pausing_a_goal_with_pending_cleanup_does_not_restore_its_cancelled_retry() {
    use crate::goal_service::GoalService;
    use orch_core::test_support::SeqIdGenerator;
    use orch_domain::{Goal, GoalStatus};

    let armed: Arm = Arc::new(Mutex::new(None));
    let goal = Goal {
        id: "goal_cleanup".into(),
        title: "cleanup".into(),
        description: String::new(),
        status: GoalStatus::Active,
        assignee: Some(AGENT.into()),
        created_at: super::harness::NOW_ISO.into(),
        updated_at: None,
    };
    let h = Harness::builder()
        .tasks([Task {
            assignee: Some(AGENT.into()),
            goal_id: Some(goal.id.clone()),
            labels: vec![orch_domain::AUTONOMOUS_LABEL.into()],
            ..make_task()
        }])
        .agents([make_agent()])
        .goals([goal.clone()])
        .wrap_task_store({
            let arm = armed.clone();
            move |inner| Arc::new(ArmOnRetry { inner, arm }) as Arc<dyn TaskStore>
        })
        .build();
    h.orchestrator.enable_daemon_mode();
    h.workspace.faults.fail_always("workspace unavailable");
    let runs = h.runs.clone();
    arm(&armed, move || {
        runs.faults.fail_next(2, "run index unavailable")
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    let deps = h.orchestrator.deps();
    let goals = GoalService::new(
        deps.goal_store.as_ref().unwrap().clone(),
        h.bus.clone(),
        deps.clock.clone(),
        Arc::new(SeqIdGenerator::new()),
        Some(deps.agent_service.clone()),
        Some(deps.task_service.clone()),
        None,
    );
    goals
        .update_status(&goal.id, GoalStatus::Paused, false)
        .await
        .unwrap();
    h.clock.advance_ms(60_000);
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(h.task(TASK).status, TaskStatus::Cancelled);
    assert!(h.persisted_state().claimed.is_empty());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty()
    );
    assert_eq!(prepares(&h), 1);
    goals
        .update_status(&goal.id, GoalStatus::Active, false)
        .await
        .unwrap();
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(prepares(&h), 1);
    assert_eq!(h.task(TASK).status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn shutdown_finishes_pending_preparing_runs_before_clearing_the_reservation() {
    let cancelled = Arc::new(AtomicUsize::new(0));
    let (h, _) = rig_with_cancel_failures(Some(cancelled), 2);
    h.runs.seed(Run {
        id: "run_left".into(),
        task_id: TASK.into(),
        status: RunStatus::Preparing,
        ..make_run()
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    h.orchestrator.daemon_stop().await.unwrap();
    assert_eq!(
        h.runs.peek("run_left").unwrap().status,
        RunStatus::Cancelled
    );
    assert!(h.persisted_state().claimed.is_empty());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty()
    );
    assert_eq!(prepares(&h), 1);
}

#[tokio::test]
async fn restart_sweeps_pending_preparations_and_preserves_the_recorded_backoff() {
    let cancelled = Arc::new(AtomicUsize::new(0));
    let (h, _) = rig_with_cancel_failures(Some(cancelled), 2);
    h.runs.seed(Run {
        id: "run_left".into(),
        task_id: TASK.into(),
        status: RunStatus::Preparing,
        ..make_run()
    });
    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    let restarted = Harness::builder()
        .tasks([h.task(TASK)])
        .agents([make_agent()])
        .state(h.persisted_state())
        .build();
    restarted.runs.seed(h.runs.peek("run_left").unwrap());
    restarted.orchestrator.daemon_initialize().await.unwrap();
    restarted.orchestrator.daemon_tick(true).await.unwrap();
    assert!(restarted.persisted_state().claimed.is_empty());
    assert_eq!(
        restarted.runs.peek("run_left").unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(
        restarted.persisted_state().retry_queue,
        h.persisted_state().retry_queue
    );
    assert_eq!(restarted.task(TASK).attempts, 1);
    assert_eq!(prepares(&restarted), 0);
}
