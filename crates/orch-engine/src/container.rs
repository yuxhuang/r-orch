//! Dependency wiring — ported from `src/container.ts`.
//!
//! The TS code has two tiers: `LightContainer` (stores + services) for
//! read-only commands and `Container` (+ orchestrator, adapters, template
//! engine) for `run`/`tui`/`doctor`/`serve`. The split is kept for structural
//! fidelity and because it documents which commands may mutate state, but
//! **no lazy-initialization machinery** is added: in Rust there are no dynamic
//! imports to avoid, so each tier simply constructs what it needs
//! (ARCHITECTURE §7).
//!
//! WP-0b builds the light tier with the four stores the walking skeleton
//! needs. WP-4 adds the remaining stores and services and `build_full`.

use std::path::PathBuf;
use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::error::Result;
use orch_core::ids::IdGenerator;
use orch_core::storage::{AgentStore, ConfigStore, StateStore, TaskStore};
use orch_domain::OrchestratorConfig;
use orch_infra::storage::{FileAgentStore, FileConfigStore, FileStateStore, FileTaskStore, Paths};
use orch_infra::{NanoIdGenerator, SystemClock};

use crate::agent_service::AgentService;
use crate::events::EventBus;
use crate::task_service::TaskService;

/// Everything a read-only command needs.
pub struct Engine {
    pub paths: Paths,
    pub config: OrchestratorConfig,
    pub tasks: Arc<dyn TaskStore>,
    pub agents: Arc<dyn AgentStore>,
    pub state: Arc<dyn StateStore>,
    pub config_store: Arc<dyn ConfigStore>,
    pub events: Arc<EventBus>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    pub task_service: Arc<TaskService>,
    pub agent_service: Arc<AgentService>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("paths", &self.paths)
            .field("project", &self.config.project.name)
            .finish_non_exhaustive()
    }
}

/// Seams a caller may replace. Production passes [`Default`]; tests and the
/// parity harness pin the clock and the id generator so output is exactly
/// predictable (ARCHITECTURE §5.6).
pub struct EngineSeams {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

impl Default for EngineSeams {
    fn default() -> Self {
        EngineSeams {
            clock: Arc::new(SystemClock::new()),
            ids: Arc::new(NanoIdGenerator::new()),
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

    let config_store = Arc::new(FileConfigStore::new(paths.clone()));
    let config = config_store.read().await?;

    let tasks: Arc<dyn TaskStore> = Arc::new(FileTaskStore::new(paths.clone()));
    let agents: Arc<dyn AgentStore> = Arc::new(FileAgentStore::new(paths.clone()));
    let state: Arc<dyn StateStore> = Arc::new(FileStateStore::new(paths.clone()));
    let events = Arc::new(EventBus::new());

    let task_service = Arc::new(TaskService::new(
        Arc::clone(&tasks),
        Arc::clone(&agents),
        Arc::clone(&events),
        config.clone(),
        Arc::clone(&seams.clock),
        Arc::clone(&seams.ids),
    ));
    let agent_service = Arc::new(AgentService::new(
        Arc::clone(&agents),
        config.clone(),
        Arc::clone(&seams.ids),
    ));

    Ok(Engine {
        paths,
        config,
        tasks,
        agents,
        state,
        config_store,
        events,
        clock: seams.clock,
        ids: seams.ids,
        task_service,
        agent_service,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::storage::TaskFilter;
    use orch_domain::CreateTaskInput;

    #[tokio::test]
    async fn build_light_fails_with_exit_3_in_an_uninitialized_directory() {
        let temp = tempfile::tempdir().unwrap();
        let err = build_light(temp.path()).await.expect_err("not initialized");
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.to_string(), "Not initialized");
    }

    #[tokio::test]
    async fn build_light_wires_services_against_the_real_stores() {
        let temp = tempfile::tempdir().unwrap();
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        std::fs::create_dir_all(Paths::new(temp.path()).root()).unwrap();

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
    async fn seams_are_injectable_so_ids_and_timestamps_are_predictable() {
        let temp = tempfile::tempdir().unwrap();
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        std::fs::create_dir_all(Paths::new(temp.path()).root()).unwrap();

        let engine = build_light_with(
            temp.path(),
            EngineSeams {
                clock: Arc::new(orch_core::test_support::FixedClock::at(0)),
                ids: Arc::new(orch_core::test_support::SeqIdGenerator::new()),
            },
        )
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
}
