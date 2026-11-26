//! Pages that resume after a recorded ordering tuple (D2, plan step 3): the
//! leaf half of the vectors P-F1 and N3b of the spec's "Cursors of the eight
//! families other than Task", for Context, Config, Template and Goal. The
//! cursor, the held tuple and the lookup arm (N3a) are other steps, so none of
//! those vectors is closed here. Synthetic data only.

use super::*;
use orch_core::store::{Entity, Principal};
use orch_domain::GOAL_STATUSES;
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
fn owner() -> Principal {
    Principal::Owner {
        identity: "owner".into(),
    }
}
fn goal(id: &str, status: GoalStatus, created_at: &str, updated_at: Option<&str>) -> Goal {
    Goal {
        id: id.into(),
        title: format!("title {id}"),
        description: String::new(),
        status,
        assignee: None,
        created_at: created_at.into(),
        updated_at: updated_at.map(str::to_owned),
    }
}
fn ctx(key: &str) -> Entity {
    Entity::Context(ContextEntry {
        key: key.into(),
        value: "v".into(),
        created_at: "2026-11-22T00:00:00.000Z".into(),
        updated_at: "2026-11-22T00:00:00.000Z".into(),
        ttl_ms: None,
        expires_at: None,
    })
}
fn page(
    db: &NativeSql<'_, '_>,
    q: NativeSelection<'_>,
    k: Option<&OrderKey<'_>>,
    n: usize,
) -> Vec<String> {
    let page = db.select_after(q, k, n).unwrap();
    page.items
        .iter()
        .map(|r| r.entity.id().to_owned())
        .collect()
}
fn ids(page: &NativePage) -> Vec<&str> {
    page.items.iter().map(|r| r.entity.id()).collect()
}
fn q<'a>(filter: &'a Filter, sort: Sort) -> NativeSelection<'a> {
    NativeSelection { filter, sort }
}

#[test]
fn a_goal_walk_by_recorded_tuples_equals_the_walk_by_anchor_records_in_every_order() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
    // Rank, then updated_at (newest first, present before absent, an empty text
    // present), then position, then id: every component decides one pair below.
    for (id, status, created, updated) in [
        ("g1", GoalStatus::Achieved, "c2", Some("u1")),
        ("g2", GoalStatus::Active, "c2", None),
        ("g3", GoalStatus::Active, "c1", Some("")),
        ("g4", GoalStatus::Active, "c2", Some("u2")),
        ("g5", GoalStatus::Active, "c2", Some("u2")),
        ("g6", GoalStatus::Paused, "c3", Some("u9")),
        ("g7", GoalStatus::Active, "c2", None),
    ] {
        db.create(Entity::Goal(Box::new(goal(id, status, created, updated))))
            .unwrap();
    }
    for status in [None, Some(GoalStatus::Active)] {
        let filter = Filter::Goals { status };
        for sort in [Sort::Default, Sort::CreatedAt, Sort::Id] {
            let whole = db.select(q(&filter, sort), None, 1000).unwrap();
            for limit in 1..=3 {
                let mut seen = Vec::new();
                let mut last: Option<Record> = None;
                loop {
                    let key = last.as_ref().map(|r| q(&filter, sort).key_of(r).unwrap());
                    let by_key = db
                        .select_after(q(&filter, sort), key.as_ref(), limit)
                        .unwrap();
                    let by_record = db.select(q(&filter, sort), last.as_ref(), limit).unwrap();
                    assert_eq!(by_key.items, by_record.items, "{status:?} {sort:?} {limit}");
                    assert_eq!(by_key.total, whole.total);
                    seen.extend(by_key.items.iter().cloned());
                    if !by_key.has_more {
                        break;
                    }
                    last = by_key.items.last().cloned();
                }
                assert_eq!(seen, whole.items, "{status:?} {sort:?} {limit}");
            }
        }
    }
    let all = Filter::Goals { status: None };
    let order = db.select(q(&all, Sort::Default), None, 100).unwrap();
    assert_eq!(ids(&order), ["g4", "g5", "g3", "g2", "g7", "g6", "g1"]);
    let order = db.select(q(&all, Sort::CreatedAt), None, 100).unwrap();
    assert_eq!(ids(&order), ["g3", "g1", "g2", "g4", "g5", "g7", "g6"]);
}

