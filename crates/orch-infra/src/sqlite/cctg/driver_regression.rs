//! Regressions for the private driver's provenance decoding, statement binding,
//! exact-identifier lookup, selection refusals and typed read ordering.
//! Synthetic data only; no process or environment mutation.

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

// Issue 343: the private driver's selection contract precedes public cursor binding.
fn assert_invalid_limit(limit: usize) {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let db = sql(&tx, &disk, &who);
    for filter in [
        Filter::Context,
        Filter::Config,
        Filter::Templates,
        Filter::Goals { status: None },
    ] {
        let result = db.select(
            NativeSelection {
                filter: &filter,
                sort: Sort::Id,
            },
            None,
            limit,
        );
        assert!(
            matches!(result, Err(NativeError::Store(StoreError::InvalidPage(_)))),
            "limit {limit}"
        );
    }
}

#[test]
fn a_zero_page_limit_is_invalid_page() {
    assert_invalid_limit(0);
}

#[test]
fn a_page_limit_over_one_thousand_is_invalid_page() {
    assert_invalid_limit(1001);
}

#[test]
fn created_at_sort_on_each_non_goal_family_is_invalid_page() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let db = sql(&tx, &disk, &who);
    for filter in [Filter::Context, Filter::Config, Filter::Templates] {
        assert!(matches!(
            db.select(
                NativeSelection {
                    filter: &filter,
                    sort: Sort::CreatedAt
                },
                None,
                1
            ),
            Err(NativeError::Store(StoreError::InvalidPage(_)))
        ));
    }
}

#[test]
fn an_anchor_outside_the_four_families_is_unrepresentable() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let db = sql(&tx, &disk, &who);
    let anchor = Record {
        entity: Entity::Message(Box::new(orch_domain::Message {
            id: "msg-anchor".into(),
            channel: orch_domain::MessageChannel::Direct,
            from_agent_id: "sender".into(),
            to_agent_id: None,
            subject: "".into(),
            body: "".into(),
            created_at: "opaque".into(),
            expires_at: None,
            status: orch_domain::MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        })),
        revision: Revision::ONE,
        position: 1,
    };
    for filter in [
        Filter::Context,
        Filter::Config,
        Filter::Templates,
        Filter::Goals { status: None },
    ] {
        let error: crate::sqlite::SqliteError = db
            .select(
                NativeSelection {
                    filter: &filter,
                    sort: Sort::Id,
                },
                Some(&anchor),
                1,
            )
            .err()
            .expect("unsupported anchor")
            .into();
        assert!(
            matches!(error, crate::sqlite::SqliteError::Unrepresentable(_)),
            "{error:?}"
        );
    }
}

#[test]
fn a_second_page_total_counts_matches_before_the_anchor() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let mut db = sql(&tx, &disk, &who);
    for family in [
        Family::Context,
        Family::Config,
        Family::Template,
        Family::Goal,
    ] {
        let filter = match family {
            Family::Context => Filter::Context,
            Family::Config => Filter::Config,
            Family::Template => Filter::Templates,
            Family::Goal => Filter::Goals { status: None },
        };
        for id in ["a", "b", "c"] {
            let entity = match family {
                Family::Context => ctx(id),
                Family::Config => Entity::Config(ConfigEntry {
                    key: id.into(),
                    value: "v".into(),
                }),
                Family::Template => Entity::Template(TemplateEntry {
                    name: id.into(),
                    body: "v".into(),
                }),
                Family::Goal => Entity::Goal(Box::new(Goal {
                    id: id.into(),
                    title: "title".into(),
                    description: "".into(),
                    status: GoalStatus::Active,
                    assignee: None,
                    created_at: "opaque".into(),
                    updated_at: None,
                })),
            };
            db.create(entity).unwrap();
        }
        let first = db
            .select(
                NativeSelection {
                    filter: &filter,
                    sort: Sort::Id,
                },
                None,
                1,
            )
            .unwrap();
        let second = db
            .select(
                NativeSelection {
                    filter: &filter,
                    sort: Sort::Id,
                },
                Some(&first.items[0]),
                1,
            )
            .unwrap();
        assert_eq!(
            (
                second
                    .items
                    .iter()
                    .map(|row| row.entity.id())
                    .collect::<Vec<_>>(),
                second.total,
                second.has_more
            ),
            (vec!["b"], 3, true),
            "{family:?}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn an_inline_type_error_is_corruption_before_an_indirect_object_access_error() {
    let (temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner("o");
    let mut db = sql(&tx, &disk, &who);
    let mut entity = ctx("k");
    let Entity::Context(entry) = &mut entity else {
        unreachable!()
    };
    entry.value = "x".repeat(INLINE_TEXT_MAX_BYTES + 1);
    db.create(entity).unwrap();
    let hash: String = tx
        .query_row(
            "SELECT sha256 FROM entity_blob_refs WHERE locator='/value'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Opening a socket as a blob yields an I/O error even as root. Bind at a
    // short private name, then link the socket inode at the longer blob path.
    let shard = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4]);
    disk.remove(&hash.parse().unwrap()).unwrap();
    let socket_path = temp.path().join("object.sock");
    let _socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::hard_link(&socket_path, shard.join(&hash)).unwrap();
    let access: crate::sqlite::SqliteError = db.get(Family::Context, "k").unwrap_err().into();
    assert!(
        matches!(access, crate::sqlite::SqliteError::EntityAccess(_)),
        "{access:?}"
    );
    let detail: String = tx
        .query_row("SELECT detail FROM entity_details", [], |row| row.get(0))
        .unwrap();
    let mut detail: Value = serde_json::from_str(&detail).unwrap();
    detail["ttl_ms"] = Value::from(-1);
    let detail = serde_json::to_string(&detail).unwrap();
    tx.execute(
        "UPDATE entity_details SET detail=?1,detail_bytes=?2,detail_sha256=?3",
        params![
            detail,
            detail.len() as i64,
            BlobHash::of(detail.as_bytes()).to_string()
        ],
    )
    .unwrap();
    let error: crate::sqlite::SqliteError = db.get(Family::Context, "k").unwrap_err().into();
    assert!(
        matches!(error, crate::sqlite::SqliteError::EntityCorrupt {
        family: "context", form: 0, check: "typed detail", ref locator
    } if locator.is_empty()),
        "{error:?}"
    );
}
