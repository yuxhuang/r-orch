//! Core orchestrator behaviour.

use std::sync::Arc;

use orch_core::test_support::{
    FakeAdapter, done_event, make_agent, make_run, make_task, output_event,
};
use orch_domain::{
    Agent, AgentStatus, OrchestratorEvent, OrchestratorEventType, OrchestratorState, Run,
    RunStatus, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, config_with_concurrency, iso_before, state_running};

fn task_at(id: &str, status: TaskStatus) -> Task {
    Task {
        id: id.into(),
        status,
        ..make_task()
    }
}

fn agent_at(id: &str, status: AgentStatus) -> Agent {
    Agent {
        id: id.into(),
        status,
        ..make_agent()
    }
}

fn done_adapter() -> Arc<FakeAdapter> {
    let adapter = FakeAdapter::new();
    adapter.set_events([done_event(NOW_ISO)]);
    Arc::new(adapter)
}

// ── constructor ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_new_orchestrator_is_not_the_lock_owner() {
    let h = Harness::builder().build();
    assert!(!h.orchestrator.is_owner());
}

// ── lock ownership ───────────────────────────────────────────────────────

#[tokio::test]
async fn run_task_reports_a_lock_conflict_with_the_holders_pid() {
    let h = Harness::builder().build();
    h.lock.set_conflict(Some(9999));

    let err = h
        .orchestrator
        .run_task("tsk_1")
        .await
        .expect_err("conflict");
    assert_eq!(err.to_string(), "Orchestrator already running (PID: 9999)");
    assert_eq!(err.exit_code(), 4);
}

#[tokio::test]
async fn run_all_reports_a_lock_conflict_with_the_holders_pid() {
    let h = Harness::builder().build();
    h.lock.set_conflict(Some(9999));

    let err = h.orchestrator.run_all().await.expect_err("conflict");
    assert_eq!(err.exit_code(), 4);
}

#[tokio::test]
async fn run_task_releases_the_lock_even_when_the_task_does_not_exist() {
    let h = Harness::builder().build();
    let err = h
        .orchestrator
        .run_task("tsk_nonexistent")
        .await
        .expect_err("missing task");
    assert_eq!(err.to_string(), "Task not found: tsk_nonexistent");
    assert_eq!(h.lock.acquire_count(), 1);
    assert_eq!(h.lock.release_count(), 1);
    assert!(!h.orchestrator.is_owner());
}

#[tokio::test]
async fn run_task_does_not_reactive_dispatch_the_other_ready_tasks() {
    // A single-task run must not consume the next ready task just because the
    // agent it used became idle.
    let h = Harness::builder()
        .tasks([
            task_at("tsk_1", TaskStatus::Todo),
            task_at("tsk_2", TaskStatus::Todo),
        ])
        .agents([agent_at("agt_1", AgentStatus::Idle)])
        .adapter(done_adapter())
        .build();

    h.orchestrator.run_task("tsk_1").await.unwrap();
    h.wait_for(|h| h.task("tsk_1").status == TaskStatus::Done)
        .await;

    // Past the 500 ms debounce a reactive dispatch would have fired by now.
    tokio::time::sleep(std::time::Duration::from_millis(650)).await;

    assert_eq!(h.task("tsk_2").status, TaskStatus::Todo);
    assert_eq!(h.runs.snapshot().len(), 1);
}

#[tokio::test]
async fn a_task_awaiting_approval_is_never_dispatched_until_released() {
    let held = Task {
        labels: vec![orch_domain::AWAITING_APPROVAL_LABEL.into()],
        ..task_at("tsk_1", TaskStatus::Todo)
    };
    let h = Harness::builder()
        .tasks([held])
        .agents([agent_at("agt_1", AgentStatus::Idle)])
        .adapter(done_adapter())
        .build();

    h.orchestrator.run_all().await.unwrap();
    h.orchestrator.run_task("tsk_1").await.unwrap();
    assert_eq!(
        (h.task("tsk_1").status, h.runs.snapshot().len()),
        (TaskStatus::Todo, 0)
    );

    h.orchestrator
        .deps()
        .task_service
        .approve("tsk_1")
        .await
        .unwrap();
    h.orchestrator.run_task("tsk_1").await.unwrap();
    h.wait_for(|h| h.task("tsk_1").status == TaskStatus::Done)
        .await;
    assert_eq!(h.runs.snapshot().len(), 1);
}

#[tokio::test]
async fn cancel_task_takes_a_temporary_lock_when_it_does_not_own_one() {
    let h = Harness::builder().build();
    h.orchestrator.cancel_task("tsk_1").await.unwrap();
    assert_eq!(h.lock.acquire_count(), 1);
    assert_eq!(h.lock.release_count(), 1);
    assert!(!h.orchestrator.is_owner());
}

