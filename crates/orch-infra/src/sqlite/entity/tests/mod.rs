use super::*;
use crate::sqlite::migrations::INITIAL_SCHEMA;
use crate::storage::v2::FileStorage;
use orch_core::store::*;
use orch_domain::Goal;

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    orch_core::test_support::assert_hermetic(root.path());
    root
}

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute_batch(INITIAL_SCHEMA).unwrap();
    conn
}

fn row(id: &str) -> GoalRow {
    GoalRow {
        id: id.into(),
        title: format!("Goal {id}"),
        description: "synthetic".into(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: 1795352400000,
        updated_at: None,
        revision: Revision::ONE,
        position: 0,
    }
}

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "local_owner".into(),
        },
    }
}

fn entity(row: &GoalRow) -> Entity {
    fn time(ms: i64) -> String {
        format!("{:.3}", jiff::Timestamp::from_millisecond(ms).unwrap())
    }
    Entity::Goal(Box::new(Goal {
        id: row.id.clone(),
        title: row.title.clone(),
        description: row.description.clone(),
        status: row.status,
        assignee: row.assignee.clone(),
        created_at: time(row.created_at),
        updated_at: row.updated_at.map(time),
    }))
}

fn compare(sql: &GoalRow, file: &Record) {
    assert_eq!(entity(sql), file.entity);
    assert_eq!(sql.revision, file.revision);
    assert_eq!(sql.position, file.position);
}

#[tokio::test]
async fn goal_creation_and_read_match_the_landed_file_reference() {
    let root = root();
    let file = FileStorage::new(root.path());
    let mut conn = connection();
    assert_eq!(get_goal(&conn, "p", "z").unwrap(), None);
    for id in ["z", "a"] {
        let mut input = row(id);
        input.position = 999;
        input.revision = Revision::ONE.next().unwrap();
        let payload = entity(&input);
        let expected = file
            .transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
            .await
            .unwrap();
        let tx = conn.transaction().unwrap();
        let actual = create_goal(&tx, "p", &input).unwrap();
        compare(&actual, &expected);
        tx.commit().unwrap();
        assert_eq!(get_goal(&conn, "p", id).unwrap(), Some(actual));
    }
}

#[tokio::test]
async fn updates_moves_deletes_and_recreation_match_the_file_reference() {
    let root = root();
    let file = FileStorage::new(root.path());
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    let mut rows = Vec::new();
    for id in ["z", "a", "Z"] {
        let input = row(id);
        let payload = entity(&input);
        let expected = file
            .transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
            .await
            .unwrap();
        let actual = create_goal(&tx, "p", &input).unwrap();
        compare(&actual, &expected);
        rows.push(actual);
    }
    let mut changed = rows[0].clone();
    changed.title = "updated ' ? ;".into();
    changed.description = "unicode: λ\nsecond line".into();
    changed.assignee = Some("agent".into());
    changed.updated_at = Some(changed.created_at + 1234);
    // Updates must ignore caller-supplied metadata.
    changed.position = 999;
    changed.revision = Revision::ONE.next().unwrap().next().unwrap();
    for status in [
        GoalStatus::Paused,
        GoalStatus::Achieved,
        GoalStatus::Abandoned,
        GoalStatus::Active,
    ] {
        changed.status = status;
        let payload = entity(&changed);
        let expected_rev = rows[0].revision;
        let expected = file
            .transaction(owner("p"), Lane::Entities, move |u| {
                u.update(payload, expected_rev)
            })
            .await
            .unwrap();
        let actual = update_goal(&tx, "p", &changed, expected_rev).unwrap();
        compare(&actual, &expected);
        assert_eq!(actual.position, 1);
        rows[0] = actual;
    }
    let expected_rev = rows[0].revision;
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.move_to_end(EntityKind::Goal, "z", expected_rev)
        })
        .await
        .unwrap();
    let moved = move_goal_to_end(&tx, "p", "z", expected_rev).unwrap();
    compare(&moved, &expected);
    assert_eq!(moved.position, 4);
    let expected_rev = moved.revision;
    file.transaction(owner("p"), Lane::Entities, move |u| {
        u.delete(EntityKind::Goal, "z", expected_rev)
    })
    .await
    .unwrap();
    delete_goal(&tx, "p", "z", expected_rev).unwrap();
    assert_eq!(get_goal(&tx, "p", "z").unwrap(), None);
    let input = row("z");
    let payload = entity(&input);
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
        .await
        .unwrap();
    let actual = create_goal(&tx, "p", &input).unwrap();
    compare(&actual, &expected);
    assert_eq!((actual.revision, actual.position), (Revision::ONE, 5));
    tx.commit().unwrap();
    assert_eq!(get_goal(&conn, "p", "z").unwrap(), Some(actual));
}

