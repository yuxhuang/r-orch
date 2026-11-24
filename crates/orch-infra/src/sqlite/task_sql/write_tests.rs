use super::tests::{conn, damaged, task};
use super::writes::TaskWrite;
use super::*;
use orch_core::store::{Principal, StoreError};
use orch_domain::TaskStatus;

fn owner() -> Principal {
    Principal::Owner {
        identity: "owner".into(),
    }
}
fn sql<'a, 'conn>(
    tx: &'a Transaction<'conn>,
    disk: &'a DiskBlobStore,
    project: &'a str,
) -> TaskSql<'a, 'conn> {
    TaskSql {
        tx,
        disk,
        project,
        state: FailureState::new(),
        source: FailureSource::Writer,
    }
}

// Capture all persisted operation state, including independent columns and
// project rows, without relying on the write implementation's projections.
fn state(tx: &Transaction<'_>) -> Vec<Vec<Vec<Value>>> {
    [
        "tasks",
        "task_details",
        "task_blob_refs",
        "task_dependencies",
        "task_labels",
        "task_scopes",
        "task_attachments",
        "blobs",
        "counters",
    ]
    .iter()
    .map(|table| {
        let mut stmt = tx
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2,3"))
            .unwrap();
        let columns = stmt.column_count();
        stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn published_slice_lengths_and_bytes_round_trip_through_real_task_load() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    let mut slot = task("slot");
    // UTF-8 byte count differs from character count; JSON escaping would also
    // differ if registration accidentally measured the serialized string.
    slot.description = "é\"\n".repeat(20000);
    let mut root = task("root");
    root.labels = (0..15000).map(|n| format!("label-\"{n}")).collect();
    let root_bytes = serde_json::to_vec(&root).unwrap();
    for input in [&slot, &root] {
        writer.create(input.clone()).unwrap();
        let loaded = sql(&tx, &disk, "p").get(&input.id).unwrap().unwrap();
        assert_eq!(loaded.entity, Entity::Task(Box::new(input.clone())));
        let mut statement = tx.prepare("SELECT r.locator,r.sha256,r.size,b.size FROM task_blob_refs r JOIN blobs b ON b.project_id=r.project_id AND b.sha256=r.sha256 WHERE r.project_id='p' AND r.task_id=?1 ORDER BY r.locator,r.ordinal").unwrap();
        let rows: Vec<(String, String, i64, i64)> = statement
            .query_map([&input.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!rows.is_empty());
        let mut returned = Vec::new();
        for (locator, digest, reference_length, registered_length) in rows {
            let hash: BlobHash = digest.parse().unwrap();
            let bytes = disk.get(&hash).unwrap();
            assert_eq!(hash, BlobHash::of(&bytes));
            assert_eq!(reference_length, i64::try_from(bytes.len()).unwrap());
            assert_eq!(registered_length, reference_length);
            assert_eq!(
                locator,
                if input.id == "slot" {
                    "/description"
                } else {
                    ""
                }
            );
            returned.extend(bytes);
        }
        assert_eq!(
            returned,
            if input.id == "slot" {
                slot.description.as_bytes()
            } else {
                &root_bytes
            }
        );
    }
    writer.ensure_committable().unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        sql(&tx, &disk, "p").get("slot").unwrap().unwrap().entity,
        Entity::Task(Box::new(slot))
    );
    assert_eq!(
        sql(&tx, &disk, "p").get("root").unwrap().unwrap().entity,
        Entity::Task(Box::new(root))
    );
}

#[test]
fn creator_is_derived_once_and_independent_columns_survive_update_and_move() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let callers = [
        owner(),
        Principal::Worker {
            identity: "worker".into(),
        },
        Principal::Task {
            identity: "identity".into(),
            task: "authenticated-task".into(),
            run: "fenced-run".into(),
            lineage: "lineage".into(),
            epoch: 2,
        },
    ];
    for (i, caller) in callers.iter().enumerate() {
        let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), caller);
        let mut input = task(&format!("t{i}"));
        let first = writer.create(input.clone()).unwrap();
        assert_eq!(first.revision, Revision::ONE);
        let creator: (String, Option<String>) = tx
            .query_row(
                "SELECT created_by_kind,created_by_run_id FROM tasks WHERE id=?1",
                [&input.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            creator,
            match i {
                0 => ("owner".into(), None),
                1 => ("daemon".into(), None),
                _ => ("agent".into(), Some("fenced-run".into())),
            }
        );
        tx.execute("UPDATE tasks SET review='review',legacy_proof='legacy',outputs='outputs',accepted_by='reviewer',accepted_at=7,accepted_run_id='accepted',landing_state='landed',landed_commit='commit',landed_into='branch',landed_at=8,landing_attempts=2,landing_conflict_paths='paths',attention_reason='reason',attention_since=9 WHERE id=?1", [&input.id]).unwrap();
        let independent = |tx: &Transaction<'_>| -> Vec<Value> {
            tx.query_row("SELECT review,legacy_proof,outputs,created_by_kind,created_by_run_id,accepted_by,accepted_at,accepted_run_id,landing_state,landed_commit,landed_into,landed_at,landing_attempts,landing_conflict_paths,attention_reason,attention_since FROM tasks WHERE id=?1", [&input.id], |r| (0..16).map(|i| r.get(i)).collect()).unwrap()
        };
        let before = independent(&tx);
        input.title = "edited".into();
        input.labels.push("awaiting-approval".into());
        input.scope = Some(vec!["x/**".into(), "x/**".into()]);
        let updated = writer.update(input.clone(), first.revision).unwrap();
        assert_eq!(updated.position, first.position);
        assert_eq!(updated.revision.get(), 2);
        let detail_before: String = tx
            .query_row(
                "SELECT detail FROM task_details WHERE task_id=?1",
                [&input.id],
                |r| r.get(0),
            )
            .unwrap();
        let (revision, position) = writer.move_to_end(&input.id, updated.revision).unwrap();
        assert_eq!(revision.get(), 3);
        assert!(position > first.position);
        assert_eq!(independent(&tx), before);
        let detail_after: String = tx
            .query_row(
                "SELECT detail FROM task_details WHERE task_id=?1",
                [&input.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(detail_before, detail_after);
        assert_eq!(
            sql(&tx, &disk, "p").get(&input.id).unwrap().unwrap().entity,
            Entity::Task(Box::new(input))
        );
    }
}

#[test]
fn landing_state_follows_completion_and_survives_reopening() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    let mut input = task("t");
    input.status = TaskStatus::Done;
    let mut revision = writer.create(input.clone()).unwrap().revision;
    for status in [TaskStatus::Todo, TaskStatus::Done] {
        input.status = status;
        revision = writer.update(input.clone(), revision).unwrap().revision;
        assert_eq!(
            tx.query_row("SELECT landing_state FROM tasks", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "skipped"
        );
    }
    tx.execute(
        "UPDATE tasks SET landing_state='landed',accepted_run_id='run',landed_commit='commit'",
        [],
    )
    .unwrap();
    input.status = TaskStatus::Todo;
    revision = writer.update(input.clone(), revision).unwrap().revision;
    input.status = TaskStatus::Done;
    writer.update(input, revision).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT landing_state,accepted_run_id,landed_commit FROM tasks",
            [],
            |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        )
        .unwrap(),
        ("landed".into(), "run".into(), "commit".into())
    );
    let mut second = task("second");
    let revision = writer.create(second.clone()).unwrap().revision;
    second.status = TaskStatus::Done;
    writer.update(second, revision).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT landing_state FROM tasks WHERE id='second'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "skipped"
    );
}

