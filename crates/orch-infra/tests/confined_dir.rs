//! R102: traversal, links and special entries never reach an outside canary.
use orch_infra::data_dir::confined::{ConfinedDir, ConfinedError, FileAccess};
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn a_name_that_is_not_one_component_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    for name in ["", ".", "..", "a/b", "/tmp", "a\0b"] {
        assert!(dir.create_file(name).is_err(), "{name:?}");
        assert!(dir.open_dir(name).is_err(), "{name:?}");
        assert!(dir.remove_file(name).is_err(), "{name:?}");
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn a_symlink_at_any_level_is_refused_and_not_followed() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let canary = outside.path().join("canary");
    std::fs::write(&canary, b"unchanged").unwrap();
    symlink(&canary, root.path().join("leaf")).unwrap();
    symlink(outside.path(), root.path().join("nested")).unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    for access in [FileAccess::Read, FileAccess::Untrusted, FileAccess::Modify] {
        assert!(dir.open_file("leaf", access).is_err());
    }
    assert!(matches!(
        dir.open_dir("nested"),
        Err(ConfinedError::Symlink(_))
    ));
    assert!(dir.ensure_dir("nested").is_err());
    assert!(dir.remove_file("leaf").is_err());
    assert_eq!(std::fs::read(canary).unwrap(), b"unchanged");
}

#[test]
fn a_hard_link_is_refused_for_append_and_in_untrusted_trees() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), b"canary").unwrap();
    std::fs::hard_link(outside.path(), root.path().join("linked")).unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    assert!(dir.open_file("linked", FileAccess::Modify).is_err());
    assert!(dir.open_file("linked", FileAccess::Untrusted).is_err());
    let mut text = String::new();
    dir.open_file("linked", FileAccess::Read)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "canary");
    assert_eq!(std::fs::read(outside.path()).unwrap(), b"canary");
}

#[test]
fn a_fifo_and_a_socket_are_refused_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    nix::unistd::mkfifo(&root.path().join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.path().join("socket")).unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(["fifo", "socket"].into_iter().all(|name| {
            [FileAccess::Read, FileAccess::Untrusted, FileAccess::Modify]
                .into_iter()
                .all(|a| dir.open_file(name, a).is_err())
        }))
        .unwrap();
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("special file open blocked")
    );
}

#[test]
fn an_entry_swapped_after_the_listing_is_judged_by_its_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), b"canary").unwrap();
    std::fs::write(root.path().join("entry"), b"original").unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    assert_eq!(dir.list().unwrap(), [std::ffi::OsString::from("entry")]);
    std::fs::remove_file(root.path().join("entry")).unwrap();
    symlink(outside.path(), root.path().join("entry")).unwrap();
    assert!(dir.open_file("entry", FileAccess::Untrusted).is_err());
    assert_eq!(std::fs::read(outside.path()).unwrap(), b"canary");
}

#[test]
fn rename_replaces_a_planted_link_and_never_follows_it() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), b"canary").unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    dir.create_file("tmp")
        .unwrap()
        .write_all(b"replacement")
        .unwrap();
    symlink(outside.path(), root.path().join("final")).unwrap();
    dir.rename("tmp", &dir, "final").unwrap();
    assert_eq!(
        std::fs::read(root.path().join("final")).unwrap(),
        b"replacement"
    );
    assert_eq!(std::fs::read(outside.path()).unwrap(), b"canary");
}

#[test]
fn rename_and_delete_stay_inside_the_root() {
    let root = tempfile::tempdir().unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    let sub = dir.ensure_dir("sub").unwrap();
    dir.create_file("item").unwrap().write_all(b"data").unwrap();
    dir.rename("item", &sub, "moved").unwrap();
    assert!(sub.rename("moved", &dir, "../escaped").is_err());
    sub.remove_file("moved").unwrap();
    assert!(sub.list().unwrap().is_empty());
    dir.remove_dir("sub").unwrap();
    assert!(dir.list().unwrap().is_empty());
}

#[test]
fn new_files_are_exclusive_and_0600() {
    // A permissive umask belongs to an isolated child, never the test runner.
    if std::env::var_os("CONFINED_UMASK_CHILD").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "new_files_are_exclusive_and_0600", "--nocapture"])
            .env_clear()
            .env("HOME", home.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                home.path().join("global.yml"),
            )
            .env("ORCH_DATA_HOME", home.path().join("daemons"))
            .env("CONFINED_UMASK_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    nix::sys::stat::umask(nix::sys::stat::Mode::empty());
    let root = tempfile::tempdir().unwrap();
    let dir = ConfinedDir::open(root.path()).unwrap();
    dir.create_file("private")
        .unwrap()
        .write_all(b"first")
        .unwrap();
    assert!(dir.create_file("private").is_err());
    assert_eq!(
        std::fs::read(root.path().join("private")).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::metadata(root.path().join("private"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    dir.ensure_dir("private-dir").unwrap();
    assert_eq!(
        std::fs::metadata(root.path().join("private-dir"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
