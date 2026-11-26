//! Order, cursors, revision, position, move and the caught failures.
use super::*;
use orch_core::store::StoreError;

fn stamped(id: &str, created_at: &str) -> Message {
    let mut m = message(id);
    m.created_at = created_at.into();
    m
}

#[test]
fn message_pages_order_on_original_created_at_then_position() {
    let mut rig = rig();
    let long_id = format!("{}-long", "i".repeat(65_540));
    let long_created = format!("2026-11-20T11{}", "0".repeat(65_540));
    // Creation order is position order. Strings compare as bytes: an offset
    // spelling does not sort by its instant, and two equal strings tie on position.
    let rows = [
        stamped("m1", "2026-11-20T10:00:00Z"),
        stamped("m2", "2026-11-20T09:00:00Z"),
        stamped("m3", "2026-11-20T11:00:00+02:00"),
        stamped("m4", "2026-11-20T10:00:00Z"),
        stamped("m5", "2026-11-20T10:00:00.000Z"),
        stamped("m6", "opaque"),
        stamped(&long_id, "2026-11-20T10:00:00Z"),
        stamped("m8", &long_created),
        stamped("m9", ""),
    ];
    for m in &rows {
        rig.create("p", m);
    }
    let filter = Filter::Messages { recipient: None };
    let expected = ["m9", "m2", "m5", "m1", "m4", &long_id, "m8", "m3", "m6"];
    let (all, total, more) = rig.ids(&filter, Sort::Default, None, 100);
    assert_eq!(all, expected);
    assert_eq!((total, more), (9, false));
    // Equal strings tie on position, never on the id.
    assert!(all.iter().position(|i| i == "m1") < all.iter().position(|i| i == "m4"));

    // Walk every page size from every anchor: the cursor continues the same order.
    for limit in 1..=4 {
        let mut seen = Vec::new();
        let mut after: Option<Record> = None;
        loop {
            let page = rig
                .reader("p")
                .select(
                    MessageSelection {
                        filter: &filter,
                        sort: Sort::Default,
                    },
                    after.as_ref(),
                    limit,
                )
                .unwrap();
            // The total counts every match, not the remainder after the cursor.
            assert_eq!(page.total, 9);
            seen.extend(page.items.iter().map(|r| id_of(r).to_owned()));
            match (page.has_more, page.items.last()) {
                (true, Some(last)) => after = Some(last.clone()),
                (false, _) => break,
                (true, None) => panic!("a full page cannot be empty"),
            }
        }
        assert_eq!(seen, expected, "limit {limit}");
    }
    // The id sort is the id alone, as bytes.
    let (by_id, ..) = rig.ids(&filter, Sort::Id, None, 100);
    let mut sorted = expected.map(String::from).to_vec();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(by_id, sorted);
    // A sort the family does not offer is refused.
    let refused = rig.reader("p").select(
        MessageSelection {
            filter: &filter,
            sort: Sort::Position,
        },
        None,
        10,
    );
    assert!(matches!(
        refused,
        Err(SqliteError::Store(StoreError::InvalidPage(_)))
    ));
    // So are a zero limit and another family's filter.
    for (f, limit) in [(&filter, 0), (&Filter::Agents, 10), (&filter, 1001)] {
        let r = rig.reader("p").select(
            MessageSelection {
                filter: f,
                sort: Sort::Default,
            },
            None,
            limit,
        );
        assert!(matches!(
            r,
            Err(SqliteError::Store(StoreError::InvalidPage(_)))
        ));
    }
}

