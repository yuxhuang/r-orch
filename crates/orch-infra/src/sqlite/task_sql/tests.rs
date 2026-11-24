use super::*;
use orch_core::store::{Entity, Revision};
use orch_domain::Task;
use rusqlite::params;

pub(super) fn task(id: &str) -> Task {
    serde_json::from_value(serde_json::json!({
        "id": id, "title":"sample", "description":"", "status":"todo", "priority":-3,
        "labels":["b","a","b"], "depends_on":["z","a","z"],
        "created_at":"2026-11-20T10:00:00.000Z", "updated_at":"2026-11-20T10:00:00.000Z",
        "attempts":0, "max_attempts":3
    }))
    .unwrap()
}

pub(super) fn conn() -> rusqlite::Connection {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(super::super::migrations::INITIAL_SCHEMA)
        .unwrap();
    c.execute_batch(super::super::migrations::ATTENTION_FACTS)
        .unwrap();
    c.execute_batch(super::super::migrations::TASK_FORMAT)
        .unwrap();
    c
}

fn seed(tx: &Transaction<'_>, disk: &DiskBlobStore, project: &str, task: &Task) {
    // Fixture SQL deliberately spells projections independently of production.
    let short = |text: &str| {
        if text.len() <= 65536 {
            text.to_owned()
        } else {
            String::new()
        }
    };
    tx.execute("INSERT INTO tasks(project_id,id,goal_id,title,description,status,priority,assignee_agent_id,attempts,max_attempts,workspace_mode,awaiting_approval,feedback,position,created_by_kind,landing_state,created_at,updated_at,revision)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,1,'owner',?14,?15,?16,1)",
        params![project,task.id,task.goal_id,short(&task.title),short(&task.description),task.status.to_string(),task.priority,task.assignee,task.attempts,task.max_attempts,task.workspace_mode.map(|m| m.to_string()),i64::from(task.labels.iter().any(|s| s=="awaiting-approval")),task.feedback.as_deref().filter(|s| s.len()<=65536),if task.status==orch_domain::TaskStatus::Done {Some("skipped")}else{None},task.created_at.parse::<jiff::Timestamp>().unwrap().as_millisecond(),task.updated_at.parse::<jiff::Timestamp>().unwrap().as_millisecond()]).unwrap();
    replace_detail(tx, disk, project, &task.id, task);
    for (query, values) in [
        (
            "INSERT OR IGNORE INTO task_dependencies VALUES (?1,?2,?3)",
            task.depends_on.as_slice(),
        ),
        (
            "INSERT OR IGNORE INTO task_labels VALUES (?1,?2,?3)",
            task.labels.as_slice(),
        ),
        (
            "INSERT OR IGNORE INTO task_scopes VALUES (?1,?2,?3)",
            task.scope.as_deref().unwrap_or_default(),
        ),
    ] {
        for value in values.iter().filter(|s| s.len() <= 65536) {
            tx.execute(query, params![project, task.id, value]).unwrap();
        }
    }
}

fn replace_detail(
    tx: &Transaction<'_>,
    disk: &DiskBlobStore,
    project: &str,
    id: &str,
    task: &Task,
) {
    let detail = super::super::task_format::codec::encode(disk, task).unwrap();
    tx.execute(
        "INSERT OR REPLACE INTO task_details VALUES (?1,?2,1,?3,?4,?5)",
        params![
            project,
            id,
            detail.inline,
            detail.bytes as i64,
            detail.sha256
        ],
    )
    .unwrap();
    tx.execute(
        "DELETE FROM task_blob_refs WHERE project_id=?1 AND task_id=?2",
        params![project, id],
    )
    .unwrap();
    for (locator, chunks) in detail.refs {
        for chunk in chunks {
            tx.execute(
                "INSERT INTO task_blob_refs VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    project,
                    id,
                    locator,
                    chunk.ordinal as i64,
                    chunk.sha256,
                    chunk.size as i64
                ],
            )
            .unwrap();
            // Test fixture registration only. Production registration is T11's.
            tx.execute(
                "INSERT OR IGNORE INTO blobs VALUES (?1,?2,?3,'disk',0,NULL)",
                params![project, chunk.sha256, chunk.size as i64],
            )
            .unwrap();
        }
    }
}

pub(super) fn damaged(error: SqliteError, task: &str) {
    match error {
        SqliteError::TaskCorrupt {
            task: actual,
            check,
            locator,
        } => {
            assert_eq!(actual, task);
            assert!(check.len() <= 32);
            assert!(locator.len() <= 32);
            assert!(!check.contains("secret"));
            assert!(!locator.contains("secret"));
        }
        other => panic!("expected structured damage: {other:?}"),
    }
}

