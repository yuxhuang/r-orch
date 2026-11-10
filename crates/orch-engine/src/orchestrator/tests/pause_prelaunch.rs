//! Board-2b F2: a goal pause and the pre-launch workspace-failure recorder.
//!
//! The pause holds only the dispatch gate ("pause never takes state") and
//! cancels the goal's queued tasks. A task whose workspace preparation failed
//! is still `todo`/`retrying`, so the recorder's read → whole-record save of
//! it must not straddle that cancellation: a save from a read made before the
//! cancel would put the task back to `retrying` (relaunched after resume) or
//! to `failed` (cascading its dependents).
//!
//! The interleavings are forced, never timed: a task store that parks the
//! recorder right after its read, and single polls of futures that do no real
//! I/O. Every test runs under a deadline so a lock-order regression fails
//! instead of hanging.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Poll;
use std::time::Duration;

use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::{
    FakeTaskStore, FakeWorkspaceManager, FakeWorkspaceManagerCall, SeqIdGenerator, make_agent,
    make_task,
};
use orch_domain::{
    AUTONOMOUS_LABEL, DomainError, Goal, GoalStatus, OrchestratorState, Task, TaskStatus,
};
use tokio::sync::oneshot;

use super::harness::{Harness, NOW_ISO};
use crate::goal_service::GoalService;

const TASK: &str = "tsk_test1";
const GOAL: &str = "goal_f2";
const AGENT: &str = "agt_test1";
const DEADLINE: Duration = Duration::from_secs(20);

/// Parks the first read of `TASK` made after the workspace preparation
/// failed, *after* the read: the caller holds the old record and has not
/// saved yet. The test releases it.
struct ParkAfterRead {
    inner: Arc<FakeTaskStore>,
    workspace: Arc<OnceLock<Arc<FakeWorkspaceManager>>>,
    park: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}

impl ParkAfterRead {
    fn armed(&self) -> bool {
        self.workspace.get().is_some_and(|workspace| {
            !workspace
                .calls
                .matching(|call| matches!(call, FakeWorkspaceManagerCall::Prepare { .. }))
                .is_empty()
        })
    }
}

#[async_trait::async_trait]
impl TaskStore for ParkAfterRead {
    async fn list(&self, filter: &TaskFilter) -> orch_core::error::Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> orch_core::error::Result<Option<Task>> {
        let read = self.inner.get(id).await;
        let park = if id == TASK && self.armed() {
            self.park.lock().unwrap_or_else(|e| e.into_inner()).take()
        } else {
            None
        };
        if let Some((reached, release)) = park {
            let _ = reached.send(());
            let _ = release.await;
        }
        read
    }

