//! Controller lifecycle regressions use a fake link and real peer directory files.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::remote::{
    RemoteBridge, RemoteExecuteHandle, RemoteOutcome, RemoteRunEvent, RemoteSubmit,
};
use orch_core::storage::{AgentStore, StateStore, TaskStore};
use orch_core::test_support::{FakeStateStore, make_agent, make_run, make_task};
use orch_domain::{
    AdapterErrorKind, Agent, ApprovalPolicy, Message, OrchestratorEvent, OrchestratorEventType,
    OrchestratorState, RemoteConfig, RemoteRunRef, RemoteStatus, ReviewCriterion, ReviewResult,
    Run, RunStatus, Task, TaskStatus,
};
use orch_infra::storage::{Paths, remote::FileRemoteStore};
use tokio_util::sync::CancellationToken;

use super::harness::{Harness, HarnessBuilder, NOW_ISO, iso_before, review_fail, state_running};

#[derive(Default)]
struct FakeBridge {
    state: Option<Arc<FakeStateStore>>,
    submitted: Mutex<Vec<RemoteSubmit>>,
    cancelled: Mutex<Vec<(String, String)>>,
    acknowledged: Mutex<Vec<(String, String)>>,
    /// Cancels the drain reports as still unacknowledged at its deadline.
    unacknowledged: Mutex<Vec<(String, String)>>,
    drain_fails: std::sync::atomic::AtomicBool,
}

impl FakeBridge {
    fn for_harness(h: &Harness) -> Self {
        Self {
            state: Some(h.state.clone()),
            ..Self::default()
        }
    }
}

#[async_trait]
impl RemoteBridge for FakeBridge {
    fn submit(&self, request: RemoteSubmit) -> Result<RemoteExecuteHandle> {
        if let Some(state) = &self.state {
            let state = state.peek();
            let entry = state
                .running
                .values()
                .find(|entry| entry.run_id == request.exec_key)
                .expect("remote running entry must be durable before submit");
            assert_eq!(entry.pid, 0);
            assert_eq!(entry.remote.as_ref().unwrap().exec_key, request.exec_key);
        }
        self.submitted.lock().unwrap().push(request);
        Ok(RemoteExecuteHandle {
            pid: 0,
            events: Box::pin(futures::stream::pending()),
        })
    }

    async fn cancel(&self, alias: &str, exec_key: &str) -> Result<()> {
        self.cancelled
            .lock()
            .unwrap()
            .push((alias.into(), exec_key.into()));
        Ok(())
    }

    async fn drain_cancellations(
        &self,
        _deadline: std::time::Duration,
    ) -> Result<Vec<orch_core::remote::PendingRemoteCancellation>> {
        if self.drain_fails.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(orch_core::error::Error::other("drain transport failed"));
        }
        Ok(self
            .unacknowledged
            .lock()
            .unwrap()
            .iter()
            .map(
                |(alias, exec_key)| orch_core::remote::PendingRemoteCancellation {
                    alias: alias.clone(),
                    exec_key: exec_key.clone(),
                },
            )
            .collect())
    }

    async fn ack_finished(&self, alias: &str, exec_key: &str) -> Result<()> {
        if let Some(state) = &self.state {
            assert!(
                state
                    .peek()
                    .running
                    .values()
                    .all(|entry| entry.run_id != exec_key),
                "finished acknowledgement must follow durable running-entry removal"
            );
        }
        self.acknowledged
            .lock()
            .unwrap()
            .push((alias.into(), exec_key.into()));
        Ok(())
    }

    async fn send_msg(&self, _peer: &str, _message: &Message) -> Result<()> {
        Ok(())
    }
}

struct PeerFixture {
    temp: tempfile::TempDir,
    store: FileRemoteStore,
}

impl PeerFixture {
    async fn new(online: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let store = FileRemoteStore::new(paths.clone());
        store
            .add(RemoteConfig {
                alias: "worker".into(),
                addr: "127.0.0.1:1".into(),
                added_at: NOW_ISO.into(),
            })
            .await
            .unwrap();
        assert!(
            orch_infra::storage::lock::acquire_lock(&paths.lock_path())
                .await
                .unwrap()
                .acquired
        );
        let fixture = Self { temp, store };
        fixture.status(online).await;
        fixture
    }

    async fn status(&self, online: bool) {
        self.store
            .write_status(&RemoteStatus {
                alias: "worker".into(),
                addr: "127.0.0.1:1".into(),
                online,
                last_seen: Some(NOW_ISO.into()),
                last_error: None,
                peer_name: Some("Worker".into()),
                instance_id: Some("worker-instance".into()),
                os: Some("linux".into()),
                agents: vec![Agent {
                    id: "agt_peer".into(),
                    name: "Peer".into(),
                    capabilities: vec!["linux".into()],
                    ..make_agent()
                }],
            })
            .await
            .unwrap();
    }

    fn builder(&self) -> HarnessBuilder {
        Harness::builder().project_root(self.temp.path().to_path_buf())
    }
}

fn remote_state(at: &str) -> OrchestratorState {
    let mut state = state_running("tsk_remote", "agt_peer@worker", "run_remote", 0, at);
    state.running.get_mut("tsk_remote").unwrap().remote = Some(RemoteRunRef {
        alias: "worker".into(),
        exec_key: "run_remote".into(),
    });
    state
}

async fn running(fixture: &PeerFixture) -> (Harness, Arc<FakeBridge>) {
    let h = fixture
        .builder()
        .tasks([
            Task {
                id: "tsk_remote".into(),
                status: TaskStatus::InProgress,
                assignee: Some("agt_peer@worker".into()),
                attempts: 1,
                max_attempts: 3,
                review_criteria: Some(vec![ReviewCriterion::TestPass]),
                ..make_task()
            },
            Task {
                id: "tsk_dependent".into(),
                depends_on: vec!["tsk_remote".into()],
                ..make_task()
            },
        ])
        .state(remote_state(NOW_ISO))
        .build();
    h.runs.seed(Run {
        id: "run_remote".into(),
        task_id: "tsk_remote".into(),
        agent_id: "agt_peer@worker".into(),
        status: RunStatus::Running,
        pid: Some(0),
        ..make_run()
    });
    h.set_state(remote_state(NOW_ISO)).await;
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    (h, bridge)
}

fn no_process_calls(h: &Harness) {
    assert!(
        h.processes.calls.calls().is_empty(),
        "remote run touched local process manager: {:?}",
        h.processes.calls.calls()
    );
}

fn cancelled_once(bridge: &FakeBridge) {
    assert_eq!(
        *bridge.cancelled.lock().unwrap(),
        vec![("worker".into(), "run_remote".into())]
    );
}

#[tokio::test]
async fn reservation_allows_only_one_submit_for_two_tasks_and_one_remote_agent() {
    let fixture = PeerFixture::new(true).await;
    let h = fixture
        .builder()
        .tasks([
            Task {
                id: "tsk_one".into(),
                ..make_task()
            },
            Task {
                id: "tsk_two".into(),
                ..make_task()
            },
        ])
        .build();
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.set_state(OrchestratorState::default()).await;
    h.tick().await.unwrap();
    h.wait_for(|_| !bridge.submitted.lock().unwrap().is_empty())
        .await;
    assert_eq!(bridge.submitted.lock().unwrap().len(), 1);
    let state = h.in_memory_state().await;
    assert_eq!(state.running.len(), 1);
    let entry = state.running.values().next().unwrap();
    assert_eq!(entry.pid, 0);
    assert_eq!(entry.remote.as_ref().unwrap().exec_key, entry.run_id);
    assert_eq!(h.persisted_state().running, state.running);
    assert!(h.workspace.calls.calls().is_empty());
    assert!(h.adapter.execute_calls.calls().is_empty());
    assert!(h.agents.list().await.unwrap().is_empty());
    no_process_calls(&h);
    h.orchestrator.cancel_task(&entry.task_id).await.unwrap();
}

