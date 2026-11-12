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

#[tokio::test]
async fn unchanged_ticks_keep_the_bus_without_state_writes_or_commits() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new().await;
    let path = f.full.paths.state_path();
    let before_file = std::fs::metadata(&path).unwrap().ino();
    let boundary = f.full.operation_boundary();
    let before = boundary.current_commit_seq();
    let mut events = f.full.events.subscribe();
    for _ in 0..2 {
        f.full.orchestrator.daemon_tick(true).await.unwrap();
    }
    assert_eq!(
        std::fs::metadata(path).unwrap().ino(),
        before_file,
        "no State replacement"
    );
    assert_eq!(boundary.current_commit_seq(), before, "no write commit");
    assert!(f.records.lock().unwrap().is_empty());
    assert_eq!(
        events
            .try_drain()
            .iter()
            .filter(|event| matches!(
                ***event,
                orch_domain::OrchestratorEvent::OrchestratorTick { .. }
            ))
            .count(),
        2
    );
}

#[tokio::test]
async fn reconciliation_removal_announces_state_once_then_becomes_clean() {
    use super::harness::{NOW_ISO, state_running};
    let f = Fixture::new().await;
    f.full.agents.save(&make_agent()).await.unwrap();
    f.full
        .tasks
        .save(&orch_domain::Task {
            status: TaskStatus::Done,
            ..make_task()
        })
        .await
        .unwrap();
    f.full
        .state
        .write(&state_running(
            &make_task().id,
            &make_agent().id,
            "run_stale",
            42,
            NOW_ISO,
        ))
        .await
        .unwrap();
    f.records.lock().unwrap().clear();
    f.full.orchestrator.daemon_tick(true).await.unwrap();
    assert!(f.full.state.read().await.unwrap().running.is_empty());
    {
        let records = f.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            records[0]
                .changes
                .iter()
                .any(|change| change.store == CommitStore::State)
        );
        assert!(!records[0].store_set_uncertain);
    }
    f.full.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(
        f.records.lock().unwrap().len(),
        1,
        "the repair is already durable"
    );
}

#[tokio::test]
async fn lazy_and_remote_dirty_flags_flush_even_equal_state_before_reload() {
    use super::harness::Harness;
    use std::sync::atomic::Ordering;
    for lazy in [false, true] {
        let h = Harness::builder().build();
        h.load_state().await.unwrap();
        if lazy {
            h.orchestrator.save_state_lazy().await;
        } else {
            h.orchestrator
                .inner
                .remote_state_dirty
                .store(true, Ordering::SeqCst);
        }
        h.reconcile().await.unwrap();
        assert_eq!(
            h.state.written().len(),
            1,
            "dirty flush must survive; redundant tail must go"
        );
        h.reconcile().await.unwrap();
        assert_eq!(h.state.written().len(), 1);
    }
}

#[tokio::test]
async fn nested_transition_saves_remain_and_the_tail_uses_the_latest_durable_state() {
    use super::harness::Harness;
    let h = Harness::builder().build();
    let mut slot = h.orchestrator.inner.state.lock().await;
    h.orchestrator.load_state(&mut slot).await.unwrap();
    let state = slot.as_mut().unwrap();
    h.orchestrator.reconcile(state).await.unwrap();
    state.stats.total_runs = 1;
    // Nested transition saves are never suppressed, even when identical.
    h.orchestrator.save_state(state).await.unwrap();
    h.orchestrator.save_state(state).await.unwrap();
    state.stats.total_runs = 0;
    h.orchestrator.reconcile(state).await.unwrap();
    assert_eq!(
        h.state
            .written()
            .iter()
            .map(|s| s.stats.total_runs)
            .collect::<Vec<_>>(),
        [1, 1, 0]
    );
    assert_eq!(h.state.peek().stats.total_runs, 0);
    h.orchestrator.reconcile(state).await.unwrap();
    assert_eq!(h.state.written().len(), 3);
}

