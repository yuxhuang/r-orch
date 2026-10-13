//! Shared support for the engine's integration tests.
//!
//! These tests drive the **public** orchestrator API (`run_all`,
//! `start_watch`, `stop`) against real adapters and, where it matters, real
//! file stores. The only thing faked is the child process, because the point
//! is to exercise the adapter's own parser and the orchestrator's collection
//! path, not to require a live AI CLI — which is also why the reference suite
//! mocks `spawn` in exactly these tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use orch_core::clock::Clock;
use orch_core::error::Result;
use orch_core::process::{
    ExitNotifier, ExitOutcome, ExitSignal, ProcessManager, Signal, SpawnOptions, SpawnedChild,
};
use orch_core::storage::{AgentStore, RunStore, StateStore, TaskStore};
use orch_core::test_support::{
    FakeAgentStore, FakeRunStore, FakeStateStore, FakeTaskStore, FakeTemplateEngine,
    FakeWorkspaceManager, FixedClock, SeqIdGenerator,
};
use orch_domain::{Agent, OrchestratorConfig, OrchestratorEvent, Task, default_config};
use orch_engine::events::EventBus;
use orch_engine::lock::InMemoryLock;
use orch_engine::orchestrator::{Orchestrator, OrchestratorDeps};
use orch_engine::review_runner::{CommandOutcome, CommandRunner};
use orch_engine::{AgentService, RunService, TaskService};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

/// Duplex capacity. Larger than any scripted payload, so a write never blocks
/// waiting for the adapter to read.
const PIPE_CAPACITY: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRecord {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
}

impl SpawnRecord {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.args.iter().any(|arg| arg == flag)
    }

    pub fn value_after(&self, flag: &str) -> Option<&str> {
        let index = self.args.iter().position(|arg| arg == flag)?;
        self.args.get(index + 1).map(String::as_str)
    }
}

/// One spawned "process": the halves of its pipes the test keeps.
struct Child {
    stdout: Option<DuplexStream>,
    stderr: Option<DuplexStream>,
    stdin: Option<DuplexStream>,
    exit: ExitNotifier,
}

/// A process manager that hands out in-memory pipes instead of children.
///
/// The test writes the agent's stdout **after** dispatch, exactly as the TS
/// mock writes to its `PassThrough` once the tick has returned.
pub struct ScriptedProcessManager {
    pid: u32,
    children: Mutex<Vec<Child>>,
    spawns: Mutex<Vec<SpawnRecord>>,
    kills: Mutex<Vec<(u32, Signal)>>,
    graceful_kills: Mutex<Vec<(u32, Duration)>>,
    /// `kill_with_grace` resolving the child's exit mirrors the real manager,
    /// whose `SIGTERM` makes node emit `close`.
    alive: Mutex<bool>,
}

impl ScriptedProcessManager {
    pub fn new(pid: u32) -> Arc<Self> {
        Arc::new(ScriptedProcessManager {
            pid,
            children: Mutex::new(Vec::new()),
            spawns: Mutex::new(Vec::new()),
            kills: Mutex::new(Vec::new()),
            graceful_kills: Mutex::new(Vec::new()),
            alive: Mutex::new(true),
        })
    }

    fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn spawns(&self) -> Vec<SpawnRecord> {
        Self::lock(&self.spawns).clone()
    }

    pub fn killed_with_grace(&self) -> Vec<(u32, Duration)> {
        Self::lock(&self.graceful_kills).clone()
    }

    /// Feed the most recently spawned child's stdout.
    pub async fn write_stdout(&self, bytes: impl AsRef<[u8]>) {
        let mut stdout = {
            let mut children = Self::lock(&self.children);
            let child = children.last_mut().expect("a spawned child");
            child.stdout.take().expect("stdout is still ours")
        };
        stdout
            .write_all(bytes.as_ref())
            .await
            .expect("write to the child's stdout");
        stdout.flush().await.expect("flush");
        let mut children = Self::lock(&self.children);
        children.last_mut().expect("a spawned child").stdout = Some(stdout);
    }

    /// Write one JSONL frame.
    pub async fn write_line(&self, line: &str) {
        self.write_stdout(format!("{line}\n")).await;
    }

    pub async fn write_stderr(&self, bytes: impl AsRef<[u8]>) {
        let mut stderr = {
            let mut children = Self::lock(&self.children);
            children
                .last_mut()
                .expect("a spawned child")
                .stderr
                .take()
                .expect("stderr is still ours")
        };
        stderr
            .write_all(bytes.as_ref())
            .await
            .expect("write stderr");
        stderr.flush().await.expect("flush");
        let mut children = Self::lock(&self.children);
        children.last_mut().expect("a spawned child").stderr = Some(stderr);
    }