#[tokio::test]
async fn worker_mirror_without_collector_cancels_immediately_and_reserves_until_exit() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            max_attempts: 1,
            labels: vec!["remote-ctl:controller".into(), "exec:run_controller".into()],
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_mirror",
            "agt_test1",
            "run_mirror",
            4321,
            &iso_before(86_400_000),
        ))
        .build();
    h.runs.seed(Run {
        id: "run_mirror".into(),
        task_id: "tsk_mirror".into(),
        status: RunStatus::Running,
        pid: Some(4321),
        ..make_run()
    });
    h.processes.set_alive(4321);
    h.reconcile().await.unwrap();
    assert_eq!(h.task("tsk_mirror").status, TaskStatus::Cancelled);
    let diagnostic = h.task("tsk_mirror").feedback.unwrap();
    assert!(diagnostic.contains("PID 4321 not signalled, may still be running"));
    assert_eq!(
        h.runs.peek("run_mirror").unwrap().error,
        Some(diagnostic.clone())
    );
    assert_eq!(h.in_memory_state().await.running.len(), 1);
    assert!(
        h.processes.killed_with_grace().is_empty(),
        "lost telemetry is not a stall"
    );
    h.processes.set_dead(4321);
    h.orchestrator.invalidate_caches().await;
    h.reconcile().await.unwrap();
    assert_eq!(h.task("tsk_mirror").status, TaskStatus::Cancelled);
    let run = h.runs.peek("run_mirror").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.error, Some(diagnostic));
    assert!(
        run.pid.is_none(),
        "released PID must not resurrect a reservation"
    );
    assert!(h.persisted_state().running.is_empty());
    assert!(h.persisted_state().retry_queue.is_empty());
    assert_eq!(h.agent("agt_test1").status, orch_domain::AgentStatus::Idle);
    // PID reuse after durable release must not resurrect the reservation.
    h.processes.set_alive(4321);
    let mut state = h.persisted_state();
    h.orchestrator
        .cleanup_stale_running_entries(&mut state)
        .await
        .unwrap();
    assert!(state.running.is_empty());
    assert_eq!(h.task("tsk_mirror").status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn worker_mirror_without_run_or_reservation_cancels_without_retry() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            max_attempts: 1,
            labels: vec!["remote-ctl:controller".into(), "exec:run_controller".into()],
            ..make_task()
        }])
        .agents([make_agent()])
        .build();
    h.reconcile().await.unwrap();
    let task = h.task("tsk_mirror");
    assert_eq!(task.status, TaskStatus::Cancelled);
    assert_eq!(task.attempts, 1);
    assert!(
        task.feedback
            .unwrap()
            .contains("before its PID was persisted")
    );
    assert!(h.persisted_state().running.is_empty());
    assert!(h.persisted_state().retry_queue.is_empty());
    assert!(h.processes.killed_with_grace().is_empty());
}

#[tokio::test]
async fn startup_cleanup_cancels_remote_without_pid_check_or_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    let mut state = remote_state(NOW_ISO);
    h.orchestrator
        .cleanup_stale_running_entries(&mut state)
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert!(state.running.is_empty());
    assert!(state.retry_queue.is_empty());
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(
        h.runs.peek("run_remote").unwrap().error.as_deref(),
        Some("Orchestrator restarted")
    );
    no_process_calls(&h);
}

#[tokio::test]
async fn orphaned_remote_run_is_cancelled_without_pid_check_or_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .cleanup_orphaned_running_runs(&OrchestratorState::default())
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(
        h.runs.peek("run_remote").unwrap().error.as_deref(),
        Some("Orchestrator restarted")
    );
    no_process_calls(&h);
}

#[tokio::test]
async fn reconcile_never_classifies_remote_pid_zero_as_crashed() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.processes.set_all_dead(true);
    h.reconcile().await.unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::InProgress);
    assert_eq!(h.in_memory_state().await.running.len(), 1);
    no_process_calls(&h);
}

#[tokio::test]
async fn reconcile_never_stall_kills_remote_even_when_offline() {
    let fixture = PeerFixture::new(false).await;
    let (h, bridge) = running(&fixture).await;
    h.state
        .write(&remote_state(&iso_before(86_400_000)))
        .await
        .unwrap();
    h.reconcile().await.unwrap();
    let state = h.in_memory_state().await;
    assert_eq!(h.task("tsk_remote").status, TaskStatus::InProgress);
    assert_eq!(state.running.len(), 1);
    assert!(state.retry_queue.is_empty());
    assert!(bridge.cancelled.lock().unwrap().is_empty());
    no_process_calls(&h);
}

#[tokio::test]
async fn reconcile_terminal_remote_task_sends_cancel_without_local_kill() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    let mut task = h.task("tsk_remote");
    task.status = TaskStatus::Cancelled;
    h.tasks.save(&task).await.unwrap();
    h.reconcile().await.unwrap();
    assert!(h.in_memory_state().await.running.is_empty());
    cancelled_once(&bridge);
    no_process_calls(&h);
}

#[tokio::test]
async fn cancel_task_sends_remote_cancel_without_local_kill_or_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    h.orchestrator.cancel_task("tsk_remote").await.unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert!(h.in_memory_state().await.retry_queue.is_empty());
    cancelled_once(&bridge);
    no_process_calls(&h);
}

#[tokio::test]
async fn force_stop_agent_sends_remote_cancel_without_local_kill() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    h.orchestrator
        .force_stop_agent("agt_peer@worker")
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Failed);
    assert!(h.in_memory_state().await.running.is_empty());
    assert!(h.in_memory_state().await.retry_queue.is_empty());
    cancelled_once(&bridge);
    no_process_calls(&h);
}

#[tokio::test]
async fn graceful_shutdown_cancels_remote_without_local_kill_or_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    let deferred = h
        .orchestrator
        .shutdown_running_entries(&remote_state(NOW_ISO))
        .await;
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    // The run waits for the drain's verdict.
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Running
    );
    h.orchestrator.drain_remote_cancellations(deferred).await;
    let run = h.runs.peek("run_remote").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(
        run.error, None,
        "acknowledged cancel is not an unknown outcome"
    );
    assert!(h.in_memory_state().await.retry_queue.is_empty());
    cancelled_once(&bridge);
    no_process_calls(&h);
}

