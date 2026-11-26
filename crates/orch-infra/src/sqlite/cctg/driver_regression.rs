//! Regressions for the private driver's provenance decoding, statement binding
//! and exact-identifier lookup. Synthetic data only; no process or environment.

use super::*;
use orch_core::store::{Entity, Principal};
use rusqlite::Connection;

const P: &str = "p";

fn conn() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    for step in [
        crate::sqlite::migrations::INITIAL_SCHEMA,
        crate::sqlite::migrations::ATTENTION_FACTS,
        crate::sqlite::migrations::TASK_FORMAT,
        crate::sqlite::migrations::AGENT_TEAM_RUN,
        crate::sqlite::migrations::CONTEXT_CONFIG_TEMPLATE_GOAL,
    ] {
        c.execute_batch(step).unwrap();
    }
    c
}
fn owner(identity: &str) -> Principal {
    Principal::Owner {
        identity: identity.into(),
    }
}
fn sql<'a, 'c>(
    tx: &'a Transaction<'c>,
    disk: &'a DiskBlobStore,
    principal: &'a Principal,
) -> NativeSql<'a, 'c> {
    NativeSql::new(tx, disk, P, Some(principal), 1_000)
}
fn ctx(key: &str) -> Entity {
    Entity::Context(ContextEntry {
        key: key.into(),
        value: "v".into(),
        created_at: "2026-11-22T00:00:00.123Z".into(),
        updated_at: "2026-11-22T00:00:01.000Z".into(),
        ttl_ms: None,
        expires_at: None,
    })
}
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}

#[test]
fn a_null_identity_is_kept_apart_from_an_empty_one() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("");
    let mut db = sql(&tx, &disk, &who);
    db.create(ctx("empty")).unwrap();
    db.create(ctx("legacy")).unwrap();
    // A legacy row recorded no identity at all.
    tx.execute(
        "UPDATE context_entries SET set_by_identity=NULL WHERE key='legacy'",
        [],
    )
    .unwrap();
    let provenance = |key| db.provenance(key).unwrap().unwrap();
    assert_eq!(provenance("empty").identity, Some(String::new()));
    assert_eq!(provenance("legacy").identity, None);
    assert_eq!(provenance("legacy").kind, "owner");
    assert_eq!(db.provenance("missing").unwrap(), None);
}

#[test]
fn a_provenance_row_that_is_not_kind_and_text_columns_is_damage() {
    let sound = [text("owner"), text("who"), SqlValue::Null, text("run")];
    let read = |row: &[SqlValue]| decode_provenance(0, row);
    assert_eq!(
        read(&sound).unwrap(),
        Provenance {
            kind: "owner".into(),
            identity: Some("who".into()),
            agent: None,
            run: Some("run".into()),
        }
    );
    let damaged = |row: &[SqlValue], column: &str| {
        assert!(
            matches!(
                read(row),
                Err(NativeError::Corrupt {
                    family: "context",
                    check: "provenance",
                    ref locator,
                    ..
                }) if locator == column
            ),
            "{row:?}"
        );
    };
    let with = |at: usize, value: SqlValue| {
        let mut row = sound.clone();
        row[at] = value;
        row
    };
    // The kind is required; a missing or non-text kind is not "no provenance".
    damaged(&with(0, SqlValue::Null), "set_by_kind");
    damaged(&with(0, SqlValue::Integer(1)), "set_by_kind");
    damaged(&with(0, SqlValue::Blob(b"owner".to_vec())), "set_by_kind");
    damaged(&with(1, SqlValue::Integer(7)), "set_by_identity");
    damaged(&with(2, SqlValue::Real(1.5)), "set_by_agent_id");
    damaged(&with(3, SqlValue::Blob(vec![0])), "set_by_run_id");
    damaged(&sound[..3], "columns");
}

#[test]
fn a_statement_must_bind_every_parameter_once() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let mut db = sql(&tx, &disk, &who);
    db.create(ctx("k")).unwrap();
    let update = "UPDATE context_entries SET set_by_agent_id=?1,set_by_run_id=?2 WHERE key='k'";
    // One value for two parameters would bind NULL into the second.
    assert!(matches!(
        db.run(update, &[text("a")]),
        Err(NativeError::Sql(rusqlite::Error::InvalidParameterCount(
            1, 2
        )))
    ));
    assert!(matches!(
        db.run(update, &[text("a"), text("r"), text("extra")]),
        Err(NativeError::Sql(rusqlite::Error::InvalidParameterCount(
            3, 2
        )))
    ));
    assert_eq!(
        db.provenance("k").unwrap().map(|p| (p.agent, p.run)),
        Some((None, None))
    );
    assert_eq!(db.run(update, &[text("a"), text("r")]).unwrap(), 1);
    assert_eq!(
        db.provenance("k").unwrap().map(|p| (p.agent, p.run)),
        Some((Some("a".into()), Some("r".into())))
    );
}

#[test]
fn a_lookup_returns_only_the_identifier_that_was_asked_for() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let mut db = sql(&tx, &disk, &who);
    let long = |c: char| c.to_string().repeat(INLINE_TEXT_MAX_BYTES + 1);
    for id in ["short", &long('x')] {
        db.create(ctx(id)).unwrap();
        let (form, stored) = key(id);
        assert!(db.get(Family::Context, id).unwrap().is_some());
        assert!(
            db.lookup(Family::Context, id, form, &stored)
                .unwrap()
                .is_some()
        );
        // A different identifier that reaches the same locator is not this entity.
        // That needs a SHA-256 collision for a digest, so it is reached directly.
        let other = if id == "short" {
            "other".into()
        } else {
            long('y')
        };
        assert!(
            db.lookup(Family::Context, &other, form, &stored)
                .unwrap()
                .is_none(),
            "form {form}"
        );
        assert!(db.get(Family::Context, &other).unwrap().is_none());
    }
}
