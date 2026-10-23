//! Shared rig for the token-eco engine tests (T1 / F4).
//!
//! Everything is real except the model: file-backed stores in a disposable
//! project directory, the real run/task/agent services, the real Liquid
//! template engine and the real `Orchestrator::run_all` dispatch path. The
//! only fake is [`ProbeAdapter`], which records exactly what dispatch hands an
//! adapter and replays scripted events. No AI CLI is ever executed and
//! nothing outside the temporary directory is touched.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the shared rig"
)]

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use orch_core::EffortPreparation;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterRegistry, AdapterTestResult, AgentEvent, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::EnvInheritance;
use orch_core::storage::ConfigStore;
use orch_core::test_support::{
    FakeProcessManager, FakeWorkspaceManager, FixedClock, SeqIdGenerator, done_event, make_agent,
    make_task, output_event,
};
use orch_domain::{
    Agent, AgentConfig, AgentStatus, OrchestratorConfig, OrchestratorEvent, PromptMode, Run,
    RunStatus, Task, TaskStatus,
};
use orch_engine::container::{Engine, EngineSeams, build_light_with};
use orch_engine::lock::InMemoryLock;
use orch_engine::orchestrator::{Orchestrator, OrchestratorDeps};
use orch_engine::review_runner::{CommandOutcome, CommandRunner};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_infra::template::LiquidTemplateEngine;
use serde_json::{Value, json};

pub const ADAPTER: &str = "claude";

/// What one `execute` call received.
#[derive(Debug, Clone)]
pub struct Captured {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub workspace: PathBuf,
    pub env: BTreeMap<String, String>,
    pub inherit_env: EnvInheritance,
    pub config: AgentConfig,
}

impl Captured {
    pub fn task_id(&self) -> &str {
        self.env
            .get("ORCH_TASK_ID")
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Whether this prompt pair was rendered by the eco templates.
    pub fn is_eco(&self) -> bool {
        self.system_prompt
            .as_deref()
            .is_some_and(|s| s.contains("## Compact prompt"))
    }

    /// Everything the child would see, system first.
    pub fn all_text(&self) -> String {
        format!(
            "{}\n\u{1f}\n{}",
            self.system_prompt.as_deref().unwrap_or(""),
            self.prompt
        )
    }
}

type Script = Vec<Result<AgentEvent, AdapterError>>;

/// Records the call order (`prepare`, `execute`), captures parameters and
/// replays one scripted stream per `execute`.
pub struct ProbeAdapter {
    pub calls: Mutex<Vec<Captured>>,
    pub order: Mutex<Vec<String>>,
    scripts: Mutex<VecDeque<Script>>,
    prepare_error: Mutex<Option<AdapterError>>,
    prepare_diagnostics: Mutex<Vec<String>>,
    clear_effort: Mutex<bool>,
    prepare_model: Mutex<Option<String>>,
    hang_next: Mutex<u32>,
    gate: Arc<tokio::sync::Semaphore>,
    execute_error: Mutex<Option<AdapterError>>,
}

impl ProbeAdapter {
    pub fn new() -> Arc<Self> {
        Arc::new(ProbeAdapter {
            calls: Mutex::new(Vec::new()),
            order: Mutex::new(Vec::new()),
            scripts: Mutex::new(VecDeque::new()),
            prepare_error: Mutex::new(None),
            prepare_diagnostics: Mutex::new(Vec::new()),
            clear_effort: Mutex::new(false),
            prepare_model: Mutex::new(None),
            hang_next: Mutex::new(0),
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
            execute_error: Mutex::new(None),
        })
    }

    pub fn push_script(&self, script: Script) {
        self.scripts.lock().unwrap().push_back(script);
    }

    /// The next `n` executions produce a stream that never yields until the
    /// run is cancelled (a long-running agent).
    /// Let every gated stream produce its events. Streams do not progress
    /// until the test releases them, so a dispatch pass never races a run that
    /// finishes instantly (which would make start times nondeterministic).
    pub fn release(&self) {
        self.gate.add_permits(10_000);
    }

    pub fn hang_next(&self, n: u32) {
        *self.hang_next.lock().unwrap() = n;
    }

    pub fn fail_preparation(&self, message: &str) {
        *self.prepare_error.lock().unwrap() = Some(AdapterError::new(message, None));
    }