#[tokio::test]
async fn force_stop_agent_takes_a_temporary_lock_when_it_does_not_own_one() {
    let h = Harness::builder()
        .agents([agent_at("agt_1", AgentStatus::Running)])
        .build();
    h.orchestrator.force_stop_agent("agt_1").await.unwrap();
    assert_eq!(h.lock.acquire_count(), 1);
    assert_eq!(h.lock.release_count(), 1);
    assert!(!h.orchestrator.is_owner());
    assert_eq!(h.agent("agt_1").status, AgentStatus::Idle);
}

// ── start_watch + stop ───────────────────────────────────────────────────

#[tokio::test]
async fn start_watch_takes_the_lock_and_records_the_holder() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .build();

    h.start_watch().await.unwrap();

    assert!(h.orchestrator.is_owner());
    let persisted = h.persisted_state();
    assert_eq!(persisted.pid, Some(std::process::id()));
    assert!(persisted.started_at.is_some());
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn stop_releases_the_lock_and_clears_the_holder() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    assert!(h.orchestrator.is_owner());

    h.orchestrator.stop().await.unwrap();
    assert!(!h.orchestrator.is_owner());
    assert!(h.lock.release_count() >= 1);
    let persisted = h.persisted_state();
    assert_eq!(persisted.pid, None);
    assert_eq!(persisted.started_at, None);
}

#[tokio::test]
async fn stop_is_idempotent() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    h.orchestrator.stop().await.unwrap();
    h.orchestrator.stop().await.unwrap();
    // The second call is a no-op, so the lock was released exactly once.
    assert_eq!(h.lock.release_count(), 1);
}

