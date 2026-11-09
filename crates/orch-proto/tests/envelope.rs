//! The frozen handshake round-trips and tolerates the future; session frames
//! and request DTOs are closed (spec R6, R7).

use orch_proto::bounded::Id;
use orch_proto::frame::RequestFrame;
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, Hello, HelloFrame, Intent, ProjectId, Reject, RejectCode,
    Reply, SessionMode, StopState, Welcome,
};
use orch_proto::ops::args::{TaskCreateArgs, TaskGetArgs};
use orch_proto::ops::{Op, OpClass, Request};
use orch_proto::path::WirePath;
use orch_proto::{BINARY_VERSION, LOCAL_PROTOCOL};

const PROJECT: ProjectId = ProjectId { dev: 1, ino: 2 };

fn root() -> WirePath {
    WirePath::new("/tmp/proj").unwrap()
}

fn caller() -> Caller {
    Caller {
        kind: CallerKind::Cli,
        agent_id: Some(Id::new("agt_1").unwrap()),
        pid: Some(123),
    }
}

fn welcome(stop_state: Option<StopState>) -> Welcome {
    Welcome {
        protocol: LOCAL_PROTOCOL,
        binary: BINARY_VERSION.try_into().unwrap(),
        project_root: root(),
        project_id: PROJECT,
        instance: Id::new("inst_1").unwrap(),
        pid: 42,
        compatible: true,
        stop_state,
    }
}

#[test]
fn session_hello_round_trips() {
    let hello = Hello::session(root(), PROJECT, &caller(), SessionMode::ReadOnly);
    let json = serde_json::to_string(&HelloFrame { hello }).unwrap();
    let back: HelloFrame = serde_json::from_str(&json).unwrap();
    assert_eq!(back.hello.protocol, LOCAL_PROTOCOL);
    assert_eq!(back.hello.binary.as_str(), BINARY_VERSION);
    assert_eq!(back.hello.project_id, PROJECT);
    assert_eq!(back.hello.project_root, root());
    assert_eq!(back.hello.intent, Intent::Session);
    back.hello.check_versions().unwrap();
    let session = back.hello.session_identity().unwrap();
    assert_eq!(session.caller, caller());
    assert_eq!(session.mode, SessionMode::ReadOnly);
}

#[test]
fn stop_hello_round_trips_without_session_fields() {
    let json = serde_json::to_string(&HelloFrame {
        hello: Hello::stop(root(), PROJECT),
    })
    .unwrap();
    assert!(!json.contains("caller"), "{json}");
    let back: HelloFrame = serde_json::from_str(&json).unwrap();
    assert_eq!(back.hello.intent, Intent::Stop);
}

#[test]
fn welcome_and_reject_round_trip() {
    for stop_state in [None, Some(StopState::Accepted), Some(StopState::Completed)] {
        let json = serde_json::to_string(&HandshakeReply::welcome(welcome(stop_state))).unwrap();
        let back: HandshakeReply = serde_json::from_str(&json).unwrap();
        assert_eq!(back.into_reply(), Some(Reply::Welcome(welcome(stop_state))));
    }

    let reject = Reject {
        code: RejectCode::Busy,
        message: "busy".try_into().unwrap(),
        sessions: Some(64),
        cap: Some(64),
        project_root: None,
    };
    let json = serde_json::to_string(&HandshakeReply::reject(reject.clone())).unwrap();
    assert_eq!(
        json,
        r#"{"reject":{"code":"busy","message":"busy","sessions":64,"cap":64}}"#
    );
    let back: HandshakeReply = serde_json::from_str(&json).unwrap();
    assert_eq!(back.into_reply(), Some(Reply::Reject(reject)));
}

/// A hello from a future client — new fields everywhere, a new caller kind,
/// higher versions — still decodes far enough to report skew, not
/// `handshake_failed` (`future_hello_fields_preserve_skew`).
#[test]
fn future_hello_fields_preserve_skew() {
    let json = r#"{"hello":{"protocol":9,"binary":"9.0.0","project_root":{"utf8":"/tmp/proj"},
        "project_id":{"dev":1,"ino":2,"generation":7},"intent":"session",
        "caller":{"kind":"ide","agent_id":null,"pid":1,"tty":"x"},"mode":"supervised",
        "capabilities":["push"]},"trace":"abc"}"#;
    let frame: HelloFrame = serde_json::from_str(json).unwrap();
    let skew = frame.hello.check_versions().unwrap_err();
    assert_eq!(
        (skew.client_protocol, skew.daemon_protocol),
        (9, LOCAL_PROTOCOL)
    );
    assert_eq!(skew.client_binary, "9.0.0");
    // At equal versions the same unknown caller kind would be malformed.
    assert!(frame.hello.session_identity().is_err());

    // Unknown intents, stop states and reject codes are tolerated too.
    let json = r#"{"hello":{"protocol":1,"binary":"0.0.0","project_root":{"utf8":"/"},
        "project_id":{"dev":1,"ino":2},"intent":"drain"}}"#;
    let frame: HelloFrame = serde_json::from_str(json).unwrap();
    assert_eq!(frame.hello.intent, Intent::Unknown);
    let json = r#"{"reject":{"code":"rate_limited","message":"m","retry_after":3},"x":1}"#;
    let reply: HandshakeReply = serde_json::from_str(json).unwrap();
    assert!(matches!(reply.into_reply(), Some(Reply::Reject(r)) if r.code == RejectCode::Unknown));
}