    pub fn fail_preparation_none(&self) {
        *self.prepare_error.lock().unwrap() = None;
    }

    pub fn prepare_diagnostics(&self, diagnostics: &[&str], clear_effort: bool) {
        *self.prepare_diagnostics.lock().unwrap() =
            diagnostics.iter().map(|d| (*d).to_owned()).collect();
        *self.clear_effort.lock().unwrap() = clear_effort;
    }

    /// Make preparation (illegally) rewrite the model, to prove dispatch refuses it.
    pub fn rewrite_model_in_preparation(&self, model: &str) {
        *self.prepare_model.lock().unwrap() = Some(model.to_owned());
    }

    pub fn fail_execute(&self, message: &str) {
        *self.execute_error.lock().unwrap() = Some(AdapterError::new(message, None));
    }

    pub fn calls(&self) -> Vec<Captured> {
        self.calls.lock().unwrap().clone()
    }

    pub fn order(&self) -> Vec<String> {
        self.order.lock().unwrap().clone()
    }

    pub fn for_task(&self, task_id: &str) -> Captured {
        self.calls()
            .into_iter()
            .rev()
            .find(|c| c.task_id() == task_id)
            .unwrap_or_else(|| panic!("no execute call for {task_id}"))
    }
}

#[async_trait]
impl Adapter for ProbeAdapter {
    fn kind(&self) -> &'static str {
        ADAPTER
    }

    async fn test(&self) -> AdapterTestResult {
        AdapterTestResult {
            ok: true,
            ..Default::default()
        }
    }

    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> Result<EffortPreparation, AdapterError> {
        self.order.lock().unwrap().push("prepare".into());
        if let Some(err) = self.prepare_error.lock().unwrap().clone() {
            return Err(err);
        }
        let mut config = params.config.clone();
        if *self.clear_effort.lock().unwrap() {
            config.effort = None;
        }
        if let Some(model) = self.prepare_model.lock().unwrap().clone() {
            config.model = Some(model);
        }
        Ok(EffortPreparation {
            config,
            diagnostics: self.prepare_diagnostics.lock().unwrap().clone(),
        })
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        self.order.lock().unwrap().push("execute".into());
        let params_cancel = params.cancel.clone();
        self.calls.lock().unwrap().push(Captured {
            prompt: params.prompt,
            system_prompt: params.system_prompt,
            workspace: params.workspace,
            env: params.env,
            inherit_env: params.inherit_env,
            config: params.config,
        });
        if let Some(err) = self.execute_error.lock().unwrap().clone() {
            return Err(err);
        }
        {
            let mut hang = self.hang_next.lock().unwrap();
            if *hang > 0 {
                *hang -= 1;
                let cancel = params_cancel;
                let events = stream::unfold(cancel, |cancel| async move {
                    cancel.cancelled().await;
                    None::<(Result<AgentEvent, AdapterError>, _)>
                });
                return Ok(ExecuteHandle {
                    pid: 4242,
                    events: Box::pin(events),
                });
            }
        }
        let script = self.scripts.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![
                Ok(output_event("2025-10-13T16:37:33.426Z", "ok")),
                Ok(done_event("2025-10-13T16:37:33.426Z")),
            ]
        });
        let gate = Arc::clone(&self.gate);
        let events = stream::once(async move {
            gate.acquire().await.expect("gate open").forget();
            stream::iter(script)
        })
        .flatten();
        Ok(ExecuteHandle {
            pid: 4242,
            events: Box::pin(events),
        })
    }

    async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
        Ok(())
    }
}

struct PassingRunner;

#[async_trait]
impl CommandRunner for PassingRunner {
    async fn run(
        &self,
        _command: &str,
        _args: &[&str],
        _cwd: &Path,
        _timeout: std::time::Duration,
    ) -> CommandOutcome {
        CommandOutcome {
            failed: false,
            stdout: "ok".into(),
            stderr: String::new(),
        }
    }
}

/// Builder inputs.
pub struct RigSpec {
    /// Raw `config.yml` content, merged over defaults when read.
    pub raw_config: Value,
    pub agents: Vec<Agent>,
    pub tasks: Vec<Task>,
    /// Library skills (name, content) served by an in-memory loader.
    pub skills: Vec<(String, String)>,
    /// Wire `Orchestrator::set_config_store` (production wiring).
    pub config_store: bool,
}