#[tokio::test]
async fn ordered_state_collections_are_not_treated_as_equal_serialized_state() {
    use super::harness::Harness;
    let mut initial = orch_domain::OrchestratorState::default();
    initial.claimed.extend(["first".into(), "second".into()]);
    let h = Harness::builder().state(initial).build();
    let mut slot = h.orchestrator.inner.state.lock().await;
    h.orchestrator.load_state(&mut slot).await.unwrap();
    let state = slot.as_mut().unwrap();
    state.claimed.shift_remove("first");
    state.claimed.insert("first".into());
    h.orchestrator.reconcile(state).await.unwrap();
    assert_eq!(
        h.state.peek().claimed.into_iter().collect::<Vec<_>>(),
        ["second", "first"]
    );
    h.orchestrator.reconcile(state).await.unwrap();
    assert_eq!(h.state.written().len(), 1);
}

#[tokio::test]
async fn failed_state_save_is_announced_and_retried_before_reload() {
    let f = Fixture::new().await;
    let path = f.full.paths.state_path();
    tokio::fs::remove_file(&path).await.unwrap();
    tokio::fs::create_dir(&path).await.unwrap();
    let boundary = f.full.operation_boundary();
    boundary
        .run(false, async {
            let mut slot = f.full.orchestrator.inner.state.lock().await;
            let state = slot.as_mut().unwrap();
            state.stats.total_runs = 7;
            assert!(f.full.orchestrator.save_state(state).await.is_err());
        })
        .await;
    {
        let records = f.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            records[0]
                .changes
                .iter()
                .any(|c| c.store == CommitStore::State)
        );
        assert!(!records[0].store_set_uncertain);
    }
    tokio::fs::remove_dir(&path).await.unwrap();
    f.full.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(f.full.state.read().await.unwrap().stats.total_runs, 7);
    assert_eq!(
        f.records.lock().unwrap().len(),
        2,
        "one retry, no redundant tail"
    );
    f.full.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(f.records.lock().unwrap().len(), 2);
}

struct PausedStateWrite {
    inner: Arc<orch_core::test_support::FakeStateStore>,
    entered: Arc<tokio::sync::Notify>,
    pause: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl orch_core::storage::StateStore for PausedStateWrite {
    async fn read(&self) -> orch_core::Result<orch_domain::OrchestratorState> {
        self.inner.read().await
    }
    async fn write(&self, state: &orch_domain::OrchestratorState) -> orch_core::Result<()> {
        if self.pause.load(std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.inner.write(state).await
    }
}

#[tokio::test]
async fn cancelled_state_save_keeps_the_unsaved_state_for_the_next_tick() {
    use super::harness::Harness;
    use std::sync::atomic::Ordering;
    let entered = Arc::new(tokio::sync::Notify::new());
    let pause = Arc::new(AtomicBool::new(false));
    let h = Harness::builder()
        .wrap_state_store({
            let entered = entered.clone();
            let pause = pause.clone();
            move |inner| {
                Arc::new(PausedStateWrite {
                    inner,
                    entered,
                    pause,
                })
            }
        })
        .build();
    h.load_state().await.unwrap();
    pause.store(true, Ordering::SeqCst);
    let save = {
        let orchestrator = h.orchestrator.clone();
        tokio::spawn(async move {
            let mut slot = orchestrator.inner.state.lock().await;
            let state = slot.as_mut().unwrap();
            state.stats.total_runs = 9;
            orchestrator.save_state(state).await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    save.abort();
    assert!(save.await.unwrap_err().is_cancelled());
    pause.store(false, Ordering::SeqCst);
    h.reconcile().await.unwrap();
    assert_eq!(h.state.peek().stats.total_runs, 9);
    assert_eq!(h.state.written().len(), 1);
    h.reconcile().await.unwrap();
    assert_eq!(h.state.written().len(), 1);
}
