use super::*;
use orch_core::store::{Entity, Principal};
use orch_domain::GOAL_STATUSES;
use rusqlite::Connection;

const P: &str = "p";
const BIG: usize = INLINE_TEXT_MAX_BYTES + 1;

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
fn owner() -> Principal {
    Principal::Owner {
        identity: "owner-1".into(),
    }
}
fn worker() -> Principal {
    Principal::Worker {
        identity: "worker-1".into(),
    }
}
fn task() -> Principal {
    Principal::Task {
        identity: "task-id".into(),
        task: "t1".into(),
        run: "run-9".into(),
        lineage: "lineage".into(),
        epoch: 3,
    }
}
fn sql<'a, 'c>(
    tx: &'a Transaction<'c>,
    disk: &'a DiskBlobStore,
    principal: &'a Principal,
) -> NativeSql<'a, 'c> {
    NativeSql::new(tx, disk, P, Some(principal), 1_000)
}
fn ctx(key: &str, value: &str) -> Entity {
    Entity::Context(ContextEntry {
        key: key.into(),
        value: value.into(),
        created_at: "2026-11-22T00:00:00.123Z".into(),
        updated_at: "2026-11-22T00:00:01.000Z".into(),
        ttl_ms: None,
        expires_at: None,
    })
}
fn cfg(key: &str, value: &str) -> Entity {
    Entity::Config(ConfigEntry {
        key: key.into(),
        value: value.into(),
    })
}
fn tpl(name: &str, body: &str) -> Entity {
    Entity::Template(TemplateEntry {
        name: name.into(),
        body: body.into(),
    })
}
fn goal(id: &str) -> Goal {
    Goal {
        id: id.into(),
        title: format!("title {id}"),
        description: "λ ' ? ;".into(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: "2026-11-22T00:00:00.123Z".into(),
        updated_at: None,
    }
}
fn goal_entity(id: &str) -> Entity {
    Entity::Goal(Box::new(goal(id)))
}
/// The family's entity with its identifier and one oversized or ordinary text.
fn entity(family: Family, id: &str, text: &str) -> Entity {
    match family {
        Family::Context => ctx(id, text),
        Family::Config => cfg(id, text),
        Family::Template => tpl(id, text),
        Family::Goal => Entity::Goal(Box::new(Goal {
            description: text.into(),
            ..goal(id)
        })),
    }
}
const FAMILIES: [Family; 4] = [
    Family::Context,
    Family::Config,
    Family::Template,
    Family::Goal,
];
fn filter(family: Family) -> Filter {
    match family {
        Family::Context => Filter::Context,
        Family::Config => Filter::Config,
        Family::Template => Filter::Templates,
        Family::Goal => Filter::Goals { status: None },
    }
}
fn ids(page: &NativePage) -> Vec<String> {
    page.items
        .iter()
        .map(|r| r.entity.id().to_owned())
        .collect()
}
fn list(sql: &NativeSql<'_, '_>, filter: &Filter, sort: Sort) -> Vec<Record> {
    sql.select(NativeSelection { filter, sort }, None, 1000)
        .unwrap()
        .items
}
/// Walk the whole order one row at a time, as a cursor does.
fn walk(sql: &NativeSql<'_, '_>, filter: &Filter, sort: Sort) -> Vec<String> {
    let mut after: Option<Record> = None;
    let mut seen = Vec::new();
    loop {
        let page = sql
            .select(NativeSelection { filter, sort }, after.as_ref(), 1)
            .unwrap();
        let Some(row) = page.items.into_iter().next() else {
            assert!(!page.has_more);
            return seen;
        };
        seen.push(row.entity.id().to_owned());
        after = Some(row);
    }
}
fn one<T: rusqlite::types::FromSql>(tx: &Transaction<'_>, query: &str) -> T {
    tx.query_row(query, [], |r| r.get(0)).unwrap()
}

#[test]
fn each_family_round_trips_every_field_and_state() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let mut entities = vec![
        ctx("Key", ""),
        Entity::Context(ContextEntry {
            key: "".into(),
            value: "λ\n\u{0}é".into(),
            created_at: "opaque".into(),
            updated_at: "".into(),
            ttl_ms: Some(0),
            expires_at: Some("".into()),
        }),
        cfg("", ""),
        cfg("KeY", "Value 🙂"),
        tpl("", ""),
        tpl("a\0b", "body\r\n"),
    ];
    for status in GOAL_STATUSES {
        for (assignee, updated) in [(None, None), (Some(""), Some("")), (Some("Ag"), Some("x"))] {
            entities.push(Entity::Goal(Box::new(Goal {
                id: format!("{status:?}-{assignee:?}-{updated:?}"),
                status,
                assignee: assignee.map(Into::into),
                updated_at: updated.map(Into::into),
                ..goal("g")
            })));
        }
    }
    for entity in &entities {
        let created = db.create(entity.clone()).unwrap();
        assert_eq!(created.entity, *entity);
        let family = Family::of(entity).unwrap();
        assert_eq!(db.get(family, entity.id()).unwrap(), Some(created));
    }
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    let db = sql(&tx, &disk, &who);
    for entity in entities {
        let family = Family::of(&entity).unwrap();
        let record = db.get(family, entity.id()).unwrap().unwrap();
        assert_eq!((record.entity, record.revision), (entity, Revision::ONE));
    }
}

#[test]
fn ttl_and_expiry_are_independent_and_ttl_is_exact_through_u64_max() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let max = i64::MAX as u64;
    let states: [(Option<u64>, Option<&str>); 8] = [
        (None, None),
        (Some(0), None),
        (None, Some("2026-01-01T00:00:00.000Z")),
        (Some(1), Some("2020-01-01T00:00:00.000Z")),
        (Some(max - 1), None),
        (Some(max), None),
        (Some(max + 1), Some("opaque")),
        (Some(u64::MAX), None),
    ];
    for (i, (ttl_ms, expires_at)) in states.into_iter().enumerate() {
        let entry = ContextEntry {
            key: format!("k{i}"),
            value: "v".into(),
            created_at: "2026-11-22T00:00:00.000Z".into(),
            updated_at: "2026-11-22T00:00:00.000Z".into(),
            ttl_ms,
            expires_at: expires_at.map(Into::into),
        };
        let record = db.create(Entity::Context(entry.clone())).unwrap();
        // Neither field is derived from the other, and nothing filters an
        // expired entry out of a read.
        assert_eq!(record.entity, Entity::Context(entry));
        assert_eq!(
            db.get(Family::Context, &format!("k{i}")).unwrap(),
            Some(record)
        );
        let native: Option<i64> = tx
            .query_row(
                "SELECT ttl_ms FROM context_entries WHERE key=?1",
                [format!("k{i}")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            native,
            ttl_ms.and_then(|v| i64::try_from(v).ok()),
            "{ttl_ms:?}"
        );
    }
    assert_eq!(list(&db, &Filter::Context, Sort::Default).len(), 8);
}

