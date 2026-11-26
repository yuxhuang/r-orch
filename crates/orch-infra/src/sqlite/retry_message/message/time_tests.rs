//! Issue 335: the Message time columns follow the one RFC 3339 table of
//! `time_projection::SPELLINGS`, on write and on read, exactly as the native
//! family does.
use super::tests::{damage, message, rig};
use crate::sqlite::time_projection::SPELLINGS;

const COLUMNS: [&str; 3] = ["created_at", "expires_at", "delivered_at"];

#[test]
fn every_message_time_slot_follows_the_spelling_table_on_write_and_on_read() {
    let mut rig = rig();
    for (i, (text, want)) in SPELLINGS.iter().enumerate() {
        let id = format!("m{i}");
        let mut m = message(&id);
        m.created_at = (*text).into();
        m.expires_at = Some((*text).into());
        m.delivered_at = Some((*text).into());
        let saved = rig.create("p", &m);
        // Write: each slot holds the projection of the table, and the
        // original text is kept beside it.
        for column in COLUMNS {
            let got: Option<i64> = rig
                .conn
                .query_row(
                    &format!("SELECT {column} FROM messages WHERE id=?1"),
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(got, *want, "write {column} of {text:?}");
            let kept: String = rig
                .conn
                .query_row(
                    &format!("SELECT {column}_text FROM messages WHERE id=?1"),
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(kept, *text, "original {column} of {text:?}");
        }
        assert_eq!(rig.get("p", &id), Some(saved));
        // Read: a stored projection that differs from the table's, whether a
        // number where NULL belongs or the wrong number, is corruption.
        for column in COLUMNS {
            let wrong = want.map_or(0, |ms| ms + 1);
            let mut wrong_values = vec![Some(wrong)];
            if want.is_some() {
                wrong_values.push(None);
            }
            for value in wrong_values {
                rig.conn.execute_batch("SAVEPOINT damage").unwrap();
                // `execute_batch`, not `execute`: the engine's guard that
                // `Adapter::execute` has one caller reads this file as source.
                let value = value.map_or("NULL".to_owned(), |v| v.to_string());
                rig.conn
                    .execute_batch(&format!(
                        "UPDATE messages SET {column}={value} WHERE id='{id}'"
                    ))
                    .unwrap();
                assert_eq!(
                    damage(rig.reader("p").get(&id)),
                    (0, "projection", "row"),
                    "read {column}={value:?} of {text:?}"
                );
                rig.conn
                    .execute_batch("ROLLBACK TO damage; RELEASE damage")
                    .unwrap();
            }
        }
    }
}