impl Default for RigSpec {
    fn default() -> Self {
        RigSpec {
            raw_config: json!({}),
            agents: vec![claude_agent("agt_a", "Alpha")],
            tasks: Vec::new(),
            skills: Vec::new(),
            config_store: true,
        }
    }
}

pub fn claude_agent(id: &str, name: &str) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        adapter: ADAPTER.into(),
        role: Some(format!("{name} role. Does {name} things.")),
        ..make_agent()
    }
}

pub fn task(id: &str, assignee: Option<&str>) -> Task {
    Task {
        id: id.into(),
        title: format!("Title of {id}"),
        description: format!("Description of {id}"),
        assignee: assignee.map(str::to_owned),
        ..make_task()
    }
}

pub fn failed_run(id: &str, task_id: &str, agent_id: &str, finished_at: &str, error: &str) -> Run {
    Run {
        id: id.into(),
        task_id: task_id.into(),
        agent_id: agent_id.into(),
        attempt: 1,
        status: RunStatus::Failed,
        started_at: "2025-10-13T16:00:00.000Z".into(),
        finished_at: Some(finished_at.into()),
        workspace_path: "/tmp/ws".into(),
        prompt: "p".into(),
        pid: None,
        error: Some(error.into()),
        error_kind: None,
        tokens: None,
    }
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub engine: Engine,
    pub orchestrator: Orchestrator,
    pub adapter: Arc<ProbeAdapter>,
    pub clock: Arc<FixedClock>,
    pub paths: Paths,
    events: Arc<Mutex<Vec<OrchestratorEvent>>>,
    _recorder: orch_engine::events::Unsubscribe,
}