#[test]
fn a_task_round_trips_every_field_with_order_repeats_and_presence() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    let mut task = task("tsk_Test");
    task.assignee = Some("agent".into());
    task.goal_id = Some("goal".into());
    task.workspace_mode = Some(orch_domain::WorkspaceMode::Worktree);
    task.workspace = Some("path-is-only-data".into());
    task.feedback = Some(String::new());
    task.proof = Some(orch_domain::TaskProof {
        remote: Some("remote".into()),
        branch: Some("branch".into()),
        pr_url: Some("link".into()),
        files_changed: vec!["z".into(), "a".into(), "z".into()],
        test_results: Some("test output".into()),
        agent_summary: Some("summary".into()),
    });
    task.scope = Some(vec!["b/**".into(), "a/**".into(), "b/**".into()]);
    task.attachments = Some(vec!["../secret".into(), "../secret".into()]);
    task.review_criteria = Some(vec![orch_domain::ReviewCriterion::Lint]);
    task.review_results = Some(vec![orch_domain::ReviewResult {
        criterion: orch_domain::ReviewCriterion::Lint,
        passed: false,
        output: "output".into(),
    }]);
    let caller = orch_core::store::Principal::Owner {
        identity: "owner".into(),
    };
    let mut writer = super::writes::TaskWrite::new(
        TaskSql {
            tx: &tx,
            project: "p",
            disk: &disk,
        },
        &caller,
    );
    writer.create(task.clone()).unwrap();
    let record = sql.get(&task.id).unwrap().unwrap();
    assert_eq!(record.entity, Entity::Task(Box::new(task.clone())));
    assert_eq!(record.revision, Revision::ONE);
    assert_eq!(record.position, 1);
    task.feedback = None;
    task.scope = Some(Vec::new());
    task.proof = None;
    task.workspace_mode = None;
    writer.update(task.clone(), record.revision).unwrap();
    let updated = sql.get(&task.id).unwrap().unwrap();
    assert_eq!(updated.entity, Entity::Task(Box::new(task)));
    assert_eq!(updated.position, 1);
    assert_eq!(updated.revision.get(), 2);
}

#[test]
fn projection_drift_in_each_native_column_and_membership_is_typed() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    seed(&tx, &disk, "p", &task("t"));
    for mutation in [
        "UPDATE tasks SET goal_id='secret'",
        "UPDATE tasks SET title='secret'",
        "UPDATE tasks SET description='secret'",
        "UPDATE tasks SET status='failed'",
        "UPDATE tasks SET priority=4",
        "UPDATE tasks SET assignee_agent_id='secret'",
        "UPDATE tasks SET attempts=1",
        "UPDATE tasks SET max_attempts=4",
        "UPDATE tasks SET workspace_mode='isolated'",
        "UPDATE tasks SET awaiting_approval=1",
        "UPDATE tasks SET feedback=''",
        "UPDATE tasks SET created_at=1",
        "UPDATE tasks SET updated_at=1",
        "UPDATE tasks SET revision=0",
        "UPDATE tasks SET position=-1",
        "DELETE FROM task_dependencies",
        "DELETE FROM task_labels",
        "INSERT INTO task_scopes VALUES ('p','t','secret')",
    ] {
        tx.execute_batch("SAVEPOINT fixture").unwrap();
        tx.execute_batch(mutation).unwrap();
        damaged(sql.get("t").unwrap_err(), "t");
        tx.execute_batch("ROLLBACK TO fixture; RELEASE fixture")
            .unwrap();
        assert!(sql.get("t").unwrap().is_some(), "{mutation}");
    }
}

#[test]
fn unknown_format_missing_detail_wrong_identity_and_bad_root_are_typed() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    seed(&tx, &disk, "p", &task("t"));
    for mutation in [
        "UPDATE task_details SET format=2",
        "DELETE FROM task_details",
        "UPDATE task_details SET detail_sha256='secret'",
        "UPDATE task_details SET detail_sha256=printf('%064d',0)",
        "UPDATE task_details SET detail=replace(detail,'sample','sampLe')",
    ] {
        tx.execute_batch("SAVEPOINT fixture").unwrap();
        tx.execute_batch(mutation).unwrap();
        damaged(sql.get("t").unwrap_err(), "t");
        tx.execute_batch("ROLLBACK TO fixture; RELEASE fixture")
            .unwrap();
    }
    replace_detail(&tx, &disk, "p", "t", &task("other-secret"));
    damaged(sql.get("t").unwrap_err(), "t");
    assert!(sql.get("missing").unwrap().is_none());
}

