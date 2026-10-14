//! `run <id>` must terminate when the target run's completion is already in
//! the batch the dispatch buffered.
//!
//! `single` drains those buffered events, then waits for `agent:completed` if
//! a run started. If the completion is *in* the drained batch and the drain
//! ignores that, the wait blocks on a second completion that never arrives and
//! the command hangs after printing its result.

use std::sync::Arc;

use orch_core::storage::ConfigStore;
use orch_domain::{
    CreateAgentInput, CreateTaskInput, OrchestratorEvent, OrchestratorEventType, WorkspaceMode,
    default_config,
};
use orch_infra::storage::{FileConfigStore, Paths};

async fn project() -> (tempfile::TempDir, orch_engine::FullEngine) {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.root())
        .await
        .expect("create .orchestry");

    let mut config = default_config();
    config.project.name = "run-wait".into();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    // There is no watch loop in `run <id>`; a long poll keeps anything else
    // from dispatching behind the test's back.
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    let engine = orch_engine::build_full_with(
        temp.path(),
        orch_engine::EngineSeams {
            // Never the developer's real `~/.orchestry/global.yml`.
            global_config_path: Some(paths.root().join("global.yml")),
            ..Default::default()
        },
    )
    .await
    .expect("build_full");
    (temp, engine)
}

/// A todo task assigned to a shell agent whose command fails at once.
async fn doomed_task(engine: &orch_engine::FullEngine) -> String {
    let agent = engine
        .agent_service
        .create(CreateAgentInput {
            name: "doomed".into(),
            adapter: "shell".into(),
            command: Some("exit 7".into()),
            ..Default::default()
        })
        .await
        .expect("create agent");
    engine
        .task_service
        .create(CreateTaskInput {
            title: "fails at once".into(),
            assignee: Some(agent.id),
            ..Default::default()
        })
        .await
        .expect("create task")
        .id
}

/// The ordering that hangs, forced deterministically.
///
/// Reproducing it by timing alone is luck: the collector needs ~15 ms (spawn,
/// exit, collect) where the dispatch's trailing `save_state` is ~1 ms, so the
/// real completion lands *after* the drain on an unloaded machine and the bug
/// stays hidden. A slower state write, a loaded machine or an adapter that
/// fails before spawning all flip it.
///
/// So the ordering is forced instead: a handler reacts to the real
/// `agent:started` by emitting a *second* start for the same task with a
/// different run id, immediately followed by that run's completion. The bus
/// sends on the channel before dispatching handlers, so the drained batch is
/// exactly `started(real)`, `started(synth)`, `completed(synth)` — the shape
/// where the completion is already consumed. The real run's own completion
/// arrives later with the *real* run id, so nothing else can end the wait.
#[tokio::test(flavor = "multi_thread")]
async fn a_completion_already_in_the_drained_batch_ends_the_wait() {
    let (_temp, engine) = project().await;
    let task_id = doomed_task(&engine).await;

    let bus = Arc::clone(&engine.events);
    let _synth = engine.events.on(OrchestratorEventType::AgentStarted, {
        let bus = Arc::clone(&bus);
        move |event| {
            let OrchestratorEvent::AgentStarted {
                task_id,
                agent_id,
                run_id,
            } = event
            else {
                return;
            };
            // Only react to the *real* start: the synthetic one re-enters this
            // handler, and reacting to it recurses until the stack is gone.
            if run_id != "run_synthetic" {
                bus.emit(OrchestratorEvent::AgentStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    run_id: "run_synthetic".to_owned(),
                });
                bus.emit(OrchestratorEvent::AgentCompleted {
                    run_id: "run_synthetic".to_owned(),
                    agent_id: agent_id.clone(),
                    success: true,
                });
            }
        }
    });

    let styles = orch_cli::output::Styles::plain();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        orch_cli::commands::run::single(&engine, &styles, &task_id),
    )
    .await;

    outcome
        .expect("`run <id>` hung: it is waiting for a completion it already consumed")
        .expect("run failed");

    let _ = engine.orchestrator.stop().await;
}

/// The ordinary path, as a guard against the fix breaking it: the completion
/// arrives *after* the drain, so the wait is entered and ended normally.
#[tokio::test(flavor = "multi_thread")]
async fn the_wait_still_ends_on_a_completion_that_arrives_later() {
    let (_temp, engine) = project().await;
    let task_id = doomed_task(&engine).await;

    let styles = orch_cli::output::Styles::plain();
    let code = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        orch_cli::commands::run::single(&engine, &styles, &task_id),
    )
    .await
    .expect("`run <id>` never returned")
    .expect("run failed");

    // `exit 7` is a failed run, which is exit 1 — so the stream really was
    // followed to its end rather than the command returning early.
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", std::process::ExitCode::from(1))
    );
    let runs = engine.run_service.list_all().await.expect("list runs");
    assert_eq!(runs.len(), 1, "expected one real run");

    let _ = engine.orchestrator.stop().await;
}