#[tokio::test]
async fn caught_stale_missing_and_duplicate_errors_match_file_and_consume_nothing() {
    let root = root();
    let file = FileStorage::new(root.path());
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    let input = row("g");
    let actual = create_goal(&tx, "p", &input).unwrap();
    let payload = entity(&input);
    file.transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
        .await
        .unwrap();
    for id in ["g", "missing"] {
        let expected_rev = Revision::ONE.next().unwrap();
        for op in 0..3 {
            let input = row(id);
            let payload = entity(&input);
            let id_owned = id.to_string();
            let expected = file
                .transaction(owner("p"), Lane::Entities, move |u| match op {
                    0 => u.update(payload, expected_rev),
                    1 => u.move_to_end(EntityKind::Goal, &id_owned, expected_rev),
                    _ => {
                        u.delete(EntityKind::Goal, &id_owned, expected_rev)?;
                        unreachable!()
                    }
                })
                .await
                .unwrap_err();
            let actual = match op {
                0 => update_goal(&tx, "p", &input, expected_rev),
                1 => move_goal_to_end(&tx, "p", id, expected_rev),
                _ => delete_goal(&tx, "p", id, expected_rev).map(|()| unreachable!()),
            }
            .unwrap_err();
            assert_eq!(actual.to_string(), expected.to_string());
        }
    }
    let payload = entity(&input);
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
        .await
        .unwrap_err();
    assert_eq!(
        create_goal(&tx, "p", &input).unwrap_err().to_string(),
        expected.to_string()
    );
    assert_eq!(get_goal(&tx, "p", "g").unwrap(), Some(actual));
    let input = row("next");
    let payload = entity(&input);
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
        .await
        .unwrap();
    compare(&create_goal(&tx, "p", &input).unwrap(), &expected);
    tx.commit().unwrap();
    assert_eq!(get_goal(&conn, "p", "next").unwrap().unwrap().position, 2);
}

#[tokio::test]
async fn project_keys_isolate_rows_revisions_and_positions_like_file() {
    let root = root();
    let file = FileStorage::new(root.path());
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    for project in ["p", "q' OR 1=1 --"] {
        for id in ["same", "other"] {
            let input = row(id);
            let payload = entity(&input);
            let expected = file
                .transaction(owner(project), Lane::Entities, move |u| u.create(payload))
                .await
                .unwrap();
            compare(&create_goal(&tx, project, &input).unwrap(), &expected);
        }
    }
    let before = get_goal(&tx, "q' OR 1=1 --", "same").unwrap();
    let mut input = row("same");
    input.title = "only p".into();
    let payload = entity(&input);
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.update(payload, Revision::ONE)
        })
        .await
        .unwrap();
    let actual = update_goal(&tx, "p", &input, Revision::ONE).unwrap();
    compare(&actual, &expected);
    delete_goal(&tx, "p", "same", actual.revision).unwrap();
    file.transaction(owner("p"), Lane::Entities, move |u| {
        u.delete(EntityKind::Goal, "same", actual.revision)
    })
    .await
    .unwrap();
    assert_eq!(get_goal(&tx, "q' OR 1=1 --", "same").unwrap(), before);
    assert_eq!(get_goal(&tx, "unknown", "same").unwrap(), None);
    tx.commit().unwrap();
}

