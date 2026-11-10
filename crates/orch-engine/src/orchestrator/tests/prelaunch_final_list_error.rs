//! A final attempt whose workspace cannot be prepared fails the task and
//! cascades to its dependents, and the cascade needs the task list. When
//! that read fails, the probe already happened: it must still count, the
//! task must not be failed without its cascade (stranding the dependents),
//! and nothing may probe the workspace again. The attempt is saved with the
//! task left `retrying`, and the record is finished after a back-off.
//!
//! The faults hit only the cascade read: the one list made while the
//! dispatch gate is held.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::{RunStore, TaskFilter, TaskStore};
use orch_core::test_support::{
    FIXED_NOW_MS, FakeTaskStore, FakeWorkspaceManagerCall, make_agent, make_task,
};
use orch_domain::{
    DomainError, Goal, GoalStatus, OrchestratorEvent, RunStatus, Task, TaskStatus,
    calculate_retry_delay,
};

use super::harness::{Harness, NOW_ISO};
use crate::goal_service::GoalService;
use crate::task_service::TaskService;

const TASK: &str = "tsk_test1";
const DEPENDENT: &str = "tsk_dependent";
const AGENT: &str = "agt_test1";
const GOAL: &str = "goal_final";

/// Fails the next `fails` task lists made under the dispatch gate, and
/// counts every such list.
struct FailCascadeReads {
    inner: Arc<FakeTaskStore>,
    service: Arc<OnceLock<Arc<TaskService>>>,
    fails: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

#[async_trait]
impl TaskStore for FailCascadeReads {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        let gated = self
            .service
            .get()
            .is_some_and(|service| service.dispatch_gate.try_lock().is_err());
        if gated {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fails.load(Ordering::SeqCst) > 0 {
                self.fails.fetch_sub(1, Ordering::SeqCst);
                return Err(Error::other("transient task index read"));
            }
        }
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

struct Rig {
    h: Harness,
    fails: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

/// Retrying with one attempt left (attempts 2 of 3), nothing queued, in
/// daemon mode; plus a dependent that only the cascade fails.
fn final_attempt() -> Task {
    Task {
        assignee: Some(AGENT.into()),
        status: TaskStatus::Retrying,
        attempts: 2,
        ..make_task()
    }
}

fn dependent() -> Task {
    Task {
        id: DEPENDENT.into(),
        depends_on: vec![TASK.into()],
        ..make_task()
    }
}

fn workspace_error() -> DomainError {
    DomainError::Workspace {
        message: "not a git repository".into(),
        hint: None,
    }
}

fn rig_with(tasks: Vec<Task>, goals: Vec<Goal>, fails: usize) -> Rig {
    let service = Arc::new(OnceLock::new());
    let fails = Arc::new(AtomicUsize::new(fails));
    let reads = Arc::new(AtomicUsize::new(0));
    let h = Harness::builder()
        .tasks(tasks)
        .agents([make_agent()])
        .goals(goals)
        .wrap_task_store({
            let (service, fails, reads) =
                (Arc::clone(&service), Arc::clone(&fails), Arc::clone(&reads));
            move |inner| {
                Arc::new(FailCascadeReads {
                    inner,
                    service,
                    fails,
                    reads,
                }) as Arc<dyn TaskStore>
            }
        })
        .build();
    let _ = service.set(Arc::clone(&h.orchestrator.deps().task_service));
    h.workspace.faults.fail_always_with(workspace_error());
    h.orchestrator.enable_daemon_mode();
    Rig { h, fails, reads }
}

fn rig(fails: usize) -> Rig {
    rig_with(vec![final_attempt(), dependent()], Vec::new(), fails)
}

fn prepares(h: &Harness) -> usize {
    h.workspace
        .calls
        .matching(|call| matches!(call, FakeWorkspaceManagerCall::Prepare { .. }))
        .len()
}

/// The back-off of the final attempt.
fn delay(h: &Harness) -> u64 {
    let scheduling = &h.orchestrator.config().scheduling;
    calculate_retry_delay(
        2,
        scheduling.retry_base_delay_ms,
        scheduling.retry_max_delay_ms,
    )
}

fn cascade_reasons(h: &Harness) -> Vec<String> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            OrchestratorEvent::TaskCascadeFailed {
                task_id, reason, ..
            } if task_id == DEPENDENT => Some(reason),
            _ => None,
        })
        .collect()
}

