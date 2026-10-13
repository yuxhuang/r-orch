//! `buildDeps()` for Rust: a complete [`OrchestratorDeps`] with real services
//! and faked edges, plus the assertion helpers every suite needs.

use std::sync::{Arc, Mutex};

use orch_core::adapter::{AdapterRegistry, AgentEvent};
use orch_core::clock::Clock;
use orch_core::ids::IdGenerator;
use orch_core::storage::{
    AgentStore, ContextStore, GoalStore, MessageStore, RunStore, StateStore, TaskStore, TeamStore,
};
use orch_core::test_support::{
    FakeAdapter, FakeAgentStore, FakeContextStore, FakeGoalStore, FakeMessageStore,
    FakeProcessManager, FakeRunStore, FakeSkillLoader, FakeStateStore, FakeTaskStore, FakeTeamStore,
    FakeTemplateEngine, FakeWorkspaceManager, FixedClock, SeqIdGenerator,
};
use orch_domain::{
    Agent, Goal, OrchestratorConfig, OrchestratorEvent, OrchestratorEventType, OrchestratorState,
    RunningEntry, Task, TaskStatus, default_config,
};

use crate::agent_service::AgentService;
use crate::events::EventBus;
use crate::lock::InMemoryLock;
use crate::message_service::MessageService;
use crate::orchestrator::{Orchestrator, OrchestratorDeps, WatchOptions};
use crate::run_service::RunService;
use crate::task_service::TaskService;

/// A command runner that answers every review criterion from one scripted
/// outcome, so no unit test shells out to `npm` (PARITY.md V31).
pub(crate) struct ScriptedCommandRunner {
    outcome: Mutex<crate::review_runner::CommandOutcome>,
    calls: Mutex<Vec<(String, Vec<String>, std::path::PathBuf)>>,
}