#[test]
fn provenance_is_bound_on_create_and_kept_by_an_update_or_move_from_another_principal() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let (a, b) = (owner(), task());
    let created = sql(&tx, &disk, &a).create(ctx("k", "v")).unwrap();
    let expected = Provenance {
        kind: "owner".into(),
        identity: Some("owner-1".into()),
        agent: None,
        run: None,
    };
    assert_eq!(
        sql(&tx, &disk, &a).provenance("k").unwrap(),
        Some(expected.clone())
    );
    let mut other = sql(&tx, &disk, &b);
    let updated = other.update(ctx("k", "changed"), created.revision).unwrap();
    let moved = other
        .move_to_end(Family::Context, "k", updated.revision)
        .unwrap();
    assert_eq!((moved.revision.get(), moved.position), (3, 2));
    assert_eq!(other.provenance("k").unwrap(), Some(expected));
    // A different caller creating another entry binds its own run and task identity.
    sql(&tx, &disk, &b).create(ctx("k2", "v")).unwrap();
    assert_eq!(
        other.provenance("k2").unwrap(),
        Some(Provenance {
            kind: "task".into(),
            identity: Some("task-id".into()),
            agent: None,
            run: Some("run-9".into()),
        })
    );
    // Evidence of an earlier source is kept byte for byte, whatever its kind.
    tx.execute(
        "UPDATE context_entries SET set_by_kind='agent',set_by_identity=NULL,set_by_agent_id='a-1',set_by_run_id='r-1' WHERE key='k'",
        [],
    )
    .unwrap();
    let current = other.get(Family::Context, "k").unwrap().unwrap();
    other.update(ctx("k", "again"), current.revision).unwrap();
    assert_eq!(
        other.provenance("k").unwrap(),
        Some(Provenance {
            kind: "agent".into(),
            identity: None,
            agent: Some("a-1".into()),
            run: Some("r-1".into()),
        })
    );
}

#[test]
fn an_owner_or_worker_identity_alone_sets_no_agent_or_run() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    for (key, who) in [("o", owner()), ("w", worker()), ("t", task())] {
        sql(&tx, &disk, &who).create(ctx(key, "v")).unwrap();
    }
    let rows: Vec<(String, String, Option<String>, Option<String>)> = tx
        .prepare("SELECT set_by_kind,set_by_identity,set_by_agent_id,set_by_run_id FROM context_entries ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            ("owner".into(), "owner-1".into(), None, None),
            ("task".into(), "task-id".into(), None, Some("run-9".into())),
            ("worker".into(), "worker-1".into(), None, None),
        ]
    );
    // A read-only or goal-only caller cannot create a context entry at all,
    // and what it refuses leaves no row, detail or counter.
    let mut anonymous = NativeSql::new(&tx, &disk, P, None, 0);
    assert!(matches!(
        anonymous.create(ctx("none", "v")),
        Err(NativeError::Provenance)
    ));
    assert_eq!(
        one::<i64>(&tx, "SELECT count(*) FROM context_entries WHERE key='none'"),
        0
    );
    assert_eq!(
        one::<i64>(
            &tx,
            "SELECT value FROM counters WHERE name='position:context'"
        ),
        3
    );
    // An identity over the inline bound is refused before any write.
    let long = Principal::Owner {
        identity: "i".repeat(BIG),
    };
    assert!(matches!(
        sql(&tx, &disk, &long).create(ctx("long", "v")),
        Err(NativeError::Encoding)
    ));
    assert_eq!(
        one::<i64>(
            &tx,
            "SELECT value FROM counters WHERE name='position:context'"
        ),
        3
    );
}