    /// Close the child's stdout, which is what an exiting process does.
    pub fn close_stdout(&self) {
        let mut children = Self::lock(&self.children);
        children.last_mut().expect("a spawned child").stdout = None;
    }

    /// Everything the adapter wrote to the child's stdin so far.
    pub async fn read_stdin(&self) -> String {
        let mut stdin = {
            let mut children = Self::lock(&self.children);
            children
                .last_mut()
                .expect("a spawned child")
                .stdin
                .take()
                .expect("stdin is still ours")
        };
        let mut buffer = vec![0u8; PIPE_CAPACITY];
        // The adapter writes its prompt frame eagerly, so a single read with a
        // short timeout is enough; a timeout means "nothing was written".
        let read = tokio::time::timeout(Duration::from_millis(200), stdin.read(&mut buffer))
            .await
            .unwrap_or(Ok(0))
            .unwrap_or(0);
        let mut children = Self::lock(&self.children);
        children.last_mut().expect("a spawned child").stdin = Some(stdin);
        String::from_utf8_lossy(&buffer[..read]).into_owned()
    }

    /// Resolve the child's exit, as a real process close would.
    pub fn exit(&self, code: i32) {
        *Self::lock(&self.alive) = false;
        if let Some(child) = Self::lock(&self.children).last() {
            child.exit.notify(ExitOutcome {
                code: Some(code),
                signal: None,
            });
        }
    }
}

#[async_trait]
impl ProcessManager for ScriptedProcessManager {
    fn is_alive(&self, _pid: u32) -> bool {
        *Self::lock(&self.alive)
    }

    fn kill(&self, pid: u32, signal: Signal) {
        Self::lock(&self.kills).push((pid, signal));
        self.exit(0);
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        Self::lock(&self.graceful_kills).push((pid, grace));
        self.exit(0);
    }

    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild> {
        Self::lock(&self.spawns).push(SpawnRecord {
            command: command.to_owned(),
            args: args.to_vec(),
            cwd: opts.cwd.clone(),
        });

        let (test_stdout, child_stdout) = tokio::io::duplex(PIPE_CAPACITY);
        let (test_stderr, child_stderr) = tokio::io::duplex(PIPE_CAPACITY);
        let (test_stdin, child_stdin) = tokio::io::duplex(PIPE_CAPACITY);
        let (notifier, exit) = ExitSignal::channel();

        Self::lock(&self.children).push(Child {
            stdout: Some(test_stdout),
            stderr: Some(test_stderr),
            stdin: Some(test_stdin),
            exit: notifier,
        });

        Ok(SpawnedChild {
            pid: self.pid,
            stdout: Some(Box::new(child_stdout)),
            stderr: Some(Box::new(child_stderr)),
            stdin: Some(Box::new(child_stdin)),
            exit,
        })
    }
}