#[tokio::test]
async fn stop_kills_running_agents_and_leaves_their_tasks_retryable() {
    let task = Task {
        status: TaskStatus::InProgress,
        attempts: 1,
        ..make_task()
    };
    let agent = Agent {
        status: AgentStatus::Running,
        ..make_agent()
    };
    let run = Run {
        status: RunStatus::Running,
        pid: Some(54321),
        ..make_run()
    };
    let h = Harness::builder()
        .tasks([task.clone()])
        .agents([agent])
        .state(state_running(
            &task.id,
            "agt_test1",
            "run_test1",
            54321,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(run);

    h.own_lock();
    h.load_state().await.unwrap();
    h.orchestrator.stop().await.unwrap();

    assert_eq!(
        h.processes.killed_with_grace(),
        vec![(54321, orch_core::process::DEFAULT_KILL_GRACE)]
    );
    // attempts (1) < max_attempts (3), so it goes back to `retrying`.
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);
    assert_eq!(
        h.runs.peek("run_test1").unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert!(h.persisted_state().running.is_empty());
}

// ── tick: reconcile ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_tick_emits_the_running_and_queued_counts() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    let ticks = h.events_of(OrchestratorEventType::OrchestratorTick);
    assert!(!ticks.is_empty());
    match &ticks[0] {
        OrchestratorEvent::OrchestratorTick { running, queued } => {
            // `running` is read from live state, so it already counts the
            // dispatch this tick performed. `queued` is read through the
            // tick-scoped task cache, which `dispatch_all` populated *before*
            // dispatching — so the same task is counted in both. That is
            // what the TUI shows.
            assert_eq!(*running, 1);
            assert_eq!(*queued, 1);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn reconcile_fails_orphaned_in_progress_tasks_and_says_so() {
    // In `in_progress` but absent from `state.running`.
    let h = Harness::builder()
        .tasks([task_at("tsk_orphan", TaskStatus::InProgress)])
        .agents([make_agent()])
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    let orphaned = h.events_of(OrchestratorEventType::TaskOrphaned);
    assert_eq!(orphaned.len(), 1);
    match &orphaned[0] {
        OrchestratorEvent::TaskOrphaned { task_id } => assert_eq!(task_id, "tsk_orphan"),
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(h.task("tsk_orphan").status, TaskStatus::Failed);
}

#[tokio::test]
async fn reconcile_resets_an_agent_stuck_in_running() {
    let h = Harness::builder()
        .agents([agent_at("agt_test1", AgentStatus::Running)])
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
}

#[tokio::test]
async fn reconcile_drops_a_running_entry_whose_task_is_already_terminal() {
    let h = Harness::builder()
        .tasks([task_at("tsk_dead", TaskStatus::Failed)])
        .agents([make_agent()])
        .state(state_running(
            "tsk_dead",
            "agt_test1",
            "run_dead",
            88888,
            NOW_ISO,
        ))
        .build();
    h.processes.set_dead(88888);

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(!h.persisted_state().running.contains_key("tsk_dead"));
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
}

/// Keeps its stream open until the run's cancellation token fires, the way a
/// real adapter's child keeps working until the cancel path kills it.
#[derive(Default)]
struct CancelAwareAdapter {
    tokens: std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>,
}

#[async_trait::async_trait]
impl orch_core::adapter::Adapter for CancelAwareAdapter {
    fn kind(&self) -> &'static str {
        "shell"
    }

    async fn test(&self) -> orch_core::adapter::AdapterTestResult {
        orch_core::adapter::AdapterTestResult::ok("1.0.0")
    }

    fn execute(
        &self,
        params: orch_core::adapter::ExecuteParams,
    ) -> std::result::Result<orch_core::adapter::ExecuteHandle, orch_core::adapter::AdapterError>
    {
        use futures::StreamExt;
        let token = params.cancel.clone();
        self.tokens.lock().unwrap().push(token.clone());
        Ok(orch_core::adapter::ExecuteHandle {
            pid: 99999,
            events: Box::pin(
                futures::stream::once(async move { token.cancelled().await }).filter_map(
                    |()| async {
                        None::<
                            std::result::Result<
                                orch_core::adapter::AgentEvent,
                                orch_core::adapter::AdapterError,
                            >,
                        >
                    },
                ),
            ),
        })
    }

    async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn reconcile_cancels_a_live_local_run_whose_task_was_made_terminal_externally() {
    use orch_core::storage::TaskStore;

    let adapter = Arc::new(CancelAwareAdapter::default());
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter_impl(adapter.clone() as Arc<dyn orch_core::adapter::Adapter>)
        .build();
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    assert!(h.in_memory_state().await.running.contains_key("tsk_test1"));
    let token = adapter.tokens.lock().unwrap()[0].clone();
    assert!(!token.is_cancelled());

    // Another process (`orch task cancel`, a remote cancel) makes the task
    // terminal without going through this engine.
    let mut task = h.task("tsk_test1");
    task.status = TaskStatus::Cancelled;
    h.tasks.save(&task).await.unwrap();
    h.tick().await.unwrap();

    assert!(
        token.is_cancelled(),
        "the local collector must be cancelled, not left running"
    );
    assert!(!h.persisted_state().running.contains_key("tsk_test1"));
    assert!(
        h.orchestrator
            .inner
            .cancels
            .lock()
            .await
            .get("tsk_test1")
            .is_none()
    );
    h.settle().await;
}

#[tokio::test]
async fn reconcile_drops_a_stale_entry_for_a_done_task_without_checking_the_clock() {
    let long_ago = iso_before(1_000_000);
    let h = Harness::builder()
        .tasks([task_at("tsk_stall", TaskStatus::Done)])
        .agents([make_agent()])
        .state(state_running(
            "tsk_stall",
            "agt_test1",
            "run_stall",
            77777,
            &long_ago,
        ))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(!h.persisted_state().running.contains_key("tsk_stall"));
    // Terminal tasks take the cleanup path, so no stall is reported.
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorStallDetected)
            .is_empty()
    );
}

#[tokio::test]
async fn reconcile_emits_stall_detected_and_kills_a_stalled_run() {
    // `make_agent` sets stall_timeout_ms = 300_000.
    let long_ago = iso_before(400_000);
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_stall".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_stall",
            "agt_test1",
            "run_stall",
            77777,
            &long_ago,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_stall".into(),
        task_id: "tsk_stall".into(),
        ..make_run()
    });

    h.reconcile().await.unwrap();

    let stalls = h.events_of(OrchestratorEventType::OrchestratorStallDetected);
    assert_eq!(stalls.len(), 1);
    assert_eq!(
        h.processes.killed_with_grace(),
        vec![(77777, std::time::Duration::from_millis(5_000))]
    );
    assert!(!h.persisted_state().running.contains_key("tsk_stall"));
    assert_eq!(h.task("tsk_stall").status, TaskStatus::Retrying);
}

#[tokio::test]
async fn reconcile_handles_a_dead_pid_through_the_failure_path_without_deadlocking() {
    // The deadlock this guards: reconcile runs *inside* the state lock, so it
    // must use the lock-free failure handler, not the one that re-acquires it.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_deadpid".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_deadpid",
            "agt_test1",
            "run_deadpid",
            99999,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_deadpid".into(),
        task_id: "tsk_deadpid".into(),
        ..make_run()
    });
    h.processes.set_dead(99999);

    let ticked = tokio::time::timeout(std::time::Duration::from_secs(5), h.reconcile()).await;
    assert!(ticked.is_ok(), "reconcile deadlocked");
    ticked.unwrap().unwrap();

    let state = h.persisted_state();
    assert!(!state.running.contains_key("tsk_deadpid"));
    // attempts 1 < max 3, so a crash mid-run is retried.
    assert_eq!(h.task("tsk_deadpid").status, TaskStatus::Retrying);
    assert!(
        state
            .retry_queue
            .iter()
            .any(|retry| retry.task_id == "tsk_deadpid")
    );
}

