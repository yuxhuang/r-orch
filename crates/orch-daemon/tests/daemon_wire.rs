#![cfg(feature = "testing")]
//! The daemon's side of the wire (T1; design D6, D12 `daemon_protocol::*`;
//! spec R6, R7): what the real session code does with each violation, through
//! the in-process harness on the same byte stream production uses.

mod support;

use orch_daemon::testing::DaemonHarness;
use orch_proto::frame::MAX_FRAME_BYTES;
use orch_proto::{BINARY_VERSION, LOCAL_PROTOCOL};
use serde_json::{Value, json};
use support::{Raw, err_code, hello, ok, session_hello};

const FUTURE: u32 = LOCAL_PROTOCOL + 1;

async fn daemon() -> DaemonHarness {
    DaemonHarness::start().await.expect("start harness")
}

async fn task_titles(daemon: &DaemonHarness) -> Vec<String> {
    let mut raw = Raw::session(daemon, "cli", "normal").await;
    let reply = raw.request(1, "task.list", json!({})).await;
    ok(&reply)
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["title"].as_str().unwrap().to_owned())
        .collect()
}

// -- bounds ---------------------------------------------------------------

/// A frame of exactly 1 MiB is read in full and answered (here with an
/// attributed `invalid_arguments`, since it carries a pad field); one byte
/// more closes the session without an answer. The exact frame leaves the
/// session usable.
#[tokio::test]
async fn one_mebibyte_frame_is_answered_and_one_byte_more_closes_the_session() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;

    let shell = |pad: usize| {
        format!(
            r#"{{"id":5,"req":{{"op":"context.list","args":{{}},"pad":"{}"}}}}"#,
            "x".repeat(pad)
        )
    };
    let base = shell(0).len();
    let exact = shell(MAX_FRAME_BYTES - base);
    assert_eq!(exact.len(), MAX_FRAME_BYTES);
    raw.send_text(&exact).await;
    let reply = raw.terminal(5).await;
    assert_eq!(err_code(&reply), "invalid_arguments");
    assert_eq!(reply["err"]["execution"], "not_executed");
    assert!(
        reply["err"]["message"].as_str().unwrap().len() < 2048,
        "the error must not echo the 1 MiB payload"
    );

    // Still alive: the next request is served.
    let status = raw.request(6, "daemon.status", json!({})).await;
    assert!(ok(&status)["pid"].is_u64());

    // One byte over: the session is closed, with no answer to that frame.
    raw.send_text(&shell(MAX_FRAME_BYTES - base + 1)).await;
    let rest = raw.until_closed().await;
    assert!(
        rest.iter().all(
            |frame| frame["id"] != 5 || frame.get("err").is_none() && frame.get("ok").is_none()
        ),
        "an over-long frame must not be answered: {rest:?}"
    );

    // The daemon is unaffected for other sessions.
    let mut other = Raw::session(&daemon, "cli", "normal").await;
    ok(&other.request(1, "daemon.status", json!({})).await);
    daemon.shutdown().await.unwrap();
}

/// A request delivered in many small writes, split inside the JSON and inside
/// a multi-byte scalar, is one request.
#[tokio::test]
async fn a_request_written_a_byte_at_a_time_is_one_request() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let frame = support::line(&json!({
        "id": 3,
        "req": {"op": "task.create", "args": {"title": "h\u{e9}llo \u{1F980}"}}
    }));
    for byte in &frame {
        raw.write_bytes(std::slice::from_ref(byte)).await;
    }
    let reply = raw.terminal(3).await;
    assert_eq!(ok(&reply)["title"], "h\u{e9}llo \u{1F980}");
    assert_eq!(task_titles(&daemon).await, ["h\u{e9}llo \u{1F980}"]);
    daemon.shutdown().await.unwrap();
}

/// Two requests in one write are two requests, answered by id.
#[tokio::test]
async fn pipelined_requests_in_one_write_are_both_answered() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let mut both = support::line(&json!({"id": 1, "req": {"op": "context.list", "args": {}}}));
    both.extend(support::line(
        &json!({"id": 2, "req": {"op": "daemon.status", "args": {}}}),
    ));
    raw.write_bytes(&both).await;
    let (a, b) = (raw.terminal(1).await, raw.terminal(2).await);
    assert!(ok(&a).is_array());
    assert!(ok(&b)["pid"].is_u64());
    daemon.shutdown().await.unwrap();
}

