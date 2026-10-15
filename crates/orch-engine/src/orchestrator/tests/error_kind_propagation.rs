//! Ported from `test/unit/application/error-kind-propagation.test.ts`.
//!
//! An adapter's classification has to survive the whole path: the `error`
//! event carries it to the bus, the collector remembers the last one, and the
//! failure handler persists it on the agent so `orch agent list` and the TUI
//! can explain *why* an agent is unhealthy.

use orch_core::adapter::{AdapterError, AgentEvent, AgentEventKind};
use orch_core::test_support::{error_event, make_agent, make_run, make_task};
use orch_domain::{
    AdapterErrorKind, Agent, AgentLastError, AgentStatus, OrchestratorEvent, OrchestratorEventType,
    OrchestratorState, Run, RunStatus, RunningEntry, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, state_running};

type EventStream =
    futures::stream::BoxStream<'static, std::result::Result<AgentEvent, AdapterError>>;

fn stream(events: Vec<AgentEvent>) -> EventStream {
    Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
}

/// A task mid-run, with its agent holding it.
fn running(task_id: &str, agent_id: &str, run_id: &str) -> Harness {
    let h = Harness::builder()
        .tasks([Task {
            id: task_id.into(),
            status: TaskStatus::InProgress,
            assignee: Some(agent_id.into()),
            ..make_task()
        }])
        .agents([Agent {
            id: agent_id.into(),
            status: AgentStatus::Running,
            current_task: Some(task_id.into()),
            ..make_task_agent()
        }])
        .state(state_running(task_id, agent_id, run_id, 1, NOW_ISO))
        .build();
    h.runs.seed(Run {
        id: run_id.into(),
        task_id: task_id.into(),
        agent_id: agent_id.into(),
        status: RunStatus::Running,
        ..make_run()
    });
    h
}

fn make_task_agent() -> Agent {
    make_agent()
}

fn entry(task_id: &str, agent_id: &str, run_id: &str) -> RunningEntry {
    RunningEntry {
        run_id: run_id.into(),
        agent_id: agent_id.into(),
        task_id: task_id.into(),
        pid: 1,
        started_at: NOW_ISO.into(),
        last_event_at: NOW_ISO.into(),
        remote: None,
    }
}

#[tokio::test]
async fn the_agent_error_event_carries_the_adapters_classification() {
    let h = Harness::builder().build();
    h.set_state(OrchestratorState::default()).await;

    h.orchestrator
        .collect_events(
            stream(vec![error_event(
                NOW_ISO,
                "Auth failed",
                AdapterErrorKind::AuthFailed,
            )]),
            "run_ek1",
            "tsk_ek1",
            "agt_ek1",
        )
        .await
        .unwrap();

    let errors = h.events_of(OrchestratorEventType::AgentError);
    assert_eq!(errors.len(), 1);
    match &errors[0] {
        OrchestratorEvent::AgentError {
            error_kind,
            run_id,
            agent_id,
            error,
        } => {
            assert_eq!(*error_kind, Some(AdapterErrorKind::AuthFailed));
            assert_eq!(run_id, "run_ek1");
            assert_eq!(agent_id, "agt_ek1");
            assert!(error.contains("Auth failed"));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn the_agent_error_event_omits_the_kind_when_the_adapter_did_not_supply_one() {
    let h = Harness::builder().build();
    h.set_state(OrchestratorState::default()).await;

    h.orchestrator
        .collect_events(
            stream(vec![AgentEvent::new(
                AgentEventKind::Error,
                NOW_ISO,
                serde_json::json!({ "message": "Some error" }),
            )]),
            "run_x",
            "tsk_x",
            "agt_x",
        )
        .await
        .unwrap();

    let errors = h.events_of(OrchestratorEventType::AgentError);
    assert_eq!(errors.len(), 1);
    match &errors[0] {
        OrchestratorEvent::AgentError { error_kind, .. } => assert_eq!(*error_kind, None),
        other => panic!("unexpected event: {other:?}"),
    }
    // And the field is *absent* on the wire, not null.
    assert!(
        serde_json::to_value(&errors[0])
            .unwrap()
            .get("errorKind")
            .is_none()
    );
}

#[tokio::test]
async fn the_failure_handler_persists_a_truncated_message_a_kind_and_a_timestamp() {
    let h = running("tsk_le1", "agt_le1", "run_le1");
    h.load_state().await.unwrap();

    let long_error = "E".repeat(1000);
    h.orchestrator
        .handle_run_failure(
            "tsk_le1",
            &entry("tsk_le1", "agt_le1", "run_le1"),
            &long_error,
            Some(AdapterErrorKind::RateLimit),
        )
        .await
        .unwrap();

    let last_error = h.agent("agt_le1").last_error.expect("last_error");
    assert_eq!(last_error.message.chars().count(), 500);
    assert_eq!(last_error.kind, "rate_limit");
    // The timestamp is the canonical ISO form the clock produces.
    assert_eq!(last_error.timestamp, NOW_ISO);
}

#[tokio::test]
async fn the_failure_handler_classifies_the_message_when_no_kind_is_supplied() {
    let h = running("tsk_ca1", "agt_ca1", "run_ca1");
    h.load_state().await.unwrap();

    h.orchestrator
        .handle_run_failure(
            "tsk_ca1",
            &entry("tsk_ca1", "agt_ca1", "run_ca1"),
            "ENOENT: command not found",
            None,
        )
        .await
        .unwrap();

    // ENOENT matches spawn_failed, which outranks adapter_not_found.
    assert_eq!(h.agent("agt_ca1").last_error.unwrap().kind, "spawn_failed");
}

#[tokio::test]
async fn the_failure_handler_clears_current_task() {
    let h = running("tsk_ct1", "agt_ct1", "run_ct1");
    h.load_state().await.unwrap();

    h.orchestrator
        .handle_run_failure(
            "tsk_ct1",
            &entry("tsk_ct1", "agt_ct1", "run_ct1"),
            "some error",
            Some(AdapterErrorKind::Timeout),
        )
        .await
        .unwrap();

    assert_eq!(h.agent("agt_ct1").current_task, None);
    assert_eq!(h.agent("agt_ct1").status, AgentStatus::Idle);
}

#[tokio::test]
async fn a_successful_dispatch_clears_a_stale_last_error() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_dc1".into(),
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_dc1".into(),
            last_error: Some(AgentLastError {
                message: "old".into(),
                kind: "timeout".into(),
                timestamp: "2025-01-01T00:00:00Z".into(),
            }),
            ..make_agent()
        }])
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    assert_eq!(h.agent("agt_dc1").last_error, None);
}

