use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    ConnectionRole, Synchronous,
    migrations::{DiskSpace, INITIAL_SCHEMA, apply_migrations, migrate},
    open_connection,
};

fn data(tmp: &tempfile::TempDir) -> DataDir {
    DataDir::acquire(
        &tmp.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap()
}
fn plenty() -> DiskSpace {
    DiskSpace {
        available: 100 * 1024 * 1024 * 1024,
        total: 200 * 1024 * 1024 * 1024,
    }
}
fn project(conn: &rusqlite::Connection) {
    conn.execute("INSERT INTO projects(project_id,id,name,status,created_at,revision) VALUES('p','p','synthetic','active',0,1)", []).unwrap();
}

#[test]
fn migrations_apply_in_order_and_are_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let steps = [
        INITIAL_SCHEMA,
        "INSERT INTO daemon_config VALUES('second','ok',1)",
        "UPDATE daemon_config SET value='third' WHERE key='second'",
    ];
    apply_migrations(&dir, &mut conn, &steps, plenty()).unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT value FROM daemon_config WHERE key='second'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        "third"
    );
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(versions, [1, 2, 3]);
    apply_migrations(&dir, &mut conn, &steps, plenty()).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap(),
        3
    );
}

#[test]
fn failed_migration_leaves_the_earlier_version() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    let steps = [
        INITIAL_SCHEMA,
        "INSERT INTO daemon_config VALUES('rolled_back','x',1); THIS IS INVALID SQL;",
    ];
    let error = apply_migrations(&dir, &mut conn, &steps, plenty()).unwrap_err();
    assert!(error.to_string().contains("THIS IS INVALID SQL"), "{error}");
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT count(*) FROM daemon_config WHERE key='rolled_back'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

#[test]
fn newer_schema_is_refused_with_both_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    let err = migrate(&dir, &mut conn).unwrap_err().to_string();
    assert!(err.contains("database schema version 2"), "{err}");
    assert!(err.contains("binary schema version 1"), "{err}");
    assert!(open_connection(&dir, ConnectionRole::Reader, Synchronous::Normal).is_err());
}

#[test]
fn a_pending_migration_is_preceded_by_a_checked_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    project(&conn);
    apply_migrations(
        &dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            "INSERT INTO daemon_config VALUES('after','migration',1)",
        ],
        plenty(),
    )
    .unwrap();
    let backup = std::fs::read_dir(tmp.path().join("data/backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["kind"], "database_only");
    assert_eq!(manifest["schema_version"], 1);
    let copy = rusqlite::Connection::open_with_flags(
        backup.join("orch.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        copy.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        copy.query_row::<i64, _, _>("SELECT count(*) FROM projects", [], |r| r.get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        copy.query_row::<i64, _, _>(
            "SELECT count(*) FROM daemon_config WHERE key='after'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        0
    );
    let bytes = std::fs::read(backup.join("orch.db")).unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(
        manifest["files"][0]["sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
}

#[test]
fn the_start_is_refused_without_space_for_the_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    project(&conn);
    let err = apply_migrations(
        &dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            "INSERT INTO daemon_config VALUES('after','migration',1)",
        ],
        DiskSpace {
            available: 5 * 1024 * 1024 * 1024,
            total: 10 * 1024 * 1024 * 1024,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("free space"));
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_copy_that_fails_its_check_stops_before_the_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    project(&conn);
    conn.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO tasks(project_id,id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision) VALUES('p','bad','x','x','done',1,0,3,1,'owner',0,0,1); PRAGMA ignore_check_constraints=OFF;").unwrap();
    let err = apply_migrations(
        &dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            "INSERT INTO daemon_config VALUES('after','migration',1)",
        ],
        plenty(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("backup verification"), "{err}");
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

#[test]
fn only_the_two_newest_copies_are_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    project(&conn);
    let steps = [INITIAL_SCHEMA, "SELECT 1", "SELECT 2", "SELECT 3"];
    for version in 2..=4 {
        apply_migrations(&dir, &mut conn, &steps[..version], plenty()).unwrap();
    }
    let mut names: Vec<_> = std::fs::read_dir(tmp.path().join("data/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2);
    assert!(names[0].starts_with("pre-migration-v2-"));
    assert!(names[1].starts_with("pre-migration-v3-"));
}