fn unknown_cancel_warnings(h: &Harness) -> Vec<(String, String)> {
    h.events_of(OrchestratorEventType::OrchestratorError)
        .into_iter()
        .filter_map(|event| match event {
            OrchestratorEvent::OrchestratorError { error, context, .. }
                if error.contains("outcome unknown") =>
            {
                Some((error, context))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn unacknowledged_shutdown_cancel_is_persisted_on_the_run_and_reported_once() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    bridge
        .unacknowledged
        .lock()
        .unwrap()
        .push(("worker".into(), "run_remote".into()));
    h.orchestrator.stop().await.unwrap();

    let run = h.runs.peek("run_remote").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    let error = run.error.expect("durable unknown-outcome diagnostic");
    for part in ["worker", "run_remote", "outcome unknown", "next Sync"] {
        assert!(error.contains(part), "{part:?} missing from {error:?}");
    }
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert_eq!(
        unknown_cancel_warnings(&h),
        vec![(error, "stop: cancel remote worker:run_remote".to_owned())],
        "one live warning per unresolved cancel"
    );
    cancelled_once(&bridge);
    no_process_calls(&h);
}

#[tokio::test]
async fn failed_shutdown_drain_records_every_deferred_cancel_as_unknown() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    bridge
        .drain_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.orchestrator.stop().await.unwrap();

    let run = h.runs.peek("run_remote").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert!(
        run.error
            .as_deref()
            .is_some_and(|error| error.contains("outcome unknown")),
        "{run:?}"
    );
    assert_eq!(unknown_cancel_warnings(&h).len(), 1);
}

#[tokio::test]
async fn unresolved_cancel_from_before_shutdown_is_still_reported_without_a_run_write() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    bridge
        .unacknowledged
        .lock()
        .unwrap()
        .push(("worker".into(), "run_earlier".into()));
    h.orchestrator.stop().await.unwrap();

    assert_eq!(h.runs.peek("run_remote").unwrap().error, None);
    let warnings = unknown_cancel_warnings(&h);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].1, "stop: cancel remote worker:run_earlier");
}

#[tokio::test]
async fn removed_peer_fails_unknown_outcome_without_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    fixture.store.remove("worker").await.unwrap();
    h.reconcile().await.unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Failed);
    assert!(h.in_memory_state().await.retry_queue.is_empty());
    assert_eq!(
        h.runs.peek("run_remote").unwrap().error.as_deref(),
        Some("remote worker removed; outcome unknown")
    );
    cancelled_once(&bridge);
    no_process_calls(&h);
}

#[tokio::test]
async fn unknown_execution_fails_without_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .collect_remote_events(
            Box::pin(futures::stream::iter([RemoteRunEvent::Unknown])),
            "run_remote",
            "tsk_remote",
            "agt_peer@worker",
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Failed);
    assert!(h.in_memory_state().await.retry_queue.is_empty());
    assert_eq!(
        h.runs.peek("run_remote").unwrap().error.as_deref(),
        Some("remote lost execution record")
    );
    no_process_calls(&h);
}

fn success() -> RemoteOutcome {
    RemoteOutcome::Succeeded {
        summary: "worker result".into(),
        files_changed: vec!["src/remote.rs".into()],
        branch: Some("orch/worker".into()),
        review_results: Some(vec![ReviewResult {
            criterion: ReviewCriterion::TestPass,
            passed: true,
            output: "worker tests passed".into(),
        }]),
        tokens: None,
    }
}

#[tokio::test]
async fn success_uses_worker_proof_and_skips_local_merge_and_criteria() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .handle_remote_outcome("tsk_remote", "run_remote", success(), true)
        .await
        .unwrap();
    let task = h.task("tsk_remote");
    assert_eq!(task.status, TaskStatus::Done);
    let proof = task.proof.unwrap();
    assert_eq!(proof.remote.as_deref(), Some("worker"));
    assert_eq!(proof.branch.as_deref(), Some("worker:orch/worker"));
    assert_eq!(proof.files_changed, vec!["src/remote.rs"]);
    assert_eq!(proof.agent_summary.as_deref(), Some("worker result"));
    assert_eq!(
        task.review_results.unwrap()[0].output,
        "worker tests passed"
    );
    assert!(h.workspace.calls.calls().is_empty());
    assert!(h.commands.calls().is_empty());
    no_process_calls(&h);
}

#[tokio::test]
async fn success_without_auto_approve_or_criteria_stays_in_review() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    let mut task = h.task("tsk_remote");
    task.review_criteria = None;
    h.tasks.save(&task).await.unwrap();
    h.orchestrator
        .handle_remote_outcome("tsk_remote", "run_remote", success(), false)
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Review);
}

#[tokio::test]
async fn review_failure_never_auto_approves_or_releases_dependents() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .handle_remote_outcome(
            "tsk_remote",
            "run_remote",
            RemoteOutcome::ReviewFailed {
                summary: "worker result".into(),
                review_results: vec![ReviewResult {
                    criterion: ReviewCriterion::TestPass,
                    passed: false,
                    output: "test failed".into(),
                }],
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Review);
    assert!(
        h.task("tsk_remote")
            .proof
            .unwrap()
            .agent_summary
            .unwrap()
            .contains("REMOTE REVIEW FAILED on worker")
    );
    assert_eq!(h.task("tsk_dependent").status, TaskStatus::Todo);
    h.orchestrator.fresh_dispatch_all().await.unwrap();
    assert!(!h.was_dispatched("tsk_dependent"));
}

#[tokio::test]
async fn integration_failure_never_auto_approves_or_releases_dependents() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .handle_remote_outcome(
            "tsk_remote",
            "run_remote",
            RemoteOutcome::IntegrationFailed {
                summary: "worker result".into(),
                info: "conflict in file".into(),
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Review);
    assert!(
        h.task("tsk_remote")
            .proof
            .unwrap()
            .agent_summary
            .unwrap()
            .contains("REMOTE MERGE CONFLICT on worker")
    );
    h.orchestrator.fresh_dispatch_all().await.unwrap();
    assert!(!h.was_dispatched("tsk_dependent"));
}

#[tokio::test]
async fn known_failure_preserves_error_kind_and_uses_normal_retry_rules() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .handle_remote_outcome(
            "tsk_remote",
            "run_remote",
            RemoteOutcome::Failed {
                error: "worker rate limit".into(),
                error_kind: Some(AdapterErrorKind::RateLimit),
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Retrying);
    assert_eq!(h.in_memory_state().await.retry_queue.len(), 1);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().error_kind,
        Some(AdapterErrorKind::RateLimit)
    );
    assert!(
        h.events_of(OrchestratorEventType::AgentError)
            .iter()
            .any(|event| matches!(
                event,
                OrchestratorEvent::AgentError {
                    error_kind: Some(AdapterErrorKind::RateLimit),
                    ..
                }
            ))
    );
    no_process_calls(&h);
}

#[tokio::test]
async fn cancellation_envelope_cancels_run_and_task_without_retry() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .handle_remote_outcome(
            "tsk_remote",
            "run_remote",
            RemoteOutcome::Cancelled {
                reason: "worker stopped".into(),
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Cancelled
    );
    assert!(h.in_memory_state().await.retry_queue.is_empty());
}

