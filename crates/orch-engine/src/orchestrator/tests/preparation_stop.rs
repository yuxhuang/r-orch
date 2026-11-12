//! A preparation spawn refused by stop is cancelled: a pre-spawn process started
//! during preparation (outside the launch boundary) is refused by the stop's
//! seal. Does the launch still surface an OrchestratorError?
use super::harness::{Harness, LaunchProcesses};
use async_trait::async_trait;
use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::process::{ProcessManager, SpawnOptions};
use orch_core::test_support::{make_agent, make_task};
use orch_domain::{OrchestratorEventType, RunStatus, TaskStatus};
use std::sync::Arc;
use tokio::sync::Notify;

struct ProbingAdapter {
    inner: Arc<dyn Adapter>,
    processes: Arc<dyn ProcessManager>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    seal_in_probe: Option<Arc<LaunchProcesses>>,
}

#[async_trait]
impl Adapter for ProbingAdapter {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
    async fn test(&self) -> AdapterTestResult {
        self.inner.test().await
    }
    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> Result<orch_core::EffortPreparation, AdapterError> {
        self.entered.notify_one();
        self.release.notified().await;
        if let Some(work) = &self.seal_in_probe {
            work.seal();
        }
        // Like a version probe or a workspace git command: a real spawn
        // through the shared, possibly sealed, process manager.
        match self
            .processes
            .spawn("/usr/bin/true", &[], SpawnOptions::default())
        {
            Ok(_) => Ok(orch_core::EffortPreparation {
                config: params.config.clone(),
                diagnostics: Vec::new(),
            }),
            Err(error) => Err(AdapterError::new(
                format!("probe could not spawn: {error}"),
                None,
            )),
        }
    }
    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        self.inner.execute(params)
    }
    async fn stop(&self, pid: u32) -> orch_core::error::Result<()> {
        self.inner.stop(pid).await
    }
}

async fn preparation_refusal(seal_only: bool) {
    let root = tempfile::tempdir().unwrap();
    let work = Arc::new(LaunchProcesses::default());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (inner, processes) = work.shell_adapter_and_processes(root.path());
    let adapter = Arc::new(ProbingAdapter {
        inner,
        processes,
        entered: entered.clone(),
        release: release.clone(),
        seal_in_probe: seal_only.then(|| work.clone()),
    });
    let mut agent = make_agent();
    agent.config.command = Some("exec /bin/sleep 120".into());
    let mut task = make_task();
    task.max_attempts = if seal_only { 3 } else { 1 };
    let mut state = orch_domain::OrchestratorState::default();
    if seal_only {
        task.status = TaskStatus::Retrying;
        task.attempts = 1;
        state.retry_queue.push(orch_domain::RetryEntry {
            task_id: task.id.clone(),
            attempt: 1,
            due_at: "2000-01-01T00:00:00Z".into(),
            error: "previous failure".into(),
        });
    }
    let queue = state.retry_queue.clone();
    let mut dependent = make_task();
    dependent.id = "tsk_dependent".into();
    dependent.depends_on = vec![task.id.clone()];
    task.assignee = Some(agent.id.clone());
    let h = Harness::builder()
        .tasks([task.clone(), dependent.clone()])
        .state(state)
        .agents([agent.clone()])
        .adapter_impl(adapter)
        .build();
    h.workspace
        .set_prepare_result(orch_core::workspace::PrepareResult {
            path: root.path().to_str().unwrap().into(),
            branch: None,
        });
    h.orchestrator.set_spawn_admission(work.admission());
    h.orchestrator.daemon_initialize().await.unwrap();

    let launch = h.orchestrator.daemon_tick(true);
    tokio::pin!(launch);
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch did not reach preparation: {result:?}"),
        () = entered.notified() => (),
    }
    if seal_only {
        release.notify_one();
        launch.await.unwrap();
        assert!(!h.orchestrator.is_shutting_down());
    } else {
        // Adopted review probe: park the real stop after seal but before it
        // can cancel preparation tokens. The refusal beats the next 25 ms poll.
        let state_guard = h.orchestrator.inner.state.lock().await;
        let stop = h.orchestrator.daemon_stop_with_spawn_seal(|| work.seal());
        tokio::pin!(stop);
        assert!(futures::poll!(&mut stop).is_pending());
        assert!(h.orchestrator.is_shutting_down());
        release.notify_one();
        assert!(futures::poll!(&mut launch).is_pending());
        drop(state_guard);
        let (launch, stop) = tokio::join!(launch, stop);
        launch.unwrap();
        stop.unwrap();
    }

    let errors = h.events_of(OrchestratorEventType::OrchestratorError);
    let current = h.task(&task.id);
    let current_agent = h.agent(&agent.id);
    eprintln!(
        "PROBE task={:?} attempts={} errors={errors:?}",
        current.status, current.attempts
    );
    eprintln!(
        "PROBE runs={:?}",
        h.runs
            .snapshot()
            .iter()
            .map(|r| (r.status, r.error.clone()))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "PROBE agent stats failed={} total={} last_error={:?}",
        current_agent.stats.tasks_failed, current_agent.stats.total_runs, current_agent.last_error
    );
    // Cancellation preserves task, agent, retry and dependent records.
    assert_eq!(current.attempts, task.attempts);
    assert_eq!(current.status, task.status);
    assert!(
        h.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
    assert_eq!(current, task);
    assert_eq!(current_agent, agent);
    assert_eq!(current_agent.stats.tasks_failed, agent.stats.tasks_failed);
    assert_eq!(h.persisted_state().retry_queue, queue);
    assert_eq!(h.task(&dependent.id), dependent);
    assert!(h.persisted_state().claimed.is_empty());
    assert!(work.is_idle());
    // A refusal by stop is not an orchestrator error.
    assert!(
        errors.is_empty(),
        "a clean stop produced an orchestrator error: {errors:?}"
    );
}

