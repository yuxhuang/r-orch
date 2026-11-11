#![cfg(feature = "testing")]
//! Project selection and identity (T1; design D4, D12 `daemon_paths::*`; spec
//! R2, R33). The endpoint-length boundary, `TMPDIR` fallback and
//! `ORCH_SOCKET` rows need a real child with its own environment and are in
//! `orch-cli/tests/daemon_paths.rs`; this file covers what an in-process
//! daemon can prove about which project it serves.

mod support;

use orch_daemon::testing::DaemonHarness;
use orch_daemon::{DaemonError, select_project_root};
use orch_proto::{BINARY_VERSION, LOCAL_PROTOCOL};
use serde_json::json;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use support::{Raw, hello, ok, project_id};

/// An initialised project (`<root>/.orchestry`), canonical path.
fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(root.join(".orchestry")).unwrap();
    (dir, root)
}

// -- select_project_root: D4 rows 2 and 3 --------------------------------

/// `ORCH_PROJECT_ROOT` wins over the cwd, including a cwd inside a different
/// initialised project (an agent worktree routing back to its owner).
#[test]
fn an_explicit_root_overrides_the_cwd() {
    let (_a, owner) = project();
    let (_b, elsewhere) = project();
    let chosen = select_project_root(Some(&owner), &elsewhere).unwrap();
    assert_eq!(chosen, owner);
    // And from a cwd with no project at all.
    let nowhere = tempfile::tempdir().unwrap();
    assert_eq!(
        select_project_root(Some(&owner), nowhere.path()).unwrap(),
        owner
    );
}

/// An invalid explicit root is an error; it never silently falls back to the
/// cwd's project.
#[test]
fn an_invalid_explicit_root_never_falls_back_to_the_cwd() {
    let (_cwd_project, cwd) = project();
    let bare = tempfile::tempdir().unwrap();
    let missing = bare.path().join("does-not-exist");
    for explicit in [bare.path(), missing.as_path()] {
        let error = select_project_root(Some(explicit), &cwd).unwrap_err();
        assert!(
            matches!(error, DaemonError::NotInitialized),
            "{explicit:?}: {error:?}"
        );
        assert_eq!(error.exit_code(), 3);
    }
    for explicit in [Path::new(""), Path::new("relative/dir"), Path::new(".")] {
        let error = select_project_root(Some(explicit), &cwd).unwrap_err();
        assert!(
            matches!(error, DaemonError::InvalidArguments(_)),
            "{explicit:?}: {error:?}"
        );
        assert_eq!(error.exit_code(), 2);
    }
}

/// Without an override the nearest initialised ancestor wins, so two nested
/// checkouts stay distinct projects; no ancestor means not initialised.
#[test]
fn the_nearest_initialised_ancestor_wins_and_checkouts_stay_distinct() {
    let (_outer, outer) = project();
    let inner = outer.join("work/inner");
    std::fs::create_dir_all(inner.join(".orchestry")).unwrap();
    let deep = inner.join("a/b/c");
    std::fs::create_dir_all(&deep).unwrap();
    let canonical_inner = inner.canonicalize().unwrap();

    assert_eq!(select_project_root(None, &deep).unwrap(), canonical_inner);
    assert_eq!(
        select_project_root(None, &outer.join("work")).unwrap(),
        outer
    );
    assert_ne!(
        project_id(&outer),
        project_id(&canonical_inner),
        "two initialised checkouts must have distinct identities"
    );
    let bare = tempfile::tempdir().unwrap();
    let error = select_project_root(None, bare.path()).unwrap_err();
    assert!(matches!(error, DaemonError::NotInitialized));
}

/// A symlink alias to a project root selects the same canonical project and
/// the same identity.
#[test]
fn a_symlink_alias_to_the_root_selects_the_same_project() {
    let (_dir, root) = project();
    let links = tempfile::tempdir().unwrap();
    let alias = links.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    assert_eq!(select_project_root(Some(&alias), &alias).unwrap(), root);
    assert_eq!(select_project_root(None, &alias).unwrap(), root);
    assert_eq!(project_id(&alias), project_id(&root));
}

/// A non-UTF-8 root survives selection and the round trip through the
/// handshake. Skipped where the filesystem refuses non-UTF-8 names (APFS);
/// Linux CI exercises it for real. The wire encoding itself is covered
/// without a filesystem in `orch-proto/tests/daemon_protocol.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_utf8_root_is_selected_served_and_echoed_losslessly() {
    let base = tempfile::tempdir().unwrap();
    let odd = base
        .path()
        .canonicalize()
        .unwrap()
        .join(std::ffi::OsStr::from_bytes(b"proj-\xff\xfe"));
    if std::fs::create_dir(&odd).is_err() {
        eprintln!(
            "SKIPPED a_non_utf8_root_is_selected_served_and_echoed_losslessly: \
             this filesystem rejects non-UTF-8 names (expected on macOS APFS); \
             Linux CI and `cargo xtask linux` run it for real"
        );
        return;
    }
    std::fs::create_dir(odd.join(".orchestry")).unwrap();
    assert_eq!(select_project_root(Some(&odd), &odd).unwrap(), odd);

    let daemon = DaemonHarness::builder().start_in(&odd).await.unwrap();
    let mut raw = Raw::open(&daemon).await;
    let frame = hello(
        &odd,
        LOCAL_PROTOCOL,
        BINARY_VERSION,
        json!({"caller": {"kind": "cli"}, "mode": "normal"}),
    );
    // The helper sends the non-UTF-8 root in the tagged-bytes form.
    assert!(
        frame["hello"]["project_root"].get("hex").is_some(),
        "{frame}"
    );
    raw.send(&frame).await;
    let welcome = raw.recv().await.unwrap();
    assert_eq!(welcome["welcome"]["compatible"], true, "{welcome}");
    assert_eq!(
        welcome["welcome"]["project_root"],
        support::wire_path(&odd),
        "{welcome}"
    );
    daemon.shutdown().await.unwrap();
}

