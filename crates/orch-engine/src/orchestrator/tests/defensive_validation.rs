//! Defensive validation in the orchestrator.
//!
//! Fix #2 (the retry-queue bound and dedup) is covered in
//! [`super::endurance`]; Fix #1 and Fix #4 are in `task_service` and
//! `agent_service`; Fix #3 is `ContextStore`'s, in `orch-infra`. What is left
//! here is Fix #5: the timestamp an adapter supplies is only trusted when it is
//! **exactly** the canonical ISO form, and the fallback mints a fresh one.

use orch_core::adapter::{AgentEvent, AgentEventKind};
use orch_core::test_support::make_task;
use orch_domain::OrchestratorState;

use super::super::MAX_RETRY_QUEUE_SIZE;
use super::super::collect::is_valid_iso_timestamp;
use super::harness::{Harness, NOW_ISO};

#[test]
fn the_canonical_iso_form_is_accepted() {
    assert!(is_valid_iso_timestamp("2025-01-01T00:00:00.000Z"));
    assert!(is_valid_iso_timestamp("2026-10-13T16:37:33.426Z"));
    assert!(is_valid_iso_timestamp(NOW_ISO));
}

#[test]
fn anything_that_is_not_the_canonical_form_is_rejected() {
    for value in [
        "",
        "   ",
        "not a date",
        "Invalid Date",
        // A locale rendering.
        "1/1/2025, 12:00:00 AM",
        // Parses, but `toISOString` would add the milliseconds, so the
        // round-trip comparison fails — the check is equality, not
        // parseability.
        "2025-01-01T00:00:00Z",
        // An offset rather than `Z`: same instant, different text.
        "2025-01-01T00:00:00.000+00:00",
        // Out of range.
        "2025-13-01T00:00:00.000Z",
        "2025-01-32T00:00:00.000Z",
    ] {
        assert!(!is_valid_iso_timestamp(value), "accepted {value:?}");
    }
}

/// Rejecting a non-string is unrepresentable here: the parameter is `&str`.
#[test]
fn a_non_string_timestamp_is_unrepresentable() {
    // `AgentEvent::timestamp` is a `String`, so the only degenerate input the
    // type system still allows is an empty or malformed one — covered above.
    let event = AgentEvent::new(AgentEventKind::Output, "", serde_json::json!({}));
    assert!(!is_valid_iso_timestamp(&event.timestamp));
}

#[tokio::test]
async fn an_unusable_event_timestamp_is_replaced_with_the_clocks_own() {
    let h = Harness::builder().tasks([make_task()]).build();
    h.set_state(OrchestratorState::default()).await;

    let events: futures::stream::BoxStream<
        'static,
        std::result::Result<AgentEvent, orch_core::adapter::AdapterError>,
    > = Box::pin(futures::stream::iter(vec![
        Ok(AgentEvent::new(
            AgentEventKind::Output,
            "not a date",
            serde_json::json!({ "text": "a" }),
        )),
        Ok(AgentEvent::new(
            AgentEventKind::Output,
            "",
            serde_json::json!({ "text": "b" }),
        )),
    ]));
    h.orchestrator
        .collect_events(events, "run_ts", "tsk_test1", "agt_test1")
        .await
        .unwrap();

    let recorded = h.runs.peek_events("run_ts");
    assert_eq!(recorded.len(), 2);
    for event in recorded {
        assert_eq!(event.timestamp, NOW_ISO);
    }
}

#[tokio::test]
async fn a_usable_event_timestamp_is_kept_as_given() {
    let h = Harness::builder().tasks([make_task()]).build();
    h.set_state(OrchestratorState::default()).await;

    let supplied = "2025-06-01T12:34:56.789Z";
    let events: futures::stream::BoxStream<
        'static,
        std::result::Result<AgentEvent, orch_core::adapter::AdapterError>,
    > = Box::pin(futures::stream::iter(vec![Ok(AgentEvent::new(
        AgentEventKind::Output,
        supplied,
        serde_json::json!({ "text": "a" }),
    ))]));
    h.orchestrator
        .collect_events(events, "run_ts2", "tsk_test1", "agt_test1")
        .await
        .unwrap();

    assert_eq!(h.runs.peek_events("run_ts2")[0].timestamp, supplied);
}

#[test]
fn the_retry_queue_bound_is_a_hundred() {
    assert_eq!(MAX_RETRY_QUEUE_SIZE, 100);
}
