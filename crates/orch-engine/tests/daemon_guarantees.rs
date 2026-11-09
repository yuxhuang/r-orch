//! Engine guarantees the daemon-owned-state design depends on (goal
//! `goal_zX0WXev`, must-row 5). Written failing-first: each guarantee below is
//! asserted against the engine **as it is today** and fails until the engine
//! work (task B0) lands.
//!
//! | # | Guarantee | Fails today because |
//! |---|-----------|---------------------|
//! | a | Pausing a goal is atomic with respect to dispatch: no task of the goal starts after the pause decision | pause does not serialise with the tick, so a dispatch in flight spawns after the pause event |
//! | b | `TaskService::cancel` stops a live agent process | cancel is a store transition only; the kill waits for the next reconcile tick |
//! | c | Context expiry is an explicit step; `get`/`list` never touch the disk | `FileContextStore::get` deletes and `list` purges + rewrites the index |
//!
//! Everything is deterministic: in-memory stores, a scripted process manager,
//! and a current-thread runtime where "is this future blocked?" is answered by
//! polling it a bounded number of times — never by sleeping.
//!
//! Explicit expiry uses the inherent `FileContextStore::expire_now` API;
//! the existing core store trait remains unchanged.

#[allow(
    dead_code,
    reason = "shared rig: this file uses only the scripted process manager and command runner"
)]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{PassingCommandRunner, ScriptedProcessManager};
use orch_core::adapter::{
    Adapter, AdapterRegistry, AdapterTestResult, EffortPreparation, ExecuteHandle, ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::error::Result;
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_core::storage::{AgentStore, GoalStore, RunStore, StateStore, TaskStore};
use orch_core::test_support::{
    FakeAgentStore, FakeGoalStore, FakeRunStore, FakeStateStore, FakeTaskStore, FakeTemplateEngine,
    FakeWorkspaceManager, FixedClock, SeqIdGenerator, make_agent, make_task,
};
use orch_core::workspace::{MergeResult, PrepareResult, WorkspaceManager};
use orch_domain::{
    Agent, AgentStatus, Goal, GoalStatus, OrchestratorConfig, OrchestratorEvent, RunStatus, Task,
    TaskStatus, default_config,
};
use orch_engine::events::EventBus;
use orch_engine::lock::InMemoryLock;
use orch_engine::orchestrator::{Orchestrator, OrchestratorDeps};
use orch_engine::{AgentService, GoalService, RunService, TaskService};
use orch_infra::adapters::PiAdapter;
use tokio::sync::Notify;

// ── Harness ──────────────────────────────────────────────────────────────

/// What happened, in the order it happened. One log shared by the process
/// manager (spawns, kills) and the event bus (pause decisions), so "after"
/// means "after" and not "in a different Vec".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mark {
    Spawn,
    Kill(u32),
    PauseDecided(String),
}

type Log = Arc<Mutex<Vec<Mark>>>;

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Records spawns and kills into the shared [`Log`], then delegates.
struct LoggingProcesses {
    inner: Arc<ScriptedProcessManager>,
    log: Log,
}

#[async_trait]
impl ProcessManager for LoggingProcesses {
    fn is_alive(&self, pid: u32) -> bool {
        self.inner.is_alive(pid)
    }

    fn kill(&self, pid: u32, signal: Signal) {
        lock(&self.log).push(Mark::Kill(pid));
        self.inner.kill(pid, signal);
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        lock(&self.log).push(Mark::Kill(pid));
        self.inner.kill_with_grace(pid, grace).await;
    }

    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild> {
        let child = self.inner.spawn(command, args, opts)?;
        lock(&self.log).push(Mark::Spawn);
        Ok(child)
    }
}

