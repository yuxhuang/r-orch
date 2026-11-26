//! Stored damage is R140 corruption, never repaired by a read.
use super::*;

/// A message with every slot indirect so each kind of reference row exists.
fn indirect() -> Message {
    let mut m = message("m");
    m.body = text_of(65_537);
    m.subject = "s".repeat(65_538);
    m
}

type Damage = fn(&Rig);

fn exec(rig: &Rig, sql: &str) {
    rig.conn
        .execute_batch(sql)
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn object(rig: &Rig, body: bool) -> PathBuf {
    let text = if body {
        text_of(65_537)
    } else {
        "s".repeat(65_538)
    };
    rig.object_path(&BlobHash::of(text.as_bytes()).to_string())
}

fn cases() -> Vec<(&'static str, Damage, (i64, &'static str, &'static str))> {
    vec![
        (
            "sender projection",
            |r| exec(r, "UPDATE messages SET from_agent_id='other' WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "status projection",
            |r| exec(r, "UPDATE messages SET status='expired' WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "channel projection",
            |r| exec(r, "UPDATE messages SET channel='lead' WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "recipient null",
            |r| exec(r, "UPDATE messages SET to_agent_id=NULL WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "recipient empty",
            |r| exec(r, "UPDATE messages SET to_agent_id='' WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "time text",
            |r| exec(r, "UPDATE messages SET created_at_text='2026' WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "millisecond",
            |r| exec(r, "UPDATE messages SET created_at=5 WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "expiry projection",
            |r| exec(r, "UPDATE messages SET expires_at_text='' WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "body length",
            |r| exec(r, "UPDATE messages SET body_bytes=65538 WHERE id='m'"),
            (0, "projection", "row"),
        ),
        (
            "body hash",
            |r| {
                exec(
                    r,
                    &format!(
                        "UPDATE messages SET body_sha256='{}' WHERE id='m'",
                        "0".repeat(64)
                    ),
                )
            },
            (0, "projection", "row"),
        ),
        (
            "revision",
            |r| {
                exec(
                    r,
                    "PRAGMA ignore_check_constraints=1; UPDATE messages SET revision=0 WHERE id='m'",
                )
            },
            (0, "counter", "revision"),
        ),
        (
            "native text encoding",
            |r| {
                exec(
                    r,
                    "UPDATE messages SET from_agent_id=CAST(x'ff' AS TEXT) WHERE id='m'",
                )
            },
            (0, "text encoding", "row"),
        ),
        (
            "detail missing",
            |r| exec(r, "DELETE FROM entity_details"),
            (0, "detail missing", ""),
        ),
        (
            "detail format",
            |r| exec(r, "UPDATE entity_details SET format=2"),
            (0, "detail metadata", ""),
        ),
        (
            "detail hash",
            |r| {
                exec(
                    r,
                    &format!(
                        "UPDATE entity_details SET detail_sha256='{}'",
                        "1".repeat(64)
                    ),
                )
            },
            (0, "detail length or hash", ""),
        ),
        (
            "detail text",
            |r| {
                exec(
                    r,
                    "UPDATE entity_details SET detail=replace(detail,'\"direct\"','\"dirext\"') WHERE entity_key='m'",
                )
            },
            (0, "detail length or hash", ""),
        ),
        (
            "reference missing",
            |r| exec(r, "DELETE FROM entity_blob_refs WHERE locator='/body'"),
            (0, "reference metadata", "/body"),
        ),
        (
            "reference extra",
            |r| {
                exec(
                    r,
                    "INSERT INTO entity_blob_refs SELECT project_id,family,key_form,entity_key,'/team_id',0,sha256,size FROM entity_blob_refs WHERE locator='/body'",
                )
            },
            (0, "reference locators", ""),
        ),
        (
            "reference ordinal",
            |r| {
                exec(
                    r,
                    "UPDATE entity_blob_refs SET ordinal=1 WHERE locator='/body'",
                )
            },
            (0, "reference metadata", "/body"),
        ),
        (
            "reference size",
            |r| {
                exec(
                    r,
                    "UPDATE entity_blob_refs SET size=size-1 WHERE locator='/body'",
                )
            },
            (0, "blob registration", "refs"),
        ),
        (
            "registration missing",
            |r| exec(r, "DELETE FROM blobs"),
            (0, "blob registration", "refs"),
        ),
        (
            "registration size",
            |r| exec(r, "UPDATE blobs SET size=size+1"),
            (0, "blob registration", "refs"),
        ),
    ]
}

#[test]
fn a_damaged_message_is_stored_data_corruption() {
    for (name, break_it, expected) in cases() {
        let mut rig = rig();
        rig.create("p", &indirect());
        rig.create("p", &message("healthy"));
        // Whole-table damage statements name the damaged message by being the only
        // one they touch, so the healthy one is built after them below.
        break_it(&rig);
        let got = damage(rig.reader("p").get("m"));
        assert_eq!(got, expected, "{name}");
        // A page that meets the damage returns no partial page.
        let page = rig.reader("p").select(
            MessageSelection {
                filter: &Filter::Messages { recipient: None },
                sort: Sort::Default,
            },
            None,
            10,
        );
        assert!(
            matches!(page, Err(SqliteError::MessageCorrupt { .. })),
            "{name}: {:?}",
            page.map(|p| p.total)
        );
        // Nothing is repaired: the read changed no row.
        assert!(damage(rig.reader("p").get("m")) == expected, "{name}");
    }
}

#[test]
fn damaged_objects_are_corruption_and_a_missing_one_proves_nothing_else() {
    let mut rig = rig();
    rig.create("p", &indirect());
    let path = object(&rig, true);
    let good = std::fs::read(&path).unwrap();
    // Corrupt bytes: the adapter refuses to return them.
    std::fs::write(&path, b"short").unwrap();
    let r = rig.reader("p").get("m");
    assert!(
        matches!(
            r,
            Err(SqliteError::MessageCorrupt {
                check: "referenced bytes",
                locator: "/body",
                ..
            })
        ),
        "{r:?}"
    );
    // A missing object is stored-data corruption as well.
    std::fs::remove_file(&path).unwrap();
    let r = rig.reader("p").get("m");
    assert!(
        matches!(
            r,
            Err(SqliteError::MessageCorrupt {
                check: "referenced bytes",
                locator: "/body",
                ..
            })
        ),
        "{r:?}"
    );
    std::fs::write(&path, &good).unwrap();
    assert!(rig.get("p", "m").is_some());
    // The other slot's object.
    let other = object(&rig, false);
    std::fs::remove_file(&other).unwrap();
    let r = rig.reader("p").get("m");
    assert!(
        matches!(
            r,
            Err(SqliteError::MessageCorrupt {
                locator: "/subject",
                ..
            })
        ),
        "{r:?}"
    );
}

#[test]
fn a_detail_the_decoder_refuses_is_corruption() {
    // Each stored detail is checked as the strict decoder defines it: an unknown
    // key, a duplicate key, a missing key, a wrong type and a null in a slot
    // other than the recipient.
    let base = r#""id":"m","channel":"direct","from_agent_id":"a","to_agent_id":null,"subject":"","body":"","created_at":"t","status":"pending""#;
    let cases: [(&str, String); 9] = [
        ("unknown key", format!("{{{base},\"extra\":\"x\"}}")),
        ("duplicate key", format!("{{{base},\"body\":\"y\"}}")),
        (
            "missing key",
            format!("{{{}}}", base.replace("\"subject\":\"\",", "")),
        ),
        (
            "null slot",
            format!("{{{}}}", base.replace("\"body\":\"\"", "\"body\":null")),
        ),
        (
            "wrong type",
            format!("{{{}}}", base.replace("\"body\":\"\"", "\"body\":1")),
        ),
        (
            "channel word",
            format!("{{{}}}", base.replace("\"direct\"", "\"mail\"")),
        ),
        (
            "status word",
            format!("{{{}}}", base.replace("\"pending\"", "\"done\"")),
        ),
        ("null optional", format!("{{{base},\"expires_at\":null}}")),
        ("not an object", "[1]".into()),
    ];
    for (name, detail) in cases {
        let mut rig = rig();
        rig.create("p", &message("m"));
        let hash = BlobHash::of(detail.as_bytes()).to_string();
        rig.conn
            .execute(
                "UPDATE entity_details SET detail=?1,detail_bytes=?2,detail_sha256=?3",
                params![detail, detail.len() as i64, hash],
            )
            .unwrap();
        let got = rig.reader("p").get("m");
        assert!(
            matches!(got, Err(SqliteError::MessageCorrupt { form: 0, .. })),
            "{name}: {got:?}"
        );
    }
}

#[test]
fn damage_to_a_row_a_page_cannot_return_does_not_fail_it() {
    let mut rig = rig();
    rig.create("p", &message("good"));
    let mut other = message("elsewhere");
    other.to_agent_id = Some("someone".into());
    rig.create("p", &other);
    rig.conn
        .execute(
            "DELETE FROM entity_details WHERE entity_key='elsewhere'",
            [],
        )
        .unwrap();
    let page = rig
        .reader("p")
        .select(
            MessageSelection {
                filter: &Filter::PendingMessages {
                    recipient: "agt_2".into(),
                    now: "0".into(),
                },
                sort: Sort::Default,
            },
            None,
            10,
        )
        .unwrap();
    assert_eq!(page.items.iter().map(id_of).collect::<Vec<_>>(), ["good"]);
    // The same read of the damaged row itself is corruption.
    assert_eq!(
        damage(rig.reader("p").get("elsewhere")),
        (0, "detail missing", "")
    );
}

#[test]
fn a_create_refuses_an_orphan_detail_without_writing() {
    let mut rig = rig();
    rig.create("p", &message("m"));
    rig.conn.execute_batch("DELETE FROM messages").unwrap();
    let r = rig.commit("p", |m| m.create(&message("m"), 1));
    assert_eq!(damage(r), (0, "orphan row", "refs"));
    assert_eq!(rig.count("messages"), 0);
}

#[test]
fn an_object_registered_only_in_another_project_is_not_a_registration() {
    let mut rig = rig();
    rig.create("p", &indirect());
    rig.create("q", &indirect());
    rig.conn
        .execute("DELETE FROM blobs WHERE project_id='p'", [])
        .unwrap();
    assert_eq!(
        damage(rig.reader("p").get("m")),
        (0, "blob registration", "refs")
    );
    assert!(rig.get("q", "m").is_some());
}
