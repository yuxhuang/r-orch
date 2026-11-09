//! Required recovery writes must gate readiness and leave a restartable image.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orch_core::test_support::{FakeProcessManager, make_agent, make_run, make_task};
use orch_domain::{AgentStatus, OrchestratorState, RunStatus, RunningEntry, TaskStatus};
use orch_engine::container::{EngineSeams, FullEngine, build_full_with_processes};

const TASK: &str = "tsk_recover";
const AGENT: &str = "agt_recover";
const RUN: &str = "run_recover";

async fn engine(root: &Path) -> FullEngine {
    let processes = Arc::new(FakeProcessManager::new());
    processes.set_all_dead(true);
    build_full_with_processes(
        root,
        EngineSeams {
            global_config_path: Some(root.join("global.yml")),
            ..Default::default()
        },
        Default::default(),
        processes,
    )
    .await
    .unwrap()
}

/// Restore even when the readiness assertion fails, so temporary state is removed.
struct ReadOnlyDir(PathBuf);

impl ReadOnlyDir {
    fn new(path: PathBuf) -> Self {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        Self(path)
    }
}

impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

async fn fixture(preparing: bool) -> (tempfile::TempDir, FullEngine) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
    let full = engine(temp.path()).await;
    let mut task = make_task();
    task.id = TASK.into();
    task.status = if preparing {
        TaskStatus::Todo
    } else {
        TaskStatus::InProgress
    };
    task.assignee = Some(AGENT.into());
    full.tasks.save(&task).await.unwrap();
    let mut agent = make_agent();
    agent.id = AGENT.into();
    agent.status = if preparing {
        AgentStatus::Idle
    } else {
        AgentStatus::Running
    };
    full.agents.save(&agent).await.unwrap();
    let mut run = make_run();
    run.id = RUN.into();
    run.task_id = TASK.into();
    run.agent_id = AGENT.into();
    run.status = if preparing {
        RunStatus::Preparing
    } else {
        RunStatus::Running
    };
    run.pid = (!preparing).then_some(24601);
    full.runs.save(&run).await.unwrap();
    let mut state = OrchestratorState::default();
    state.claimed.insert(TASK.into());
    if !preparing {
        state.running.insert(
            TASK.into(),
            RunningEntry {
                task_id: TASK.into(),
                agent_id: AGENT.into(),
                run_id: RUN.into(),
                pid: 24601,
                started_at: run.started_at.clone(),
                last_event_at: run.started_at,
                remote: None,
            },
        );
    }
    full.state.write(&state).await.unwrap();
    (temp, full)
}

#[tokio::test]
async fn orphan_preparing_write_failure_blocks_readiness_then_explicit_restart_finishes_it() {
    let (temp, full) = fixture(true).await;
    let fault = ReadOnlyDir::new(full.paths.runs_dir());

    let result = full.daemon_initialize().await;
    assert!(
        result.is_err(),
        "readiness accepted a failed preparing-run finish"
    );
    assert_eq!(
        full.runs.get(RUN).await.unwrap().unwrap().status,
        RunStatus::Preparing
    );
    assert!(full.state.read().await.unwrap().claimed.contains(TASK));
    drop(full);
    drop(fault);

    // Repair alone does not recover anything; an explicit new initialization does.
    let restarted = engine(temp.path()).await;
    assert_eq!(
        restarted.runs.get(RUN).await.unwrap().unwrap().status,
        RunStatus::Preparing
    );
    restarted.daemon_initialize().await.unwrap();
    assert_eq!(
        restarted.runs.get(RUN).await.unwrap().unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(
        restarted.tasks.get(TASK).await.unwrap().unwrap().status,
        TaskStatus::Todo
    );
    assert_eq!(
        restarted.agents.get(AGENT).await.unwrap().unwrap().status,
        AgentStatus::Idle
    );
    assert!(restarted.state.read().await.unwrap().claimed.is_empty());
    restarted.orchestrator.daemon_stop().await.unwrap();
}

async fn dead_local_write_failure(directory: &str) {
    let (temp, full) = fixture(false).await;
    let fault = ReadOnlyDir::new(full.paths.root().join(directory));

    let result = full.daemon_initialize().await;
    assert!(
        result.is_err(),
        "readiness accepted a failed {directory} write"
    );
    let state = full.state.read().await.unwrap();
    assert!(
        state.running.contains_key(TASK),
        "lost the reservation needed to retry {directory} recovery"
    );
    assert!(state.claimed.contains(TASK));
    drop(full);
    drop(fault);

    let restarted = engine(temp.path()).await;
    assert!(
        restarted
            .state
            .read()
            .await
            .unwrap()
            .running
            .contains_key(TASK)
    );
    restarted.daemon_initialize().await.unwrap();
    assert_eq!(
        restarted.tasks.get(TASK).await.unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    assert_eq!(
        restarted.runs.get(RUN).await.unwrap().unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(
        restarted.agents.get(AGENT).await.unwrap().unwrap().status,
        AgentStatus::Idle
    );
    let state = restarted.state.read().await.unwrap();
    assert!(state.running.is_empty());
    assert!(state.claimed.is_empty());
    assert!(state.retry_queue.is_empty());
    restarted.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn dead_local_task_write_failure_blocks_readiness_and_preserves_retry_evidence() {
    dead_local_write_failure("tasks").await;
}

#[tokio::test]
async fn dead_local_run_write_failure_blocks_readiness_and_preserves_retry_evidence() {
    dead_local_write_failure("runs").await;
}

#[tokio::test]
async fn dead_local_agent_write_failure_blocks_readiness_and_preserves_retry_evidence() {
    dead_local_write_failure("agents").await;
}

#[tokio::test]
async fn failed_task_index_write_is_not_hidden_by_a_cancelled_entity_on_retry() {
    let (temp, full) = fixture(false).await;
    let index = full.paths.tasks_dir().join("_index.json");
    let saved_index = index.with_extension("saved");
    std::fs::rename(&index, &saved_index).unwrap();
    std::fs::create_dir(&index).unwrap();

    let result = full.daemon_initialize().await;
    assert!(
        result.is_err(),
        "readiness accepted a failed task index write"
    );
    assert_eq!(
        full.tasks.get(TASK).await.unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    assert!(full.state.read().await.unwrap().running.contains_key(TASK));
    drop(full);
    std::fs::remove_dir(&index).unwrap();
    std::fs::rename(&saved_index, &index).unwrap();

    let restarted = engine(temp.path()).await;
    assert_eq!(
        restarted.tasks.list(&Default::default()).await.unwrap()[0].status,
        TaskStatus::InProgress
    );
    restarted.daemon_initialize().await.unwrap();
    assert_eq!(
        restarted.tasks.list(&Default::default()).await.unwrap()[0].status,
        TaskStatus::Cancelled
    );
    assert!(restarted.state.read().await.unwrap().running.is_empty());
    restarted.orchestrator.daemon_stop().await.unwrap();
}