#[tokio::test]
async fn duplicate_terminal_envelope_does_not_change_stats_or_proof() {
    let fixture = PeerFixture::new(true).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .handle_remote_outcome("tsk_remote", "run_remote", success(), true)
        .await
        .unwrap();
    let task = h.task("tsk_remote");
    let stats = h.in_memory_state().await.stats;
    h.orchestrator
        .handle_remote_outcome(
            "tsk_remote",
            "run_remote",
            RemoteOutcome::Cancelled {
                reason: "duplicate late envelope".into(),
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote"), task);
    assert_eq!(h.in_memory_state().await.stats, stats);
}

#[tokio::test]
async fn mirror_task_failing_criteria_never_force_approves() {
    let mut agent = make_agent();
    agent.config.approval_policy = Some(ApprovalPolicy::Auto);
    let h = Harness::builder()
        .agents([agent])
        .tasks([Task {
            id: "tsk_mirror".into(),
            labels: vec!["remote-ctl:controller-id".into()],
            review_criteria: Some(vec![ReviewCriterion::TestPass]),
            ..make_task()
        }])
        .review_outcome(review_fail())
        .build();
    // The mirror only runs while its controller holds the session.
    h.orchestrator
        .set_inbound_controller(Some("controller-id".into()));
    h.set_state(OrchestratorState::default()).await;
    h.tick().await.unwrap();
    h.settle().await;
    assert_eq!(h.task("tsk_mirror").status, TaskStatus::Review);
    assert!(!h.task("tsk_mirror").review_results.unwrap()[0].passed);
    assert!(!h.events_of(OrchestratorEventType::OrchestratorError).iter().any(|event|
        matches!(event, OrchestratorEvent::OrchestratorError { context, .. } if context == "auto-review-with-auto-approve")));
}

#[tokio::test]
async fn mirror_prompt_is_verbatim_with_worker_role_skills_and_remote_instructions() {
    let description = "controller prompt\n  {{ literal }}\nend\n";
    let mut agent = Agent {
        role: Some("worker role".into()),
        ..make_agent()
    };
    agent.config.skills = Some(vec!["local-skill".into()]);
    let h = Harness::builder()
        .agents([agent])
        .skills([("local-skill", "worker skill content")])
        .tasks([Task {
            id: "tsk_mirror".into(),
            description: description.into(),
            labels: vec![
                "remote:controller".into(),
                "remote-ctl:controller-id".into(),
                "remote-from:lead".into(),
            ],
            ..make_task()
        }])
        .build();
    h.orchestrator
        .set_inbound_controller(Some("controller-id".into()));
    h.set_state(OrchestratorState::default()).await;
    h.tick().await.unwrap();
    h.settle().await;
    let call = h.adapter.execute_calls.first().expect("worker execute");
    assert_eq!(call.prompt, description);
    let system = call.system_prompt.unwrap();
    assert!(system.contains("worker role"));
    assert!(system.contains("worker skill content"));
    assert!(system.contains("## Remote execution"));
    assert!(system.contains("orch msg send lead@controller"));
    assert!(system.contains("controller files are not available"));
    assert!(system.contains("Do not use `orch task add`, `orch goal`, or `orch context`"));
    assert!(h.templates.calls.calls().is_empty());
}

#[tokio::test]
async fn capability_filter_routes_auto_assignment_to_matching_remote() {
    let fixture = PeerFixture::new(true).await;
    let h = fixture
        .builder()
        .agents([Agent {
            capabilities: vec!["macos".into()],
            ..make_agent()
        }])
        .tasks([Task {
            id: "tsk_linux".into(),
            labels: vec!["cap:linux".into()],
            ..make_task()
        }])
        .build();
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.set_state(OrchestratorState::default()).await;
    h.tick().await.unwrap();
    h.wait_for(|_| !bridge.submitted.lock().unwrap().is_empty())
        .await;
    assert_eq!(bridge.submitted.lock().unwrap()[0].agent_id, "agt_peer");
    assert!(h.adapter.execute_calls.calls().is_empty());
    h.orchestrator.cancel_task("tsk_linux").await.unwrap();
}

#[tokio::test]
async fn explicit_remote_assignee_with_wrong_capability_waits() {
    let fixture = PeerFixture::new(true).await;
    let h = fixture
        .builder()
        .tasks([Task {
            id: "tsk_mismatch".into(),
            assignee: Some("agt_peer@worker".into()),
            labels: vec!["cap:macos".into()],
            ..make_task()
        }])
        .build();
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.set_state(OrchestratorState::default()).await;
    h.tick().await.unwrap();
    assert_eq!(h.task("tsk_mismatch").status, TaskStatus::Todo);
    assert!(bridge.submitted.lock().unwrap().is_empty());
    assert!(h.in_memory_state().await.running.is_empty());
}

#[tokio::test]
async fn dropped_link_stream_leaves_execution_pending_without_retry() {
    let fixture = PeerFixture::new(false).await;
    let (h, _) = running(&fixture).await;
    h.orchestrator
        .collect_remote_events(
            Box::pin(futures::stream::empty()),
            "run_remote",
            "tsk_remote",
            "agt_peer@worker",
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let state = h.in_memory_state().await;
    assert_eq!(state.running.len(), 1);
    assert!(state.retry_queue.is_empty());
    assert_eq!(h.task("tsk_remote").status, TaskStatus::InProgress);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Running
    );
    no_process_calls(&h);
}

#[tokio::test]
async fn remote_finish_does_not_overwrite_an_already_cancelled_controller_task() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    let mut task = h.task("tsk_remote");
    task.status = TaskStatus::Cancelled;
    h.tasks.save(&task).await.unwrap();
    h.orchestrator
        .handle_remote_outcome("tsk_remote", "run_remote", success(), true)
        .await
        .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Cancelled
    );
    cancelled_once(&bridge);
}

#[tokio::test]
async fn unsatisfied_capability_task_does_not_starve_a_dispatchable_lower_priority_task() {
    let h = Harness::builder()
        .config(super::harness::config_with_concurrency(1))
        .agents([Agent {
            capabilities: vec!["linux".into()],
            ..make_agent()
        }])
        .tasks([
            Task {
                id: "tsk_waiting".into(),
                priority: 1,
                labels: vec!["cap:freebsd".into()],
                ..make_task()
            },
            Task {
                id: "tsk_ready".into(),
                priority: 2,
                ..make_task()
            },
        ])
        .build();
    h.tick().await.unwrap();
    h.settle().await;
    assert_eq!(h.task("tsk_waiting").status, TaskStatus::Todo);
    assert!(h.was_dispatched("tsk_ready"));
}

#[tokio::test]
async fn claimed_remote_agent_is_reserved_before_running_entry_exists() {
    let fixture = PeerFixture::new(true).await;
    let mut state = OrchestratorState::default();
    state.claimed.insert("tsk_claimed".into());
    let h = fixture
        .builder()
        .state(state)
        .tasks([
            Task {
                id: "tsk_claimed".into(),
                assignee: Some("agt_peer@worker".into()),
                ..make_task()
            },
            Task {
                id: "tsk_waiting".into(),
                ..make_task()
            },
        ])
        .build();
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.tick().await.unwrap();
    assert_eq!(h.task("tsk_waiting").status, TaskStatus::Todo);
    assert!(h.in_memory_state().await.running.is_empty());
    assert!(bridge.submitted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn transient_terminal_state_write_retries_before_ack_without_counting_completion_twice() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    h.state.faults.fail_once("transient terminal state write");
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        h.orchestrator.collect_remote_events(
            Box::pin(futures::stream::iter([RemoteRunEvent::Finished(success())])),
            "run_remote",
            "tsk_remote",
            "agt_peer@worker",
            true,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("terminal persistence retry must finish")
    .unwrap();
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Done);
    let persisted = h.persisted_state();
    assert!(persisted.running.is_empty());
    assert_eq!(persisted.stats.total_runs, 1);
    assert_eq!(persisted.stats.total_tasks_completed, 1);
    assert_eq!(
        h.state.written().len(),
        2,
        "one rejected write and one successful retry"
    );
    assert_eq!(
        *bridge.acknowledged.lock().unwrap(),
        vec![("worker".into(), "run_remote".into())]
    );
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .iter()
            .any(
                |event| matches!(event, OrchestratorEvent::OrchestratorError { error, .. }
            if error.contains("transient terminal state write"))
            )
    );
}