#[test]
fn alias_stale_unrepresentable_and_exhausted_counters_change_nothing() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    writer.create(task("Case")).unwrap();
    let before = state(&tx);
    assert!(matches!(
        writer.create(task("case")),
        Err(SqliteError::Store(StoreError::IdAlias { .. }))
    ));
    assert!(matches!(
        writer.create(task("Case")),
        Err(SqliteError::Store(StoreError::AlreadyExists { .. }))
    ));
    assert!(matches!(
        writer.update(task("Case"), Revision::ONE.next().unwrap()),
        Err(SqliteError::Store(StoreError::StaleRevision { .. }))
    ));
    assert!(matches!(
        writer.delete("missing", Revision::ONE),
        Err(SqliteError::Store(StoreError::StaleRevision {
            actual: None,
            ..
        }))
    ));
    let mut input = task("bad");
    input.created_at = "not-canonical".into();
    input.description = "x".repeat(70000);
    assert!(matches!(
        writer.create(input),
        Err(SqliteError::Unrepresentable(_))
    ));
    assert_eq!(state(&tx), before);
    tx.execute("UPDATE counters SET value=?1", [i64::MAX])
        .unwrap();
    let before = state(&tx);
    assert!(matches!(
        writer.create(task("overflow")),
        Err(SqliteError::Store(StoreError::CounterExhausted))
    ));
    assert!(matches!(
        writer.move_to_end("Case", Revision::ONE),
        Err(SqliteError::Store(StoreError::CounterExhausted))
    ));
    assert_eq!(state(&tx), before);
    tx.execute("UPDATE tasks SET revision=?1", [i64::MAX])
        .unwrap();
    let max: Revision = serde_json::from_value(i64::MAX.into()).unwrap();
    let before = state(&tx);
    assert!(matches!(
        writer.update(task("Case"), max),
        Err(SqliteError::Store(StoreError::CounterExhausted))
    ));
    assert_eq!(state(&tx), before);
    writer.ensure_committable().unwrap();
}

