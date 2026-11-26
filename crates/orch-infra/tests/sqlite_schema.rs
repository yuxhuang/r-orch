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
    "entity_blob_refs",
    "entity_details",
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
    "task_blob_refs",
    "task_dependencies",
    "task_details",
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
        use orch_core::store::attention::{AttentionFacts, DiskSubject};
        let (facts, kind, subject_kind, severity) = if scope.is_empty() {
            (
                AttentionFacts::DiskFloor {
                    subject: DiskSubject::DataDirectory,
                    free_bytes: 1,
                    floor_bytes: 10,
                },
                "disk_floor",
                "data_directory",
                "blocking",
            )
        } else {
            (
                AttentionFacts::DispatchOffWithWork {
                    waiting_tasks: 1,
                    oldest_waiting_at: 0,
                },
                "dispatch_off_with_work",
                "dispatch",
                "warning",
            )
        };
        conn.execute("INSERT INTO attention(project_id,id,kind,subject_kind,subject_id,severity,title,opened_at,revision,facts_json) VALUES(?1,?2,?3,?4,?5,?6,?7,0,1,?8)", rusqlite::params![scope,id,kind,subject_kind,facts.subject_id(),severity,facts.title(),serde_json::to_string(&facts).unwrap()])
    };
    insert("", "1").unwrap();
    insert("p", "1").unwrap();
    assert!(insert("p", "2").is_err());
    conn.execute(
        "UPDATE attention SET resolved_at=1,resolved_by='daemon',resolved_reason='dispatch_enabled' WHERE project_id='p' AND id='1'",
        [],
    )
    .unwrap();
    insert("p", "2").unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT author_kind FROM attention WHERE project_id='p' AND id='2'",
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

#[test]
fn entity_key_forms_and_reference_bounds_are_disjoint_for_all_seven_families() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    let hash = "a".repeat(64);
    for family in [
        "agent", "team", "run", "context", "config", "template", "goal",
    ] {
        for form in [0, 1] {
            conn.execute(
                "INSERT INTO entity_details VALUES('p',?1,?2,?3,1,'{}',2,?3)",
                rusqlite::params![family, form, hash],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entity_blob_refs VALUES('p',?1,?2,?3,'/value',0,?3,65537)",
                rusqlite::params![family, form, hash],
            )
            .unwrap();
        }
        let zero = conn.execute(
            "INSERT INTO entity_blob_refs VALUES('p',?1,0,'zero','/prompt',0,?2,0)",
            rusqlite::params![family, hash],
        );
        assert_eq!(zero.is_ok(), family == "run");
    }
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM entity_details", [], |r| r.get(0))
            .unwrap(),
        14
    );
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM entity_blob_refs", [], |r| r.get(0))
            .unwrap(),
        15
    );
    for (form, key) in [
        (2, "a"),
        (1, "wrong"),
        (1, &"A".repeat(64)),
        (0, &"a".repeat(65537)),
    ] {
        assert!(
            conn.execute(
                "INSERT INTO entity_details VALUES('p','agent',?1,?2,1,'{}',2,?3)",
                rusqlite::params![form, key, hash]
            )
            .is_err()
        );
    }
    for size in [-1, 67108865] {
        assert!(
            conn.execute(
                "INSERT INTO entity_blob_refs VALUES('p','run',0,'r','/prompt',0,?1,?2)",
                rusqlite::params![hash, size]
            )
            .is_err()
        );
    }
    assert!(
        conn.execute(
            "INSERT INTO entity_details VALUES('p','unknown',0,'r',1,'{}',2,?1)",
            [&hash]
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO entity_details VALUES('p','run',0,'r',1,NULL,65536,?1)",
            [&hash]
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO entity_details VALUES('p','run',0,'r',1,'é',1,?1)",
            [&hash]
        )
        .is_err()
    );
}