#[test]
fn opaque_and_equivalent_instant_times_are_kept_and_project_only_when_exact() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let exact = orch_core::clock::epoch_ms_from_iso("2026-11-22T00:00:00.123Z").unwrap();
    let cases = [
        ("2026-11-22T00:00:00.123Z", Some(exact)),
        ("2026-11-22T01:00:00.123+01:00", Some(exact)),
        ("2026-11-22T00:00:00.123000Z", Some(exact)),
        ("2026-11-22T00:00:00.1239Z", None),
        ("2026-11-22T00:00:00.123456789Z", None),
        ("opaque", None),
        ("", None),
        ("+010000-01-01T00:00:00.000Z", None),
        ("1969-12-31T23:59:59.999Z", Some(-1)),
        ("0000-01-01T00:00:00.000Z", Some(TIME_MIN_MS)),
        ("0000-01-01T00:00:00.001Z", Some(TIME_MIN_MS + 1)),
        ("-000001-12-31T23:59:59.999Z", None),
        ("-009999-01-01T00:00:00Z", None),
        ("9999-12-30T22:00:00.999Z", Some(253_402_207_200_999)),
    ];
    for (i, (text, ms)) in cases.into_iter().enumerate() {
        let id = format!("c{i}");
        let mut entry = ContextEntry {
            key: id.clone(),
            value: "v".into(),
            created_at: text.into(),
            updated_at: text.into(),
            ttl_ms: None,
            expires_at: Some(text.into()),
        };
        let record = db.create(Entity::Context(entry.clone())).unwrap();
        let native: (Option<i64>, Option<i64>, Option<i64>) = tx
            .query_row(
                "SELECT created_at,updated_at,expires_at FROM context_entries WHERE key=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(native, (ms, ms, ms), "{text}");
        // The original spelling, never the instant, comes back.
        assert_eq!(record.entity, Entity::Context(entry.clone()));
        entry.value = "u".into();
        let again = db
            .update(Entity::Context(entry.clone()), record.revision)
            .unwrap();
        assert_eq!(db.get(Family::Context, &id).unwrap(), Some(again));
        let goal = Goal {
            id,
            created_at: text.into(),
            updated_at: Some(text.into()),
            ..goal("g")
        };
        db.create(Entity::Goal(Box::new(goal.clone()))).unwrap();
        let native: (Option<i64>, Option<i64>) = tx
            .query_row(
                "SELECT created_at,updated_at FROM goals WHERE id=?1",
                [&goal.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(native, (ms, ms), "{text}");
    }
    // Equivalent instants are different keys for the orders: bytes decide.
    let page = db
        .select(
            NativeSelection {
                filter: &Filter::Goals { status: None },
                sort: Sort::CreatedAt,
            },
            None,
            100,
        )
        .unwrap();
    let texts: Vec<_> = page
        .items
        .iter()
        .map(|r| match &r.entity {
            Entity::Goal(g) => g.created_at.clone(),
            _ => unreachable!(),
        })
        .collect();
    let mut sorted = texts.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(texts, sorted);
}

fn by_bytes(mut ids: Vec<String>) -> Vec<String> {
    ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    ids
}

#[test]
fn identifier_and_goal_orders_tie_and_page_on_original_bytes_across_indirect_keys() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let prefix = "x".repeat(INLINE_TEXT_MAX_BYTES);
    // Indirect keys share a 65,536-byte prefix with an inline key: only the
    // original bytes can order them, and their digests order differently.
    let ids: Vec<String> = [
        format!("{prefix}b"),
        prefix.clone(),
        format!("{prefix}a"),
        format!("{prefix}{}", "a".repeat(5)),
        "Z".into(),
        "a".into(),
        "é".into(),
        "".into(),
        "a\0b".into(),
        "y".into(),
    ]
    .to_vec();
    for family in FAMILIES {
        for id in &ids {
            db.create(entity(family, id, "v")).unwrap();
        }
        let expected = by_bytes(ids.clone());
        let f = filter(family);
        // A goal's default order ties on position, which is creation order.
        let default = if family == Family::Goal {
            ids.clone()
        } else {
            expected.clone()
        };
        assert_eq!(walk(&db, &f, Sort::Default), default, "{family:?}");
        assert_eq!(walk(&db, &f, Sort::Id), expected, "{family:?}");
        let all = db
            .select(
                NativeSelection {
                    filter: &f,
                    sort: Sort::Id,
                },
                None,
                1000,
            )
            .unwrap();
        assert_eq!((ids_of(&all), all.total), (expected.clone(), 10));
        // A page of two resumes from the anchor, indirect or inline, at the same place.
        let first = db
            .select(
                NativeSelection {
                    filter: &f,
                    sort: Sort::Id,
                },
                None,
                2,
            )
            .unwrap();
        assert!(first.has_more);
        let rest = db
            .select(
                NativeSelection {
                    filter: &f,
                    sort: Sort::Id,
                },
                first.items.last(),
                100,
            )
            .unwrap();
        assert_eq!([ids_of(&first), ids_of(&rest)].concat(), expected);
    }
    // Goal created-at text order with indirect texts and ties on position then id.
    let goal_at = |id: &str, created: &str| {
        Entity::Goal(Box::new(Goal {
            created_at: created.into(),
            ..goal(id)
        }))
    };
    db.create(goal_at("tie-b", &format!("{prefix}b"))).unwrap();
    db.create(goal_at("tie-a", &format!("{prefix}b"))).unwrap();
    db.create(goal_at("low", &format!("{prefix}a"))).unwrap();
    db.create(goal_at("inline", "9")).unwrap();
    let f = Filter::Goals { status: None };
    let created: Vec<_> = walk(&db, &f, Sort::CreatedAt)
        .into_iter()
        .filter(|id| ["tie-a", "tie-b", "low", "inline"].contains(&id.as_str()))
        .collect();
    // "9" < "x…a" < "x…b" by bytes; the tie is broken by position, not id.
    assert_eq!(created, ["inline", "low", "tie-b", "tie-a"]);
}

fn ids_of(page: &NativePage) -> Vec<String> {
    ids(page)
}

#[test]
fn goal_default_order_puts_a_present_updated_at_before_an_absent_one() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let long = format!("{}z", "z".repeat(INLINE_TEXT_MAX_BYTES));
    for (id, status, updated) in [
        ("abandoned-new", GoalStatus::Abandoned, Some("9")),
        ("achieved-absent", GoalStatus::Achieved, None),
        ("active-absent-2", GoalStatus::Active, None),
        ("active-absent-1", GoalStatus::Active, None),
        ("active-empty", GoalStatus::Active, Some("")),
        ("active-a", GoalStatus::Active, Some("a")),
        ("active-b", GoalStatus::Active, Some("b")),
        ("active-long", GoalStatus::Active, Some(long.as_str())),
        ("paused-a", GoalStatus::Paused, Some("a")),
        ("achieved-a", GoalStatus::Achieved, Some("a")),
    ] {
        db.create(Entity::Goal(Box::new(Goal {
            status,
            updated_at: updated.map(Into::into),
            ..goal(id)
        })))
        .unwrap();
    }
    let all = Filter::Goals { status: None };
    let expected = [
        // Newest original text first, a present empty text last of the present.
        "active-long",
        "active-b",
        "active-a",
        "active-empty",
        "active-absent-2",
        "active-absent-1",
        "paused-a",
        "achieved-a",
        "achieved-absent",
        "abandoned-new",
    ];
    // Absent texts tie, and the tie breaks on position (creation order), not id.
    let expected: Vec<_> = expected.iter().map(|s| s.to_string()).collect();
    assert_eq!(walk(&db, &all, Sort::Default), expected);
    // The filter applies before the limit: a status page holds only that status.
    let paused = Filter::Goals {
        status: Some(GoalStatus::Paused),
    };
    let page = db
        .select(
            NativeSelection {
                filter: &paused,
                sort: Sort::Default,
            },
            None,
            1,
        )
        .unwrap();
    assert_eq!(
        (ids(&page), page.total, page.has_more),
        (vec!["paused-a".to_string()], 1, false)
    );
    let active = Filter::Goals {
        status: Some(GoalStatus::Active),
    };
    let page = db
        .select(
            NativeSelection {
                filter: &active,
                sort: Sort::Default,
            },
            None,
            3,
        )
        .unwrap();
    assert_eq!((page.items.len(), page.total, page.has_more), (3, 6, true));
    // An anchor of another filter, family or an unsupported sort is refused.
    let achieved = page.items[0].clone();
    for (f, sort) in [
        (&paused, Sort::Default),
        (&Filter::Context, Sort::Default),
        (&all, Sort::UpdatedAt),
        (&Filter::Config, Sort::CreatedAt),
    ] {
        assert!(
            db.select(NativeSelection { filter: f, sort }, Some(&achieved), 1)
                .is_err(),
            "{f:?} {sort:?}"
        );
    }
    assert!(
        db.select(
            NativeSelection {
                filter: &all,
                sort: Sort::Default
            },
            None,
            0
        )
        .is_err()
    );
    assert!(
        db.select(
            NativeSelection {
                filter: &Filter::Agents,
                sort: Sort::Default
            },
            None,
            1
        )
        .is_err()
    );
}