#[tokio::test]
async fn start_watch_cancels_a_dead_pids_task_rather_than_retrying_it() {
    // Startup recovery is deliberately different from reconcile: a restart
    // must not make an agent redo work it already committed.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_deadpid".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(OrchestratorState {
            retry_queue: vec![orch_domain::RetryEntry {
                task_id: "tsk_deadpid".into(),
                attempt: 2,
                due_at: iso_before(1_000),
                error: "earlier".into(),
            }],
            ..state_running("tsk_deadpid", "agt_test1", "run_deadpid", 99999, NOW_ISO)
        })
        .build();
    h.runs.seed(Run {
        id: "run_deadpid".into(),
        task_id: "tsk_deadpid".into(),
        ..make_run()
    });
    h.processes.set_dead(99999);

    h.start_watch().await.unwrap();

    assert_eq!(h.task("tsk_deadpid").status, TaskStatus::Cancelled);
    let state = h.persisted_state();
    assert!(!state.running.contains_key("tsk_deadpid"));
    assert!(state.retry_queue.is_empty());
    assert_eq!(
        h.runs.peek("run_deadpid").unwrap().error.as_deref(),
        Some("Orchestrator restarted")
    );
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn the_failure_handler_updates_the_task_and_enqueues_a_retry() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_fail".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_fail",
            "agt_test1",
            "run_fail",
            77777,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_fail".into(),
        task_id: "tsk_fail".into(),
        ..make_run()
    });

    h.load_state().await.unwrap();
    let entry = h.in_memory_state().await.running["tsk_fail"].clone();
    h.orchestrator
        .handle_run_failure("tsk_fail", &entry, "test error", None)
        .await
        .unwrap();

    let state = h.in_memory_state().await;
    assert_eq!(state.retry_queue.len(), 1);
    assert_eq!(state.retry_queue[0].task_id, "tsk_fail");
    assert!(!state.running.contains_key("tsk_fail"));
    assert_eq!(h.task("tsk_fail").status, TaskStatus::Retrying);

    let retries = h.events_of(OrchestratorEventType::RunRetry);
    assert_eq!(retries.len(), 1);
    match &retries[0] {
        OrchestratorEvent::RunRetry {
            run_id,
            attempt,
            delay_ms,
        } => {
            assert_eq!(run_id, "run_fail");
            assert_eq!(*attempt, 2);
            // calculate_retry_delay(attempts - 1 = 0, 10_000, 300_000).
            assert_eq!(*delay_ms, 10_000);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn reconcile_drains_a_retry_that_has_come_due() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_retry".into(),
            status: TaskStatus::Retrying,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(OrchestratorState {
            retry_queue: vec![orch_domain::RetryEntry {
                task_id: "tsk_retry".into(),
                attempt: 2,
                due_at: iso_before(1_000),
                error: "previous error".into(),
            }],
            ..OrchestratorState::default()
        })
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(
        !h.persisted_state()
            .retry_queue
            .iter()
            .any(|retry| retry.task_id == "tsk_retry")
    );
    assert!(h.was_dispatched("tsk_retry"));
}

#[tokio::test]
async fn a_retry_that_is_not_yet_due_stays_queued() {
    let future =
        orch_core::clock::iso_from_epoch_ms(orch_core::test_support::FIXED_NOW_MS + 60_000);
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_retry".into(),
            status: TaskStatus::Retrying,
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(OrchestratorState {
            retry_queue: vec![orch_domain::RetryEntry {
                task_id: "tsk_retry".into(),
                attempt: 2,
                due_at: future,
                error: "later".into(),
            }],
            ..OrchestratorState::default()
        })
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.persisted_state().retry_queue.len(), 1);
}

// ── dispatch ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn dispatch_sends_a_todo_task_to_an_idle_agent() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(h.was_dispatched("tsk_test1"));
    assert_eq!(h.adapter.execute_calls.len(), 1);
}

#[tokio::test]
async fn dispatch_stops_at_the_concurrency_cap() {
    let h = Harness::builder()
        .tasks([
            task_at("tsk_1", TaskStatus::InProgress),
            task_at("tsk_2", TaskStatus::Todo),
        ])
        .agents([make_agent()])
        .config(config_with_concurrency(1))
        .adapter(done_adapter())
        .state(state_running("tsk_1", "agt_test1", "run_1", 11111, NOW_ISO))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(!h.was_dispatched("tsk_2"));
}

#[tokio::test]
async fn dispatch_skips_a_task_blocked_on_an_unfinished_dependency() {
    let h = Harness::builder()
        .tasks([
            task_at("tsk_dep", TaskStatus::InProgress),
            Task {
                id: "tsk_blocked".into(),
                depends_on: vec!["tsk_dep".into()],
                ..make_task()
            },
        ])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(!h.was_dispatched("tsk_blocked"));
}