#[tokio::test]
async fn state_reload_flushes_pending_terminal_removal_before_replacing_memory() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    h.state
        .faults
        .fail_once("transient terminal state write before reload");
    let orchestrator = h.orchestrator.clone();
    let collector = tokio::spawn(async move {
        orchestrator
            .collect_remote_events(
                Box::pin(futures::stream::iter([RemoteRunEvent::Finished(success())])),
                "run_remote",
                "tsk_remote",
                "agt_peer@worker",
                true,
                CancellationToken::new(),
            )
            .await
    });
    h.wait_for(|h| h.state.written().len() == 1).await;
    assert_eq!(
        h.persisted_state().running.len(),
        1,
        "failed write leaves old disk state"
    );
    assert!(bridge.acknowledged.lock().unwrap().is_empty());

    // This is the same reload used by tick/cancel. It must flush the pending
    // terminal removal before reading the older state from the store.
    h.load_state().await.unwrap();
    assert!(h.in_memory_state().await.running.is_empty());
    assert!(h.persisted_state().running.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(3), collector)
        .await
        .expect("outcome collector must finish")
        .unwrap()
        .unwrap();

    assert_eq!(h.task("tsk_remote").status, TaskStatus::Done);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Succeeded
    );
    let persisted = h.persisted_state();
    assert!(persisted.running.is_empty());
    assert_eq!(persisted.stats.total_runs, 1);
    assert_eq!(persisted.stats.total_tasks_completed, 1);
    assert_eq!(
        *bridge.acknowledged.lock().unwrap(),
        vec![("worker".into(), "run_remote".into())]
    );
    assert!(bridge.cancelled.lock().unwrap().is_empty());
}

#[tokio::test]
async fn progress_log_failure_does_not_drop_authoritative_terminal_outcome() {
    let fixture = PeerFixture::new(true).await;
    let (h, bridge) = running(&fixture).await;
    h.runs
        .faults
        .fail_once("progress log temporarily unavailable");
    let progress = orch_core::adapter::AgentEvent::new(
        orch_core::adapter::AgentEventKind::Output,
        NOW_ISO,
        serde_json::json!({"text": "best effort progress"}),
    );
    h.orchestrator
        .collect_remote_events(
            Box::pin(futures::stream::iter([
                RemoteRunEvent::Event(progress),
                RemoteRunEvent::Finished(success()),
            ])),
            "run_remote",
            "tsk_remote",
            "agt_peer@worker",
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        h.runs.peek_events("run_remote").is_empty(),
        "the injected log append actually failed"
    );
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Done);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Succeeded
    );
    assert_eq!(
        h.task("tsk_remote").proof.unwrap().agent_summary.as_deref(),
        Some("worker result")
    );
    assert!(h.persisted_state().running.is_empty());
    assert_eq!(h.persisted_state().stats.total_runs, 1);
    assert_eq!(
        *bridge.acknowledged.lock().unwrap(),
        vec![("worker".into(), "run_remote".into())]
    );
    assert!(h.events_of(OrchestratorEventType::OrchestratorError).iter().any(|event|
        matches!(event, OrchestratorEvent::OrchestratorError { error, context, .. }
            if error.contains("progress log temporarily unavailable") && context.contains("record remote progress"))));
}

#[tokio::test]
async fn reassignment_to_local_does_not_merge_a_previous_worker_branch() {
    let agent = make_agent();
    let h = Harness::builder()
        .agents([agent.clone()])
        .tasks([Task {
            assignee: Some(agent.id.clone()),
            proof: Some(orch_domain::TaskProof {
                remote: Some("worker".into()),
                branch: Some("worker:topic".into()),
                ..Default::default()
            }),
            ..make_task()
        }])
        .build();
    h.tick().await.unwrap();
    h.settle().await;
    let proof = h.task(&make_task().id).proof.unwrap();
    assert!(proof.remote.is_none());
    assert!(proof.branch.is_none());
    assert!(h.workspace.merged().is_empty());
}

#[tokio::test]
async fn unknown_outcome_retries_run_task_and_state_errors_without_losing_the_collector() {
    for target in ["run", "task", "state"] {
        let fixture = PeerFixture::new(true).await;
        let (h, _) = running(&fixture).await;
        let cancel = CancellationToken::new();
        h.orchestrator
            .inner
            .cancels
            .lock()
            .await
            .insert("tsk_remote".into(), cancel.clone());
        match target {
            "run" => h.runs.faults.fail_once("transient unknown run write"),
            "task" => h.tasks.faults.fail_once("transient unknown task write"),
            _ => h.state.faults.fail_once("transient unknown state write"),
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            h.orchestrator.collect_remote_events(
                Box::pin(futures::stream::iter([RemoteRunEvent::Unknown])),
                "run_remote",
                "tsk_remote",
                "agt_peer@worker",
                true,
                cancel,
            ),
        )
        .await
        .expect("unknown outcome retry must finish")
        .unwrap();
        let persisted = h.persisted_state();
        assert!(
            persisted.running.is_empty(),
            "{target} error left a running entry"
        );
        assert!(persisted.retry_queue.is_empty());
        assert_eq!(persisted.stats.total_runs, 1);
        assert_eq!(h.task("tsk_remote").status, TaskStatus::Failed);
        assert_eq!(
            h.runs.peek("run_remote").unwrap().error.as_deref(),
            Some("remote lost execution record")
        );
    }
}

#[tokio::test]
async fn worker_mirror_without_criteria_autoapproves_under_suggest_policy() {
    let mut agent = make_agent();
    agent.config.approval_policy = Some(ApprovalPolicy::Suggest);
    let h = Harness::builder()
        .agents([agent])
        .tasks([Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            assignee: Some("agt_test1".into()),
            labels: vec!["remote-ctl:controller".into()],
            ..make_task()
        }])
        .state(state_running(
            "tsk_mirror",
            "agt_test1",
            "run_mirror",
            42,
            NOW_ISO,
        ))
        .build();
    h.set_state(state_running(
        "tsk_mirror",
        "agt_test1",
        "run_mirror",
        42,
        NOW_ISO,
    ))
    .await;
    h.runs.seed(Run {
        id: "run_mirror".into(),
        task_id: "tsk_mirror".into(),
        ..make_run()
    });
    h.orchestrator
        .handle_run_success(
            "tsk_mirror",
            "run_mirror",
            "agt_test1",
            None,
            Some("finished".into()),
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(h.task("tsk_mirror").status, TaskStatus::Done);
}

/// One local run occupies the only local slot (`max_concurrent_agents = 1`),
/// while the worker's agent is online and idle.
fn local_slot_full_with_idle_remote(fixture: &PeerFixture, extra: Vec<Task>) -> Harness {
    let mut tasks = vec![Task {
        id: "tsk_local_busy".into(),
        status: TaskStatus::InProgress,
        assignee: Some("agt_test1".into()),
        ..make_task()
    }];
    tasks.extend(extra);
    fixture
        .builder()
        .config(super::harness::config_with_concurrency(1))
        .agents([
            make_agent(),
            Agent {
                id: "agt_local_idle".into(),
                name: "LocalIdle".into(),
                capabilities: vec!["macos".into()],
                ..make_agent()
            },
        ])
        .tasks(tasks)
        .state(state_running(
            "tsk_local_busy",
            "agt_test1",
            "run_local_busy",
            4242,
            NOW_ISO,
        ))
        .build()
}

