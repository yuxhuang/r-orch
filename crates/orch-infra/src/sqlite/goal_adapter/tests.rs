use super::*;
use crate::sqlite::{ConnectionRole, Synchronous, Writer, migrations, open_connection, test_data};
use crate::storage::v2::FileStorage;
use orch_core::store::{Caller, EntityKind, Principal, StorageExt, StoreError};
use orch_domain::GoalStatus;
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
            let adapter = GoalAdapter::new(&handle, &pool);
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
        let adapter = GoalAdapter::new(&handle, &pool);
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
        let adapter = GoalAdapter::new(&handle, &pool);
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
        let adapter = GoalAdapter::new(&handle, &pool);
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
fn timestamps_are_lossless_or_refused_before_any_write() {
    for time in ["2026-11-22T00:00:00.123Z", "1969-12-31T23:59:59.999Z"] {
        let goal = Goal {
            created_at: time.into(),
            updated_at: Some(time.into()),
            ..goal("g")
        };
        let row = encode(&goal).unwrap();
        assert_eq!(decode(row).unwrap().entity, Entity::Goal(Box::new(goal)));
    }
    for time in [
        "bad",
        "2026-11-22T00:00:00.123456Z",
        "2026-11-22T00:00:00Z",
        "2026-11-22T01:00:00.123+01:00",
    ] {
        assert!(
            encode(&Goal {
                created_at: time.into(),
                ..goal("g")
            })
            .is_err(),
            "{time}"
        );
        assert!(
            encode(&Goal {
                updated_at: Some(time.into()),
                ..goal("g")
            })
            .is_err(),
            "{time}"
        );
    }
    let mut row = encode(&goal("g")).unwrap();
    row.created_at = i64::MAX;
    assert!(decode(row).is_err());
}

#[test]
fn invalid_timestamp_and_exhausted_or_unseeded_counters_consume_nothing() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = GoalAdapter::new(&handle, &pool);
        adapter
            .write("p".into(), deadline(), |u| {
                assert!(matches!(
                    u.create(Goal {
                        created_at: "2026-11-22T00:00:00.123456Z".into(),
                        ..goal("invalid")
                    }),
                    Err(SqliteError::Unrepresentable(_))
                ));
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
        assert!(matches!(error, SqliteError::Sql(_)));
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
        let adapter = GoalAdapter::new(&handle, &pool);
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
        let adapter = GoalAdapter::new(&handle, &pool);
        let result = adapter
            .write("p".into(), deadline(), move |u| {
                u.create(goal("before"))?;
                // Deterministically simulate SQLite ending the outer transaction
                // on an error outside an operation's savepoint (e.g. IOERR).
                u.read.tx.execute_batch("ROLLBACK")?;
                assert!(u.read.tx.is_autocommit());
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
                let counters: i64 = u.tx.query_row(
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
fn unrepresentable_stored_timestamp_does_not_partially_move_a_goal() {
    let (_temp, data) = test_data();
    setup(&data);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|scope| {
        let _close = Close(&handle);
        scope.spawn(move || writer.run());
        let adapter = GoalAdapter::new(&handle, &pool);
        adapter
            .write("p".into(), deadline(), |u| u.create(goal("g")))
            .unwrap()
            .wait()
            .unwrap();
        handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute(
                    "UPDATE goals SET created_at=?1 WHERE project_id='p'",
                    [i64::MAX],
                )?;
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        adapter
            .write("p".into(), deadline(), |u| {
                assert!(matches!(
                    u.move_to_end("g", Revision::ONE),
                    Err(SqliteError::Unrepresentable(_))
                ));
                assert_eq!(u.create(goal("next"))?.position, 2);
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        handle
            .submit(Lane::Entities, None, |tx| {
                let row = entity::get_goal(tx, "p", "g").map_err(classify)?.unwrap();
                assert_eq!((row.revision, row.position), (Revision::ONE, 1));
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        handle.close();
    });
}
