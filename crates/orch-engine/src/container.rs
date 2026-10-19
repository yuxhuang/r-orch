//! Dependency wiring.
//!
//! Two tiers: [`Engine`] (stores + services) for the read-only
//! commands and [`FullEngine`] (+ orchestrator, adapters, process manager,
//! template engine, skill loader, workspace manager, doctor) for `run`, `tui`,
//! `doctor` and `serve`.
//!
//! The split is kept for structural fidelity and because it documents which
//! commands may mutate state. **No lazy-initialization machinery is added**:
//! in Rust there are no dynamic imports to avoid, so each tier simply
//! constructs what it needs (ARCHITECTURE §7).

use std::path::PathBuf;
use std::sync::Arc;

use orch_core::adapter::AdapterRegistry;
use orch_core::clock::Clock;
use orch_core::error::Result;
use orch_core::ids::IdGenerator;
use orch_core::process::ProcessManager;
use orch_core::skills::SkillLoader;
use orch_core::storage::{
    AgentStore, ConfigStore, ContextStore, GlobalConfigStore, GoalStore, MessageStore, RunStore,
    StateStore, TaskStore, TeamStore,
};
use orch_core::template::TemplateEngine;
use orch_core::workspace::WorkspaceManager;
use orch_domain::{GlobalConfig, OrchestratorConfig};
use orch_infra::adapters::build_registry_with_clock;
use orch_infra::storage::{
    FileAgentStore, FileConfigStore, FileContextStore, FileGlobalConfigStore, FileGoalStore,
    FileMessageStore, FileRunStore, FileStateStore, FileTaskStore, FileTeamStore, Paths,
};
use orch_infra::template::LiquidTemplateEngine;
use orch_infra::workspace::GitWorkspaceManager;
use orch_infra::{NanoIdGenerator, SystemClock};

use crate::agent_service::AgentService;
use crate::doctor_service::DoctorService;
use crate::events::EventBus;
use crate::goal_service::GoalService;
use crate::lock::{FileLock, ProjectLock};
use crate::message_service::MessageService;
use crate::orchestrator::{Orchestrator, OrchestratorDeps};
use crate::review_runner::{CommandRunner, ProcessCommandRunner};
use crate::run_service::RunService;
use crate::task_service::TaskService;
use crate::team_service::TeamService;

/// Everything a read-only command needs.
pub struct Engine {
    pub paths: Paths,
    pub config: OrchestratorConfig,
    pub global_config: GlobalConfig,

    pub tasks: Arc<dyn TaskStore>,
    pub agents: Arc<dyn AgentStore>,
    pub runs: Arc<dyn RunStore>,
    pub state: Arc<dyn StateStore>,
    pub config_store: Arc<dyn ConfigStore>,
    pub global_config_store: Arc<dyn GlobalConfigStore>,
    pub context: Arc<dyn ContextStore>,
    pub messages: Arc<dyn MessageStore>,
    pub goals: Arc<dyn GoalStore>,
    pub teams: Arc<dyn TeamStore>,

    pub events: Arc<EventBus>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,

    pub task_service: Arc<TaskService>,
    pub agent_service: Arc<AgentService>,
    pub run_service: Arc<RunService>,
    pub message_service: Arc<MessageService>,
    pub goal_service: Arc<GoalService>,
    pub team_service: Arc<TeamService>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("paths", &self.paths)
            .field("project", &self.config.project.name)
            .finish_non_exhaustive()
    }
}

/// The full tier: everything in [`Engine`] plus the machinery that runs agents.
pub struct FullEngine {
    pub engine: Engine,
    pub processes: Arc<dyn ProcessManager>,
    pub adapters: Arc<AdapterRegistry>,
    pub workspace: Arc<dyn WorkspaceManager>,
    pub templates: Arc<dyn TemplateEngine>,
    pub skills: Arc<dyn SkillLoader>,
    pub lock: Arc<dyn ProjectLock>,
    pub doctor_service: Arc<DoctorService>,
    pub orchestrator: Arc<Orchestrator>,
}