#[tokio::test]
async fn full_local_slots_still_dispatch_a_remote_eligible_task_to_an_idle_remote_agent() {
    let fixture = PeerFixture::new(true).await;
    let h = local_slot_full_with_idle_remote(
        &fixture,
        vec![Task {
            id: "tsk_for_worker".into(),
            labels: vec!["cap:linux".into()],
            ..make_task()
        }],
    );
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    let submitted: Vec<String> = bridge
        .submitted
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.agent_id.clone())
        .collect();
    assert_eq!(submitted, vec!["agt_peer".to_owned()]);
    let state = h.in_memory_state().await;
    assert!(state.running["tsk_for_worker"].remote.is_some());
    assert!(state.running.contains_key("tsk_local_busy"));
    assert!(h.adapter.execute_calls.calls().is_empty());
    h.orchestrator.cancel_task("tsk_for_worker").await.unwrap();
}

#[tokio::test]
async fn full_local_slots_hold_a_local_only_task_while_a_remote_task_dispatches() {
    let fixture = PeerFixture::new(true).await;
    let h = local_slot_full_with_idle_remote(
        &fixture,
        vec![
            // Ahead in the queue and only a local (macOS) agent can take it.
            Task {
                id: "tsk_local_only".into(),
                priority: 1,
                labels: vec!["cap:macos".into()],
                ..make_task()
            },
            Task {
                id: "tsk_for_worker".into(),
                priority: 2,
                labels: vec!["cap:linux".into()],
                ..make_task()
            },
        ],
    );
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.task("tsk_local_only").status, TaskStatus::Todo);
    assert!(h.adapter.execute_calls.calls().is_empty());
    let state = h.in_memory_state().await;
    assert!(!state.running.contains_key("tsk_local_only"));
    assert!(!state.claimed.contains("tsk_local_only"));
    assert_eq!(bridge.submitted.lock().unwrap().len(), 1);
    assert!(state.running["tsk_for_worker"].remote.is_some());
    h.orchestrator.cancel_task("tsk_for_worker").await.unwrap();
}

#[tokio::test]
async fn an_in_flight_remote_run_does_not_use_the_local_slot() {
    let fixture = PeerFixture::new(true).await;
    let h = fixture
        .builder()
        .config(super::harness::config_with_concurrency(1))
        .agents([make_agent()])
        .tasks([
            Task {
                id: "tsk_remote".into(),
                status: TaskStatus::InProgress,
                assignee: Some("agt_peer@worker".into()),
                attempts: 1,
                ..make_task()
            },
            Task {
                id: "tsk_local".into(),
                ..make_task()
            },
        ])
        .state(remote_state(NOW_ISO))
        .build();
    h.runs.seed(Run {
        id: "run_remote".into(),
        task_id: "tsk_remote".into(),
        agent_id: "agt_peer@worker".into(),
        status: RunStatus::Running,
        pid: Some(0),
        ..make_run()
    });
    let bridge = Arc::new(FakeBridge::for_harness(&h));
    h.orchestrator.set_remote_bridge(bridge.clone());
    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.adapter.execute_calls.calls().len(), 1);
    assert!(h.was_dispatched("tsk_local"));
    assert!(h.in_memory_state().await.running.contains_key("tsk_remote"));
    h.settle().await;
}

#[tokio::test]
async fn worker_quarantine_missing_agent_does_not_abort_startup_or_release() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            labels: vec!["remote-ctl:controller".into()],
            ..make_task()
        }])
        .state(state_running(
            "tsk_mirror",
            "agt_missing",
            "run_mirror",
            4321,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_mirror".into(),
        task_id: "tsk_mirror".into(),
        agent_id: "agt_missing".into(),
        status: RunStatus::Running,
        pid: Some(4321),
        ..make_run()
    });
    h.processes.set_alive(4321);
    h.start_watch_skipping_seeding().await.unwrap();
    assert_eq!(h.persisted_state().running.len(), 1);
    h.processes.set_dead(4321);
    // Exercise startup's own persistence, before the outer watch/tick save
    // can hide a status-write error after the in-memory removal.
    let mut restarted = h.persisted_state();
    h.orchestrator
        .cleanup_stale_running_entries(&mut restarted)
        .await
        .unwrap();
    assert!(restarted.running.is_empty());
    assert!(h.persisted_state().running.is_empty());
    assert!(h.runs.peek("run_mirror").unwrap().pid.is_none());
    h.orchestrator.stop().await.unwrap();
    let mut state = h.persisted_state();
    h.orchestrator
        .cleanup_stale_running_entries(&mut state)
        .await
        .unwrap();
    assert!(state.running.is_empty());
}

#[tokio::test]
async fn worker_quarantine_store_fault_does_not_abort_tick_or_startup() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            labels: vec!["remote-ctl:controller".into()],
            ..make_task()
        }])
        .build();
    // Per-mirror failure must not skip the rest of orphan reconciliation.
    h.runs.faults.fail_once("mirror run read fault");
    h.reconcile().await.unwrap();
    h.orchestrator.invalidate_caches().await;
    h.reconcile().await.unwrap();
    assert_eq!(h.task("tsk_mirror").status, TaskStatus::Cancelled);
    h.tasks.faults.fail_once("mirror list fault");
    let mut state = h.persisted_state();
    h.orchestrator
        .cleanup_stale_running_entries(&mut state)
        .await
        .unwrap();
}

#[cfg(unix)]
struct QuarantineDecoy(std::process::Child);