#[test]
fn positions_are_monotonic_and_a_delete_never_rewinds_them() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    for family in FAMILIES {
        let a = db.create(entity(family, "a", "1")).unwrap();
        let b = db.create(entity(family, "b", "1")).unwrap();
        assert_eq!((a.position, b.position), (1, 2));
        let changed = db.update(entity(family, "a", "2"), a.revision).unwrap();
        assert_eq!((changed.position, changed.revision.get()), (1, 2));
        db.delete(family, "b", b.revision).unwrap();
        let c = db.create(entity(family, "c", "1")).unwrap();
        assert_eq!(c.position, 3, "{family:?}: a delete must not rewind");
        let moved = db.move_to_end(family, "a", changed.revision).unwrap();
        assert_eq!((moved.position, moved.revision.get()), (4, 3));
        let again = db.create(entity(family, "b", "9")).unwrap();
        assert_eq!((again.position, again.revision), (5, Revision::ONE));
        // Another project and another family keep their own counters.
        let other = NativeSql::new(&tx, &disk, "q", Some(&who), 0)
            .create(entity(family, "a", "x"))
            .unwrap();
        assert_eq!(other.position, 1);
    }
    let counters: Vec<(String, i64)> = tx
        .prepare("SELECT name,value FROM counters WHERE project_id='p' ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        counters,
        [
            "position:config",
            "position:context",
            "position:goal",
            "position:template"
        ]
        .map(|n| (n.to_owned(), 5))
    );
}

#[test]
fn integration_and_publication_columns_survive_a_typed_update_and_move() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let created = db.create(goal_entity("g")).unwrap();
    tx.execute(
        "UPDATE goals SET integration_branch='orch/g',integration_head='abc',published_at=77,published_commit='def',published_by='someone' WHERE id='g'",
        [],
    )
    .unwrap();
    let state = |tx: &Transaction<'_>| -> (String, String, i64, String, String) {
        tx.query_row(
            "SELECT integration_branch,integration_head,published_at,published_commit,published_by FROM goals WHERE id='g'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    };
    let before = state(&tx);
    let updated = db
        .update(
            Entity::Goal(Box::new(Goal {
                status: GoalStatus::Achieved,
                updated_at: Some("later".into()),
                ..goal("g")
            })),
            created.revision,
        )
        .unwrap();
    assert_eq!(state(&tx), before);
    db.move_to_end(Family::Goal, "g", updated.revision).unwrap();
    assert_eq!(state(&tx), before);
}

#[test]
fn the_shared_template_name_scenario_runs_through_the_sqlite_mapping() {
    // Issue 192 H3: T-V4..T-V12 plus a storage-only embedded NUL, each created,
    // fetched by its exact key and walked by the id order one row at a time.
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let names = [
        "",
        "\u{1b}[0m",
        " sp ace ",
        ".",
        "..",
        "A",
        "a",
        "a\0b",
        "a\nb",
        "a\\b",
        "e\u{301}",
        &"x".repeat(252),
        "\u{7f}",
        "\u{85}",
        "é",
        "\u{202e}rtl",
        "\u{feff}x",
    ];
    for family in [Family::Template, Family::Config] {
        for name in names {
            db.create(entity(family, name, &format!("body of {name:?}")))
                .unwrap();
        }
        for name in names {
            let record = db.get(family, name).unwrap().unwrap();
            assert_eq!(
                record.entity,
                entity(family, name, &format!("body of {name:?}"))
            );
        }
        // Case and normalization form are not folded.
        assert!(db.get(family, "B").unwrap().is_none());
        let walked = walk(&db, &filter(family), Sort::Id);
        assert_eq!(
            walked,
            by_bytes(names.iter().map(|s| s.to_string()).collect())
        );
        assert_eq!(walked[0], "");
    }
}

#[test]
fn identities_are_project_scoped_and_families_do_not_alias_each_other() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    for family in FAMILIES {
        sql(&tx, &disk, &who)
            .create(entity(family, "same", "p"))
            .unwrap();
        NativeSql::new(&tx, &disk, "q", Some(&who), 0)
            .create(entity(family, "same", "q"))
            .unwrap();
    }
    for family in FAMILIES {
        let db = sql(&tx, &disk, &who);
        let rows = list(&db, &filter(family), Sort::Default);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity, entity(family, "same", "p"));
    }
    // A literal key that looks like the digest of an indirect key is another row.
    let long = "k".repeat(BIG);
    let digest = BlobHash::of(long.as_bytes()).to_string();
    let mut db = sql(&tx, &disk, &who);
    db.create(ctx(&long, "indirect")).unwrap();
    db.create(ctx(&digest, "literal")).unwrap();
    for (key, value) in [(&long, "indirect"), (&digest, "literal")] {
        assert_eq!(
            db.get(Family::Context, key).unwrap().unwrap().entity,
            ctx(key, value)
        );
    }
    // Deleting one family's or project's row leaves the others.
    let record = db.get(Family::Config, "same").unwrap().unwrap();
    db.delete(Family::Config, "same", record.revision).unwrap();
    assert!(db.get(Family::Config, "same").unwrap().is_none());
    assert!(db.get(Family::Template, "same").unwrap().is_some());
    assert_eq!(
        one::<i64>(
            &tx,
            "SELECT count(*) FROM entity_details WHERE family='config' AND project_id='p'"
        ),
        0
    );
    assert_eq!(
        one::<i64>(
            &tx,
            "SELECT count(*) FROM entity_details WHERE family='config' AND project_id='q'"
        ),
        1
    );
}

