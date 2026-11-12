//! A stop must order its spawn seal against the whole durable launch phase.
use super::harness::{Harness, LaunchProcesses, reserve_process_launch};
use async_trait::async_trait;
use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::{FakeTaskStore, make_agent, make_task};
use orch_domain::{AgentStatus, OrchestratorEventType, RunStatus, Task, TaskStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct Barrier {
    entered: Notify,
    release: Notify,
    armed: AtomicBool,
}

struct HeldWrite {
    inner: Arc<FakeTaskStore>,
    barrier: Arc<Barrier>,
}

#[async_trait]
impl TaskStore for HeldWrite {
    async fn get(&self, id: &str) -> orch_core::error::Result<Option<Task>> {
        self.inner.get(id).await
    }
    async fn list(&self, filter: &TaskFilter) -> orch_core::error::Result<Vec<Task>> {
        self.inner.list(filter).await
    }
    async fn save(&self, task: &Task) -> orch_core::error::Result<()> {
        if task.status == TaskStatus::InProgress && self.barrier.armed.swap(false, Ordering::SeqCst)
        {
            self.barrier.entered.notify_one();
            self.barrier.release.notified().await;
        }
        self.inner.save(task).await
    }
    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

async fn stop(h: &Harness, work: &LaunchProcesses) {
    h.orchestrator
        .daemon_stop_with_spawn_seal(|| work.seal())
        .await
        .unwrap();
}

async fn stop_during_launch(max_attempts: u32) {
    let root = tempfile::tempdir().unwrap();
    let work = Arc::new(LaunchProcesses::default());
    let barrier = Arc::new(Barrier::default());
    let mut agent = make_agent();
    agent.config.command = Some("exec /bin/sleep 120".into());
    let mut task = make_task();
    task.max_attempts = max_attempts;
    task.assignee = Some(agent.id.clone());
    let mut dependent = make_task();
    dependent.id = "tsk_dependent".into();
    dependent.depends_on = vec![task.id.clone()];
    let h = Harness::builder()
        .tasks([task.clone(), dependent.clone()])
        .agents([agent.clone()])
        .adapter_impl(work.shell_adapter(root.path()))
        .wrap_task_store({
            let barrier = barrier.clone();
            move |inner| Arc::new(HeldWrite { inner, barrier })
        })
        .build();
    h.workspace
        .set_prepare_result(orch_core::workspace::PrepareResult {
            path: root.path().to_str().unwrap().into(),
            branch: None,
        });
    h.orchestrator.daemon_initialize().await.unwrap();
    h.orchestrator.set_spawn_admission(work.admission());
    barrier.armed.store(true, Ordering::SeqCst);
    let launch = h.orchestrator.daemon_tick(true);
    tokio::pin!(launch);
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch did not reach the write barrier: {result:?}"),
        () = barrier.entered.notified() => (),
    }
    let stop = stop(&h, &work);
    tokio::pin!(stop);
    // Poll to the boundary wait. No timer decides the winner: the launch
    // already owns that boundary and its first write is still held.
    assert!(futures::poll!(&mut stop).is_pending());
    barrier.release.notify_one();
    let (launch, ()) = tokio::join!(launch, stop);
    launch.unwrap();
    let current = h.task(&task.id);
    let runs = h.runs.snapshot();
    let current_agent = h.agent(&agent.id);
    let errors = h.events_of(OrchestratorEventType::OrchestratorError);
    eprintln!(
        "max_attempts={max_attempts}; task={current:?}; dependent={:?}; runs={runs:?}; agent={current_agent:?}; errors={errors:?}",
        h.task(&dependent.id)
    );
    if !errors.is_empty() {
        assert_eq!(current.attempts, 1);
        assert_eq!(
            current.status,
            if max_attempts == 1 {
                TaskStatus::Failed
            } else {
                TaskStatus::Retrying
            }
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Failed);
        assert_eq!(runs[0].error.as_deref(), Some("spawn admission closed"));
        assert_eq!(current_agent.stats.tasks_failed, 1);
        assert_eq!(current_agent.stats.total_runs, 1);
        assert_eq!(
            current_agent.last_error.as_ref().unwrap().message,
            "spawn admission closed"
        );
        assert_eq!(
            h.task(&dependent.id).status,
            if max_attempts == 1 {
                TaskStatus::Failed
            } else {
                TaskStatus::Todo
            }
        );
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], orch_domain::OrchestratorEvent::OrchestratorError { context, .. } if context == "dispatch task tsk_test1")
        );
    }
    assert!(
        errors.is_empty(),
        "a clean stop produced an orchestrator error: {errors:?}"
    );
    assert_eq!(current.attempts, 1);
    assert_eq!(
        current.status,
        if max_attempts == 1 {
            TaskStatus::Failed
        } else {
            TaskStatus::Retrying
        }
    );
    assert!(runs.iter().all(|run| run.status == RunStatus::Cancelled));
    assert_eq!(current_agent.status, AgentStatus::Idle);
    assert_eq!(current_agent.stats.tasks_failed, agent.stats.tasks_failed);
    assert_eq!(current_agent.stats.total_runs, agent.stats.total_runs);
    assert_eq!(current_agent.last_error, agent.last_error);
    assert_eq!(h.task(&dependent.id).status, TaskStatus::Todo);
    assert!(h.persisted_state().claimed.is_empty());
    assert!(h.persisted_state().running.is_empty());
    work.wait_idle().await;
}

