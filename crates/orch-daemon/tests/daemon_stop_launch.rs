#![cfg(feature = "testing")]
//! Stop wins before the final launch boundary; its retry survives a restart.
mod support;

use async_trait::async_trait;
use orch_core::process::{SpawnAdmission, SpawnReservation};
use orch_daemon::testing::DaemonHarness;
use orch_domain::{OrchestratorEventType, RetryEntry, RunStatus, TaskStatus};
use orch_infra::process::gate::ProcessSpawnAdmission;
use serde_json::json;
use std::sync::{Arc, Mutex};
use support::{DEADLINE, Raw, ok, shell_agent, task};
use tokio::sync::Notify;

#[derive(Default)]
struct HeldAdmission {
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl SpawnAdmission for HeldAdmission {
    async fn reserve(&self) -> Box<dyn SpawnReservation> {
        let reservation = ProcessSpawnAdmission.reserve().await;
        self.entered.notify_one();
        self.release.notified().await;
        reservation
    }
}

async fn stop_launch(explicit: bool, max_attempts: u32) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".orchestry")).unwrap();
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .start_in(root.path())
        .await
        .unwrap();
    let engine = daemon.engine().orchestrator.clone();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent_id = shell_agent(&mut raw, &daemon, "stop-race", "true").await;
    let id = task(
        &mut raw,
        "preserve this attempt",
        json!({"assignee": agent_id, "max_attempts": max_attempts}),
    )
    .await;
    let dependent = task(&mut raw, "dependent", json!({"depends_on": [id]})).await;
    let full = daemon.engine();
    let boundary = full.operation_boundary();
    let (before, agent, queue) = boundary
        .run(false, async {
            let mut before = full.tasks.get(&id).await.unwrap().unwrap();
            if max_attempts > 1 {
                before.status = TaskStatus::Retrying;
                before.attempts = 1;
                full.tasks.save(&before).await.unwrap();
                let mut state = full.state.read().await.unwrap();
                state.retry_queue.push(RetryEntry {
                    task_id: id.clone(),
                    attempt: 1,
                    due_at: "2099-01-01T00:00:00.000Z".into(),
                    error: "previous failure".into(),
                });
                full.state.write(&state).await.unwrap();
            }
            (
                before,
                full.agents.get(&agent_id).await.unwrap().unwrap(),
                full.state.read().await.unwrap().retry_queue,
            )
        })
        .await
        .value;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = errors.clone();
    let _subscription = full
        .events
        .on(OrchestratorEventType::OrchestratorError, move |event| {
            sink.lock().unwrap().push(event.clone());
        });
    let admission = Arc::new(HeldAdmission::default());
    engine.set_spawn_admission(admission.clone());
    let launch = engine.daemon_launch_task(&id);
    tokio::pin!(launch);
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch missed admission: {result:?}"),
        () = admission.entered.notified() => (),
    }
    if explicit {
        let reply = raw.request(2_000_000, "daemon.stop", json!({})).await;
        assert_eq!(ok(&reply)["clean"], true, "{reply}");
    }
    drop(raw);
    daemon.shutdown().await.unwrap();
    admission.release.notify_one();
    launch.await.unwrap();
    assert!(
        errors.lock().unwrap().is_empty(),
        "{:?}",
        errors.lock().unwrap()
    );

    let successor = DaemonHarness::builder()
        .maintenance_timer(false)
        .start_in(root.path())
        .await
        .unwrap();
    let full = successor.engine();
    assert_eq!(full.tasks.get(&id).await.unwrap().unwrap(), before);
    assert_eq!(full.agents.get(&agent_id).await.unwrap().unwrap(), agent);
    assert_eq!(full.state.read().await.unwrap().retry_queue, queue);
    assert_eq!(
        full.tasks.get(&dependent).await.unwrap().unwrap().status,
        TaskStatus::Todo
    );
    let runs = full.runs.list_for_task(&id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Cancelled);
    assert!(full.state.read().await.unwrap().claimed.is_empty());
    full.orchestrator.daemon_launch_task(&id).await.unwrap();
    assert_eq!(
        full.tasks.get(&id).await.unwrap().unwrap().attempts,
        before.attempts + 1
    );
    assert!(
        full.runs
            .list_for_task(&id)
            .await
            .unwrap()
            .iter()
            .any(|run| run.attempt == before.attempts + 1 && run.id != runs[0].id)
    );
    successor.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_stop_preserves_retry_until_restart() {
    tokio::time::timeout(DEADLINE, stop_launch(true, 3))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_stop_preserves_last_attempt_and_dependent() {
    tokio::time::timeout(DEADLINE, stop_launch(true, 1))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signal_shutdown_preserves_retry_until_restart() {
    tokio::time::timeout(DEADLINE, stop_launch(false, 3))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signal_shutdown_preserves_last_attempt_and_dependent() {
    tokio::time::timeout(DEADLINE, stop_launch(false, 1))
        .await
        .unwrap();
}