/// `text` of exactly `bytes` UTF-8 bytes whose character count is smaller.
fn bytes_of(bytes: usize) -> String {
    format!("{}{}", "é".repeat(bytes / 2), "a".repeat(bytes % 2))
}
/// A family's entity with one member replaced by `text`.
fn with(entity: &Entity, member: &str, text: &str) -> Entity {
    let family = Family::of(entity).unwrap();
    let mut value = payload(entity).unwrap();
    value[member] = text.into();
    typed(family, value).unwrap()
}
fn members(family: Family) -> Vec<&'static str> {
    let (required, optional) = family.members();
    required
        .iter()
        .chain(optional)
        .copied()
        .filter(|m| !family.plain().contains(m))
        .collect()
}
fn full(family: Family) -> Entity {
    match family {
        Family::Context => Entity::Context(ContextEntry {
            ttl_ms: Some(7),
            expires_at: Some("e".into()),
            ..match ctx("k", "v") {
                Entity::Context(c) => c,
                _ => unreachable!(),
            }
        }),
        Family::Config => cfg("k", "v"),
        Family::Template => tpl("k", "v"),
        Family::Goal => Entity::Goal(Box::new(Goal {
            assignee: Some("a".into()),
            updated_at: Some("u".into()),
            ..goal("k")
        })),
    }
}
fn refs(tx: &Transaction<'_>, family: Family) -> Vec<(String, i64, i64)> {
    tx.prepare("SELECT locator,ordinal,size FROM entity_blob_refs WHERE family=?1 ORDER BY locator,ordinal")
        .unwrap()
        .query_map([family.name()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn the_threshold_counts_utf8_bytes_for_every_text_slot_of_each_family() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    for family in FAMILIES {
        for member in members(family) {
            for (size, tagged) in [(INLINE_TEXT_MAX_BYTES, false), (BIG, true)] {
                let text = bytes_of(size);
                assert_eq!(text.len(), size);
                assert!(text.chars().count() < size);
                // Each slot is its own row so a locator count is exact.
                let id = format!("{member}-{size}");
                let base = full(family);
                let mut e = with(&base, member, &text);
                let identity = family.texts()[0].member;
                if member != identity {
                    e = with(&e, identity, &id);
                }
                let mut db = sql(&tx, &disk, &who);
                let record = db.create(e.clone()).unwrap();
                assert_eq!(record.entity, e, "{family:?} {member} {size}");
                assert_eq!(db.get(family, e.id()).unwrap().unwrap().entity, e);
                let locator = format!("/{member}");
                let n = refs(&tx, family).iter().filter(|r| r.0 == locator).count();
                assert_eq!(n, usize::from(tagged), "{family:?} {member} {size}");
                // The native copy is the explicit form, never a duplicate of the text.
                if let Some(t) = family.texts().iter().find(|t| t.member == member) {
                    let (form, stored) = key(e.id());
                    let (f, native, n): (i64, Option<String>, Option<i64>) = tx
                        .query_row(
                            &format!(
                                "SELECT {c}_form,{c},{c}_bytes FROM {t} WHERE {i}_form=?1 AND {i}=?2",
                                c = t.column,
                                t = family.table(),
                                i = family.identity()
                            ),
                            params![form, stored],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .unwrap();
                    assert_eq!(f, i64::from(tagged), "{family:?} {member} {size}");
                    if tagged {
                        let held = native.unwrap();
                        assert!(held.is_empty() || (member == identity && held.len() == 64));
                        assert_eq!(n, Some(size as i64));
                    } else {
                        assert_eq!((native.as_deref(), n), (Some(text.as_str()), None));
                    }
                }
            }
        }
    }
}

#[test]
fn the_aggregate_detail_is_indirect_even_when_every_text_fits_inline() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let e = Entity::Context(ContextEntry {
        key: "k".repeat(20_000),
        value: "v".repeat(30_000),
        created_at: "c".repeat(20_000),
        updated_at: "u".into(),
        ttl_ms: Some(u64::MAX),
        expires_at: None,
    });
    db.create(e.clone()).unwrap();
    assert_eq!(
        refs(&tx, Family::Context)
            .iter()
            .map(|r| r.0.as_str())
            .collect::<Vec<_>>(),
        [""]
    );
    let (detail, bytes): (Option<String>, i64) = tx
        .query_row("SELECT detail,detail_bytes FROM entity_details", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!(detail.is_none() && bytes > INLINE_TEXT_MAX_BYTES as i64);
    assert_eq!(
        db.get(Family::Context, &"k".repeat(20_000))
            .unwrap()
            .unwrap()
            .entity,
        e
    );
    // One byte under the same aggregate stays inline.
    let small = Entity::Context(ContextEntry {
        key: "s".into(),
        value: "v".repeat(30_000),
        created_at: "c".into(),
        updated_at: "u".into(),
        ttl_ms: None,
        expires_at: None,
    });
    db.create(small.clone()).unwrap();
    assert_eq!(db.get(Family::Context, "s").unwrap().unwrap().entity, small);
    assert_eq!(refs(&tx, Family::Context).len(), 1);
}

fn corrupt(error: NativeError) -> (&'static str, String) {
    match error {
        NativeError::Corrupt { check, locator, .. } => (check, locator),
        other => panic!("expected stored-data damage: {other:?}"),
    }
}

#[test]
fn a_text_over_one_object_is_ordered_chunks_with_a_whole_hash() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let chunk = MAX_BLOB_BYTES as usize;
    // A two-byte character split by each chunk boundary; the chunks all differ.
    // `repeated_chunks_keep_every_ordinal` stores a repeated chunk.
    let mut body = "a".repeat(chunk - 1);
    body.push('é');
    body.push_str(&"a".repeat(chunk - 1));
    body.push('é');
    body.push_str("tail");
    let e = tpl("big", &body);
    db.create(e.clone()).unwrap();
    let rows = refs(&tx, Family::Template);
    assert_eq!(
        rows,
        [
            ("/body".to_owned(), 0, chunk as i64),
            ("/body".to_owned(), 1, chunk as i64),
            ("/body".to_owned(), 2, 6)
        ]
    );
    assert_eq!(db.get(Family::Template, "big").unwrap().unwrap().entity, e);
    let (first, second): (String, String) = tx
        .query_row(
            "SELECT (SELECT sha256 FROM entity_blob_refs WHERE ordinal=0),(SELECT sha256 FROM entity_blob_refs WHERE ordinal=1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(first, second);
    // Metadata damage is typed and no partial entity escapes, however it is damaged.
    for damage in [
        "UPDATE entity_blob_refs SET ordinal=ordinal+10 WHERE ordinal=2",
        "DELETE FROM entity_blob_refs WHERE ordinal=2",
        "UPDATE entity_blob_refs SET size=5 WHERE ordinal=2",
        "UPDATE entity_blob_refs SET ordinal=CASE ordinal WHEN 0 THEN 1 WHEN 1 THEN 0 ELSE 2 END",
        "UPDATE entity_blob_refs SET sha256=(SELECT sha256 FROM entity_blob_refs WHERE ordinal=0) WHERE ordinal=1",
    ] {
        tx.execute_batch("SAVEPOINT damage").unwrap();
        // Swapping ordinals needs a deferred uniqueness check: use a temporary offset.
        if damage.contains("CASE") {
            tx.execute("UPDATE entity_blob_refs SET ordinal=ordinal+100", [])
                .unwrap();
            tx.execute("UPDATE entity_blob_refs SET ordinal=CASE ordinal WHEN 100 THEN 1 WHEN 101 THEN 0 ELSE 2 END", []).unwrap();
        } else {
            tx.execute_batch(damage).unwrap();
        }
        let error = db.get(Family::Template, "big").unwrap_err();
        assert!(
            matches!(
                error,
                NativeError::Corrupt {
                    family: "template",
                    ..
                }
            ),
            "{damage}: {error:?}"
        );
        tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
            .unwrap();
    }
    // Registration and coverage damage is named, never repaired or skipped.
    let first_chunk = "(SELECT sha256 FROM entity_blob_refs WHERE ordinal=0)";
    for (damage, check) in [
        (format!("DELETE FROM blobs WHERE sha256={first_chunk}"), "blob registration"),
        (format!("UPDATE blobs SET size=size-1 WHERE sha256={first_chunk}"), "blob registration"),
        (
            "INSERT INTO entity_blob_refs SELECT project_id,family,key_form,entity_key,'/name',0,sha256,size FROM entity_blob_refs WHERE ordinal=0".into(),
            "reference locators",
        ),
        ("DELETE FROM entity_blob_refs".into(), "reference metadata"),
    ] {
        tx.execute_batch("SAVEPOINT damage").unwrap();
        tx.execute_batch(&damage).unwrap();
        assert_eq!(corrupt(db.get(Family::Template, "big").unwrap_err()).0, check, "{damage}");
        tx.execute_batch("ROLLBACK TO damage; RELEASE damage").unwrap();
    }
    assert_eq!(db.get(Family::Template, "big").unwrap().unwrap().entity, e);
    // A missing object is damage too, and a whole-hash mismatch cannot hide behind the chunks.
    disk.remove(&first.parse().unwrap()).unwrap();
    let error = db.get(Family::Template, "big").unwrap_err();
    assert!(
        matches!(
            error,
            NativeError::Corrupt {
                check: "referenced bytes",
                ..
            }
        ),
        "{error:?}"
    );
    assert!(matches!(
        db.select(
            NativeSelection {
                filter: &Filter::Templates,
                sort: Sort::Id
            },
            None,
            10
        ),
        Err(NativeError::Corrupt { .. })
    ));
}

#[test]
fn repeated_chunks_keep_every_ordinal() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    let chunk = MAX_BLOB_BYTES as usize;
    // Two byte-identical whole chunks, then a short tail: one object behind two ordinals.
    let repeated = "a".repeat(chunk);
    let body = format!("{repeated}{repeated}tail");
    let e = tpl("rep", &body);
    db.create(e.clone()).unwrap();
    assert_eq!(
        refs(&tx, Family::Template),
        [
            ("/body".to_owned(), 0, chunk as i64),
            ("/body".to_owned(), 1, chunk as i64),
            ("/body".to_owned(), 2, 4)
        ]
    );
    let hashes: Vec<String> = tx
        .prepare("SELECT sha256 FROM entity_blob_refs ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(hashes[0], hashes[1], "the repeat is one hash");
    assert_ne!(hashes[0], hashes[2]);
    assert_eq!(
        one::<i64>(
            &tx,
            "SELECT count(*) FROM blobs WHERE sha256=(SELECT sha256 FROM entity_blob_refs WHERE ordinal=0)"
        ),
        1,
        "one registered object behind both ordinals"
    );
    assert_eq!(one::<i64>(&tx, "SELECT count(*) FROM blobs"), 2);
    // Every ordinal comes back, in order, byte for byte.
    assert_eq!(db.get(Family::Template, "rep").unwrap().unwrap().entity, e);
    // Dropping either ordinal of the repeat is typed damage: the shared object cannot cover for it.
    for ordinal in [0, 1] {
        tx.execute_batch("SAVEPOINT damage").unwrap();
        tx.execute("DELETE FROM entity_blob_refs WHERE ordinal=?1", [ordinal])
            .unwrap();
        assert_eq!(
            one::<i64>(&tx, "SELECT count(*) FROM entity_blob_refs"),
            2,
            "ordinal {ordinal}: the other repeat row remains"
        );
        let error = db.get(Family::Template, "rep").unwrap_err();
        assert!(
            matches!(
                error,
                NativeError::Corrupt {
                    family: "template",
                    ..
                }
            ),
            "ordinal {ordinal}: {error:?}"
        );
        tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
            .unwrap();
    }
    assert_eq!(db.get(Family::Template, "rep").unwrap().unwrap().entity, e);
    // Losing the one shared object is damage at the first ordinal that needs it.
    let shared: String = hashes[0].clone();
    disk.remove(&shared.parse().unwrap()).unwrap();
    assert_eq!(
        corrupt(db.get(Family::Template, "rep").unwrap_err()).0,
        "referenced bytes"
    );
}

#[test]
fn projection_drift_in_each_column_is_a_stored_data_error() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    for family in FAMILIES {
        sql(&tx, &disk, &who).create(full(family)).unwrap();
        let identity = family.identity();
        for column in family.columns() {
            if column.ends_with("_form")
                || column.ends_with("_sha256")
                || column.ends_with("_bytes")
                || column == identity
            {
                continue;
            }
            // Text columns take another text; numbers move by one or appear from NULL.
            let drift = if column == "status" {
                "UPDATE goals SET status='paused'".to_owned()
            } else {
                format!(
                    "UPDATE {t} SET {c}=CASE typeof({c}) WHEN 'text' THEN {c}||'!' WHEN 'integer' THEN {c}+1 ELSE 5 END",
                    t = family.table(),
                    c = column
                )
            };
            tx.execute_batch("SAVEPOINT d").unwrap();
            tx.execute_batch(&drift).unwrap();
            let error = sql(&tx, &disk, &who).get(family, "k").unwrap_err();
            let (check, locator) = corrupt(error);
            assert_eq!(
                (check, locator.as_str()),
                ("projection", column.as_str()),
                "{family:?}"
            );
            assert!(
                sql(&tx, &disk, &who)
                    .select(
                        NativeSelection {
                            filter: &filter(family),
                            sort: Sort::Id
                        },
                        None,
                        1
                    )
                    .is_err()
            );
            tx.execute_batch("ROLLBACK TO d; RELEASE d").unwrap();
        }
        // Counters and identity cannot be repaired by the reader either.
        tx.execute_batch("SAVEPOINT d").unwrap();
        tx.execute_batch(&format!("UPDATE {} SET revision=0", family.table()))
            .unwrap_or(());
        tx.execute_batch("ROLLBACK TO d; RELEASE d").unwrap();
        assert!(sql(&tx, &disk, &who).get(family, "k").unwrap().is_some());
    }
}

#[test]
fn detail_damage_and_a_detail_that_disagrees_with_its_row_are_stored_data_errors() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    db.create(ctx("k", "v")).unwrap();
    let forge = |detail: &str| {
        tx.execute(
            "UPDATE entity_details SET detail=?1,detail_bytes=length(CAST(?1 AS BLOB)),detail_sha256=?2",
            params![detail, BlobHash::of(detail.as_bytes()).to_string()],
        )
        .unwrap();
    };
    let good = r#"{"key":"k","value":"v","created_at":"2026-11-22T00:00:00.123Z","updated_at":"2026-11-22T00:00:01.000Z"}"#;
    for (detail, check) in [
        // A valid but different value: the projections no longer match the detail.
        (good.replace(r#""v""#, r#""w""#), "projection"),
        (good.replace(r#""k""#, r#""other""#), "identity projection"),
        (
            good.replace(r#"{"key""#, r#"{"extra":1,"key""#),
            "object fields",
        ),
        (good.replace(r#""value":"v","#, ""), "object fields"),
        (good.replace(r#""v""#, "null"), "object fields"),
        (
            good.replace(r#""value":"v""#, r#""value":"v","value":"w""#),
            "strict JSON",
        ),
        (
            good.replace(r#""updated_at""#, r#""ttl_ms":1.5,"updated_at""#),
            "typed detail",
        ),
        (
            good.replace(r#""updated_at""#, r#""ttl_ms":-1,"updated_at""#),
            "typed detail",
        ),
        (good.replace(r#""v""#, "7"), "reference tag"),
        ("[]".into(), "object type"),
        ("".into(), "strict JSON"),
    ] {
        forge(&detail);
        let error = db.get(Family::Context, "k").unwrap_err();
        let (actual, _) = corrupt(error);
        assert!(
            actual == check || (check == "text type" && actual == "typed detail"),
            "{detail}: {actual}"
        );
    }
    forge(good);
    assert!(db.get(Family::Context, "k").unwrap().is_some());
    tx.execute(
        "UPDATE entity_details SET detail_sha256=?1",
        ["0".repeat(64)],
    )
    .unwrap();
    assert_eq!(
        corrupt(db.get(Family::Context, "k").unwrap_err()).0,
        "detail length or hash"
    );
    forge(good);
    tx.execute("UPDATE entity_details SET format=2", [])
        .unwrap_or(0);
    tx.execute("DELETE FROM entity_details", []).unwrap();
    assert_eq!(
        corrupt(db.get(Family::Context, "k").unwrap_err()).0,
        "detail missing"
    );
}

#[test]
fn revision_races_and_exhausted_counters_refuse_before_any_change() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    for family in FAMILIES {
        let first = db.create(entity(family, "k", "1")).unwrap();
        let stale = Revision::ONE.next().unwrap();
        assert!(matches!(
            db.update(entity(family, "k", "2"), stale),
            Err(NativeError::Store(StoreError::StaleRevision { .. }))
        ));
        assert!(matches!(
            db.delete(family, "k", stale),
            Err(NativeError::Store(StoreError::StaleRevision { actual: Some(a), .. })) if a == Revision::ONE
        ));
        assert!(matches!(
            db.move_to_end(family, "missing", Revision::ONE),
            Err(NativeError::Store(StoreError::StaleRevision {
                actual: None,
                ..
            }))
        ));
        assert!(matches!(
            db.create(entity(family, "k", "dup")),
            Err(NativeError::Store(StoreError::AlreadyExists { .. }))
        ));
        // Two writers racing on one revision: the second loses.
        let winner = db.update(entity(family, "k", "w"), first.revision).unwrap();
        assert!(matches!(
            db.update(entity(family, "k", "late"), first.revision),
            Err(NativeError::Store(StoreError::StaleRevision { .. }))
        ));
        assert_eq!(db.get(family, "k").unwrap().unwrap(), winner);
        // The signed column bound: the next revision or position would not fit.
        tx.execute_batch(&format!(
            "UPDATE {} SET revision={}",
            family.table(),
            i64::MAX
        ))
        .unwrap();
        let max: Revision = serde_json::from_value(i64::MAX.into()).unwrap();
        for op in 0..3 {
            let error = match op {
                0 => db.update(entity(family, "k", "x"), max).unwrap_err(),
                1 => db.move_to_end(family, "k", max).unwrap_err(),
                _ => {
                    tx.execute(
                        "UPDATE counters SET value=?1 WHERE name=?2",
                        params![i64::MAX, family.counter()],
                    )
                    .unwrap();
                    db.create(entity(family, "next", "x")).unwrap_err()
                }
            };
            assert!(
                matches!(error, NativeError::Store(StoreError::CounterExhausted)),
                "{family:?} {op}: {error:?}"
            );
        }
        assert!(db.get(family, "next").unwrap().is_none());
        assert_eq!(db.get(family, "k").unwrap().unwrap().revision, max);
    }
}

#[test]
fn damaged_counters_and_orphan_rows_are_refused_without_mutation() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    for family in FAMILIES {
        db.create(entity(family, "a", "1")).unwrap();
        db.create(entity(family, "b", "1")).unwrap();
        let counter = |db: &mut NativeSql<'_, '_>| db.create(entity(family, "c", "x")).unwrap_err();
        let set = |value: Option<i64>| {
            tx.execute(
                "DELETE FROM counters WHERE project_id='p' AND name=?1",
                [family.counter()],
            )
            .unwrap();
            if let Some(value) = value {
                tx.execute_batch(&format!(
                    "INSERT INTO counters(project_id,name,value) VALUES('p','{}',{value})",
                    family.counter()
                ))
                .unwrap();
            }
        };
        set(None);
        assert!(
            matches!(counter(&mut db), NativeError::Counter(e) if matches!(*e, crate::sqlite::SqliteError::CounterMissing { .. }))
        );
        set(Some(-1));
        assert!(
            matches!(counter(&mut db), NativeError::Counter(e) if matches!(*e, crate::sqlite::SqliteError::CounterNegative { .. }))
        );
        set(Some(1));
        assert!(
            matches!(counter(&mut db), NativeError::Counter(e) if matches!(*e, crate::sqlite::SqliteError::CounterBehind { .. }))
        );
        set(Some(2));
        assert!(db.get(family, "c").unwrap().is_none());
        // A stale detail row without a native row is not adopted by a new entity.
        let orphan = entity(family, "ghost", "x");
        let detail = encode(&disk, &orphan).unwrap();
        let (form, key) = key("ghost");
        tx.execute(
            "INSERT INTO entity_details VALUES('p',?1,?2,?3,1,?4,?5,?6)",
            params![
                family.name(),
                form,
                key,
                detail.inline,
                detail.bytes as i64,
                detail.sha256
            ],
        )
        .unwrap();
        assert_eq!(corrupt(db.create(orphan).unwrap_err()).0, "orphan row");
        assert_eq!(
            one::<i64>(
                &tx,
                &format!(
                    "SELECT value FROM counters WHERE name='{}'",
                    family.counter()
                )
            ),
            2
        );
    }
}

#[test]
fn a_failed_step_leaves_the_old_row_and_references_readable_even_when_caught() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = sql(&tx, &disk, &who);
    for family in FAMILIES {
        let old = entity(family, "k", &"o".repeat(BIG));
        let created = db.create(old.clone()).unwrap();
        let state = |tx: &Transaction<'_>| -> Vec<Vec<String>> {
            ["entity_details", "entity_blob_refs", "blobs", "counters"]
                .iter()
                .map(|t| {
                    tx.prepare(&format!("SELECT * FROM {t} ORDER BY 1,2,3"))
                        .unwrap()
                        .query_map([], |r| {
                            Ok((0..r.as_ref().column_count())
                                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                                .collect::<String>())
                        })
                        .unwrap()
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .unwrap()
                })
                .collect()
        };
        let before = (state(&tx), db.get(family, "k").unwrap().unwrap());
        // The reference rows fail in the middle of an update, create and move.
        tx.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON entity_blob_refs BEGIN SELECT RAISE(ABORT,'synthetic'); END").unwrap();
        assert!(matches!(
            db.update(entity(family, "k", &"n".repeat(BIG)), created.revision),
            Err(NativeError::Sql(_))
        ));
        assert!(matches!(
            db.create(entity(family, "new", &"n".repeat(BIG))),
            Err(NativeError::Sql(_))
        ));
        tx.execute_batch("DROP TRIGGER fail").unwrap();
        assert_eq!((state(&tx), db.get(family, "k").unwrap().unwrap()), before);
        assert!(db.get(family, "new").unwrap().is_none());
        // The unit goes on: a later operation succeeds and nothing was consumed.
        let later = db
            .update(entity(family, "k", "small"), created.revision)
            .unwrap();
        assert_eq!(later.entity, entity(family, "k", "small"));
        assert_eq!(db.create(entity(family, "new", "x")).unwrap().position, 2);
        db.delete(family, "new", Revision::ONE).unwrap();
        db.delete(family, "k", later.revision).unwrap();
    }
    // A failure that ends the whole transaction poisons it for later work.
    tx.execute_batch("CREATE TRIGGER abort BEFORE INSERT ON project_config WHEN NEW.key='fatal' BEGIN SELECT RAISE(ROLLBACK,'synthetic'); END").unwrap();
    assert!(matches!(
        db.create(cfg("fatal", "x")),
        Err(NativeError::Rollback { .. })
    ));
    assert!(matches!(
        db.create(cfg("after", "x")),
        Err(NativeError::Aborted)
    ));
    assert!(matches!(
        db.get(Family::Config, "after"),
        Err(NativeError::Aborted)
    ));
}

#[test]
fn snapshot_and_unit_of_work_hydrate_one_state() {
    let (temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let path = temp.path().join("snapshot.db");
    {
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        for step in [
            crate::sqlite::migrations::INITIAL_SCHEMA,
            crate::sqlite::migrations::ATTENTION_FACTS,
            crate::sqlite::migrations::TASK_FORMAT,
            crate::sqlite::migrations::AGENT_TEAM_RUN,
            crate::sqlite::migrations::CONTEXT_CONFIG_TEMPLATE_GOAL,
        ] {
            c.execute_batch(step).unwrap();
        }
    }
    let who = owner();
    let mut writer = Connection::open(&path).unwrap();
    let mut reader = Connection::open(&path).unwrap();
    let tx = writer.transaction().unwrap();
    for family in FAMILIES {
        sql(&tx, &disk, &who)
            .create(entity(family, "a", "1"))
            .unwrap();
    }
    // The unit's own uncommitted writes are visible to it and to no one else.
    assert!(list(&sql(&tx, &disk, &who), &Filter::Config, Sort::Id).len() == 1);
    let snapshot = reader.transaction().unwrap();
    assert!(
        sql(&snapshot, &disk, &who)
            .get(Family::Config, "a")
            .unwrap()
            .is_none()
    );
    tx.commit().unwrap();
    // The snapshot began before the commit and keeps its state, across every family.
    assert!(
        sql(&snapshot, &disk, &who)
            .get(Family::Config, "a")
            .unwrap()
            .is_none()
    );
    let tx = writer.transaction().unwrap();
    let a = sql(&tx, &disk, &who)
        .get(Family::Template, "a")
        .unwrap()
        .unwrap();
    for family in FAMILIES {
        let mut db = sql(&tx, &disk, &who);
        let r = db.get(family, "a").unwrap().unwrap();
        db.update(entity(family, "a", "2"), r.revision).unwrap();
        db.create(entity(family, "b", "1")).unwrap();
    }
    tx.commit().unwrap();
    for family in FAMILIES {
        assert!(
            sql(&snapshot, &disk, &who)
                .get(family, "a")
                .unwrap()
                .is_none(),
            "{family:?}"
        );
        assert_eq!(
            list(&sql(&snapshot, &disk, &who), &filter(family), Sort::Id).len(),
            0
        );
    }
    drop(snapshot);
    let fresh = reader.transaction().unwrap();
    assert_eq!(
        sql(&fresh, &disk, &who)
            .get(Family::Template, "a")
            .unwrap()
            .unwrap()
            .revision
            .get(),
        a.revision.get() + 1
    );
    for family in FAMILIES {
        assert_eq!(
            walk(&sql(&fresh, &disk, &who), &filter(family), Sort::Id),
            ["a", "b"]
        );
    }
}
