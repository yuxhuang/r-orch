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
