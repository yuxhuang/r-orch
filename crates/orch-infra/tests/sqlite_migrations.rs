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
fn the_task_format_migration_keeps_earlier_versions_and_applies_to_a_populated_predecessor_without_rows()
 {
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
    conn.execute(
        "INSERT INTO import_manifest VALUES('p','goal','g','keep',0)",
        [],
    )
    .unwrap();
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
        orch_infra::sqlite::migrations::SCHEMA_VERSION as i64
    );
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT source_sha256 FROM import_manifest WHERE entity_id='g'",
            [],
            |r| r.get(0)
        )
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
            .query_row::<String, _, _>(
                "SELECT source_sha256 FROM import_manifest WHERE entity_id='g'",
                [],
                |r| r.get(0)
            )
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
fn a_predecessor_with_any_task_or_membership_row_is_refused_and_left_unchanged() {
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
    for version in [1, 2] {
        for (table, insert) in rows {
            let tmp = tempfile::tempdir().unwrap();
            let dir = data(&tmp);
            let mut conn =
                open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
            apply_migrations(
                &dir,
                &mut conn,
                &[INITIAL_SCHEMA, ATTENTION_FACTS][..version],
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
            for _ in 0..3 {
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
                    version as i64
                );
                assert_eq!(
                    conn.query_row::<i64, _, _>(
                        "SELECT count(*) FROM schema_migrations",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                    version as i64
                );
                assert_eq!(
                    conn.query_row::<i64, _, _>(
                        &format!("SELECT count(*) FROM {table}"),
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                    1
                );
                let after: Vec<(String, String)> = conn
                    .prepare(
                        "SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name",
                    )
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
            }
            assert_eq!(
                std::fs::read_dir(tmp.path().join("data/backups"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}

#[test]
fn agent_team_run_registry_reaches_four_and_repeats_without_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let four = [
        INITIAL_SCHEMA,
        orch_infra::sqlite::migrations::ATTENTION_FACTS,
        orch_infra::sqlite::migrations::TASK_FORMAT,
        orch_infra::sqlite::migrations::AGENT_TEAM_RUN,
    ];
    apply_migrations(&dir, &mut conn, &four, plenty()).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        4
    );
    let history: Vec<(i64, i64)> = conn
        .prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    apply_migrations(&dir, &mut conn, &four, plenty()).unwrap();
    let after: Vec<(i64, i64)> = conn
        .prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(history, after);
    assert_eq!(
        history.iter().map(|r| r.0).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
}

#[test]
fn settings_registry_reaches_five_and_repeats_without_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &five_steps(), plenty()).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        5
    );
    let history: Vec<(i64, i64)> = conn
        .prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    apply_migrations(&dir, &mut conn, &five_steps(), plenty()).unwrap();
    let after: Vec<(i64, i64)> = conn
        .prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(history, after);
    assert_eq!(
        history.iter().map(|r| r.0).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
}

#[test]
fn agent_team_run_refuses_each_populated_predecessor_before_any_change() {
    use orch_infra::sqlite::migrations::{ATTENTION_FACTS, TASK_FORMAT};
    let fixtures = [
        (
            "agents",
            "INSERT INTO agents(project_id,id,name,adapter,config,position,created_at,updated_at,revision) VALUES('p','private-id','private-name','codex','{}',1,0,0,1)",
        ),
        (
            "teams",
            "INSERT INTO teams VALUES('p','private-id','private-name',NULL,'active','a','{}',0,0,1)",
        ),
        (
            "team_members",
            "INSERT INTO team_members VALUES('p','private-id','a','member',0)",
        ),
        (
            "team_tasks",
            "INSERT INTO team_tasks VALUES('p','private-id','t')",
        ),
        (
            "runs",
            "INSERT INTO runs(project_id,id,task_id,agent_id,run_no,counted,attempt_no,epoch,status,started_at,revision) VALUES('p','private-id','t','a',1,1,1,1,'running',0,1)",
        ),
    ];
    for version in [1, 3] {
        for (table, insert) in fixtures {
            let tmp = tempfile::tempdir().unwrap();
            let dir = data(&tmp);
            let mut conn =
                open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
            apply_migrations(
                &dir,
                &mut conn,
                &[INITIAL_SCHEMA, ATTENTION_FACTS, TASK_FORMAT][..version],
                plenty(),
            )
            .unwrap();
            project(&conn);
            conn.execute(insert, []).unwrap();
            let before: String = conn
                .query_row(
                    "SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema ORDER BY name)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            for _ in 0..2 {
                let error = migrate(&dir, &mut conn).unwrap_err().to_string();
                assert!(error.contains(&format!("{table}=1")), "{error}");
                assert!(!error.contains("private-"), "{error}");
                assert_eq!(
                    conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                        .unwrap(),
                    version as i64
                );
                assert_eq!(
                    conn.query_row::<i64, _, _>(
                        "SELECT count(*) FROM schema_migrations",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                    version as i64
                );
                assert_eq!(
                    conn.query_row::<i64, _, _>(
                        &format!("SELECT count(*) FROM {table}"),
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                    1
                );
                assert_eq!(conn.query_row::<String, _, _>("SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema ORDER BY name)", [], |r| r.get(0)).unwrap(), before);
                assert_eq!(
                    std::fs::read_dir(tmp.path().join("data/backups"))
                        .unwrap()
                        .count(),
                    0
                );
            }
        }
    }
}

#[test]
fn agent_team_run_transactional_hook_refuses_rows_created_after_preflight() {
    use orch_infra::sqlite::migrations::{AGENT_TEAM_RUN, ATTENTION_FACTS, TASK_FORMAT};
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    // Version zero has no preflight tables. This simulates a trusted intermediate
    // migration creating a predecessor row: only the 004 hook can catch it.
    let predecessor = format!(
        "{INITIAL_SCHEMA}; INSERT INTO team_tasks VALUES('p','private-team','private-task');"
    );
    let error = apply_migrations(
        &dir,
        &mut conn,
        &[&predecessor, ATTENTION_FACTS, TASK_FORMAT, AGENT_TEAM_RUN],
        plenty(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("team_tasks=1"), "{error}");
    assert!(!error.contains("private-"));
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM team_tasks", [], |r| r.get(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT count(*) FROM sqlite_schema WHERE name='entity_details'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn agent_team_run_upgrade_preserves_task_authority_and_other_families() {
    use orch_infra::sqlite::migrations::{ATTENTION_FACTS, TASK_FORMAT};
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(
        &dir,
        &mut conn,
        &[INITIAL_SCHEMA, ATTENTION_FACTS, TASK_FORMAT],
        plenty(),
    )
    .unwrap();
    project(&conn);
    conn.execute(
        "INSERT INTO task_details VALUES('p','t',1,'{}',2,?1)",
        ["0".repeat(64)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_blob_refs VALUES('p','t','/description',0,?1,65537)",
        ["1".repeat(64)],
    )
    .unwrap();
    conn.execute("INSERT INTO counters VALUES('p','position:task',7)", [])
        .unwrap();
    let sql: String = conn.query_row("SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema WHERE name IN ('task_details','task_blob_refs','task_blob_refs_by_hash') ORDER BY name)", [], |r| r.get(0)).unwrap();
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(conn.query_row::<String, _, _>("SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema WHERE name IN ('task_details','task_blob_refs','task_blob_refs_by_hash') ORDER BY name)", [], |r| r.get(0)).unwrap(), sql);
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT detail FROM task_details", [], |r| r.get(0))
            .unwrap(),
        "{}"
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT size FROM task_blob_refs", [], |r| r.get(0))
            .unwrap(),
        65537
    );
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT value FROM counters WHERE name='position:task'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        7
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM projects", [], |r| r.get(0))
            .unwrap(),
        1
    );
    let backups: Vec<_> = std::fs::read_dir(tmp.path().join("data/backups"))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(backups.len(), 1);
    let copy = rusqlite::Connection::open_with_flags(
        backups[0].path().join("orch.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        copy.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        3
    );
    assert_eq!(
        copy.query_row::<String, _, _>("SELECT detail FROM task_details", [], |r| r.get(0))
            .unwrap(),
        "{}"
    );
}

fn four_steps() -> [&'static str; 4] {
    use orch_infra::sqlite::migrations::{AGENT_TEAM_RUN, ATTENTION_FACTS, TASK_FORMAT};
    [INITIAL_SCHEMA, ATTENTION_FACTS, TASK_FORMAT, AGENT_TEAM_RUN]
}

fn columns(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn the_context_config_template_goal_migration_keeps_earlier_versions_and_applies_to_a_predecessor_without_rows_in_these_tables_and_keeps_unrelated_rows()
 {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &four_steps(), plenty()).unwrap();
    project(&conn);
    conn.execute(
        "INSERT INTO import_manifest VALUES('p','goal','g','keep',0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entity_details VALUES('p','agent',0,'a',1,'{}',2,?1)",
        ["0".repeat(64)],
    )
    .unwrap();
    conn.execute("INSERT INTO counters VALUES('p','position:agent',7)", [])
        .unwrap();
    let before: Vec<(i64, i64)> = conn
        .prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let earlier = |conn: &rusqlite::Connection| -> String {
        conn.query_row("SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema WHERE name NOT IN ('context_entries','project_config','templates','goals') ORDER BY name)", [], |r| r.get(0)).unwrap()
    };
    let sql = earlier(&conn);
    apply_migrations(&dir, &mut conn, &five_steps(), plenty()).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        5
    );
    assert_eq!(earlier(&conn), sql, "no earlier table or index is touched");
    let after: Vec<(i64, i64)> = conn
        .prepare(
            "SELECT version,applied_at FROM schema_migrations WHERE version<5 ORDER BY version",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(before, after);
    for (table, new) in [
        (
            "context_entries",
            ["ttl_ms", "created_at", "position", "set_by_identity"],
        ),
        (
            "project_config",
            ["position", "key_form", "value_sha256", "value_bytes"],
        ),
        (
            "templates",
            ["position", "name_form", "body_sha256", "body_bytes"],
        ),
        (
            "goals",
            [
                "position",
                "created_at_text",
                "updated_at_text_form",
                "assignee_agent_id_bytes",
            ],
        ),
    ] {
        let present = columns(&conn, table);
        for column in new {
            assert!(present.iter().any(|c| c == column), "{table}.{column}");
        }
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
    // Later-phase and legacy columns of the rebuilt tables are kept.
    for column in [
        "integration_branch",
        "integration_head",
        "published_at",
        "published_commit",
        "published_by",
    ] {
        assert!(columns(&conn, "goals").iter().any(|c| c == column));
    }
    for column in [
        "set_by_kind",
        "set_by_agent_id",
        "set_by_run_id",
        "expires_at",
    ] {
        assert!(
            columns(&conn, "context_entries")
                .iter()
                .any(|c| c == column)
        );
    }
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT source_sha256 FROM import_manifest", [], |r| r
            .get(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT value FROM counters WHERE name='position:agent'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        7
    );
    let copies: Vec<_> = std::fs::read_dir(tmp.path().join("data/backups"))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(copies.len(), 1);
    let copy = rusqlite::Connection::open_with_flags(
        copies[0].path().join("orch.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        copy.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        4
    );
    assert_eq!(
        copy.query_row::<String, _, _>("SELECT detail FROM entity_details", [], |r| r.get(0))
            .unwrap(),
        "{}"
    );
    apply_migrations(&dir, &mut conn, &five_steps(), plenty()).unwrap();
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_predecessor_with_any_context_config_template_or_goal_row_is_refused_and_left_readable() {
    let fixtures = [
        (
            "context_entries",
            "INSERT INTO context_entries(project_id,key,value,set_by_kind,updated_at,revision) VALUES('p','private-key','private-value','owner',0,1)",
        ),
        (
            "project_config",
            "INSERT INTO project_config VALUES('p','private-key','private-value',1)",
        ),
        (
            "templates",
            "INSERT INTO templates VALUES('p','private-name','private-body',1)",
        ),
        (
            "goals",
            "INSERT INTO goals(project_id,id,title,description,status,position,created_at,revision) VALUES('p','private-id','private-title','private-text','active',1,0,1)",
        ),
    ];
    for version in [1, 4] {
        for (table, insert) in fixtures {
            let tmp = tempfile::tempdir().unwrap();
            let dir = data(&tmp);
            let mut conn =
                open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
            apply_migrations(&dir, &mut conn, &four_steps()[..version], plenty()).unwrap();
            project(&conn);
            conn.execute(insert, []).unwrap();
            let schema = |conn: &rusqlite::Connection| -> String {
                conn.query_row(
                    "SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema ORDER BY name)",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
            };
            let before = schema(&conn);
            for _ in 0..2 {
                let error = migrate(&dir, &mut conn).unwrap_err().to_string();
                assert!(error.contains(&format!("{table}=1")), "{error}");
                assert!(!error.contains("private-"), "{error}");
                assert_eq!(
                    conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                        .unwrap(),
                    version as i64
                );
                assert_eq!(
                    conn.query_row::<i64, _, _>(
                        "SELECT count(*) FROM schema_migrations",
                        [],
                        |r| { r.get(0) }
                    )
                    .unwrap(),
                    version as i64
                );
                assert_eq!(
                    conn.query_row::<i64, _, _>(
                        &format!("SELECT count(*) FROM {table}"),
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                    1
                );
                assert_eq!(schema(&conn), before);
                assert_eq!(
                    std::fs::read_dir(tmp.path().join("data/backups"))
                        .unwrap()
                        .count(),
                    0
                );
            }
        }
    }
}

#[test]
fn context_config_template_goal_transactional_hook_refuses_rows_created_after_preflight() {
    use orch_infra::sqlite::migrations::CONTEXT_CONFIG_TEMPLATE_GOAL;
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    // Version zero has no preflight tables. This simulates a trusted intermediate
    // migration creating a predecessor row: only the 005 hook can catch it.
    let predecessor = format!(
        "{INITIAL_SCHEMA}; INSERT INTO templates VALUES('p','private-name','private-body',1);"
    );
    let [_, attention, task, entities] = four_steps();
    let error = apply_migrations(
        &dir,
        &mut conn,
        &[
            &predecessor,
            attention,
            task,
            entities,
            CONTEXT_CONFIG_TEMPLATE_GOAL,
        ],
        plenty(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("templates=1"), "{error}");
    assert!(!error.contains("private-"));
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        4
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM templates", [], |r| r.get(0))
            .unwrap(),
        1
    );
    assert!(
        !columns(&conn, "templates").iter().any(|c| c == "position"),
        "the predecessor table is not rebuilt"
    );
}

fn five_steps() -> [&'static str; 5] {
    use orch_infra::sqlite::migrations::CONTEXT_CONFIG_TEMPLATE_GOAL;
    let [initial, attention, task, entities] = four_steps();
    [
        initial,
        attention,
        task,
        entities,
        CONTEXT_CONFIG_TEMPLATE_GOAL,
    ]
}

#[test]
fn message_registry_reaches_six_and_repeats_without_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(orch_infra::sqlite::migrations::SCHEMA_VERSION, 6);
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        6
    );
    let history = |conn: &rusqlite::Connection| -> Vec<(i64, i64)> {
        conn.prepare("SELECT version,applied_at FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let before = history(&conn);
    assert_eq!(
        before.iter().map(|r| r.0).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(history(&conn), before);
    assert!(columns(&conn, "messages").contains(&"revision".into()));
}

#[test]
fn message_preflight_refuses_every_predecessor_before_copy_or_intermediate_changes() {
    use orch_infra::sqlite::{Class, Role, SqliteError, classify};
    for version in 1..=5 {
        let tmp = tempfile::tempdir().unwrap();
        let dir = data(&tmp);
        let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        apply_migrations(&dir, &mut conn, &five_steps()[..version], plenty()).unwrap();
        project(&conn);
        conn.execute("INSERT INTO messages VALUES('p','private-message','direct','private-sender',NULL,NULL,NULL,'private-subject','private-body','pending',1,0,NULL,NULL)", []).unwrap();
        let state = |conn: &rusqlite::Connection| -> (String, i64, i64, String) {
            conn.query_row("SELECT (SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema ORDER BY name)), (SELECT user_version FROM pragma_user_version), (SELECT count(*) FROM schema_migrations), (SELECT body FROM messages)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
        };
        let before = state(&conn);
        let files = snapshot(tmp.path());
        for _ in 0..2 {
            // Preflight takes precedence even over an insufficient copy budget.
            let mut steps = five_steps().to_vec();
            steps.push(orch_infra::sqlite::migrations::MESSAGE_FORMAT);
            let error = apply_migrations(
                &dir,
                &mut conn,
                &steps,
                DiskSpace {
                    available: 0,
                    total: 1,
                },
            )
            .unwrap_err();
            assert!(matches!(error, SqliteError::Migration(_)));
            assert!(
                error.to_string().ends_with(
                    "message entity format v1 refuses ambiguous predecessor rows: messages=1"
                ),
                "{error}"
            );
            assert!(!error.to_string().contains("private-"));
            assert_eq!(
                classify(&error).leaves(),
                &[(Role::Operation, Class::Ordinary)]
            );
            assert_eq!(state(&conn), before);
            assert_eq!(snapshot(tmp.path()), files);
        }
        drop(conn);
        let conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        assert_eq!(state(&conn), before);
        assert_eq!(
            std::fs::read_dir(tmp.path().join("data/backups"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn message_transactional_hook_refuses_late_rows_and_rolls_back_its_whole_step() {
    use orch_infra::sqlite::migrations::MESSAGE_FORMAT;
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let predecessor = format!(
        "{INITIAL_SCHEMA}; INSERT INTO messages VALUES('p','private-message','direct','private-sender',NULL,NULL,NULL,'private-subject','private-body','pending',1,0,NULL,NULL);"
    );
    let mut steps = five_steps().to_vec();
    steps[0] = &predecessor;
    steps.push(MESSAGE_FORMAT);
    let error = apply_migrations(&dir, &mut conn, &steps, plenty()).unwrap_err();
    assert!(error.to_string().contains("messages=1"), "{error}");
    drop(conn);
    let conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let state: (i64, i64, String, i64) = conn.query_row("SELECT (SELECT user_version FROM pragma_user_version), (SELECT count(*) FROM schema_migrations), (SELECT body FROM messages), (SELECT count(*) FROM sqlite_schema WHERE name IN ('entity_details_message','entity_blob_refs_message'))", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(state, (5, 5, "private-body".into(), 0));
    assert!(!columns(&conn, "messages").contains(&"revision".into()));
}

#[test]
fn message_upgrade_copies_all_sibling_authority_and_references_and_keeps_checked_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(&dir, &mut conn, &five_steps(), plenty()).unwrap();
    project(&conn);
    conn.execute_batch("
        INSERT INTO agents(project_id,id,name,adapter,config,position,revision) VALUES('p','a','name','codex','{}',1,7);
        INSERT INTO teams(project_id,id,name,lead_agent_id,created_at_text,status,config,position,revision) VALUES('p','team','name','a','opaque','active','{}',2,8);
        INSERT INTO team_members(project_id,team_id,ordinal,agent_id,role,joined_at_text) VALUES('p','team',0,'a','member','opaque');
        INSERT INTO team_tasks(project_id,team_id,task_id) VALUES('p','team','task');
        INSERT INTO runs(project_id,id,task_id,agent_id,status,revision,position,attempt) VALUES('p','r','task','a','running',9,3,10);
        INSERT INTO context_entries(project_id,key,value,set_by_kind,position,revision) VALUES('p','context','value','owner',4,10);
        INSERT INTO project_config(project_id,key,value,position,revision) VALUES('p','config','value',5,11);
        INSERT INTO templates(project_id,name,body,position,revision) VALUES('p','template','body',6,12);
        INSERT INTO goals(project_id,id,title,description,status,position,revision) VALUES('p','goal','title','description','active',7,13);
    ").unwrap();
    let objects = orch_infra::blob::DiskBlobStore::open(dir.root()).unwrap();
    let bytes = vec![b'x'; 65537];
    let object = objects.put(&bytes).unwrap();
    let families = [
        "agent", "team", "run", "context", "config", "template", "goal",
    ];
    // More than two full batches; both key forms, NUL bytes and repeated hashes.
    for family in families {
        for i in 0..75 {
            let form = i % 2;
            let key = if form == 0 {
                format!("key-{i}\0é")
            } else {
                format!("{i:064x}")
            };
            conn.execute(
                "INSERT INTO entity_details VALUES('p',?1,?2,?3,1,?4,?5,?6)",
                rusqlite::params![family, form, key, "{\"text\":\"é\"}", 13, "0".repeat(64)],
            )
            .unwrap();
            for ordinal in 0..2 {
                conn.execute(
                    "INSERT INTO entity_blob_refs VALUES('p',?1,?2,?3,'/body',?4,?5,65537)",
                    rusqlite::params![family, form, key, ordinal, object.to_string()],
                )
                .unwrap();
            }
        }
        conn.execute(
            "INSERT INTO counters VALUES('p',?1,999)",
            [format!("position:{family}")],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO counters VALUES('p','position:message',4321)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO blobs VALUES('p',?1,65537,'disk',0,NULL)",
        [object.to_string()],
    )
    .unwrap();
    let rows = |conn: &rusqlite::Connection, table: &str| -> Vec<Vec<rusqlite::types::Value>> {
        let n = conn
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap()
            .column_count();
        let order = (1..=n).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        stmt.query_map([], |r| (0..n).map(|i| r.get(i)).collect())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let tables = [
        "entity_details",
        "entity_blob_refs",
        "counters",
        "blobs",
        "agents",
        "teams",
        "team_members",
        "team_tasks",
        "runs",
        "context_entries",
        "project_config",
        "templates",
        "goals",
        "schema_migrations",
    ];
    let before: Vec<_> = tables.iter().map(|t| rows(&conn, t)).collect();
    let untouched = |conn: &rusqlite::Connection| -> String {
        conn.query_row("SELECT group_concat(sql) FROM (SELECT sql FROM sqlite_schema WHERE name NOT IN ('entity_details','entity_blob_refs','entity_blob_refs_by_hash','messages','messages_pending') AND name NOT LIKE 'sqlite_autoindex_%' ORDER BY name)", [], |r|r.get(0)).unwrap()
    };
    let schema = untouched(&conn);
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(objects.get(&object).unwrap(), bytes);
    assert_eq!(untouched(&conn), schema);
    for (i, table) in tables[..tables.len() - 1].iter().enumerate() {
        assert_eq!(rows(&conn, table), before[i], "{table}");
    }
    assert_eq!(
        &rows(&conn, "schema_migrations")[..5],
        before[tables.len() - 1]
    );
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
    let copy = rusqlite::Connection::open_with_flags(
        copies[0].path().join("orch.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        copy.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        5
    );
    for (i, table) in tables.iter().enumerate() {
        assert_eq!(rows(&copy, table), before[i], "copy {table}");
    }
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/backups"))
            .unwrap()
            .count(),
        1
    );
}