#[test]
fn caught_sql_failures_restore_create_update_and_position_counter() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    let mut input = task("t");
    input.description = "old".repeat(30000);
    writer.create(input.clone()).unwrap();
    let before = state(&tx);
    tx.execute_batch("CREATE TEMP TRIGGER fail_label BEFORE INSERT ON task_labels WHEN NEW.label='fail' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let mut replacement = input.clone();
    replacement.labels.push("fail".into());
    replacement.description = "new".repeat(30000);
    assert!(matches!(
        writer.update(replacement, Revision::ONE),
        Err(SqliteError::Sql(_))
    ));
    assert_eq!(state(&tx), before);
    assert_eq!(
        sql(&tx, &disk, "p").get("t").unwrap().unwrap().entity,
        Entity::Task(Box::new(input))
    );
    tx.execute_batch("DROP TRIGGER fail_label; CREATE TEMP TRIGGER fail_counter BEFORE UPDATE ON counters BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        writer.create(task("new")),
        Err(SqliteError::Sql(_))
    ));
    assert_eq!(state(&tx), before);
    assert!(matches!(
        writer.move_to_end("t", Revision::ONE),
        Err(SqliteError::Sql(_))
    ));
    assert_eq!(state(&tx), before);
    tx.execute_batch("DROP TRIGGER fail_counter").unwrap();
    writer.create(task("unrelated")).unwrap();
    writer.ensure_committable().unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    assert!(sql(&tx, &disk, "p").get("new").unwrap().is_none());
    assert!(sql(&tx, &disk, "p").get("unrelated").unwrap().is_some());
}

