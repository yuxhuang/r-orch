#![cfg(feature = "testing")]
//! Permissions and credentials on a real socket (T1; design D11, D12
//! `daemon_security::uid_permissions_and_symlinks`; spec R7, R8, R20).
//!
//! Foreign-uid rejection cannot be exercised end to end in CI (there is no
//! second uid) and the harness has no peer-credential injection seam; it is
//! pinned at unit level by `session::tests::foreign_or_unavailable_peer_uid_is_closed_before_any_byte`
//! and checked on real hardware in the QA pass. Symlink/regular-file/live
//! listener protection at the socket path is a start-time rule exercised by
//! the binary tests in `orch-cli/tests/daemon_lifecycle.rs`; the run-directory
//! and umask rows are in `orch-cli/tests/daemon_security.rs`.

mod support;

use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use support::{Raw, ok};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_discovery_and_guard_files_are_private() {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let root = daemon.project_root().to_path_buf();
    let socket = daemon.socket().unwrap().to_path_buf();

    let meta = std::fs::symlink_metadata(&socket).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(mode(&socket), 0o600, "socket");
    assert_eq!(meta.uid(), nix::unistd::geteuid().as_raw());
    assert_eq!(mode(socket.parent().unwrap()), 0o700, "socket directory");
    assert_eq!(
        mode(&root.join(".orchestry/daemon.json")),
        0o600,
        "daemon.json"
    );
    assert_eq!(
        mode(&root.join(".orchestry/.daemon-owner.lock")),
        0o600,
        "owner guard"
    );
    assert!(socket.as_os_str().len() <= 100);

    // `.orchestry` itself keeps the mode its owner gave it (not chmodded).
    assert_eq!(
        mode(&root.join(".orchestry")),
        0o700,
        "harness created it 0700"
    );
    daemon.shutdown().await.unwrap();
}

/// The published record has exactly the reviewed fields: nothing from the
/// environment, no secrets, no task data. Adding a field makes this fail until
/// someone decides it is safe to publish.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_json_publishes_only_the_reviewed_fields() {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let body = std::fs::read(daemon.project_root().join(".orchestry/daemon.json")).unwrap();
    assert!(body.len() <= 16 * 1024);
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "binary",
            "executable",
            "instance",
            "pid",
            "project_id",
            "project_root",
            "protocol",
            "schema",
            "socket",
            "started_at"
        ]
    );
    assert_eq!(value["schema"], 1);
    assert_eq!(value["pid"], std::process::id());
    daemon.shutdown().await.unwrap();
}

/// Same uid is accepted on the real socket, and the whole session works
/// over it exactly as over the in-memory transport.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_uid_peer_is_accepted_on_the_real_socket() {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let reply = raw
        .request(1, "task.create", json!({"title": "over the socket"}))
        .await;
    assert_eq!(ok(&reply)["title"], "over the socket");
    let status = raw.request(2, "daemon.status", json!({})).await;
    assert_eq!(
        ok(&status)["socket"]["utf8"],
        daemon.socket().unwrap().to_str().unwrap()
    );
    daemon.shutdown().await.unwrap();
}

/// A socket that nobody accepted from yet and a client that sends nothing do
/// not consume the daemon: the 2 s handshake deadline closes the connection
/// with a reject, on the real transport as well.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_client_on_the_real_socket_is_closed_by_the_handshake_deadline() {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let mut idle = Raw::open(&daemon).await;
    let rest = idle.until_closed().await;
    assert!(
        rest.iter()
            .all(|f| f["reject"]["code"] == "handshake_failed"),
        "{rest:?}"
    );
    let mut live = Raw::session(&daemon, "cli", "normal").await;
    ok(&live.request(1, "daemon.status", json!({})).await);
    daemon.shutdown().await.unwrap();
}

/// Shutdown removes only what it created: an unrelated file in `.orchestry`
/// survives (the scratch socket directory is harness-owned and deleted with
/// it, so only the project side can be observed afterwards).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_removes_only_its_own_files() {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let socket = daemon.socket().unwrap().to_path_buf();
    let root = daemon.project_root().to_path_buf();
    let note = root.join(".orchestry/notes.txt");
    std::fs::write(&note, b"mine").unwrap();
    let _project = daemon.quiesce().await.unwrap();
    assert!(!socket.exists());
    assert!(!root.join(".orchestry/daemon.json").exists());
    assert_eq!(std::fs::read(&note).unwrap(), b"mine");
}
