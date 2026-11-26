use super::*;
use crate::blob::DiskBlobStore;
use crate::sqlite::SqliteError;
use crate::sqlite::{ConnectionRole, Synchronous, Writer, migrations, open_connection, test_data};
use crate::storage::v2::FileStorage;
use orch_core::store::{Caller, EntityKind, Principal, StorageExt, StoreError};
use orch_core::test_support::FixedClock;
use orch_domain::GoalStatus;
use rusqlite::OptionalExtension;
use std::sync::Arc;
use std::time::Duration;

struct Close<'a>(&'a WriterHandle);
impl Drop for Close<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn goal(id: &str) -> Goal {
    Goal {
        id: id.into(),
        title: format!("goal {id}"),
        description: "synthetic λ ' ? ;".into(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: "2026-11-22T00:00:00.123Z".into(),
        updated_at: None,
    }
}
fn adapter_for<'a, 'd>(
    handle: &'a WriterHandle,
    pool: &'a ReadPool<'d>,
    data: &crate::data_dir::DataDir,
) -> GoalAdapter<'a, 'd> {
    GoalAdapter::new(
        handle,
        pool,
        Arc::new(DiskBlobStore::open(data.root()).unwrap()),
        Arc::new(FixedClock::new()),
    )
}
fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn setup(data: &crate::data_dir::DataDir) {
    let mut connection =
        open_connection(data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(data, &mut connection).unwrap();
}

// Real executor commits and rollback are the seam: replacing write with a row
// call on an unrelated connection would fail snapshot/reopen and atomicity.
#[tokio::test]
async fn committed_goal_lifecycle_matches_file_reference_and_reopens() {
    let (temp, data) = test_data();
    setup(&data);
    let file = FileStorage::new(temp.path().join("reference"));
    let mut expected = Vec::new();
    for id in ["z", "a"] {
        let payload = Entity::Goal(Box::new(goal(id)));
        expected.push(
            file.transaction(owner("p"), orch_core::store::Lane::Entities, move |u| {
                u.create(payload)
            })
            .await
            .unwrap(),
        );
    }
    let payload = Entity::Goal(Box::new(Goal {
        title: "updated".into(),
        ..goal("z")
    }));
    let changed = file
        .transaction(owner("p"), orch_core::store::Lane::Entities, move |u| {
            u.update(payload, Revision::ONE)
        })
        .await
        .unwrap();
    let moved = file
        .transaction(owner("p"), orch_core::store::Lane::Entities, move |u| {
            u.move_to_end(EntityKind::Goal, "z", changed.revision)
        })
        .await
        .unwrap();
    let moved_revision = moved.revision;
    file.transaction(owner("p"), orch_core::store::Lane::Entities, move |u| {
        u.delete(EntityKind::Goal, "z", moved_revision)
    })
    .await
    .unwrap();
    let recreated = file
        .transaction(owner("p"), orch_core::store::Lane::Entities, move |u| {
            u.create(Entity::Goal(Box::new(goal("z"))))
        })
        .await
        .unwrap();
    {
        let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
        let pool = ReadPool::open(&data, Synchronous::Normal, 2).unwrap();
        std::thread::scope(|scope| {
            let _close = Close(&handle);
            scope.spawn(move || writer.run());
            let adapter = adapter_for(&handle, &pool, &data);
            let initial = adapter
                .write("p".into(), deadline(), |u| {
                    Ok(vec![u.create(goal("z"))?, u.create(goal("a"))?])
                })
                .unwrap()
                .wait()
                .unwrap();
            assert_eq!(initial, expected);
            let actual = adapter
                .write("p".into(), deadline(), |u| {
                    let changed = u.update(
                        Goal {
                            title: "updated".into(),
                            ..goal("z")
                        },
                        Revision::ONE,
                    )?;
                    u.move_to_end("z", changed.revision)
                })
                .unwrap()
                .wait()
                .unwrap();
            assert_eq!(actual, moved);
            let actual = adapter
                .write("p".into(), deadline(), move |u| {
                    u.delete("z", moved_revision)?;
                    u.create(goal("z"))
                })
                .unwrap()
                .wait()
                .unwrap();
            assert_eq!(actual, recreated);
            assert_eq!(
                adapter.read("p", deadline(), |u| u.get("z")).unwrap(),
                Some(recreated.clone())
            );
            assert_eq!(
                adapter.read("other", deadline(), |u| u.get("z")).unwrap(),
                None
            );
            handle.close();
        });
    }
    // Close every connection AND the directory guard before reacquiring.
    drop(data);
    let data = crate::data_dir::DataDir::acquire(
        &temp.path().join("data"),
        &crate::data_dir::Holder {
            kind: crate::data_dir::LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        assert_eq!(
            adapter.read("p", deadline(), |u| u.get("z")).unwrap(),
            Some(recreated)
        );
        let next = adapter
            .write("p".into(), deadline(), |u| u.create(goal("after-reopen")))
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!((next.revision, next.position), (Revision::ONE, 5));
        handle.close();
    });
}

#[test]
fn snapshot_keeps_goal_and_counter_state_across_concurrent_commit() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 2).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        let first = adapter
            .write("p".into(), deadline(), |u| u.create(goal("g")))
            .unwrap()
            .wait()
            .unwrap();
        adapter
            .read("p", deadline(), |snapshot| {
                assert_eq!(snapshot.get("g")?, Some(first.clone()));
                let moved = adapter
                    .write("p".into(), deadline(), |u| {
                        let moved = u.move_to_end("g", Revision::ONE)?;
                        u.create(goal("later"))?;
                        Ok(moved)
                    })?
                    .wait()?;
                assert_eq!(moved.position, 2);
                assert_eq!(snapshot.get("g")?, Some(first));
                assert_eq!(snapshot.get("later")?, None);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            adapter
                .read("p", deadline(), |u| u.get("later"))
                .unwrap()
                .unwrap()
                .position,
            3
        );
        handle.close();
    });
}