#[test]
fn hello_rejects_duplicate_known_fields_and_bad_types() {
    let duplicate = r#"{"hello":{"protocol":1,"protocol":2,"binary":"0","project_root":{"utf8":"/"},"project_id":{"dev":1,"ino":2}}}"#;
    assert!(serde_json::from_str::<HelloFrame>(duplicate).is_err());
    let bad_type = r#"{"hello":{"protocol":"1","binary":"0","project_root":{"utf8":"/"},"project_id":{"dev":1,"ino":2}}}"#;
    assert!(serde_json::from_str::<HelloFrame>(bad_type).is_err());
    let missing = r#"{"hello":{"protocol":1,"binary":"0","project_root":{"utf8":"/"}}}"#;
    assert!(serde_json::from_str::<HelloFrame>(missing).is_err());
    // Duplicate caller fields are rejected once the session is decoded.
    let dup_caller = format!(
        r#"{{"hello":{{"protocol":{LOCAL_PROTOCOL},"binary":"{BINARY_VERSION}","project_root":{{"utf8":"/"}},"project_id":{{"dev":1,"ino":2}},"caller":{{"kind":"cli","kind":"tui"}}}}}}"#
    );
    let frame: HelloFrame = serde_json::from_str(&dup_caller).unwrap();
    assert!(frame.hello.session_identity().is_err());
}

#[test]
fn hello_and_welcome_wire_shape_is_frozen() {
    let hello = Hello::session(root(), PROJECT, &caller(), SessionMode::Normal);
    let value = serde_json::to_value(HelloFrame { hello }).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"hello": {
            "protocol": LOCAL_PROTOCOL,
            "binary": BINARY_VERSION,
            "project_root": {"utf8": "/tmp/proj"},
            "project_id": {"dev": 1, "ino": 2},
            "intent": "session",
            "caller": {"kind": "cli", "agent_id": "agt_1", "pid": 123},
            "mode": "normal",
        }})
    );
    let value =
        serde_json::to_value(HandshakeReply::welcome(welcome(Some(StopState::Accepted)))).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"welcome": {
            "protocol": LOCAL_PROTOCOL,
            "binary": BINARY_VERSION,
            "project_root": {"utf8": "/tmp/proj"},
            "project_id": {"dev": 1, "ino": 2},
            "instance": "inst_1",
            "pid": 42,
            "compatible": true,
            "stop_state": "accepted",
        }})
    );
}

#[test]
fn request_frames_round_trip_with_their_class() {
    let request = TaskGetArgs {
        id: Id::new("tsk_1").unwrap(),
    }
    .into_request();
    assert_eq!(request.name(), "task.get");
    assert_eq!(request.class(), OpClass::Read);
    let frame = RequestFrame {
        id: orch_proto::frame::RequestId(1.try_into().unwrap()),
        req: request,
    };
    let json = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        json,
        r#"{"id":1,"req":{"op":"task.get","args":{"id":"tsk_1"}}}"#
    );
    assert_eq!(serde_json::from_str::<RequestFrame>(&json).unwrap(), frame);
    assert_eq!(<TaskCreateArgs as Op>::CLASS, OpClass::Mutate);
}

/// `request_dtos_are_typed_and_closed`: unknown fields at every level, unknown
/// ops, missing args and duplicate fields are decode errors.
#[test]
fn request_dtos_reject_unknown_and_duplicate_fields() {
    let reject = |json: &str| {
        assert!(
            serde_json::from_str::<RequestFrame>(json).is_err(),
            "accepted: {json}"
        )
    };
    // Baseline accepted.
    serde_json::from_str::<RequestFrame>(r#"{"id":1,"req":{"op":"task.get","args":{"id":"t"}}}"#)
        .unwrap();
    reject(r#"{"id":1,"req":{"op":"task.get","args":{"id":"t","extra":1}}}"#);
    reject(r#"{"id":1,"req":{"op":"task.get","args":{"id":"t"},"caller":"x"}}"#);
    reject(r#"{"id":1,"req":{"op":"task.get","args":{"id":"t"}},"x":1}"#);
    reject(r#"{"id":1,"req":{"op":"task.get","args":{"id":"t","id":"u"}}}"#);
    reject(r#"{"id":1,"req":{"op":"task.get"}}"#);
    reject(r#"{"id":1,"req":{"op":"fs.read","args":{"path":"/etc/passwd"}}}"#);
    reject(r#"{"id":0,"req":{"op":"task.get","args":{"id":"t"}}}"#);
    // Nested argument structs are closed too.
    reject(
        r#"{"id":1,"req":{"op":"task.definition.apply","args":{"id":"t","definition":
            {"input":{"document":"{}","format":"json","path":"/x"},"kind":"patch"}}}}"#,
    );
    // Bounds are part of the type: a 257-byte id does not decode.
    let long = "x".repeat(257);
    reject(&format!(
        r#"{{"id":1,"req":{{"op":"task.get","args":{{"id":"{long}"}}}}}}"#
    ));
    // Sanity: an untyped value where a typed field belongs is rejected.
    let _: Request = serde_json::from_str(r#"{"op":"context.list","args":{}}"#).unwrap();
    reject(r#"{"id":1,"req":{"op":"context.set","args":{"key":"k","value":{"a":1}}}}"#);
}