#[tokio::test]
async fn launch_committed_before_stop_consumes_attempt_and_retries() {
    tokio::time::timeout(std::time::Duration::from_secs(10), stop_during_launch(3))
        .await
        .unwrap();
}

#[tokio::test]
async fn last_attempt_committed_before_stop_fails_task_without_cascade() {
    tokio::time::timeout(std::time::Duration::from_secs(10), stop_during_launch(1))
        .await
        .unwrap();
}

struct HeldAdmission(Arc<Barrier>);

#[async_trait]
impl orch_core::process::SpawnAdmission for HeldAdmission {
    async fn reserve(&self) -> Box<dyn orch_core::process::SpawnReservation> {
        let reservation = reserve_process_launch().await;
        self.0.entered.notify_one();
        self.0.release.notified().await;
        reservation
    }
}

async fn stop_before_launch(max_attempts: u32) {
    let mut task = make_task();
    task.max_attempts = max_attempts;
    task.assignee = Some(make_agent().id);
    let mut state = orch_domain::OrchestratorState::default();
    if max_attempts > 1 {
        task.status = TaskStatus::Retrying;
        task.attempts = 1;
        state.retry_queue.push(orch_domain::RetryEntry {
            task_id: task.id.clone(),
            attempt: 1,
            due_at: "2099-01-01T00:00:00.000Z".into(),
            error: "previous failure".into(),
        });
    }
    let queue = state.retry_queue.clone();
    let agent = make_agent();
    let h = Harness::builder()
        .tasks([task.clone()])
        .agents([agent.clone()])
        .state(state)
        .build();
    h.orchestrator.daemon_initialize().await.unwrap();
    let barrier = Arc::new(Barrier::default());
    h.orchestrator
        .set_spawn_admission(Arc::new(HeldAdmission(barrier.clone())));
    let launch = h.orchestrator.daemon_launch_task(&task.id);
    tokio::pin!(launch);
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch did not reach admission: {result:?}"),
        () = barrier.entered.notified() => (),
    }
    let work = LaunchProcesses::default();
    stop(&h, &work).await;
    barrier.release.notify_one();
    launch.await.unwrap();
    assert_eq!(h.task(&task.id), task);
    assert_eq!(h.agent(&agent.id), agent);
    assert_eq!(h.persisted_state().retry_queue, queue);
    assert!(h.persisted_state().claimed.is_empty());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty()
    );
    assert!(h.adapter.execute_calls.calls().is_empty());
    assert!(
        h.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .is_empty()
    );

    // Assemble a successor from the saved state and records, as a restart
    // does. Its first actual launch must use the attempt the stop preserved.
    let successor = Harness::builder()
        .tasks([h.task(&task.id)])
        .agents([h.agent(&agent.id)])
        .state(h.persisted_state())
        .build();
    successor.orchestrator.daemon_initialize().await.unwrap();
    successor
        .orchestrator
        .daemon_launch_task(&task.id)
        .await
        .unwrap();
    assert_eq!(successor.task(&task.id).attempts, task.attempts + 1);
    assert_eq!(successor.runs.snapshot()[0].attempt, task.attempts + 1);
    successor.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn stop_winning_launch_preserves_retry_and_attempt() {
    tokio::time::timeout(std::time::Duration::from_secs(10), stop_before_launch(3))
        .await
        .unwrap();
}