#[tokio::test]
async fn dispatch_reports_and_skips_a_scope_overlap_with_running_work() {
    let h = Harness::builder()
        .tasks([
            Task {
                id: "tsk_s1".into(),
                status: TaskStatus::InProgress,
                scope: Some(vec!["src/auth/**".into()]),
                ..make_task()
            },
            Task {
                id: "tsk_s2".into(),
                scope: Some(vec!["src/auth/login.ts".into()]),
                ..make_task()
            },
        ])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    let overlaps = h.events_of(OrchestratorEventType::TaskScopeOverlap);
    assert_eq!(overlaps.len(), 1);
    match &overlaps[0] {
        OrchestratorEvent::TaskScopeOverlap {
            task_id,
            overlapping_task_id,
            patterns,
        } => {
            assert_eq!(task_id, "tsk_s2");
            assert_eq!(overlapping_task_id, "tsk_s1");
            assert_eq!(patterns, &vec!["src/auth/login.ts".to_owned()]);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(!h.was_dispatched("tsk_s2"));
}

#[tokio::test]
async fn a_task_without_a_scope_dispatches_alongside_scoped_work() {
    let h = Harness::builder()
        .tasks([
            Task {
                id: "tsk_scoped".into(),
                status: TaskStatus::InProgress,
                scope: Some(vec!["src/auth/**".into()]),
                ..make_task()
            },
            task_at("tsk_noscope", TaskStatus::Todo),
        ])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(h.was_dispatched("tsk_noscope"));
}

#[tokio::test]
async fn a_blocked_candidate_is_not_itself_an_obstacle_to_later_candidates() {
    // tsk_pb overlaps the in-progress tsk_pa and is blocked; tsk_pc overlaps
    // neither, so it must still run — a blocked candidate is never added to
    // the scope index.
    let h = Harness::builder()
        .tasks([
            Task {
                id: "tsk_pa".into(),
                status: TaskStatus::InProgress,
                scope: Some(vec!["src/auth/**".into()]),
                ..make_task()
            },
            Task {
                id: "tsk_pb".into(),
                scope: Some(vec!["src/auth/login.ts".into()]),
                updated_at: "2025-01-02T00:00:00Z".into(),
                ..make_task()
            },
            Task {
                id: "tsk_pc".into(),
                scope: Some(vec!["src/api/**".into()]),
                updated_at: "2025-01-01T00:00:00Z".into(),
                ..make_task()
            },
        ])
        .agents([make_agent()])
        .config(config_with_concurrency(3))
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(!h.was_dispatched("tsk_pb"));
    assert!(h.was_dispatched("tsk_pc"));
}

// ── dispatch ordering ────────────────────────────────────────────────────

/// One slot and one agent, so only the winner of the sort is dispatched.
async fn dispatch_with_single_slot(tasks: Vec<Task>) -> Harness {
    let h = Harness::builder()
        .tasks(tasks)
        .agents([make_agent()])
        .config(config_with_concurrency(1))
        .adapter(done_adapter())
        .build();
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h
}

#[tokio::test]
async fn a_lower_priority_number_goes_first() {
    let h = dispatch_with_single_slot(vec![
        Task {
            id: "tsk_p3".into(),
            priority: 3,
            updated_at: "2025-01-02T00:00:00Z".into(),
            ..make_task()
        },
        Task {
            id: "tsk_p1".into(),
            priority: 1,
            updated_at: "2025-01-01T00:00:00Z".into(),
            ..make_task()
        },
    ])
    .await;
    assert!(h.was_dispatched("tsk_p1"));
    assert!(!h.was_dispatched("tsk_p3"));
}

#[tokio::test]
async fn a_goal_linked_task_beats_an_unlinked_one_at_the_same_priority() {
    let h = dispatch_with_single_slot(vec![
        Task {
            id: "tsk_nogoal".into(),
            priority: 2,
            updated_at: "2025-01-02T00:00:00Z".into(),
            ..make_task()
        },
        Task {
            id: "tsk_goal".into(),
            priority: 2,
            goal_id: Some("goal_abc".into()),
            updated_at: "2025-01-01T00:00:00Z".into(),
            ..make_task()
        },
    ])
    .await;
    assert!(h.was_dispatched("tsk_goal"));
    assert!(!h.was_dispatched("tsk_nogoal"));
}

#[tokio::test]
async fn priority_outranks_goal_linkage() {
    let h = dispatch_with_single_slot(vec![
        Task {
            id: "tsk_p2g".into(),
            priority: 2,
            goal_id: Some("goal_abc".into()),
            ..make_task()
        },
        Task {
            id: "tsk_p1".into(),
            priority: 1,
            ..make_task()
        },
    ])
    .await;
    assert!(h.was_dispatched("tsk_p1"));
    assert!(!h.was_dispatched("tsk_p2g"));
}

#[tokio::test]
async fn recency_breaks_a_priority_and_goal_tie() {
    let h = dispatch_with_single_slot(vec![
        Task {
            id: "tsk_old".into(),
            priority: 2,
            updated_at: "2025-01-01T00:00:00Z".into(),
            ..make_task()
        },
        Task {
            id: "tsk_new".into(),
            priority: 2,
            updated_at: "2025-01-02T00:00:00Z".into(),
            ..make_task()
        },
    ])
    .await;
    assert!(h.was_dispatched("tsk_new"));
    assert!(!h.was_dispatched("tsk_old"));
}

// ── cancel_task ──────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_task_kills_the_agent_and_marks_the_task_cancelled() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_cancel".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_cancel",
            "agt_test1",
            "run_cancel",
            55555,
            "2025-01-01T00:00:00Z",
        ))
        .build();
    h.runs.seed(Run {
        id: "run_cancel".into(),
        task_id: "tsk_cancel".into(),
        ..make_run()
    });

    h.own_lock();
    h.orchestrator.cancel_task("tsk_cancel").await.unwrap();

    assert_eq!(
        h.processes.killed_with_grace(),
        vec![(55555, std::time::Duration::from_millis(3_000))]
    );
    assert_eq!(h.task("tsk_cancel").status, TaskStatus::Cancelled);
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert_eq!(
        h.runs.peek("run_cancel").unwrap().status,
        RunStatus::Cancelled
    );
    assert!(!h.persisted_state().running.contains_key("tsk_cancel"));
}