#[test]
fn native_entity_forms_duplicate_names_and_member_occurrences() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    for id in ["a", "b"] {
        conn.execute("INSERT INTO agents(project_id,id,name,adapter,config,position,revision) VALUES('p',?1,'same','codex','{}',1,1)", [id]).unwrap();
        conn.execute("INSERT INTO teams(project_id,id,name,lead_agent_id,created_at_text,status,config,position,revision) VALUES('p',?1,'same','a','opaque','active','{}',1,1)", [id]).unwrap();
    }
    for ordinal in [0, 1] {
        conn.execute("INSERT INTO team_members(project_id,team_id,ordinal,agent_id,role,joined_at_text) VALUES('p','a',?1,'a','member','opaque')", [ordinal]).unwrap();
    }
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM team_members", [], |r| r.get(0))
            .unwrap(),
        2
    );
    assert!(conn.execute("INSERT INTO team_members(project_id,team_id,ordinal,agent_id,role,joined_at_text) VALUES('p','a',1,'b','member','opaque')", []).is_err());
    conn.execute(
        "INSERT INTO team_tasks(project_id,team_id,task_id) VALUES('p','a','t')",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO team_tasks(project_id,team_id,task_id) VALUES('p','a','t')",
            []
        )
        .is_err()
    );
    let oversized = "é".repeat(32769);
    assert!(
        conn.execute("UPDATE agents SET name=?1 WHERE id='a'", [&oversized])
            .is_err()
    );
    let hash = "b".repeat(64);
    conn.execute(
        "UPDATE agents SET name='',name_form=1,name_sha256=?1,name_bytes=65538 WHERE id='a'",
        [&hash],
    )
    .unwrap();
    assert!(
        conn.execute("UPDATE agents SET name_bytes=65536 WHERE id='a'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE agents SET name_sha256=NULL WHERE id='a'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE agents SET autonomous=2 WHERE id='a'", [])
            .is_err()
    );
    assert_eq!(
        conn.query_row::<(Option<i64>, Option<i64>, Option<i64>), _, _>(
            "SELECT autonomous,created_at,updated_at FROM agents WHERE id='a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        )
        .unwrap(),
        (None, None, None)
    );
}

#[test]
fn run_claim_tuple_payload_attempt_and_identity_constraints_are_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    for (id, attempt) in [("r2", 2), ("r10b", 10), ("r10a", 10)] {
        conn.execute("INSERT INTO runs(project_id,id,task_id,agent_id,status,attempt,revision) VALUES('p',?1,'t','a','running',?2,1)", rusqlite::params![id,attempt]).unwrap();
    }
    let order: Vec<String> = conn
        .prepare("SELECT id FROM runs WHERE task_id='t' ORDER BY attempt DESC,id COLLATE BINARY")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(order, ["r10a", "r10b", "r2"]);
    for invalid in [
        "run_no=1",
        "epoch=1",
        "counted=0",
        "attempt_no=1",
        "run_no=1,epoch=1,counted=2",
        "run_no=1,epoch=1,counted=1",
        "run_no=1,epoch=1,counted=0,attempt_no=1",
    ] {
        assert!(
            conn.execute(&format!("UPDATE runs SET {invalid} WHERE id='r2'"), [])
                .is_err(),
            "{invalid}"
        );
    }
    conn.execute(
        "UPDATE runs SET run_no=1,epoch=1,counted=1,attempt_no=1 WHERE id='r2'",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE runs SET run_no=1,epoch=2,counted=0 WHERE id='r10a'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE runs SET run_no=2,epoch=1,counted=0 WHERE id='r10a'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE runs SET run_no=2,epoch=2,counted=1,attempt_no=1 WHERE id='r10a'",
            []
        )
        .is_err()
    );
    conn.execute(
        "UPDATE runs SET run_no=2,epoch=2,counted=0 WHERE id='r10a'",
        [],
    )
    .unwrap();
    for attempt in [-1i64, 4294967296] {
        assert!(
            conn.execute("UPDATE runs SET attempt=?1 WHERE id='r2'", [attempt])
                .is_err()
        );
    }
    conn.execute("UPDATE runs SET attempt=4294967295 WHERE id='r2'", [])
        .unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM counters", [], |r| r.get(0))
            .unwrap(),
        0
    );
}

