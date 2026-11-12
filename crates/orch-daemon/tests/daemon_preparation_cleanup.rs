#![cfg(feature = "testing")]
//! File-store recovery after both release attempts fail, through daemon ticks.

mod support;

use orch_daemon::testing::DaemonHarness;
use orch_domain::OrchestratorState;
use serde_json::json;
use support::{Raw, TestClock, ok, task};

fn state(daemon: &DaemonHarness) -> OrchestratorState {
    serde_json::from_slice(
        &std::fs::read(daemon.project_root().join(".orchestry/state.json")).unwrap(),
    )
    .unwrap()
}

async fn candidate(raw: &mut Raw, daemon: &DaemonHarness) -> String {
    // The private project is not a git repository: workspace preparation
    // fails before an agent process can be started.
    let reply = raw
        .request(
            1_100,
            "agent.create",
            json!({
                "name": "worktree-agent", "adapter": "shell", "command": "exit 0",
                "workspace_mode": "worktree",
                "env": [
                    {"name": "HOME", "value": daemon.project_root().to_str().unwrap()},
                    {"name": "PATH", "value": "/usr/bin:/bin"},
                ],
            }),
        )
        .await;
    let agent = ok(&reply)["agent"]["id"].as_str().unwrap();
    task(raw, "cannot prepare workspace", json!({"assignee": agent})).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_recovery_releases_the_claim_without_repeating_the_attempt() {
    let clock = TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let id = candidate(&mut raw, &daemon).await;
    let runs = daemon.project_root().join(".orchestry/runs");
    std::fs::create_dir_all(&runs).unwrap();
    let broken = runs.join("unreadable.json");
    std::fs::write(&broken, b"invalid json").unwrap();

    // Both the recorder and its error-path release read the same broken
    // store. The claim and back-off are visible after the launch returns.
    daemon.step_dispatch().await.unwrap();
    let before = state(&daemon);
    assert!(before.claimed.contains(&id));
    assert_eq!(before.retry_queue.len(), 1);
    let reply = raw.request(1_101, "task.get", json!({"id": id})).await;
    assert_eq!(ok(&reply)["attempts"], 1);

    std::fs::remove_file(broken).unwrap();
    daemon.step_dispatch().await.unwrap();
    let after = state(&daemon);
    assert!(
        after.claimed.is_empty(),
        "the release has no reconciliation owner"
    );
    assert_eq!(after.retry_queue, before.retry_queue);
    assert!(after.running.is_empty());
    let reply = raw.request(1_102, "task.get", json!({"id": id})).await;
    assert_eq!(ok(&reply)["attempts"], 1);
    clock.advance_ms(10_001);
    daemon.step_dispatch().await.unwrap();
    let reply = raw.request(1_103, "task.get", json!({"id": id})).await;
    assert_eq!(ok(&reply)["attempts"], 2);
    daemon.shutdown().await.unwrap();
}
