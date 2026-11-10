use orch_proto::frame::{EventBody, ServerFrame};

#[test]
fn store_wide_change_without_id_round_trips() {
    let raw = br#"{"ev":{"subscription":3,"instance":"i","seq":1,"store":"state","kind":"updated","commit_seq":9001}}"#;
    let frame = ServerFrame::decode(raw).expect("store-wide changes have no entity id");
    assert!(
        matches!(&frame, ServerFrame::Event(e) if matches!(e.ev.body, EventBody::Change { .. }))
    );
    assert_eq!(
        serde_json::to_value(frame).unwrap(),
        serde_json::from_slice::<serde_json::Value>(raw).unwrap()
    );
}

#[test]
fn uncertain_store_set_has_a_distinct_resync_reason() {
    let raw = br#"{"ev":{"subscription":3,"instance":"i","seq":1,"resync":{"reason":"store_set_uncertain"}}}"#;
    let frame = ServerFrame::decode(raw).expect("uncertain store set must invalidate every view");
    assert_eq!(
        serde_json::to_value(frame).unwrap(),
        serde_json::from_slice::<serde_json::Value>(raw).unwrap()
    );
}

#[test]
fn push_semantics_have_their_own_protocol_revision() {
    assert_eq!(orch_proto::LOCAL_PROTOCOL, 2);
}

#[test]
fn raw_ack_origin_baseline_and_first_event_are_independent() {
    use orch_proto::ops::results::SubscribeAck;
    let ServerFrame::Ok(frame) = ServerFrame::decode(
        br#"{"id":1,"ok":{"subscription":3,"instance":"i","seq":0},"commit_seq":9000}"#,
    )
    .unwrap() else {
        panic!("terminal ack")
    };
    let ack: SubscribeAck = serde_json::from_str(frame.ok.get()).unwrap();
    assert_eq!(ack.seq, 0);
    assert_eq!(frame.commit_seq, 9000);
    let ServerFrame::Event(frame) = ServerFrame::decode(br#"{"ev":{"subscription":3,"instance":"i","seq":1,"store":"tasks","id":"tsk_1","kind":"created","commit_seq":9001}}"#).unwrap() else { panic!("first event") };
    assert_eq!(frame.ev.seq, 1);
    assert!(matches!(
        frame.ev.body,
        EventBody::Change { id: Some(_), .. }
    ));
}

#[test]
fn store_wide_changes_are_updates_and_shapes_do_not_mix() {
    for body in [
        r#""store":"tasks","kind":"created","commit_seq":9"#,
        r#""store":"tasks","kind":"deleted","commit_seq":9"#,
        r#""store":"state","kind":"updated","commit_seq":9,"resync":{"reason":"lagged"}"#,
        r#""id":"t","resync":{"reason":"lagged"}"#,
        r#""resync":{"reason":"unknown"}"#,
    ] {
        let raw = format!(r#"{{"ev":{{"subscription":3,"instance":"i","seq":1,{body}}}}}"#);
        assert!(ServerFrame::decode(raw.as_bytes()).is_err(), "{raw}");
    }
}