/// A one-shot rendezvous that parks the engine at a chosen point so a test can
/// act "while a dispatch tick is in flight" without sleeping.
#[derive(Default)]
struct Gate {
    armed: std::sync::atomic::AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Gate {
    fn armed() -> Arc<Gate> {
        let gate = Gate::default();
        gate.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        Arc::new(gate)
    }

    /// Called by the engine seam: parks until [`Gate::open`] when armed.
    async fn pass(&self) {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    /// Test side: resolves once the engine is parked at the gate.
    async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    fn open(&self) {
        self.release.notify_one();
    }
}

/// The real Pi adapter, except `prepare_effort` — which the orchestrator awaits
/// after the task is `in_progress` and the run exists but before any child
/// is spawned — parks on a [`Gate`].
struct GatedAdapter {
    inner: PiAdapter,
    gate: Option<Arc<Gate>>,
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
        if let Some(gate) = &self.gate {
            gate.pass().await;
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

/// The fake workspace manager, except `prepare` — which the orchestrator awaits
/// while the task is still `todo` — parks on a [`Gate`].
struct GatedWorkspace {
    inner: FakeWorkspaceManager,
    gate: Option<Arc<Gate>>,
}

#[async_trait]
impl WorkspaceManager for GatedWorkspace {
    async fn prepare(
        &self,
        task: &Task,
        agent: &Agent,
        config: &OrchestratorConfig,
    ) -> Result<PrepareResult> {
        if let Some(gate) = &self.gate {
            gate.pass().await;
        }
        self.inner.prepare(task, agent, config).await
    }

    async fn merge_back(&self, branch: &str) -> Result<MergeResult> {
        self.inner.merge_back(branch).await
    }

    async fn cleanup(&self, task_id: &str, branch: Option<&str>) -> Result<()> {
        self.inner.cleanup(task_id, branch).await
    }

    fn validate(
        &self,
        workspace_path: &std::path::Path,
        project_root: &std::path::Path,
    ) -> Result<()> {
        self.inner.validate(workspace_path, project_root)
    }

    async fn get_changed_files(&self, branch: &str) -> Result<Vec<String>> {
        self.inner.get_changed_files(branch).await
    }
}

/// Where the engine parks for the pause tests.
#[derive(Clone, Copy)]
enum GateAt {
    /// Workspace preparation: the task is still `todo` in the store.
    WorkspacePrepare,
    /// Effort preparation: the task is `in_progress`, the run exists, the child
    /// is not yet spawned.
    BeforeSpawn,
}

struct Harness {
    orchestrator: Arc<Orchestrator>,
    tasks: Arc<FakeTaskStore>,
    agents: Arc<FakeAgentStore>,
    runs: Arc<FakeRunStore>,
    state: Arc<FakeStateStore>,
    task_service: Arc<TaskService>,
    goal_service: Arc<GoalService>,
    processes: Arc<ScriptedProcessManager>,
    log: Log,
    _recorder: orch_engine::events::Unsubscribe,
}

const GOAL: &str = "gol_pause";
const WORKER: &str = "agt_worker";

fn agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
        name: id.into(),
        adapter: "pi".into(),
        status: AgentStatus::Idle,
        ..make_agent()
    }
}

fn task(id: &str) -> Task {
    Task {
        id: id.into(),
        title: format!("title {id}"),
        status: TaskStatus::Todo,
        ..make_task()
    }
}

fn goal(status: GoalStatus) -> Goal {
    Goal {
        id: GOAL.into(),
        title: "keep the lights on".into(),
        description: String::new(),
        status,
        assignee: Some(WORKER.into()),
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: None,
    }
}

impl Harness {
    fn new(tasks: Vec<Task>, gate: Option<(GateAt, Arc<Gate>)>) -> Harness {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let scripted = ScriptedProcessManager::new(24601);
        let processes: Arc<dyn ProcessManager> = Arc::new(LoggingProcesses {
            inner: Arc::clone(&scripted),
            log: Arc::clone(&log),
        });

        let mut config = default_config();
        config.scheduling.poll_interval_ms = 100_000;

        let task_store = Arc::new(FakeTaskStore::with_tasks(tasks));
        let agent_store = Arc::new(FakeAgentStore::with_agents(vec![agent(WORKER)]));
        let run_store = Arc::new(FakeRunStore::new());
        let state_store = Arc::new(FakeStateStore::new());
        let goal_store = Arc::new(FakeGoalStore::with_goals(vec![goal(GoalStatus::Active)]));
        let bus = Arc::new(EventBus::new());
        bus.set_max_listeners(0);
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new());
        let ids = Arc::new(SeqIdGenerator::new());

        let (adapter_gate, workspace_gate) = match gate {
            Some((GateAt::BeforeSpawn, gate)) => (Some(gate), None),
            Some((GateAt::WorkspacePrepare, gate)) => (None, Some(gate)),
            None => (None, None),
        };
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(GatedAdapter {
            inner: PiAdapter::new(Arc::clone(&processes)),
            gate: adapter_gate,
        }));

        let log_for_events = Arc::clone(&log);
        let recorder = bus.on_any(move |event| {
            if let OrchestratorEvent::GoalStatusChanged {
                goal_id,
                to: GoalStatus::Paused,
                ..
            } = event
            {
                lock(&log_for_events).push(Mark::PauseDecided(goal_id.clone()));
            }
        });

        let task_service = Arc::new(TaskService::new(
            Arc::clone(&task_store) as Arc<dyn TaskStore>,
            Arc::clone(&agent_store) as Arc<dyn AgentStore>,
            Arc::clone(&bus),
            config.clone(),
            Arc::clone(&clock),
            Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
            None,
        ));
        let agent_service = Arc::new(AgentService::new(
            Arc::clone(&agent_store) as Arc<dyn AgentStore>,
            Arc::clone(&state_store) as Arc<dyn StateStore>,
            Arc::clone(&bus),
            config.clone(),
            Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
        ));
        let run_service = Arc::new(RunService::new(
            Arc::clone(&run_store) as Arc<dyn RunStore>,
            Arc::clone(&bus),
            Arc::clone(&clock),
            Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
        ));
        let goal_service = Arc::new(GoalService::new(
            Arc::clone(&goal_store) as Arc<dyn GoalStore>,
            Arc::clone(&bus),
            Arc::clone(&clock),
            Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
            Some(Arc::clone(&agent_service)),
            Some(Arc::clone(&task_service)),
            None,
        ));

        let deps = OrchestratorDeps {
            task_store: Arc::clone(&task_store) as Arc<dyn TaskStore>,
            agent_store: Arc::clone(&agent_store) as Arc<dyn AgentStore>,
            run_store: Arc::clone(&run_store) as Arc<dyn RunStore>,
            state_store: Arc::clone(&state_store) as Arc<dyn StateStore>,
            adapters: Arc::new(registry),
            workspace: Arc::new(GatedWorkspace {
                inner: FakeWorkspaceManager::new(),
                gate: workspace_gate,
            }),
            templates: Arc::new(FakeTemplateEngine::new()),
            processes,
            events: Arc::clone(&bus),
            task_service: Arc::clone(&task_service),
            agent_service,
            run_service,
            clock,
            lock: Arc::new(InMemoryLock::new()),
            command_runner: Arc::new(PassingCommandRunner),
            config,
            project_root: std::path::PathBuf::from("/tmp/project"),
            context_store: None,
            message_service: None,
            goal_store: Some(Arc::clone(&goal_store) as Arc<dyn GoalStore>),
            skill_loader: None,
        };

        Harness {
            orchestrator: Arc::new(Orchestrator::new(deps)),
            tasks: task_store,
            agents: agent_store,
            runs: run_store,
            state: state_store,
            task_service,
            goal_service,
            processes: scripted,
            log,
            _recorder: recorder,
        }
    }