impl std::fmt::Debug for FullEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FullEngine")
            .field("engine", &self.engine)
            .field("adapters", &self.adapters.kinds())
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for FullEngine {
    type Target = Engine;

    fn deref(&self) -> &Engine {
        &self.engine
    }
}

/// Seams a caller may replace. Production passes [`Default`]; tests pin the clock and the id
/// generator so output is exactly predictable (ARCHITECTURE §5.6).
pub struct EngineSeams {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    /// Where `global.yml` lives, for a test that must not touch the real one.
    ///
    /// `None` keeps the production resolution: `ORCHESTRY_GLOBAL_CONFIG_PATH`,
    /// else `~/.orchestry/global.yml`. That default is machine-global, so a
    /// test using it writes the developer's own preferences — which is not
    /// hypothetical. Two tests in `orch-cli` did exactly that, and the user's
    /// `~/.orchestry/global.yml` was found holding `activity_filter:
    /// nonsense`, a literal from one of them.
    ///
    /// An env var is **not** a sufficient fix: it was first set through
    /// `[env]` in `.cargo/config.toml`, which cargo only loads when the
    /// working directory is inside the workspace — so the writer came straight
    /// back for any invocation from elsewhere. Verified by running the suite
    /// with a cwd outside the repo, which reproduced the file byte for byte.
    /// A seam the test passes explicitly cannot be bypassed that way.
    pub global_config_path: Option<PathBuf>,
}

impl Default for EngineSeams {
    fn default() -> Self {
        EngineSeams {
            clock: Arc::new(SystemClock::new()),
            ids: Arc::new(NanoIdGenerator::new()),
            global_config_path: None,
        }
    }
}

/// Build the light tier. Fails with `NotInitializedError` (exit 3) when
/// `.orchestry/` is absent, which is why the check happens before anything
/// else is constructed.
pub async fn build_light(project_root: impl Into<PathBuf>) -> Result<Engine> {
    build_light_with(project_root, EngineSeams::default()).await
}

pub async fn build_light_with(
    project_root: impl Into<PathBuf>,
    seams: EngineSeams,
) -> Result<Engine> {
    let paths = Paths::new(project_root);
    paths.require_init().await?;

    let config_store: Arc<dyn ConfigStore> = Arc::new(FileConfigStore::new(paths.clone()));
    let global_config_store: Arc<dyn GlobalConfigStore> = match &seams.global_config_path {
        Some(path) => Arc::new(FileGlobalConfigStore::at(path.clone())),
        None => Arc::new(FileGlobalConfigStore::new()),
    };
    let mut config = config_store.read().await?;
    if config.peer.name.is_none() {
        config.peer.name = Some(orch_infra::storage::remote::default_peer_name());
    }
    // The light tier does not read `global.yml`: `globalConfig` stays at its
    // default until the full tier loads it, and no read-only command
    // consults it.
    let global_config = orch_domain::default_global_config();

    let tasks: Arc<dyn TaskStore> = Arc::new(FileTaskStore::new(paths.clone()));
    let agents: Arc<dyn AgentStore> = Arc::new(FileAgentStore::new(paths.clone()));
    let runs: Arc<dyn RunStore> = Arc::new(FileRunStore::new(paths.clone()));
    let state: Arc<dyn StateStore> = Arc::new(FileStateStore::new(paths.clone()));
    let context: Arc<dyn ContextStore> = Arc::new(FileContextStore::new(
        paths.clone(),
        Arc::clone(&seams.clock),
    ));
    let messages: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(
        paths.clone(),
        Arc::clone(&seams.clock),
    ));
    let goals: Arc<dyn GoalStore> = Arc::new(FileGoalStore::new(paths.clone()));
    let teams: Arc<dyn TeamStore> = Arc::new(FileTeamStore::new(paths.clone()));
    let events = Arc::new(EventBus::new());

    let task_service = Arc::new(
        TaskService::new(
            Arc::clone(&tasks),
            Arc::clone(&agents),
            Arc::clone(&events),
            config.clone(),
            Arc::clone(&seams.clock),
            Arc::clone(&seams.ids),
            Some(paths.clone()),
        )
        .with_remote_directory(orch_infra::storage::remote::RemoteAgentDirectory::new(
            paths.clone(),
        )),
    );
    let agent_service = Arc::new(AgentService::new(
        Arc::clone(&agents),
        Arc::clone(&state),
        Arc::clone(&events),
        config.clone(),
        Arc::clone(&seams.ids),
    ));
    let run_service = Arc::new(RunService::new(
        Arc::clone(&runs),
        Arc::clone(&events),
        Arc::clone(&seams.clock),
        Arc::clone(&seams.ids),
    ));
    let message_service = Arc::new(
        MessageService::new(
            Arc::clone(&messages),
            Arc::clone(&agents),
            Arc::clone(&teams),
            Arc::clone(&events),
            Arc::clone(&seams.clock),
            Arc::clone(&seams.ids),
        )
        .with_remote_directory(orch_infra::storage::remote::RemoteAgentDirectory::new(
            paths.clone(),
        )),
    );
    let goal_service = Arc::new(GoalService::new(
        Arc::clone(&goals),
        Arc::clone(&events),
        Arc::clone(&seams.clock),
        Arc::clone(&seams.ids),
        Some(Arc::clone(&agent_service)),
        Some(Arc::clone(&task_service)),
        Some(Arc::clone(&context)),
    ));
    let team_service = Arc::new(TeamService::new(
        Arc::clone(&teams),
        Arc::clone(&agents),
        Arc::clone(&tasks),
        Arc::clone(&events),
        Arc::clone(&seams.clock),
        Arc::clone(&seams.ids),
    ));

    Ok(Engine {
        paths,
        config,
        global_config,
        tasks,
        agents,
        runs,
        state,
        config_store,
        global_config_store,
        context,
        messages,
        goals,
        teams,
        events,
        clock: seams.clock,
        ids: seams.ids,
        task_service,
        agent_service,
        run_service,
        message_service,
        goal_service,
        team_service,
    })
}