#[tokio::test]
async fn cancel_task_cancels_the_run_token_before_dropping_it() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_abort".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_abort",
            "agt_test1",
            "run_abort",
            66666,
            "2025-01-01T00:00:00Z",
        ))
        .build();
    h.runs.seed(Run {
        id: "run_abort".into(),
        task_id: "tsk_abort".into(),
        ..make_run()
    });

    h.own_lock();
    let token = h.orchestrator.register_cancel("tsk_abort").await;
    h.orchestrator.cancel_task("tsk_abort").await.unwrap();

    assert!(token.is_cancelled());
    assert!(
        h.orchestrator
            .inner
            .cancels
            .lock()
            .await
            .get("tsk_abort")
            .is_none()
    );
}

#[tokio::test]
async fn cancel_task_clears_the_tasks_retry_queue_entry() {
    let h = Harness::builder()
        .tasks([task_at("tsk_cr", TaskStatus::Retrying)])
        .state(OrchestratorState {
            retry_queue: vec![orch_domain::RetryEntry {
                task_id: "tsk_cr".into(),
                attempt: 2,
                due_at: "2025-01-01T00:01:00Z".into(),
                error: "err".into(),
            }],
            ..OrchestratorState::default()
        })
        .build();

    h.own_lock();
    h.orchestrator.cancel_task("tsk_cr").await.unwrap();

    assert!(h.persisted_state().retry_queue.is_empty());
    assert_eq!(h.task("tsk_cr").status, TaskStatus::Cancelled);
}

// ── force_stop_agent ─────────────────────────────────────────────────────

#[tokio::test]
async fn force_stop_agent_kills_every_run_the_agent_owns() {
    let mut state = state_running(
        "tsk_fs1",
        "agt_fs",
        "run_fs1",
        11111,
        "2025-01-01T00:00:00Z",
    );
    state.running.insert(
        "tsk_fs2".to_owned(),
        super::harness::running_entry(
            "tsk_fs2",
            "agt_fs",
            "run_fs2",
            22222,
            "2025-01-01T00:00:00Z",
        ),
    );

    let h = Harness::builder()
        .tasks([
            task_at("tsk_fs1", TaskStatus::InProgress),
            task_at("tsk_fs2", TaskStatus::InProgress),
        ])
        .agents([agent_at("agt_fs", AgentStatus::Running)])
        .state(state)
        .build();
    h.runs.seed(Run {
        id: "run_fs1".into(),
        ..make_run()
    });
    h.runs.seed(Run {
        id: "run_fs2".into(),
        ..make_run()
    });

    h.own_lock();
    h.orchestrator.force_stop_agent("agt_fs").await.unwrap();

    let killed = h.processes.killed_with_grace();
    assert!(killed.contains(&(11111, std::time::Duration::from_millis(3_000))));
    assert!(killed.contains(&(22222, std::time::Duration::from_millis(3_000))));
    assert_eq!(h.task("tsk_fs1").status, TaskStatus::Failed);
    assert_eq!(h.task("tsk_fs2").status, TaskStatus::Failed);
    assert!(h.persisted_state().running.is_empty());
    assert_eq!(h.agent("agt_fs").status, AgentStatus::Idle);
}

// ── collect_events ───────────────────────────────────────────────────────

