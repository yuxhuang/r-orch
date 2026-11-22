use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{ConnectionRole, Synchronous, open_connection};

fn holder() -> Holder {
    Holder {
        kind: LockKind::Daemon,
        pid: std::process::id(),
        started_at: "synthetic".into(),
        binary_version: "test".into(),
        executable: "/synthetic/orch".into(),
    }
}

#[test]
fn connections_have_the_required_settings() {
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    for role in [
        ConnectionRole::Writer,
        ConnectionRole::Checkpoint,
        ConnectionRole::Reader,
    ] {
        let conn = open_connection(&data, role, Synchronous::Normal).unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        for (pragma, expected) in [
            ("synchronous", 1),
            ("foreign_keys", 1),
            ("busy_timeout", 5000),
        ] {
            let value: i64 = conn.pragma_query_value(None, pragma, |r| r.get(0)).unwrap();
            assert_eq!(value, expected, "{role:?}: {pragma}");
        }
    }
}

#[test]
fn synchronous_full_is_a_setting() {
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Full).unwrap();
    let sync: i64 = conn
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    assert_eq!(sync, 2);
}

#[test]
fn every_writing_connection_reads_back_its_sync_settings() {
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    for role in [ConnectionRole::Writer, ConnectionRole::Checkpoint] {
        let conn = open_connection(&data, role, Synchronous::Normal).unwrap();
        for (pragma, expected) in [
            ("fullfsync", 1),
            ("checkpoint_fullfsync", 1),
            ("wal_autocheckpoint", 0),
        ] {
            let actual: i64 = conn.pragma_query_value(None, pragma, |r| r.get(0)).unwrap();
            assert_eq!(actual, expected, "{role:?}: {pragma}");
        }
    }
}

#[test]
fn read_connections_cannot_write() {
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT;")
        .unwrap();
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert!(reader.execute("INSERT INTO sample VALUES(1)", []).is_err());
}

#[test]
fn sqlite_files_are_private_and_links_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    let data = DataDir::acquire(&path, &holder()).unwrap();
    let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch("CREATE TABLE sample(id INTEGER) STRICT; INSERT INTO sample VALUES(1)")
        .unwrap();
    for name in ["orch.db", "orch.db-wal", "orch.db-shm"] {
        assert_eq!(
            std::fs::metadata(path.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(conn);
    let canary = temp.path().join("canary");
    std::fs::write(&canary, b"canary").unwrap();
    std::fs::remove_file(path.join("orch.db")).unwrap();
    symlink(&canary, path.join("orch.db")).unwrap();
    assert!(open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).is_err());
    assert_eq!(std::fs::read(canary).unwrap(), b"canary");
}
