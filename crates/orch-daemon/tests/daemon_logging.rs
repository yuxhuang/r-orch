#![cfg(feature = "testing")]
//! No payload text in daemon logs, at any level (T1; design D11 "Logs/errors";
//! spec R20). This binary installs a process-global `tracing` subscriber that
//! records every event at every level, so it holds exactly one test.

mod support;

use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use std::sync::{Arc, Mutex};
use support::{Raw, session_hello};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const CANARY: &str = "CANARY-7f3a91-secret";

#[derive(Default, Clone)]
struct Capture(Arc<Mutex<Vec<String>>>);

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={:?}", field.name(), value));
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(format!(
            "{} {}{}",
            event.metadata().level(),
            event.metadata().target(),
            fields.0
        ));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Hostile and ordinary traffic, every field carrying a canary: malformed
/// frames, an unknown op, a bad hello, an oversize line, a duplicate id, a
/// rejected mode, a failing operation and a successful one. None of the
/// canary may appear in any log line, and the logs must not be empty (a
/// subscriber that saw nothing proves nothing).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_request_text_reaches_the_logs_at_any_level() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone()).unwrap();

    let daemon = DaemonHarness::start().await.unwrap();
    let root = daemon.project_root().to_path_buf();

    // Bad hellos.
    for frame in [
        format!(r#"{{"hello":{{"protocol":1,"binary":"{CANARY}"}}}}"#),
        format!(r#"{{"hello":{{"protocol":"{CANARY}"}}}}"#),
        format!("not json {CANARY}"),
    ] {
        let mut raw = Raw::open(&daemon).await;
        raw.send_text(&frame).await;
        let _ = raw.until_closed().await;
    }
    // Skewed and wrong-project hellos.
    let mut raw = Raw::open(&daemon).await;
    raw.send(&support::hello(
        &root,
        9,
        CANARY,
        json!({"caller": {"kind": CANARY}}),
    ))
    .await;
    let _ = raw.until_closed().await;
    let mut raw = Raw::open(&daemon).await;
    let mut wrong = session_hello(&root, "cli", "normal");
    wrong["hello"]["project_id"] = json!({"dev": 1, "ino": 1});
    wrong["hello"]["project_root"] = json!({"utf8": format!("/{CANARY}")});
    raw.send(&wrong).await;
    let _ = raw.until_closed().await;

    // A live session with failing and succeeding requests.
    let mut raw = Raw::session(&daemon, "mcp", "read_only").await;
    raw.request(1, "task.create", json!({"title": CANARY}))
        .await; // not_permitted
    raw.request(2, &format!("{CANARY}.op"), json!({})).await; // unknown op
    raw.request(3, "task.get", json!({"id": CANARY})).await; // not found
    raw.request(4, "task.get", json!({"id": CANARY, "extra": CANARY}))
        .await; // schema
    raw.send_text(&format!(
        r#"{{"id":5,"req":{{"op":"context.list","args":{{"{CANARY}":"{CANARY}"}}}}}}"#
    ))
    .await;
    let _ = raw.terminal(5).await;
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    owner
        .request(
            1,
            "task.create",
            json!({"title": CANARY, "description": CANARY}),
        )
        .await;
    owner
        .request(2, "context.set", json!({"key": CANARY, "value": CANARY}))
        .await;
    owner
        .request(
            3,
            "message.send",
            json!({"to": CANARY, "subject": CANARY, "body": CANARY}),
        )
        .await;
    // Protocol violations that close the session.
    owner
        .send_text(&format!(r#"{{"id":9,"req":{{"op":"{CANARY}"}}"#))
        .await;
    let mut dup = Raw::session(&daemon, "cli", "normal").await;
    dup.send(&json!({"id": 1, "req": {"op": "logs.follow", "args": {}}}))
        .await;
    dup.send_text(&format!(
        r#"{{"id":1,"req":{{"op":"task.get","args":{{"id":"{CANARY}"}}}}}}"#
    ))
    .await;
    let mut big = Raw::session(&daemon, "cli", "normal").await;
    big.send_text(&format!(
        r#"{{"id":1,"req":{{"op":"x"}},"pad":"{}{CANARY}"}}"#,
        "x".repeat(1024 * 1024)
    ))
    .await;
    let _ = big.until_closed().await;
    drop((raw, owner, dup));
    daemon.shutdown().await.unwrap();

    let lines = capture.0.lock().unwrap().clone();
    assert!(
        lines.len() >= 10,
        "the subscriber saw too little to prove anything ({} events)",
        lines.len()
    );
    let leaked: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("CANARY"))
        .collect();
    assert!(leaked.is_empty(), "request text in the logs: {leaked:#?}");
}
