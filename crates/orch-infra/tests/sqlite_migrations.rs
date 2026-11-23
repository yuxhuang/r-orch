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
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
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
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
    let binary = orch_infra::sqlite::migrations::SCHEMA_VERSION;
    conn.pragma_update(None, "user_version", (binary + 1) as i64)
        .unwrap();
    let err = migrate(&dir, &mut conn).unwrap_err().to_string();
    assert!(
        err.contains(&format!("database schema version {}", binary + 1)),
        "{err}"
    );
    assert!(
        err.contains(&format!("binary schema version {binary}")),
        "{err}"
    );
    assert!(open_connection(&dir, ConnectionRole::Reader, Synchronous::Normal).is_err());
}

#[test]
fn a_pending_migration_is_preceded_by_a_checked_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
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
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
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
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
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
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
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

#[test]
fn incomplete_copies_do_not_count_toward_retention_or_fail_committed_migrations() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
    project(&conn);
    let backups = dir.root().open_dir("backups").unwrap();
    let incomplete = backups.create_dir("pre-migration-v1-1").unwrap();
    incomplete.create_file("orch.db").unwrap();
    incomplete.create_file("orch.db-wal").unwrap();
    let steps = [INITIAL_SCHEMA, "SELECT 1", "SELECT 2", "SELECT 3"];
    for version in 2..=4 {
        apply_migrations(&dir, &mut conn, &steps[..version], plenty()).unwrap();
        assert_eq!(
            conn.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            version as u32
        );
    }
    let names = backups.list().unwrap();
    assert_eq!(names.len(), 3);
    assert!(
        names
            .iter()
            .any(|n| n.to_str().unwrap().starts_with("pre-migration-v2-"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.to_str().unwrap().starts_with("pre-migration-v3-"))
    );
    assert_eq!(
        incomplete.list().unwrap(),
        [
            std::ffi::OsString::from("orch.db"),
            std::ffi::OsString::from("orch.db-wal")
        ]
    );
}

#[test]
fn retention_failure_does_not_report_a_committed_migration_as_failed() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
    project(&conn);
    let steps = [INITIAL_SCHEMA, "SELECT 1", "SELECT 2", "SELECT 3"];
    apply_migrations(&dir, &mut conn, &steps[..2], plenty()).unwrap();
    let old = std::fs::read_dir(tmp.path().join("data/backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = apply_migrations(&dir, &mut conn, &steps, plenty());
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o700)).unwrap();
    // There are only two copies so far. Make a third to exercise pruning.
    result.unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = apply_migrations(
        &dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            "SELECT 1",
            "SELECT 2",
            "SELECT 3",
            "SELECT 4",
        ],
        plenty(),
    );
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o700)).unwrap();
    result.unwrap();
    assert_eq!(
        conn.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        5
    );
    assert!(old.join("orch.db").exists());
}