    fn log(&self) -> Vec<Mark> {
        lock(&self.log).clone()
    }

    fn spawn_count(&self) -> usize {
        self.log().iter().filter(|m| **m == Mark::Spawn).count()
    }
}

/// Poll `future` up to `polls` times, yielding to the runtime between polls.
/// `Some(output)` if it finished, `None` if it is still pending — i.e. blocked
/// on something the other tasks in this test have not released. Deterministic
/// on a current-thread runtime with in-memory stores: the number of polls an
/// unblocked future needs is bounded and independent of timing.
async fn poll_to_completion<F: std::future::Future + Unpin>(
    future: &mut F,
    polls: usize,
) -> Option<F::Output> {
    for _ in 0..polls {
        tokio::select! {
            biased;
            output = &mut *future => return Some(output),
            () = tokio::task::yield_now() => {}
        }
    }
    None
}

/// Enough yields for any in-memory pause to finish, small enough to stay fast.
const SETTLE_POLLS: usize = 400;

// ── (a) Pause atomicity ──────────────────────────────────────────────────

/// A goal-assigned autonomous task that dispatch has already picked up.
fn autonomous_task(id: &str) -> Task {
    Task {
        labels: vec![orch_domain::AUTONOMOUS_LABEL.to_owned()],
        goal_id: Some(GOAL.into()),
        assignee: Some(WORKER.into()),
        ..task(id)
    }
}