#[tokio::test]
async fn guard_stop_winning_last_attempt_preserves_task() {
    tokio::time::timeout(std::time::Duration::from_secs(10), stop_before_launch(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn genuine_launch_failure_during_stop_is_still_recorded() {
    let adapter = Arc::new(orch_core::test_support::FakeAdapter::new());
    adapter.set_execute_error(Some(orch_core::adapter::AdapterError::new(
        "missing executable",
        None,
    )));
    let barrier = Arc::new(Barrier::default());
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(adapter)
        .wrap_task_store({
            let barrier = barrier.clone();
            move |inner| Arc::new(HeldWrite { inner, barrier })
        })
        .build();
    h.orchestrator.daemon_initialize().await.unwrap();
    barrier.armed.store(true, Ordering::SeqCst);
    let launch = h.orchestrator.daemon_tick(true);
    tokio::pin!(launch);
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch missed the write barrier: {result:?}"),
        () = barrier.entered.notified() => (),
    }
    let work = LaunchProcesses::default();
    let stop = stop(&h, &work);
    tokio::pin!(stop);
    assert!(futures::poll!(&mut stop).is_pending());
    barrier.release.notify_one();
    let (launch, ()) = tokio::join!(launch, stop);
    launch.unwrap();
    assert_eq!(h.task(&make_task().id).attempts, 1);
    assert_eq!(h.runs.snapshot()[0].status, RunStatus::Failed);
    assert_eq!(h.agent(&make_agent().id).stats.tasks_failed, 1);
    assert_eq!(
        h.events_of(OrchestratorEventType::OrchestratorError).len(),
        1
    );
}

async fn refused_without_stop(sealed: bool) {
    let root = tempfile::tempdir().unwrap();
    let work = Arc::new(LaunchProcesses::default());
    if sealed {
        work.seal();
    }
    // A nonexistent working directory makes the real process port return
    // ENOENT without starting bash or any model CLI.
    let mut agent = make_agent();
    agent.config.command = Some("true".into());
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent])
        .adapter_impl(work.shell_adapter(root.path()))
        .build();
    h.workspace
        .set_prepare_result(orch_core::workspace::PrepareResult {
            path: root
                .path()
                .join("missing-directory")
                .to_str()
                .unwrap()
                .into(),
            branch: None,
        });
    h.orchestrator.daemon_initialize().await.unwrap();
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert!(!h.orchestrator.is_shutting_down());
    assert_eq!(h.task(&make_task().id).attempts, 1);
    assert_eq!(h.task(&make_task().id).status, TaskStatus::Retrying);
    assert_eq!(h.runs.snapshot()[0].status, RunStatus::Failed);
    assert_eq!(h.agent(&make_agent().id).stats.tasks_failed, 1);
    assert_eq!(h.agent(&make_agent().id).stats.total_runs, 1);
    assert!(h.agent(&make_agent().id).last_error.is_some());
    assert_eq!(
        h.events_of(OrchestratorEventType::OrchestratorError).len(),
        1
    );
    h.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn a_real_spawn_error_without_stop_consumes_the_attempt() {
    refused_without_stop(false).await;
}

#[tokio::test]
async fn a_tracker_refusal_without_stop_is_not_swallowed() {
    refused_without_stop(true).await;
}

#[tokio::test]
async fn dropping_launch_during_first_write_releases_admission() {
    let root = tempfile::tempdir().unwrap();
    let work = Arc::new(LaunchProcesses::default());
    let barrier = Arc::new(Barrier::default());
    let mut agent = make_agent();
    agent.config.command = Some("true".into());
    let task = make_task();
    let h = Harness::builder()
        .tasks([task.clone()])
        .agents([agent])
        .adapter_impl(work.shell_adapter(root.path()))
        .wrap_task_store({
            let barrier = barrier.clone();
            move |inner| Arc::new(HeldWrite { inner, barrier })
        })
        .build();
    h.orchestrator.set_spawn_admission(work.admission());
    h.orchestrator.daemon_initialize().await.unwrap();
    barrier.armed.store(true, Ordering::SeqCst);
    let mut launch = Box::pin(h.orchestrator.daemon_launch_task(&task.id));
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch missed first write: {result:?}"),
        () = barrier.entered.notified() => (),
    }
    assert!(!work.is_idle());
    work.seal();
    drop(launch);
    assert!(work.is_idle());
    tokio::time::timeout(std::time::Duration::from_secs(10), stop(&h, &work))
        .await
        .unwrap();
    assert_eq!(h.task(&task.id), task);
    assert!(
        h.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .is_empty()
    );
}

