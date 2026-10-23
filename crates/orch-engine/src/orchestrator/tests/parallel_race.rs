//! When a child exits cleanly there is a window between PID death and the
//! success handler taking the state lock. Reconcile firing inside that window
//! used to read the dead PID as a crash and fail a run that had just
//! succeeded. The `active_collectors` guard closes it for the PID check, and
//! `finishing_collectors` closes it for the stall check once the stream has
//! ended. While the stream is still open the stall check must run: a silent
//! child with a live collector is exactly what it exists to catch.

use std::sync::Arc;

use orch_core::adapter::{AdapterError, AgentEvent, AgentEventKind};
use orch_core::test_support::{
    FakeAdapter, done_event, make_agent, make_run, make_task, output_event,
};
use orch_domain::{
    Agent, AgentStatus, OrchestratorEvent, OrchestratorEventType, OrchestratorState, Run,
    RunStatus, RunningEntry, Task, TaskStatus,
};

use super::core::CancelAwareAdapter;
use super::harness::{Harness, NOW_ISO, iso_before, running_entry, state_running};

fn in_progress(id: &str) -> Task {
    Task {
        id: id.into(),
        status: TaskStatus::InProgress,
        ..make_task()
    }
}

fn running_agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
        status: AgentStatus::Running,
        ..make_agent()
    }
}

#[tokio::test]
async fn reconcile_skips_the_pid_check_for_a_task_with_an_active_collector() {
    let mut state = OrchestratorState::default();
    state.running.insert(
        "tsk_r1".to_owned(),
        running_entry("tsk_r1", "agt_r1", "run_r1", 100, NOW_ISO),
    );
    state.running.insert(
        "tsk_r2".to_owned(),
        running_entry("tsk_r2", "agt_r2", "run_r2", 200, NOW_ISO),
    );

    let h = Harness::builder()
        .tasks([in_progress("tsk_r1"), in_progress("tsk_r2")])
        .agents([running_agent("agt_r1"), running_agent("agt_r2")])
        .state(state)
        .build();
    // Both children exited cleanly.
    h.processes.set_all_dead(true);

    {
        let mut collectors = h.orchestrator.inner.active_collectors.lock().await;
        collectors.insert("tsk_r1".to_owned());
        collectors.insert("tsk_r2".to_owned());
    }

    h.own_lock();
    h.reconcile().await.unwrap();

    // Neither task was failed, and no run was touched.
    assert_eq!(h.task("tsk_r1").status, TaskStatus::InProgress);
    assert_eq!(h.task("tsk_r2").status, TaskStatus::InProgress);
    assert!(h.runs.saved().is_empty());
}

#[tokio::test]
async fn reconcile_does_detect_the_crash_when_no_collector_is_active() {
    let h = Harness::builder()
        .tasks([in_progress("tsk_c1")])
        .agents([running_agent("agt_c1")])
        .state(OrchestratorState {
            running: [(
                "tsk_c1".to_owned(),
                running_entry("tsk_c1", "agt_c1", "run_c1", 100, NOW_ISO),
            )]
            .into_iter()
            .collect(),
            ..OrchestratorState::default()
        })
        .build();
    h.runs.seed(Run {
        id: "run_c1".into(),
        task_id: "tsk_c1".into(),
        agent_id: "agt_c1".into(),
        ..make_run()
    });
    h.processes.set_all_dead(true);

    h.own_lock();
    h.reconcile().await.unwrap();

    assert_ne!(h.task("tsk_c1").status, TaskStatus::InProgress);
    assert_eq!(h.runs.peek("run_c1").unwrap().status, RunStatus::Failed);
    assert_eq!(
        h.runs.peek("run_c1").unwrap().error.as_deref(),
        Some("Process crashed unexpectedly")
    );
}

#[tokio::test]
async fn the_collector_registration_is_released_once_collection_finishes() {
    let adapter = FakeAdapter::new();
    adapter.set_events([output_event(NOW_ISO, "hello"), done_event(NOW_ISO)]);

    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_ac".into(),
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_ac".into(),
            ..make_agent()
        }])
        .adapter(Arc::new(adapter))
        .build();

    h.own_lock();
    h.orchestrator.fresh_dispatch_only("tsk_ac").await.unwrap();
    h.settle().await;

    assert!(
        h.orchestrator
            .inner
            .active_collectors
            .lock()
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_stream_error_finalizes_the_run_even_when_the_entry_is_already_gone() {
    // Reconcile cleaned the running entry up first; the collector must still
    // finalize the run or it stays `running` forever.
    let h = Harness::builder()
        .tasks([in_progress("tsk_fin")])
        .agents([running_agent("agt_fin")])
        .build();
    h.runs.seed(Run {
        id: "run_fin".into(),
        task_id: "tsk_fin".into(),
        agent_id: "agt_fin".into(),
        status: RunStatus::Running,
        ..make_run()
    });

    h.own_lock();
    h.set_state(OrchestratorState::default()).await;

    let failing: futures::stream::BoxStream<
        'static,
        std::result::Result<AgentEvent, AdapterError>,
    > = Box::pin(futures::stream::iter(vec![Err(AdapterError::new(
        "Process exited with code 1",
        None,
    ))]));
    h.orchestrator
        .collect_events(failing, "run_fin", "tsk_fin", "agt_fin")
        .await
        .unwrap();

    let run = h.runs.peek("run_fin").unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.as_deref(), Some("Process exited with code 1"));
}