#[test]
fn native_extremes_and_unowned_columns_do_not_rewrite_the_detail() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let mut task = task("t");
    task.priority = i64::MIN;
    task.attempts = u32::MAX;
    task.max_attempts = u32::MAX;
    seed(&tx, &disk, "p", &task);
    tx.execute_batch(
        "UPDATE tasks SET review='secret', legacy_proof='secret', outputs='secret',
        created_by_kind='import', created_by_run_id='secret', accepted_by='secret',
        accepted_at=1, accepted_run_id='secret', landing_state='landed', landed_commit='secret',
        landed_into='secret', landed_at=2, landing_attempts=7, landing_conflict_paths='secret',
        attention_reason='secret', attention_since=3, revision=9223372036854775807,
        position=9223372036854775807",
    )
    .unwrap();
    let before = tx.total_changes();
    let record = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    }
    .get("t")
    .unwrap()
    .unwrap();
    assert_eq!(record.entity, Entity::Task(Box::new(task)));
    assert_eq!(record.revision.get(), i64::MAX as u64);
    assert_eq!(record.position, i64::MAX as u64);
    assert_eq!(tx.total_changes(), before);
}

#[test]
fn indirect_fields_memberships_and_aggregate_root_use_project_scoped_metadata() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let mut task = task("t");
    task.title = "t".repeat(65537);
    task.description = "d".repeat(65536);
    task.feedback = Some("f".repeat(65537));
    task.labels.push("l".repeat(65537));
    task.scope = Some(vec![
        "s".repeat(65536),
        "s".repeat(65536),
        "s".repeat(65537),
    ]);
    seed(&tx, &disk, "p", &task);
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    assert_eq!(
        sql.get("t").unwrap().unwrap().entity,
        Entity::Task(Box::new(task))
    );
    assert!(
        tx.query_row("SELECT detail IS NULL FROM task_details", [], |r| r
            .get::<_, bool>(0))
            .unwrap()
    );
    for mutation in [
        "UPDATE blobs SET project_id='other'",
        "UPDATE blobs SET size=size+1",
        "UPDATE blobs SET storage_location='secret'",
        "DELETE FROM task_blob_refs WHERE locator='/title'",
        "UPDATE task_blob_refs SET ordinal=ordinal+1",
        "UPDATE task_blob_refs SET locator='/secret' WHERE locator='/title'",
        "INSERT INTO task_labels VALUES ('p','t','secret')",
    ] {
        tx.execute_batch("SAVEPOINT fixture").unwrap();
        tx.execute_batch(mutation).unwrap();
        damaged(sql.get("t").unwrap_err(), "t");
        tx.execute_batch("ROLLBACK TO fixture; RELEASE fixture")
            .unwrap();
    }
}

#[test]
fn hydrate_retains_input_order_project_binding_and_discards_partial_results() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    for id in ["a", "B", "c"] {
        seed(&tx, &disk, "p", &task(id));
    }
    let mut other = task("a");
    other.title = "other project".into();
    seed(&tx, &disk, "q", &other);
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    let ids = vec!["c".into(), "a".into(), "B".into()];
    let rows = sql.hydrate(&ids).unwrap();
    for (row, id) in rows.iter().zip(&ids) {
        assert_eq!(row.entity, Entity::Task(Box::new(task(id))));
    }
    assert_eq!(
        TaskSql {
            tx: &tx,
            project: "q",
            disk: &disk
        }
        .get("a")
        .unwrap()
        .unwrap()
        .entity,
        Entity::Task(Box::new(other))
    );
    damaged(
        sql.hydrate(&["a".into(), "missing".into()]).unwrap_err(),
        "missing",
    );
    tx.execute(
        "DELETE FROM task_details WHERE project_id='p' AND task_id='B'",
        [],
    )
    .unwrap();
    damaged(sql.hydrate(&ids).unwrap_err(), "B");
}