#[tokio::test]
async fn outer_rollback_discards_rows_revisions_and_counters_like_file() {
    let root = root();
    let file = FileStorage::new(root.path());
    let mut conn = connection();
    let input = row("kept");
    let tx = conn.transaction().unwrap();
    create_goal(&tx, "p", &input).unwrap();
    tx.commit().unwrap();
    let payload = entity(&input);
    file.transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
        .await
        .unwrap();
    {
        let tx = conn.transaction().unwrap();
        update_goal(&tx, "p", &row("kept"), Revision::ONE).unwrap();
        create_goal(&tx, "p", &row("discarded")).unwrap();
        move_goal_to_end(&tx, "p", "kept", Revision::ONE.next().unwrap()).unwrap();
        delete_goal(&tx, "p", "discarded", Revision::ONE).unwrap();
    }
    let payload = entity(&row("discarded"));
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.update(entity(&row("kept")), Revision::ONE)?;
            u.create(payload)?;
            u.move_to_end(EntityKind::Goal, "kept", Revision::ONE.next()?)?;
            u.delete(EntityKind::Goal, "discarded", Revision::ONE)?;
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(expected.is_err());
    let kept = file
        .read(owner("p"))
        .await
        .unwrap()
        .get(EntityKind::Goal, "kept")
        .await
        .unwrap()
        .unwrap();
    compare(&get_goal(&conn, "p", "kept").unwrap().unwrap(), &kept);
    assert_eq!(get_goal(&conn, "p", "discarded").unwrap(), None);
    let payload = entity(&row("after"));
    let expected = file
        .transaction(owner("p"), Lane::Entities, move |u| u.create(payload))
        .await
        .unwrap();
    let tx = conn.transaction().unwrap();
    compare(&create_goal(&tx, "p", &row("after")).unwrap(), &expected);
    tx.commit().unwrap();
}

#[test]
fn counter_write_failure_undoes_only_the_failed_operation_even_when_caught() {
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    let kept = create_goal(&tx, "p", &row("kept")).unwrap();
    tx.execute_batch(
        "CREATE TEMP TRIGGER refuse_counter BEFORE INSERT ON counters
        BEGIN SELECT RAISE(FAIL, 'injected counter failure'); END;",
    )
    .unwrap();
    assert!(matches!(
        create_goal(&tx, "p", &row("failed")),
        Err(EntityError::Sql(_))
    ));
    assert!(matches!(
        move_goal_to_end(&tx, "p", "kept", kept.revision),
        Err(EntityError::Sql(_))
    ));
    assert_eq!(get_goal(&tx, "p", "kept").unwrap(), Some(kept));
    assert_eq!(get_goal(&tx, "p", "failed").unwrap(), None);
    tx.execute_batch("DROP TRIGGER refuse_counter").unwrap();
    assert_eq!(create_goal(&tx, "p", &row("next")).unwrap().position, 2);
    tx.commit().unwrap();
}

