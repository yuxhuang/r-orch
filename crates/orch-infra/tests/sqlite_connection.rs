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

#[test]
fn a_guarded_transaction_reserves_the_writer_before_its_first_statement() {
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let contender = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    contender.busy_timeout(std::time::Duration::ZERO).unwrap();
    let transaction = writer.transaction().unwrap();
    let error = contender.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    assert!(DataDir::acquire(&temp.path().join("data"), &holder()).is_err());
    transaction.rollback().unwrap();
    contender
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .unwrap();
}

#[test]
fn opening_connections_preserves_cross_process_writer_locks() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    if let Some(path) = std::env::var_os("SQLITE_LOCK_CONTENDER_DB") {
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .unwrap();
        conn.busy_timeout(Duration::ZERO).unwrap();
        let result = conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK");
        if std::env::var_os("SQLITE_LOCK_EXPECT_BUSY").is_some() {
            assert_eq!(
                result.unwrap_err().sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
        } else {
            result.unwrap();
        }
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    let contend = |busy: bool| {
        let home = tempfile::tempdir().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "opening_connections_preserves_cross_process_writer_locks",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", home.path())
            .env("TMPDIR", home.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                home.path().join("global.yml"),
            )
            .env("ORCH_DATA_HOME", home.path().join("daemons"))
            .env("SQLITE_LOCK_CONTENDER_DB", data.database_path())
            .stdout(Stdio::null());
        if busy {
            command.env("SQLITE_LOCK_EXPECT_BUSY", "1");
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "external contender failed (expected busy={busy}): {status}"
                );
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("external contender timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let transaction = writer.transaction().unwrap();
    contend(true);
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    contend(true);
    let checkpoint =
        open_connection(&data, ConnectionRole::Checkpoint, Synchronous::Normal).unwrap();
    contend(true);
    drop(reader);
    drop(checkpoint);
    contend(true);
    transaction.rollback().unwrap();
    contend(false);
}

#[test]
fn every_sqlite_filename_refuses_links_and_special_files() {
    use std::os::unix::fs::symlink;
    for name in ["orch.db", "orch.db-wal", "orch.db-shm"] {
        for kind in ["symlink", "hardlink", "fifo", "directory"] {
            let temp = tempfile::tempdir().unwrap();
            let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
            drop(open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap());
            let path = temp.path().join("data").join(name);
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            let canary = temp.path().join("canary");
            std::fs::write(&canary, b"unchanged").unwrap();
            match kind {
                "symlink" => symlink(&canary, &path).unwrap(),
                "hardlink" => std::fs::hard_link(&canary, &path).unwrap(),
                "fifo" => nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap(),
                "directory" => std::fs::create_dir(&path).unwrap(),
                _ => unreachable!(),
            }
            assert!(
                open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).is_err(),
                "{name}: {kind}"
            );
            assert_eq!(std::fs::read(canary).unwrap(), b"unchanged");
        }
    }
}