impl ScriptedCommandRunner {
    fn new(outcome: crate::review_runner::CommandOutcome) -> Self {
        ScriptedCommandRunner {
            outcome: Mutex::new(outcome),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Make every criterion fail from now on.
    pub(crate) fn set_outcome(&self, outcome: crate::review_runner::CommandOutcome) {
        *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = outcome;
    }

    pub(crate) fn calls(&self) -> Vec<(String, Vec<String>, std::path::PathBuf)> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait::async_trait]
impl crate::review_runner::CommandRunner for ScriptedCommandRunner {
    async fn run(
        &self,
        command: &str,
        args: &[&str],
        cwd: &std::path::Path,
        _timeout: std::time::Duration,
    ) -> crate::review_runner::CommandOutcome {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((
                command.to_owned(),
                args.iter().map(|a| (*a).to_owned()).collect(),
                cwd.to_path_buf(),
            ));
        self.outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A passing criterion.
pub(crate) fn review_pass() -> crate::review_runner::CommandOutcome {
    crate::review_runner::CommandOutcome {
        failed: false,
        stdout: "ok".into(),
        stderr: String::new(),
    }
}

/// A failing criterion.
pub(crate) fn review_fail() -> crate::review_runner::CommandOutcome {
    crate::review_runner::CommandOutcome {
        failed: true,
        stdout: String::new(),
        stderr: "FAILED".into(),
    }
}

/// `poll_interval_ms` for a test config: long enough that the tick loop never
/// self-fires, so every tick in a test is one the test asked for.
pub(crate) const TEST_POLL_INTERVAL_MS: u64 = 100_000;

/// Everything a suite needs to drive and inspect one orchestrator.
pub(crate) struct Harness {
    pub orchestrator: Orchestrator,
    pub tasks: Arc<FakeTaskStore>,
    pub agents: Arc<FakeAgentStore>,
    pub runs: Arc<FakeRunStore>,
    pub state: Arc<FakeStateStore>,
    pub goals: Arc<FakeGoalStore>,
    pub context: Arc<FakeContextStore>,
    pub messages: Arc<FakeMessageStore>,
    pub processes: Arc<FakeProcessManager>,
    pub workspace: Arc<FakeWorkspaceManager>,
    pub templates: Arc<FakeTemplateEngine>,
    pub skills: Arc<FakeSkillLoader>,
    pub adapter: Arc<FakeAdapter>,
    pub lock: Arc<InMemoryLock>,
    pub commands: Arc<ScriptedCommandRunner>,
    pub clock: Arc<FixedClock>,
    pub bus: Arc<EventBus>,
    pub config: OrchestratorConfig,
    recorded: Arc<Mutex<Vec<OrchestratorEvent>>>,
    /// Keeps the wildcard subscription attached for the harness's lifetime.
    _recorder: Option<crate::events::Unsubscribe>,
}

/// Builder, standing in for the TS `buildDeps(overrides)` spread.
pub(crate) struct HarnessBuilder {
    tasks: Vec<Task>,
    agents: Vec<Agent>,
    goals: Vec<Goal>,
    state: OrchestratorState,
    config: OrchestratorConfig,
    adapter: Arc<FakeAdapter>,
    register_adapter: bool,
    with_goal_store: bool,
    with_skills: bool,
    with_messages: bool,
    with_context: bool,
    skills: Arc<FakeSkillLoader>,
    clock: Arc<FixedClock>,
    review_outcome: crate::review_runner::CommandOutcome,
    #[allow(clippy::type_complexity)]
    wrap_tasks: Option<Box<dyn FnOnce(Arc<FakeTaskStore>) -> Arc<dyn TaskStore>>>,
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        HarnessBuilder::new()
    }
}

impl HarnessBuilder {
    pub(crate) fn new() -> Self {
        HarnessBuilder {
            tasks: Vec::new(),
            agents: Vec::new(),
            goals: Vec::new(),
            state: OrchestratorState::default(),
            config: test_config(),
            adapter: Arc::new(FakeAdapter::new()),
            register_adapter: true,
            with_goal_store: false,
            with_skills: false,
            with_messages: false,
            with_context: true,
            skills: Arc::new(FakeSkillLoader::new()),
            clock: Arc::new(FixedClock::new()),
            review_outcome: review_pass(),
            wrap_tasks: None,
        }
    }

    /// Put a decorator in front of the task store — the Rust equivalent of
    /// re-mocking `taskStore.get`. The harness's own fake is still what
    /// `tasks`, `task()` and `saved_statuses()` observe.
    pub(crate) fn wrap_task_store(
        mut self,
        wrap: impl FnOnce(Arc<FakeTaskStore>) -> Arc<dyn TaskStore> + 'static,
    ) -> Self {
        self.wrap_tasks = Some(Box::new(wrap));
        self
    }

    /// What every auto-review criterion reports.
    pub(crate) fn review_outcome(mut self, outcome: crate::review_runner::CommandOutcome) -> Self {
        self.review_outcome = outcome;
        self
    }

    pub(crate) fn tasks(mut self, tasks: impl IntoIterator<Item = Task>) -> Self {
        self.tasks = tasks.into_iter().collect();
        self
    }

    pub(crate) fn agents(mut self, agents: impl IntoIterator<Item = Agent>) -> Self {
        self.agents = agents.into_iter().collect();
        self
    }

    pub(crate) fn goals(mut self, goals: impl IntoIterator<Item = Goal>) -> Self {
        self.goals = goals.into_iter().collect();
        self.with_goal_store = true;
        self
    }

    pub(crate) fn state(mut self, state: OrchestratorState) -> Self {
        self.state = state;
        self
    }

    pub(crate) fn config(mut self, config: OrchestratorConfig) -> Self {
        self.config = config;
        self
    }

    /// Replay these events from the single registered adapter.
    pub(crate) fn adapter_events(self, events: impl IntoIterator<Item = AgentEvent>) -> Self {
        self.adapter.set_events(events);
        self
    }

    pub(crate) fn adapter(mut self, adapter: Arc<FakeAdapter>) -> Self {
        self.adapter = adapter;
        self
    }

    /// Leave the registry empty, so `require()` fails — the TS default, since
    /// `buildDeps` hands over a bare `new AdapterRegistry()`.
    pub(crate) fn without_adapter(mut self) -> Self {
        self.register_adapter = false;
        self
    }

    pub(crate) fn with_goal_store(mut self) -> Self {
        self.with_goal_store = true;
        self
    }

    pub(crate) fn with_messages(mut self) -> Self {
        self.with_messages = true;
        self
    }

    pub(crate) fn without_context(mut self) -> Self {
        self.with_context = false;
        self
    }

    pub(crate) fn skills<K, V>(mut self, library: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.skills = Arc::new(FakeSkillLoader::with_skills(library));
        self.with_skills = true;
        self
    }

    /// Wire a skill loader even though its library is empty, so
    /// `load_skills` is still called.
    pub(crate) fn with_skill_loader(mut self) -> Self {
        self.with_skills = true;
        self
    }

    pub(crate) fn build(self) -> Harness {
        let tasks = Arc::new(FakeTaskStore::with_tasks(self.tasks));
        let agents = Arc::new(FakeAgentStore::with_agents(self.agents));
        let runs = Arc::new(FakeRunStore::new());
        let state = Arc::new(FakeStateStore::with_state(self.state));
        let goals = Arc::new(FakeGoalStore::with_goals(self.goals));
        let context = Arc::new(FakeContextStore::new());
        let messages = Arc::new(FakeMessageStore::new());
        let teams = Arc::new(FakeTeamStore::new());
        let processes = Arc::new(FakeProcessManager::new());
        let workspace = Arc::new(FakeWorkspaceManager::new());
        let templates = Arc::new(FakeTemplateEngine::new());
        let lock = Arc::new(InMemoryLock::new());
        let commands = Arc::new(ScriptedCommandRunner::new(self.review_outcome));
        let bus = Arc::new(EventBus::new());
        // Every suite attaches handlers; the default of 10 would warn.
        bus.set_max_listeners(0);
        let clock = self.clock;
        let ids: Arc<dyn IdGenerator> = Arc::new(SeqIdGenerator::new());

        let task_store: Arc<dyn TaskStore> = match self.wrap_tasks {
            Some(wrap) => wrap(Arc::clone(&tasks)),
            None => Arc::clone(&tasks) as Arc<dyn TaskStore>,
        };

        let mut registry = AdapterRegistry::new();
        if self.register_adapter {
            registry.register(Arc::clone(&self.adapter) as Arc<dyn orch_core::adapter::Adapter>);
        }

        let task_service = Arc::new(TaskService::new(
            Arc::clone(&task_store),
            Arc::clone(&agents) as Arc<dyn AgentStore>,
            Arc::clone(&bus),
            self.config.clone(),
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::clone(&ids),
            None,
        ));
        let agent_service = Arc::new(AgentService::new(
            Arc::clone(&agents) as Arc<dyn AgentStore>,
            Arc::clone(&state) as Arc<dyn StateStore>,
            Arc::clone(&bus),
            self.config.clone(),
            Arc::clone(&ids),
        ));
        let run_service = Arc::new(RunService::new(
            Arc::clone(&runs) as Arc<dyn RunStore>,
            Arc::clone(&bus),
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::clone(&ids),
        ));
        let message_service = Arc::new(MessageService::new(
            Arc::clone(&messages) as Arc<dyn MessageStore>,
            Arc::clone(&agents) as Arc<dyn AgentStore>,
            teams as Arc<dyn TeamStore>,
            Arc::clone(&bus),
            Arc::clone(&clock) as Arc<dyn Clock>,
            Arc::clone(&ids),
        ));

        let recorded: Arc<Mutex<Vec<OrchestratorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let recorder = bus.on_any(move |event| {
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(event.clone())
        });

        let deps = OrchestratorDeps {
            task_store: Arc::clone(&task_store),
            agent_store: Arc::clone(&agents) as Arc<dyn AgentStore>,
            run_store: Arc::clone(&runs) as Arc<dyn RunStore>,
            state_store: Arc::clone(&state) as Arc<dyn StateStore>,
            adapters: Arc::new(registry),
            workspace: Arc::clone(&workspace) as Arc<dyn orch_core::workspace::WorkspaceManager>,
            templates: Arc::clone(&templates) as Arc<dyn orch_core::template::TemplateEngine>,
            processes: Arc::clone(&processes) as Arc<dyn orch_core::process::ProcessManager>,
            events: Arc::clone(&bus),
            task_service,
            agent_service,
            run_service,
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            lock: Arc::clone(&lock) as Arc<dyn crate::lock::ProjectLock>,
            command_runner: Arc::clone(&commands)
                as Arc<dyn crate::review_runner::CommandRunner>,
            config: self.config.clone(),
            project_root: std::path::PathBuf::from("/tmp/project"),
            context_store: self
                .with_context
                .then(|| Arc::clone(&context) as Arc<dyn ContextStore>),
            message_service: self.with_messages.then_some(message_service),
            goal_store: self
                .with_goal_store
                .then(|| Arc::clone(&goals) as Arc<dyn GoalStore>),
            skill_loader: self
                .with_skills
                .then(|| Arc::clone(&self.skills) as Arc<dyn orch_core::skills::SkillLoader>),
        };

        Harness {
            orchestrator: Orchestrator::new(deps),
            tasks,
            agents,
            runs,
            state,
            goals,
            context,
            messages,
            processes,
            workspace,
            templates,
            skills: self.skills,
            adapter: self.adapter,
            lock,
            commands,
            clock,
            bus,
            config: self.config,
            recorded,
            _recorder: Some(recorder),
        }
    }
}

impl Harness {
    pub(crate) fn builder() -> HarnessBuilder {
        HarnessBuilder::new()
    }

    /// Start watch mode with signal handling off — a real `SIGINT` would hit
    /// the test process, not a child.
    pub(crate) async fn start_watch(&self) -> orch_core::error::Result<()> {
        self.orchestrator
            .start_watch(WatchOptions::default())
            .await
    }

    pub(crate) async fn start_watch_seeding(&self) -> orch_core::error::Result<()> {
        self.orchestrator
            .start_watch(WatchOptions {
                skip_autonomous_seeding: false,
                handle_signals: false,
            })
            .await
    }

    /// One tick, without the watch loop. The common case: the suites drive
    /// ticks explicitly.
    pub(crate) async fn tick(&self) -> orch_core::error::Result<()> {
        self.orchestrator.tick_once().await
    }

    /// Reconcile only, with no dispatch after it.
    ///
    /// A whole tick would re-dispatch a task reconcile has just moved to
    /// `retrying`, which is correct but hides what the reconcile tests are
    /// actually asserting.
    pub(crate) async fn reconcile(&self) -> orch_core::error::Result<()> {
        let mut slot = self.orchestrator.inner.state.lock().await;
        self.orchestrator.load_state(&mut slot).await?;
        let state = slot.as_mut().expect("state loaded");
        self.orchestrator.reconcile(state).await
    }

    /// Pretend the lock is held, as the TS tests do with
    /// `(orch as any).lockAcquired = true`.
    pub(crate) fn own_lock(&self) {
        self.orchestrator
            .inner
            .lock_acquired
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Load state into the orchestrator without running a tick — the TS
    /// `await (orch as any).loadState()`.
    pub(crate) async fn load_state(&self) -> orch_core::error::Result<()> {
        let mut slot = self.orchestrator.inner.state.lock().await;
        self.orchestrator.load_state(&mut slot).await
    }

    /// Install an in-memory state directly, bypassing the store.
    pub(crate) async fn set_state(&self, state: OrchestratorState) {
        *self.orchestrator.inner.state.lock().await = Some(state);
    }

    /// The orchestrator's current in-memory state.
    pub(crate) async fn in_memory_state(&self) -> OrchestratorState {
        self.orchestrator
            .inner
            .state
            .lock()
            .await
            .clone()
            .expect("state loaded")
    }

    /// The last state written to the store.
    pub(crate) fn persisted_state(&self) -> OrchestratorState {
        self.state.peek()
    }

    pub(crate) fn task(&self, id: &str) -> Task {
        self.tasks.peek(id).unwrap_or_else(|| panic!("no task {id}"))
    }

    pub(crate) fn agent(&self, id: &str) -> Agent {
        self.agents
            .peek(id)
            .unwrap_or_else(|| panic!("no agent {id}"))
    }

    pub(crate) fn events(&self) -> Vec<OrchestratorEvent> {
        self.recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn events_of(&self, event_type: OrchestratorEventType) -> Vec<OrchestratorEvent> {
        self.events()
            .into_iter()
            .filter(|event| event.event_type() == event_type)
            .collect()
    }

    pub(crate) fn clear_events(&self) {
        self.recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Statuses a task was saved with, in order. The replacement for
    /// `taskStore.save.mock.calls.find(...)`.
    pub(crate) fn saved_statuses(&self, task_id: &str) -> Vec<TaskStatus> {
        self.tasks
            .saved()
            .into_iter()
            .filter(|task| task.id == task_id)
            .map(|task| task.status)
            .collect()
    }

    pub(crate) fn was_dispatched(&self, task_id: &str) -> bool {
        self.saved_statuses(task_id)
            .contains(&TaskStatus::InProgress)
    }

    /// Wait until `predicate` holds, polling the way the TS `waitFor` helper
    /// does. Bounded so a regression fails rather than hangs.
    pub(crate) async fn wait_for<F>(&self, mut predicate: F)
    where
        F: FnMut(&Harness) -> bool,
    {
        for _ in 0..2000 {
            if predicate(self) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("wait_for timed out");
    }

    /// Let every detached collector task finish. Collection runs on its own
    /// task, so a test that asserts on completion has to yield to it.
    pub(crate) async fn settle(&self) {
        for _ in 0..200 {
            if self
                .orchestrator
                .inner
                .active_collectors
                .lock()
                .await
                .is_empty()
            {
                // One more yield so the success handler, which runs after the
                // collector deregisters, has a turn.
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("collectors did not settle");
    }
}

/// `DEFAULT_CONFIG` with a poll interval that never fires on its own.
pub(crate) fn test_config() -> OrchestratorConfig {
    let mut config = default_config();
    config.scheduling.poll_interval_ms = TEST_POLL_INTERVAL_MS;
    config
}

pub(crate) fn config_with_concurrency(max: usize) -> OrchestratorConfig {
    let mut config = test_config();
    config.scheduling.max_concurrent_agents = max;
    config
}

/// A running entry for `task_id`, as the TS tests hand-build in
/// `createMockStateStore({ running: { … } })`.
pub(crate) fn running_entry(
    task_id: &str,
    agent_id: &str,
    run_id: &str,
    pid: u32,
    at: &str,
) -> RunningEntry {
    RunningEntry {
        run_id: run_id.into(),
        agent_id: agent_id.into(),
        task_id: task_id.into(),
        pid,
        started_at: at.into(),
        last_event_at: at.into(),
    }
}

/// A state whose single running entry is `task_id`.
pub(crate) fn state_running(
    task_id: &str,
    agent_id: &str,
    run_id: &str,
    pid: u32,
    at: &str,
) -> OrchestratorState {
    OrchestratorState {
        running: [(
            task_id.to_owned(),
            running_entry(task_id, agent_id, run_id, pid, at),
        )]
        .into_iter()
        .collect(),
        ..OrchestratorState::default()
    }
}

/// The ISO instant [`FixedClock`] starts at, for seeding `started_at`.
pub(crate) const NOW_ISO: &str = orch_core::test_support::FIXED_NOW_ISO;

/// `now - delta_ms`, for aging a run without sleeping.
pub(crate) fn iso_before(delta_ms: i64) -> String {
    orch_core::clock::iso_from_epoch_ms(orch_core::test_support::FIXED_NOW_MS - delta_ms)
}