#[test]
fn caught_single_statement_failures_undo_trigger_changes_too() {
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    let kept = create_goal(&tx, "p", &row("kept")).unwrap();
    tx.execute_batch(
        "CREATE TEMP TRIGGER refuse_update BEFORE UPDATE ON goals BEGIN
        INSERT INTO counters(project_id,name,value) VALUES('p','trigger-side-effect',1);
        SELECT RAISE(FAIL, 'injected update failure'); END;
        CREATE TEMP TRIGGER refuse_delete BEFORE DELETE ON goals BEGIN
        INSERT INTO counters(project_id,name,value) VALUES('p','trigger-side-effect',1);
        SELECT RAISE(FAIL, 'injected delete failure'); END;",
    )
    .unwrap();
    assert!(update_goal(&tx, "p", &row("kept"), kept.revision).is_err());
    let effects: i64 = tx
        .query_row(
            "SELECT count(*) FROM counters WHERE name='trigger-side-effect'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(effects, 0);
    assert!(delete_goal(&tx, "p", "kept", kept.revision).is_err());
    let effects: i64 = tx
        .query_row(
            "SELECT count(*) FROM counters WHERE name='trigger-side-effect'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(effects, 0);
    tx.commit().unwrap();
    assert_eq!(get_goal(&conn, "p", "kept").unwrap(), Some(kept));
}

#[test]
fn signed_sqlite_counter_and_revision_limits_are_refused_without_changes() {
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    create_goal(&tx, "p", &row("kept")).unwrap();
    tx.execute(
        "UPDATE counters SET value=?1 WHERE project_id='p'",
        [i64::MAX],
    )
    .unwrap();
    assert!(matches!(
        create_goal(&tx, "p", &row("overflow")),
        Err(EntityError::Store(StoreError::CounterExhausted))
    ));
    assert!(matches!(
        move_goal_to_end(&tx, "p", "kept", Revision::ONE),
        Err(EntityError::Store(StoreError::CounterExhausted))
    ));
    tx.execute(
        "UPDATE goals SET revision=?1 WHERE project_id='p'",
        [i64::MAX],
    )
    .unwrap();
    let kept = get_goal(&tx, "p", "kept").unwrap().unwrap();
    assert!(matches!(
        update_goal(&tx, "p", &row("kept"), kept.revision),
        Err(EntityError::Store(StoreError::CounterExhausted))
    ));
    assert_eq!(get_goal(&tx, "p", "kept").unwrap(), Some(kept));
    assert_eq!(get_goal(&tx, "p", "overflow").unwrap(), None);
    tx.commit().unwrap();
}

#[test]
fn later_phase_columns_survive_updates_and_moves() {
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    create_goal(&tx, "p", &row("g")).unwrap();
    tx.execute_batch(
        "UPDATE goals SET integration_branch='branch',integration_head='head',published_at=123,
        published_commit='commit',published_by='owner' WHERE project_id='p'",
    )
    .unwrap();
    let updated = update_goal(&tx, "p", &row("g"), Revision::ONE).unwrap();
    move_goal_to_end(&tx, "p", "g", updated.revision).unwrap();
    let preserved: (String,String,i64,String,String) = tx.query_row(
        "SELECT integration_branch,integration_head,published_at,published_commit,published_by FROM goals WHERE project_id='p'",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(
        preserved,
        (
            "branch".into(),
            "head".into(),
            123,
            "commit".into(),
            "owner".into()
        )
    );
    tx.commit().unwrap();
}

#[test]
fn invalid_stored_metadata_is_an_error_never_a_missing_row() {
    for column in ["revision", "position"] {
        let mut conn = connection();
        let tx = conn.transaction().unwrap();
        create_goal(&tx, "p", &row("g")).unwrap();
        for value in [0, -1] {
            tx.execute(&format!("UPDATE goals SET {column}=?1"), [value])
                .unwrap();
            assert!(matches!(get_goal(&tx, "p", "g"), Err(EntityError::Sql(_))));
        }
    }
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    create_goal(&tx, "p", &row("g")).unwrap();
    tx.execute_batch("UPDATE goals SET status='unknown'")
        .unwrap();
    assert!(matches!(get_goal(&tx, "p", "g"), Err(EntityError::Sql(_))));
    tx.execute_batch("UPDATE counters SET value=-1").unwrap();
    assert!(matches!(
        create_goal(&tx, "p", &row("new")),
        Err(EntityError::Sql(_))
    ));
}

#[test]
fn committed_rows_and_deleted_positions_survive_reopening_the_database() {
    let root = root();
    let path = root.path().join("state.sqlite");
    {
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch(INITIAL_SCHEMA).unwrap();
        let tx = conn.transaction().unwrap();
        let row = create_goal(&tx, "p", &row("g")).unwrap();
        let row = move_goal_to_end(&tx, "p", "g", row.revision).unwrap();
        delete_goal(&tx, "p", "g", row.revision).unwrap();
        tx.commit().unwrap();
    }
    let mut conn = Connection::open(&path).unwrap();
    assert_eq!(get_goal(&conn, "p", "g").unwrap(), None);
    let tx = conn.transaction().unwrap();
    let created = create_goal(&tx, "p", &row("next")).unwrap();
    assert_eq!((created.position, created.revision), (3, Revision::ONE));
    tx.commit().unwrap();
    drop(conn);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(get_goal(&conn, "p", "next").unwrap(), Some(created));
}
