//! Wire-protocol vectors (T1, design D12 `daemon_protocol::*`; spec R6, R7).
//!
//! These run on the codec and DTOs alone, with no daemon: the session-level
//! consequences (what the daemon does with each violation) are in
//! `orch-daemon/tests/daemon_wire.rs`.

use orch_proto::bounded::Id;
use orch_proto::error::{ConnectFailure, ErrorBody, ErrorCode, Execution, START_COMMAND};
use orch_proto::frame::{
    ChangeKind, ErrFrame, Event, EventBody, EventFrame, MAX_DEPTH, MAX_FRAME_BYTES, OkFrame, Part,
    PartFrame, PartKind, RequestFrame, RequestId, ResyncReason, ServerFrame, Store, StreamEncoding,
    StreamedResult,
};
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, Hello, HelloFrame, Intent, MAX_HANDSHAKE_BYTES, ProjectId,
    Reply, SessionMode, StopState,
};
use orch_proto::ndjson::{self, CodecError, FrameReader};
use orch_proto::ops::args::{ContextListArgs, TaskGetArgs};
use orch_proto::ops::{CATALOGUE, Op};
use orch_proto::path::WirePath;
use orch_proto::{BINARY_VERSION, LOCAL_PROTOCOL};
use serde::de::DeserializeOwned;
use serde_json::value::to_raw_value;
use tokio::io::{AsyncWriteExt, BufReader};

const PROJECT: ProjectId = ProjectId { dev: 3, ino: 4 };

fn id(n: u64) -> RequestId {
    RequestId(n.try_into().unwrap())
}

fn reader(bytes: &[u8]) -> FrameReader<BufReader<&[u8]>> {
    FrameReader::new(BufReader::new(bytes))
}

// ---------------------------------------------------------------------------
// frame_catalogue_and_bounds
// ---------------------------------------------------------------------------

