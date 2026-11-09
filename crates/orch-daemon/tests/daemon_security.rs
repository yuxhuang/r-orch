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
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
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
    keys.sort();
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

/// R2 / spec line 216: an explicit start keeps the daemon's runtime files out
/// of git in a project initialized before the daemon existed. The legacy
/// five-entry `.orchestry/.gitignore` gains each runtime entry exactly once,
/// its own lines and mode untouched; a second start appends nothing; a
/// symlinked `.gitignore` is not followed and start still reaches READY.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_ignores_runtime_metadata() {
    const LEGACY: &str = "# Runtime state\nstate.json\n*.lock\n\n# Logs and runs\nruns/\nlogs/\n\n# Agent workspaces\nworkspaces/\n";
    const RUNTIME: [&str; 4] = ["run/", "daemon.json", "daemon.log", "daemon.log.1"];
    let project = |name: &str| {
        let temp = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(temp.path().join(".orchestry"))
            .unwrap();
        temp
    };

    let legacy = project("orch-gi-");
    let gitignore = legacy.path().join(".orchestry/.gitignore");
    std::fs::write(&gitignore, LEGACY).unwrap();
    std::fs::set_permissions(&gitignore, std::fs::Permissions::from_mode(0o644)).unwrap();

    let daemon = DaemonHarness::builder()
        .on_socket()
        .start_in(legacy.path())
        .await
        .unwrap();
    let after = std::fs::read_to_string(&gitignore).unwrap();
    daemon.shutdown().await.unwrap();
    assert!(after.starts_with(LEGACY), "{after}");
    for entry in RUNTIME {
        assert_eq!(
            after.lines().filter(|line| *line == entry).count(),
            1,
            "{entry} in {after}"
        );
    }
    assert_eq!(mode(&gitignore), 0o644, "mode untouched");

    let daemon = DaemonHarness::builder()
        .on_socket()
        .start_in(legacy.path())
        .await
        .unwrap();
    daemon.shutdown().await.unwrap();
    assert_eq!(std::fs::read_to_string(&gitignore).unwrap(), after);

    let linked = project("orch-gl-");
    let target = linked.path().join("tracked.gitignore");
    std::fs::write(&target, LEGACY).unwrap();
    let link = linked.path().join(".orchestry/.gitignore");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let daemon = DaemonHarness::builder()
        .on_socket()
        .start_in(linked.path())
        .await
        .expect("a hygiene failure never blocks READY");
    assert!(
        linked.path().join(".orchestry/daemon.json").exists(),
        "published"
    );
    daemon.shutdown().await.unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), LEGACY);
}
