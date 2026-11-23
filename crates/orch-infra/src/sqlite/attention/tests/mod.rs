use super::*;
use crate::sqlite::{ConnectionRole, Synchronous, migrations, open_connection};

fn dispatch() -> AttentionFacts {
    AttentionFacts::DispatchOffWithWork {
        waiting_tasks: 7,
        oldest_waiting_at: 11,
    }
}
fn disk(subject: DiskSubject) -> AttentionFacts {
    AttentionFacts::DiskFloor {
        subject,
        free_bytes: 12,
        floor_bytes: i64::MAX as u64,
    }
}
fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner_1".into(),
        },
    }
}
fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    db.execute_batch(migrations::ATTENTION_FACTS).unwrap();
    db
}

#[test]
fn both_kinds_and_both_disk_subjects_round_trip_after_real_reopen() {
    let (_root, data) = crate::sqlite::test_data();
    let expected = {
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        migrations::migrate(&data, &mut conn).unwrap();
        let tx = conn.transaction().unwrap();
        let mut expected = Vec::new();
        for (scope, facts, reason) in [
            (
                "",
                disk(DiskSubject::DataDirectory),
                ResolutionReason::DiskSpaceRecovered,
            ),
            (
                "",
                disk(DiskSubject::ProjectWorkspaces {
                    project: "p1".into(),
                }),
                ResolutionReason::DiskSpaceRecovered,
            ),
            ("p1", dispatch(), ResolutionReason::NoDispatchableWork),
        ] {
            let row = open(&tx, scope, &facts, 20).unwrap();
            let row = ack(&tx, &owner(scope), row.id, row.revision, 30).unwrap();
            let row = resolve(&tx, scope, row.id, row.revision, reason, 40).unwrap();
            expected.push((scope, row));
        }
        tx.commit().unwrap();
        expected
    };
    let conn = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    for (scope, row) in expected {
        assert_eq!(get(&conn, scope, row.id).unwrap(), Some(row));
    }
    assert!(get(&conn, "p2", 1).unwrap().is_none());
    assert!(get(&conn, "p1", 2).unwrap().is_none());
    assert_eq!(
        page(&conn, "", AttentionFilter::All, None, 1)
            .unwrap()
            .items[0]
            .facts,
        disk(DiskSubject::DataDirectory)
    );
}

#[test]
fn numeric_pages_and_bounded_strict_expiry_cross_nine_ten_eleven() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    conn.execute_batch(migrations::ATTENTION_FACTS).unwrap();
    let tx = conn.transaction().unwrap();
    for id in 1..=11 {
        let row = open(&tx, "p1", &dispatch(), 20).unwrap();
        assert_eq!(row.id, id);
        resolve(
            &tx,
            "p1",
            id,
            row.revision,
            ResolutionReason::DispatchEnabled,
            30,
        )
        .unwrap();
    }
    open(&tx, "p2", &dispatch(), 20).unwrap();
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let p = page(&tx, "p1", AttentionFilter::Resolved, cursor.as_ref(), 3).unwrap();
        ids.extend(p.items.iter().map(|r| r.id));
        cursor = p.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids, (1..=11).collect::<Vec<_>>());
    let cursor = AttentionCursor::after("p1", AttentionFilter::All, 9);
    assert_eq!(
        page(&tx, "p1", AttentionFilter::All, Some(&cursor), 1)
            .unwrap()
            .items[0]
            .id,
        10
    );
    assert!(page(&tx, "p2", AttentionFilter::All, Some(&cursor), 1).is_err());
    assert!(page(&tx, "p1", AttentionFilter::Open, Some(&cursor), 1).is_err());
    assert!(
        page(
            &tx,
            "p1",
            AttentionFilter::All,
            Some(&AttentionCursor::after(
                "p1",
                AttentionFilter::All,
                u64::MAX
            )),
            1
        )
        .unwrap()
        .items
        .is_empty()
    );
    assert_eq!(expire(&tx, "p1", 30, 3).unwrap(), 0);
    for first in [1, 4, 7, 10] {
        assert_eq!(
            expire(&tx, "p1", 31, 3).unwrap(),
            if first == 10 { 2 } else { 3 }
        );
        for id in first..=(first + 2).min(11) {
            assert!(get(&tx, "p1", id).unwrap().is_none());
        }
        if first < 10 {
            assert!(get(&tx, "p1", first + 3).unwrap().is_some());
        }
    }
    assert!(get(&tx, "p2", 1).unwrap().is_some());
    assert_eq!(open(&tx, "p1", &dispatch(), 40).unwrap().id, 12);
    tx.commit().unwrap();
    drop(conn);
    let mut conn = Connection::open(path).unwrap();
    let tx = conn.transaction().unwrap();
    let row = get(&tx, "p1", 12).unwrap().unwrap();
    assert_eq!(row.facts, dispatch());
    resolve(
        &tx,
        "p1",
        row.id,
        row.revision,
        ResolutionReason::DispatchEnabled,
        50,
    )
    .unwrap();
    assert_eq!(open(&tx, "p1", &dispatch(), 60).unwrap().id, 13);
    tx.commit().unwrap();
}

