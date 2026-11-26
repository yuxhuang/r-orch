//! Pages that resume after a recorded ordering tuple (D2, plan step 3): the
//! leaf half of the vectors P-F1, P-F2 and Q8 of the spec's "Cursors of the
//! eight families other than Task". The cursor, the held tuple and the
//! lookup arm are other steps, so none of those vectors is closed here.
use super::*;
use crate::sqlite::retry_message::message::OrderKey;
use orch_core::store::StoreError;
use std::borrow::Cow;

fn stamped(id: &str, created_at: &str) -> Message {
    let mut m = message(id);
    m.created_at = created_at.into();
    m
}

fn key(created_at: &str, position: u64, id: &str) -> OrderKey<'static> {
    OrderKey::Default {
        created_at: Cow::Owned(created_at.into()),
        position,
        id: Cow::Owned(id.into()),
    }
}

fn after(
    rig: &Rig,
    filter: &Filter,
    sort: Sort,
    key: Option<&OrderKey<'_>>,
    limit: usize,
) -> (Vec<String>, u64, bool) {
    let page = rig
        .reader("p")
        .select_after(MessageSelection { filter, sort }, key, limit)
        .unwrap();
    let ids = page.items.iter().map(|r| id_of(r).to_owned()).collect();
    (ids, page.total, page.has_more)
}

/// The tuple a page ending on `anchor` records, taken before anything changes.
fn recorded(filter: &Filter, sort: Sort, anchor: &Record) -> OrderKey<'static> {
    let selection = MessageSelection { filter, sort };
    match selection.key_of(anchor).unwrap() {
        OrderKey::Id(id) => OrderKey::Id(Cow::Owned(id.into_owned())),
        OrderKey::Default {
            created_at,
            position,
            id,
        } => key(&created_at, position, &id),
    }
}

#[test]
fn a_walk_by_recorded_tuples_equals_the_walk_by_anchor_records_in_every_order() {
    let mut rig = rig();
    // Ties on created_at are decided by position, and equal strings never by id.
    for (id, at) in [
        ("m3", "2026-11-20T10:00:00Z"),
        ("m1", "2026-11-20T10:00:00Z"),
        ("m2", "2026-11-20T09:00:00Z"),
        ("m4", "2026-11-20T10:00:00Z"),
        ("m5", "opaque"),
    ] {
        rig.create("p", &stamped(id, at));
    }
    let all = Filter::Messages { recipient: None };
    for sort in [Sort::Default, Sort::Id] {
        let whole = rig.ids(&all, sort, None, 100).0;
        for limit in 1..=3 {
            let mut seen = Vec::new();
            let mut anchor: Option<Record> = None;
            loop {
                let tuple = anchor.as_ref().map(|a| recorded(&all, sort, a));
                let (ids, total, more) = after(&rig, &all, sort, tuple.as_ref(), limit);
                // The record-taking entry and the tuple-taking entry agree.
                assert_eq!(
                    ids,
                    rig.ids(&all, sort, anchor.as_ref(), limit).0,
                    "{sort:?} {limit}"
                );
                // The total counts every match, not the rows after the tuple.
                assert_eq!(total, 5);
                seen.extend(ids.clone());
                if !more {
                    break;
                }
                anchor = rig.get("p", ids.last().unwrap());
            }
            assert_eq!(seen, whole, "{sort:?} {limit}");
        }
    }
    assert_eq!(
        rig.ids(&all, Sort::Default, None, 100).0,
        ["m2", "m3", "m1", "m4", "m5"]
    );
}