#[test]
fn the_filter_and_order_apply_before_count_cursor_and_limit() {
    let mut rig = rig();
    // Rejected rows come first by creation order, so a page of two must skip them.
    for i in 0..6 {
        let mut m = message(&format!("x{i}"));
        m.to_agent_id = Some("other".into());
        rig.create("p", &m);
    }
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        let mut m = message(id);
        m.created_at = format!("2026-11-20T10:00:0{}Z", 4 - i);
        m.status = if *id == "c" {
            MessageStatus::Delivered
        } else {
            MessageStatus::Pending
        };
        rig.create("p", &m);
    }
    let filter = pending("agt_2", "2026-11-20T00:00:00Z");
    let (first, total, more) = rig.ids(&filter, Sort::Default, None, 2);
    assert_eq!(
        (first.as_slice(), total, more),
        (&["e".to_string(), "d".to_string()][..], 4, true)
    );
    let anchor = rig.get("p", "d").unwrap();
    let (rest, total, more) = rig.ids(&filter, Sort::Default, Some(&anchor), 5);
    assert_eq!(
        (rest, total, more),
        (vec!["b".to_string(), "a".to_string()], 4, false)
    );
    // An anchor that does not match the filter is not a cursor of it.
    let stray = rig.get("p", "x0").unwrap();
    let r = rig.reader("p").select(
        MessageSelection {
            filter: &filter,
            sort: Sort::Default,
        },
        Some(&stray),
        5,
    );
    assert!(matches!(
        r,
        Err(SqliteError::Store(StoreError::InvalidCursor))
    ));
}