// Include SQLite sidecars and backup entries, so a refusal must leave both
// databases and both directory trees byte-for-byte unchanged.
fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(
        root: &std::path::Path,
        at: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, Vec<u8>)>,
    ) {
        let mut entries: Vec<_> = std::fs::read_dir(at)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                out.push((relative, Vec::new()));
                visit(root, &path, out);
            } else {
                out.push((relative, std::fs::read(path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn mismatched_migration_roots_refuse_before_copy_disk_floor_or_write() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let source_dir = data(&source);
    let destination_dir = data(&destination);
    let mut conn =
        open_connection(&source_dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&source_dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
    project(&conn);
    let mut other = open_connection(
        &destination_dir,
        ConnectionRole::Writer,
        Synchronous::Normal,
    )
    .unwrap();
    migrate(&destination_dir, &mut other).unwrap();
    project(&other);
    let source_before = snapshot(source.path());
    let destination_before = snapshot(destination.path());
    for space in [
        DiskSpace {
            available: 0,
            total: u64::MAX,
        },
        plenty(),
    ] {
        let error = apply_migrations(
            &destination_dir,
            &mut conn,
            &[
                INITIAL_SCHEMA,
                "INSERT INTO daemon_config VALUES('wrong_root','x',1)",
            ],
            space,
        )
        .unwrap_err();
        assert!(
            matches!(error, orch_infra::sqlite::SqliteError::Migration(_)),
            "{error}"
        );
        assert!(
            error.to_string().contains("different data directory"),
            "{error}"
        );
        assert_eq!(snapshot(source.path()), source_before);
        assert_eq!(snapshot(destination.path()), destination_before);
    }
}

#[test]
fn mismatched_startup_roots_refuse_before_initial_schema_write() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let source_dir = data(&source);
    let destination_dir = data(&destination);
    let mut conn =
        open_connection(&source_dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let source_before = snapshot(source.path());
    let destination_before = snapshot(destination.path());
    let error = migrate(&destination_dir, &mut conn).unwrap_err();
    assert!(
        error.to_string().contains("different data directory"),
        "{error}"
    );
    assert_eq!(snapshot(source.path()), source_before);
    assert_eq!(snapshot(destination.path()), destination_before);
}

#[test]
fn matching_migration_root_copies_and_updates_only_its_own_database() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let source_dir = data(&source);
    let _destination_dir = data(&destination);
    let mut conn =
        open_connection(&source_dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&source_dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
    project(&conn);
    let destination_before = snapshot(destination.path());
    apply_migrations(
        &source_dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            "INSERT INTO daemon_config VALUES('right_root','ok',1)",
        ],
        plenty(),
    )
    .unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM projects", [], |r| r.get(0))
            .unwrap(),
        1
    );
    let backups: Vec<_> = std::fs::read_dir(source.path().join("data/backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let copy = rusqlite::Connection::open_with_flags(
        backups[0].join("orch.db"),
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
            "SELECT count(*) FROM daemon_config WHERE key='right_root'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(snapshot(destination.path()), destination_before);
}

#[test]
fn attention_migration_preserves_legacy_rows_and_checked_version_one_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
    project(&conn);
    conn.execute("INSERT INTO attention(id,project_id,kind,subject_kind,subject_id,severity,title,opened_at,revision) VALUES('11','p','dispatch_off_with_work','dispatch','dispatch','warning','legacy',20,1)", []).unwrap();
    apply_migrations(
        &dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            orch_infra::sqlite::migrations::ATTENTION_FACTS,
        ],
        plenty(),
    )
    .unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row::<Option<String>, _, _>(
            "SELECT facts_json FROM attention WHERE id='11'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        None
    );
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT value FROM counters WHERE project_id='p' AND name='attention:id'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        11
    );
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .count(),
        1
    );
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn task_format_migrates_a_populated_task_free_predecessor_and_keeps_its_copy() {
    use orch_infra::sqlite::migrations::ATTENTION_FACTS;
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(
        &dir,
        &mut conn,
        &[INITIAL_SCHEMA, ATTENTION_FACTS],
        plenty(),
    )
    .unwrap();
    project(&conn);
    conn.execute("INSERT INTO goals(project_id,id,title,description,status,position,created_at,revision) VALUES('p','g','keep','text','active',1,0,1)", []).unwrap();
    let before: Vec<(i64, i64)> = conn
        .prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT title FROM goals WHERE id='g'", [], |r| r.get(0))
            .unwrap(),
        "keep"
    );
    let after: Vec<(i64, i64)> = conn
        .prepare(
            "SELECT version,applied_at FROM schema_migrations WHERE version<3 ORDER BY version",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(before, after);
    for table in ["task_details", "task_blob_refs"] {
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT strict FROM pragma_table_list WHERE name=?1",
                [table],
                |r| r.get(0)
            )
            .unwrap(),
            1
        );
    }
    let copies: Vec<_> = std::fs::read_dir(tmp.path().join("data/backups"))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(copies.len(), 1);
    let backup = rusqlite::Connection::open(copies[0].path().join("orch.db")).unwrap();
    assert_eq!(
        backup
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        2
    );
    assert_eq!(
        backup
            .query_row::<String, _, _>("SELECT title FROM goals WHERE id='g'", [], |r| r.get(0))
            .unwrap(),
        "keep"
    );
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .count(),
        1
    );
    // Required physical bounds, including the byte count rather than character count.
    conn.execute(
        "INSERT INTO task_details VALUES('p','t',1,'é',2,'hash')",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO task_details VALUES('p','x',1,'é',1,'hash')",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO task_blob_refs VALUES('p','t','',0,'hash',0)",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO task_blob_refs VALUES('p','t','',0,'hash',67108865)",
            []
        )
        .is_err()
    );
}

#[test]
fn every_ambiguous_task_predecessor_table_refuses_before_any_schema_change() {
    use orch_infra::sqlite::{SqliteError, migrations::ATTENTION_FACTS};
    let rows = [
        (
            "tasks",
            "INSERT INTO tasks(project_id,id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision) VALUES('p','private-id','private-title','private-body','todo',1,0,3,1,'owner',0,0,1)",
        ),
        (
            "task_dependencies",
            "INSERT INTO task_dependencies VALUES('p','private-id','private-dependency')",
        ),
        (
            "task_labels",
            "INSERT INTO task_labels VALUES('p','private-id','private-label')",
        ),
        (
            "task_scopes",
            "INSERT INTO task_scopes VALUES('p','private-id','private-pattern')",
        ),
        (
            "task_attachments",
            "INSERT INTO task_attachments VALUES('p','private-id','private-name','private-hash')",
        ),
    ];
    for (table, insert) in rows {
        let tmp = tempfile::tempdir().unwrap();
        let dir = data(&tmp);
        let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        apply_migrations(
            &dir,
            &mut conn,
            &[INITIAL_SCHEMA, ATTENTION_FACTS],
            plenty(),
        )
        .unwrap();
        project(&conn);
        conn.execute_batch(insert).unwrap();
        let before: Vec<(String, String)> = conn
            .prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let error = migrate(&dir, &mut conn).unwrap_err();
        assert!(matches!(error, SqliteError::Migration(_)), "{error}");
        let message = error.to_string();
        assert!(message.contains(&format!("{table}=1")), "{message}");
        assert!(!message.contains("private-"), "{message}");
        for name in [
            "tasks",
            "task_dependencies",
            "task_labels",
            "task_scopes",
            "task_attachments",
        ] {
            assert!(message.contains(&format!("{name}=")), "{message}");
        }
        assert_eq!(
            conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            1
        );
        let after: Vec<(String, String)> = conn
            .prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            conn.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap(),
            "ok"
        );
        let copies: Vec<_> = std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(copies.len(), 1);
        let backup = rusqlite::Connection::open(copies[0].path().join("orch.db")).unwrap();
        assert_eq!(
            backup
                .query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            1
        );
    }
}