#[test]
fn duplicate_transitions_rollback_and_caught_failed_insert_preserve_counter() {
    let mut conn = db();
    {
        let tx = conn.transaction().unwrap();
        open(&tx, "p1", &dispatch(), 20).unwrap();
    }
    let tx = conn.transaction().unwrap();
    let row = open(&tx, "p1", &dispatch(), 20).unwrap();
    assert_eq!(row.id, 1);
    let before = tx.total_changes();
    assert_eq!(open(&tx, "p1", &dispatch(), 21).unwrap(), row);
    assert_eq!(before, tx.total_changes());
    let row = ack(&tx, &owner("p1"), 1, row.revision, 30).unwrap();
    assert_eq!(ack(&tx, &owner("p1"), 1, row.revision, 31).unwrap(), row);
    assert!(
        resolve(
            &tx,
            "p1",
            1,
            row.revision,
            ResolutionReason::DiskSpaceRecovered,
            40
        )
        .is_err()
    );
    assert!(ack(&tx, &owner("p1"), 1, Revision::ONE, 31).is_err());
    let row = resolve(
        &tx,
        "p1",
        1,
        row.revision,
        ResolutionReason::DispatchEnabled,
        40,
    )
    .unwrap();
    assert_eq!(
        resolve(
            &tx,
            "p1",
            1,
            row.revision,
            ResolutionReason::DispatchEnabled,
            41
        )
        .unwrap(),
        row
    );
    tx.execute_batch("CREATE TRIGGER fail_attention AFTER UPDATE ON counters BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(open(&tx, "p1", &dispatch(), 50).is_err());
    tx.execute_batch("DROP TRIGGER fail_attention").unwrap();
    assert_eq!(open(&tx, "p1", &dispatch(), 50).unwrap().id, 2);
    tx.commit().unwrap();
}

#[test]
fn malformed_facts_scope_metadata_and_boundaries_are_refused() {
    let mut conn = db();
    let tx = conn.transaction().unwrap();
    assert!(open(&tx, "", &dispatch(), 20).is_err());
    assert!(open(&tx, "p1", &disk(DiskSubject::DataDirectory), 20).is_err());
    open(&tx, "p1", &dispatch(), 20).unwrap();
    for text in [
        "{}",
        r#"{"kind":"dispatch_off_with_work","waiting_tasks":7,"oldest_waiting_at":11,"title":"injected"}"#,
    ] {
        tx.execute("UPDATE attention SET facts_json=?1", [text])
            .unwrap();
        assert!(get(&tx, "p1", 1).is_err());
    }
    tx.execute(
        "UPDATE attention SET facts_json=?1,subject_kind='data_directory'",
        [json(&dispatch()).unwrap()],
    )
    .unwrap();
    assert!(get(&tx, "p1", 1).is_err());
    for limit in [0, 1001] {
        assert!(page(&tx, "p1", AttentionFilter::All, None, limit).is_err());
        assert!(expire(&tx, "p1", 50, limit).is_err());
    }
    assert!(expire(&tx, "p1", -1, 1).is_err());
    tx.execute(
        "UPDATE counters SET value=?1 WHERE project_id='p2'",
        [i64::MAX],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO counters VALUES('p2','attention:id',?1)",
        [i64::MAX],
    )
    .unwrap();
    assert!(matches!(
        open(&tx, "p2", &dispatch(), 20),
        Err(SqliteError::Store(StoreError::CounterExhausted))
    ));
}

#[test]
fn legacy_rows_without_typed_facts_are_preserved_and_refused() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    conn.execute("INSERT INTO attention(id,project_id,kind,subject_kind,subject_id,severity,title,opened_at,revision) VALUES('1','p1','dispatch_off_with_work','dispatch','dispatch','warning','legacy',20,1)", []).unwrap();
    conn.execute_batch(migrations::ATTENTION_FACTS).unwrap();
    assert!(
        get(&conn, "p1", 1)
            .unwrap_err()
            .to_string()
            .contains("typed facts missing")
    );
    assert!(page(&conn, "p1", AttentionFilter::All, None, 1).is_err());
    assert_eq!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM attention", [], |r| r.get(0))
            .unwrap(),
        1
    );
}
