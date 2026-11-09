#[allow(
    dead_code,
    reason = "the shared scripted rig also serves other integration suites"
)]
mod common;

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{
    Adapter, AdapterRegistry, AdapterTestResult, EffortPreparation, ExecuteHandle, ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::error::Result;
use orch_core::storage::GoalStore;
use orch_core::test_support::{
    FakeAgentStore, FakeGoalStore, FakeRunStore, FakeStateStore, FakeTaskStore, FakeTemplateEngine,
    FakeWorkspaceManager, FixedClock, SeqIdGenerator, make_agent, make_task,
};
use orch_domain::{AgentStatus, Goal, GoalStatus, RunStatus, TaskStatus, default_config};
use orch_engine::events::EventBus;
use orch_engine::goal_service::UpdateGoalFields;
use orch_engine::lock::InMemoryLock;
use orch_engine::orchestrator::{Orchestrator, OrchestratorDeps};
use orch_engine::{AgentService, GoalService, RunService, TaskService};
use orch_infra::adapters::PiAdapter;
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}
struct GatedAdapter {
    inner: PiAdapter,
    gate: Arc<Gate>,
}
#[async_trait]
impl Adapter for GatedAdapter {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
    async fn test(&self) -> AdapterTestResult {
        self.inner.test().await
    }
    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> std::result::Result<EffortPreparation, orch_core::adapter::AdapterError> {
        self.gate.entered.notify_one();
        self.gate.release.notified().await;
        self.inner.prepare_effort(params).await
    }
    fn execute(
        &self,
        params: ExecuteParams,
    ) -> std::result::Result<ExecuteHandle, orch_core::adapter::AdapterError> {
        self.inner.execute(params)
    }
    async fn stop(&self, pid: u32) -> Result<()> {
        self.inner.stop(pid).await
    }
}
struct Rig {
    _project: tempfile::TempDir,
    orchestrator: Orchestrator,
    task_service: Arc<TaskService>,
    goal_service: GoalService,
    tasks: Arc<FakeTaskStore>,
    goals: Arc<FakeGoalStore>,
    runs: Arc<FakeRunStore>,
    bus: Arc<EventBus>,
    processes: Arc<common::ScriptedProcessManager>,
    gate: Arc<Gate>,
}
impl Rig {
    fn new() -> Self {
        let project = tempfile::tempdir().unwrap();
        let config = default_config();
        let tasks = Arc::new(FakeTaskStore::with_tasks([orch_domain::Task {
            id: "tsk_one".into(),
            assignee: Some("agt_one".into()),
            goal_id: Some("gol_one".into()),
            ..make_task()
        }]));
        let agents = Arc::new(FakeAgentStore::with_agents([orch_domain::Agent {
            id: "agt_one".into(),
            adapter: "pi".into(),
            status: AgentStatus::Idle,
            ..make_agent()
        }]));
        let goals = Arc::new(FakeGoalStore::with_goals([Goal {
            id: "gol_one".into(),
            title: "old".into(),
            description: String::new(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: None,
        }]));
        let runs = Arc::new(FakeRunStore::new());
        let state = Arc::new(FakeStateStore::new());
        let bus = Arc::new(EventBus::new());
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new());
        let ids: Arc<dyn orch_core::IdGenerator> = Arc::new(SeqIdGenerator::new());
        let processes = common::ScriptedProcessManager::new(24601);
        let gate = Arc::new(Gate::default());
        let mut adapters = AdapterRegistry::new();
        adapters.register(Arc::new(GatedAdapter {
            inner: PiAdapter::new(processes.clone()),
            gate: gate.clone(),
        }));
        let task_service = Arc::new(TaskService::new(
            tasks.clone(),
            agents.clone(),
            bus.clone(),
            config.clone(),
            clock.clone(),
            ids.clone(),
            None,
        ));
        let agent_service = Arc::new(AgentService::new(
            agents.clone(),
            state.clone(),
            bus.clone(),
            config.clone(),
            ids.clone(),
        ));
        let run_service = Arc::new(RunService::new(
            runs.clone(),
            bus.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let goal_service = GoalService::new(
            goals.clone(),
            bus.clone(),
            clock.clone(),
            ids,
            Some(agent_service.clone()),
            Some(task_service.clone()),
            None,
        );
        let orchestrator = Orchestrator::new(OrchestratorDeps {
            task_store: tasks.clone(),
            agent_store: agents,
            run_store: runs.clone(),
            state_store: state,
            adapters: Arc::new(adapters),
            workspace: Arc::new(FakeWorkspaceManager::new()),
            templates: Arc::new(FakeTemplateEngine::new()),
            processes: processes.clone(),
            events: bus.clone(),
            task_service: task_service.clone(),
            agent_service,
            run_service,
            clock,
            lock: Arc::new(InMemoryLock::new()),
            command_runner: Arc::new(common::PassingCommandRunner),
            config,
            project_root: project.path().to_path_buf(),
            context_store: None,
            message_service: None,
            goal_store: Some(goals.clone()),
            skill_loader: None,
        });
        orchestrator.enable_daemon_mode();
        Self {
            _project: project,
            orchestrator,
            task_service,
            goal_service,
            tasks,
            goals,
            runs,
            bus,
            processes,
            gate,
        }
    }
}

#[tokio::test]
async fn goal_edit_cannot_resurrect_active_and_pause_vetoes_prepared_launch() {
    let rig = Rig::new();
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let pause = async {
        rig.gate.entered.notified().await;
        let boundary = rig.bus.operation_boundary();
        boundary
            .run(true, async {
                rig.goal_service
                    .update(
                        "gol_one",
                        &UpdateGoalFields {
                            title: Some("new".into()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                rig.goal_service
                    .update_status("gol_one", GoalStatus::Paused, false)
                    .await
                    .unwrap();
            })
            .await;
        // This decision completed while effort preparation still has no permit.
        assert_eq!(
            rig.goals.get("gol_one").await.unwrap().unwrap().status,
            GoalStatus::Paused
        );
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, pause);
    result.unwrap();
    assert!(rig.processes.spawns().is_empty());
    let goal = rig.goals.get("gol_one").await.unwrap().unwrap();
    assert_eq!(goal.title, "new");
    assert_eq!(goal.status, GoalStatus::Paused);
    assert_eq!(rig.tasks.peek("tsk_one").unwrap().status, TaskStatus::Todo);
    assert!(
        rig.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
}

#[tokio::test]
async fn concurrent_manual_ticks_never_launch_a_task_twice() {
    let rig = Rig::new();
    let first = rig.orchestrator.daemon_tick(true);
    let second = async {
        rig.gate.entered.notified().await;
        rig.orchestrator.daemon_tick(true).await.unwrap();
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(first, second);
    result.unwrap();
    assert_eq!(rig.processes.spawns().len(), 1);
    rig.processes.close_stdout();
    rig.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn cancellation_during_preparation_cannot_be_erased_by_completion() {
    let rig = Rig::new();
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let cancel = async {
        rig.gate.entered.notified().await;
        rig.task_service.cancel("tsk_one").await.unwrap();
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, cancel);
    result.unwrap();
    assert!(rig.processes.spawns().is_empty());
    assert_eq!(
        rig.tasks.peek("tsk_one").unwrap().status,
        TaskStatus::Cancelled
    );
    assert!(
        rig.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
}