#[tokio::test]
async fn collecting_events_logs_them_and_forwards_them_to_the_bus() {
    let adapter = FakeAdapter::new();
    adapter.set_events([
        output_event(NOW_ISO, "working..."),
        orch_core::adapter::AgentEvent::new(
            orch_core::adapter::AgentEventKind::FileChange,
            NOW_ISO,
            serde_json::json!({ "path": "src/foo.ts" }),
        ),
        orch_core::adapter::AgentEvent::new(
            orch_core::adapter::AgentEventKind::Done,
            NOW_ISO,
            serde_json::json!({ "result": "completed!" }),
        )
        .with_tokens(orch_domain::TokenUsage {
            input: 100,
            output: 200,
            reasoning: 0,
            total: 300,
            cache_read: 0,
            cache_write: 0,
        }),
    ]);

    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(Arc::new(adapter))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    assert!(!h.events_of(OrchestratorEventType::AgentOutput).is_empty());
    let file_events = h.events_of(OrchestratorEventType::AgentFileChanged);
    assert_eq!(file_events.len(), 1);
    match &file_events[0] {
        // Regression (bug-dom-002): the path must survive the serialization
        // step that once nulled `event.data`.
        OrchestratorEvent::AgentFileChanged { path, .. } => assert_eq!(path, "src/foo.ts"),
        other => panic!("unexpected event: {other:?}"),
    }

    // Three events reached the run log, and the run carries the tokens.
    let run = h.runs.snapshot().into_iter().next().expect("a run");
    assert_eq!(h.runs.peek_events(&run.id).len(), 3);
    assert_eq!(h.runs.peek(&run.id).unwrap().tokens.unwrap().total, 300);
    assert_eq!(
        h.task("tsk_test1").proof.unwrap().agent_summary.as_deref(),
        Some("completed!")
    );
}

// ── consecutive tick failures ────────────────────────────────────────────

#[tokio::test]
async fn a_failing_tick_emits_orchestrator_error_with_the_tick_context() {
    let h = Harness::builder().build();
    h.load_state().await.unwrap();
    h.state.faults.fail_always("boom");

    let err = h.tick().await.expect_err("state read fails");
    assert_eq!(err.to_string(), "boom");
    h.orchestrator.on_tick_failure(err).await;

    let errors = h.events_of(OrchestratorEventType::OrchestratorError);
    assert_eq!(errors.len(), 1);
    match &errors[0] {
        OrchestratorEvent::OrchestratorError { context, fatal, .. } => {
            assert_eq!(context, "tick");
            assert!(!fatal);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

// ── state recovery ───────────────────────────────────────────────────────

#[tokio::test]
async fn start_watch_preserves_the_persisted_stats() {
    let h = Harness::builder()
        .state(OrchestratorState {
            stats: orch_domain::StateStats {
                total_tasks_completed: 42,
                ..Default::default()
            },
            ..OrchestratorState::default()
        })
        .build();

    h.start_watch().await.unwrap();
    assert_eq!(h.persisted_state().stats.total_tasks_completed, 42);
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn reconcile_cleans_up_a_stale_entry_for_a_done_task() {
    let h = Harness::builder()
        .tasks([task_at("tsk_done", TaskStatus::Done)])
        .agents([make_agent()])
        .state(state_running(
            "tsk_done",
            "agt_test1",
            "run_old",
            99999,
            "2025-01-01T00:00:00Z",
        ))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(!h.persisted_state().running.contains_key("tsk_done"));
}

// ── completion bookkeeping ───────────────────────────────────────────────

#[tokio::test]
async fn the_success_handler_clears_current_task_and_accumulates_runtime() {
    let agent = Agent {
        id: "agt_suc".into(),
        current_task: Some("tsk_suc".into()),
        stats: orch_domain::AgentStats {
            total_runtime_ms: 1_000,
            ..Default::default()
        },
        ..make_agent()
    };
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_suc".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([agent])
        .state(state_running(
            "tsk_suc",
            "agt_suc",
            "run_suc",
            11111,
            &iso_before(5_000),
        ))
        .build();
    h.runs.seed(Run {
        id: "run_suc".into(),
        task_id: "tsk_suc".into(),
        agent_id: "agt_suc".into(),
        ..make_run()
    });

    h.load_state().await.unwrap();
    h.orchestrator
        .handle_run_success("tsk_suc", "run_suc", "agt_suc", None, None, vec![])
        .await
        .unwrap();

    let saved = h.agent("agt_suc");
    assert_eq!(saved.current_task, None);
    // Exactly the 5 s the entry was aged by — no sleeping required.
    assert_eq!(saved.stats.total_runtime_ms, 6_000);
    assert_eq!(saved.stats.total_runs, 1);
    assert_eq!(saved.stats.tasks_completed, 1);
    assert_eq!(saved.status, AgentStatus::Idle);
    assert_eq!(h.in_memory_state().await.stats.total_runtime_ms, 5_000);
}

#[tokio::test]
async fn the_failure_handler_clears_current_task_and_accumulates_runtime() {
    let agent = Agent {
        id: "agt_fail".into(),
        current_task: Some("tsk_fail2".into()),
        stats: orch_domain::AgentStats {
            total_runtime_ms: 500,
            ..Default::default()
        },
        ..make_agent()
    };
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_fail2".into(),
            status: TaskStatus::InProgress,
            attempts: 2,
            max_attempts: 3,
            ..make_task()
        }])
        .agents([agent])
        .state(state_running(
            "tsk_fail2",
            "agt_fail",
            "run_fail2",
            22222,
            &iso_before(3_000),
        ))
        .build();
    h.runs.seed(Run {
        id: "run_fail2".into(),
        task_id: "tsk_fail2".into(),
        agent_id: "agt_fail".into(),
        ..make_run()
    });

    h.load_state().await.unwrap();
    let entry = h.in_memory_state().await.running["tsk_fail2"].clone();
    h.orchestrator
        .handle_run_failure("tsk_fail2", &entry, "crash", None)
        .await
        .unwrap();

    let saved = h.agent("agt_fail");
    assert_eq!(saved.current_task, None);
    assert_eq!(saved.stats.total_runtime_ms, 3_500);
    assert_eq!(saved.stats.tasks_failed, 1);
    assert_eq!(saved.stats.total_runs, 1);
}

#[tokio::test]
async fn a_successful_run_always_records_some_runtime() {
    // The "Speedster badge" scenario: a run that finishes fast must still
    // leave a non-zero total_runtime_ms.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_speed".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_speed".into(),
            ..make_agent()
        }])
        .state(state_running(
            "tsk_speed",
            "agt_speed",
            "run_speed",
            55555,
            &iso_before(1_000),
        ))
        .build();
    h.runs.seed(Run {
        id: "run_speed".into(),
        task_id: "tsk_speed".into(),
        agent_id: "agt_speed".into(),
        ..make_run()
    });

    h.load_state().await.unwrap();
    h.orchestrator
        .handle_run_success("tsk_speed", "run_speed", "agt_speed", None, None, vec![])
        .await
        .unwrap();

    assert!(h.agent("agt_speed").stats.total_runtime_ms > 0);
}