/// One recorded adapter session from `fixtures/adapters/<kind>/<name>`.
///
/// Sharing WP-2's recordings rather than inlining payloads keeps one source of
/// truth per adapter: a frame added to a fixture is seen by the adapter's own
/// parser test and by the orchestrator e2e below at the same time.
pub fn adapter_fixture(kind: &str, name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/adapters")
        .join(kind)
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// A command runner that answers every auto-review criterion as passing, so
/// no integration test shells out to `npm` (PARITY.md V31).
pub struct PassingCommandRunner;

#[async_trait]
impl CommandRunner for PassingCommandRunner {
    async fn run(
        &self,
        _command: &str,
        _args: &[&str],
        _cwd: &std::path::Path,
        _timeout: Duration,
    ) -> CommandOutcome {
        CommandOutcome {
            failed: false,
            stdout: "ok".into(),
            stderr: String::new(),
        }
    }
}

/// One orchestrator over in-memory stores, a real adapter registry and a
/// scripted process manager.
pub struct Rig {
    pub orchestrator: Orchestrator,
    pub tasks: Arc<FakeTaskStore>,
    pub agents: Arc<FakeAgentStore>,
    pub runs: Arc<FakeRunStore>,
    pub processes: Arc<ScriptedProcessManager>,
    events: Arc<Mutex<Vec<OrchestratorEvent>>>,
    _recorder: orch_engine::events::Unsubscribe,
}

impl Rig {
    /// `config.scheduling.poll_interval_ms` is left long: every integration
    /// test drives dispatch explicitly through `run_all`.
    pub fn new(
        tasks: Vec<Task>,
        agents: Vec<Agent>,
        adapter: Arc<dyn orch_core::adapter::Adapter>,
        processes: Arc<ScriptedProcessManager>,
    ) -> Rig {
        let mut config = default_config();
        config.scheduling.poll_interval_ms = 100_000;
        Rig::with_config(tasks, agents, adapter, processes, config)
    }

    pub fn with_config(
        tasks: Vec<Task>,
        agents: Vec<Agent>,
        adapter: Arc<dyn orch_core::adapter::Adapter>,
        processes: Arc<ScriptedProcessManager>,
        config: OrchestratorConfig,
    ) -> Rig {
        let task_store = Arc::new(FakeTaskStore::with_tasks(tasks));
        let agent_store = Arc::new(FakeAgentStore::with_agents(agents));
        let run_store = Arc::new(FakeRunStore::new());
        let state_store = Arc::new(FakeStateStore::new());
        let bus = Arc::new(EventBus::new());
        bus.set_max_listeners(0);
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new());
        let ids = Arc::new(SeqIdGenerator::new());

        let mut registry = orch_core::adapter::AdapterRegistry::new();
        registry.register(adapter);

        let events: Arc<Mutex<Vec<OrchestratorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let recorder = bus.on_any(move |event| {
            sink.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event.clone())
        });

        let deps = OrchestratorDeps {
            task_store: Arc::clone(&task_store) as Arc<dyn TaskStore>,
            agent_store: Arc::clone(&agent_store) as Arc<dyn AgentStore>,
            run_store: Arc::clone(&run_store) as Arc<dyn RunStore>,
            state_store: Arc::clone(&state_store) as Arc<dyn StateStore>,
            adapters: Arc::new(registry),
            workspace: Arc::new(FakeWorkspaceManager::new()),
            templates: Arc::new(FakeTemplateEngine::new()),
            processes: Arc::clone(&processes) as Arc<dyn ProcessManager>,
            events: Arc::clone(&bus),
            task_service: Arc::new(TaskService::new(
                Arc::clone(&task_store) as Arc<dyn TaskStore>,
                Arc::clone(&agent_store) as Arc<dyn AgentStore>,
                Arc::clone(&bus),
                config.clone(),
                Arc::clone(&clock),
                Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
                None,
            )),
            agent_service: Arc::new(AgentService::new(
                Arc::clone(&agent_store) as Arc<dyn AgentStore>,
                Arc::clone(&state_store) as Arc<dyn StateStore>,
                Arc::clone(&bus),
                config.clone(),
                Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
            )),
            run_service: Arc::new(RunService::new(
                Arc::clone(&run_store) as Arc<dyn RunStore>,
                Arc::clone(&bus),
                Arc::clone(&clock),
                Arc::clone(&ids) as Arc<dyn orch_core::ids::IdGenerator>,
            )),
            clock,
            lock: Arc::new(InMemoryLock::new()),
            command_runner: Arc::new(PassingCommandRunner),
            config,
            project_root: std::path::PathBuf::from("/tmp/project"),
            context_store: None,
            message_service: None,
            goal_store: None,
            skill_loader: None,
        };

        Rig {
            orchestrator: Orchestrator::new(deps),
            tasks: task_store,
            agents: agent_store,
            runs: run_store,
            processes,
            events,
            _recorder: recorder,
        }
    }

    pub fn events(&self) -> Vec<OrchestratorEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// `from→to` for every status change of one task, in order.
    pub fn transitions(&self, task_id: &str) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                OrchestratorEvent::TaskStatusChanged {
                    task_id: id,
                    from,
                    to,
                } if id == task_id => Some(format!("{from}→{to}")),
                _ => None,
            })
            .collect()
    }

    pub fn task(&self, id: &str) -> Task {
        self.tasks
            .peek(id)
            .unwrap_or_else(|| panic!("no task {id}"))
    }

    pub fn agent(&self, id: &str) -> Agent {
        self.agents
            .peek(id)
            .unwrap_or_else(|| panic!("no agent {id}"))
    }

    /// Poll until `predicate` holds, as the TS `waitFor` helper does.
    pub async fn wait_for(&self, label: &str, mut predicate: impl FnMut(&Rig) -> bool) {
        for _ in 0..2000 {
            if predicate(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("{label}: condition was not met in time");
    }

    pub async fn wait_for_status(&self, task_id: &str, status: orch_domain::TaskStatus) {
        let owned = task_id.to_owned();
        self.wait_for(&format!("{task_id} reaches {status}"), move |rig| {
            rig.tasks
                .peek(&owned)
                .is_some_and(|task| task.status == status)
        })
        .await;
    }
}