/// Build the full tier. Reads `~/.orchestry/global.yml`, which the light tier
/// deliberately skips.
pub async fn build_full(project_root: impl Into<PathBuf>) -> Result<FullEngine> {
    build_full_with(project_root, EngineSeams::default()).await
}

pub async fn build_full_with(
    project_root: impl Into<PathBuf>,
    seams: EngineSeams,
) -> Result<FullEngine> {
    build_full_internal(
        project_root.into(),
        seams,
        orch_domain::PeerConfig::default(),
    )
    .await
}

pub async fn build_full_with_peer(
    project_root: impl Into<PathBuf>,
    peer: orch_domain::PeerConfig,
) -> Result<FullEngine> {
    build_full_internal(project_root.into(), EngineSeams::default(), peer).await
}

async fn build_full_internal(
    project_root: PathBuf,
    seams: EngineSeams,
    peer: orch_domain::PeerConfig,
) -> Result<FullEngine> {
    let mut engine = build_light_with(project_root.clone(), seams).await?;
    if peer.name.is_some() {
        engine.config.peer.name = peer.name;
    }
    if peer.listen.is_some() {
        engine.config.peer.listen = peer.listen;
    }
    engine.global_config = engine.global_config_store.read().await?;

    let processes: Arc<dyn ProcessManager> = Arc::new(orch_infra::process::SystemProcessManager);
    // Adapters stamp the events they parse out of a child's stdout, so they
    // get the injected clock too — otherwise the seam would reach every
    // timestamp except the ones the adapters actually read.
    let adapters = Arc::new(build_registry_with_clock(
        Arc::clone(&processes),
        Arc::clone(&engine.clock),
    ));
    let workspace: Arc<dyn WorkspaceManager> = Arc::new(GitWorkspaceManager::new(
        project_root.clone(),
        engine.paths.root(),
        Arc::clone(&processes),
    ));
    let templates: Arc<dyn TemplateEngine> = Arc::new(LiquidTemplateEngine::new());
    let skills: Arc<dyn SkillLoader> = Arc::new(orch_infra::skills::FileSkillLoader::new());
    let lock: Arc<dyn ProjectLock> = Arc::new(FileLock::new(engine.paths.lock_path()));
    let doctor_service = Arc::new(DoctorService::new(
        Arc::clone(&adapters),
        project_root.clone(),
    ));
    let command_runner: Arc<dyn CommandRunner> = Arc::new(ProcessCommandRunner);

    let orchestrator = Arc::new(Orchestrator::new(OrchestratorDeps {
        task_store: Arc::clone(&engine.tasks),
        agent_store: Arc::clone(&engine.agents),
        run_store: Arc::clone(&engine.runs),
        state_store: Arc::clone(&engine.state),
        adapters: Arc::clone(&adapters),
        workspace: Arc::clone(&workspace),
        templates: Arc::clone(&templates),
        processes: Arc::clone(&processes),
        events: Arc::clone(&engine.events),
        task_service: Arc::clone(&engine.task_service),
        agent_service: Arc::clone(&engine.agent_service),
        run_service: Arc::clone(&engine.run_service),
        clock: Arc::clone(&engine.clock),
        lock: Arc::clone(&lock),
        command_runner,
        config: engine.config.clone(),
        project_root,
        context_store: Some(Arc::clone(&engine.context)),
        message_service: Some(Arc::clone(&engine.message_service)),
        goal_store: Some(Arc::clone(&engine.goals)),
        skill_loader: Some(Arc::clone(&skills)),
    }));

    Ok(FullEngine {
        engine,
        processes,
        adapters,
        workspace,
        templates,
        skills,
        lock,
        doctor_service,
        orchestrator,
    })
}

