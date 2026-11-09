#[allow(
    dead_code,
    reason = "the shared scripted rig also serves other integration suites"
)]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use orch_core::adapter::{
    Adapter, AdapterRegistry, AdapterTestResult, EffortPreparation, ExecuteHandle, ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::error::Result;
use orch_core::storage::{AgentStore, GoalStore, TaskStore};
use orch_core::test_support::{
    FakeAgentStore, FakeGoalStore, FakeMessageStore, FakeRunStore, FakeStateStore, FakeTaskStore,
    FakeTeamStore, FakeTemplateEngine, FakeWorkspaceManager, FixedClock, SeqIdGenerator,
    make_agent, make_task,
};
use orch_core::workspace::{MergeResult, PrepareResult, WorkspaceManager};
use orch_domain::{
    AgentStatus, Goal, GoalStatus, Message, MessageChannel, MessageStatus, RunStatus, TaskStatus,
    default_config,
};
use orch_engine::events::EventBus;
use orch_engine::goal_service::UpdateGoalFields;
use orch_engine::lock::InMemoryLock;
use orch_engine::orchestrator::{Orchestrator, OrchestratorDeps};
use orch_engine::{AgentService, GoalService, MessageService, RunService, TaskService};
use orch_infra::adapters::PiAdapter;
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    fail_preparation: AtomicBool,
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
        if self.gate.fail_preparation.load(Ordering::SeqCst) {
            return Err(orch_core::adapter::AdapterError::new(
                "scripted preparation failure",
                None,
            ));
        }
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
struct GatedWorkspace {
    inner: Arc<FakeWorkspaceManager>,
    merge_gate: Arc<Gate>,
}
#[async_trait]
impl WorkspaceManager for GatedWorkspace {
    async fn prepare(
        &self,
        task: &orch_domain::Task,
        agent: &orch_domain::Agent,
        config: &orch_domain::OrchestratorConfig,
    ) -> Result<PrepareResult> {
        self.inner.prepare(task, agent, config).await
    }
    async fn merge_back(&self, branch: &str) -> Result<MergeResult> {
        self.merge_gate.entered.notify_one();
        self.merge_gate.release.notified().await;
        self.inner.merge_back(branch).await
    }
    async fn cleanup(&self, task_id: &str, branch: Option<&str>) -> Result<()> {
        self.inner.cleanup(task_id, branch).await
    }
    fn validate(&self, path: &std::path::Path, root: &std::path::Path) -> Result<()> {
        self.inner.validate(path, root)
    }
    async fn get_changed_files(&self, branch: &str) -> Result<Vec<String>> {
        self.inner.get_changed_files(branch).await
    }
}
struct Rig {
    _project: tempfile::TempDir,
    orchestrator: Orchestrator,
    task_service: Arc<TaskService>,
    goal_service: GoalService,
    tasks: Arc<FakeTaskStore>,
    agents: Arc<FakeAgentStore>,
    messages: Arc<FakeMessageStore>,
    goals: Arc<FakeGoalStore>,
    runs: Arc<FakeRunStore>,
    state: Arc<FakeStateStore>,
    bus: Arc<EventBus>,
    processes: Arc<common::ScriptedProcessManager>,
    gate: Arc<Gate>,
    merge_gate: Arc<Gate>,
    workspace: Arc<FakeWorkspaceManager>,
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
        let messages = Arc::new(FakeMessageStore::new());
        let runs = Arc::new(FakeRunStore::new());
        let state = Arc::new(FakeStateStore::new());
        let bus = Arc::new(EventBus::new());
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new());
        let ids: Arc<dyn orch_core::IdGenerator> = Arc::new(SeqIdGenerator::new());
        let processes = common::ScriptedProcessManager::new(24601);
        let gate = Arc::new(Gate::default());
        let merge_gate = Arc::new(Gate::default());
        let workspace = Arc::new(FakeWorkspaceManager::new());
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
            ids.clone(),
            Some(agent_service.clone()),
            Some(task_service.clone()),
            None,
        );
        let message_service = Arc::new(MessageService::new(
            messages.clone(),
            agents.clone(),
            Arc::new(FakeTeamStore::new()),
            bus.clone(),
            clock.clone(),
            ids,
        ));
        let orchestrator = Orchestrator::new(OrchestratorDeps {
            task_store: tasks.clone(),
            agent_store: agents.clone(),
            run_store: runs.clone(),
            state_store: state.clone(),
            adapters: Arc::new(adapters),
            workspace: Arc::new(GatedWorkspace {
                inner: workspace.clone(),
                merge_gate: merge_gate.clone(),
            }),
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
            message_service: Some(message_service),
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
            agents,
            messages,
            goals,
            runs,
            state,
            bus,
            processes,
            gate,
            merge_gate,
            workspace,
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

/// `orch run <id>` starts a `retrying` task before its back-off is due, as
/// v0.13 did, and consumes the queued retry; a batch launch leaves it queued.
#[tokio::test]
async fn an_explicit_launch_overrides_the_retry_wait_and_a_batch_launch_honours_it() {
    let rig = Rig::new();
    rig.tasks.seed(orch_domain::Task {
        status: TaskStatus::Retrying,
        ..rig.tasks.peek("tsk_one").unwrap()
    });
    rig.state.seed(orch_domain::OrchestratorState {
        retry_queue: vec![orch_domain::RetryEntry {
            task_id: "tsk_one".into(),
            attempt: 1,
            due_at: "2099-01-01T00:00:00.000Z".into(),
            error: "scripted".into(),
        }],
        ..Default::default()
    });

    rig.orchestrator
        .daemon_launch_task_with_guard("tsk_one", Arc::new(AtomicBool::new(false)), false)
        .await
        .unwrap();
    assert!(rig.runs.snapshot().is_empty(), "a batch launch waited");
    assert_eq!(rig.state.peek().retry_queue.len(), 1);

    // A refused launch returns without preparing, so the gate never opens.
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let release = async {
        let entered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rig.gate.entered.notified(),
        )
        .await
        .is_ok();
        rig.gate.release.notify_one();
        entered
    };
    let (result, entered) = tokio::join!(launch, release);
    result.unwrap();
    assert!(entered, "the explicit launch did not start preparing");
    assert_eq!(rig.processes.spawns().len(), 1);
    assert_eq!(
        rig.tasks.peek("tsk_one").unwrap().status,
        TaskStatus::InProgress
    );
    assert!(
        rig.state.peek().retry_queue.is_empty(),
        "the retry was consumed"
    );
    rig.processes.close_stdout();
    rig.orchestrator.daemon_stop().await.unwrap();
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

#[tokio::test]
async fn authored_agent_edits_during_effort_preparation_veto_stale_launch() {
    for field in ["name", "adapter", "role", "autonomous", "capabilities"] {
        let rig = Rig::new();
        let launch = rig.orchestrator.daemon_launch_task("tsk_one");
        let edit = async {
            rig.gate.entered.notified().await;
            rig.bus
                .operation_boundary()
                .run(true, async {
                    let mut agent = rig.agents.peek("agt_one").unwrap();
                    match field {
                        "name" => agent.name = "new name".into(),
                        "adapter" => agent.adapter = "claude".into(),
                        "role" => agent.role = Some("new role".into()),
                        "autonomous" => agent.autonomous = Some(true),
                        "capabilities" => agent.capabilities = vec!["macos".into()],
                        _ => unreachable!(),
                    }
                    rig.agents.save(&agent).await.unwrap();
                })
                .await;
            rig.gate.release.notify_one();
        };
        let (result, ()) = tokio::join!(launch, edit);
        result.unwrap();
        assert!(
            rig.processes.spawns().is_empty(),
            "stale {field} snapshot launched"
        );
        assert_eq!(rig.tasks.peek("tsk_one").unwrap().status, TaskStatus::Todo);
    }
}

#[tokio::test]
async fn runtime_agent_stats_during_effort_preparation_do_not_veto_launch() {
    let rig = Rig::new();
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let stats = async {
        rig.gate.entered.notified().await;
        rig.bus
            .operation_boundary()
            .run(true, async {
                let mut agent = rig.agents.peek("agt_one").unwrap();
                agent.stats.total_runs += 1;
                rig.agents.save(&agent).await.unwrap();
            })
            .await;
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, stats);
    result.unwrap();
    assert_eq!(rig.processes.spawns().len(), 1);
    rig.processes.close_stdout();
    rig.orchestrator.daemon_stop().await.unwrap();
}

fn pending_message(id: &str) -> Message {
    Message {
        id: id.into(),
        channel: MessageChannel::Direct,
        from_agent_id: "cli".into(),
        to_agent_id: Some("agt_one".into()),
        subject: "launch message".into(),
        body: id.into(),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        expires_at: None,
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
        reply_to: None,
    }
}

#[tokio::test]
async fn paused_preparation_keeps_prompt_messages_pending() {
    let rig = Rig::new();
    rig.messages.seed(pending_message("msg_snapshot"));
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let pause = async {
        rig.gate.entered.notified().await;
        rig.goal_service
            .update_status("gol_one", GoalStatus::Paused, false)
            .await
            .unwrap();
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, pause);
    result.unwrap();
    assert!(rig.processes.spawns().is_empty());
    assert_eq!(
        rig.messages.peek("msg_snapshot").unwrap().status,
        MessageStatus::Pending
    );
}

#[tokio::test]
async fn successful_launch_marks_only_its_message_snapshot_once() {
    use orch_core::test_support::MessageStoreCall;
    let rig = Rig::new();
    rig.messages.seed(pending_message("msg_snapshot"));
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let release = async {
        rig.gate.entered.notified().await;
        assert_eq!(
            rig.messages.peek("msg_snapshot").unwrap().status,
            MessageStatus::Pending
        );
        rig.messages.seed(pending_message("msg_later"));
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, release);
    result.unwrap();
    assert_eq!(rig.processes.spawns().len(), 1);
    assert_eq!(
        rig.messages.peek("msg_snapshot").unwrap().status,
        MessageStatus::Delivered
    );
    assert_eq!(
        rig.messages.peek("msg_later").unwrap().status,
        MessageStatus::Pending
    );
    assert_eq!(
        rig.messages
            .calls
            .matching(
                |call| matches!(call, MessageStoreCall::MarkDelivered(id) if id == "msg_snapshot")
            )
            .len(),
        1
    );
    rig.processes.close_stdout();
    rig.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn failed_effort_preparation_keeps_prompt_messages_pending() {
    let rig = Rig::new();
    rig.messages.seed(pending_message("msg_snapshot"));
    rig.gate.fail_preparation.store(true, Ordering::SeqCst);
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let fail = async {
        rig.gate.entered.notified().await;
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, fail);
    assert!(result.is_err());
    assert!(rig.processes.spawns().is_empty());
    assert_eq!(
        rig.messages.peek("msg_snapshot").unwrap().status,
        MessageStatus::Pending
    );
    assert!(
        rig.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Failed)
    );
}

#[tokio::test]
async fn cancellation_while_live_completion_waits_for_merge_preserves_cancelled_state() {
    let rig = Rig::new();
    rig.workspace.set_prepare_result(PrepareResult {
        path: rig._project.path().display().to_string(),
        branch: Some("scripted-branch".into()),
    });
    let launch = rig.orchestrator.daemon_launch_task("tsk_one");
    let release = async {
        rig.gate.entered.notified().await;
        rig.gate.release.notify_one();
    };
    let (result, ()) = tokio::join!(launch, release);
    result.unwrap();
    assert_eq!(rig.processes.spawns().len(), 1);
    rig.processes.exit(0);
    rig.processes.close_stdout();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rig.merge_gate.entered.notified(),
    )
    .await
    .unwrap();
    rig.task_service.cancel("tsk_one").await.unwrap();
    rig.bus
        .operation_boundary()
        .run(true, async {
            let mut task = rig.tasks.peek("tsk_one").unwrap();
            task.title = "edited during completion".into();
            rig.tasks.save(&task).await.unwrap();
        })
        .await;
    rig.merge_gate.release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rig.orchestrator.wait_for_collectors(),
    )
    .await
    .unwrap();
    let task = rig.tasks.peek("tsk_one").unwrap();
    assert_eq!(task.status, TaskStatus::Cancelled);
    assert_eq!(task.title, "edited during completion");
    assert!(
        rig.runs
            .snapshot()
            .iter()
            .all(|run| run.status == RunStatus::Cancelled)
    );
    rig.orchestrator.daemon_stop().await.unwrap();
}