#[test]
fn delete_is_scoped_keeps_shared_objects_and_restores_every_row_on_caught_failure() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let mut tx = conn.transaction().unwrap();
    let caller = owner();
    let mut input = task("same");
    input.description = "shared".repeat(12000);
    for project in ["p", "q"] {
        let mut writer = TaskWrite::new(sql(&tx, &disk, project), &caller);
        writer.create(input.clone()).unwrap();
        writer.create(task("other")).unwrap();
    }
    let hash = BlobHash::of(input.description.as_bytes());
    for (project, id) in [("p", "same"), ("p", "other"), ("q", "same")] {
        crate::blob::BlobReferences::new(&disk, project)
            .store_attachment(&mut tx, id, "name", input.description.as_bytes(), 7)
            .unwrap();
    }
    // Another naming kind also shares the object; Task deletion cannot reclaim it.
    tx.execute("INSERT INTO runs(project_id,id,task_id,agent_id,run_no,counted,epoch,status,started_at,prompt_sha256,revision) VALUES('p','run','same','a',1,0,1,'done',0,?1,1)", [hash.to_string()]).unwrap();
    let before = state(&tx);
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    assert!(matches!(
        writer.delete("same", Revision::ONE.next().unwrap()),
        Err(SqliteError::Store(StoreError::StaleRevision { .. }))
    ));
    assert_eq!(state(&tx), before);
    tx.execute_batch("CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON task_details BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        writer.delete("same", Revision::ONE),
        Err(SqliteError::Sql(_))
    ));
    assert_eq!(state(&tx), before);
    writer.ensure_committable().unwrap();
    tx.execute_batch("DROP TRIGGER fail_delete").unwrap();
    writer.create(task("caught-delete-unrelated")).unwrap();
    writer.ensure_committable().unwrap();
    let restored = state(&tx);
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(state(&tx), restored);
    assert!(sql(&tx, &disk, "p").get("same").unwrap().is_some());
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    writer.update(input.clone(), Revision::ONE).unwrap();
    assert_eq!(
        tx.query_row("SELECT count(*) FROM task_attachments", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    writer
        .move_to_end("same", Revision::ONE.next().unwrap())
        .unwrap();
    assert_eq!(
        tx.query_row("SELECT count(*) FROM task_attachments", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    writer
        .delete("same", Revision::ONE.next().unwrap().next().unwrap())
        .unwrap();
    for table in [
        "tasks",
        "task_details",
        "task_blob_refs",
        "task_dependencies",
        "task_labels",
        "task_scopes",
        "task_attachments",
    ] {
        let id = if table == "tasks" { "id" } else { "task_id" };
        assert_eq!(
            tx.query_row(
                &format!("SELECT count(*) FROM {table} WHERE project_id='p' AND {id}='same'"),
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    assert!(sql(&tx, &disk, "q").get("same").unwrap().is_some());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM task_attachments", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM blobs WHERE sha256=?1",
            [hash.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(disk.get(&hash).unwrap(), input.description.as_bytes());
    writer.create(task("unrelated")).unwrap();
    writer.ensure_committable().unwrap();
    tx.commit().unwrap();
}

#[test]
fn failed_rollback_preserves_both_causes_and_forbids_commit_after_catch() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    writer.create(task("t")).unwrap();
    tx.execute_batch("CREATE TEMP TRIGGER fail_rollback BEFORE DELETE ON task_details BEGIN SELECT RAISE(ROLLBACK,'injected'); END;").unwrap();
    let error = writer.delete("t", Revision::ONE).unwrap_err();
    match error {
        SqliteError::OperationRollback {
            operation,
            rollback,
        } => {
            assert!(matches!(*operation, SqliteError::Sql(_)));
            assert!(matches!(rollback, rusqlite::Error::SqliteFailure(..)));
        }
        other => panic!("both causes required: {other:?}"),
    }
    assert!(matches!(
        writer.ensure_committable(),
        Err(SqliteError::TransactionAborted)
    ));
    assert!(matches!(
        writer.create(task("later")),
        Err(SqliteError::TransactionAborted)
    ));
    // The shared writer must enforce this result; this private leaf never commits.
    assert!(tx.is_autocommit());
}

#[test]
fn malformed_utf8_is_structured_on_every_stored_text_surface_but_sql_errors_stay_sql() {
    for mutation in [
        "UPDATE tasks SET title=CAST(x'80' AS TEXT)",
        "UPDATE task_details SET detail=CAST(x'80' AS TEXT),detail_bytes=1",
        "UPDATE task_details SET detail_sha256=CAST(x'80' AS TEXT)",
        "UPDATE task_blob_refs SET locator=CAST(x'80' AS TEXT)",
        "UPDATE task_blob_refs SET sha256=CAST(x'80' AS TEXT)",
        "UPDATE task_labels SET label=CAST(x'80' AS TEXT) WHERE label='a'",
        "UPDATE blobs SET storage_location=CAST(x'80' AS TEXT)",
    ] {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = conn();
        let tx = conn.transaction().unwrap();
        let caller = owner();
        let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
        let mut input = task("t");
        input.description = "x".repeat(70000);
        writer.create(input).unwrap();
        tx.execute_batch(mutation).unwrap();
        damaged(sql(&tx, &disk, "p").get("t").unwrap_err(), "t");
    }
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    tx.execute_batch("DROP TABLE tasks").unwrap();
    assert!(matches!(
        sql(&tx, &disk, "p").get("missing"),
        Err(SqliteError::Sql(_))
    ));
}

#[test]
fn sql_projection_membership_and_counter_damage_precede_transient_slot_access() {
    use std::os::unix::fs::PermissionsExt;
    for mutation in [
        "UPDATE tasks SET priority=99",
        "DELETE FROM task_labels WHERE label='a'",
        "UPDATE tasks SET revision=0",
        "UPDATE tasks SET position=0",
    ] {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = conn();
        let tx = conn.transaction().unwrap();
        let caller = owner();
        let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
        let mut input = task("t");
        input.description = "x".repeat(70000);
        let hash = BlobHash::of(input.description.as_bytes()).to_string();
        writer.create(input).unwrap();
        let path = _temp
            .path()
            .join("data/blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let access = sql(&tx, &disk, "p").get("t").unwrap_err();
        assert!(matches!(
            access,
            SqliteError::TaskAccess(std::io::ErrorKind::PermissionDenied)
        ));
        tx.execute_batch(mutation).unwrap();
        let result = sql(&tx, &disk, "p").get("t");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        damaged(result.unwrap_err(), "t");
    }
}

#[test]
fn same_id_in_two_projects_with_distinct_memberships_and_references_loads_its_own_state() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    for project in ["p", "q"] {
        let mut input = task("same");
        input.description = project.repeat(70000);
        input.labels = vec![
            project.into(),
            "awaiting-approval".into(),
            "x".repeat(70000),
        ];
        input.scope = Some(vec![format!("{project}/**"), "x".repeat(70000)]);
        input.depends_on = vec![project.into(), "x".repeat(70000)];
        let mut writer = TaskWrite::new(sql(&tx, &disk, project), &caller);
        writer.create(input.clone()).unwrap();
        assert_eq!(
            sql(&tx, &disk, project)
                .get("same")
                .unwrap()
                .unwrap()
                .entity,
            Entity::Task(Box::new(input))
        );
    }
    tx.execute("DELETE FROM task_labels WHERE project_id='q'", [])
        .unwrap();
    damaged(sql(&tx, &disk, "q").get("same").unwrap_err(), "same");
    assert!(sql(&tx, &disk, "p").get("same").unwrap().is_some());
}

#[test]
fn move_changes_only_revision_position_without_reading_detail_or_objects() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let deny = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let active = deny.clone();
    conn.authorizer(Some(move |context: AuthContext<'_>| match context.action {
        AuthAction::Read { table_name, .. }
            if active.load(std::sync::atomic::Ordering::Relaxed)
                && matches!(
                    table_name,
                    "task_details"
                        | "task_blob_refs"
                        | "blobs"
                        | "task_labels"
                        | "task_dependencies"
                        | "task_scopes"
                ) =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }))
    .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    let mut input = task("t");
    input.description = "x".repeat(70000);
    writer.create(input.clone()).unwrap();
    let before = state(&tx);
    deny.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        writer.move_to_end("t", Revision::ONE).unwrap(),
        (Revision::ONE.next().unwrap(), 2)
    );
    deny.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut after = state(&tx);
    // The independently captured tasks row has position at column 16 and
    // revision last; all other bytes/tables are identical.
    after[0][0][16] = before[0][0][16].clone();
    let last = after[0][0].len() - 1;
    after[0][0][last] = before[0][0][last].clone();
    after[8] = before[8].clone(); // the durable position high-water mark
    assert_eq!(after, before);
    assert_eq!(
        sql(&tx, &disk, "p").get("t").unwrap().unwrap().entity,
        Entity::Task(Box::new(input))
    );
}

#[test]
fn a_late_publication_failure_keeps_old_rows_and_leaves_only_unreferenced_bytes() {
    let (temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    let input = task("t");
    writer.create(input.clone()).unwrap();
    let before = state(&tx);
    let mut replacement = input.clone();
    replacement.title = "title".repeat(15000);
    replacement.description = "description".repeat(7000);
    let title_hash = BlobHash::of(replacement.title.as_bytes());
    let description_hash = BlobHash::of(replacement.description.as_bytes()).to_string();
    assert_ne!(&title_hash.to_string()[..2], &description_hash[..2]);
    // A non-directory in the second object's shard fails publication after the
    // first object is durable. All paths are below synthetic test data.
    let obstruction = temp
        .path()
        .join("data/blobs/sha256")
        .join(&description_hash[..2]);
    std::fs::write(&obstruction, b"obstruction").unwrap();
    assert!(matches!(
        writer.update(replacement.clone(), Revision::ONE),
        Err(SqliteError::TaskPublication(_))
    ));
    assert_eq!(state(&tx), before);
    assert_eq!(disk.get(&title_hash).unwrap(), replacement.title.as_bytes());
    assert_eq!(
        sql(&tx, &disk, "p").get("t").unwrap().unwrap().entity,
        Entity::Task(Box::new(input))
    );
    writer.create(task("unrelated")).unwrap();
    writer.ensure_committable().unwrap();
    tx.commit().unwrap();
}

#[test]
fn missing_behind_and_negative_position_high_water_marks_are_refused_without_mutation() {
    for mutation in [
        "DELETE FROM counters",
        "UPDATE counters SET value=0",
        "UPDATE counters SET value=-1",
    ] {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = conn();
        let tx = conn.transaction().unwrap();
        let caller = owner();
        let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
        writer.create(task("t")).unwrap();
        tx.execute_batch(mutation).unwrap();
        let before = state(&tx);
        let error = writer.create(task("next")).unwrap_err();
        assert!(matches!(
            error,
            SqliteError::CounterMissing { .. }
                | SqliteError::CounterBehind { .. }
                | SqliteError::CounterNegative { .. }
        ));
        assert_eq!(state(&tx), before);
    }
}

#[test]
fn outer_rollback_alone_refuses_next_task_write() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    writer.create(task("before")).unwrap();
    tx.execute_batch("ROLLBACK").unwrap();
    assert!(tx.is_autocommit());
    assert!(matches!(
        writer.create(task("after")),
        Err(SqliteError::TransactionAborted)
    ));
    assert_eq!(
        tx.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn failed_savepoint_rollback_poison_refuses_with_outer_transaction_still_active() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Savepoint {
            operation: TransactionOperation::Rollback,
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    writer.create(task("t")).unwrap();
    tx.execute_batch("CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON task_details BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        writer.delete("t", Revision::ONE),
        Err(SqliteError::OperationRollback { .. })
    ));
    assert!(!tx.is_autocommit());
    assert!(matches!(
        writer.ensure_committable(),
        Err(SqliteError::TransactionAborted)
    ));
    assert!(matches!(
        writer.create(task("later")),
        Err(SqliteError::TransactionAborted)
    ));
    tx.rollback().unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn native_approval_column_is_independent_of_shared_projection_calculation() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    let mut input = task("t");
    input.labels = vec!["first".into(), "awaiting-approval".into()];
    writer.create(input.clone()).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT awaiting_approval FROM tasks WHERE project_id='p' AND id='t'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    // Damage only the native column: do not reuse the write/read projection helper.
    tx.execute(
        "UPDATE tasks SET awaiting_approval=0 WHERE project_id='p' AND id='t'",
        [],
    )
    .unwrap();
    assert!(matches!(
        sql(&tx, &disk, "p").get("t"),
        Err(SqliteError::TaskCorrupt {
            locator: "awaiting_approval",
            ..
        })
    ));
}

#[test]
fn damaged_position_names_requested_task_and_preserves_bounded_codec_check() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut writer = TaskWrite::new(sql(&tx, &disk, "p"), &caller);
    writer.create(task("t")).unwrap();
    tx.execute("UPDATE tasks SET position=0", []).unwrap();
    assert!(
        matches!(writer.create(task("requested")), Err(SqliteError::TaskCorrupt { task, check: "counter", locator: "position" }) if task == "requested")
    );
    tx.execute("UPDATE tasks SET position=1", []).unwrap();
    tx.execute("UPDATE task_details SET detail_sha256=?1", ["0".repeat(64)])
        .unwrap();
    assert!(
        matches!(sql(&tx, &disk, "p").get("t"), Err(SqliteError::TaskCorrupt { task, check: "detail length or hash", locator: "detail" }) if task == "t")
    );
}