#[test]
fn native_identity_and_claim_keys_cannot_alias_literal_digest_text() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    let hash = "b".repeat(64);
    conn.execute("INSERT INTO agents(project_id,id,name,adapter,config,position,revision) VALUES('p',?1,'literal','codex','{}',1,1)", [&hash]).unwrap();
    conn.execute("INSERT INTO agents(project_id,id,id_form,id_sha256,id_bytes,name,adapter,config,position,revision) VALUES('p',?1,1,?1,65537,'indirect','codex','{}',2,1)", [&hash]).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM agents WHERE id=?1", [&hash], |r| r
            .get(0))
            .unwrap(),
        2
    );
    conn.execute("INSERT INTO runs(project_id,id,task_id,agent_id,status,run_no,epoch,counted,attempt_no,revision) VALUES('p','literal',?1,'a','running',1,1,1,1,1)", [&hash]).unwrap();
    conn.execute("INSERT INTO runs(project_id,id,task_id,task_id_form,task_id_sha256,task_id_bytes,agent_id,status,run_no,epoch,counted,attempt_no,revision) VALUES('p','indirect',?1,1,?1,65537,'a','running',1,1,1,1,1)", [&hash]).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM runs", [], |r| r.get(0))
            .unwrap(),
        2
    );
}

// Apply the shipped registry SQL with the product's SQLite reader. Each case
// uses a savepoint so a successful probe cannot change the next case's inputs.
fn native_contract_fixture() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for sql in [
        include_str!("../src/sqlite/schema.sql"),
        include_str!("../src/sqlite/migrations/002_attention_facts.sql"),
        include_str!("../src/sqlite/migrations/003_task_format.sql"),
        include_str!("../src/sqlite/migrations/004_agent_team_run.sql"),
    ] {
        conn.execute_batch(sql).unwrap();
    }
    conn.execute_batch("
        INSERT INTO agents(project_id,id,name,adapter,config,position,revision) VALUES('p','a','name','codex','{}',1,1);
        INSERT INTO teams(project_id,id,name,lead_agent_id,created_at_text,status,config,position,revision) VALUES('p','t','name','a','opaque','active','{}',1,1);
        INSERT INTO team_members(project_id,team_id,ordinal,agent_id,role,joined_at_text) VALUES('p','t',0,'a','member','opaque');
        INSERT INTO team_tasks(project_id,team_id,task_id) VALUES('p','t','task');
        INSERT INTO runs(project_id,id,task_id,agent_id,status,revision) VALUES('p','r','task','a','running',1);
    ").unwrap();
    conn
}

const NATIVE_TEXTS: &[(&str, &str, bool)] = &[
    ("agents", "id", true),
    ("agents", "name", false),
    ("agents", "adapter", false),
    ("agents", "role", false),
    ("agents", "current_task", false),
    ("agents", "last_error", false),
    ("teams", "id", true),
    ("teams", "name", false),
    ("teams", "description", false),
    ("teams", "lead_agent_id", false),
    ("teams", "created_at_text", false),
    ("team_members", "team_id", true),
    ("team_members", "role", false),
    ("team_members", "joined_at_text", false),
    ("team_tasks", "team_id", true),
    ("runs", "id", true),
    ("runs", "task_id", true),
    ("runs", "agent_id", true),
    ("runs", "started_at_text", false),
    ("runs", "error", false),
];

