//! Issue #209 N1: each test isolates the named stored-row guard.
use super::*;

fn assert_invalid<T>(result: Result<T>) {
    let error = result.err();
    assert!(
        matches!(
            error,
            Some(SqliteError::Store(StoreError::InvalidAttention(_)))
        ),
        "expected stored attention validation failure, got {error:?}"
    );
}

fn seeded() -> Connection {
    let mut conn = db();
    let tx = conn.transaction().unwrap();
    let row = open(&tx, "p1", &dispatch(), 20).unwrap();
    assert_eq!(get(&tx, "p1", 1).unwrap(), Some(row));
    tx.commit().unwrap();
    conn
}

#[test]
fn stored_facts_and_scope_are_validated_on_read() {
    // decode -> facts.validate: metadata still agrees with the typed facts.
    for update in [
        "UPDATE attention SET project_id=''",
        r#"UPDATE attention SET facts_json='{"kind":"dispatch_off_with_work","waiting_tasks":0,"oldest_waiting_at":11}'"#,
    ] {
        let conn = seeded();
        assert_eq!(conn.execute(update, []).unwrap(), 1);
        let scope = if update.contains("project_id") {
            ""
        } else {
            "p1"
        };
        assert_invalid(get(&conn, scope, 1));
        assert_invalid(page(&conn, scope, AttentionFilter::All, None, 1));
    }
}

#[test]
fn stored_noncanonical_id_is_refused_on_page() {
    // get binds canonical "1"; page must decode the stored spelling itself.
    let conn = seeded();
    assert_eq!(conn.execute("UPDATE attention SET id='01'", []).unwrap(), 1);
    assert_invalid(page(&conn, "p1", AttentionFilter::All, None, 1));
}

#[test]
fn stored_author_kind_must_be_daemon() {
    // decode metadata guard: all facts and other author fields stay valid.
    let conn = seeded();
    assert_eq!(
        conn.execute("UPDATE attention SET author_kind='agent'", [])
            .unwrap(),
        1
    );
    assert_invalid(get(&conn, "p1", 1));
}

#[test]
fn stored_acknowledgement_cannot_predate_open() {
    // decode acknowledgement time guard, with a valid paired identity.
    let conn = seeded();
    conn.execute("UPDATE attention SET acked_at=20,acked_by='owner_1'", [])
        .unwrap();
    assert_eq!(
        get(&conn, "p1", 1)
            .unwrap()
            .unwrap()
            .acknowledgement
            .unwrap()
            .at,
        20
    );
    assert_eq!(
        conn.execute("UPDATE attention SET acked_at=19", [])
            .unwrap(),
        1
    );
    assert_invalid(get(&conn, "p1", 1));
}

#[test]
fn stored_resolution_must_be_by_daemon() {
    // decode resolved_by guard, with valid time and kind-specific reason.
    let conn = seeded();
    conn.execute("UPDATE attention SET resolved_at=20,resolved_reason='dispatch_enabled',resolved_by='daemon'", []).unwrap();
    assert!(get(&conn, "p1", 1).unwrap().unwrap().resolution.is_some());
    assert_eq!(
        conn.execute("UPDATE attention SET resolved_by='worker'", [])
            .unwrap(),
        1
    );
    assert_invalid(get(&conn, "p1", 1));
}

#[test]
fn checked_insert_trigger_refuses_noncanonical_id_and_null_facts() {
    // BEFORE INSERT guard, not the reader: each raw insert is otherwise valid.
    for (id, facts) in [("01", Some(json(&dispatch()).unwrap())), ("1", None)] {
        let conn = seeded();
        let before = conn
            .query_row::<i64, _, _>("SELECT count(*) FROM attention", [], |r| r.get(0))
            .unwrap();
        let result = conn.execute(
            "INSERT INTO attention(id,project_id,kind,subject_kind,subject_id,severity,title,opened_at,revision,facts_json) SELECT ?1,'p2',kind,subject_kind,subject_id,severity,title,opened_at,revision,?2 FROM attention WHERE project_id='p1'",
            params![id, facts],
        );
        assert!(
            matches!(result, Err(rusqlite::Error::SqliteFailure(ref error, _)) if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER),
            "expected checked-insert trigger refusal, got {result:?}"
        );
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM attention", [], |r| r.get(0))
                .unwrap(),
            before
        );
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT count(*) FROM counters WHERE project_id='p2'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            0
        );
    }
}