#[cfg(unix)]
impl Drop for QuarantineDecoy {
    fn drop(&mut self) {
        // This child owns its process group and has no descendants.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
async fn quarantine_decoy_survives(action: &str) {
    use std::os::unix::process::CommandExt;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(temp.path().join(".orchestry")).unwrap();
    let mut decoy = QuarantineDecoy(
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .env_clear()
            .env("HOME", &home)
            .current_dir(temp.path())
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = decoy.0.id();
    let release_without_task = matches!(action, "missing-release" | "malformed-release");
    if release_without_task {
        use orch_core::storage::ConfigStore;
        let configs = orch_infra::storage::FileConfigStore::new(Paths::new(temp.path()));
        let mut config = configs.read().await.unwrap();
        config.scheduling.poll_interval_ms = 100;
        configs.write(&config).await.unwrap();
    }
    let e = crate::build_full_with(
        temp.path(),
        crate::EngineSeams {
            global_config_path: Some(home.join("global.yml")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    e.agents
        .save(&orch_domain::Agent {
            adapter: "shell".into(),
            config: orch_domain::AgentConfig {
                command: Some("/usr/bin/true".into()),
                ..make_agent().config
            },
            ..make_agent()
        })
        .await
        .unwrap();
    e.tasks
        .save(&Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            labels: vec!["remote-ctl:controller".into()],
            ..make_task()
        })
        .await
        .unwrap();
    e.runs
        .save(&Run {
            id: "run_mirror".into(),
            task_id: "tsk_mirror".into(),
            status: RunStatus::Running,
            pid: Some(pid),
            ..make_run()
        })
        .await
        .unwrap();
    let mut state = state_running("tsk_mirror", "agt_test1", "run_mirror", pid, NOW_ISO);
    if release_without_task {
        e.state.write(&state).await.unwrap();
        e.orchestrator
            .start_watch(crate::WatchOptions {
                skip_autonomous_seeding: true,
                handle_signals: false,
            })
            .await
            .unwrap();
    } else {
        e.orchestrator
            .cleanup_stale_running_entries(&mut state)
            .await
            .unwrap();
    }
    let diagnostic = e
        .runs
        .get("run_mirror")
        .await
        .unwrap()
        .unwrap()
        .error
        .unwrap();
    assert!(diagnostic.contains("outcome unknown"));
    if release_without_task {
        let task = e.tasks.get("tsk_mirror").await.unwrap().unwrap();
        let task_path = e.paths.task_path("tsk_mirror").unwrap();
        let index_path = task_path.parent().unwrap().join("_index.json");
        let index = std::fs::read(&index_path).unwrap();
        if action == "missing-release" {
            std::fs::remove_file(&task_path).unwrap();
        } else {
            std::fs::write(&task_path, "[: broken yaml").unwrap();
        }
        for result in [
            e.orchestrator.cancel_task("tsk_mirror").await,
            e.orchestrator.force_stop_agent("agt_test1").await,
        ] {
            assert_eq!(
                result.unwrap_err().to_string(),
                "Worker execution ownership cannot verify; not signalled; reservation retained"
            );
        }
        // A completed reconcile with the faulty task must retain a live child.
        e.orchestrator.tick_once().await.unwrap();
        assert!(decoy.0.try_wait().unwrap().is_none());
        assert!(
            e.state
                .read()
                .await
                .unwrap()
                .running
                .contains_key("tsk_mirror")
        );
        let run = e.runs.get("run_mirror").await.unwrap().unwrap();
        assert_eq!(run.pid, Some(pid));
        assert_eq!(run.error.as_deref(), Some(diagnostic.as_str()));
        decoy.0.kill().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(decoy.0.wait().unwrap().signal(), Some(9));
        let release_started = std::time::Instant::now();
        // Keep the task faulty throughout: only the actual 100 ms watcher
        // may release the dead reservation, not a task-file repair.
        let released = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !e
                    .state
                    .read()
                    .await
                    .unwrap()
                    .running
                    .contains_key("tsk_mirror")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        let release_elapsed = release_started.elapsed();
        // Stop even when the release assertion will fail.
        let stop_started = std::time::Instant::now();
        tokio::time::timeout(std::time::Duration::from_secs(5), e.orchestrator.stop())
            .await
            .unwrap()
            .unwrap();
        let stop_elapsed = stop_started.elapsed();
        released.expect("dead quarantine must release without a readable task");
        let run = e.runs.get("run_mirror").await.unwrap().unwrap();
        assert_eq!(run.pid, None);
        assert_eq!(run.error.as_deref(), Some(diagnostic.as_str()));
        assert_eq!(std::fs::read(&index_path).unwrap(), index);
        if action == "missing-release" {
            assert!(!task_path.exists());
        } else {
            assert_eq!(
                std::fs::read_to_string(&task_path).unwrap(),
                "[: broken yaml"
            );
        }
        // Rebuild and restart while the individual file remains faulty.
        let restarted = crate::build_full_with(
            temp.path(),
            crate::EngineSeams {
                global_config_path: Some(home.join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        restarted
            .orchestrator
            .start_watch(crate::WatchOptions {
                skip_autonomous_seeding: true,
                handle_signals: false,
            })
            .await
            .unwrap();
        assert!(restarted.state.read().await.unwrap().running.is_empty());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            restarted.orchestrator.stop(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            restarted.runs.get("run_mirror").await.unwrap().unwrap(),
            run
        );
        // Simulate reuse of the old numeric PID. Use the real persisted run
        // and index task with fake liveness, rather than waiting for OS reuse.
        let h = Harness::builder()
            .tasks([task])
            .agents([make_agent()])
            .build();
        h.runs.seed(run);
        h.processes.set_alive(pid);
        let mut state = h.persisted_state();
        h.orchestrator
            .cleanup_stale_running_entries(&mut state)
            .await
            .unwrap();
        assert!(state.running.is_empty());
        assert!(h.processes.killed_with_grace().is_empty());
        eprintln!(
            "{action}: reaped PID {pid}; release {release_elapsed:?}; stop {stop_elapsed:?}; restart/reuse held no reservation"
        );
        return;
    }
    *e.orchestrator.inner.state.lock().await = Some(state.clone());
    let mut rejected = None;
    match action {
        "force" => {
            rejected = Some(e.orchestrator.force_stop_agent("agt_test1").await);
        }
        "cancel" => {
            rejected = Some(e.orchestrator.cancel_task("tsk_mirror").await);
        }
        "malformed" => {
            std::fs::write(e.paths.task_path("tsk_mirror").unwrap(), "[: broken yaml").unwrap()
        }
        "missing" => std::fs::remove_file(e.paths.task_path("tsk_mirror").unwrap()).unwrap(),
        "retry" => {
            e.task_service.retry("tsk_mirror").await.unwrap();
        }
        "prefix-loss" => {
            let mut task = e.tasks.get("tsk_mirror").await.unwrap().unwrap();
            task.feedback = None;
            task.labels.clear();
            e.tasks.save(&task).await.unwrap();
            e.orchestrator.reconcile(&mut state).await.unwrap();
            assert!(state.running.contains_key("tsk_mirror"));
            // Even if graceful stop cleared the state map and the task's
            // labels/feedback were lost, the durable run marker reconstructs
            // the live reservation on restart.
            let mut task = e.tasks.get("tsk_mirror").await.unwrap().unwrap();
            task.labels.clear();
            task.feedback = None;
            e.tasks.save(&task).await.unwrap();
            state.running.clear();
            e.orchestrator
                .cleanup_stale_running_entries(&mut state)
                .await
                .unwrap();
            assert!(state.running.contains_key("tsk_mirror"));
            assert_eq!(
                e.runs
                    .get("run_mirror")
                    .await
                    .unwrap()
                    .unwrap()
                    .error
                    .as_deref(),
                Some(diagnostic.as_str())
            );
            // Reap to distinguish exit from zombie liveness, then release.
            assert!(
                decoy.0.try_wait().unwrap().is_none(),
                "decoy was signalled during reconcile"
            );
            decoy.0.kill().unwrap();
            decoy.0.wait().unwrap();
            e.orchestrator.reconcile(&mut state).await.unwrap();
            assert!(!state.running.contains_key("tsk_mirror"));
            assert!(
                e.runs
                    .get("run_mirror")
                    .await
                    .unwrap()
                    .unwrap()
                    .pid
                    .is_none()
            );
            *e.orchestrator.inner.state.lock().await = Some(state);
            e.orchestrator.stop().await.unwrap();
            return;
        }
        "normal" => {}
        _ => panic!("unknown quarantine action"),
    }
    let status = decoy.0.try_wait().unwrap();
    assert!(
        status.is_none(),
        "unverified decoy exited after {action}: {status:?}"
    );
    if let Some(result) = rejected {
        assert!(result.unwrap_err().to_string().contains("not signalled"));
    }
    if matches!(action, "force" | "cancel") {
        assert!(
            e.state
                .read()
                .await
                .unwrap()
                .running
                .contains_key("tsk_mirror")
        );
        assert!(
            e.orchestrator
                .inner
                .state
                .lock()
                .await
                .as_ref()
                .unwrap()
                .running
                .contains_key("tsk_mirror")
        );
    }
    let started = std::time::Instant::now();
    tokio::time::timeout(std::time::Duration::from_secs(5), e.orchestrator.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let status = decoy.0.try_wait().unwrap();
    assert!(status.is_none(), "unverified decoy exited: {status:?}");
    assert_eq!(
        e.runs
            .get("run_mirror")
            .await
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some(diagnostic.as_str())
    );
    // Survival above is an actual unreaped child check; also verify cleanup's
    // reaped exit status, so a zombie cannot satisfy the test.
    decoy.0.kill().unwrap();
    let status = decoy.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(9));
}

#[cfg(unix)]
macro_rules! quarantine_decoy_test {
    ($name:ident, $action:literal) => {
        #[tokio::test]
        async fn $name() {
            quarantine_decoy_survives($action).await;
        }
    };
}
#[cfg(unix)]
quarantine_decoy_test!(worker_quarantine_decoy_force_stop, "force");
#[cfg(unix)]
quarantine_decoy_test!(worker_quarantine_decoy_cancel, "cancel");
#[cfg(unix)]
quarantine_decoy_test!(worker_quarantine_decoy_malformed_task_stop, "malformed");
#[cfg(unix)]
quarantine_decoy_test!(worker_quarantine_decoy_missing_task_stop, "missing");
#[cfg(unix)]
quarantine_decoy_test!(worker_quarantine_decoy_retry_then_stop, "retry");
#[cfg(unix)]
quarantine_decoy_test!(worker_quarantine_decoy_normal_stop, "normal");
#[cfg(unix)]
quarantine_decoy_test!(
    worker_quarantine_decoy_prefix_loss_reconcile_releases_after_exit,
    "prefix-loss"
);

#[tokio::test]
async fn worker_mirror_current_collector_cancel_still_signals() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_mirror".into(),
            status: TaskStatus::InProgress,
            labels: vec!["remote-ctl:controller".into()],
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_mirror",
            "agt_test1",
            "run_mirror",
            4321,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_mirror".into(),
        task_id: "tsk_mirror".into(),
        status: RunStatus::Running,
        pid: Some(4321),
        ..make_run()
    });
    h.orchestrator
        .inner
        .active_collectors
        .lock()
        .await
        .insert("tsk_mirror".into());
    h.orchestrator.cancel_task("tsk_mirror").await.unwrap();
    assert_eq!(
        h.processes
            .killed_with_grace()
            .iter()
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>(),
        vec![4321]
    );
    assert!(h.persisted_state().running.is_empty());
}

#[cfg(unix)]
quarantine_decoy_test!(
    worker_quarantine_missing_task_releases_dead_reservation_without_restoring_record,
    "missing-release"
);
#[cfg(unix)]
quarantine_decoy_test!(
    worker_quarantine_malformed_task_releases_dead_reservation_without_restoring_record,
    "malformed-release"
);

#[tokio::test]
async fn worker_quarantine_missing_task_retains_dead_reservation_until_run_record_readable() {
    let h = Harness::builder()
        .agents([make_agent()])
        .state(state_running(
            "tsk_mirror",
            "agt_test1",
            "run_mirror",
            4321,
            NOW_ISO,
        ))
        .build();
    let diagnostic = "Worker execution collector lost; outcome unknown; PID 4321 not signalled";
    h.runs.seed(Run {
        id: "run_mirror".into(),
        task_id: "tsk_mirror".into(),
        status: RunStatus::Cancelled,
        pid: Some(4321),
        error: Some(diagnostic.into()),
        ..make_run()
    });
    h.processes.set_dead(4321);
    h.runs.faults.fail_always("run record unreadable");
    h.reconcile().await.unwrap();
    assert!(h.persisted_state().running.contains_key("tsk_mirror"));
    assert_eq!(h.runs.peek("run_mirror").unwrap().pid, Some(4321));
    h.runs.faults.clear();
    h.reconcile().await.unwrap();
    assert!(h.persisted_state().running.is_empty());
    let run = h.runs.peek("run_mirror").unwrap();
    assert_eq!(run.pid, None);
    assert_eq!(run.error.as_deref(), Some(diagnostic));
    assert!(h.processes.killed_with_grace().is_empty());
}

#[tokio::test]
async fn local_collectorless_missing_task_releases_dead_reservation_without_signalling() {
    // CR C3: no task or remote-ctl label, so only the saved entry/run can
    // drive lifecycle cleanup. The conservative signal guard stays intact.
    let h = Harness::builder()
        .agents([make_agent()])
        .state(state_running(
            "tsk_local",
            "agt_test1",
            "run_local",
            4321,
            NOW_ISO,
        ))
        .build();
    let run = Run {
        id: "run_local".into(),
        task_id: "tsk_local".into(),
        status: RunStatus::Running,
        pid: Some(4321),
        error: Some("original local diagnostic".into()),
        ..make_run()
    };
    h.runs.seed(run.clone());
    h.processes.set_alive(4321);
    h.reconcile().await.unwrap();
    assert!(h.persisted_state().running.contains_key("tsk_local"));
    for result in [
        h.orchestrator.cancel_task("tsk_local").await,
        h.orchestrator.force_stop_agent("agt_test1").await,
    ] {
        assert_eq!(
            result.unwrap_err().to_string(),
            "Worker execution ownership cannot verify; not signalled; reservation retained"
        );
    }
    h.orchestrator
        .shutdown_running_entries(&h.persisted_state())
        .await;
    assert!(h.processes.killed_with_grace().is_empty());
    assert_eq!(h.runs.peek("run_local").unwrap(), run);
    h.processes.set_dead(4321);
    h.orchestrator.invalidate_caches().await;
    h.reconcile().await.unwrap();
    assert!(h.persisted_state().running.is_empty());
    let mut released = run;
    released.pid = None;
    assert_eq!(h.runs.peek("run_local").unwrap(), released);
    assert_eq!(
        h.agents.peek("agt_test1").unwrap().status,
        orch_domain::AgentStatus::Idle
    );
    assert!(h.processes.killed_with_grace().is_empty());
}

/// A worker must not start a mirror whose `remote-ctl:` controller is not the
/// session it is serving now: only that controller can be told the run started
/// and only it will collect the outcome, so running the mirror for anyone else
/// spends an agent on work nobody is waiting for. In the live failure the stale
/// mirror of a long-gone controller sat dispatchable on the worker forever.
#[tokio::test]
async fn a_mirror_is_dispatched_only_while_its_own_controller_is_connected() {
    use orch_core::test_support::{FakeAdapter, done_event};
    let adapter = FakeAdapter::new();
    adapter.set_events([done_event(NOW_ISO)]);
    let h = Harness::builder()
        .agents([make_agent()])
        .tasks([Task {
            id: "tsk_mirror".into(),
            assignee: Some("agt_test1".into()),
            labels: vec![
                "remote:mac".into(),
                "remote-ctl:controller-a".into(),
                "exec:run_a".into(),
            ],
            ..make_task()
        }])
        .adapter(Arc::new(adapter))
        .build();
    h.load_state().await.unwrap();

    // Nobody connected, then a different controller connected: neither may
    // start this mirror.
    for controller in [None, Some("controller-b".to_owned())] {
        h.orchestrator.set_inbound_controller(controller);
        h.orchestrator.invalidate_caches().await;
        h.tick().await.unwrap();
        assert!(!h.was_dispatched("tsk_mirror"));
        assert_eq!(h.task("tsk_mirror").status, TaskStatus::Todo);
        assert!(h.adapter.execute_calls.is_empty());
    }

    h.orchestrator
        .set_inbound_controller(Some("controller-a".into()));
    h.orchestrator.invalidate_caches().await;
    h.tick().await.unwrap();
    assert!(h.was_dispatched("tsk_mirror"));
    assert_eq!(h.adapter.execute_calls.len(), 1);
}