/// The probe counted, the record pending: `retrying` at attempts 3 with a
/// back-off due `delay` after `now_ms`, the dependent untouched, nothing
/// claimed or preparing.
async fn assert_pending(h: &Harness, now_ms: i64) {
    let task = h.task(TASK);
    assert_eq!(
        (task.status, task.attempts),
        (TaskStatus::Retrying, 3),
        "the failed probe was not counted"
    );
    assert_eq!(
        h.task(DEPENDENT).status,
        TaskStatus::Todo,
        "the dependent failed before its dependency"
    );
    let state = h.persisted_state();
    assert_eq!(state.retry_queue.len(), 1, "{state:?}");
    let retry = &state.retry_queue[0];
    assert_eq!(retry.task_id, TASK);
    assert_eq!(retry.attempt, 3);
    assert_eq!(
        retry.due_at,
        orch_core::clock::iso_from_epoch_ms(now_ms + delay(h) as i64)
    );
    assert!(state.claimed.is_empty(), "orphan claim: {state:?}");
    assert!(
        !h.runs
            .list_for_task(TASK)
            .await
            .unwrap()
            .iter()
            .any(|run| run.status == RunStatus::Preparing),
        "a preparing run survived the failure"
    );
}

/// Recorded: failed at attempts 3, the dependent cascaded with the reason of
/// the failed probe, nothing left queued.
fn assert_recorded(h: &Harness) {
    let task = h.task(TASK);
    assert_eq!((task.status, task.attempts), (TaskStatus::Failed, 3));
    assert_eq!(h.task(DEPENDENT).status, TaskStatus::Failed);
    assert_eq!(
        cascade_reasons(h),
        [format!(
            "dependency {TASK} failed: {}",
            Error::Domain(workspace_error())
        )]
    );
    let state = h.persisted_state();
    assert!(state.retry_queue.is_empty(), "{state:?}");
    assert!(state.claimed.is_empty(), "{state:?}");
}

/// Ticks at the same instant: no probe, and no read either.
async fn assert_holds_off(rig: &Rig) {
    let (probes, reads) = (prepares(&rig.h), rig.reads.load(Ordering::SeqCst));
    for _ in 0..3 {
        rig.h.orchestrator.daemon_tick(true).await.unwrap();
    }
    assert_eq!(prepares(&rig.h), probes, "probed again while recording");
    assert_eq!(
        rig.reads.load(Ordering::SeqCst),
        reads,
        "retried the read without a back-off"
    );
}

/// The guard for the success path: a readable store fails the task and its
/// dependent in the same operation.
#[tokio::test]
async fn a_final_attempt_with_a_readable_store_is_recorded_at_once() {
    let rig = rig(0);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();

    assert_eq!(prepares(&rig.h), 1);
    assert_recorded(&rig.h);
}

#[tokio::test]
async fn a_transient_cascade_read_error_counts_the_probe_and_records_after_the_back_off() {
    let rig = rig(1);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();

    assert_eq!(
        rig.fails.load(Ordering::SeqCst),
        0,
        "the cascade never read"
    );
    assert_eq!(prepares(&rig.h), 1);
    assert_pending(&rig.h, FIXED_NOW_MS).await;
    assert_holds_off(&rig).await;

    rig.h.clock.advance_ms(delay(&rig.h) as i64 + 1);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(prepares(&rig.h), 1, "every probe must count: 3 of 3");
    assert_recorded(&rig.h);
}

/// Each failed read waits one more back-off; none probes.
#[tokio::test]
async fn repeated_cascade_read_errors_back_off_without_probing() {
    let rig = rig(3);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_pending(&rig.h, FIXED_NOW_MS).await;

    let mut now = FIXED_NOW_MS;
    for remaining in [1, 0] {
        rig.h.clock.advance_ms(delay(&rig.h) as i64 + 1);
        now += delay(&rig.h) as i64 + 1;
        rig.h.orchestrator.daemon_tick(true).await.unwrap();
        assert_eq!(rig.fails.load(Ordering::SeqCst), remaining);
        assert_pending(&rig.h, now).await;
        assert_holds_off(&rig).await;
    }
    assert_eq!(rig.reads.load(Ordering::SeqCst), 3);

    rig.h.clock.advance_ms(delay(&rig.h) as i64 + 1);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(prepares(&rig.h), 1);
    assert_recorded(&rig.h);
}