#[test]
fn returned_error_rolls_back_whole_work_and_caught_stale_preserves_prior_work() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        let error = adapter
            .write("p".into(), deadline(), |u| {
                u.create(goal("rolled-back"))?;
                u.delete("missing", Revision::ONE)
            })
            .unwrap()
            .wait()
            .unwrap_err();
        assert!(matches!(
            error,
            SqliteError::Store(StoreError::StaleRevision { actual: None, .. })
        ));
        assert_eq!(
            adapter
                .read("p", deadline(), |u| u.get("rolled-back"))
                .unwrap(),
            None
        );
        let saved = adapter
            .write("p".into(), deadline(), |u| {
                let row = u.create(goal("saved"))?;
                assert!(matches!(
                    u.update(goal("saved"), Revision::ONE.next().unwrap()),
                    Err(SqliteError::Store(StoreError::StaleRevision { .. }))
                ));
                assert!(matches!(
                    u.create(goal("saved")),
                    Err(SqliteError::Store(StoreError::AlreadyExists { .. }))
                ));
                assert_eq!(u.get("saved")?, Some(row));
                u.create(goal("second"))
            })
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(saved.position, 2);
        assert_eq!(
            adapter
                .write("other".into(), deadline(), |u| u.create(goal("saved")))
                .unwrap()
                .wait()
                .unwrap()
                .position,
            1
        );
        handle.close();
    });
}

#[test]
fn timestamps_are_kept_as_the_original_strings_through_the_executor() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        let times = [
            "2026-11-22T00:00:00.123Z",
            "1969-12-31T23:59:59.999Z",
            "0000-01-01T00:00:00.000Z",
            "2026-11-22T00:00:00Z",
            "2026-11-22T00:00:00.123456Z",
            "2026-11-22T01:00:00.123+01:00",
            "-001-12-31T23:59:59.999Z",
            "+010000-01-01T00:00:00.000Z",
            "opaque",
            "",
        ];
        for (i, time) in times.iter().enumerate() {
            let payload = Goal {
                created_at: (*time).into(),
                updated_at: Some((*time).into()),
                ..goal(&format!("g{i}"))
            };
            let written = adapter
                .write("p".into(), deadline(), {
                    let payload = payload.clone();
                    move |u| u.create(payload)
                })
                .unwrap()
                .wait()
                .unwrap();
            assert_eq!(written.entity, Entity::Goal(Box::new(payload.clone())));
            assert_eq!(
                adapter
                    .read("p", deadline(), |u| u.get(&format!("g{i}")))
                    .unwrap(),
                Some(written)
            );
        }
        handle.close();
    });
}