#[tokio::test]
async fn preparation_spawn_refused_during_stop_is_cancelled() {
    preparation_refusal(false).await;
}

#[tokio::test]
async fn preparation_spawn_refused_before_shutdown_flag_is_cancelled() {
    preparation_refusal(true).await;
}

struct HeldWorkspace {
    inner: Arc<dyn orch_core::workspace::WorkspaceManager>,
    entered: Notify,
    release: Notify,
    work: Arc<LaunchProcesses>,
}
#[async_trait]
impl orch_core::workspace::WorkspaceManager for HeldWorkspace {
    async fn prepare(
        &self,
        task: &orch_domain::Task,
        agent: &orch_domain::Agent,
        config: &orch_domain::OrchestratorConfig,
    ) -> orch_core::Result<orch_core::workspace::PrepareResult> {
        self.entered.notify_one();
        self.release.notified().await;
        // A production git prepare call observes the seal before the engine's
        // shutdown flag or cancellation token changes.
        self.work.seal();
        self.inner.prepare(task, agent, config).await
    }
    async fn merge_back(
        &self,
        branch: &str,
    ) -> orch_core::Result<orch_core::workspace::MergeResult> {
        self.inner.merge_back(branch).await
    }
    async fn cleanup(&self, task: &str, branch: Option<&str>) -> orch_core::Result<()> {
        self.inner.cleanup(task, branch).await
    }
    fn validate(
        &self,
        workspace: &std::path::Path,
        root: &std::path::Path,
    ) -> orch_core::Result<()> {
        self.inner.validate(workspace, root)
    }
    async fn get_changed_files(&self, branch: &str) -> orch_core::Result<Vec<String>> {
        self.inner.get_changed_files(branch).await
    }
}

#[tokio::test]
async fn workspace_spawn_refused_before_shutdown_flag_is_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let work = Arc::new(LaunchProcesses::default());
    let workspace = Arc::new(HeldWorkspace {
        inner: work.workspace(root.path()),
        entered: Notify::new(),
        release: Notify::new(),
        work: work.clone(),
    });
    let mut agent = make_agent();
    agent.config.workspace_mode = Some(orch_domain::WorkspaceMode::Worktree);
    let mut task = make_task();
    task.assignee = Some(agent.id.clone());
    task.max_attempts = 1;
    let mut dependent = make_task();
    dependent.id = "tsk_dependent".into();
    dependent.depends_on = vec![task.id.clone()];
    let h = Harness::builder()
        .tasks([task.clone(), dependent.clone()])
        .agents([agent.clone()])
        .workspace_impl(workspace.clone())
        .build();
    h.orchestrator.set_spawn_admission(work.admission());
    h.orchestrator.daemon_initialize().await.unwrap();
    let launch = h.orchestrator.daemon_tick(true);
    tokio::pin!(launch);
    tokio::select! {
        biased;
        result = &mut launch => panic!("workspace missed barrier: {result:?}"),
        () = workspace.entered.notified() => (),
    }
    workspace.release.notify_one();
    launch.await.unwrap();
    assert!(!h.orchestrator.is_shutting_down());
    assert_eq!(h.task(&task.id), task);
    assert_eq!(h.task(&dependent.id), dependent);
    assert_eq!(h.agent(&agent.id), agent);
    assert!(h.runs.snapshot().is_empty());
    assert!(h.persisted_state().retry_queue.is_empty());
    assert!(h.persisted_state().claimed.is_empty());
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .is_empty()
    );
    assert!(work.is_idle());
}