/// `orch run <id>` skips a back-off, but not the record: it finishes it.
#[tokio::test]
async fn an_explicit_run_records_the_pending_failure_instead_of_probing() {
    let rig = rig(1);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_pending(&rig.h, FIXED_NOW_MS).await;

    rig.h.orchestrator.daemon_launch_task(TASK).await.unwrap();
    assert_eq!(prepares(&rig.h), 1);
    assert_recorded(&rig.h);
}

#[tokio::test]
async fn a_cancel_while_the_record_is_pending_stands() {
    let rig = rig(1);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_pending(&rig.h, FIXED_NOW_MS).await;

    rig.h
        .orchestrator
        .deps()
        .task_service
        .cancel(TASK)
        .await
        .unwrap();
    rig.h.clock.advance_ms(delay(&rig.h) as i64 + 1);
    let reads = rig.reads.load(Ordering::SeqCst);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    rig.h.orchestrator.daemon_launch_task(TASK).await.unwrap();

    let task = rig.h.task(TASK);
    assert_eq!((task.status, task.attempts), (TaskStatus::Cancelled, 3));
    assert_eq!(prepares(&rig.h), 1);
    assert_eq!(rig.reads.load(Ordering::SeqCst), reads, "recorded anyway");
    assert!(cascade_reasons(&rig.h).is_empty());
    assert!(rig.h.persisted_state().claimed.is_empty());
}

/// A paused goal holds its pending record like any of its work, and the
/// record completes once the goal resumes.
#[tokio::test]
async fn a_paused_goal_holds_the_pending_record_until_resumed() {
    let rig = rig_with(
        vec![
            Task {
                goal_id: Some(GOAL.into()),
                ..final_attempt()
            },
            dependent(),
        ],
        vec![Goal {
            id: GOAL.into(),
            title: "final".into(),
            description: String::new(),
            status: GoalStatus::Active,
            assignee: Some(AGENT.into()),
            created_at: NOW_ISO.into(),
            updated_at: None,
        }],
        1,
    );
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_pending(&rig.h, FIXED_NOW_MS).await;

    let deps = rig.h.orchestrator.deps();
    let goals = GoalService::new(
        Arc::clone(deps.goal_store.as_ref().unwrap()),
        Arc::clone(&rig.h.bus),
        Arc::clone(&deps.clock),
        Arc::new(orch_core::test_support::SeqIdGenerator::new()),
        Some(Arc::clone(&deps.agent_service)),
        Some(Arc::clone(&deps.task_service)),
        None,
    );
    goals
        .update_status(GOAL, GoalStatus::Paused, false)
        .await
        .unwrap();
    rig.h.clock.advance_ms(delay(&rig.h) as i64 + 1);
    let reads = rig.reads.load(Ordering::SeqCst);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    rig.h.orchestrator.daemon_launch_task(TASK).await.unwrap();
    assert_eq!(rig.h.task(TASK).status, TaskStatus::Retrying);
    assert_eq!(rig.h.task(DEPENDENT).status, TaskStatus::Todo);
    assert_eq!(rig.reads.load(Ordering::SeqCst), reads);

    goals
        .update_status(GOAL, GoalStatus::Active, false)
        .await
        .unwrap();
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(prepares(&rig.h), 1);
    assert_recorded(&rig.h);
}

/// The pending record is on disk: a restarted daemon finishes it without
/// probing.
#[tokio::test]
async fn a_restarted_daemon_records_the_pending_failure_without_probing() {
    let rig = rig(1);
    rig.h.orchestrator.daemon_tick(true).await.unwrap();
    assert_pending(&rig.h, FIXED_NOW_MS).await;

    let restarted = Harness::builder()
        .tasks([rig.h.task(TASK), rig.h.task(DEPENDENT)])
        .agents([make_agent()])
        .state(rig.h.persisted_state())
        .build();
    restarted
        .workspace
        .faults
        .fail_always_with(workspace_error());
    restarted.orchestrator.enable_daemon_mode();
    restarted.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(restarted.task(TASK).status, TaskStatus::Retrying);

    restarted.clock.advance_ms(delay(&restarted) as i64 + 1);
    restarted.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(prepares(&restarted), 0, "the restart probed again");
    assert_recorded(&restarted);
}