#[test]
fn snapshot_hydrates_selected_ids_before_and_after_another_connection_commits() {
    let (temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let path = temp.path().join("snapshot.sqlite");
    let mut reader = rusqlite::Connection::open(&path).unwrap();
    reader.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    for ddl in [
        super::super::migrations::INITIAL_SCHEMA,
        super::super::migrations::ATTENTION_FACTS,
        super::super::migrations::TASK_FORMAT,
    ] {
        reader.execute_batch(ddl).unwrap();
    }
    let tx = reader.transaction().unwrap();
    seed(&tx, &disk, "p", &task("a"));
    seed(&tx, &disk, "p", &task("B"));
    tx.commit().unwrap();
    let tx = reader.transaction().unwrap();
    let page = super::super::task_page::query(
        &tx,
        "p",
        &orch_core::store::Filter::Tasks {
            status: None,
            goal: None,
        },
        &orch_core::store::PageRequest {
            sort: orch_core::store::Sort::Id,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert_eq!(page.ids, ["B", "a"]);
    let mut writer = rusqlite::Connection::open(path).unwrap();
    let write = writer.transaction().unwrap();
    write.execute("DELETE FROM tasks WHERE id='a'", []).unwrap();
    write
        .execute("DELETE FROM task_details WHERE task_id='a'", [])
        .unwrap();
    write.commit().unwrap();
    let rows = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    }
    .hydrate(&page.ids)
    .unwrap();
    assert_eq!(rows.len(), 2);
    tx.commit().unwrap();
    let tx = reader.transaction().unwrap();
    damaged(
        TaskSql {
            tx: &tx,
            project: "p",
            disk: &disk,
        }
        .hydrate(&page.ids)
        .unwrap_err(),
        "a",
    );
}

#[test]
fn transient_access_remains_distinct_and_corrupt_objects_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let mut task = task("t");
    task.description = "secret".repeat(12000);
    seed(&tx, &disk, "p", &task);
    let hash = tx
        .query_row(
            "SELECT sha256 FROM task_blob_refs WHERE locator='/description'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    let path = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash);
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let error = sql.get("t").unwrap_err();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        matches!(
            error,
            SqliteError::TaskAccess(std::io::ErrorKind::PermissionDenied)
        ),
        "{error}"
    );
    assert!(
        !error
            .to_string()
            .contains(&temp.path().display().to_string())
    );
    assert!(sql.get("t").unwrap().is_some());
    std::fs::write(&path, b"corrupt").unwrap();
    damaged(sql.get("t").unwrap_err(), "t");
    std::fs::remove_file(path).unwrap();
    damaged(sql.get("t").unwrap_err(), "t");
}

#[test]
fn reference_rows_cannot_smuggle_keys_and_registrations_are_checked_before_access() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let mut task = task("t");
    task.description = "d".repeat(65537);
    task.feedback = Some("f".repeat(65537));
    seed(&tx, &disk, "p", &task);
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    let hash = tx
        .query_row(
            "SELECT sha256 FROM task_blob_refs WHERE locator='/description'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    let path = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    tx.execute("DELETE FROM blobs WHERE sha256=(SELECT sha256 FROM task_blob_refs WHERE locator='/feedback')",[]).unwrap();
    let error = sql.get("t").unwrap_err();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    damaged(error, "t");
    replace_detail(&tx, &disk, "p", "t", &task);
    tx.execute(
        "UPDATE task_blob_refs SET locator=?1 WHERE locator='/description'",
        ["/secret/".repeat(1000)],
    )
    .unwrap();
    damaged(sql.get("t").unwrap_err(), "t");
}

#[test]
fn a_surplus_reference_without_a_tag_is_damage_and_sql_failures_stay_typed() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    seed(&tx, &disk, "p", &task("t"));
    let hash = disk.put(b"object").unwrap().to_string();
    tx.execute("INSERT INTO blobs VALUES ('p',?1,6,'disk',0,NULL)", [&hash])
        .unwrap();
    tx.execute(
        "INSERT INTO task_blob_refs VALUES ('p','t','/secret',0,?1,6)",
        [&hash],
    )
    .unwrap();
    let sql = TaskSql {
        tx: &tx,
        project: "p",
        disk: &disk,
    };
    damaged(sql.get("t").unwrap_err(), "t");
    tx.execute("DELETE FROM task_blob_refs", []).unwrap();
    assert!(sql.get("t").unwrap().is_some());
    tx.execute("DROP TABLE task_details", []).unwrap();
    assert!(matches!(sql.get("t"), Err(SqliteError::Sql(_))));
}

#[test]
fn transaction_local_reads_observe_one_uncommitted_state_and_rollback() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    {
        let tx = conn.transaction().unwrap();
        seed(&tx, &disk, "p", &task("t"));
        let sql = TaskSql {
            tx: &tx,
            project: "p",
            disk: &disk,
        };
        assert_eq!(sql.hydrate(&["t".into()]).unwrap().len(), 1);
        tx.rollback().unwrap();
    }
    let tx = conn.transaction().unwrap();
    assert!(
        TaskSql {
            tx: &tx,
            project: "p",
            disk: &disk
        }
        .get("t")
        .unwrap()
        .is_none()
    );
}
