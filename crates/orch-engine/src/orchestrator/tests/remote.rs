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
    h.orchestrator
        .shutdown_running_entries(&remote_state(NOW_ISO))
        .await;
    assert_eq!(h.task("tsk_remote").status, TaskStatus::Cancelled);
    assert_eq!(
        h.runs.peek("run_remote").unwrap().status,
        RunStatus::Cancelled
    );
    assert!(h.in_memory_state().await.retry_queue.is_empty());
    cancelled_once(&bridge);
    no_process_calls(&h);
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