// ── immediate dispatch ───────────────────────────────────────────────────

#[tokio::test]
async fn task_created_triggers_a_dispatch_after_the_debounce() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();
    h.start_watch().await.unwrap();
    // The initial tick dispatched the seeded task; start from a clean slate.
    h.tasks.calls.clear();

    h.tasks.seed(task_at("tsk_reactive", TaskStatus::Todo));
    h.agents.seed(agent_at("agt_test1", AgentStatus::Idle));
    h.bus.emit(OrchestratorEvent::TaskCreated {
        task: Box::new(task_at("tsk_reactive", TaskStatus::Todo)),
    });

    h.wait_for(|h| h.was_dispatched("tsk_reactive")).await;
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn several_task_created_events_coalesce_into_one_dispatch() {
    let h = Harness::builder().agents([make_agent()]).build();
    h.start_watch().await.unwrap();
    let writes_before = h.state.written().len();

    for index in 0..3 {
        h.bus.emit(OrchestratorEvent::TaskCreated {
            task: Box::new(task_at(&format!("tsk_batch_{index}"), TaskStatus::Todo)),
        });
    }
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // One `fresh_dispatch_all` writes state once, not three times.
    let writes = h.state.written().len() - writes_before;
    assert!(writes <= 2, "{writes} state writes for one debounce window");
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn stop_clears_the_pending_immediate_dispatch() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();

    h.bus.emit(OrchestratorEvent::TaskCreated {
        task: Box::new(make_task()),
    });
    // Let the scheduling task install the timer.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    h.orchestrator.stop().await.unwrap();

    assert!(!h.orchestrator.has_pending_immediate_dispatch().await);
}

#[tokio::test]
async fn nothing_is_dispatched_once_shutdown_has_begun() {
    let h = Harness::builder().agents([make_agent()]).build();
    h.start_watch().await.unwrap();
    h.orchestrator.stop().await.unwrap();
    let writes_before = h.state.written().len();

    h.orchestrator.schedule_immediate_dispatch(0);
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    assert_eq!(h.state.written().len(), writes_before);
}

#[tokio::test]
async fn an_immediate_dispatch_defers_while_a_tick_is_in_flight_then_runs() {
    let h = Harness::builder().agents([make_agent()]).build();
    h.start_watch().await.unwrap();
    h.orchestrator
        .inner
        .tick_in_progress
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let writes_before = h.state.written().len();

    h.orchestrator.schedule_immediate_dispatch(0);
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(
        h.state.written().len(),
        writes_before,
        "dispatch ran while a tick was in flight"
    );

    h.orchestrator
        .inner
        .tick_in_progress
        .store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert!(h.state.written().len() > writes_before);
    h.orchestrator.stop().await.unwrap();
}