impl Rig {
    pub async fn new(spec: RigSpec) -> Rig {
        let dir = tempfile::tempdir().expect("tempdir");
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.root()).expect(".orchestry");
        FileConfigStore::new(paths.clone())
            .write_raw(&spec.raw_config, false)
            .await
            .expect("seed config.yml");
        Rig::assemble(dir, spec, true, None).await
    }

    /// Like [`Rig::new`] but dispatching to `adapter` (a real adapter over a
    /// fake process manager) instead of the scripted [`ProbeAdapter`].
    pub async fn new_with_adapter(spec: RigSpec, adapter: Arc<dyn Adapter>) -> Rig {
        let dir = tempfile::tempdir().expect("tempdir");
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.root()).expect(".orchestry");
        FileConfigStore::new(paths.clone())
            .write_raw(&spec.raw_config, false)
            .await
            .expect("seed config.yml");
        Rig::assemble(dir, spec, true, Some(adapter)).await
    }

    /// A process restart: a brand-new engine and orchestrator over the same
    /// project directory (stores are re-read from disk; nothing is re-seeded).
    pub async fn restart(self, config_store: bool) -> Rig {
        let Rig { dir, .. } = self;
        let spec = RigSpec {
            agents: Vec::new(),
            tasks: Vec::new(),
            config_store,
            ..Default::default()
        };
        Rig::assemble(dir, spec, false, None).await
    }

    async fn assemble(
        dir: tempfile::TempDir,
        spec: RigSpec,
        seed: bool,
        real: Option<Arc<dyn Adapter>>,
    ) -> Rig {
        let paths = Paths::new(dir.path());
        let clock = Arc::new(FixedClock::new());
        let engine = build_light_with(
            dir.path(),
            EngineSeams {
                clock: Arc::clone(&clock) as Arc<dyn Clock>,
                ids: Arc::new(SeqIdGenerator::new()),
                // Never the developer's real global config.
                global_config_path: Some(dir.path().join("global.yml")),
            },
        )
        .await
        .expect("build engine");

        if seed {
            for agent in &spec.agents {
                engine.agents.save(agent).await.expect("seed agent");
            }
            for task in &spec.tasks {
                engine.tasks.save(task).await.expect("seed task");
            }
        }

        let adapter = ProbeAdapter::new();
        let mut registry = AdapterRegistry::new();
        registry.register(real.unwrap_or_else(|| Arc::clone(&adapter) as Arc<dyn Adapter>));

        let processes: Arc<dyn orch_core::process::ProcessManager> =
            Arc::new(FakeProcessManager::new());
        let orchestrator = Orchestrator::new(OrchestratorDeps {
            task_store: Arc::clone(&engine.tasks),
            agent_store: Arc::clone(&engine.agents),
            run_store: Arc::clone(&engine.runs),
            state_store: Arc::clone(&engine.state),
            adapters: Arc::new(registry),
            workspace: Arc::new(FakeWorkspaceManager::new()),
            templates: Arc::new(LiquidTemplateEngine::new()),
            processes,
            events: Arc::clone(&engine.events),
            task_service: Arc::clone(&engine.task_service),
            agent_service: Arc::clone(&engine.agent_service),
            run_service: Arc::clone(&engine.run_service),
            clock: Arc::clone(&engine.clock),
            lock: Arc::new(InMemoryLock::new()),
            command_runner: Arc::new(PassingRunner),
            config: engine.config.clone(),
            project_root: dir.path().to_path_buf(),
            context_store: Some(Arc::clone(&engine.context)),
            message_service: Some(Arc::clone(&engine.message_service)),
            goal_store: Some(Arc::clone(&engine.goals)),
            skill_loader: (!spec.skills.is_empty()).then(|| {
                Arc::new(orch_core::test_support::FakeSkillLoader::with_skills(
                    spec.skills.clone(),
                )) as Arc<dyn orch_core::skills::SkillLoader>
            }),
        });
        if spec.config_store {
            orchestrator.set_config_store(Arc::clone(&engine.config_store));
        }

        let events: Arc<Mutex<Vec<OrchestratorEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let recorder = engine
            .events
            .on_any(move |event| sink.lock().unwrap().push(event.clone()));

        Rig {
            dir,
            engine,
            orchestrator,
            adapter,
            clock,
            paths,
            events,
            _recorder: recorder,
        }
    }

    pub fn config_store(&self) -> Arc<dyn ConfigStore> {
        Arc::clone(&self.engine.config_store)
    }

    pub fn events(&self) -> Vec<OrchestratorEvent> {
        self.events.lock().unwrap().clone()
    }

    /// One dispatch pass: everything dispatchable runs, collectors drain.
    pub async fn dispatch(&self) {
        let _ = self.orchestrator.run_all().await;
        self.adapter.release();
        self.orchestrator.wait_for_collectors().await;
    }

    pub async fn dispatch_result(&self) -> orch_core::error::Result<()> {
        let result = self.orchestrator.run_all().await;
        self.adapter.release();
        self.orchestrator.wait_for_collectors().await;
        result
    }

    pub async fn task(&self, id: &str) -> Task {
        self.engine.tasks.get(id).await.unwrap().expect("task")
    }

    pub async fn agent(&self, id: &str) -> Agent {
        self.engine.agents.get(id).await.unwrap().expect("agent")
    }

    pub async fn runs(&self) -> Vec<Run> {
        self.engine.runs.list_all().await.unwrap()
    }

    pub async fn reseed_task(&self, task: Task) {
        self.engine.tasks.save(&task).await.unwrap();
    }

    pub fn config_path(&self) -> PathBuf {
        self.paths.config_path()
    }

    pub fn config_bytes(&self) -> Vec<u8> {
        std::fs::read(self.config_path()).unwrap_or_default()
    }

    /// Every file under `.orchestry/agents` (definitions and the index),
    /// path → bytes.
    pub fn agents_snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        snapshot(&self.paths.agents_dir())
    }

    pub async fn set_project_mode_raw(&self, mode: PromptMode) {
        let store = FileConfigStore::new(self.paths.clone());
        let mut raw = store.read_raw().await.unwrap();
        orch_domain::set_by_path(
            &mut raw,
            "defaults.agent.prompt_mode",
            Value::String(mode.as_str().to_owned()),
        );
        store.write_raw(&raw, false).await.unwrap();
    }
}

pub fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(path, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

pub fn config_with(prompt_mode: Option<&str>, extra: Value) -> Value {
    let mut raw = extra;
    if let Some(mode) = prompt_mode {
        orch_domain::set_by_path(
            &mut raw,
            "defaults.agent.prompt_mode",
            Value::String(mode.to_owned()),
        );
    }
    raw
}

pub fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Running => "running",
        _ => "other",
    }
}

pub fn is_todo(task: &Task) -> bool {
    task.status == TaskStatus::Todo
}

pub fn lib_config() -> OrchestratorConfig {
    orch_domain::default_config()
}