fn probe_update(
    conn: &rusqlite::Connection,
    sql: &str,
    values: &[&dyn rusqlite::ToSql],
    valid: bool,
) {
    conn.execute_batch("SAVEPOINT probe").unwrap();
    let result = conn.execute(sql, values);
    assert_eq!(result.is_ok(), valid, "{sql}: {result:?}");
    if valid {
        assert_eq!(result.unwrap(), 1, "probe must target one fixture row");
    } else {
        assert_eq!(
            result.unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
    }
    conn.execute_batch("ROLLBACK TO probe; RELEASE probe")
        .unwrap();
}

#[test]
fn every_native_text_form_enforces_its_complete_projection_contract() {
    let conn = native_contract_fixture();
    let hash = "a".repeat(64);
    let max = "é".repeat(32768);
    let over = format!("{max}x");
    for &(table, col, identity) in NATIVE_TEXTS {
        probe_update(
            &conn,
            &format!("UPDATE {table} SET {col}=?1"),
            &[&max],
            true,
        );
        probe_update(
            &conn,
            &format!("UPDATE {table} SET {col}=?1"),
            &[&over],
            false,
        );
        for sibling in [format!("{col}_sha256=?1"), format!("{col}_bytes=65537")] {
            let params: Vec<&dyn rusqlite::ToSql> = if sibling.contains("?1") {
                vec![&hash]
            } else {
                vec![]
            };
            probe_update(
                &conn,
                &format!("UPDATE {table} SET {sibling}"),
                &params,
                false,
            );
        }
        let native = if identity { hash.as_str() } else { "" };
        let prefix =
            format!("UPDATE {table} SET {col}_form=1,{col}=?1,{col}_sha256=?2,{col}_bytes=");
        probe_update(&conn, &format!("{prefix}65537"), &[&native, &hash], true);
        probe_update(&conn, &format!("{prefix}65536"), &[&native, &hash], false);
        probe_update(&conn, &format!("{prefix}NULL"), &[&native, &hash], false);
        probe_update(
            &conn,
            &format!("{prefix}65537"),
            &[&"mismatch", &hash],
            false,
        );
        probe_update(
            &conn,
            &format!("{prefix}65537"),
            &[&native, &Option::<String>::None],
            false,
        );
        probe_update(
            &conn,
            &format!("{prefix}65537"),
            &[&Option::<String>::None, &hash],
            false,
        );
        probe_update(
            &conn,
            &format!("UPDATE {table} SET {col}_form=2"),
            &[],
            false,
        );
    }
}

#[test]
fn every_digest_check_rejects_bound_nul_and_nonhex_bytes() {
    let conn = native_contract_fixture();
    let hash = "a".repeat(64);
    let bad = [
        format!("{hash}\0"),
        format!("{hash}\0/../../canary"),
        format!("aa\0{}", "a".repeat(61)),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "é".repeat(64),
    ];
    for digest in std::iter::once(&hash).chain(bad.iter()) {
        let valid = digest == &hash;
        for &(table, col, identity) in NATIVE_TEXTS {
            let native = if identity { digest.as_str() } else { "" };
            probe_update(
                &conn,
                &format!(
                    "UPDATE {table} SET {col}=?1,{col}_form=1,{col}_sha256=?2,{col}_bytes=65537"
                ),
                &[&native, digest],
                valid,
            );
        }
        // Four shared-pair digest expressions: key in each table, detail and ref hash.
        for (sql, params) in [
            (
                "INSERT INTO entity_details VALUES('p','run',1,?1,1,'{}',2,?2)",
                rusqlite::params![digest, hash],
            ),
            (
                "INSERT INTO entity_details VALUES('p','run',0,'r',1,'{}',2,?1)",
                rusqlite::params![digest],
            ),
            (
                "INSERT INTO entity_blob_refs VALUES('p','run',1,?1,'/prompt',0,?2,1)",
                rusqlite::params![digest, hash],
            ),
            (
                "INSERT INTO entity_blob_refs VALUES('p','run',0,'r','/prompt',0,?1,1)",
                rusqlite::params![digest],
            ),
        ] {
            probe_update(&conn, sql, params, valid);
        }
    }
}

#[test]
fn all_native_identity_forms_keep_literal_digest_text_distinct() {
    let conn = native_contract_fixture();
    let hash = "b".repeat(64);
    for (table, key) in [
        ("agents", "id"),
        ("teams", "id"),
        ("runs", "id"),
        ("team_members", "team_id"),
        ("team_tasks", "team_id"),
    ] {
        conn.execute(&format!("UPDATE {table} SET {key}=?1"), [&hash])
            .unwrap();
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let projection = columns
            .iter()
            .map(|c| {
                if c == &format!("{key}_form") {
                    "1".into()
                } else if c == &format!("{key}_sha256") {
                    "?1".into()
                } else if c == &format!("{key}_bytes") {
                    "65537".into()
                } else {
                    c.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("INSERT INTO {table} SELECT {projection} FROM {table} WHERE {key}_form=0");
        assert_eq!(conn.execute(&sql, [&hash]).unwrap(), 1, "{table}");
        assert!(
            conn.execute(&sql, [&hash]).is_err(),
            "duplicate indirect {table}"
        );
        assert!(
            conn.execute(
                &format!("INSERT INTO {table} SELECT * FROM {table} WHERE {key}_form=0"),
                []
            )
            .is_err(),
            "duplicate literal {table}"
        );
        assert_eq!(
            conn.query_row::<i64, _, _>(
                &format!("SELECT count(*) FROM {table} WHERE {key}=?1"),
                [&hash],
                |r| r.get(0)
            )
            .unwrap(),
            2
        );
    }
}

#[test]
fn native_auxiliary_bounds_config_and_run_order_index_are_pinned() {
    let conn = native_contract_fixture();
    let over = "a".repeat(65537);
    for sql in [
        "UPDATE team_members SET agent_id=?1",
        "UPDATE team_tasks SET task_id=?1",
    ] {
        probe_update(&conn, sql, &[&over], false);
    }
    probe_update(&conn, "UPDATE agents SET config='[]'", &[], false);
    probe_update(&conn, "UPDATE teams SET position=-1", &[], false);
    let index: Vec<(String, i64)> = conn.prepare("SELECT name,desc FROM pragma_index_xinfo('runs_by_task_attempt') WHERE key=1 ORDER BY seqno").unwrap()
        .query_map([], |r| Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(
        index,
        [
            ("project_id".into(), 0),
            ("task_id_form".into(), 0),
            ("task_id".into(), 0),
            ("attempt".into(), 1),
            ("id_form".into(), 0),
            ("id".into(), 0)
        ]
    );
}

// Migration 005's four rebuilt tables: each key and text is an explicit
// inline or digest form, and the port-owned counters are bounded.
#[test]
fn settings_tables_enforce_key_forms_text_forms_provenance_and_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    let hash = "a".repeat(64);
    let over = "x".repeat(65537);
    // (table, identity column, text column, columns and values of the other required members)
    let tables = [
        (
            "context_entries",
            "key",
            "value",
            "set_by_kind,position,revision",
            "'owner',1,1",
        ),
        ("project_config", "key", "value", "position,revision", "1,1"),
        ("templates", "name", "body", "position,revision", "1,1"),
        (
            "goals",
            "id",
            "title",
            "description,created_at_text,status,position,revision",
            "'',''  ,'active',1,1",
        ),
    ];
    for (table, id, text, other, values) in tables {
        let insert = |id_value: &str,
                      form: i64,
                      sha: Option<&str>,
                      bytes: Option<i64>,
                      v: &str,
                      vform: i64| {
            conn.execute(
                &format!(
                    "INSERT INTO {table}(project_id,{id},{id}_form,{id}_sha256,{id}_bytes,{text},{text}_form,{text}_sha256,{text}_bytes,{other}) VALUES('p',?1,?2,?3,?4,?5,?6,?7,?8,{values})"
                ),
                rusqlite::params![id_value, form, sha, bytes, v, vform, (vform == 1).then_some(&hash), (vform == 1).then_some(65537)],
            )
        };
        // A literal that looks like a digest and an indirect identity with that digest are two rows.
        insert(&hash, 0, None, None, "v", 0).unwrap();
        insert(&hash, 1, Some(&hash), Some(65537), "v", 0).unwrap();
        assert!(
            insert(&hash, 0, None, None, "w", 0).is_err(),
            "{table}: duplicate literal"
        );
        assert!(
            insert(&hash, 1, Some(&hash), Some(65537), "w", 0).is_err(),
            "{table}: duplicate indirect"
        );
        // An indirect identity holds its digest and a length over the inline bound.
        for (name, form, sha, bytes) in [
            ("a", 1, Some(hash.as_str()), Some(65537)),
            (hash.as_str(), 1, Some(hash.as_str()), Some(65536)),
            (hash.as_str(), 1, Some(hash.as_str()), None),
            (hash.as_str(), 1, None, Some(65537)),
            (hash.as_str(), 2, None, None),
            (over.as_str(), 0, None, None),
            ("b", 0, Some(hash.as_str()), None),
        ] {
            assert!(
                insert(name, form, sha, bytes, "v", 0).is_err(),
                "{table}: {name:.8} {form} {sha:?} {bytes:?}"
            );
        }
        // A text is inline up to 65,536 bytes; above it the column is empty beside a digest.
        insert("inline", 0, None, None, &"x".repeat(65536), 0).unwrap();
        assert!(
            insert("big-inline", 0, None, None, &over, 0).is_err(),
            "{table}: oversize inline text"
        );
        insert("indirect-text", 0, None, None, "", 1).unwrap();
        assert!(
            insert("held", 0, None, None, "kept", 1).is_err(),
            "{table}: form 1 holds ''"
        );
        assert!(insert("no-digest", 0, None, None, "", 3).is_err());
        // The counters of the port are never optional or out of range.
        for set in [
            "position=-1",
            "revision=0",
            "position=NULL",
            "revision=NULL",
        ] {
            assert!(
                conn.execute(&format!("UPDATE {table} SET {set} WHERE {id}='inline'"), [])
                    .is_err(),
                "{table}: {set}"
            );
        }
    }
    let goal = |set: &str| conn.execute(&format!("UPDATE goals SET {set} WHERE id='inline'"), []);
    assert!(goal("status='done'").is_err());
    for status in ["active", "paused", "achieved", "abandoned"] {
        goal(&format!("status='{status}'")).unwrap();
    }
    // A goal's optional texts are NULL only inline, and its times are never required.
    goal("assignee_agent_id=NULL,created_at=NULL,updated_at=NULL,updated_at_text=NULL").unwrap();
    assert!(goal("assignee_agent_id_form=1").is_err());
    assert!(goal(&format!("assignee_agent_id='{over}'")).is_err());
    let context = |set: &str| {
        conn.execute(
            &format!("UPDATE context_entries SET {set} WHERE key='inline'"),
            [],
        )
    };
    context("ttl_ms=NULL,created_at=NULL,updated_at=NULL,expires_at=NULL").unwrap();
    context("ttl_ms=0").unwrap();
    context(&format!("ttl_ms={}", i64::MAX)).unwrap();
    assert!(context("ttl_ms=-1").is_err());
    for column in ["set_by_identity", "set_by_agent_id", "set_by_run_id"] {
        context(&format!("{column}='ok'")).unwrap();
        assert!(context(&format!("{column}='{over}'")).is_err(), "{column}");
    }
    assert!(context("set_by_kind=NULL").is_err());
}

#[test]
fn message_family_extends_only_the_existing_entity_checks() {
    use orch_infra::sqlite::migrations::{
        AGENT_TEAM_RUN, ATTENTION_FACTS, CONTEXT_CONFIG_TEMPLATE_GOAL, DiskSpace, INITIAL_SCHEMA,
        TASK_FORMAT, apply_migrations,
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    apply_migrations(
        &dir,
        &mut conn,
        &[
            INITIAL_SCHEMA,
            ATTENTION_FACTS,
            TASK_FORMAT,
            AGENT_TEAM_RUN,
            CONTEXT_CONFIG_TEMPLATE_GOAL,
        ],
        DiskSpace {
            available: u64::MAX,
            total: 1,
        },
    )
    .unwrap();
    let ddl = |conn: &rusqlite::Connection, table: &str| -> String {
        conn.query_row(
            "SELECT sql FROM sqlite_schema WHERE name=?1",
            [table],
            |r| r.get(0),
        )
        .unwrap()
    };
    let old: Vec<_> = ["entity_details", "entity_blob_refs"]
        .iter()
        .map(|t| ddl(&conn, t))
        .collect();
    migrate(&dir, &mut conn).unwrap();
    for (i, table) in ["entity_details", "entity_blob_refs"].iter().enumerate() {
        // ALTER TABLE quotes the new table name; all constraints stay identical.
        assert_eq!(
            ddl(&conn, table).replace(&format!("\"{table}\""), table),
            old[i].replace("'template','goal'", "'template','goal','message'")
        );
    }
    for family in [
        "agent", "team", "run", "context", "config", "template", "goal", "message", "unknown",
    ] {
        let detail = conn.execute(
            "INSERT INTO entity_details VALUES('p',?1,0,'key',1,'{}',2,?2)",
            rusqlite::params![family, "0".repeat(64)],
        );
        assert_eq!(detail.is_ok(), family != "unknown", "{family}");
        for (locator, size) in [("/body", 65537), ("/prompt", 0), ("/body", 0)] {
            conn.execute_batch("SAVEPOINT probe").unwrap();
            let refs = conn.execute(
                "INSERT INTO entity_blob_refs VALUES('p',?1,0,'key',?2,0,?3,?4)",
                rusqlite::params![family, locator, "1".repeat(64), size],
            );
            assert_eq!(
                refs.is_ok(),
                family != "unknown" && (size > 0 || (family == "run" && locator == "/prompt")),
                "{family} {locator} {size}"
            );
            conn.execute_batch("ROLLBACK TO probe; RELEASE probe")
                .unwrap();
        }
    }
    let partial: bool = conn
        .query_row(
            "SELECT partial FROM pragma_index_list('messages') WHERE name='messages_pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(partial);
    let pending_index = ddl(&conn, "messages_pending");
    assert_eq!(
        pending_index
            .split_once(" WHERE ")
            .map(|(_, predicate)| predicate),
        Some("status = 'pending'")
    );
    let indexes:Vec<(String,i64)> = conn.prepare("SELECT name,desc FROM pragma_index_xinfo('messages_pending') WHERE key=1 ORDER BY seqno").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(
        indexes,
        [
            ("project_id".into(), 0),
            ("to_agent_id_form".into(), 0),
            ("to_agent_id".into(), 0)
        ]
    );
}

#[test]
fn message_native_quartets_enforce_r97_bytes_presence_and_identity_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = data(&tmp);
    let mut conn = open_connection(&dir, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&dir, &mut conn).unwrap();
    conn.execute("INSERT INTO messages(project_id,id,channel,from_agent_id,subject,body,status,position,revision,created_at_text) VALUES('p','m','lead','sender','','body','pending',1,1,'opaque')",[]).unwrap();
    let hash = "a".repeat(64);
    for (field, nullable) in [
        ("id", false),
        ("from_agent_id", false),
        ("to_agent_id", true),
        ("team_id", true),
        ("reply_to", true),
        ("subject", false),
        ("body", false),
        ("created_at_text", false),
        ("expires_at_text", true),
        ("delivered_at_text", true),
    ] {
        let sql = format!("UPDATE messages SET {field}=?1 WHERE project_id='p'");
        for text in [
            "".into(),
            "é".repeat(32768),
            format!("{}\0hidden", "x".repeat(65529)),
        ] {
            probe_update(&conn, &sql, &[&text], true);
        }
        probe_update(&conn, &sql, &[&"é".repeat(32769)], false);
        probe_update(&conn, &sql, &[&Option::<String>::None], nullable);
        let indirect = format!(
            "UPDATE messages SET {field}=?1,{field}_form=1,{field}_sha256=?2,{field}_bytes=?3 WHERE project_id='p'"
        );
        let projection = if field == "id" { hash.as_str() } else { "" };
        probe_update(&conn, &indirect, &[&projection, &hash, &65537], true);
        let mismatch = if field == "id" {
            "b".repeat(64)
        } else {
            "x".into()
        };
        probe_update(&conn, &indirect, &[&mismatch, &hash, &65537], false);
        probe_update(
            &conn,
            &indirect,
            &[&Option::<String>::None, &hash, &65537],
            false,
        );
        probe_update(&conn, &indirect, &[&projection, &hash, &65536], false);
        for bad in [
            format!("{}\0", "a".repeat(63)),
            "A".repeat(64),
            "z".repeat(64),
            "a".repeat(63),
        ] {
            let projection = if field == "id" { bad.as_str() } else { "" };
            probe_update(&conn, &indirect, &[&projection, &bad, &65537], false);
        }
        probe_update(
            &conn,
            &indirect,
            &[&projection, &Option::<String>::None, &65537],
            false,
        );
        probe_update(
            &conn,
            &format!("UPDATE messages SET {field}_sha256=?1 WHERE project_id='p'"),
            &[&hash],
            false,
        );
        probe_update(
            &conn,
            &format!("UPDATE messages SET {field}_bytes=65537 WHERE project_id='p'"),
            &[],
            false,
        );
        probe_update(
            &conn,
            &format!("UPDATE messages SET {field}_form=2 WHERE project_id='p'"),
            &[],
            false,
        );
    }
    for sql in [
        "UPDATE messages SET revision=0",
        "UPDATE messages SET position=-1",
        "UPDATE messages SET channel='unknown'",
        "UPDATE messages SET status='unknown'",
    ] {
        probe_update(&conn, sql, &[], false);
    }
    // A literal digest and the indirect identity/recipient must coexist.
    conn.execute("INSERT INTO messages(project_id,id,channel,from_agent_id,to_agent_id,subject,body,status,position,revision,created_at_text) VALUES('p',?1,'direct','sender',?1,'','','pending',2,1,'')",[&hash]).unwrap();
    conn.execute("INSERT INTO messages(project_id,id,id_form,id_sha256,id_bytes,channel,from_agent_id,to_agent_id,to_agent_id_form,to_agent_id_sha256,to_agent_id_bytes,subject,body,status,position,revision,created_at_text) VALUES('p',?1,1,?1,65537,'broadcast','sender','',1,?1,65537,'','','pending',3,1,'')",[&hash]).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM messages WHERE id=?1", [&hash], |r| r
            .get(0))
            .unwrap(),
        2
    );
}