#[test]
fn a_snapshot_walk_is_not_disturbed_by_a_later_commit() {
    let mut rig = rig();
    for i in 0..4 {
        rig.create(
            "p",
            &stamped(&format!("m{i}"), &format!("2026-11-20T10:00:0{i}Z")),
        );
    }
    let filter = Filter::Messages { recipient: None };
    let first = rig.ids(&filter, Sort::Default, None, 2).0;
    assert_eq!(first, ["m0", "m1"]);
    // A reader on its own connection keeps its snapshot across a later commit.
    let dir = rig.temp.path().join("snap.db");
    let mut writer = Connection::open(&dir).unwrap();
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
        for i in 0..4 {
            m.create(
                &stamped(&format!("m{i}"), &format!("2026-11-20T10:00:0{i}Z")),
                1,
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let mut reader = Connection::open(&dir).unwrap();
    let snapshot = reader.transaction().unwrap();
    let page = MessageReader::new(&snapshot, "p", &rig.blobs)
        .select(
            MessageSelection {
                filter: &filter,
                sort: Sort::Default,
            },
            None,
            2,
        )
        .unwrap();
    let anchor = page.items.last().cloned();
    {
        let tx = writer.transaction().unwrap();
        let mut m = Messages::new(&tx, &who, &rig.blobs);
        m.delete("m2", m.get("m2").unwrap().unwrap().revision)
            .unwrap();
        m.create(&stamped("m0b", "2026-11-20T10:00:00Z"), 1)
            .unwrap();
        tx.commit().unwrap();
    }
    let rest = MessageReader::new(&snapshot, "p", &rig.blobs)
        .select(
            MessageSelection {
                filter: &filter,
                sort: Sort::Default,
            },
            anchor.as_ref(),
            10,
        )
        .unwrap();
    assert_eq!(
        (rest.items.iter().map(id_of).collect::<Vec<_>>(), rest.total),
        (vec!["m2", "m3"], 4)
    );
}

#[test]
fn revision_position_and_move_follow_the_port() {
    let mut rig = rig();
    let one = rig.create("p", &message("one"));
    let two = rig.create("p", &message("two"));
    assert_eq!(
        (one.revision, one.position, two.position),
        (Revision::ONE, 1, 2)
    );
    // A duplicate id is refused; another project's is not.
    let dup = rig.commit("p", |m| m.create(&message("one"), 7));
    assert!(matches!(
        dup,
        Err(SqliteError::Store(StoreError::AlreadyExists { .. }))
    ));
    rig.create("q", &message("one"));

    // An ordinary update keeps the position and counts the revision.
    let mut edited = message("one");
    edited.body = "changed".into();
    edited.status = MessageStatus::Delivered;
    let updated = rig
        .commit("p", |m| m.update(&edited, Revision::ONE, 7))
        .unwrap();
    let rev2 = Revision::ONE.next().unwrap();
    assert_eq!((updated.revision, updated.position), (rev2, 1));
    assert_eq!(body_of(&rig.get("p", "one").unwrap()), "changed");

    // A stale revision is refused by update, delete and move, and changes nothing.
    let stale = |r: crate::sqlite::Result<_>| match r {
        Err(SqliteError::Store(StoreError::StaleRevision { actual, .. })) => actual,
        other => panic!("{:?}", other.map(|_: Record| ())),
    };
    assert_eq!(
        stale(rig.commit("p", |m| m.update(&edited, Revision::ONE, 7))),
        Some(rev2)
    );
    assert_eq!(
        stale(rig.commit("p", |m| m.move_to_end("one", Revision::ONE))),
        Some(rev2)
    );
    let r = rig.commit("p", |m| m.delete("one", Revision::ONE));
    assert!(matches!(
        r,
        Err(SqliteError::Store(StoreError::StaleRevision { .. }))
    ));
    let missing = rig.commit("p", |m| m.update(&message("absent"), Revision::ONE, 7));
    assert_eq!(stale(missing), None);
    assert_eq!(rig.get("p", "one").unwrap().revision, rev2);

    // A move allocates a position after every one issued, and keeps the value.
    let moved = rig.commit("p", |m| m.move_to_end("one", rev2)).unwrap();
    assert_eq!((moved.revision.get(), moved.position), (3, 3));
    assert_eq!(rig.counter("p"), Some(3));
    assert_eq!(body_of(&rig.get("p", "one").unwrap()), "changed");

    // A delete does not lower the counter, and the counter is not rebuilt from MAX.
    rig.commit("p", |m| m.delete("one", moved.revision))
        .unwrap();
    assert_eq!(rig.counter("p"), Some(3));
    assert!(rig.get("p", "one").is_none());
    assert_eq!(rig.create("p", &message("three")).position, 4);
    // Positions are per project.
    assert_eq!(rig.counter("q"), Some(1));
    // Deleting leaves no detail or reference row, and the other project's stay.
    assert_eq!(rig.count("entity_details"), 3);
}

#[test]
fn a_counter_behind_its_rows_refuses_before_any_change() {
    let mut rig = rig();
    rig.create("p", &message("one"));
    rig.conn
        .execute(
            "UPDATE counters SET value=0 WHERE name='position:message'",
            [],
        )
        .unwrap();
    let r = rig.commit("p", |m| m.create(&message("two"), 7));
    assert!(matches!(r, Err(SqliteError::CounterBehind { .. })), "{r:?}");
    rig.conn
        .execute("DELETE FROM counters WHERE name='position:message'", [])
        .unwrap();
    let r = rig.commit("p", |m| m.create(&message("two"), 7));
    assert!(
        matches!(r, Err(SqliteError::CounterMissing { .. })),
        "{r:?}"
    );
    assert_eq!(rig.count("messages"), 1);
    assert_eq!(rig.count("entity_details"), 1);
}

/// A failure inside the operation, caught by the caller, who then commits other
/// work in the same transaction (R7), leaves the old message readable.
#[test]
fn a_caught_failure_leaves_the_old_message_readable() {
    let mut rig = rig();
    let old = rig.create("p", &message("old"));
    let mut big = message("old");
    big.body = "b".repeat(65_537);
    big.subject = "s".repeat(65_537);
    // Fail the second reference row: both objects are already published.
    rig.conn
        .execute_batch(
            "CREATE TRIGGER fail_second BEFORE INSERT ON entity_blob_refs
             WHEN NEW.locator='/subject' BEGIN SELECT RAISE(ABORT,'injected'); END;",
        )
        .unwrap();
    let who = caller("p");
    let tx = rig.conn.transaction().unwrap();
    {
        let mut messages = Messages::new(&tx, &who, &rig.blobs);
        let failed = messages.update(&big, old.revision, 7);
        assert!(matches!(failed, Err(SqliteError::Sql(_))), "{failed:?}");
        let failed = messages.create(
            &{
                let mut n = big.clone();
                n.id = "fresh".into();
                n
            },
            7,
        );
        assert!(matches!(failed, Err(SqliteError::Sql(_))), "{failed:?}");
        // The caller catches both and commits unrelated work.
        messages.create(&message("other"), 7).unwrap();
    }
    tx.commit().unwrap();
    let got = rig.get("p", "old").unwrap();
    assert_eq!((got.revision, body_of(&got)), (old.revision, "body"));
    assert!(rig.get("p", "fresh").is_none());
    assert_eq!(rig.get("p", "other").unwrap().position, 2);
    // Nothing of the failed operations remains reachable, though the objects do.
    assert_eq!(rig.count("entity_blob_refs"), 0);
    assert_eq!(rig.count("blobs"), 0);
    assert_eq!(rig.counter("p"), Some(2));
    // Failing the later chunk of one text behaves the same.
    rig.conn.execute_batch("DROP TRIGGER fail_second").unwrap();
    rig.conn
        .execute_batch(
            "CREATE TRIGGER fail_later BEFORE INSERT ON entity_blob_refs
             WHEN NEW.ordinal=1 BEGIN SELECT RAISE(ABORT,'injected'); END;",
        )
        .unwrap();
    let mut chunked = message("old");
    chunked.body = "c".repeat(crate::blob::MAX_BLOB_BYTES as usize + 1);
    let r = rig.commit("p", |m| m.update(&chunked, old.revision, 7));
    assert!(matches!(r, Err(SqliteError::Sql(_))), "{r:?}");
    assert_eq!(body_of(&rig.get("p", "old").unwrap()), "body");
    assert_eq!((rig.count("entity_blob_refs"), rig.count("blobs")), (0, 0));
}

#[test]
fn a_publication_that_fails_changes_nothing() {
    let mut rig = rig();
    let old = rig.create("p", &message("old"));
    let mut big = message("old");
    // The second chunk cannot be published: its object path is a directory.
    let first = "x".repeat(crate::blob::MAX_BLOB_BYTES as usize);
    big.body = format!("{first}tail");
    let tail = BlobHash::of(b"tail").to_string();
    let path = rig.object_path(&tail);
    std::fs::create_dir_all(&path).unwrap();
    let r = rig.commit("p", |m| m.update(&big, old.revision, 7));
    assert!(
        matches!(r, Err(SqliteError::Io(_) | SqliteError::Confined(_))),
        "{r:?}"
    );
    assert_eq!(body_of(&rig.get("p", "old").unwrap()), "body");
    assert_eq!((rig.count("entity_blob_refs"), rig.count("blobs")), (0, 0));
}

/// The registered candidate: a database migrated through the real registry to
/// the Message head, opened as the writer opens it, not the schema constants.
#[test]
fn the_registered_schema_head_serves_the_mapping() {
    use crate::sqlite::{ConnectionRole, Synchronous, open_connection};
    let (temp, data) = test_data();
    let blobs = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut conn).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        migrations::SCHEMA_VERSION as i64
    );
    let who = caller("p");
    let mut m = message("m");
    m.body = text_of(65_537);
    {
        let tx = conn.transaction().unwrap();
        let mut messages = Messages::new(&tx, &who, &blobs);
        let created = messages.create(&m, 5).unwrap();
        let moved = messages.move_to_end("m", created.revision).unwrap();
        assert_eq!((moved.revision.get(), moved.position), (2, 2));
        tx.commit().unwrap();
    }
    let snapshot = MessageReader::new(&conn, "p", &blobs)
        .get("m")
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.entity, Entity::Message(Box::new(m)));
    drop(temp);
}

#[test]
fn a_digest_is_never_an_identity_and_a_long_identity_is_found_by_its_original_bytes() {
    let mut rig = rig();
    let long = format!("{}-long", "i".repeat(65_540));
    let digest = BlobHash::of(long.as_bytes()).to_string();
    rig.create("p", &message(&long));
    // A short id spelled like the long id's digest is another message.
    rig.create("p", &message(&digest));
    assert_eq!(rig.get("p", &long).unwrap().entity.id(), long);
    assert_eq!(rig.get("p", &digest).unwrap().entity.id(), digest);
    assert_eq!(rig.get("p", &long).unwrap().position, 1);
    assert_eq!(rig.get("p", &digest).unwrap().position, 2);
    // Another long id of the same length and prefix is not found.
    let near = format!("{}-lonG", "i".repeat(65_540));
    assert!(rig.get("p", &near).is_none());
    let (all, total, _) = rig.ids(&Filter::Messages { recipient: None }, Sort::Id, None, 10);
    assert_eq!((all.len(), total), (2, 2));
}
