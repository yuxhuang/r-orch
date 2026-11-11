//! A phase reserves ordering; only its store adapters declare writes.
use crate::container::commit::{CommitRecord, CommitSource, CommitStore, Publication};
use crate::container::{EngineSeams, FullEngine, build_full_with_processes};
use orch_core::test_support::{FakeProcessManager, make_agent, make_task};
use orch_domain::{AgentStatus, TaskStatus};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

struct Fixture {
    full: FullEngine,
    records: Arc<Mutex<Vec<CommitRecord>>>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".orchestry")).unwrap();
        let full = build_full_with_processes(
            dir.path(),
            EngineSeams {
                global_config_path: Some(dir.path().join("global.yml")),
                ..Default::default()
            },
            Default::default(),
            Arc::new(FakeProcessManager::new()),
        )
        .await
        .unwrap();
        full.orchestrator.daemon_initialize().await.unwrap();
        let records = Arc::new(Mutex::new(Vec::new()));
        let sink = records.clone();
        full.operation_boundary()
            .set_publisher(Some(Arc::new(move |p| {
                if let Publication::Commit(record) = p {
                    sink.lock().unwrap().push(record.clone());
                }
            })));
        Self {
            full,
            records,
            _dir: dir,
        }
    }

    async fn candidate(&self) {
        self.full.agents.save(&make_agent()).await.unwrap();
        self.full.tasks.save(&make_task()).await.unwrap();
        self.records.lock().unwrap().clear();
    }

    async fn silent_selection(&self, cancelled: bool) {
        let boundary = self.full.operation_boundary();
        let before = boundary.current_commit_seq();
        let mut commits = boundary.subscribe_commits();
        self.full
            .orchestrator
            .daemon_launch_task_with_guard(
                &make_task().id,
                Arc::new(AtomicBool::new(cancelled)),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            boundary.current_commit_seq(),
            before,
            "a rejected candidate wrote nothing"
        );
        assert!(
            self.records.lock().unwrap().is_empty(),
            "no typed commit for a no-write selection"
        );
        assert!(matches!(
            commits.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn occupied_candidate_publishes_nothing() {
    let f = Fixture::new().await;
    f.candidate().await;
    f.full
        .orchestrator
        .inner
        .preparing_agents
        .lock()
        .await
        .insert("tsk_other".into(), (1, make_agent().id));
    f.silent_selection(false).await;
}

#[tokio::test]
async fn missing_agent_candidate_publishes_nothing() {
    let f = Fixture::new().await;
    f.full.tasks.save(&make_task()).await.unwrap();
    f.records.lock().unwrap().clear();
    f.silent_selection(false).await;
}

#[tokio::test]
async fn ineligible_agent_candidate_publishes_nothing() {
    let f = Fixture::new().await;
    f.candidate().await;
    f.full
        .agents
        .save(&orch_domain::Agent {
            status: AgentStatus::Disabled,
            ..make_agent()
        })
        .await
        .unwrap();
    f.records.lock().unwrap().clear();
    f.silent_selection(false).await;
}

#[tokio::test]
async fn cancelled_candidate_publishes_nothing() {
    let f = Fixture::new().await;
    f.candidate().await;
    f.silent_selection(true).await;
}

#[tokio::test]
async fn stale_candidate_publishes_nothing() {
    let f = Fixture::new().await;
    f.candidate().await;
    f.full
        .tasks
        .save(&orch_domain::Task {
            status: TaskStatus::Cancelled,
            ..make_task()
        })
        .await
        .unwrap();
    f.records.lock().unwrap().clear();
    f.silent_selection(false).await;
}

#[tokio::test]
async fn claimed_candidate_announces_state_before_preparation() {
    let f = Fixture::new().await;
    f.candidate().await;
    // Selection succeeds; adapter validation fails only after the claim is saved.
    f.full
        .agents
        .save(&orch_domain::Agent {
            adapter: "unregistered".into(),
            ..make_agent()
        })
        .await
        .unwrap();
    f.records.lock().unwrap().clear();
    let _ = f
        .full
        .orchestrator
        .daemon_launch_task(&make_task().id)
        .await;
    let records = f.records.lock().unwrap();
    assert!(!records.is_empty());
    assert!(
        records[0]
            .changes
            .iter()
            .any(|c| c.store == CommitStore::State)
    );
    assert!(
        records.iter().all(|r| !r.store_set_uncertain),
        "{records:?}"
    );
}

#[tokio::test]
async fn clean_shutdown_publishes_only_its_state_write() {
    let f = Fixture::new().await;
    let before = f.full.operation_boundary().current_commit_seq();
    f.full.orchestrator.daemon_stop().await.unwrap();
    let records = f.records.lock().unwrap();
    assert_eq!(
        records.len(),
        1,
        "snapshot and cancellation perform no store write: {records:?}"
    );
    assert_eq!(f.full.operation_boundary().current_commit_seq(), before + 1);
    assert_eq!(records[0].changes[0].store, CommitStore::State);
    assert!(!records[0].store_set_uncertain);
}

#[tokio::test]
async fn startup_read_failure_publishes_nothing() {
    let f = Fixture::new().await;
    let path = f.full.paths.state_path();
    tokio::fs::remove_file(&path).await.unwrap();
    tokio::fs::create_dir(&path).await.unwrap();
    let before = f.full.operation_boundary().current_commit_seq();
    assert!(f.full.orchestrator.daemon_initialize().await.is_err());
    assert_eq!(f.full.operation_boundary().current_commit_seq(), before);
    assert!(f.records.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_mailbox_and_rehydration_publish_nothing() {
    let f = Fixture::new().await;
    let before = f.full.operation_boundary().current_commit_seq();
    f.full
        .message_service
        .mark_snapshot_delivered("agent", "task", &[])
        .await
        .unwrap();
    f.full.orchestrator.rehydrate_remote_runs().await.unwrap();
    assert_eq!(f.full.operation_boundary().current_commit_seq(), before);
    assert!(f.records.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_tick_publishes_nothing_even_after_peer_or_client_commit() {
    let f = Fixture::new().await;
    for source in [CommitSource::Peer, CommitSource::Rpc] {
        f.full
            .operation_boundary()
            .run_from(source, false, async {
                f.full.context.set("wake", "value", None).await.unwrap();
            })
            .await;
        f.records.lock().unwrap().clear();
        let before = f.full.operation_boundary().current_commit_seq();
        f.full
            .orchestrator
            .daemon_tick_with_guard(true, Arc::new(AtomicBool::new(true)))
            .await
            .unwrap();
        assert_eq!(f.full.operation_boundary().current_commit_seq(), before);
        assert!(f.records.lock().unwrap().is_empty());
    }
}
