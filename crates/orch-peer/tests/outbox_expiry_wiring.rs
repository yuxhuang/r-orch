use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orch_domain::{Message, MessageChannel, MessageStatus};
use orch_engine::{EngineSeams, WatchOptions, build_full_with};
use orch_infra::storage::Paths;
use orch_peer::PeerRuntime;

#[test]
fn owner_start_prunes_expired_outbox_without_connections_or_listener() {
    // Set HOME only on a child test process, never on cargo or sibling tests.
    if std::env::var_os("OUTBOX_EXPIRY_WIRING_CHILD").is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_owner_start());
        return;
    }

    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let empty_path = sandbox.path().join("bin");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&empty_path).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "owner_start_prunes_expired_outbox_without_connections_or_listener",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &empty_path)
        .env("OUTBOX_EXPIRY_WIRING_CHILD", "1")
        .current_dir(sandbox.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("isolated owner test exceeded 10s: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "isolated owner test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

async fn check_owner_start() {
    let project = tempfile::tempdir().unwrap();
    let paths = Paths::new(project.path());
    tokio::fs::create_dir_all(paths.root()).await.unwrap();
    let engine = Arc::new(
        build_full_with(
            project.path(),
            EngineSeams {
                global_config_path: Some(paths.root().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    assert!(engine.config.peer.listen.is_none());
    assert!(engine.agents.list().await.unwrap().is_empty());
    let message = Message {
        id: "msg_expired_offline".into(),
        channel: MessageChannel::Direct,
        from_agent_id: "cli".into(),
        to_agent_id: Some("worker@never-connected".into()),
        subject: "offline expiry".into(),
        body: "expired before owner startup".into(),
        reply_to: None,
        created_at: "2000-01-01T00:00:00.000Z".into(),
        expires_at: Some("2000-01-01T00:00:01.000Z".into()),
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
    };
    engine.messages.save(&message).await.unwrap();
    let message_path = paths.message_path(&message.id).unwrap();
    let index_path = paths.messages_dir().join("_index.json");
    assert!(message_path.exists());
    let before: Vec<Message> =
        serde_json::from_slice(&tokio::fs::read(&index_path).await.unwrap()).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].id, message.id);

    let mut peers = None;
    engine
        .orchestrator
        .start_watch_with(
            WatchOptions {
                skip_autonomous_seeding: true,
                handle_signals: false,
            },
            || async {
                peers = Some(PeerRuntime::start(engine.clone()).await?);
                Ok(())
            },
        )
        .await
        .unwrap();
    // Observe disk directly: MessageStore::list can rebuild an index, hiding
    // a missing index update. No remote is configured to trigger outbox().
    let pruned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let index: Vec<Message> =
                serde_json::from_slice(&tokio::fs::read(&index_path).await.unwrap()).unwrap();
            if !message_path.exists() && index.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    engine.orchestrator.stop().await.unwrap();
    drop(peers);
    assert!(!engine.orchestrator.is_owner());
    assert!(
        pruned.is_ok(),
        "owner startup did not remove the expired message file and index entry within 5s"
    );
}
