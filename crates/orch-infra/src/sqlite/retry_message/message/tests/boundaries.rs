//! Regression controls for aggregate/recipient boundaries and strict detail decoding.
use super::*;

fn aggregate_at(bytes: i64, indirect: bool) {
    let mut empty = message("m");
    empty.body.clear();
    let mut baseline = rig();
    baseline.create("p", &empty);
    let overhead: i64 = baseline
        .conn
        .query_row("SELECT detail_bytes FROM entity_details", [], |r| r.get(0))
        .unwrap();

    let mut rig = rig();
    let mut m = empty;
    m.body = "a".repeat(usize::try_from(bytes - overhead).unwrap());
    let saved = rig.create("p", &m);
    let stored: (bool, i64) = rig
        .conn
        .query_row(
            "SELECT detail IS NULL,detail_bytes FROM entity_details",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (indirect, bytes));
    assert_eq!(rig.get("p", "m"), Some(saved));
}

#[test]
fn aggregate_detail_of_65536_bytes_stays_inline_and_round_trips() {
    aggregate_at(65_536, false);
}

#[test]
fn aggregate_detail_of_65537_bytes_is_indirect_and_round_trips() {
    aggregate_at(65_537, true);
}

fn recipient_page(filter: impl FnOnce(String) -> Filter) -> (Vec<String>, u64, bool) {
    let mut rig = rig();
    let recipient = "r".repeat(65_536);
    let mut m = message("m");
    m.to_agent_id = Some(recipient.clone());
    rig.create("p", &m);
    rig.ids(&filter(recipient), Sort::Default, None, 10)
}

#[test]
fn pending_filter_finds_the_exact_65536_byte_recipient() {
    assert_eq!(
        recipient_page(|recipient| pending(&recipient, "0")),
        (vec!["m".into()], 1, false)
    );
}

#[test]
fn messages_filter_finds_the_exact_65536_byte_recipient() {
    assert_eq!(
        recipient_page(|recipient| Filter::Messages {
            recipient: Some(recipient)
        }),
        (vec!["m".into()], 1, false)
    );
}

// Use the exact stored detail so a permissive decoder cannot be hidden by a
// later projection mismatch. Recompute the metadata so only the JSON is damaged.
fn read_detail_with(suffix: &str) -> crate::sqlite::Result<Option<Record>> {
    let mut rig = rig();
    rig.create("p", &message("m"));
    let mut detail = rig.text("SELECT detail FROM entity_details");
    assert_eq!(detail.pop(), Some('}'));
    detail.push_str(suffix);
    detail.push('}');
    let hash = BlobHash::of(detail.as_bytes()).to_string();
    rig.conn
        .execute(
            "UPDATE entity_details SET detail=?1,detail_bytes=?2,detail_sha256=?3",
            params![detail, detail.len() as i64, hash],
        )
        .unwrap();
    rig.reader("p").get("m")
}

#[test]
fn row_matching_detail_control_decodes_the_whole_message() {
    assert_eq!(
        read_detail_with("").unwrap().unwrap().entity,
        Entity::Message(Box::new(message("m")))
    );
}

#[test]
fn row_matching_detail_with_an_extra_key_fails_the_detail_keys_check() {
    assert_eq!(
        damage(read_detail_with(r#","extra":"x""#)),
        (0, "detail keys", "")
    );
}

#[test]
fn row_matching_detail_with_null_optional_values_fails_the_detail_keys_check() {
    for slot in ["expires_at", "delivered_at", "team_id", "reply_to"] {
        assert_eq!(
            damage(read_detail_with(&format!(",\"{slot}\":null"))),
            (0, "detail keys", ""),
            "{slot}"
        );
    }
}

#[test]
fn row_matching_detail_with_a_duplicate_key_fails_the_strict_json_check() {
    assert_eq!(
        damage(read_detail_with(r#","body":"body""#)),
        (0, "strict JSON", "")
    );
}