#[tokio::test]
async fn a_stream_error_with_a_live_entry_goes_through_the_failure_handler() {
    let h = Harness::builder()
        .tasks([Task {
            attempts: 1,
            ..in_progress("tsk_fin")
        }])
        .agents([running_agent("agt_fin")])
        .state(OrchestratorState {
            running: [(
                "tsk_fin".to_owned(),
                RunningEntry {
                    ..running_entry("tsk_fin", "agt_fin", "run_fin", 100, NOW_ISO)
                },
            )]
            .into_iter()
            .collect(),
            ..OrchestratorState::default()
        })
        .build();
    h.runs.seed(Run {
        id: "run_fin".into(),
        task_id: "tsk_fin".into(),
        agent_id: "agt_fin".into(),
        status: RunStatus::Running,
        ..make_run()
    });

    h.own_lock();
    h.load_state().await.unwrap();

    let failing: futures::stream::BoxStream<
        'static,
        std::result::Result<AgentEvent, AdapterError>,
    > = Box::pin(futures::stream::iter(vec![
        Ok(AgentEvent::new(
            AgentEventKind::Output,
            NOW_ISO,
            serde_json::json!({ "text": "working" }),
        )),
        Err(AdapterError::new("spawn ENOENT", None)),
    ]));
    h.orchestrator
        .collect_events(failing, "run_fin", "tsk_fin", "agt_fin")
        .await
        .unwrap();

    // The failure handler ran: run failed, agent released, task retrying.
    assert_eq!(h.runs.peek("run_fin").unwrap().status, RunStatus::Failed);
    assert_eq!(h.agent("agt_fin").status, AgentStatus::Idle);
    assert_eq!(h.task("tsk_fin").status, TaskStatus::Retrying);
    // The classification came from the thrown error, not from an error event.
    assert_eq!(h.agent("agt_fin").last_error.unwrap().kind, "spawn_failed");
}

#[tokio::test]
async fn reconcile_detects_a_stall_while_the_collector_is_still_reading() {
    // A run this process dispatched: its collector is live and its stream is
    // open, but the child has said nothing for longer than the stall timeout.
    let adapter = Arc::new(CancelAwareAdapter::default());
    let h = Harness::builder()
        .tasks([Task {
            attempts: 0,
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter_impl(adapter.clone() as Arc<dyn orch_core::adapter::Adapter>)
        .build();
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    let run_id = h.in_memory_state().await.running["tsk_test1"]
        .run_id
        .clone();
    assert!(
        h.orchestrator
            .inner
            .active_collectors
            .lock()
            .await
            .contains("tsk_test1")
    );

    let stall_timeout = h.orchestrator.config().defaults.agent.stall_timeout_ms as i64;
    h.clock.advance_ms(stall_timeout + 1);
    h.reconcile().await.unwrap();

    let stalls = h.events_of(OrchestratorEventType::OrchestratorStallDetected);
    assert_eq!(
        stalls,
        vec![OrchestratorEvent::OrchestratorStallDetected {
            run_id: run_id.clone()
        }]
    );
    assert_eq!(h.processes.killed_with_grace().len(), 1);
    assert!(adapter.tokens.lock().unwrap()[0].is_cancelled());
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);

    // The stream ends once the child is gone; the collector's late success
    // handler finds no running entry and must leave the stall verdict alone.
    h.settle().await;
    let run = h.runs.peek(&run_id).unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.as_deref(), Some("Agent stalled (no events)"));
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);
}