#[test]
fn exhausted_or_unseeded_counters_consume_nothing() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        adapter
            .write("p".into(), deadline(), |u| {
                let first = u.create(goal("g"))?;
                assert_eq!(first.position, 1);
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("DELETE FROM counters WHERE project_id='p'", [])?;
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        let error = adapter
            .write("p".into(), deadline(), |u| u.create(goal("unseeded")))
            .unwrap()
            .wait()
            .unwrap_err();
        assert!(matches!(error, SqliteError::CounterMissing { .. }));
        handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute(
                    "INSERT INTO counters(project_id,name,value) VALUES('p','position:goal',?1)",
                    [i64::MAX],
                )?;
                tx.execute(
                    "UPDATE goals SET revision=?1 WHERE project_id='p' AND id='g'",
                    [i64::MAX],
                )?;
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        let max_revision: Revision = serde_json::from_value(i64::MAX.into()).unwrap();
        for op in 0..3 {
            let error = adapter
                .write("p".into(), deadline(), move |u| match op {
                    0 => u.create(goal("overflow")),
                    1 => u.update(goal("g"), max_revision),
                    _ => u.move_to_end("g", max_revision),
                })
                .unwrap()
                .wait()
                .unwrap_err();
            assert!(matches!(
                error,
                SqliteError::Store(StoreError::CounterExhausted)
            ));
        }
        assert_eq!(
            adapter
                .read("p", deadline(), |u| u.get("g"))
                .unwrap()
                .unwrap()
                .revision,
            max_revision
        );
        assert_eq!(
            adapter
                .read("p", deadline(), |u| u.get("unseeded"))
                .unwrap(),
            None
        );
        assert_eq!(
            adapter
                .read("p", deadline(), |u| u.get("overflow"))
                .unwrap(),
            None
        );
        handle.close();
    });
}

#[test]
fn caught_rollback_failure_poison_prevents_later_operations_and_commit() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        handle.submit(Lane::Entities, None, |tx| {
            tx.execute_batch("CREATE TRIGGER abort_goal BEFORE INSERT ON goals WHEN NEW.id='fatal' BEGIN SELECT RAISE(ROLLBACK, 'synthetic'); END")?;
            Ok(())
        }).unwrap().wait().unwrap();
        let adapter = adapter_for(&handle, &pool, &data);
        let error = adapter.write("p".into(), deadline(), |u| {
            u.create(goal("earlier"))?;
            assert!(matches!(u.create(goal("fatal")), Err(SqliteError::OperationRollback {operation, ..}) if matches!(*operation, SqliteError::Sql(_))));
            assert!(matches!(u.create(goal("after")), Err(SqliteError::TransactionAborted)));
            Ok(())
        }).unwrap().wait().unwrap_err();
        assert!(matches!(error, SqliteError::TransactionAborted));
        for id in ["earlier", "fatal", "after"] {
            assert_eq!(adapter.read("p", deadline(), |u| u.get(id)).unwrap(), None);
        }
        let fresh = adapter
            .write("p".into(), deadline(), |u| u.create(goal("fresh")))
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(fresh.position, 1);
        handle.close();
    });
}

#[test]
fn ended_transaction_blocks_later_operations_without_leaking_rows_or_counter() {
    ended_transaction_is_aborted(true);
}

#[test]
fn ended_transaction_cannot_return_success_without_a_later_operation() {
    ended_transaction_is_aborted(false);
}

fn ended_transaction_is_aborted(later_operation: bool) {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        let result = adapter
            .write("p".into(), deadline(), move |u| {
                u.create(goal("before"))?;
                // Deterministically simulate SQLite ending the outer transaction
                // on an error outside an operation's savepoint (e.g. IOERR).
                u.sql.tx().execute_batch("ROLLBACK")?;
                assert!(u.sql.tx().is_autocommit());
                if later_operation {
                    u.create(goal("after"))?;
                }
                Ok(())
            })
            .unwrap()
            .wait();
        for id in ["before", "after"] {
            assert_eq!(adapter.read("p", deadline(), |u| u.get(id)).unwrap(), None);
        }
        adapter
            .read("p", deadline(), |u| {
                let counters: i64 = u.sql.tx().query_row(
                    "SELECT count(*) FROM counters WHERE project_id='p' AND name='position:goal'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(counters, 0);
                Ok(())
            })
            .unwrap();
        let fresh = adapter
            .write("p".into(), deadline(), |u| u.create(goal("fresh")))
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!((fresh.revision, fresh.position), (Revision::ONE, 1));
        assert_eq!(
            adapter.read("p", deadline(), |u| u.get("fresh")).unwrap(),
            Some(fresh)
        );
        assert!(
            matches!(result, Err(SqliteError::TransactionAborted)),
            "{result:?}"
        );
        handle.close();
    });
}