/// Run one dispatch tick, park it at `at`, pause the goal while it is parked,
/// release it, and return the harness for assertions.
async fn pause_during_tick(at: GateAt) -> Harness {
    let gate = Gate::armed();
    let harness = Harness::new(
        vec![autonomous_task("tsk_a")],
        Some((at, Arc::clone(&gate))),
    );

    let tick = {
        let orchestrator = Arc::clone(&harness.orchestrator);
        async move { orchestrator.run_all().await }
    };
    let pause_while_in_flight = async {
        gate.wait_until_entered().await;

        let goal_service = Arc::clone(&harness.goal_service);
        let mut pause = Box::pin(async move {
            goal_service
                .update_status(GOAL, GoalStatus::Paused, false)
                .await
        });
        // If pause serialises with the tick it stays pending here (the tick
        // is parked); if not, it completes. Either way the gate opens next, so
        // the test never waits on the outcome it is checking.
        let early = poll_to_completion(&mut pause, SETTLE_POLLS).await;
        gate.open();
        let result = match early {
            Some(result) => result,
            None => pause.await,
        };
        result.expect("pausing an active goal succeeds");
    };

    let (tick_result, ()) = tokio::join!(tick, pause_while_in_flight);
    tick_result.expect("the tick itself does not error");
    harness
}

fn spawns_after_pause(log: &[Mark]) -> usize {
    let Some(pause_at) = log
        .iter()
        .position(|mark| matches!(mark, Mark::PauseDecided(_)))
    else {
        return 0;
    };
    log[pause_at..]
        .iter()
        .filter(|mark| **mark == Mark::Spawn)
        .count()
}