struct CancelAfterAdmission {
    inner: Arc<dyn orch_core::process::SpawnAdmission>,
    guard: Arc<AtomicBool>,
}
struct CancellingReservation {
    inner: Box<dyn orch_core::process::SpawnReservation>,
    guard: Arc<AtomicBool>,
}
#[async_trait]
impl orch_core::process::SpawnAdmission for CancelAfterAdmission {
    async fn reserve(&self) -> Box<dyn orch_core::process::SpawnReservation> {
        Box::new(CancellingReservation {
            inner: self.inner.reserve().await,
            guard: self.guard.clone(),
        })
    }
}
impl orch_core::process::SpawnReservation for CancellingReservation {
    fn try_admit(&mut self) -> bool {
        let admitted = self.inner.try_admit();
        assert!(admitted);
        self.guard.store(true, Ordering::SeqCst);
        admitted
    }
    fn enter(self: Box<Self>) -> Box<dyn orch_core::process::SpawnScope> {
        self.inner.enter()
    }
}

#[tokio::test]
async fn cancellation_at_final_checks_releases_admitted_launch() {
    let work = Arc::new(LaunchProcesses::default());
    let guard = Arc::new(AtomicBool::new(false));
    let task = make_task();
    let agent = make_agent();
    let h = Harness::builder()
        .tasks([task.clone()])
        .agents([agent.clone()])
        .build();
    h.orchestrator
        .set_spawn_admission(Arc::new(CancelAfterAdmission {
            inner: work.admission(),
            guard: guard.clone(),
        }));
    h.orchestrator.daemon_initialize().await.unwrap();
    h.orchestrator
        .daemon_launch_task_with_guard(&task.id, guard, false)
        .await
        .unwrap();
    assert_eq!(h.task(&task.id), task);
    assert_eq!(h.agent(&agent.id), agent);
    assert!(
        h.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
    assert!(h.persisted_state().retry_queue.is_empty());
    assert!(h.persisted_state().claimed.is_empty());
    assert!(h.adapter.execute_calls.calls().is_empty());
    assert!(work.is_idle());
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .is_empty()
    );
}

struct SealAtReservation(Arc<LaunchProcesses>);
#[async_trait]
impl orch_core::process::SpawnAdmission for SealAtReservation {
    async fn reserve(&self) -> Box<dyn orch_core::process::SpawnReservation> {
        let reservation = self.0.admission().reserve().await;
        self.0.seal();
        reservation
    }
}

#[tokio::test]
async fn sealed_tracker_refuses_final_admission_before_task_writes() {
    let work = Arc::new(LaunchProcesses::default());
    let task = make_task();
    let agent = make_agent();
    let h = Harness::builder()
        .tasks([task.clone()])
        .agents([agent.clone()])
        .build();
    h.orchestrator
        .set_spawn_admission(Arc::new(SealAtReservation(work.clone())));
    h.orchestrator.daemon_initialize().await.unwrap();
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert!(!h.orchestrator.is_shutting_down());
    assert_eq!(h.task(&task.id), task);
    assert_eq!(h.agent(&agent.id), agent);
    assert_eq!(h.runs.snapshot().len(), 1);
    assert_eq!(h.runs.snapshot()[0].status, RunStatus::Cancelled);
    assert!(h.persisted_state().retry_queue.is_empty());
    assert!(h.persisted_state().claimed.is_empty());
    assert!(h.adapter.execute_calls.calls().is_empty());
    assert!(work.is_idle());
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .is_empty()
    );
}
