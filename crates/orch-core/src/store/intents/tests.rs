//! What the shared scenarios cannot reach: the payload's wire shape, and states
//! of the reference that only a hand-built value shows.
#![allow(
    clippy::unwrap_used,
    reason = "test helper module; failures terminate the scenario"
)]

use super::fake::IntentState;
use super::*;

fn blob() -> IntentPayload {
    IntentPayload::BlobDelete {
        hash: "ab".repeat(32),
    }
}

#[test]
fn a_payload_has_one_tagged_shape_and_round_trips() {
    for (payload, json) in [
        (
            IntentPayload::ProcessStart { run: "r1".into() },
            r#"{"kind":"process.start","run":"r1"}"#,
        ),
        (
            IntentPayload::ProcessStop { run: "r1".into() },
            r#"{"kind":"process.stop","run":"r1"}"#,
        ),
        (
            IntentPayload::WorkspaceRemove { task: "t1".into() },
            r#"{"kind":"workspace.remove","task":"t1"}"#,
        ),
        (
            blob(),
            &format!(r#"{{"kind":"blob.delete","hash":"{}"}}"#, "ab".repeat(32)),
        ),
    ] {
        assert_eq!(serde_json::to_string(&payload).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<IntentPayload>(json).unwrap(),
            payload
        );
    }
}

#[test]
fn a_payload_with_an_unknown_kind_field_or_path_is_not_read() {
    for json in [
        r#"{"kind":"process.kill","run":"r1"}"#,
        r#"{"kind":"process.stop","run":"r1","pid":42}"#,
        r#"{"kind":"workspace.remove","task":"t1","path":"/tmp/x"}"#,
        r#"{"kind":"workspace.remove"}"#,
        r#"{"run":"r1"}"#,
    ] {
        assert!(
            serde_json::from_str::<IntentPayload>(json).is_err(),
            "{json}"
        );
    }
}

#[test]
fn only_a_process_intent_names_a_run() {
    assert_eq!(
        IntentPayload::ProcessStop { run: "r1".into() }.run(),
        Some("r1")
    );
    assert_eq!(
        IntentPayload::WorkspaceRemove { task: "t".into() }.run(),
        None
    );
    assert_eq!(blob().run(), None);
}

#[test]
fn the_last_id_is_refused_without_a_change() {
    let mut state = IntentState::default();
    state.add(&blob(), 1).unwrap();
    // A counter at its limit, which no port call can reach.
    let mut spent = IntentState::at_last_id(u64::MAX, state);
    assert!(matches!(
        spent.add(&blob(), 2),
        Err(StoreError::CounterExhausted)
    ));
    assert_eq!(spent.pending(0, 10).unwrap().len(), 1);
}

#[test]
fn a_clock_before_the_epoch_deletes_nothing_and_does_not_overflow() {
    let mut state = IntentState::default();
    state.add(&blob(), 0).unwrap();
    state.finish(1, 0).unwrap();
    assert_eq!(state.delete_finished(i64::MIN, 10).unwrap(), 0);
    assert!(state.get(1).is_some());
}

#[test]
fn a_row_finished_at_the_signed_minimum_is_kept_until_exactly_seven_days() {
    let mut state = IntentState::default();
    state.add(&blob(), i64::MIN).unwrap();
    state.finish(1, i64::MIN).unwrap();
    for now in [i64::MIN, i64::MIN + DONE_RETENTION_MS - 1] {
        assert_eq!(state.delete_finished(now, 10).unwrap(), 0);
        assert!(state.get(1).is_some());
    }
    assert_eq!(
        state
            .delete_finished(i64::MIN + DONE_RETENTION_MS, 10)
            .unwrap(),
        1
    );
}