#[test]
fn rows_tied_on_created_at_and_position_order_resume_exactly_after_a_deleted_anchor() {
    let mut rig = rig();
    for id in ["a", "b", "c", "d", "e"] {
        rig.create("p", &stamped(id, "2026-11-20T10:00:00Z"));
    }
    let all = Filter::Messages { recipient: None };
    let (page, ..) = rig.ids(&all, Sort::Default, None, 2);
    assert_eq!(page, ["a", "b"]);
    let tuple = recorded(&all, Sort::Default, &rig.get("p", "b").unwrap());
    assert_eq!(tuple, key("2026-11-20T10:00:00Z", 2, "b"));
    // The anchor and the next row, which tie with it, are both deleted.
    for id in ["b", "c"] {
        let revision = rig.get("p", id).unwrap().revision;
        rig.commit("p", |m| m.delete(id, revision).unwrap());
    }
    let (rest, total, more) = after(&rig, &all, Sort::Default, Some(&tuple), 10);
    assert_eq!(
        (rest.as_slice(), total, more),
        (&["d".into(), "e".into()][..], 3, false)
    );
}

#[test]
fn a_tuple_that_names_no_row_resumes_at_its_place_by_bytes() {
    let mut rig = rig();
    for (id, at) in [("a", "A"), ("b", "Z"), ("c", "z"), ("d", "é")] {
        rig.create("p", &stamped(id, at));
    }
    let all = Filter::Messages { recipient: None };
    // Bytes, not locale or character count: "Z" < "z" < "é" (0xC3 0xA9).
    assert_eq!(
        rig.ids(&all, Sort::Default, None, 10).0,
        ["a", "b", "c", "d"]
    );
    for (k, expected) in [
        (key("Z", 0, "zzz"), vec!["b", "c", "d"]),
        (key("Z", 2, "a"), vec!["b", "c", "d"]),
        (key("Z", 2, "b"), vec!["c", "d"]),
        (key("z", 3, "c"), vec!["d"]),
        (key("é", 4, "d"), vec![]),
        (key("", 0, ""), vec!["a", "b", "c", "d"]),
    ] {
        let (ids, total, more) = after(&rig, &all, Sort::Default, Some(&k), 10);
        assert_eq!(
            (ids, total, more),
            (expected.iter().map(|s| s.to_string()).collect(), 4, false),
            "{k:?}"
        );
    }
    // A key of the id sort compares the identity alone.
    let (ids, ..) = after(
        &rig,
        &all,
        Sort::Id,
        Some(&OrderKey::Id(Cow::Borrowed("b"))),
        10,
    );
    assert_eq!(ids, ["c", "d"]);
}

#[test]
fn the_identity_sort_compares_bytes_and_not_characters() {
    let mut rig = rig();
    // "é" is one character of two bytes: by bytes it sorts after "b", and by
    // characters it would sort before the two-character ids.
    for id in ["b", "é", "ab", "a1"] {
        rig.create("p", &stamped(id, "t"));
    }
    let all = Filter::Messages { recipient: None };
    assert_eq!(rig.ids(&all, Sort::Id, None, 10).0, ["a1", "ab", "b", "é"]);
    for (at, expected) in [
        ("a1", vec!["ab", "b", "é"]),
        ("a2", vec!["ab", "b", "é"]),
        ("ab", vec!["b", "é"]),
        ("b", vec!["é"]),
        ("é", vec![]),
    ] {
        let key = OrderKey::Id(Cow::Borrowed(at));
        let (ids, total, more) = after(&rig, &all, Sort::Id, Some(&key), 10);
        assert_eq!(
            (ids, total, more),
            (expected.iter().map(|s| s.to_string()).collect(), 4, false),
            "{at}"
        );
    }
}

