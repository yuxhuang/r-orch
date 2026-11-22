use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{ConnectionRole, Synchronous, migrations::migrate, open_connection};
use std::collections::BTreeSet;

const TABLES: &[&str] = &[
    "agents",
    "attention",
    "blobs",
    "context_entries",
    "counters",
    "daemon_config",
    "goals",
    "identities",
    "identity_tokens",
    "import_manifest",
    "intents",
    "leases",
    "messages",
    "project_config",
    "project_sources",
    "projects",
    "request_keys",
    "results",
    "retry_queue",
    "run_event_gaps",
    "run_events",
    "runs",
    "schema_migrations",
    "task_attachments",
    "task_dependencies",
    "task_epochs",
    "task_labels",
    "task_scopes",
    "tasks",
    "team_members",
    "team_tasks",
    "teams",
    "templates",
];

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

#[test]
fn new_database_matches_the_schema_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    let mut stmt = conn.prepare("SELECT name, strict FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        TABLES
    );
    assert!(rows.iter().all(|r| r.1 == 1));
    drop(stmt);
    let names: Vec<String> = conn
        .prepare("SELECT id FROM identities ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(names, ["local_owner", "local_worker"]);
    let lineage: String = conn
        .query_row(
            "SELECT value FROM daemon_config WHERE key='lineage'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lineage.len(), 32);
    assert!(
        lineage
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    migrate(&dir, &mut conn).unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT value FROM daemon_config WHERE key='lineage'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        lineage
    );
}

#[test]
fn every_project_table_carries_project_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    for table in TABLES.iter().filter(|t| {
        ![
            "daemon_config",
            "identities",
            "identity_tokens",
            "schema_migrations",
        ]
        .contains(t)
    }) {
        let count: i64 = conn.query_row("SELECT count(*) FROM pragma_table_info(?1) WHERE name='project_id' AND \"notnull\"=1", [table], |r|r.get(0)).unwrap();
        assert_eq!(count, 1, "{table}");
        let in_key: i64 = conn
            .query_row(
                "SELECT pk FROM pragma_table_info(?1) WHERE name='project_id'",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert!(in_key > 0, "{table}");
    }
}

#[test]
fn kept_columns_are_present() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    for (table, columns) in [
        (
            "runs",
            "model adapter_version worker_id epoch run_no attempt_no counted branch base_sha head_sha head_verified_at push_window_until events_pruned_at events_pruned_count tokens_input tokens_output tokens_reasoning tokens_cache_read tokens_cache_write local",
        ),
        (
            "tasks",
            "accepted_by accepted_at accepted_run_id landing_state landed_commit landed_into landed_at landing_attempts landing_conflict_paths attention_reason attention_since created_by_kind created_by_run_id outputs legacy_proof position",
        ),
        (
            "goals",
            "integration_branch integration_head published_at published_commit published_by position",
        ),
        (
            "leases",
            "worker_id epoch lineage state offered_at branch base_sha fetch_ref",
        ),
    ] {
        let found: BTreeSet<String> = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .unwrap()
            .query_map([table], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for column in columns.split_whitespace() {
            assert!(found.contains(column), "{table}.{column}");
        }
    }
}

fn insert_task(
    conn: &rusqlite::Connection,
    project: &str,
    id: &str,
    status: &str,
    landing: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute("INSERT INTO tasks(project_id,id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision,landing_state) VALUES(?1,?2,'title','body',?3,1,0,3,1,'owner',0,0,1,?4)", rusqlite::params![project,id,status,landing])
}

#[test]
fn done_without_landing_state_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    assert!(insert_task(&conn, "p", "a", "done", None).is_err());
    insert_task(&conn, "p", "a", "done", Some("skipped")).unwrap();
    assert!(insert_task(&conn, "p", "b", "todo", Some("landed")).is_err());
}

#[test]
fn task_ids_are_unique_under_ascii_case_only_within_a_project() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    insert_task(&conn, "p", "tsk_Case", "todo", None).unwrap();
    assert!(insert_task(&conn, "p", "tsk_case", "todo", None).is_err());
    insert_task(&conn, "q", "tsk_case", "todo", None).unwrap();
    insert_task(&conn, "p", "tsk_cas3", "todo", None).unwrap();
    insert_task(&conn, "p", "é", "todo", None).unwrap();
    insert_task(&conn, "p", "É", "todo", None).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM tasks WHERE id='tsk_case'", [], |r| r
            .get(0))
            .unwrap(),
        1
    );
}

#[test]
fn project_keys_preserve_order_and_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    for (table, expected) in [
        ("projects", vec!["project_id", "id"]),
        ("blobs", vec!["project_id", "sha256"]),
        ("intents", vec!["project_id", "id"]),
        ("attention", vec!["project_id", "id"]),
    ] {
        let key: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")
            .unwrap()
            .query_map([table], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(key, expected, "{table}");
    }
    assert!(
        conn.execute(
            "INSERT INTO projects VALUES('p','q','bad','active',0,1)",
            []
        )
        .is_err()
    );
    conn.execute(
        "INSERT INTO projects VALUES('p','p','one','active',0,1)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO projects VALUES('q','q','one','active',0,1)",
            []
        )
        .is_err()
    );
    for scope in ["p", "q"] {
        conn.execute("INSERT INTO blobs(project_id,sha256,size,storage_location,created_at) VALUES(?1,'digest',0,'local',0)", [scope]).unwrap();
    }
    assert!(
        conn.execute(
            "INSERT INTO blobs VALUES('p','digest',0,'local',0,NULL)",
            []
        )
        .is_err()
    );
    conn.execute(
        "INSERT INTO intents(id,kind,payload,created_at) VALUES(1,'daemon','{}',0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intents(project_id,id,kind,payload,created_at) VALUES('p',1,'project','{}',0)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO intents(project_id,kind,payload,created_at) VALUES('p','project','{}',0)",
            []
        )
        .is_err()
    );
    assert!(conn.execute("INSERT INTO intents(project_id,id,kind,payload,created_at) VALUES(NULL,2,'daemon','{}',0)", []).is_err());
    conn.execute(
        "UPDATE intents SET done_at=1 WHERE project_id='p' AND id=1",
        [],
    )
    .unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT count(*) FROM intents WHERE project_id='' AND id=1 AND done_at IS NULL",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn attention_uniqueness_ends_when_an_item_is_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    let insert = |scope: &str, id: &str| {
        conn.execute("INSERT INTO attention(project_id,id,kind,subject_kind,subject_id,severity,title,opened_at,revision) VALUES(?1,?2,'disk_floor','daemon','subject','blocking','title',0,1)", [scope,id])
    };
    insert("", "same").unwrap();
    insert("p", "same").unwrap();
    assert!(insert("p", "other").is_err());
    conn.execute(
        "UPDATE attention SET resolved_at=1 WHERE project_id='p' AND id='same'",
        [],
    )
    .unwrap();
    insert("p", "other").unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT author_kind FROM attention WHERE project_id='p' AND id='other'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        "daemon"
    );
    let indexes: Vec<String> = conn.prepare("SELECT name FROM sqlite_schema WHERE type='index' AND name IN ('intents_pending','blobs_sweepable','attention_one_per_subject','attention_open','attention_history') ORDER BY name").unwrap()
        .query_map([], |r|r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(
        indexes,
        [
            "attention_history",
            "attention_one_per_subject",
            "attention_open",
            "blobs_sweepable",
            "intents_pending"
        ]
    );
}