#[tokio::test(flavor = "current_thread")]
async fn pausing_a_goal_while_dispatch_is_about_to_spawn_starts_nothing_after_the_pause() {
    let harness = pause_during_tick(GateAt::BeforeSpawn).await;

    assert_eq!(
        spawns_after_pause(&harness.log()),
        0,
        "a process was spawned after the pause decision; log: {:?}",
        harness.log()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pausing_a_goal_while_dispatch_is_preparing_starts_nothing_after_the_pause() {
    let harness = pause_during_tick(GateAt::WorkspacePrepare).await;

    assert_eq!(
        spawns_after_pause(&harness.log()),
        0,
        "a process was spawned after the pause decision; log: {:?}",
        harness.log()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pausing_a_goal_while_dispatch_is_preparing_leaves_task_run_and_process_in_agreement() {
    // Either outcome is legal — the tick won the race (task started *before*
    // the pause decision and keeps running) or the pause won (task cancelled,
    // nothing started) — but never a mixture: a cancelled task with a live
    // run, a run left `preparing` with no process, or a started task nothing
    // is running.
    let harness = pause_during_tick(GateAt::WorkspacePrepare).await;

    let task = harness.tasks.peek("tsk_a").unwrap();
    let active_runs: Vec<_> = harness
        .runs
        .snapshot()
        .into_iter()
        .filter(|run| matches!(run.status, RunStatus::Preparing | RunStatus::Running))
        .map(|run| (run.id, run.status))
        .collect();
    let started = harness.spawn_count();
    let observed = (task.status, started, active_runs.len());

    assert!(
        observed == (TaskStatus::Cancelled, 0, 0) || observed == (TaskStatus::InProgress, 1, 1),
        "task/spawns/active-runs disagree: {observed:?}; active runs {active_runs:?}; log {:?}",
        harness.log()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_paused_goals_task_is_not_dispatched_by_a_later_tick() {
    // Guards the trivial ordering (pause first, tick second) so the tests
    // above cannot be satisfied by simply never dispatching anything.
    let harness = Harness::new(vec![autonomous_task("tsk_a")], None);
    harness
        .goal_service
        .update_status(GOAL, GoalStatus::Paused, false)
        .await
        .unwrap();

    harness.orchestrator.run_all().await.unwrap();

    assert_eq!(harness.spawn_count(), 0);
    assert_eq!(
        harness.tasks.peek("tsk_a").unwrap().status,
        TaskStatus::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_unpaused_goals_task_is_dispatched_so_the_gate_harness_can_fail() {
    // Control for the gate: with no pause, the same parked tick must spawn
    // exactly once. If this fails the pause tests prove nothing.
    let gate = Gate::armed();
    let harness = Harness::new(
        vec![autonomous_task("tsk_a")],
        Some((GateAt::BeforeSpawn, Arc::clone(&gate))),
    );
    let tick = {
        let orchestrator = Arc::clone(&harness.orchestrator);
        async move { orchestrator.run_all().await }
    };
    let release = async {
        gate.wait_until_entered().await;
        assert_eq!(harness.spawn_count(), 0, "parked tick has not spawned yet");
        gate.open();
    };
    let (result, ()) = tokio::join!(tick, release);
    result.unwrap();

    assert_eq!(harness.spawn_count(), 1);
    assert_eq!(
        harness.tasks.peek("tsk_a").unwrap().status,
        TaskStatus::InProgress
    );
}

// ── (b) Task cancel kills a live agent process ───────────────────────────

/// A plain task dispatched and running under the scripted Pi child.
async fn running_task() -> Harness {
    let harness = Harness::new(vec![task("tsk_live")], None);
    harness.orchestrator.run_all().await.unwrap();
    assert_eq!(harness.spawn_count(), 1, "precondition: the agent is live");
    assert_eq!(
        harness.tasks.peek("tsk_live").unwrap().status,
        TaskStatus::InProgress
    );
    harness
}

fn killed(harness: &Harness) -> bool {
    harness.log().contains(&Mark::Kill(24601)) || !harness.processes.killed_with_grace().is_empty()
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrator_cancel_task_kills_the_live_process_baseline() {
    // Passes today: the orchestrator-level cancel already kills. It is the
    // behaviour `TaskService::cancel` must reach through the daemon.
    let harness = running_task().await;

    harness.orchestrator.cancel_task("tsk_live").await.unwrap();

    assert!(killed(&harness), "log: {:?}", harness.log());
    assert_eq!(
        harness.tasks.peek("tsk_live").unwrap().status,
        TaskStatus::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn task_service_cancel_kills_the_live_agent_process() {
    let harness = running_task().await;

    // The path every daemon client reaches (CLI `task cancel`, MCP
    // `task_cancel`, TUI). No tick is driven: the kill must not wait for one.
    harness.task_service.cancel("tsk_live").await.unwrap();

    assert!(
        killed(&harness),
        "task cancel left the agent process running; log: {:?}",
        harness.log()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn task_service_cancel_finishes_the_run_and_frees_the_agent() {
    let harness = running_task().await;

    harness.task_service.cancel("tsk_live").await.unwrap();

    let runs = harness.runs.snapshot();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Cancelled);
    let worker = harness.agents.peek(WORKER).unwrap();
    assert_eq!(worker.status, AgentStatus::Idle);
    assert!(
        harness.state.peek().running.is_empty(),
        "state.json still lists the cancelled task as running"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_task_that_is_not_running_kills_nothing() {
    // Over-kill guard: cancel must only reach a process for a task that has one.
    let harness = Harness::new(vec![task("tsk_idle")], None);

    harness.task_service.cancel("tsk_idle").await.unwrap();

    assert!(!killed(&harness), "log: {:?}", harness.log());
    assert_eq!(
        harness.tasks.peek("tsk_idle").unwrap().status,
        TaskStatus::Cancelled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_one_running_task_does_not_kill_another_tasks_process() {
    let harness = Harness::new(vec![task("tsk_one")], None);
    harness.orchestrator.run_all().await.unwrap();
    // A second, unrelated task that never started.
    harness.tasks.seed(task("tsk_two"));

    harness.task_service.cancel("tsk_two").await.unwrap();

    assert!(!killed(&harness), "log: {:?}", harness.log());
    assert_eq!(
        harness.tasks.peek("tsk_one").unwrap().status,
        TaskStatus::InProgress
    );
}

// ── (c) Context expiry is an explicit step ───────────────────────────────

mod context_expiry {
    use super::*;
    use orch_core::storage::ContextStore;
    use orch_infra::storage::{FileContextStore, Paths};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    const T0_MS: i64 = 1_760_000_000_000;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        clock: Arc<FixedClock>,
        store: FileContextStore,
    }

    /// Entries `stale` (ttl 1s), `fresh` (ttl 1h) and `forever` (no ttl), all
    /// written at `T0`, then the clock moved to just past `stale`'s expiry.
    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let clock = Arc::new(FixedClock::at(T0_MS));
        let store = FileContextStore::new(Paths::new(&root), Arc::clone(&clock) as Arc<dyn Clock>);
        store.set("stale", "old", Some(1_000)).await.unwrap();
        store.set("fresh", "new", Some(3_600_000)).await.unwrap();
        store.set("forever", "kept", None).await.unwrap();
        clock.advance_ms(2_000);
        Fixture {
            _temp: temp,
            root,
            clock,
            store,
        }
    }

    /// Every state file under `dir` → (bytes, mtime), keyed by path relative
    /// to `dir`. Anything a read rewrites, deletes or creates shows up as a
    /// difference. `.index.lock` is a lock, not state, and is ignored.
    fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, SystemTime)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else if path.file_name().is_some_and(|name| name != ".index.lock") {
                    let modified = std::fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .expect("mtime");
                    let relative = path.strip_prefix(root).expect("under root");
                    out.insert(
                        relative.display().to_string(),
                        (std::fs::read(&path).expect("read"), modified),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    /// Files added, removed or modified between two snapshots, as a short
    /// human-readable list (`-` removed, `+` added, `~` changed).
    fn diff(
        before: &BTreeMap<String, (Vec<u8>, SystemTime)>,
        after: &BTreeMap<String, (Vec<u8>, SystemTime)>,
    ) -> Vec<String> {
        let mut changes = Vec::new();
        for (path, old) in before {
            match after.get(path) {
                None => changes.push(format!("- {path}")),
                Some(new) if new != old => changes.push(format!("~ {path}")),
                Some(_) => {}
            }
        }
        changes.extend(
            after
                .keys()
                .filter(|path| !before.contains_key(*path))
                .map(|path| format!("+ {path}")),
        );
        changes
    }

    fn assert_untouched(root: &Path, before: &BTreeMap<String, (Vec<u8>, SystemTime)>, what: &str) {
        let changes = diff(before, &snapshot(root));
        assert!(changes.is_empty(), "{what} touched the disk: {changes:?}");
    }

    #[tokio::test]
    async fn get_of_an_expired_key_reports_absent_and_leaves_the_disk_untouched() {
        let f = fixture().await;
        let before = snapshot(&f.root);

        let got = f.store.get("stale").await.unwrap();

        assert!(got.is_none(), "an expired key must read as absent");
        assert_untouched(&f.root, &before, "get()");
    }

    #[tokio::test]
    async fn list_omits_expired_keys_and_leaves_the_disk_untouched() {
        let f = fixture().await;
        let before = snapshot(&f.root);

        let keys: Vec<String> = f
            .store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.key)
            .collect();

        assert_eq!(keys, vec!["forever", "fresh"]);
        assert_untouched(&f.root, &before, "list()");
    }

    #[tokio::test]
    async fn get_all_omits_expired_keys_and_leaves_the_disk_untouched() {
        let f = fixture().await;
        let before = snapshot(&f.root);

        let all = f.store.get_all().await.unwrap();

        assert_eq!(
            all.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["forever", "fresh"]
        );
        assert_untouched(&f.root, &before, "get_all()");
    }

    #[tokio::test]
    async fn reading_with_nothing_expired_leaves_the_disk_untouched() {
        // Control: proves the snapshot comparison is not itself flaky.
        let f = fixture().await;
        f.clock.set_ms(T0_MS);
        let before = snapshot(&f.root);

        f.store.list().await.unwrap();
        f.store.get("stale").await.unwrap();

        assert_untouched(&f.root, &before, "a read with nothing expired");
    }

    // ── Explicit expiry ─────────────────────────────────────────────────
    //
    // `FileContextStore::expire_now(&self) -> Result<Vec<String>>`: remove every
    // expired entry file, rewrite the index once, return the removed keys
    // sorted. Idempotent; a call with nothing expired changes nothing on disk.
    //
    // This API is inherent, so existing core store trait implementations stay unchanged.

    #[tokio::test]
    async fn expire_now_removes_exactly_the_expired_entries_and_reports_them() {
        let f = fixture().await;

        let removed = f.store.expire_now().await.unwrap();

        assert_eq!(removed, vec!["stale".to_owned()]);
        assert!(!f.root.join(".orchestry/context/stale.json").exists());
        assert!(f.root.join(".orchestry/context/fresh.json").exists());
        assert!(f.root.join(".orchestry/context/forever.json").exists());
        let index = std::fs::read_to_string(f.root.join(".orchestry/context/_index.json")).unwrap();
        assert!(!index.contains("stale"), "index still lists stale: {index}");
        assert!(
            index.contains("fresh") && index.contains("forever"),
            "{index}"
        );
    }

    #[tokio::test]
    async fn expire_now_with_nothing_expired_changes_nothing_on_disk() {
        let f = fixture().await;
        f.clock.set_ms(T0_MS);
        let before = snapshot(&f.root);

        let removed = f.store.expire_now().await.unwrap();

        assert!(removed.is_empty());
        assert_untouched(&f.root, &before, "a read with nothing expired");
    }

    #[tokio::test]
    async fn expire_now_twice_removes_nothing_the_second_time() {
        let f = fixture().await;

        f.store.expire_now().await.unwrap();
        let second = f.store.expire_now().await.unwrap();

        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn a_key_renewed_before_the_expiry_step_survives_it() {
        // The lost-renewal race: `set` after the key expired but before the
        // sweep must win, because the sweep re-checks expiry under the store's
        // own view rather than acting on a stale list.
        let f = fixture().await;
        f.store
            .set("stale", "renewed", Some(3_600_000))
            .await
            .unwrap();

        let removed = f.store.expire_now().await.unwrap();

        assert!(removed.is_empty());
        assert_eq!(
            f.store.get("stale").await.unwrap().unwrap().value,
            "renewed"
        );
    }
}