#[test]
fn a_goal_that_left_the_filter_still_names_its_place_in_both_orders() {
    // P-F1 and N3b, leaf half: `--status active`, the page ends on goal g, g is
    // achieved, resume. The tuple holds g's original values, so g's new status
    // changes nothing; the record-taking entry refuses the record.
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
    for (id, created) in [("a", "c1"), ("b", "c2"), ("c", "c3"), ("d", "c4")] {
        db.create(Entity::Goal(Box::new(goal(
            id,
            GoalStatus::Active,
            created,
            None,
        ))))
        .unwrap();
    }
    let active = Filter::Goals {
        status: Some(GoalStatus::Active),
    };
    for sort in [Sort::Default, Sort::CreatedAt] {
        let first = db.select(q(&active, sort), None, 2).unwrap();
        assert_eq!(ids(&first), ["a", "b"], "{sort:?}");
        let anchor = first.items.last().unwrap();
        let key = q(&active, sort).key_of(anchor).unwrap();
        // The recorded tuple is the original one.
        assert_eq!(page(&db, q(&active, sort), Some(&key), 10), ["c", "d"]);
        let revision = anchor.revision;
        let achieved = goal("b", GoalStatus::Achieved, "c2", Some("later"));
        let moved = db
            .update(Entity::Goal(Box::new(achieved)), revision)
            .unwrap();
        let refused = db.select(q(&active, sort), Some(&moved), 10);
        assert!(
            matches!(refused, Err(NativeError::Store(StoreError::InvalidCursor))),
            "{sort:?}"
        );
        let rest = db.select_after(q(&active, sort), Some(&key), 10).unwrap();
        assert_eq!(ids(&rest), ["c", "d"], "{sort:?}");
        // The page counts the goals that match now, before the cursor.
        assert_eq!((rest.total, rest.has_more), (3, false));
        // Put it back for the next order.
        let back = goal("b", GoalStatus::Active, "c2", None);
        db.update(Entity::Goal(Box::new(back)), moved.revision)
            .unwrap();
    }
}

#[test]
fn tied_goals_resume_after_a_deleted_anchor_by_position_then_identity() {
    for sort in [Sort::Default, Sort::CreatedAt] {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = conn();
        let tx = conn.transaction().unwrap();
        let who = owner();
        let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
        let tied = |id| Entity::Goal(Box::new(goal(id, GoalStatus::Active, "same", Some("same"))));
        // One status, one updated_at and one created_at: only position and id decide.
        for id in ["e", "d", "c", "b", "a"] {
            db.create(tied(id)).unwrap();
        }
        let all = Filter::Goals { status: None };
        let first = db.select(q(&all, sort), None, 2).unwrap();
        assert_eq!(ids(&first), ["e", "d"], "{sort:?}");
        let key = q(&all, sort).key_of(first.items.last().unwrap()).unwrap();
        // The anchor and the row after it are both gone.
        for id in ["d", "c"] {
            let revision = db.get(Family::Goal, id).unwrap().unwrap().revision;
            db.delete(Family::Goal, id, revision).unwrap();
        }
        assert_eq!(
            page(&db, q(&all, sort), Some(&key), 10),
            ["b", "a"],
            "{sort:?}"
        );
        // Recreated, they are new rows at the end of the position order: after
        // the original tuple, so they are returned there.
        for id in ["d", "c"] {
            db.create(tied(id)).unwrap();
        }
        assert_eq!(
            page(&db, q(&all, sort), Some(&key), 10),
            ["b", "a", "d", "c"],
            "{sort:?}"
        );
    }
}