/// Let the daemon's tasks run (a cooperative scheduler point, not a delay).
async fn let_daemon_run() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

/// A repeated id while the first is outstanding is a protocol violation: the
/// duplicate is never answered and the session closes. `logs.follow` stays
/// outstanding until cancelled.
#[tokio::test]
async fn a_duplicate_outstanding_id_closes_the_session_unanswered() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    raw.send(&json!({"id": 9, "req": {"op": "logs.follow", "args": {}}}))
        .await;
    let_daemon_run().await;
    raw.send(&json!({"id": 9, "req": {"op": "daemon.status", "args": {}}}))
        .await;
    let rest = raw.until_closed().await;
    assert!(
        rest.iter()
            .all(|frame| frame.get("ok").is_none_or(|ok| ok.get("pid").is_none())),
        "the duplicate was answered: {rest:?}"
    );
    daemon.shutdown().await.unwrap();
}

/// Depth 64 is a normal (attributable) decode error; depth 65 never reaches a
/// parser and closes the session without a reply.
#[tokio::test]
async fn nesting_deeper_than_64_closes_the_session_but_64_is_only_a_bad_request() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let nest = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    // The request object itself consumes two levels (frame, req); args get the
    // rest. 62 levels of args is exactly depth 64 overall.
    raw.send_text(&format!(
        r#"{{"id":1,"req":{{"op":"context.list","args":{}}}}}"#,
        nest(62)
    ))
    .await;
    let reply = raw.terminal(1).await;
    assert_eq!(err_code(&reply), "invalid_arguments");

    raw.send_text(&format!(
        r#"{{"id":2,"req":{{"op":"context.list","args":{}}}}}"#,
        nest(63)
    ))
    .await;
    let rest = raw.until_closed().await;
    assert!(rest.is_empty(), "depth 65 must close silently: {rest:?}");
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unknown_op_is_an_attributed_error_and_the_session_survives() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let reply = raw
        .request(1, "fs.read", json!({"path": "/etc/passwd"}))
        .await;
    assert_eq!(err_code(&reply), "invalid_arguments");
    assert_eq!(reply["err"]["execution"], "not_executed");
    assert_eq!(reply["err"]["message"], "unknown operation");
    // Not even the op name is echoed back.
    assert!(!reply.to_string().contains("passwd"), "{reply}");
    ok(&raw.request(2, "daemon.status", json!({})).await);
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unparseable_frame_without_a_recoverable_id_closes_the_session() {
    let daemon = daemon().await;
    for garbage in [
        r#"not json"#.to_owned(),
        r#"{"req":{"op":"daemon.status","args":{}}}"#.to_owned(), // no id
        r#"{"id":0,"req":{"op":"daemon.status","args":{}}}"#.to_owned(), // id 0
        r#"[1,2]"#.to_owned(),
    ] {
        let mut raw = Raw::session(&daemon, "cli", "normal").await;
        raw.send_text(&garbage).await;
        let rest = raw.until_closed().await;
        assert!(rest.is_empty(), "{garbage}: {rest:?}");
    }
    // An empty line is a framing error too.
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    raw.write_bytes(b"\n").await;
    assert!(raw.until_closed().await.is_empty());
    daemon.shutdown().await.unwrap();
}

/// A peer that vanishes mid-frame costs only its own session; the unfinished
/// frame is never executed.
#[tokio::test]
async fn premature_eof_mid_frame_ends_only_that_session_and_executes_nothing() {
    let daemon = daemon().await;
    {
        let mut raw = Raw::session(&daemon, "cli", "normal").await;
        let frame = support::line(&json!({
            "id": 1, "req": {"op": "task.create", "args": {"title": "half"}}
        }));
        raw.write_bytes(&frame[..frame.len() - 1]).await; // everything but the LF
        // Dropping closes the connection with the frame unterminated.
    }
    // The daemon serves others and never created the task.
    assert_eq!(task_titles(&daemon).await, Vec::<String>::new());
    daemon.shutdown().await.unwrap();
}