    async fn save(&self, task: &Task) -> orch_core::error::Result<()> {
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

struct Interleaving {
    h: Harness,
    goals: Arc<GoalService>,
    /// Fires once the recorder has read the task and parked.
    reached: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}

fn goal(status: GoalStatus) -> Goal {
    Goal {
        id: GOAL.into(),
        title: "F2".into(),
        description: String::new(),
        status,
        assignee: Some(AGENT.into()),
        created_at: NOW_ISO.into(),
        updated_at: None,
    }
}

/// An autonomous task of the goal's assignee: exactly what a pause cancels.
fn goal_task(attempts: u32) -> Task {
    Task {
        assignee: Some(AGENT.into()),
        labels: vec![AUTONOMOUS_LABEL.to_owned()],
        goal_id: Some(GOAL.into()),
        attempts,
        ..make_task()
    }
}

/// A dependent outside the goal, so the pause itself never touches it.
fn dependent() -> Task {
    Task {
        id: "tsk_dependent".into(),
        depends_on: vec![TASK.into()],
        ..make_task()
    }
}

fn interleaving(tasks: Vec<Task>) -> Interleaving {
    let workspace = Arc::new(OnceLock::new());
    let (reached_tx, reached) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let h = Harness::builder()
        .tasks(tasks)
        .agents([make_agent()])
        .goals([goal(GoalStatus::Active)])
        .wrap_task_store({
            let workspace = Arc::clone(&workspace);
            move |inner| {
                Arc::new(ParkAfterRead {
                    inner,
                    workspace,
                    park: Mutex::new(Some((reached_tx, release_rx))),
                }) as Arc<dyn TaskStore>
            }
        })
        .build();
    let _ = workspace.set(Arc::clone(&h.workspace));
    // The project is not a repository: preparation fails before any run.
    h.workspace.faults.fail_always_with(DomainError::Workspace {
        message: "not a git repository".into(),
        hint: None,
    });
    let deps = h.orchestrator.deps();
    // The pause the daemon's `goal.status` runs: same bus, so the same
    // operation boundary, and the same task service, so the same gate.
    let goals = Arc::new(GoalService::new(
        Arc::clone(deps.goal_store.as_ref().unwrap()),
        Arc::clone(&h.bus),
        Arc::clone(&deps.clock),
        Arc::new(SeqIdGenerator::new()),
        Some(Arc::clone(&deps.agent_service)),
        Some(Arc::clone(&deps.task_service)),
        None,
    ));
    Interleaving {
        h,
        goals,
        reached,
        release,
    }
}

/// Start the pause and poll it once without a co-operative budget: with
/// nothing but in-memory stores it either completes or is blocked on a lock.
/// Returns whether it completed, and the rest of it to await.
async fn pause_once(
    goals: &Arc<GoalService>,
) -> (
    bool,
    tokio::task::JoinHandle<orch_core::error::Result<Goal>>,
) {
    let goals = Arc::clone(goals);
    let mut pause = Box::pin(tokio::task::unconstrained(async move {
        goals.update_status(GOAL, GoalStatus::Paused, false).await
    }));
    match futures::poll!(&mut pause) {
        Poll::Ready(result) => (true, tokio::spawn(async move { result })),
        Poll::Pending => (false, tokio::spawn(pause)),
    }
}

fn prepares_of(h: &Harness, task_id: &str) -> usize {
    h.workspace
        .calls
        .matching(|call| {
            matches!(call, FakeWorkspaceManagerCall::Prepare { task_id: id, .. } if id == task_id)
        })
        .len()
}

async fn resume_and_pass_the_back_off(h: &Harness, goals: &GoalService) {
    goals
        .update_status(GOAL, GoalStatus::Active, false)
        .await
        .unwrap();
    h.clock
        .advance_ms(h.orchestrator.config().scheduling.retry_max_delay_ms as i64 + 1);
}

/// The F2 sequence on the scheduler's caller (`dispatch_all`): the recorder
/// reads the task, the owner pauses the goal, the recorder saves. The pause's
/// cancellation must survive, and the retry the recorder queued must not
/// relaunch the cancelled task after the goal resumes.
#[tokio::test]
async fn a_pause_during_the_dispatch_failure_record_is_not_overwritten() {
    tokio::time::timeout(DEADLINE, async {
        let Interleaving {
            h,
            goals,
            reached,
            release,
        } = interleaving(vec![goal_task(0)]);
        h.own_lock();
        let tick = tokio::spawn({
            let orchestrator = h.orchestrator.clone();
            async move { orchestrator.tick_once().await }
        });
        reached.await.expect("the recorder read the task");

        let (paused_inside_the_window, pause) = pause_once(&goals).await;
        release.send(()).unwrap();
        tick.await.unwrap().unwrap();
        pause.await.unwrap().unwrap();

        let task = h.task(TASK);
        assert_eq!(
            task.status,
            TaskStatus::Cancelled,
            "the recorder's save overwrote the pause (saved: {:?})",
            h.saved_statuses(TASK)
        );
        assert!(
            !paused_inside_the_window,
            "the pause completed between the recorder's read and its save"
        );
        // The recorder read first, so its attempt stands; the pause then won.
        assert_eq!(task.attempts, 1);
        assert_eq!(
            h.saved_statuses(TASK),
            vec![TaskStatus::Retrying, TaskStatus::Cancelled]
        );

        // Reverse order: the retry it queued comes due after the resume and
        // finds a cancelled task, which it drops without a launch.
        resume_and_pass_the_back_off(&h, &goals).await;
        h.tick().await.unwrap();
        assert_eq!(h.task(TASK).status, TaskStatus::Cancelled);
        assert!(!h.was_dispatched(TASK));
        assert_eq!(prepares_of(&h, TASK), 1);
        assert!(
            !h.persisted_state()
                .retry_queue
                .iter()
                .any(|retry| retry.task_id == TASK)
        );
    })
    .await
    .expect("deadlock: the recorder and the pause did not both finish");
}

/// The same sequence on the daemon's caller (`daemon_record_workspace_failure`).
/// The record and the pause are both whole operations on the project
/// boundary, so the pause already waits for the record; the gate inside the
/// recorder must not change that order or deadlock against it.
#[tokio::test]
async fn a_pause_during_the_daemon_failure_record_waits_for_it() {
    tokio::time::timeout(DEADLINE, async {
        let Interleaving {
            h,
            goals,
            reached,
            release,
        } = interleaving(vec![goal_task(0)]);
        h.orchestrator.enable_daemon_mode();
        let launch = tokio::spawn({
            let orchestrator = h.orchestrator.clone();
            async move { orchestrator.daemon_launch_task(TASK).await }
        });
        reached.await.expect("the recorder read the task");

        let (paused_inside_the_window, pause) = pause_once(&goals).await;
        release.send(()).unwrap();
        let launched = launch.await.unwrap();
        assert!(
            matches!(
                launched,
                Err(orch_core::error::Error::Domain(
                    DomainError::Workspace { .. }
                ))
            ),
            "{launched:?}"
        );
        pause.await.unwrap().unwrap();

        assert!(!paused_inside_the_window);
        let task = h.task(TASK);
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert_eq!(task.attempts, 1);

        resume_and_pass_the_back_off(&h, &goals).await;
        h.orchestrator
            .daemon_tick_with_guard(true, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(h.task(TASK).status, TaskStatus::Cancelled);
        assert!(!h.was_dispatched(TASK));
        assert_eq!(prepares_of(&h, TASK), 1);
    })
    .await
    .expect("deadlock: the recorder and the pause did not both finish");
}

/// Pause first: a recorder that starts while the pause holds the gate does
/// not read the task until the pause has cancelled it, then records nothing.
async fn recorder_behind_a_held_gate(
    attempts: u32,
    from_the_scheduler: bool,
) -> (Harness, OrchestratorState, bool, bool) {
    let Interleaving {
        h,
        reached: mut reached_rx,
        release,
        ..
    } = interleaving(vec![goal_task(attempts), dependent()]);
    // Arm the park: this is the failure the recorder accounts.
    h.workspace.calls.record(FakeWorkspaceManagerCall::Prepare {
        task_id: TASK.into(),
        agent_id: AGENT.into(),
    });
    let error = orch_core::error::Error::Domain(DomainError::Workspace {
        message: "not a git repository".into(),
        hint: None,
    });
    // The scheduler passes its pass's task list; the daemon lists on demand.
    let snapshot = if from_the_scheduler {
        Some(h.tasks.list(&TaskFilter::default()).await.unwrap())
    } else {
        None
    };
    let deps = h.orchestrator.deps();
    let gate = deps.task_service.dispatch_gate.lock().await;

    let mut state = OrchestratorState::default();
    let mut record = Box::pin(tokio::task::unconstrained(
        h.orchestrator.record_workspace_dispatch_failure(
            &mut state,
            TASK,
            snapshot.as_deref(),
            &error,
        ),
    ));
    assert!(futures::poll!(&mut record).is_pending());
    let read_under_the_gate = reached_rx.try_recv().is_ok();

    // Released first: if the recorder has not parked yet, the cancel's own
    // read takes the park instead and must not wait on it.
    release.send(()).unwrap();
    // What the pause does under the gate.
    deps.task_service.cancel_record(TASK).await.unwrap();
    drop(gate);
    let queued = record.await;
    (h, state, queued, read_under_the_gate)
}

#[tokio::test]
async fn a_recorder_behind_a_pause_sees_the_cancel_and_queues_no_retry() {
    tokio::time::timeout(DEADLINE, async {
        let (h, state, queued, read_under_the_gate) = recorder_behind_a_held_gate(0, true).await;
        let task = h.task(TASK);
        assert_eq!(
            task.status,
            TaskStatus::Cancelled,
            "the recorder's save overwrote the pause (saved: {:?})",
            h.saved_statuses(TASK)
        );
        assert!(
            !read_under_the_gate,
            "the recorder read the task while the gate was held"
        );
        assert_eq!(task.attempts, 0);
        assert!(!queued);
        assert!(state.retry_queue.is_empty());
    })
    .await
    .expect("deadlock: the recorder did not finish after the gate was released");
}

#[tokio::test]
async fn a_final_attempt_behind_a_pause_neither_fails_the_task_nor_its_dependents() {
    tokio::time::timeout(DEADLINE, async {
        let (h, state, queued, read_under_the_gate) = recorder_behind_a_held_gate(2, false).await;
        let task = h.task(TASK);
        assert_eq!(
            task.status,
            TaskStatus::Cancelled,
            "the recorder's save overwrote the pause (saved: {:?})",
            h.saved_statuses(TASK)
        );
        assert_eq!(
            h.task("tsk_dependent").status,
            TaskStatus::Todo,
            "the dependent was cascade-failed"
        );
        assert!(!read_under_the_gate);
        assert_eq!(task.attempts, 2);
        assert!(!queued);
        assert!(state.retry_queue.is_empty());
    })
    .await
    .expect("deadlock: the recorder did not finish after the gate was released");
}