// Pre-R140 observation scaffolding, not the composed Storage contract. This
// private helper currently permits a callback to catch EntityCorrupt and commit.
// Before composition/activation, every NativeError must be observed at its source:
// caught logical corruption must latch failure and abort the whole transaction.
// The composition and #127/R140 owners track that blocking precondition.
#[test]
fn a_drifted_projection_does_not_partially_move_a_goal() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        adapter
            .write("p".into(), deadline(), |u| u.create(goal("g")))
            .unwrap()
            .wait()
            .unwrap();
        handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("UPDATE goals SET title='drift' WHERE project_id='p'", [])?;
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        adapter
            .write("p".into(), deadline(), |u| {
                assert!(matches!(
                    u.move_to_end("g", Revision::ONE),
                    Err(SqliteError::EntityCorrupt {
                        family: "goal",
                        check: "projection",
                        ..
                    })
                ));
                assert_eq!(u.create(goal("next"))?.position, 2);
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        handle
            .submit(Lane::Entities, None, |tx| {
                let row: (i64, i64) = tx.query_row(
                    "SELECT revision,position FROM goals WHERE project_id='p' AND id='g'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                assert_eq!(row, (1, 1));
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        handle.close();
    });
}

#[test]
fn damaged_position_counters_are_typed_and_refused_without_mutation() {
    for damaged in [None, Some(-1_i64), Some(0), Some(1)] {
        let (_temp, data) = test_data();
        setup(&data);
        let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        std::thread::scope(|scope| {
            let _close = Close(&handle);
            scope.spawn(move || writer.run());
            let adapter = adapter_for(&handle, &pool, &data);
            adapter
                .write("p".into(), deadline(), |u| {
                    u.create(goal("first"))?;
                    u.create(goal("second"))?;
                    Ok(())
                })
                .unwrap()
                .wait()
                .unwrap();
            handle.submit(Lane::Entities, None, move |tx| {
                tx.execute("DELETE FROM counters WHERE project_id='p' AND name='position:goal'", [])?;
                if let Some(value) = damaged {
                    tx.execute("INSERT INTO counters(project_id,name,value) VALUES('p','position:goal',?1)", [value])?;
                }
                Ok(())
            }).unwrap().wait().unwrap();
            // Catch both errors inside a successful writer transaction: neither
            // operation may alter the row, revision or durable high-water mark.
            adapter.write("p".into(), deadline(), move |u| {
                let before = u.get("first")?.unwrap();
                for result in [u.create(goal("refused")), u.move_to_end("first", before.revision)] {
                    let error = result.unwrap_err();
                    match damaged {
                        None => assert!(matches!(error, SqliteError::CounterMissing { project, name: "position:goal" } if project == "p")),
                        Some(value) if value < 0 => assert!(matches!(error, SqliteError::CounterNegative { project, name: "position:goal", value: -1 } if project == "p")),
                        Some(value) => assert!(matches!(error, SqliteError::CounterBehind { project, name: "position:goal", value: actual } if project == "p" && actual == value)),
                    }
                }
                assert_eq!(u.get("first")?.unwrap(), before);
                assert!(u.get("refused")?.is_none());
                let actual: Option<i64> = u.sql.tx().query_row(
                    "SELECT value FROM counters WHERE project_id='p' AND name='position:goal'", [], |r| r.get(0)
                ).optional()?;
                assert_eq!(actual, damaged);
                Ok(())
            }).unwrap().wait().unwrap();
            assert_eq!(
                adapter
                    .write("q".into(), deadline(), |u| u.create(goal("independent")))
                    .unwrap()
                    .wait()
                    .unwrap()
                    .position,
                1
            );
            handle.submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO counters(project_id,name,value) VALUES('p','position:goal',2) ON CONFLICT(project_id,name) DO UPDATE SET value=2", [])?;
                Ok(())
            }).unwrap().wait().unwrap();
            assert_eq!(
                adapter
                    .write("p".into(), deadline(), |u| u.create(goal("after")))
                    .unwrap()
                    .wait()
                    .unwrap()
                    .position,
                3
            );
        });
    }
}