/// BUG repro (see the fix task named in the ignore reason): when a session is
/// torn down before the `logs.follow` task has registered its observation, the
/// teardown cannot cancel it, so the follow loops until the daemon stops and
/// the connection's writer is never released: the client never sees EOF.
/// This is the same exchange as the test above without the scheduler point.
#[tokio::test]
async fn a_session_closed_immediately_after_logs_follow_is_fully_torn_down() {
    let daemon = daemon().await;
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    raw.send(&json!({"id": 9, "req": {"op": "logs.follow", "args": {}}}))
        .await;
    raw.send(&json!({"id": 9, "req": {"op": "daemon.status", "args": {}}}))
        .await;
    let rest = tokio::time::timeout(std::time::Duration::from_secs(8), raw.until_closed())
        .await
        .expect("the closed session must reach EOF within the 5 s drain");
    assert!(
        rest.iter()
            .all(|f| f.get("ok").is_none_or(|ok| ok.get("pid").is_none()))
    );
    daemon.shutdown().await.unwrap();
}

// -- version skew ---------------------------------------------------------

/// Everything a client could pipeline behind a skewed hello. If any of it ran,
/// a task would exist afterwards.
fn pipelined_mutation() -> Vec<u8> {
    support::line(
        &json!({"id": 1, "req": {"op": "task.create", "args": {"title": "must-not-exist"}}}),
    )
}

async fn skewed_attempt(daemon: &DaemonHarness, hello_frame: Value) -> (Value, Vec<Value>) {
    let mut raw = Raw::open(daemon).await;
    raw.send(&hello_frame).await;
    raw.write_bytes(&pipelined_mutation()).await;
    let welcome = raw
        .recv()
        .await
        .expect("a welcome naming the daemon's versions");
    (welcome, raw.until_closed().await)
}

/// Each axis, each direction: a client whose protocol or binary differs from
/// the daemon's gets the *daemon's* versions back, `compatible: false`, and
/// the connection is closed before any pipelined request is decoded.
#[tokio::test]
async fn a_client_on_any_other_protocol_or_binary_is_refused_with_the_daemons_versions() {
    let daemon = daemon().await;
    let root = daemon.project_root().to_path_buf();
    let cases = [
        ("client protocol higher", FUTURE, BINARY_VERSION),
        (
            "client protocol lower",
            LOCAL_PROTOCOL.saturating_sub(1),
            BINARY_VERSION,
        ),
        ("client binary higher", LOCAL_PROTOCOL, "99.0.0"),
        ("client binary lower", LOCAL_PROTOCOL, "0.0.1"),
        (
            "equal protocol, binary mismatch",
            LOCAL_PROTOCOL,
            "0.13.0-other",
        ),
    ];
    for (label, protocol, binary) in cases {
        let h = hello(
            &root,
            protocol,
            binary,
            json!({"intent": "session", "caller": {"kind": "cli"}, "mode": "normal"}),
        );
        let (welcome, rest) = skewed_attempt(&daemon, h).await;
        assert_eq!(
            welcome["welcome"]["compatible"], false,
            "{label}: {welcome}"
        );
        assert_eq!(welcome["welcome"]["protocol"], LOCAL_PROTOCOL, "{label}");
        assert_eq!(welcome["welcome"]["binary"], BINARY_VERSION, "{label}");
        assert!(
            rest.is_empty(),
            "{label}: frames after a skewed welcome: {rest:?}"
        );
    }
    assert_eq!(
        task_titles(&daemon).await,
        Vec::<String>::new(),
        "a skewed request ran"
    );
    daemon.shutdown().await.unwrap();
}

/// The mirror image: the *daemon* is the one on another version (harness
/// seam), the client is this build.
#[tokio::test]
async fn a_daemon_on_another_protocol_or_binary_refuses_this_client_and_runs_nothing() {
    for (label, protocol, binary) in [
        ("daemon protocol higher", FUTURE, BINARY_VERSION),
        (
            "daemon protocol lower",
            LOCAL_PROTOCOL.saturating_sub(1),
            BINARY_VERSION,
        ),
        ("daemon binary higher", LOCAL_PROTOCOL, "99.0.0"),
        ("daemon binary lower", LOCAL_PROTOCOL, "0.0.1"),
    ] {
        let daemon = DaemonHarness::builder()
            .versions(protocol, binary)
            .start()
            .await
            .unwrap();
        let h = session_hello(daemon.project_root(), "cli", "normal");
        let (welcome, rest) = skewed_attempt(&daemon, h).await;
        assert_eq!(
            welcome["welcome"]["compatible"], false,
            "{label}: {welcome}"
        );
        assert_eq!(welcome["welcome"]["protocol"], protocol, "{label}");
        assert_eq!(welcome["welcome"]["binary"], binary, "{label}");
        assert!(rest.is_empty(), "{label}: {rest:?}");
        assert!(
            daemon
                .engine()
                .tasks
                .list(&Default::default())
                .await
                .unwrap()
                .is_empty(),
            "{label}: a request behind a skewed hello was executed"
        );
        daemon.shutdown().await.unwrap();
    }
}