// -- wrong project --------------------------------------------------------

/// Identity is `(dev, ino)` of `.orchestry`, not the path: a hello naming the
/// right path with another project's identity is `wrong_project` (and tells
/// the client which root this daemon serves); a hello with a different path
/// string but the right identity is the same project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_project_is_decided_by_identity_not_by_path() {
    let daemon = DaemonHarness::start().await.unwrap();
    let served = daemon.project_root().to_path_buf();
    let (_other_dir, other) = project();

    // Right path, wrong identity.
    let mut raw = Raw::open(&daemon).await;
    let mut lie = hello(
        &served,
        LOCAL_PROTOCOL,
        BINARY_VERSION,
        json!({"caller": {"kind": "cli"}, "mode": "normal"}),
    );
    lie["hello"]["project_id"] = project_id(&other);
    raw.send(&lie).await;
    let reply = raw.recv().await.expect("a reject");
    assert_eq!(reply["reject"]["code"], "wrong_project", "{reply}");
    assert_eq!(
        reply["reject"]["project_root"]["utf8"],
        served.to_str().unwrap()
    );
    assert!(raw.until_closed().await.is_empty());

    // Wrong path, right identity: same project (an alias, a moved mount …).
    let mut raw = Raw::open(&daemon).await;
    let mut alias = hello(
        &other,
        LOCAL_PROTOCOL,
        BINARY_VERSION,
        json!({"caller": {"kind": "cli"}, "mode": "normal"}),
    );
    alias["hello"]["project_id"] = project_id(&served);
    raw.send(&alias).await;
    let welcome = raw.recv().await.unwrap();
    assert_eq!(welcome["welcome"]["compatible"], true, "{welcome}");
    ok(&raw.request(1, "daemon.status", json!({})).await);

    // A request-level project switch does not exist: the project is fixed at
    // hello and no op takes a root.
    let reply = raw
        .request(
            2,
            "daemon.status",
            json!({"project_root": other.to_str().unwrap()}),
        )
        .await;
    assert_eq!(reply["err"]["code"], "invalid_arguments", "{reply}");
    daemon.shutdown().await.unwrap();
}

/// A conflicting endpoint is never a license to serve some other project:
/// two daemons, two projects, and each answers only for its own identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_daemons_answer_only_for_their_own_project() {
    let a = DaemonHarness::start().await.unwrap();
    let b = DaemonHarness::start().await.unwrap();
    for (daemon, stranger) in [(&a, &b), (&b, &a)] {
        let mut raw = Raw::open(daemon).await;
        let mut frame = hello(
            stranger.project_root(),
            LOCAL_PROTOCOL,
            BINARY_VERSION,
            json!({"caller": {"kind": "cli"}, "mode": "normal"}),
        );
        frame["hello"]["project_id"] = project_id(stranger.project_root());
        raw.send(&frame).await;
        let reply = raw.recv().await.unwrap();
        assert_eq!(reply["reject"]["code"], "wrong_project", "{reply}");
        // A stop for the stranger project stops nothing here.
        let mut stop = Raw::open(daemon).await;
        let mut frame = hello(
            stranger.project_root(),
            LOCAL_PROTOCOL,
            BINARY_VERSION,
            json!({"intent": "stop"}),
        );
        frame["hello"]["project_id"] = project_id(stranger.project_root());
        stop.send(&frame).await;
        assert_eq!(
            stop.recv().await.unwrap()["reject"]["code"],
            "wrong_project"
        );
        let mut live = Raw::session(daemon, "cli", "normal").await;
        ok(&live.request(1, "daemon.status", json!({})).await);
    }
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

/// A `.orchestry` that is a symlink is not a project to serve: the identity
/// would be that of the link's target, chosen by whoever planted the link.
#[tokio::test]
async fn a_symlinked_state_dir_is_not_served() {
    let real = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    std::fs::create_dir(victim.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(
        victim.path().join("elsewhere"),
        real.path().join(".orchestry"),
    )
    .unwrap();
    let error = DaemonHarness::builder().start_in(real.path()).await.err();
    assert!(
        matches!(
            error,
            Some(DaemonError::InvalidArguments(_) | DaemonError::NotInitialized)
        ),
        "{error:?}"
    );
    assert!(
        std::fs::read_dir(victim.path().join("elsewhere"))
            .unwrap()
            .next()
            .is_none(),
        "a daemon wrote through the symlink"
    );
}