/// Every frame kind survives encode → line reader → decode unchanged.
#[tokio::test]
async fn every_frame_kind_round_trips_through_the_line_codec() {
    let request = RequestFrame {
        id: id(7),
        req: TaskGetArgs {
            id: Id::new("tsk_1").unwrap(),
        }
        .into_request(),
    };
    let ok = OkFrame {
        id: id(7),
        ok: to_raw_value(&serde_json::json!({"a": [1, 2, {"b": null}]})).unwrap(),
        commit_seq: 41,
    };
    let void_ok = OkFrame {
        id: id(8),
        ok: to_raw_value(&serde_json::Value::Null).unwrap(),
        commit_seq: 0,
    };
    let err = ErrFrame {
        id: id(9),
        err: ErrorBody::new(ErrorCode::NotPermitted, "nope", Execution::NotExecuted),
    };
    let parts: Vec<PartFrame> = [PartKind::Log, PartKind::Event, PartKind::ResultChunk]
        .into_iter()
        .enumerate()
        .map(|(n, kind)| PartFrame {
            id: id(10),
            part: Part {
                n: n as u64,
                kind,
                data: to_raw_value(&serde_json::json!({"n": n})).unwrap(),
            },
        })
        .collect();
    let events: Vec<EventFrame> = [
        EventBody::Change {
            store: Store::Tasks,
            id: Id::new("tsk_1").unwrap(),
            kind: ChangeKind::Updated,
            commit_seq: 5,
        },
        EventBody::Resync {
            reason: ResyncReason::Lagged,
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(i, body)| EventFrame {
        ev: Event {
            subscription: 3,
            instance: Id::new("inst").unwrap(),
            seq: i as u64 + 1,
            body,
        },
    })
    .collect();
    let streamed = OkFrame {
        id: id(11),
        ok: to_raw_value(&StreamedResult {
            streamed: true,
            encoding: StreamEncoding::JsonUtf8,
            bytes: 9,
            sha256: Id::new("ab".repeat(32)).unwrap(),
        })
        .unwrap(),
        commit_seq: 3,
    };

    // One buffer holding the whole conversation, one frame per line.
    let mut wire = Vec::new();
    wire.extend(ndjson::encode(&request, MAX_FRAME_BYTES).unwrap());
    let server_lines: Vec<Vec<u8>> = [
        ndjson::encode(&ok, MAX_FRAME_BYTES).unwrap(),
        ndjson::encode(&void_ok, MAX_FRAME_BYTES).unwrap(),
        ndjson::encode(&err, MAX_FRAME_BYTES).unwrap(),
        ndjson::encode(&streamed, MAX_FRAME_BYTES).unwrap(),
    ]
    .into_iter()
    .chain(
        parts
            .iter()
            .map(|p| ndjson::encode(p, MAX_FRAME_BYTES).unwrap()),
    )
    .chain(
        events
            .iter()
            .map(|e| ndjson::encode(e, MAX_FRAME_BYTES).unwrap()),
    )
    .collect();
    for line in &server_lines {
        wire.extend_from_slice(line);
    }

    let mut frames = reader(&wire);
    let got: Option<RequestFrame> = frames.read(MAX_FRAME_BYTES).await.unwrap();
    assert_eq!(got, Some(request));
    let mut decoded = Vec::new();
    while let Some(line) = frames.read_line(MAX_FRAME_BYTES).await.unwrap() {
        decoded.push(ServerFrame::decode(&line).unwrap());
    }
    assert_eq!(decoded.len(), server_lines.len());
    assert!(matches!(&decoded[0], ServerFrame::Ok(f) if f.commit_seq == 41 && f.id == id(7)));
    assert!(
        matches!(&decoded[1], ServerFrame::Ok(f) if f.ok.get() == "null"),
        "a void result must stay a result, not become an absent key"
    );
    assert!(matches!(&decoded[2], ServerFrame::Err(f) if *f == err));
    assert!(matches!(&decoded[3], ServerFrame::Ok(f) if f.ok.get().contains("\"streamed\":true")));
    for (i, part) in parts.iter().enumerate() {
        assert!(
            matches!(&decoded[4 + i], ServerFrame::Part(f) if f.part.n == i as u64 && f.part.kind == part.part.kind)
        );
    }
    for (i, event) in events.iter().enumerate() {
        assert!(matches!(&decoded[7 + i], ServerFrame::Event(f) if f == event));
    }
}

/// A frame of exactly `MAX_FRAME_BYTES` is accepted, one byte more is refused
/// on both the writing and the reading side, and neither side truncates.
#[tokio::test]
async fn one_mebibyte_is_accepted_exactly_and_one_byte_more_is_refused() {
    let shell = |pad: usize| OkFrame {
        id: id(1),
        ok: to_raw_value(&"x".repeat(pad)).unwrap(),
        commit_seq: 0,
    };
    let base = serde_json::to_vec(&shell(0)).unwrap().len();
    let exact = shell(MAX_FRAME_BYTES - base);
    let over = shell(MAX_FRAME_BYTES - base + 1);

    let line = ndjson::encode(&exact, MAX_FRAME_BYTES).unwrap();
    assert_eq!(
        line.len(),
        MAX_FRAME_BYTES + 1,
        "payload plus its single LF"
    );
    assert!(matches!(
        ndjson::encode(&over, MAX_FRAME_BYTES),
        Err(CodecError::Oversize { max }) if max == MAX_FRAME_BYTES
    ));

    // Reader side: the same two lines, written without the writer's check.
    let mut wire = serde_json::to_vec(&exact).unwrap();
    assert_eq!(wire.len(), MAX_FRAME_BYTES);
    wire.push(b'\n');
    assert!(
        reader(&wire)
            .read_line(MAX_FRAME_BYTES)
            .await
            .unwrap()
            .is_some()
    );
    let mut wire = serde_json::to_vec(&over).unwrap();
    assert_eq!(wire.len(), MAX_FRAME_BYTES + 1);
    wire.push(b'\n');
    assert!(matches!(
        reader(&wire).read_line(MAX_FRAME_BYTES).await,
        Err(CodecError::Oversize { .. })
    ));
    // An unterminated over-long line is refused before EOF is reached.
    let endless = vec![b'x'; MAX_FRAME_BYTES + 2];
    assert!(matches!(
        reader(&endless).read_line(MAX_FRAME_BYTES).await,
        Err(CodecError::Oversize { .. })
    ));
}

/// A frame split at every possible boundary, including inside a multi-byte
/// scalar, is reassembled from a tiny pipe; the reader owns the partial line.
#[tokio::test]
async fn a_frame_split_into_single_bytes_is_reassembled() {
    let frame = OkFrame {
        id: id(1),
        ok: to_raw_value(&"h\u{e9}llo \u{1F980} w\u{f6}rld").unwrap(),
        commit_seq: 2,
    };
    let mut wire = ndjson::encode(&frame, MAX_FRAME_BYTES).unwrap();
    wire.extend(ndjson::encode(&frame, MAX_FRAME_BYTES).unwrap());
    // Two lines in a 3-byte pipe, written one byte at a time.
    let (mut tx, rx) = tokio::io::duplex(3);
    let writer = tokio::spawn(async move {
        for byte in wire {
            tx.write_all(&[byte]).await.unwrap();
            tx.flush().await.unwrap();
        }
    });
    let mut frames = FrameReader::new(BufReader::new(rx));
    for _ in 0..2 {
        let line = frames.read_line(MAX_FRAME_BYTES).await.unwrap().unwrap();
        let ServerFrame::Ok(got) = ServerFrame::decode(&line).unwrap() else {
            panic!("not an ok frame");
        };
        assert_eq!(got.ok.get(), frame.ok.get());
    }
    writer.await.unwrap();
    assert!(frames.read_line(MAX_FRAME_BYTES).await.unwrap().is_none());
}

#[tokio::test]
async fn premature_eof_in_a_frame_is_an_error_but_clean_eof_between_frames_is_not() {
    let whole = br#"{"id":1,"req":{"op":"context.list","args":{}}}"#;
    // Cut anywhere inside the frame: unterminated.
    for cut in [1, 10, whole.len()] {
        assert!(
            matches!(
                reader(&whole[..cut]).read_line(MAX_FRAME_BYTES).await,
                Err(CodecError::Unterminated)
            ),
            "cut at {cut}"
        );
    }
    // After a terminated frame, EOF is the peer closing: not an error.
    let mut wire = whole.to_vec();
    wire.push(b'\n');
    let mut frames = reader(&wire);
    assert!(frames.read_line(MAX_FRAME_BYTES).await.unwrap().is_some());
    assert!(frames.read_line(MAX_FRAME_BYTES).await.unwrap().is_none());
    // No bytes at all is also a clean close.
    assert!(
        reader(b"")
            .read_line(MAX_FRAME_BYTES)
            .await
            .unwrap()
            .is_none()
    );
}

#[test]
fn empty_lines_and_blank_frames_are_refused() {
    let r = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    r.block_on(async {
        assert!(matches!(
            reader(b"\n").read_line(MAX_FRAME_BYTES).await,
            Err(CodecError::Empty)
        ));
        // A CR is not a terminator: the line is non-empty and not JSON.
        let line = reader(b"\r\n").read_line(MAX_FRAME_BYTES).await.unwrap();
        assert!(ndjson::decode::<RequestFrame>(&line.unwrap()).is_err());
    });
}

/// Depth 64 decodes; depth 65 is refused before parsing, so a hostile
/// nesting cannot reach serde's recursion at all.
#[test]
fn nesting_depth_64_is_accepted_and_65_is_refused() {
    let nest = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    assert!(ndjson::decode::<serde_json::Value>(nest(MAX_DEPTH).as_bytes()).is_ok());
    assert!(matches!(
        ndjson::decode::<serde_json::Value>(nest(MAX_DEPTH + 1).as_bytes()),
        Err(CodecError::TooDeep)
    ));
    // The same bound applies inside a request frame's payload.
    let deep_args = format!(
        r#"{{"id":1,"req":{{"op":"context.list","args":{}}}}}"#,
        nest(MAX_DEPTH)
    );
    assert!(matches!(
        ndjson::decode::<RequestFrame>(deep_args.as_bytes()),
        Err(CodecError::TooDeep)
    ));
    // Deep brackets inside a string are data, not nesting.
    let quoted = format!(
        r#"{{"id":1,"req":{{"op":"context.list","args":{{}}}},"s":"{}"}}"#,
        "[".repeat(500)
    );
    assert!(!matches!(
        ndjson::decode::<RequestFrame>(quoted.as_bytes()),
        Err(CodecError::TooDeep)
    ));
}

#[test]
fn unknown_op_unknown_tag_and_duplicate_keys_are_refused_without_echoing_input() {
    let refuse = |json: &str| {
        let error = ndjson::decode::<RequestFrame>(json.as_bytes())
            .expect_err(&format!("accepted: {json}"));
        // Errors are a category and an offset, never the offending text.
        let shown = error.to_string();
        assert!(!shown.contains("canary"), "{shown}");
    };
    refuse(r#"{"id":1,"req":{"op":"canary.op","args":{}}}"#);
    refuse(r#"{"id":1,"req":{"op":"context.list","args":{"canary":1}}}"#);
    refuse(r#"{"id":1,"id":2,"req":{"op":"context.list","args":{}}}"#);
    refuse(r#"{"id":1,"req":{"op":"context.list","op":"context.list","args":{}}}"#);
    refuse(r#"{"id":1,"req":{"args":{}},"canary":true}"#);
    refuse(r#"[1,2,3]"#);
    refuse(r#""canary""#);
    // Invalid UTF-8 in a frame is not JSON.
    assert!(ndjson::decode::<RequestFrame>(b"{\"id\":1,\"req\":\xff}").is_err());
}

macro_rules! probe_every_op {
    ($( ($variant:ident, $method:ident, $name:literal, $args:ty, $out:ty, $class:ident) ),* $(,)?) => {
        /// `(wire name, top-level args type rejects an injected unknown field)`.
        fn probe_unknown_field_on_every_op() -> Vec<(&'static str, Result<(), String>)> {
            vec![ $( ($name, probe_args::<$args>()), )* ]
        }
    };
}

fn probe_args<A: DeserializeOwned>() -> Result<(), String> {
    match serde_json::from_str::<A>(r#"{"__generated_probe_field__":1}"#) {
        Ok(_) => Err("accepted an unknown field".to_owned()),
        // serde reports the unknown key as soon as it sees it, before the
        // missing-field check, so this proves deny_unknown_fields rather
        // than "some other required field was absent".
        Err(e)
            if e.to_string()
                .contains("unknown field `__generated_probe_field__`") =>
        {
            Ok(())
        }
        Err(e) => Err(format!("rejected, but not as an unknown field: {e}")),
    }
}

orch_proto::for_each_op!(probe_every_op);

/// `deny_unknown_fields` on **every** request DTO, generated from the same
/// catalogue macro that defines the `Request` enum, so a newly added
/// operation is probed without anyone remembering to list it.
#[test]
fn every_request_dto_denies_unknown_fields() {
    let results = probe_unknown_field_on_every_op();
    assert_eq!(
        results.len(),
        CATALOGUE.len(),
        "the probe must cover the whole catalogue"
    );
    let failures: Vec<String> = results
        .into_iter()
        .filter_map(|(op, result)| result.err().map(|why| format!("{op}: {why}")))
        .collect();
    assert!(
        failures.is_empty(),
        "open request DTOs:\n{}",
        failures.join("\n")
    );
}

/// The envelope is closed around the op too: a caller, env, cwd or any key
/// next to `op`/`args` cannot smuggle per-request authority (D11).
#[test]
fn the_request_envelope_has_no_room_for_per_request_authority() {
    for smuggled in ["caller", "env", "cwd", "argv", "path", "mode", "agent_id"] {
        let json =
            format!(r#"{{"id":1,"req":{{"op":"context.list","args":{{}},"{smuggled}":"x"}}}}"#);
        assert!(
            serde_json::from_str::<RequestFrame>(&json).is_err(),
            "{smuggled} accepted inside req"
        );
        let json =
            format!(r#"{{"id":1,"req":{{"op":"context.list","args":{{}}}},"{smuggled}":"x"}}"#);
        assert!(
            serde_json::from_str::<RequestFrame>(&json).is_err(),
            "{smuggled} accepted beside req"
        );
    }
    // And the baseline really is accepted, so the loop above proves something.
    let ok = RequestFrame {
        id: id(1),
        req: ContextListArgs {}.into_request(),
    };
    let line = serde_json::to_string(&ok).unwrap();
    assert_eq!(serde_json::from_str::<RequestFrame>(&line).unwrap(), ok);
}

// ---------------------------------------------------------------------------
// strict_skew_both_directions (the client's view; the daemon's view is in
// orch-daemon/tests/daemon_wire.rs)
// ---------------------------------------------------------------------------

fn session_hello(protocol: u32, binary: &str) -> HelloFrame {
    let hello = Hello::session(
        WirePath::new("/tmp/proj").unwrap(),
        PROJECT,
        &Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        },
        SessionMode::Normal,
    );
    let mut value = serde_json::to_value(HelloFrame { hello }).unwrap();
    value["hello"]["protocol"] = protocol.into();
    value["hello"]["binary"] = binary.into();
    serde_json::from_value(value).unwrap()
}

/// Each axis is checked on its own, in both directions, and the equal case
/// passes. Same package semver but a different protocol (and the reverse) are
/// both skew; there is no "close enough".
#[test]
fn any_difference_in_protocol_or_binary_is_skew_in_either_direction() {
    session_hello(LOCAL_PROTOCOL, BINARY_VERSION)
        .hello
        .check_versions()
        .unwrap();
    let cases = [
        (
            "lower protocol",
            LOCAL_PROTOCOL.saturating_sub(1),
            BINARY_VERSION,
        ),
        ("higher protocol", LOCAL_PROTOCOL + 1, BINARY_VERSION),
        ("lower binary", LOCAL_PROTOCOL, "0.0.1"),
        ("higher binary", LOCAL_PROTOCOL, "99.99.99"),
        // Equal protocol, different binary: still skew (spec R6).
        ("binary metadata only", LOCAL_PROTOCOL, "0.13.0+other"),
        ("both", LOCAL_PROTOCOL + 7, "7.7.7"),
    ];
    for (label, protocol, binary) in cases {
        let skew = session_hello(protocol, binary)
            .hello
            .check_versions()
            .expect_err(label);
        assert_eq!(skew.client_protocol, protocol, "{label}");
        assert_eq!(skew.client_binary, binary, "{label}");
        assert_eq!(skew.daemon_protocol, LOCAL_PROTOCOL, "{label}");
        assert_eq!(skew.daemon_binary, BINARY_VERSION, "{label}");
    }
}

/// The user-facing text names both versions exactly and points at the
/// stop-then-start procedure, never the plain start hint (D5).
#[test]
fn the_skew_message_names_both_versions_and_the_restart_procedure() {
    let skew = session_hello(LOCAL_PROTOCOL + 1, "9.9.9")
        .hello
        .check_versions()
        .unwrap_err();
    // This client is the *daemon's* peer: build the message a client prints
    // when the daemon is the higher side.
    let failure = ConnectFailure::VersionSkew {
        project_root: "/tmp/proj".into(),
        skew: orch_proto::handshake::VersionSkew {
            client_protocol: skew.daemon_protocol,
            client_binary: skew.daemon_binary.clone(),
            daemon_protocol: skew.client_protocol,
            daemon_binary: skew.client_binary.clone(),
        },
    };
    let expected = format!(
        "  \u{2715} No orch daemon is running for /tmp/proj (daemon is orch 9.9.9 protocol {}, \
         this client is orch {BINARY_VERSION} protocol {LOCAL_PROTOCOL})\n    \
         Start it with: orch daemon stop, then orch daemon start",
        LOCAL_PROTOCOL + 1
    );
    assert_eq!(failure.render(false), expected);
    assert_eq!(failure.exit_code(), orch_proto::error::exit::NO_DAEMON);
    assert!(
        !failure
            .render(false)
            .contains(&format!("Start it with: {START_COMMAND}\n")),
        "skew must not suggest a bare start"
    );

    // The other direction: daemon lower than the client.
    let lower = ConnectFailure::VersionSkew {
        project_root: "/tmp/proj".into(),
        skew: orch_proto::handshake::VersionSkew {
            client_protocol: LOCAL_PROTOCOL,
            client_binary: BINARY_VERSION.to_owned(),
            daemon_protocol: 0,
            daemon_binary: "0.0.1".to_owned(),
        },
    };
    let text = lower.render(false);
    assert!(text.contains("daemon is orch 0.0.1 protocol 0"), "{text}");
    assert!(
        text.contains(&format!(
            "this client is orch {BINARY_VERSION} protocol {LOCAL_PROTOCOL}"
        )),
        "{text}"
    );
    // In an agent context both versions stay and every hint goes (R25).
    let agent = lower.render(true);
    assert!(agent.contains("Do not start it"), "{agent}");
    assert!(
        agent.contains("0.0.1") && agent.contains(BINARY_VERSION),
        "{agent}"
    );
    assert!(!agent.contains("Start it with"), "{agent}");
}

// ---------------------------------------------------------------------------
// hello envelope tolerance and the stop intent across skew
// ---------------------------------------------------------------------------

/// The frozen envelopes ignore unknown fields at every level, in both
/// directions; this is what lets any future client read a daemon's versions.
#[test]
fn frozen_envelopes_tolerate_unknown_fields_at_every_level() {
    let hello = format!(
        r#"{{"hello":{{"protocol":{LOCAL_PROTOCOL},"binary":"{BINARY_VERSION}",
        "project_root":{{"utf8":"/tmp/p"}},"project_id":{{"dev":1,"ino":2,"gen":3}},
        "intent":"session","caller":{{"kind":"cli"}},"mode":"normal",
        "new_top_level_in_hello":[1,2,3]}},"new_sibling":{{"a":true}}}}"#
    );
    let frame: HelloFrame = serde_json::from_str(&hello).unwrap();
    frame.hello.check_versions().unwrap();
    assert_eq!(frame.hello.intent, Intent::Session);

    // A welcome from a newer daemon: extra fields, an unknown stop_state.
    let welcome = r#"{"welcome":{"protocol":2,"binary":"2.0.0","project_root":{"utf8":"/tmp/p"},
        "project_id":{"dev":1,"ino":2},"instance":"i","pid":9,"compatible":false,
        "stop_state":"draining","features":["push"]},"trace":"t"}"#;
    let reply: HandshakeReply = serde_json::from_str(welcome).unwrap();
    let Some(Reply::Welcome(welcome)) = reply.into_reply() else {
        panic!("expected a welcome");
    };
    assert_eq!((welcome.protocol, welcome.binary.as_str()), (2, "2.0.0"));
    assert!(!welcome.compatible);

    // A reject with extra fields and a code we do not know.
    let reject = r#"{"reject":{"code":"quota","message":"m","retry_after_ms":10},"x":0}"#;
    let reply: HandshakeReply = serde_json::from_str(reject).unwrap();
    assert!(matches!(reply.into_reply(), Some(Reply::Reject(_))));
}

/// Tolerance is not looseness: duplicates, wrong types and an oversize hello
/// are still refused.
#[tokio::test]
async fn envelope_tolerance_does_not_extend_to_duplicates_types_or_size() {
    let dup = format!(
        r#"{{"hello":{{"protocol":1,"binary":"{BINARY_VERSION}","binary":"x","project_root":{{"utf8":"/"}},"project_id":{{"dev":1,"ino":2}}}}}}"#
    );
    assert!(serde_json::from_str::<HelloFrame>(&dup).is_err());
    let wrong = r#"{"hello":{"protocol":"one","binary":"1","project_root":{"utf8":"/"},"project_id":{"dev":1,"ino":2}}}"#;
    assert!(serde_json::from_str::<HelloFrame>(wrong).is_err());

    // The handshake line limit is 16 KiB, exact then over.
    let pad = |n: usize| {
        let mut line = format!(
            r#"{{"hello":{{"protocol":1,"binary":"1","project_root":{{"utf8":"/"}},"project_id":{{"dev":1,"ino":2}},"pad":"{}"}}}}"#,
            "x".repeat(n)
        );
        line.push('\n');
        line
    };
    let base = pad(0).len() - 1;
    let exact = pad(MAX_HANDSHAKE_BYTES - base);
    assert_eq!(exact.len() - 1, MAX_HANDSHAKE_BYTES);
    assert!(
        reader(exact.as_bytes())
            .read_line(MAX_HANDSHAKE_BYTES)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        reader(pad(MAX_HANDSHAKE_BYTES - base + 1).as_bytes())
            .read_line(MAX_HANDSHAKE_BYTES)
            .await,
        Err(CodecError::Oversize { .. })
    ));
}

/// A stop hello from any version — past or future, with or without session
/// fields — decodes as a stop and needs nothing but the envelope; its
/// versions are reported by `check_versions` for diagnostics only.
#[test]
fn a_stop_hello_is_accepted_across_skew_without_decoding_session_fields() {
    for (protocol, binary) in [
        (LOCAL_PROTOCOL + 5, "5.0.0"),
        (0, "0.0.1"),
        (LOCAL_PROTOCOL, "0.0.0-other-build"),
    ] {
        let json = format!(
            r#"{{"hello":{{"protocol":{protocol},"binary":"{binary}","project_root":{{"utf8":"/tmp/p"}},
            "project_id":{{"dev":1,"ino":2}},"intent":"stop",
            "caller":{{"kind":"hologram","wholly":"new"}},"mode":"telepathic"}}}}"#
        );
        let frame: HelloFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame.hello.intent, Intent::Stop, "{protocol} {binary}");
        let skewed = frame.hello.check_versions().is_err();
        assert_eq!(
            skewed,
            !(protocol == LOCAL_PROTOCOL && binary == BINARY_VERSION),
            "{protocol} {binary}"
        );
    }

    // This build's own stop hello carries no session fields at all.
    let own = Hello::stop(WirePath::new("/tmp/p").unwrap(), PROJECT);
    assert_eq!(own.intent, Intent::Stop);
    assert!(
        own.session_identity().is_err(),
        "nothing to decode, by design"
    );
    let json = serde_json::to_string(&HelloFrame { hello: own }).unwrap();
    assert!(json.contains(r#""intent":"stop""#), "{json}");
    assert!(!json.contains("caller") && !json.contains("mode"), "{json}");

    // And the accepted/completed stop states are frozen wire values.
    for (state, text) in [
        (StopState::Accepted, r#""accepted""#),
        (StopState::Completed, r#""completed""#),
    ] {
        assert_eq!(serde_json::to_string(&state).unwrap(), text);
    }
}

// ---------------------------------------------------------------------------
// paths on the wire (D4: lossless for non-UTF-8 roots, escaped for humans)
// ---------------------------------------------------------------------------

#[test]
fn a_non_utf8_path_round_trips_as_tagged_hex_and_a_utf8_path_as_text() {
    use std::os::unix::ffi::OsStrExt;
    let raw = std::ffi::OsStr::from_bytes(b"/tmp/proj-\xff\xfe/x");
    let path = WirePath::new(raw).unwrap();
    let json = serde_json::to_string(&path).unwrap();
    assert!(json.starts_with(r#"{"hex":""#), "{json}");
    let back: WirePath = serde_json::from_str(&json).unwrap();
    assert_eq!(back.as_path().as_os_str().as_bytes(), raw.as_bytes());

    let text = serde_json::to_string(&WirePath::new("/tmp/ok").unwrap()).unwrap();
    assert_eq!(text, r#"{"utf8":"/tmp/ok"}"#);
    // Malformed hex and a NUL byte are refused, in either encoding.
    assert!(serde_json::from_str::<WirePath>(r#"{"hex":"zz"}"#).is_err());
    assert!(serde_json::from_str::<WirePath>(r#"{"hex":"abc"}"#).is_err());
    assert!(serde_json::from_str::<WirePath>("{\"utf8\":\"/a\\u0000b\"}").is_err());
    assert!(WirePath::new("/a\0b").is_err());
    // 4096 bytes is the limit.
    assert!(WirePath::new(format!("/{}", "a".repeat(4095))).is_ok());
    assert!(WirePath::new(format!("/{}", "a".repeat(4096))).is_err());
}

#[test]
fn paths_shown_to_a_human_have_their_control_characters_escaped() {
    let hostile = WirePath::new("/tmp/\u{1b}[2Jevil\nname").unwrap();
    let shown = hostile.display_escaped();
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\n'),
        "{shown:?}"
    );
    assert!(shown.contains("evil"));
    // The same escaping applies to the roots in connection failures.
    let failure = ConnectFailure::no_daemon(
        "/tmp/\u{1b}[2J/x",
        orch_proto::error::NoDaemonDetail::NoSocket,
    );
    assert!(!failure.render(false).contains('\u{1b}'));
}
