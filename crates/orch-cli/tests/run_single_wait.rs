//! Does `run <id>` terminate when the run finishes before the drain?
//!
//! Scratch test written by WP-4 while reviewing `commands/run.rs`. Not
//! intended to be merged as-is — it is the reproduction for a review finding.

use orch_core::storage::ConfigStore;
use orch_domain::{CreateAgentInput, CreateTaskInput, WorkspaceMode, default_config};
use orch_infra::storage::{FileConfigStore, Paths};

#[tokio::test(flavor = "multi_thread")]
async fn run_single_returns_when_the_agent_fails_immediately() {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.root())
        .await
        .expect("create .orchestry");

    let mut config = default_config();
    config.project.name = "run-wait".into();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    // No watch loop in `run <id>`; a long poll keeps anything else out of it.
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    let engine = orch_engine::build_full(temp.path())
        .await
        .expect("build_full");

    // A shell agent whose command cannot exist: the spawn fails, so the
    // collector emits `agent:completed` almost immediately — while
    // `fresh_dispatch_only` is still in its trailing `save_state` write.
    let agent = engine
        .agent_service
        .create(CreateAgentInput {
            name: "doomed".into(),
            adapter: "shell".into(),
            command: Some("/nonexistent/orch-review-binary".into()),
            ..Default::default()
        })
        .await
        .expect("create agent");
    let task = engine
        .task_service
        .create(CreateTaskInput {
            title: "fails at once".into(),
            assignee: Some(agent.id.clone()),
            ..Default::default()
        })
        .await
        .expect("create task");

    let styles = orch_cli::output::Styles::plain();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        orch_cli::commands::run::single(&engine, &styles, &task.id),
    )
    .await;

    let code = outcome.expect("`run <id>` never returned: it is waiting for a completion it already consumed");
    code.expect("run failed");

    // It really dispatched: without this the test could pass by doing nothing.
    let runs = engine.run_service.list_all().await.expect("list runs");
    assert_eq!(runs.len(), 1, "expected one real run");

    let _ = engine.orchestrator.stop().await;
    drop(temp);
}