/// The text a client would print for these exchanges names both versions
/// exactly; compose it from the replies above the way `orch-client` does.
#[tokio::test]
async fn the_versions_in_a_skewed_welcome_are_enough_to_print_the_restart_message() {
    let daemon = DaemonHarness::builder()
        .versions(FUTURE, "9.9.9")
        .start()
        .await
        .unwrap();
    let (welcome, _) = skewed_attempt(
        &daemon,
        session_hello(daemon.project_root(), "cli", "normal"),
    )
    .await;
    let failure = orch_proto::error::ConnectFailure::VersionSkew {
        project_root: daemon.project_root().display().to_string(),
        skew: orch_proto::handshake::VersionSkew {
            client_protocol: LOCAL_PROTOCOL,
            client_binary: BINARY_VERSION.to_owned(),
            daemon_protocol: welcome["welcome"]["protocol"].as_u64().unwrap() as u32,
            daemon_binary: welcome["welcome"]["binary"].as_str().unwrap().to_owned(),
        },
    };
    let text = failure.render(false);
    assert!(
        text.contains(&format!("daemon is orch 9.9.9 protocol {FUTURE}, this client is orch {BINARY_VERSION} protocol {LOCAL_PROTOCOL}")),
        "{text}"
    );
    assert!(
        text.ends_with("Start it with: orch daemon stop, then orch daemon start"),
        "{text}"
    );
    daemon.shutdown().await.unwrap();
}

// -- frozen envelope ------------------------------------------------------

/// Unknown fields anywhere in the frozen hello are ignored; at the daemon's
/// own versions that is a normal session, at other versions a skew welcome.
#[tokio::test]
async fn a_hello_with_unknown_fields_is_a_normal_hello() {
    let daemon = daemon().await;
    let root = daemon.project_root().to_path_buf();
    let mut raw = Raw::open(&daemon).await;
    let mut frame = session_hello(&root, "cli", "normal");
    frame["hello"]["future_capability"] = json!({"push": true});
    frame["trace_id"] = json!("abc");
    frame["hello"]["project_id"]["generation"] = json!(3);
    raw.send(&frame).await;
    let welcome = raw.recv().await.unwrap();
    assert_eq!(welcome["welcome"]["compatible"], true, "{welcome}");
    ok(&raw.request(1, "daemon.status", json!({})).await);

    // The same envelope from a future client reports skew, not an error.
    let mut future = hello(
        &root,
        FUTURE,
        "9.0.0",
        json!({"intent": "session", "caller": {"kind": "ide", "x": 1}, "mode": "supervised"}),
    );
    future["hello"]["capabilities"] = json!(["push"]);
    let (welcome, rest) = skewed_attempt(&daemon, future).await;
    assert_eq!(welcome["welcome"]["compatible"], false, "{welcome}");
    assert!(welcome.get("reject").is_none());
    assert!(rest.is_empty());
    daemon.shutdown().await.unwrap();
}