// Compare stored values, including detail bytes, ordered chunk memberships, blob
// registrations and the allocator. A hydrated record alone misses leaked state.
fn stored_goal_state(tx: &rusqlite::Transaction<'_>) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "goals",
        "entity_details",
        "entity_blob_refs",
        "blobs",
        "counters",
    ]
    .into_iter()
    .map(|table| {
        let columns = tx
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap()
            .column_count();
        let order = (1..=columns)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = tx
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn a_zero_stored_position_is_corruption_instead_of_a_readable_goal() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        adapter
            .write("p".into(), deadline(), |u| {
                u.create(goal("damaged"))?;
                // Zero is allowed by the schema, so this exercises reader validation.
                assert_eq!(
                    u.sql
                        .tx()
                        .execute("UPDATE goals SET position=0 WHERE id='damaged'", [])?,
                    1
                );
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        assert!(matches!(
            adapter.read("p", deadline(), |u| u.get("damaged")),
            Err(SqliteError::EntityCorrupt {
                family: "goal",
                check: "counter",
                ..
            })
        ));
    });
}

#[test]
fn a_zero_surviving_position_refuses_create_and_move_without_mutation() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        adapter
            .write("p".into(), deadline(), |u| {
                u.create(goal("damaged"))?;
                let healthy = u.create(goal("healthy"))?;
                assert_eq!(
                    u.sql
                        .tx()
                        .execute("UPDATE goals SET position=0 WHERE id='damaged'", [])?,
                    1
                );
                let before = stored_goal_state(u.sql.tx());
                // Pre-R140 helper seam only; composition must observe caught corruption.
                for operation in 0..2 {
                    let result = if operation == 0 {
                        u.create(goal("refused"))
                    } else {
                        u.move_to_end("healthy", healthy.revision)
                    };
                    assert!(matches!(
                        result,
                        Err(SqliteError::EntityCorrupt {
                            family: "goal",
                            check: "counter",
                            locator,
                            ..
                        }) if locator == "position"
                    ));
                    assert_eq!(stored_goal_state(u.sql.tx()), before);
                }
                assert_eq!(u.get("healthy")?, Some(healthy));
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
    });
}

#[test]
fn an_unsupported_detail_format_is_corruption_before_detail_is_removed() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        adapter
            .write("p".into(), deadline(), |u| {
                u.create(goal("g"))?;
                assert_eq!(
                    u.sql
                        .tx()
                        .execute("UPDATE entity_details SET format=2 WHERE family='goal'", [])?,
                    1
                );
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        assert!(matches!(
            adapter.read("p", deadline(), |u| u.get("g")),
            Err(SqliteError::EntityCorrupt {
                family: "goal",
                check: "detail metadata",
                ..
            })
        ));
    });
}

#[test]
fn a_caught_counter_write_failure_rolls_back_the_move_and_trigger_effects() {
    caught_savepoint_failure_restores_goal_state(false);
}

#[test]
fn a_caught_delete_failure_restores_detail_references_and_trigger_effects() {
    caught_savepoint_failure_restores_goal_state(true);
}

fn caught_savepoint_failure_restores_goal_state(delete: bool) {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = adapter_for(&handle, &pool, &data);
        let (original, next) = adapter.write("p".into(), deadline(), move |u| {
            u.create(goal("earlier"))?;
            let original = u.create(Goal { description: "λ".repeat(32_769), ..goal("g") })?;
            let refs: i64 = u.sql.tx().query_row("SELECT count(*) FROM entity_blob_refs WHERE family='goal' AND entity_key='g'", [], |r| r.get(0))?;
            assert!(refs > 0, "delete must clear actual indirect references");
            let trigger = if delete {
                "CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON goals WHEN OLD.id='g' BEGIN
                 INSERT INTO counters(project_id,name,value) VALUES('p','trigger-effect',99);
                 SELECT RAISE(FAIL,'synthetic delete'); END"
            } else {
                "CREATE TEMP TRIGGER fail_move BEFORE UPDATE ON counters WHEN OLD.name='position:goal' BEGIN
                 INSERT INTO counters(project_id,name,value) VALUES('p','trigger-effect',99);
                 SELECT RAISE(FAIL,'synthetic move'); END"
            };
            u.sql.tx().execute_batch(trigger)?;
            let before = stored_goal_state(u.sql.tx());
            let error = if delete { u.delete("g", original.revision) }
                else { u.move_to_end("g", original.revision).map(|_| ()) }.unwrap_err();
            assert!(matches!(error, SqliteError::Sql(rusqlite::Error::SqliteFailure(ref e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER), "{error:?}");
            // FAIL preserves trigger writes unless the operation rolls back its
            // savepoint. Delete has already cleared detail/refs; move has already
            // updated revision/position before its counter write fails.
            assert_eq!(stored_goal_state(u.sql.tx()), before);
            assert_eq!(u.get("g")?, Some(original.clone()));
            u.sql.tx().execute_batch(if delete { "DROP TRIGGER fail_delete" } else { "DROP TRIGGER fail_move" })?;
            let next = u.create(goal("after"))?;
            assert_eq!(next.position, 3);
            Ok((original, next))
        }).unwrap().wait().unwrap();
        adapter
            .read("p", deadline(), |u| {
                assert_eq!(u.get("g")?, Some(original));
                assert!(u.get("earlier")?.is_some());
                assert_eq!(u.get("after")?, Some(next));
                let effects: i64 = u.sql.tx().query_row(
                    "SELECT count(*) FROM counters WHERE name='trigger-effect'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(effects, 0);
                Ok(())
            })
            .unwrap();
    });
}