#[tokio::test]
async fn reconcile_skips_the_stall_check_once_the_collectors_stream_has_ended() {
    // The child went quiet past the stall timeout, then exited cleanly. Its
    // stream has ended and the success handler is queued behind reconcile's
    // state lock: neither the dead PID nor the old stall clock is a failure.
    let stale = iso_before(700_000);
    let h = Harness::builder()
        .tasks([in_progress("tsk_fin")])
        .agents([running_agent("agt_fin")])
        .state(state_running("tsk_fin", "agt_fin", "run_fin", 100, &stale))
        .build();
    h.runs.seed(Run {
        id: "run_fin".into(),
        task_id: "tsk_fin".into(),
        agent_id: "agt_fin".into(),
        status: RunStatus::Running,
        ..make_run()
    });
    h.processes.set_all_dead(true);
    h.own_lock();

    // Hold the state lock the way a tick does, then let the collector run
    // into it, exactly as dispatch spawns it.
    let mut slot = h.orchestrator.inner.state.lock().await;
    h.orchestrator.load_state(&mut slot).await.unwrap();
    h.orchestrator
        .inner
        .active_collectors
        .lock()
        .await
        .insert("tsk_fin".to_owned());
    let orchestrator = h.orchestrator.clone();
    let collector = tokio::spawn(async move {
        let ended: futures::stream::BoxStream<
            'static,
            std::result::Result<AgentEvent, AdapterError>,
        > = Box::pin(futures::stream::empty());
        orchestrator
            .collect_events(ended, "run_fin", "tsk_fin", "agt_fin")
            .await
    });
    let mut finishing = false;
    for _ in 0..2000 {
        if h.orchestrator
            .inner
            .finishing_collectors
            .lock()
            .await
            .contains("tsk_fin")
        {
            finishing = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert!(finishing, "the collector never reported its stream ended");

    h.orchestrator
        .reconcile(slot.as_mut().expect("state loaded"))
        .await
        .unwrap();
    drop(slot);

    assert!(
        h.events_of(OrchestratorEventType::OrchestratorStallDetected)
            .is_empty()
    );
    assert!(h.processes.killed_with_grace().is_empty());

    // The queued success handler then completes the run normally.
    collector.await.unwrap().unwrap();
    assert_eq!(h.runs.peek("run_fin").unwrap().status, RunStatus::Succeeded);
    assert_ne!(h.task("tsk_fin").status, TaskStatus::Retrying);
}

/// A child that outlives its cancel for a while, the way one does between
/// SIGTERM and exit: each run's stream stays open until the test fires that
/// run's `exits` token, whatever happens to the run's cancel token.
#[derive(Default)]
struct SlowExitAdapter {
    exits: std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>,
}

#[async_trait::async_trait]
impl orch_core::adapter::Adapter for SlowExitAdapter {
    fn kind(&self) -> &'static str {
        "shell"
    }

    async fn test(&self) -> orch_core::adapter::AdapterTestResult {
        orch_core::adapter::AdapterTestResult::ok("1.0.0")
    }

    fn execute(
        &self,
        _params: orch_core::adapter::ExecuteParams,
    ) -> std::result::Result<orch_core::adapter::ExecuteHandle, AdapterError> {
        use futures::StreamExt;
        let exit = tokio_util::sync::CancellationToken::new();
        self.exits.lock().unwrap().push(exit.clone());
        Ok(orch_core::adapter::ExecuteHandle {
            pid: 99999,
            events: Box::pin(
                futures::stream::once(async move { exit.cancelled().await }).filter_map(
                    |()| async { None::<std::result::Result<AgentEvent, AdapterError>> },
                ),
            ),
        })
    }

    async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_stalled_task_is_not_redispatched_while_its_old_stream_is_still_open() {
    // The retry must wait for the stalled run's collector. Dispatched any
    // earlier, the new run shares the task id the collector state is keyed
    // by, and the old stream's clean end completes the *new* run's task.
    let adapter = Arc::new(SlowExitAdapter::default());
    let h = Harness::builder()
        .tasks([Task {
            attempts: 0,
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter_impl(adapter.clone() as Arc<dyn orch_core::adapter::Adapter>)
        .build();
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    let stalled_run = h.in_memory_state().await.running["tsk_test1"]
        .run_id
        .clone();

    let config = h.orchestrator.config();
    h.clock
        .advance_ms(config.defaults.agent.stall_timeout_ms as i64 + 1);
    h.reconcile().await.unwrap();
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);

    // The retry is due, but the killed child has not exited yet.
    h.clock
        .advance_ms(config.scheduling.retry_max_delay_ms as i64 + 1);
    h.tick().await.unwrap();
    assert_eq!(adapter.exits.lock().unwrap().len(), 1);
    assert!(!h.in_memory_state().await.running.contains_key("tsk_test1"));
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);

    // The child exits; its collector finds nothing to complete.
    adapter.exits.lock().unwrap()[0].cancel();
    h.settle().await;
    let run = h.runs.peek(&stalled_run).unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.as_deref(), Some("Agent stalled (no events)"));
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);

    // Now the retry goes out, as a run of its own.
    h.tick().await.unwrap();
    assert_eq!(adapter.exits.lock().unwrap().len(), 2);
    let retry_run = h.in_memory_state().await.running["tsk_test1"]
        .run_id
        .clone();
    assert_ne!(retry_run, stalled_run);
    assert_eq!(h.task("tsk_test1").status, TaskStatus::InProgress);
}
