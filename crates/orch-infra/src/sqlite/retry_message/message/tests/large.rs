//! R97 thresholds for every text slot, and the physical proofs above one object.
use super::*;
use crate::blob::MAX_BLOB_BYTES;

const MAX: usize = MAX_BLOB_BYTES as usize;

/// `bytes` UTF-8 bytes of two-byte characters, plus one ASCII byte when odd.
fn utf8_text(bytes: usize) -> String {
    let mut text = "\u{e9}".repeat(bytes / 2);
    if bytes % 2 == 1 {
        text.push('a');
    }
    assert_eq!(text.len(), bytes);
    text
}

fn with_slot(slot: &str, text: &str) -> Message {
    let mut m = message("m");
    match slot {
        "id" => m.id = text.into(),
        "from_agent_id" => m.from_agent_id = text.into(),
        "to_agent_id" => m.to_agent_id = Some(text.into()),
        "subject" => m.subject = text.into(),
        "body" => m.body = text.into(),
        "created_at" => m.created_at = text.into(),
        "expires_at" => m.expires_at = Some(text.into()),
        "delivered_at" => m.delivered_at = Some(text.into()),
        "team_id" => m.team_id = Some(text.into()),
        "reply_to" => m.reply_to = Some(text.into()),
        _ => unreachable!(),
    }
    m
}

fn column(slot: &str) -> &str {
    match slot {
        "created_at" => "created_at_text",
        "expires_at" => "expires_at_text",
        "delivered_at" => "delivered_at_text",
        other => other,
    }
}

#[test]
fn the_threshold_counts_utf8_bytes_for_every_message_text_slot() {
    let mut rig = rig();
    for (slot, locator) in super::super::format::SLOTS {
        // 65,536 bytes of two-byte characters stay inline; one byte more does not,
        // though the string has fewer than 65,536 characters either way.
        for (bytes, form) in [(65_536usize, 0i64), (65_537, 1)] {
            let text = utf8_text(bytes);
            let m = with_slot(slot, &text);
            rig.conn.execute_batch("DELETE FROM messages; DELETE FROM entity_details; DELETE FROM entity_blob_refs").unwrap();
            rig.create("p", &m);
            assert_eq!(
                rig.get("p", &m.id).unwrap().entity,
                Entity::Message(Box::new(m.clone()))
            );
            let col = column(slot);
            let stored: (i64, Option<String>, Option<i64>) = rig
                .conn
                .query_row(
                    &format!("SELECT {col}_form,{col}_sha256,{col}_bytes FROM messages"),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            let hash = BlobHash::of(text.as_bytes()).to_string();
            let expect = if form == 1 {
                (1, Some(hash.clone()), Some(bytes as i64))
            } else {
                (0, None, None)
            };
            assert_eq!(stored, expect, "{slot} {bytes}");
            let refs: Vec<(String, i64, String, i64)> = rig
                .conn
                .prepare("SELECT locator,ordinal,sha256,size FROM entity_blob_refs")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            if form == 1 {
                assert_eq!(
                    refs,
                    [(locator.to_owned(), 0, hash, bytes as i64)],
                    "{slot}"
                );
                // The identity quartet holds the digest in form 1 and the row is found by it.
                if slot == "id" {
                    assert_eq!(rig.text("SELECT id FROM messages"), refs[0].2);
                }
            } else {
                // Nothing is named at the slot. The detail that holds 65,536 bytes
                // of text is itself over the threshold, so its own object (locator
                // `""`) is the only reference a message of one slot can have.
                assert!(refs.iter().all(|r| r.0.is_empty()), "{slot}: {refs:?}");
            }
        }
    }
}

#[test]
fn the_aggregate_detail_over_the_threshold_is_itself_an_object() {
    let mut rig = rig();
    let mut m = message("agg");
    m.subject = utf8_text(65_536);
    m.body = utf8_text(65_536);
    let saved = rig.create("p", &m);
    assert_eq!(saved.revision, Revision::ONE);
    let (inline_is_null, bytes): (bool, i64) = rig
        .conn
        .query_row(
            "SELECT detail IS NULL,detail_bytes FROM entity_details",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(inline_is_null && bytes > 65_536 * 2);
    let locators: Vec<String> = rig
        .conn
        .prepare("SELECT locator FROM entity_blob_refs")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(locators, [""]);
    assert_eq!(
        rig.get("p", "agg").unwrap().entity,
        Entity::Message(Box::new(m.clone()))
    );
    // Update to a small value drops the root object's reference row.
    let mut small = m.clone();
    small.subject = "s".into();
    small.body = "b".into();
    rig.commit("p", |x| x.update(&small, Revision::ONE, 8))
        .unwrap();
    assert_eq!(rig.count("entity_blob_refs"), 0);
    assert_eq!(
        rig.get("p", "agg").unwrap().entity,
        Entity::Message(Box::new(small))
    );
}

#[test]
fn text_of_exactly_64_mib_is_one_object_and_one_byte_more_is_two() {
    let mut rig = rig();
    let mut one = message("one");
    one.body = text_of(MAX);
    rig.create("p", &one);
    let hash = BlobHash::of(one.body.as_bytes()).to_string();
    let refs: Vec<(i64, String, i64)> = rig
        .conn
        .prepare("SELECT ordinal,sha256,size FROM entity_blob_refs ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(refs, [(0, hash.clone(), MAX as i64)]);
    assert_eq!(
        rig.get("p", "one").unwrap().entity,
        Entity::Message(Box::new(one.clone()))
    );

    // A character split by a chunk boundary: the euro sign starts one byte before it.
    let mut split = message("split");
    split.body = format!("{}\u{20ac}", text_of(MAX - 1));
    assert_eq!(split.body.len(), MAX + 2);
    rig.create("p", &split);
    let sizes: Vec<i64> = rig
        .conn
        .prepare("SELECT size FROM entity_blob_refs WHERE entity_key='split' ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(sizes, [MAX as i64, 2]);
    assert_eq!(
        rig.get("p", "split").unwrap().entity,
        Entity::Message(Box::new(split))
    );

    // Repeated chunks keep every occurrence and register one object.
    let mut repeated = message("repeat");
    repeated.body = text_of(2 * MAX + 1);
    rig.create("p", &repeated);
    let rows: Vec<(i64, String)> = rig
        .conn
        .prepare("SELECT ordinal,sha256 FROM entity_blob_refs WHERE entity_key='repeat' ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].1, rows[1].1);
    assert_eq!(
        rows[0].1, hash,
        "the first chunk is the one object of the first message"
    );
    assert_ne!(rows[1].1, rows[2].1);
    assert_eq!(
        rig.get("p", "repeat").unwrap().entity,
        Entity::Message(Box::new(repeated))
    );
    // Every object has one registration in the project, however often it is named.
    assert_eq!(rig.count("blobs"), 4);
    // Two names of one object: the first message's body is named by both.
    let shared: i64 = rig
        .conn
        .query_row(
            "SELECT count(*) FROM entity_blob_refs WHERE sha256=?1",
            [&hash],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(shared, 3);
    let _ = params![];
}