#[tokio::test]
async fn a_reconcile_pid_crash_records_the_crash_message_and_a_derived_kind() {
    let h = running("tsk_rc1", "agt_rc1", "run_rc1");
    h.processes.set_dead(1);

    h.reconcile().await.unwrap();

    let last_error = h.agent("agt_rc1").last_error.expect("last_error");
    assert_eq!(last_error.message, "Process crashed unexpectedly");
    // The reconcile crash path passes no kind and no exit code, and
    // `classify_adapter_error` only reaches `process_crash` through a non-zero
    // exit code — so a reaped PID classifies as `unknown`. Replicated; the
    // crash is identified by the message, not the kind.
    assert_eq!(last_error.kind, "unknown");
    assert_eq!(
        h.runs.peek("run_rc1").unwrap().error.as_deref(),
        Some("Process crashed unexpectedly")
    );
}

#[tokio::test]
async fn a_reconcile_stall_records_the_stall_message() {
    // `make_agent` has stall_timeout_ms = 300_000; age the entry past it.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_st1".into(),
            status: TaskStatus::InProgress,
            assignee: Some("agt_st1".into()),
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_st1".into(),
            status: AgentStatus::Running,
            current_task: Some("tsk_st1".into()),
            ..make_agent()
        }])
        .state(state_running(
            "tsk_st1",
            "agt_st1",
            "run_st1",
            12345,
            &super::harness::iso_before(400_000),
        ))
        .build();
    h.runs.seed(Run {
        id: "run_st1".into(),
        task_id: "tsk_st1".into(),
        agent_id: "agt_st1".into(),
        status: RunStatus::Running,
        ..make_run()
    });

    h.reconcile().await.unwrap();

    assert_eq!(
        h.agent("agt_st1").last_error.unwrap().message,
        "Agent stalled (no events)"
    );
}

#[tokio::test]
async fn the_last_error_record_has_the_contract_shape() {
    // `kind` is a plain string on the wire, not an enum, so a kind written by
    // a newer adapter still reads back.
    let last_error = AgentLastError {
        message: "err".into(),
        kind: "unknown".into(),
        timestamp: "2025-01-01T00:00:00Z".into(),
    };
    let json = serde_json::to_value(&last_error).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "message": "err",
            "kind": "unknown",
            "timestamp": "2025-01-01T00:00:00Z",
        })
    );

    for kind in AdapterErrorKind::ALL {
        let record = AgentLastError {
            message: "err".into(),
            kind: kind.as_str().to_owned(),
            timestamp: NOW_ISO.into(),
        };
        assert_eq!(
            serde_json::from_value::<AgentLastError>(serde_json::to_value(&record).unwrap())
                .unwrap(),
            record
        );
    }
}

#[tokio::test]
async fn the_last_error_event_kind_wins_over_the_thrown_errors_kind() {
    // `collect_events` remembers the last `error` event's kind and prefers it
    // to whatever the stream error classified as.
    let h = running("tsk_pref", "agt_pref", "run_pref");
    h.load_state().await.unwrap();

    let events: EventStream = Box::pin(futures::stream::iter(vec![
        Ok(error_event(
            NOW_ISO,
            "rate limited",
            AdapterErrorKind::RateLimit,
        )),
        Err(AdapterError::new("spawn ENOENT", None)),
    ]));
    h.orchestrator
        .collect_events(events, "run_pref", "tsk_pref", "agt_pref")
        .await
        .unwrap();

    assert_eq!(
        h.agent("agt_pref").last_error.unwrap().kind,
        "rate_limit",
        "the error event's kind should win"
    );
}