/// At equal versions the session fields are strict: an unknown caller kind or
/// intent is a `handshake_failed` reject, never a half-open session.
#[tokio::test]
async fn at_equal_versions_unknown_caller_mode_or_intent_is_handshake_failed() {
    let daemon = daemon().await;
    let root = daemon.project_root().to_path_buf();
    let cases = [
        json!({"intent": "session", "caller": {"kind": "ide"}, "mode": "normal"}),
        json!({"intent": "session", "caller": {"kind": "cli"}, "mode": "supervised"}),
        json!({"intent": "session", "caller": {"kind": "cli", "extra": 1}, "mode": "normal"}),
        json!({"intent": "session", "mode": "normal"}),
        json!({"intent": "drain", "caller": {"kind": "cli"}, "mode": "normal"}),
    ];
    for extra in cases {
        let mut raw = Raw::open(&daemon).await;
        raw.send(&hello(&root, LOCAL_PROTOCOL, BINARY_VERSION, extra.clone()))
            .await;
        let reply = raw.recv().await.expect("a reject");
        assert_eq!(
            reply["reject"]["code"], "handshake_failed",
            "{extra}: {reply}"
        );
        assert!(raw.until_closed().await.is_empty(), "{extra}");
    }
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_malformed_or_oversize_hello_is_rejected_and_closed() {
    let daemon = daemon().await;
    let root = daemon.project_root().to_path_buf();
    for bytes in [
        b"{\"hello\":".to_vec(),         // truncated
        b"{\"nothello\":{}}\n".to_vec(), // wrong envelope
        format!("{{\"hello\":{{\"pad\":\"{}\"}}}}\n", "x".repeat(17 * 1024)).into_bytes(), // > 16 KiB
    ] {
        let mut raw = Raw::open(&daemon).await;
        raw.write_bytes(&bytes).await;
        let reply = raw.recv().await;
        // Either an explicit reject or a plain close; never a welcome.
        if let Some(reply) = reply {
            assert_eq!(reply["reject"]["code"], "handshake_failed", "{reply}");
        }
        assert!(raw.until_closed().await.is_empty());
    }
    // A hello is valid only in first position: a request before it is refused.
    let mut raw = Raw::open(&daemon).await;
    raw.send(&json!({"id": 1, "req": {"op": "daemon.status", "args": {}}}))
        .await;
    let reply = raw.recv().await;
    assert!(
        reply.is_none_or(|r| r.get("ok").is_none()),
        "request served before any hello"
    );
    let _ = root;
    daemon.shutdown().await.unwrap();
}

// -- the stop intent crosses skew ----------------------------------------

/// The one exchange that works across any skew: a stop hello is acknowledged
/// (`accepted`), the daemon performs its normal clean shutdown, and the
/// client gets `completed`.
#[tokio::test]
async fn a_stop_hello_from_a_different_version_stops_the_daemon_cleanly() {
    for (label, protocol, binary) in [
        ("future client", FUTURE, "9.0.0"),
        ("past client", 0, "0.0.1"),
        ("same protocol, other build", LOCAL_PROTOCOL, "0.0.0-other"),
    ] {
        let daemon = daemon().await;
        let root = daemon.project_root().to_path_buf();
        let mut raw = Raw::open(&daemon).await;
        // Session fields of a future client are never decoded for a stop.
        raw.send(&hello(
            &root,
            protocol,
            binary,
            json!({"intent": "stop", "caller": {"kind": "hologram"}, "mode": "telepathic"}),
        ))
        .await;
        let accepted = raw.recv().await.expect("accepted");
        assert_eq!(
            accepted["welcome"]["stop_state"], "accepted",
            "{label}: {accepted}"
        );
        assert_eq!(accepted["welcome"]["compatible"], false, "{label}");
        let completed = raw.recv().await.expect("completed");
        assert_eq!(
            completed["welcome"]["stop_state"], "completed",
            "{label}: {completed}"
        );
        // `completed` is the terminal cleanup result; the harness's own
        // shutdown is idempotent afterwards and must still report clean.
        daemon.shutdown().await.unwrap();
        assert!(!root.join(".orchestry/orchestry.lock").exists(), "{label}");
    }
}

/// The stop intent is still bound to the right project and the right uid:
/// a stop for another project is a wrong_project reject and stops nothing.
#[tokio::test]
async fn a_stop_hello_for_another_project_is_refused_and_stops_nothing() {
    let daemon = daemon().await;
    let other = daemon::tempdir();
    let mut raw = Raw::open(&daemon).await;
    raw.send(&hello(
        other.path(),
        LOCAL_PROTOCOL,
        BINARY_VERSION,
        json!({"intent": "stop"}),
    ))
    .await;
    let reply = raw.recv().await.unwrap();
    assert_eq!(reply["reject"]["code"], "wrong_project", "{reply}");
    // Still serving.
    let mut live = Raw::session(&daemon, "cli", "normal").await;
    ok(&live.request(1, "daemon.status", json!({})).await);
    daemon.shutdown().await.unwrap();
}

mod daemon {
    /// A second initialized project, so its `.orchestry` has a different
    /// dev/ino from the daemon's.
    pub fn tempdir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".orchestry")).unwrap();
        dir
    }
}