#[test]
fn an_anchor_that_left_the_filter_still_names_its_place() {
    // Q8, leaf half: a pending message is delivered, so it leaves the filter and
    // its ordering values do not change.
    let mut rig = rig();
    for (id, at) in [("a", "01"), ("b", "02"), ("c", "03"), ("d", "04")] {
        rig.create("p", &stamped(id, at));
    }
    let now = "2026-11-20T00:00:00Z";
    let filter = pending("agt_2", now);
    let (first, ..) = rig.ids(&filter, Sort::Default, None, 2);
    assert_eq!(first, ["a", "b"]);
    let anchor = rig.get("p", "b").unwrap();
    let tuple = recorded(&filter, Sort::Default, &anchor);
    let mut delivered = stamped("b", "02");
    delivered.status = MessageStatus::Delivered;
    delivered.delivered_at = Some("2026-11-20T00:00:01Z".into());
    rig.commit("p", |m| m.update(&delivered, anchor.revision, 7).unwrap());
    // The record-taking entry refuses an anchor that no longer matches...
    let refused = rig.reader("p").select(
        MessageSelection {
            filter: &filter,
            sort: Sort::Default,
        },
        rig.get("p", "b").as_ref(),
        10,
    );
    assert!(matches!(
        refused,
        Err(SqliteError::Store(StoreError::InvalidCursor))
    ));
    // ...the tuple-taking entry continues exactly, and counts the matches left.
    let (rest, total, more) = after(&rig, &filter, Sort::Default, Some(&tuple), 10);
    assert_eq!(
        (rest.as_slice(), total, more),
        (&["c".into(), "d".into()][..], 3, false)
    );
    // An anchor that was never pending, expired or sent elsewhere is no different.
    let (rest, ..) = after(
        &rig,
        &filter,
        Sort::Default,
        Some(&key("02", 99, "elsewhere")),
        10,
    );
    assert_eq!(rest, ["c", "d"]);
}

#[test]
fn a_changed_anchor_is_resumed_from_its_original_tuple() {
    let mut rig = rig();
    for (id, at) in [("a", "01"), ("b", "02"), ("c", "03")] {
        rig.create("p", &stamped(id, at));
    }
    let all = Filter::Messages { recipient: None };
    let anchor = rig.get("p", "a").unwrap();
    let tuple = recorded(&all, Sort::Default, &anchor);
    // The anchor is re-timed past the other rows: its new place is after them.
    rig.commit("p", |m| {
        m.update(&stamped("a", "09"), anchor.revision, 7).unwrap()
    });
    let (rest, ..) = after(&rig, &all, Sort::Default, Some(&tuple), 10);
    assert_eq!(rest, ["b", "c", "a"]);
    // Moved to the end of the creation order, the anchor's position changed: its
    // new place is after its original tuple, and the rows that did not change
    // are neither skipped nor repeated.
    let moved = rig.get("p", "b").unwrap();
    let tuple = recorded(&all, Sort::Default, &moved);
    rig.commit("p", |m| m.move_to_end("b", moved.revision).unwrap());
    let (rest, ..) = after(&rig, &all, Sort::Default, Some(&tuple), 10);
    assert_eq!(rest, ["b", "c", "a"]);
}

#[test]
fn a_tuple_of_another_shape_or_a_bad_limit_is_refused_before_any_page() {
    let rig = rig();
    let all = Filter::Messages { recipient: None };
    let run = |sort, k: Option<&OrderKey<'_>>, limit| {
        rig.reader("p")
            .select_after(MessageSelection { filter: &all, sort }, k, limit)
            .map(|_| ())
    };
    let id = OrderKey::Id(Cow::Borrowed("x"));
    let default = key("t", 1, "x");
    assert!(run(Sort::Default, Some(&default), 1).is_ok());
    assert!(run(Sort::Id, Some(&id), 1).is_ok());
    for (sort, k) in [(Sort::Default, &id), (Sort::Id, &default)] {
        assert!(matches!(
            run(sort, Some(k), 1),
            Err(SqliteError::Store(StoreError::InvalidCursor))
        ));
    }
    for limit in [0, 1001] {
        assert!(matches!(
            run(Sort::Default, Some(&id), limit),
            Err(SqliteError::Store(StoreError::InvalidPage(_)))
        ));
    }
    // A sort the family lacks and a filter that is not a message's.
    assert!(matches!(
        run(Sort::Name, None, 1),
        Err(SqliteError::Store(StoreError::InvalidPage(_)))
    ));
    let teams = Filter::Teams;
    assert!(matches!(
        rig.reader("p").select_after(
            MessageSelection {
                filter: &teams,
                sort: Sort::Default
            },
            None,
            1
        ),
        Err(SqliteError::Store(StoreError::InvalidPage(_)))
    ));
}

