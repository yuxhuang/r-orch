//! Startup primitives for T9; the running daemon stays file-backed until T24.
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use std::os::unix::fs::PermissionsExt;

fn holder() -> Holder {
    Holder {
        kind: LockKind::Daemon,
        pid: std::process::id(),
        started_at: "2026-11-20T00:00:00Z".into(),
        binary_version: "synthetic".into(),
        executable: "/synthetic/orch".into(),
    }
}

#[test]
fn layout_and_modes_are_as_specified() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    let data = DataDir::acquire(&path, &holder()).unwrap();
    for relative in [
        "",
        "blobs",
        "blobs/sha256",
        "blobs/tmp",
        "repos",
        "spool",
        "spool/finish",
        "backups",
        "run",
    ] {
        assert_eq!(
            std::fs::metadata(path.join(relative))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "{relative}"
        );
    }
    assert_eq!(
        std::fs::metadata(path.join("orch.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(data);
    assert!(path.join("orch.lock").exists());
}

#[test]
fn a_directory_open_to_others_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(DataDir::acquire(&path, &holder()).is_err());
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn second_daemon_refuses_and_names_the_holder() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    let first = DataDir::acquire(&path, &holder()).unwrap();
    let before = std::fs::read(path.join("orch.lock")).unwrap();
    let err = DataDir::acquire(&path, &holder()).unwrap_err();
    assert_eq!(err.exit_code(), 4);
    let text = err.to_string();
    for value in [
        "daemon",
        &std::process::id().to_string(),
        "2026-11-20T00:00:00Z",
    ] {
        assert!(text.contains(value), "{text}");
    }
    assert_eq!(std::fs::read(path.join("orch.lock")).unwrap(), before);
    drop(first);
    DataDir::acquire(&path, &holder()).unwrap();
}

#[test]
fn network_filesystem_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    let err = DataDir::acquire_with_probe(&path, &holder(), |_| Ok(false)).unwrap_err();
    assert!(err.to_string().contains("network"));
    assert!(!path.join("orch.lock").exists());
}