#[test]
fn identity_only_families_resume_by_bytes_and_ignore_the_sort() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
    // Bytes: "Z" < "a" < "é" (0xC3 0xA9), and a prefix sorts before its extension.
    for key in ["é", "a", "Z", "a1"] {
        db.create(ctx(key)).unwrap();
    }
    for sort in [Sort::Default, Sort::Id] {
        let at = |id: &str| OrderKey::Id(Cow::Owned(id.into()));
        let all = page(&db, q(&Filter::Context, sort), None, 10);
        assert_eq!(all, ["Z", "a", "a1", "é"]);
        for (after, expected) in [
            ("", vec!["Z", "a", "a1", "é"]),
            ("Z", vec!["a", "a1", "é"]),
            ("a", vec!["a1", "é"]),
            // No row has this identity: the tuple still names a place.
            ("a0", vec!["a1", "é"]),
            ("a1", vec!["é"]),
            ("é", vec![]),
        ] {
            let got = page(&db, q(&Filter::Context, sort), Some(&at(after)), 10);
            assert_eq!(got, expected, "{sort:?} after {after:?}");
        }
    }
    // A goal's shapes are not the identity shape, and the reverse.
    let id = OrderKey::Id(Cow::Borrowed("x"));
    let goals = Filter::Goals { status: None };
    for sort in [Sort::Default, Sort::CreatedAt] {
        assert!(matches!(
            db.select_after(q(&goals, sort), Some(&id), 1),
            Err(NativeError::Store(StoreError::InvalidCursor))
        ));
    }
    let created = OrderKey::GoalCreated {
        created_at: Cow::Borrowed("c"),
        position: 1,
        id: Cow::Borrowed("x"),
    };
    let default = OrderKey::GoalDefault {
        rank: 0,
        updated_at: None,
        position: 1,
        id: Cow::Borrowed("x"),
    };
    for (sort, key) in [
        (Sort::Default, &created),
        (Sort::CreatedAt, &default),
        (Sort::Id, &default),
    ] {
        assert!(matches!(
            db.select_after(q(&goals, sort), Some(key), 1),
            Err(NativeError::Store(StoreError::InvalidCursor))
        ));
    }
    for filter in [Filter::Context, Filter::Config, Filter::Templates] {
        assert!(matches!(
            db.select_after(q(&filter, Sort::Default), Some(&default), 1),
            Err(NativeError::Store(StoreError::InvalidCursor))
        ));
    }
    // The limits and the families keep their refusals, and the limit comes first.
    for limit in [0, 1001] {
        assert!(matches!(
            db.select_after(q(&Filter::Context, Sort::Id), Some(&default), limit),
            Err(NativeError::Store(StoreError::InvalidPage(_)))
        ));
    }
    assert!(matches!(
        db.select_after(q(&Filter::Context, Sort::CreatedAt), None, 1),
        Err(NativeError::Store(StoreError::InvalidPage(_)))
    ));
}

#[test]
fn the_goal_tuple_is_compared_component_by_component() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
    for (id, status, updated) in [
        ("p1", GoalStatus::Paused, Some("b")),
        ("a1", GoalStatus::Active, None),
        ("a2", GoalStatus::Active, Some("")),
        ("a3", GoalStatus::Active, Some("b")),
        ("a4", GoalStatus::Active, Some("a")),
    ] {
        db.create(Entity::Goal(Box::new(goal(id, status, "c", updated))))
            .unwrap();
    }
    let all = Filter::Goals { status: None };
    let key =
        |rank: u64, updated_at: Option<&str>, position: u64, id: &str| OrderKey::GoalDefault {
            rank,
            updated_at: updated_at.map(|u| Cow::Owned(u.to_owned())),
            position,
            id: Cow::Owned(id.to_owned()),
        };
    let rank = |s: GoalStatus| GOAL_STATUSES.iter().position(|c| *c == s).unwrap() as u64;
    let active = rank(GoalStatus::Active);
    assert_eq!(
        page(&db, q(&all, Sort::Default), None, 10),
        ["a3", "a4", "a2", "a1", "p1"]
    );
    for (k, expected) in [
        // The rank decides first: nothing of rank 0 follows rank 1.
        (key(rank(GoalStatus::Paused), None, 0, ""), vec![]),
        (
            key(active, Some("zzz"), 0, ""),
            vec!["a3", "a4", "a2", "a1", "p1"],
        ),
        // Newest first: after "b" comes "a", then the empty text, then absent.
        (
            key(active, Some("b"), 4, "a3"),
            vec!["a4", "a2", "a1", "p1"],
        ),
        (key(active, Some("a"), 5, "a4"), vec!["a2", "a1", "p1"]),
        (key(active, Some(""), 3, "a2"), vec!["a1", "p1"]),
        // Absent is its own place, after every present text and before the next rank.
        (key(active, None, 2, "a1"), vec!["p1"]),
        (key(active, None, 0, ""), vec!["a1", "p1"]),
        // Position decides before the identity within an equal updated_at.
        (
            key(active, Some("b"), 4, "zz"),
            vec!["a4", "a2", "a1", "p1"],
        ),
        (
            key(active, Some("b"), 3, "zz"),
            vec!["a3", "a4", "a2", "a1", "p1"],
        ),
    ] {
        assert_eq!(
            page(&db, q(&all, Sort::Default), Some(&k), 10),
            expected,
            "{k:?}"
        );
    }
}