#[test]
fn hydration_is_in_the_view_the_page_is_selected_from() {
    let mut rig = rig();
    for (id, at) in [("a", "01"), ("b", "02"), ("c", "03")] {
        rig.create("p", &stamped(id, at));
    }
    let all = Filter::Messages { recipient: None };
    let path = rig.temp.path().join("view.db");
    let mut writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    for step in [
        migrations::INITIAL_SCHEMA,
        migrations::ATTENTION_FACTS,
        migrations::TASK_FORMAT,
        migrations::AGENT_TEAM_RUN,
        migrations::CONTEXT_CONFIG_TEMPLATE_GOAL,
        migrations::MESSAGE_FORMAT,
    ] {
        writer.execute_batch(step).unwrap();
    }
    let who = caller("p");
    {
        let tx = writer.transaction().unwrap();
        let mut m = Messages::new(&tx, &who, &rig.blobs);
        for (id, at) in [("a", "01"), ("b", "02"), ("c", "03")] {
            m.create(&stamped(id, at), 1).unwrap();
        }
        tx.commit().unwrap();
    }
    let mut reader = Connection::open(&path).unwrap();
    let snapshot = reader.transaction().unwrap();
    let tuple = key("01", 1, "a");
    let select = |conn: &Connection| {
        MessageReader::new(conn, "p", &rig.blobs)
            .select_after(
                MessageSelection {
                    filter: &all,
                    sort: Sort::Default,
                },
                Some(&tuple),
                10,
            )
            .unwrap()
    };
    let before = select(&snapshot);
    assert_eq!(
        before.items.iter().map(id_of).collect::<Vec<_>>(),
        ["b", "c"]
    );
    // A later commit edits one row, deletes the other and adds a third.
    {
        let tx = writer.transaction().unwrap();
        let mut m = Messages::new(&tx, &who, &rig.blobs);
        let c = m.get("c").unwrap().unwrap();
        let mut edited = stamped("c", "03");
        edited.body = "edited".into();
        m.update(&edited, c.revision, 2).unwrap();
        let b = m.get("b").unwrap().unwrap();
        m.delete("b", b.revision).unwrap();
        m.create(&stamped("d", "04"), 2).unwrap();
        // The writer's own transaction sees its uncommitted state.
        let own = MessageReader::new(&tx, &who.project, &rig.blobs)
            .select_after(
                MessageSelection {
                    filter: &all,
                    sort: Sort::Default,
                },
                Some(&tuple),
                10,
            )
            .unwrap();
        assert_eq!(own.items.iter().map(id_of).collect::<Vec<_>>(), ["c", "d"]);
        tx.commit().unwrap();
    }
    let still = select(&snapshot);
    assert_eq!(still.items, before.items);
    assert_eq!(body_of(&still.items[1]), "body");
    drop(snapshot);
    let fresh = reader.transaction().unwrap();
    let now = select(&fresh);
    assert_eq!(now.items.iter().map(id_of).collect::<Vec<_>>(), ["c", "d"]);
    assert_eq!(body_of(&now.items[0]), "edited");
}

#[test]
fn damage_in_any_candidate_fails_the_page_whatever_the_tuple() {
    let mut rig = rig();
    for (id, at) in [("a", "01"), ("b", "02"), ("c", "03")] {
        rig.create("p", &stamped(id, at));
    }
    rig.conn
        .execute("UPDATE messages SET channel='lead' WHERE id='a'", [])
        .unwrap();
    let all = Filter::Messages { recipient: None };
    // "a" sorts before the tuple and is not returned, but it is hydrated.
    let result = rig.reader("p").select_after(
        MessageSelection {
            filter: &all,
            sort: Sort::Default,
        },
        Some(&key("02", 2, "b")),
        10,
    );
    assert!(matches!(
        result.map(|_| ()),
        Err(SqliteError::MessageCorrupt { .. })
    ));
}