/// Both tiers, and the orchestrator, must be movable and shareable across
/// threads.
///
/// This is load-bearing rather than hygiene: the TUI's facade is
/// **synchronous** while every service here is `async`, and `orch-cli` already
/// runs `dispatch` inside `Runtime::block_on`. So the draw loop has to live on
/// a thread that is *not* driving the runtime — `Handle::block_on` panics on
/// one that is — which means the engine gets moved onto a plain
/// `std::thread`. A stray `Rc` or `RefCell` anywhere in the container would
/// make that impossible, and the error would land on whoever wires the TUI
/// rather than on whoever introduced it. Hence a static assertion here.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<Engine>();
    assert_send_sync::<FullEngine>();
    assert_send_sync::<Orchestrator>();
    assert_send_sync::<EngineSeams>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::storage::{GoalFilter, TaskFilter};
    use orch_domain::{CreateAgentInput, CreateGoalInput, CreateTaskInput, CreateTeamInput};

    /// An initialized project: just the `.orchestry/` directory, which is
    /// what `require_init` checks for.
    fn initialized() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        std::fs::create_dir_all(Paths::new(temp.path()).root()).expect("create .orchestry");
        temp
    }

    fn deterministic_seams() -> EngineSeams {
        EngineSeams {
            clock: Arc::new(orch_core::test_support::FixedClock::at(0)),
            ids: Arc::new(orch_core::test_support::SeqIdGenerator::new()),
            // Left at the default here because these cases only build the
            // light tier, which does not construct the global-config store.
            // Anything reading or writing `global.yml` must set it.
            global_config_path: None,
        }
    }

    #[tokio::test]
    async fn build_light_fails_with_exit_3_in_an_uninitialized_directory() {
        let temp = tempfile::tempdir().unwrap();
        let err = build_light(temp.path()).await.expect_err("not initialized");
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.to_string(), "Not initialized");
    }

    #[tokio::test]
    async fn build_full_fails_the_same_way() {
        let temp = tempfile::tempdir().unwrap();
        let err = build_full(temp.path()).await.expect_err("not initialized");
        assert_eq!(err.exit_code(), 3);
    }

    #[tokio::test]
    async fn build_light_wires_services_against_the_real_stores() {
        let temp = initialized();

        let engine = build_light(temp.path()).await.unwrap();
        // Defaults came from the (absent) config file.
        assert_eq!(engine.config.project.name, "my-project");

        let task = engine
            .task_service
            .create(CreateTaskInput {
                title: "Do it".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(task.id.starts_with("tsk_"));

        // It went through the file store, so a fresh engine sees it.
        let engine = build_light(temp.path()).await.unwrap();
        let tasks = engine
            .task_service
            .list(&TaskFilter::default())
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Do it");
    }

    #[tokio::test]
    async fn every_light_service_shares_one_set_of_stores() {
        let temp = initialized();
        let engine = build_light(temp.path()).await.unwrap();

        let agent = engine
            .agent_service
            .create(CreateAgentInput {
                name: "shelly".into(),
                adapter: "shell".into(),
                command: Some("echo hi".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let task = engine
            .task_service
            .create(CreateTaskInput {
                title: "Do it".into(),
                assignee: Some(agent.name.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        // The task service resolved the name through the agent store the
        // agent service had just written.
        assert_eq!(task.assignee.as_deref(), Some(agent.id.as_str()));

        let team = engine
            .team_service
            .create(CreateTeamInput {
                name: "core".into(),
                lead_agent_id: agent.id.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(team.lead_agent_id, agent.id);
        engine
            .team_service
            .add_task(&team.id, &task.id)
            .await
            .unwrap();

        let goal = engine
            .goal_service
            .create(CreateGoalInput {
                title: "Ship".into(),
                assignee: Some(agent.id.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        // The goal's autonomous side effect reached the agent store.
        assert_eq!(
            engine
                .agents
                .get(&agent.id)
                .await
                .unwrap()
                .unwrap()
                .autonomous,
            Some(true)
        );

        let messages = engine
            .message_service
            .send(orch_domain::CreateMessageInput {
                channel: orch_domain::MessageChannel::Direct,
                from_agent_id: "cli".into(),
                to_agent_id: Some(agent.id.clone()),
                subject: "hi".into(),
                body: "there".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(messages.len(), 1);

        // And a fresh engine reads all of it back off disk.
        let reopened = build_light(temp.path()).await.unwrap();
        assert_eq!(reopened.team_service.list().await.unwrap().len(), 1);
        assert_eq!(
            reopened.goal_service.get(&goal.id).await.unwrap().title,
            "Ship"
        );
        assert_eq!(reopened.message_service.list_all().await.unwrap().len(), 1);
        assert_eq!(
            reopened
                .goal_service
                .list(&GoalFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn seams_are_injectable_so_ids_and_timestamps_are_predictable() {
        let temp = initialized();
        let engine = build_light_with(temp.path(), deterministic_seams())
            .await
            .unwrap();

        let task = engine
            .task_service
            .create(CreateTaskInput {
                title: "t".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(task.id, "tsk_0000001");
        assert_eq!(task.created_at, "1970-01-01T00:00:00.000Z");
    }

    #[tokio::test]
    async fn build_full_registers_the_eight_adapters_in_contract_order() {
        let temp = initialized();
        let full = build_full(temp.path()).await.unwrap();

        assert_eq!(
            full.adapters.kinds(),
            orch_infra::adapters::REGISTRATION_ORDER.to_vec()
        );
    }

    #[tokio::test]
    async fn build_full_shares_one_event_bus_with_the_light_tier() {
        let temp = initialized();
        let full = build_full(temp.path()).await.unwrap();

        let mut sub = full.events.subscribe();
        full.task_service
            .create(CreateTaskInput {
                title: "Do it".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(sub.try_drain().len(), 1);
    }

    #[tokio::test]
    async fn build_full_locks_over_the_projects_own_lock_file() {
        let temp = initialized();
        let full = build_full(temp.path()).await.unwrap();

        assert!(!full.orchestrator.is_owner());
        assert!(full.lock.acquire().await.acquired);
        assert!(full.paths.lock_path().exists());
        full.lock.release().await;
        assert!(!full.paths.lock_path().exists());
    }

    #[tokio::test]
    async fn the_full_tier_derefs_to_the_light_one() {
        let temp = initialized();
        let full = build_full(temp.path()).await.unwrap();
        // `full.config` reaches `full.engine.config`, so a caller that only
        // needs the light tier does not have to know about the split.
        assert_eq!(full.config.project.name, "my-project");
        assert_eq!(full.paths.root(), Paths::new(temp.path()).root());
    }
}
