#![cfg(feature = "testing")]
//! Contracts retained when the local store/execution frontends are removed.

#[tokio::test]
async fn offline_init_creates_files_without_starting_a_daemon() {
    let root = tempfile::tempdir().unwrap();
    let outcome = orch_daemon::initialize(orch_daemon::InitOptions {
        project_root: root.path().to_owned(),
        adapter: Some("shell".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(!outcome.already_initialized);
    assert!(root.path().join(".orchestry/config.yml").is_file());
    for runtime in ["state.json", "daemon.json", "run/daemon.sock"] {
        assert!(
            !root.path().join(".orchestry").join(runtime).exists(),
            "{runtime}"
        );
    }
}

mod support;

use orch_daemon::testing::DaemonHarness;
use orch_domain::{OrchestratorEvent, TaskStatus};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use support::{Raw, dispatch_state, ok, task};

const WAIT: Duration = Duration::from_secs(5);

async fn daemon() -> DaemonHarness {
    DaemonHarness::builder()
        .maintenance_timer(false)
        .start()
        .await
        .unwrap()
}

async fn assert_daemon_owns_project(daemon: &DaemonHarness) {
    let contender = DaemonHarness::builder()
        .maintenance_timer(false)
        .start_in(daemon.project_root())
        .await
        .err();
    assert!(
        matches!(contender, Some(orch_daemon::DaemonError::AlreadyRunning(_))),
        "{contender:?}"
    );
    let mut raw = Raw::session(daemon, "cli", "normal").await;
    ok(&raw.request(1, "daemon.status", json!({})).await);
}

async fn terminal_batch(statuses: &[TaskStatus]) -> (DaemonHarness, Value) {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    for status in statuses {
        let id = task(&mut raw, "terminal fixture", json!({})).await;
        let mut seed = daemon.engine().tasks.get(&id).await.unwrap().unwrap();
        seed.status = *status;
        daemon.engine().tasks.save(&seed).await.unwrap();
    }
    let start = std::time::Instant::now();
    let reply = tokio::time::timeout(WAIT, raw.request(1, "dispatch.until_terminal", json!({})))
        .await
        .expect("terminal work must not wait for the configured tick interval");
    if statuses.is_empty() {
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    assert_eq!(dispatch_state(&mut raw).await, (false, false));
    assert_daemon_owns_project(&daemon).await;
    assert_eq!(
        daemon
            .engine()
            .tasks
            .list(&Default::default())
            .await
            .unwrap()
            .len(),
        statuses.len(),
        "a batch must not seed autonomous tasks"
    );
    (daemon, reply)
}

#[tokio::test]
async fn all_terminal_tasks_are_all_done() {
    let (daemon, reply) = terminal_batch(&[TaskStatus::Done]).await;
    assert_eq!(ok(&reply), "all_done");
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn one_failed_task_is_has_failed() {
    let (daemon, reply) = terminal_batch(&[TaskStatus::Done, TaskStatus::Failed]).await;
    assert_eq!(ok(&reply), "has_failed");
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_cancelled_task_is_still_all_done() {
    let (daemon, reply) = terminal_batch(&[TaskStatus::Done, TaskStatus::Cancelled]).await;
    assert_eq!(ok(&reply), "all_done");
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_empty_project_exits_immediately() {
    let (daemon, reply) = terminal_batch(&[]).await;
    assert_eq!(ok(&reply), "all_done");
    daemon.shutdown().await.unwrap();
}

async fn pending_batch(daemon: &DaemonHarness) -> (Raw, Raw, String) {
    let mut batch = Raw::session(daemon, "cli", "normal").await;
    let mut owner = Raw::session(daemon, "cli", "normal").await;
    let id = task(&mut owner, "unassigned work", json!({})).await;
    batch
        .send(&json!({"id":1,"req":{"op":"dispatch.until_terminal","args":{}}}))
        .await;
    tokio::time::timeout(WAIT, async {
        while dispatch_state(&mut owner).await != (false, true) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("batch became active");
    (batch, owner, id)
}

#[tokio::test]
async fn dispatch_off_interrupts_the_wait_with_a_pending_task() {
    let daemon = daemon().await;
    let (mut batch, mut owner, _) = pending_batch(&daemon).await;
    ok(&owner
        .request(2, "dispatch.set", json!({"dispatch":"off"}))
        .await);
    let reply = tokio::time::timeout(WAIT, batch.terminal(1)).await.unwrap();
    assert_eq!(ok(&reply), "interrupted");
    assert_eq!(dispatch_state(&mut owner).await, (false, false));
    assert_daemon_owns_project(&daemon).await;
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_finished_all_done_reply_is_not_retracted_by_later_shutdown() {
    let (daemon, reply) = terminal_batch(&[TaskStatus::Done]).await;
    daemon.shutdown().await.unwrap();
    assert_eq!(ok(&reply), "all_done");
}

#[tokio::test]
async fn a_finished_has_failed_reply_is_not_retracted_by_later_shutdown() {
    let (daemon, reply) = terminal_batch(&[TaskStatus::Failed]).await;
    daemon.shutdown().await.unwrap();
    assert_eq!(ok(&reply), "has_failed");
}

#[tokio::test]
async fn an_interrupted_batch_leaves_ownership_until_explicit_shutdown() {
    let daemon = daemon().await;
    let root = daemon.project_root().to_owned();
    let (mut batch, mut owner, _) = pending_batch(&daemon).await;
    ok(&owner
        .request(2, "dispatch.set", json!({"dispatch":"off"}))
        .await);
    let reply = tokio::time::timeout(WAIT, batch.terminal(1)).await.unwrap();
    assert_eq!(ok(&reply), "interrupted");
    assert!(root.join(".orchestry/orchestry.lock").exists());
    let quiet = daemon.quiesce().await.unwrap();
    assert!(!root.join(".orchestry/orchestry.lock").exists());
    drop(quiet);
}

#[tokio::test]
async fn the_wait_ends_as_soon_as_the_last_task_becomes_terminal() {
    let daemon = daemon().await;
    let (mut batch, mut owner, id) = pending_batch(&daemon).await;
    ok(&owner.request(2, "task.cancel", json!({"id":id})).await);
    let reply = tokio::time::timeout(WAIT, batch.terminal(1)).await.unwrap();
    assert_eq!(ok(&reply), "all_done");
    assert_eq!(dispatch_state(&mut owner).await, (false, false));
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_warnings_are_available_before_dispatch() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let id = support::shell_agent(&mut raw, &daemon, "cursor fixture", "true").await;
    let mut agent = daemon.engine().agents.get(&id).await.unwrap().unwrap();
    agent.adapter = "cursor".into();
    agent.config.effort = Some(orch_domain::ReasoningEffort::High);
    daemon.engine().agents.save(&agent).await.unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let reply = raw.request(1, "ui.bootstrap", json!({})).await;
    let warnings = ok(&reply)["startup_warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{reply}");
    let warning = warnings[0].as_str().unwrap();
    assert!(
        warning.contains(&agent.id) && warning.contains("legacy-ignored"),
        "{warning}"
    );
    assert_eq!(dispatch_state(&mut raw).await, (false, false));
    assert!(
        daemon
            .engine()
            .state
            .read()
            .await
            .unwrap()
            .running
            .is_empty()
    );
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn clean_agents_have_no_startup_warnings() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    support::shell_agent(&mut raw, &daemon, "clean fixture", "true").await;
    let reply = raw.request(1, "ui.bootstrap", json!({})).await;
    assert_eq!(ok(&reply)["startup_warnings"], json!([]));
    daemon.shutdown().await.unwrap();
}

/// Establish subscription readiness through the real stream before emitting
/// the burst. The emitter is test-owned and joined on every normal path.
async fn live_logs(daemon: &DaemonHarness) -> Raw {
    let mut raw = Raw::session(daemon, "cli", "normal").await;
    raw.send(&json!({"id":1,"req":{"op":"logs.follow","args":{}}}))
        .await;
    let events = Arc::clone(&daemon.engine().events);
    let (done, mut stop) = tokio::sync::oneshot::channel::<()>();
    let emitter = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(1));
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = interval.tick() => events.emit(OrchestratorEvent::OrchestratorTick {running:0, queued:0}),
            }
        }
    });
    let first = tokio::time::timeout(WAIT, raw.recv())
        .await
        .unwrap()
        .unwrap();
    done.send(()).unwrap();
    emitter.await.unwrap();
    assert!(first.get("part").is_some(), "{first}");
    raw
}

#[tokio::test]
async fn the_log_stream_delivers_the_whole_burst_in_order_before_cancel() {
    let daemon = daemon().await;
    let mut raw = live_logs(&daemon).await;
    for n in 0..5 {
        daemon
            .engine()
            .events
            .emit(OrchestratorEvent::OrchestratorError {
                error: format!("wind-down {n}"),
                context: "stop".into(),
                fatal: false,
            });
    }
    let mut seen = Vec::new();
    tokio::time::timeout(WAIT, async {
        while seen.len() < 5 {
            let frame = raw.recv().await.unwrap();
            if let Some(error) = frame["part"]["data"]["error"].as_str() {
                seen.push(error.to_owned());
            }
        }
    })
    .await
    .expect("the stream drained the burst");
    assert_eq!(
        seen,
        (0..5).map(|n| format!("wind-down {n}")).collect::<Vec<_>>()
    );
    raw.send(&json!({"id":2,"req":{"op":"request.cancel","args":{"id":1}}}))
        .await;
    ok(&raw.terminal(1).await);
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_log_stream_ends_when_the_daemon_stops() {
    let daemon = daemon().await;
    let mut raw = live_logs(&daemon).await;
    daemon.shutdown().await.unwrap();
    tokio::time::timeout(WAIT, raw.until_closed())
        .await
        .expect("closed daemon left an observation waiting forever");
}

async fn listener(daemon: &DaemonHarness) -> String {
    let mut raw = Raw::session(daemon, "cli", "normal").await;
    tokio::time::timeout(WAIT, async {
        loop {
            let reply = raw.request(1, "remote.status", json!({})).await;
            if let Some(addr) = ok(&reply)["listen"].as_str() {
                return addr.to_owned();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("listener published through the daemon")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_handshakes_use_the_ready_daemon_before_any_dispatch_tick() {
    use orch_peer::codec::read_frame;
    use orch_peer::protocol::Frame;
    use tokio::io::{AsyncWriteExt, BufReader};
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .on_socket()
        .listen("127.0.0.1:0")
        .peer_name("test-worker")
        .start()
        .await
        .unwrap();
    let record = orch_client_record(daemon.project_root());
    assert_eq!(record["pid"], std::process::id());
    let addr = listener(&daemon).await;
    let mut peer = tokio::net::TcpStream::connect(addr).await.unwrap();
    peer.write_all(b"{\"t\":\"hello\",\"proto\":1,\"instance_id\":\"early\",\"name\":\"early\",\"os\":\"linux\",\"agents\":[]}\n").await.unwrap();
    let welcome = tokio::time::timeout(WAIT, read_frame(&mut BufReader::new(peer)))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(welcome, Some(Frame::Welcome { proto: 1, .. })),
        "{welcome:?}"
    );
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    assert_eq!(dispatch_state(&mut raw).await, (false, false));
    daemon.shutdown().await.unwrap();
}

fn orch_client_record(root: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(root.join(".orchestry/daemon.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn explicit_shutdown_unpublishes_the_inbound_controller() {
    let daemon = daemon().await;
    let orchestrator = Arc::clone(&daemon.engine().orchestrator);
    orchestrator.set_inbound_controller(Some("controller-id".into()));
    daemon.shutdown().await.unwrap();
    assert!(orchestrator.inbound_controller().is_none());
}

#[tokio::test]
async fn a_listener_bind_failure_releases_daemon_ownership() {
    let root = tempfile::tempdir().unwrap();
    orch_daemon::initialize(orch_daemon::InitOptions {
        project_root: root.path().to_owned(),
        adapter: Some("shell".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let port = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let result = DaemonHarness::builder()
        .listen(port.local_addr().unwrap().to_string())
        .start_in(root.path())
        .await;
    let error = result.err().expect("busy port rejects startup");
    assert!(error.to_string().contains("Listen on"), "{error}");
    assert!(!root.path().join(".orchestry/orchestry.lock").exists());
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .start_in(root.path())
        .await
        .unwrap();
    assert_daemon_owns_project(&daemon).await;
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_shutdown_closes_the_listener_and_unpublishes_its_status() {
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .listen("127.0.0.1:0")
        .peer_name("test-worker")
        .start()
        .await
        .unwrap();
    let addr = listener(&daemon).await;
    tokio::net::TcpStream::connect(&addr).await.unwrap();
    let quiet = daemon.quiesce().await.unwrap();
    assert!(
        !quiet
            .project_root()
            .join(".orchestry/remotes/_listener.json")
            .exists()
    );
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}
