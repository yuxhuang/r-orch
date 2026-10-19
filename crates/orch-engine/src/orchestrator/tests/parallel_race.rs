//! When a child exits cleanly there is a window between PID death and the
//! success handler taking the state lock. Reconcile firing inside that window
//! used to read the dead PID as a crash and fail a run that had just
//! succeeded. The `active_collectors` guard closes it.

use std::sync::Arc;

use orch_core::adapter::{AdapterError, AgentEvent, AgentEventKind};
use orch_core::test_support::{
    FakeAdapter, done_event, make_agent, make_run, make_task, output_event,
};
use orch_domain::{
    Agent, AgentStatus, OrchestratorState, Run, RunStatus, RunningEntry, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, running_entry};

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
