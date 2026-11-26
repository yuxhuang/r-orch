//! Issue 335: the native time columns follow the one RFC 3339 table of
//! `time_projection::SPELLINGS`, on write and on read.
use super::*;
use crate::sqlite::time_projection::SPELLINGS;
use serde_json::json;

fn conn() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for sql in [
        crate::sqlite::migrations::INITIAL_SCHEMA,
        crate::sqlite::migrations::ATTENTION_FACTS,
        crate::sqlite::migrations::TASK_FORMAT,
        crate::sqlite::migrations::AGENT_TEAM_RUN,
    ] {
        conn.execute_batch(sql).unwrap();
    }
    conn
}

fn run_at(id: &str, text: &str) -> Run {
    serde_json::from_value(json!({"id":id,"task_id":"t","agent_id":"a","attempt":1,
        "status":"running","started_at":text,"finished_at":text,
        "workspace_path":"/synthetic","prompt":""}))
    .unwrap()
}

fn team_at(id: &str, text: &str) -> Team {
    serde_json::from_value(
        json!({"id":id,"name":"n","status":"active","task_pool":[],"lead_agent_id":"a",
        "members":[{"agent_id":"a","role":"member","joined_at":text}],
        "created_at":text,"updated_at":text,"config":{"auto_claim":false}}),
    )
    .unwrap()
}

/// Every native time slot of one spelling: the five column reads in the order
/// of `COLUMNS`, which the damage step below rewrites one by one.
const COLUMNS: [(&str, &str, &str); 5] = [
    ("runs", "started_at", "id"),
    ("runs", "finished_at", "id"),
    ("teams", "created_at", "id"),
    ("teams", "updated_at", "id"),
    ("team_members", "joined_at", "team_id"),
];

#[test]
fn every_native_time_slot_follows_the_spelling_table_on_write_and_on_read() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    for (i, (text, want)) in SPELLINGS.iter().enumerate() {
        let id = format!("s{i}");
        let run = sql
            .create(Entity::Run(Box::new(run_at(&id, text))))
            .unwrap();
        let team = sql
            .create(Entity::Team(Box::new(team_at(&id, text))))
            .unwrap();
        // Write: each slot holds the projection of the table, the original
        // text beside it, and the entity reads back as it was written.
        for (table, column, key) in COLUMNS {
            let got: Option<i64> = tx
                .query_row(
                    &format!("SELECT {column} FROM {table} WHERE {key}=?1"),
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(got, *want, "write {table}.{column} of {text:?}");
        }
        assert_eq!(sql.get(Family::Run, &id).unwrap(), Some(run));
        assert_eq!(sql.get(Family::Team, &id).unwrap(), Some(team));
        // Read: a stored projection that differs from the table's, whether a
        // number where NULL belongs or the wrong number, is corruption.
        for (table, column, key) in COLUMNS {
            let family = if table == "runs" {
                Family::Run
            } else {
                Family::Team
            };
            let wrong = want.map_or(0, |ms| ms + 1);
            // `execute_batch`, not `execute`: the engine's guard that
            // `Adapter::execute` has one caller reads this file as source.
            let mut wrong_values = vec![wrong.to_string()];
            if want.is_some() {
                wrong_values.push("NULL".into());
            }
            for value in wrong_values {
                tx.execute_batch("SAVEPOINT damage").unwrap();
                tx.execute_batch(&format!(
                    "UPDATE {table} SET {column}={value} WHERE {key}='{id}'"
                ))
                .unwrap();
                assert!(
                    matches!(sql.get(family, &id), Err(NativeError::Corrupt { .. })),
                    "read {table}.{column}={value} of {text:?}"
                );
                tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
                    .unwrap();
            }
        }
    }
}