#[test]
fn hydration_is_in_the_view_the_page_is_selected_from() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let path = _temp.path().join("view.sqlite");
    let mut writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    for step in [
        crate::sqlite::migrations::INITIAL_SCHEMA,
        crate::sqlite::migrations::ATTENTION_FACTS,
        crate::sqlite::migrations::TASK_FORMAT,
        crate::sqlite::migrations::AGENT_TEAM_RUN,
        crate::sqlite::migrations::CONTEXT_CONFIG_TEMPLATE_GOAL,
    ] {
        writer.execute_batch(step).unwrap();
    }
    let who = owner();
    {
        let tx = writer.transaction().unwrap();
        let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
        for (id, created) in [("a", "c1"), ("b", "c2"), ("c", "c3")] {
            db.create(Entity::Goal(Box::new(goal(
                id,
                GoalStatus::Active,
                created,
                None,
            ))))
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let mut reader = Connection::open(&path).unwrap();
    let snapshot = reader.transaction().unwrap();
    let active = Filter::Goals {
        status: Some(GoalStatus::Active),
    };
    let key = OrderKey::GoalCreated {
        created_at: Cow::Borrowed("c1"),
        position: 1,
        id: Cow::Borrowed("a"),
    };
    let select = |tx: &Transaction<'_>| {
        NativeSql::new(tx, &disk, P, None, 1_000)
            .select_after(q(&active, Sort::CreatedAt), Some(&key), 10)
            .unwrap()
    };
    let before = select(&snapshot);
    assert_eq!(ids(&before), ["b", "c"]);
    // A later commit achieves one goal, deletes another and adds a third.
    {
        let tx = writer.transaction().unwrap();
        let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
        let b = db.get(Family::Goal, "b").unwrap().unwrap();
        db.update(
            Entity::Goal(Box::new(goal("b", GoalStatus::Achieved, "c2", None))),
            b.revision,
        )
        .unwrap();
        let c = db.get(Family::Goal, "c").unwrap().unwrap();
        db.delete(Family::Goal, "c", c.revision).unwrap();
        db.create(Entity::Goal(Box::new(goal(
            "d",
            GoalStatus::Active,
            "c4",
            None,
        ))))
        .unwrap();
        // The caller's own transaction sees its uncommitted state.
        assert_eq!(
            ids(&db
                .select_after(q(&active, Sort::CreatedAt), Some(&key), 10)
                .unwrap()),
            ["d"]
        );
        tx.commit().unwrap();
    }
    assert_eq!(select(&snapshot).items, before.items);
    drop(snapshot);
    let fresh = reader.transaction().unwrap();
    assert_eq!(ids(&select(&fresh)), ["d"]);
}

#[test]
fn damage_in_any_candidate_fails_the_page_whatever_the_tuple() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let who = owner();
    let mut db = NativeSql::new(&tx, &disk, P, Some(&who), 1_000);
    for key in ["a", "b", "c"] {
        db.create(ctx(key)).unwrap();
    }
    tx.execute("UPDATE context_entries SET value='drift' WHERE key='a'", [])
        .unwrap();
    // "a" sorts before the tuple and is not returned, but it is hydrated.
    let result = db.select_after(
        q(&Filter::Context, Sort::Id),
        Some(&OrderKey::Id(Cow::Borrowed("b"))),
        10,
    );
    assert!(
        matches!(result, Err(NativeError::Corrupt { .. })),
        "{:?}",
        result.map(|_| ())
    );
}
