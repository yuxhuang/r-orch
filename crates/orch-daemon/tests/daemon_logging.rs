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

    // Diagnostic errors and corrupt persisted payloads are reachable through
    // models.catalog and run log/history operations too.
    struct FailedDiagnostic;
    #[async_trait::async_trait]
    impl orch_infra::models::DiscoveryRuntime for FailedDiagnostic {
        async fn run_command(
            &self,
            _: &str,
            _: &[&str],
        ) -> orch_core::Result<orch_infra::models::CommandOutput> {
            Err(orch_core::Error::other(CANARY))
        }
        async fn read_text_file(&self, _: &std::path::Path) -> orch_core::Result<String> {
            Err(orch_core::Error::other(CANARY))
        }
        fn codex_home(&self) -> std::path::PathBuf {
            Default::default()
        }
    }
    orch_infra::models::ModelDiscovery::with_runtime(Arc::new(FailedDiagnostic))
        .discover(orch_domain::AdapterKind::Opencode)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let corrupt = temp.path().join("corrupt.jsonl");
    tokio::fs::write(&corrupt, CANARY).await.unwrap();
    let _: Vec<serde_json::Value> = orch_infra::storage::fs_utils::read_jsonl(&corrupt)
        .await
        .unwrap();
    let _: Vec<serde_json::Value> = orch_infra::storage::fs_utils::read_jsonl_tail(&corrupt, 10)
        .await
        .unwrap();
    use orch_core::storage::RunStore;
    let paths = orch_infra::storage::paths::Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.runs_dir()).await.unwrap();
    tokio::fs::write(paths.runs_dir().join("corrupt.jsonl"), CANARY)
        .await
        .unwrap();
    let store = orch_infra::storage::run::FileRunStore::new(paths);
    store.read_events("corrupt").await.unwrap();
    let mut stream = store.stream_events("corrupt", Default::default());
    assert!(
        std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
            .await
            .is_none()
    );

    // Oversize warnings must not publish a caller-controlled file name either.
    let paths = orch_infra::storage::paths::Paths::new(temp.path());
    let canary_path = paths.runs_dir().join(format!("{CANARY}.jsonl"));
    tokio::fs::File::create(&canary_path)
        .await
        .unwrap()
        .set_len(51 * 1024 * 1024)
        .await
        .unwrap();
    let _: Vec<serde_json::Value> = orch_infra::storage::fs_utils::read_jsonl(&canary_path)
        .await
        .unwrap();
    let store = orch_infra::storage::run::FileRunStore::new(paths);
    let mut stream = store.stream_events(CANARY, Default::default());
    assert!(
        std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
            .await
            .is_none()
    );

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
