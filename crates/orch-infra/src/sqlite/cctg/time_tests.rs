//! Issue 341: persisted Context and Goal times follow the shared spelling table.
use super::*;
use crate::sqlite::time_projection::SPELLINGS;
use rusqlite::Connection;

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    for step in [
        crate::sqlite::migrations::INITIAL_SCHEMA,
        crate::sqlite::migrations::ATTENTION_FACTS,
        crate::sqlite::migrations::TASK_FORMAT,
        crate::sqlite::migrations::AGENT_TEAM_RUN,
        crate::sqlite::migrations::CONTEXT_CONFIG_TEMPLATE_GOAL,
    ] {
        conn.execute_batch(step).unwrap();
    }
    conn
}

fn entity(family: Family, id: &str, text: &str, optional: bool) -> Entity {
    match family {
        Family::Context => Entity::Context(ContextEntry {
            key: id.into(),
            value: "value".into(),
            created_at: text.into(),
            updated_at: text.into(),
            ttl_ms: None,
            expires_at: optional.then(|| text.into()),
        }),
        Family::Goal => Entity::Goal(Box::new(Goal {
            id: id.into(),
            title: "title".into(),
            description: "description".into(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: text.into(),
            updated_at: optional.then(|| text.into()),
        })),
        _ => unreachable!(),
    }
}

fn columns(family: Family) -> &'static [(&'static str, bool)] {
    match family {
        Family::Context => &[
            ("created_at", false),
            ("updated_at", false),
            ("expires_at", true),
        ],
        Family::Goal => &[("created_at", false), ("updated_at", true)],
        _ => unreachable!(),
    }
}

#[test]
fn every_cctg_time_slot_follows_the_spelling_table_on_create_update_and_read() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = conn();
    let who = Principal::Owner {
        identity: "owner".into(),
    };
    let mut saved = Vec::new();
    {
        let tx = conn.transaction().unwrap();
        let mut db = NativeSql::new(&tx, &disk, "p", Some(&who), 1_000);
        for (i, (text, want)) in SPELLINGS.iter().enumerate() {
            for family in [Family::Context, Family::Goal] {
                for optional in [false, true] {
                    for update in [false, true] {
                        let id = format!("s{i}-{optional}-{update}");
                        let value = entity(family, &id, text, optional);
                        let record = if update {
                            let old = db
                                .create(entity(family, &id, "1970-01-01T00:00:00Z", !optional))
                                .unwrap();
                            db.update(value.clone(), old.revision).unwrap()
                        } else {
                            db.create(value.clone()).unwrap()
                        };
                        assert_eq!(record.entity, value);
                        // Inspect the SQLite value, including its storage class.
                        for (column, is_optional) in columns(family) {
                            let expected = if *is_optional && !optional {
                                None
                            } else {
                                *want
                            };
                            let got: SqlValue = tx
                                .query_row(
                                    &format!(
                                        "SELECT {column} FROM {} WHERE {}=?1",
                                        family.table(),
                                        family.identity()
                                    ),
                                    [&id],
                                    |r| r.get(0),
                                )
                                .unwrap();
                            assert_eq!(
                                got,
                                expected.map(SqlValue::Integer).unwrap_or(SqlValue::Null),
                                "write {}.{column} of {text:?}, optional={optional}, update={update}",
                                family.name()
                            );
                        }
                        saved.push((family, record, *text, *want, optional));
                    }
                }
            }
        }
        tx.commit().unwrap();
    }
    let tx = conn.transaction().unwrap();
    let db = NativeSql::new(&tx, &disk, "p", Some(&who), 1_000);
    for (family, record, text, want, optional) in saved {
        let id = record.entity.id();
        // A new transaction hydrates exact text (including NUL, whitespace,
        // fractions and offset spelling), plus absent optional fields.
        assert_eq!(
            db.get(family, id).unwrap(),
            Some(record.clone()),
            "read {text:?}"
        );
        for (column, is_optional) in columns(family) {
            let expected = if *is_optional && !optional {
                None
            } else {
                want
            };
            let mut wrong_values = vec![Some(expected.map_or(0, |ms| ms + 1))];
            if expected.is_some() {
                wrong_values.push(None);
            }
            for wrong in wrong_values {
                tx.execute_batch("SAVEPOINT damage").unwrap();
                let value = wrong.map_or("NULL".into(), |ms| ms.to_string());
                tx.execute_batch(&format!(
                    "UPDATE {} SET {column}={value} WHERE {}='{id}'",
                    family.table(),
                    family.identity(),
                ))
                .unwrap();
                assert!(
                    matches!(db.get(family, id), Err(NativeError::Corrupt {
                    family: damaged_family, form: 0, check: "projection", ref locator,
                }) if damaged_family == family.name() && locator == column),
                    "read {}.{column}={value} of {text:?}, optional={optional}",
                    family.name()
                );
                tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
                    .unwrap();
            }
        }
    }
}
