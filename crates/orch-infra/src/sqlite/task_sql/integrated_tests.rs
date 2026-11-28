use super::tests::task;
use super::writes::TaskWrite;
use super::*;
use crate::sqlite::{
    ConnectionRole, Lane, ReadPool, Synchronous, Writer, WriterHandle, migrations, open_connection,
};
use orch_core::store::{Filter, PageRequest, Principal, Sort, StoreError};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

fn owner() -> Principal {
    Principal::Owner {
        identity: "synthetic".into(),
    }
}
fn sql<'a, 'c>(
    tx: &'a Transaction<'c>,
    disk: &'a DiskBlobStore,
    state: &FailureState,
    source: FailureSource,
) -> TaskSql<'a, 'c> {
    TaskSql {
        tx,
        disk,
        project: "p",
        state: state.clone(),
        source,
    }
}
fn submit<T: Send + 'static>(
    handle: &WriterHandle,
    work: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let receipt = handle.submit(Lane::Entities, None, work)?;
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(receipt.wait());
    });
    recv.recv_timeout(Duration::from_secs(15))
        .expect("writer receipt deadline")
}
fn scenario(
    body: impl FnOnce(&crate::data_dir::DataDir, &WriterHandle, &FailureState, &Arc<DiskBlobStore>),
) {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let mut setup = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut setup).unwrap();
    drop(setup);
    let state = FailureState::new();
    let (writer, handle) =
        Writer::open_with_state(&data, Synchronous::Normal, 8, state.clone()).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(&handle);
        body(&data, &handle, &state, &disk);
        handle.close();
        worker.join().unwrap();
    });
}
fn seed(handle: &WriterHandle, disk: &Arc<DiskBlobStore>, state: &FailureState, damaged: bool) {
    let disk = disk.clone();
    let state = state.clone();
    submit(handle, move |tx| {
        let caller = owner();
        let mut write = TaskWrite::new(sql(tx, &disk, &state, FailureSource::Writer), &caller);
        write.create(task("a"))?;
        write.create(task("b"))?;
        write.ensure_committable()?;
        if damaged {
            tx.execute("DELETE FROM task_details WHERE task_id='b'", [])?;
        }
        Ok(())
    })
    .unwrap();
}
fn rows(data: &crate::data_dir::DataDir) -> i64 {
    open_connection(data, ConnectionRole::Reader, Synchronous::Normal)
        .unwrap()
        .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
        .unwrap()
}
fn request() -> PageRequest {
    PageRequest {
        limit: 1000,
        cursor: None,
        sort: Sort::Position,
        with_count: true,
    }
}
fn filter() -> Filter {
    Filter::Tasks {
        status: None,
        goal: None,
    }
}

#[test]
fn caught_task_get_and_unit_of_work_corruption_abort_every_enclosing_write() {
    for operation in 0..4 {
        caught_task_corruption_aborts_enclosing_write(operation);
    }
}

fn caught_task_corruption_aborts_enclosing_write(operation: u8) {
    scenario(|data, handle, state, disk| {
        seed(handle, disk, state, true);
        let disk = disk.clone();
        let observed = state.clone();
        let answer = submit(handle, move |tx| {
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk, &observed, FailureSource::Writer), &caller);
            write.create(task("before"))?;
            if operation != 0 {
                tx.execute(
                    "UPDATE tasks SET revision=0 WHERE project_id='p' AND id='b'",
                    [],
                )?;
            }
            let result = match operation {
                0 => sql(tx, &disk, &observed, FailureSource::Writer)
                    .get("b")
                    .map(|_| ()),
                1 => write.update(task("b"), Revision::ONE).map(|_| ()),
                2 => write.move_to_end("b", Revision::ONE).map(|_| ()),
                _ => write.delete("b", Revision::ONE),
            };
            assert!(matches!(result, Err(SqliteError::TaskCorrupt { .. })));
            assert!(matches!(
                write.create(task("after")),
                Err(SqliteError::StorageUnavailable(_))
            ));
            // Even a callback which catches the error and writes directly cannot commit.
            tx.execute("UPDATE tasks SET priority=77 WHERE id='a'", [])?;
            Ok(())
        });
        assert!(matches!(answer, Err(SqliteError::StorageUnavailable(_))));
        assert_eq!(rows(data), 2);
        let connection =
            open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT priority FROM tasks WHERE id='a'", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            -3
        );
        for lane in [
            Lane::Claims,
            Lane::Entities,
            Lane::Events,
            Lane::Maintenance,
        ] {
            let result = handle
                .submit(lane, None, |_| Ok(()))
                .and_then(|receipt| receipt.wait());
            assert!(matches!(result, Err(SqliteError::StorageUnavailable(_))));
        }
        assert_eq!(
            state.failure().unwrap().causes,
            vec![(
                crate::sqlite::Role::Operation,
                crate::sqlite::FatalCause::LogicalCorruption
            )]
        );
    });
}

#[test]
fn real_page_hydration_discards_partial_records_and_fails_shared_writer() {
    scenario(|data, handle, state, disk| {
        seed(handle, disk, state, true);
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
        let answer = pool.read(Instant::now() + Duration::from_secs(5), |tx| {
            let page =
                crate::sqlite::task_page::query(tx, "p", &filter(), &request(), None).unwrap();
            assert_eq!(page.ids, ["a", "b"]);
            assert_eq!(page.total, Some(2));
            sql(tx, disk, state, FailureSource::Reader).hydrate(&page.ids)
        });
        assert!(matches!(answer, Err(SqliteError::TaskCorrupt { .. })));
        assert_eq!(state.failure().unwrap().source, FailureSource::Reader);
        assert!(matches!(
            submit(handle, |_| Ok(())),
            Err(SqliteError::StorageUnavailable(_))
        ));
        assert_eq!(rows(data), 2);
    });
}

#[test]
fn caught_snapshot_get_and_live_tool_verification_fail_writer_but_artifact_does_not() {
    for source in [
        FailureSource::Reader,
        FailureSource::Tool,
        FailureSource::Artifact,
    ] {
        scenario(|data, handle, state, disk| {
            seed(handle, disk, state, true);
            let pool =
                ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
            let answer = pool.read(Instant::now() + Duration::from_secs(5), |tx| {
                let leaf = sql(tx, disk, state, source);
                assert!(leaf.get("a")?.is_some());
                assert!(matches!(
                    leaf.get("b"),
                    Err(SqliteError::TaskCorrupt { .. })
                ));
                Ok("caught")
            });
            if source == FailureSource::Artifact {
                assert_eq!(answer.unwrap(), "caught");
                assert!(state.failure().is_none());
                submit(handle, |_| Ok(())).unwrap();
            } else {
                assert!(matches!(answer, Err(SqliteError::StorageUnavailable(_))));
                assert_eq!(state.failure().unwrap().source, source);
                assert!(matches!(
                    submit(handle, |_| Ok(())),
                    Err(SqliteError::StorageUnavailable(_))
                ));
            }
        });
    }
}

#[test]
fn selecting_snapshot_and_hydration_agree_across_concurrent_commit() {
    scenario(|data, handle, state, disk| {
        seed(handle, disk, state, false);
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
        let records = pool
            .read(Instant::now() + Duration::from_secs(5), |tx| {
                let page =
                    crate::sqlite::task_page::query(tx, "p", &filter(), &request(), None).unwrap();
                let disk2 = disk.clone();
                let state2 = state.clone();
                submit(handle, move |tx| {
                    let caller = owner();
                    let mut write =
                        TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
                    let mut changed = task("b");
                    changed.title = "new".into();
                    write.update(changed, Revision::ONE)?;
                    write.create(task("c"))?;
                    write.ensure_committable()
                })?;
                assert_eq!(page.total, Some(2));
                sql(tx, disk, state, FailureSource::Reader).hydrate(&page.ids)
            })
            .unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.revision == Revision::ONE));
        assert_eq!(records[1].entity, Entity::Task(Box::new(task("b"))));
        pool.read(Instant::now() + Duration::from_secs(5), |tx| {
            let record = sql(tx, disk, state, FailureSource::Reader)
                .get("b")?
                .unwrap();
            assert_eq!(record.revision.get(), 2);
            assert!(
                sql(tx, disk, state, FailureSource::Reader)
                    .get("c")?
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(rows(data), 3);
        assert!(state.failure().is_none());
    });
}

#[test]
fn absent_stale_timeout_and_caught_ordinary_rollback_leave_writer_usable() {
    scenario(|data, handle, state, disk| {
        seed(handle, disk, state, false);
        let expired = handle
            .submit(
                Lane::Entities,
                Some(Instant::now() - Duration::from_secs(1)),
                |_| Ok(()),
            )
            .unwrap();
        assert!(matches!(expired.wait(), Err(SqliteError::Overloaded)));
        let disk = disk.clone();
        let state2 = state.clone();
        submit(handle, move |tx| {
            let leaf = sql(tx, &disk, &state2, FailureSource::Writer);
            assert!(leaf.get("absent")?.is_none());
            let caller = owner(); let mut write = TaskWrite::new(leaf, &caller);
            assert!(matches!(write.delete("a", Revision::ONE.next()?), Err(SqliteError::Store(StoreError::StaleRevision { .. }))));
            tx.execute_batch("CREATE TEMP TRIGGER refuse_task_delete BEFORE DELETE ON task_details BEGIN SELECT RAISE(ABORT,'ordinary'); END;")?;
            assert!(matches!(write.delete("a", Revision::ONE), Err(SqliteError::Sql(_))));
            write.create(task("unrelated"))?;
            write.ensure_committable()
        }).unwrap();
        assert_eq!(rows(data), 3);
        assert!(state.failure().is_none());
    });
}

#[test]
fn more_than_one_page_accounts_for_every_committed_task_and_position() {
    scenario(|data, handle, state, disk| {
        let disk2 = disk.clone();
        let state2 = state.clone();
        submit(handle, move |tx| {
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
            for n in 0..1003 {
                write.create(task(&format!("t{n:04}")))?;
            }
            write.ensure_committable()
        })
        .unwrap();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
        pool.read(Instant::now() + Duration::from_secs(5), |tx| {
            let mut after = None;
            let mut records = Vec::new();
            loop {
                let page =
                    crate::sqlite::task_page::query(tx, "p", &filter(), &request(), after.as_ref())
                        .unwrap();
                assert_eq!(page.total, Some(1003));
                records.extend(sql(tx, disk, state, FailureSource::Reader).hydrate(&page.ids)?);
                after = page.next;
                if after.is_none() {
                    break;
                }
            }
            assert_eq!(records.len(), 1003);
            for (n, record) in records.iter().enumerate() {
                assert_eq!(record.position, (n + 1) as u64);
                assert_eq!(record.revision, Revision::ONE);
                assert_eq!(
                    record.entity,
                    Entity::Task(Box::new(task(&format!("t{n:04}"))))
                );
            }
            assert_eq!(
                tx.query_row(
                    "SELECT value FROM counters WHERE project_id='p' AND name='position:task'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1003
            );
            Ok(())
        })
        .unwrap();
    });
}

#[test]
fn transient_real_object_access_can_be_caught_then_writer_recovers() {
    use std::os::unix::fs::PermissionsExt;
    scenario(|data, handle, state, disk| {
        let disk2 = disk.clone();
        let state2 = state.clone();
        let mut input = task("large");
        input.description = "x".repeat(70000);
        let hash = BlobHash::of(input.description.as_bytes()).to_string();
        submit(handle, move |tx| {
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
            write.create(input)?;
            write.ensure_committable()
        })
        .unwrap();
        let path = data
            .database_path()
            .parent()
            .unwrap()
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(hash);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let disk2 = disk.clone();
        let state2 = state.clone();
        let result = submit(handle, move |tx| {
            assert!(matches!(
                sql(tx, &disk2, &state2, FailureSource::Writer).get("large"),
                Err(SqliteError::TaskAccess(
                    std::io::ErrorKind::PermissionDenied
                ))
            ));
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
            write.create(task("unrelated"))?;
            write.ensure_committable()
        });
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        result.unwrap();
        // Deny creation in the real publication directory, then recover with an
        // inline write. This is access failure, never evidence of row damage.
        let staging = data.database_path().parent().unwrap().join("blobs/tmp");
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o000)).unwrap();
        let disk2 = disk.clone();
        let state2 = state.clone();
        let result = submit(handle, move |tx| {
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
            let mut input = task("denied");
            input.description = "y".repeat(70000);
            assert!(matches!(
                write.create(input),
                Err(SqliteError::TaskPublication(_))
            ));
            write.create(task("recovered"))?;
            write.ensure_committable()
        });
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
        result.unwrap();
        assert!(state.failure().is_none());
        assert_eq!(rows(data), 3);
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
        assert!(
            pool.read(Instant::now() + Duration::from_secs(5), |tx| sql(
                tx,
                disk,
                state,
                FailureSource::Reader
            )
            .get("large"))
                .unwrap()
                .is_some()
        );
    });
}

#[test]
fn deliberately_swallowed_task_cleanup_unwind_still_refuses_commit() {
    scenario(|data, handle, state, disk| {
        seed(handle, disk, state, false);
        let disk2 = disk.clone();
        let state2 = state.clone();
        let result = submit(handle, move |tx| {
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
            write.create(task("before"))?;
            tx.execute_batch("CREATE TEMP TRIGGER end_outer BEFORE DELETE ON task_details BEGIN SELECT RAISE(ROLLBACK,'injected'); END;")?;
            let unwind = super::write_tests::cleanup_unwind(|| write.delete("a", Revision::ONE));
            assert!(matches!(
                unwind.error(),
                SqliteError::OperationRollback { .. }
            ));
            assert!(tx.is_autocommit());
            assert!(matches!(
                write.ensure_committable(),
                Err(SqliteError::TransactionAborted)
            ));
            Ok(())
        });
        assert!(matches!(result, Err(SqliteError::TransactionAborted)));
        assert_eq!(rows(data), 2);
        assert!(state.failure().is_none());
        let disk2 = disk.clone();
        let state2 = state.clone();
        submit(handle, move |tx| {
            let caller = owner();
            let mut write =
                TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
            write.create(task("recovered"))?;
            write.ensure_committable()
        })
        .unwrap();
        assert_eq!(rows(data), 3);
    });
}

#[test]
fn committed_registration_conflict_is_fatal_but_matching_registration_commits() {
    for conflict in [false, true] {
        scenario(|data, handle, state, disk| {
            let mut input = task("large");
            input.description = "x".repeat(70000);
            let hash = disk.put(input.description.as_bytes()).unwrap().to_string();
            let hash2 = hash.clone();
            submit(handle, move |tx| {
                tx.execute(
                    "INSERT INTO blobs VALUES('p',?1,?2,'disk',0,NULL)",
                    params![hash2, 70000 + i64::from(conflict)],
                )?;
                Ok(())
            })
            .unwrap();
            let disk2 = disk.clone();
            let state2 = state.clone();
            let result = submit(handle, move |tx| {
                let caller = owner();
                let mut write =
                    TaskWrite::new(sql(tx, &disk2, &state2, FailureSource::Writer), &caller);
                let result = write.create(input);
                if conflict {
                    assert!(matches!(
                        result,
                        Err(SqliteError::TaskCorrupt {
                            check: "blob registration",
                            ..
                        })
                    ));
                    Ok(()) // Catch the fatal; the executor must still roll back.
                } else {
                    result.map(|_| ())
                }
            });
            assert_eq!(rows(data), if conflict { 0 } else { 1 });
            let reader =
                open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
            assert_eq!(
                reader
                    .query_row(
                        "SELECT size FROM blobs WHERE project_id='p' AND sha256=?1",
                        [hash],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                70000 + i64::from(conflict)
            );
            if conflict {
                assert!(matches!(result, Err(SqliteError::StorageUnavailable(_))));
                assert!(matches!(
                    submit(handle, |_| Ok(())),
                    Err(SqliteError::StorageUnavailable(_))
                ));
            } else {
                result.unwrap();
                assert!(state.failure().is_none());
            }
        });
    }
}

#[test]
fn current_project_lease_fences_task_mutation_in_the_same_writer_transaction() {
    use crate::sqlite::leases;
    use orch_core::store::{
        Caller,
        leases::{ClaimOutcome, NewLease},
    };
    scenario(|data, handle, state, disk| {
        seed(handle, disk, state, false);
        let lease = submit(handle, |tx| {
            let caller = Caller {
                project: "p".into(),
                principal: owner(),
            };
            match leases::claim_lease(
                tx,
                &caller,
                &NewLease {
                    task: "a".into(),
                    agent: "agent".into(),
                    run: "run".into(),
                },
                0,
            )? {
                ClaimOutcome::Granted(lease) => Ok(lease),
                other => panic!("expected lease, got {other:?}"),
            }
        })
        .unwrap();
        for case in 0..4 {
            let mut caller = Caller {
                project: "p".into(),
                principal: Principal::Task {
                    identity: "synthetic".into(),
                    task: "a".into(),
                    run: "run".into(),
                    lineage: lease.lineage.clone(),
                    epoch: lease.epoch,
                },
            };
            if case == 2
                && let Principal::Task { task, .. } = &mut caller.principal
            {
                *task = "other".into();
            }
            let lineage = if case == 0 {
                "stale".into()
            } else {
                lease.lineage.clone()
            };
            let epoch = lease.epoch + u64::from(case == 1);
            let disk2 = disk.clone();
            let state2 = state.clone();
            let result = submit(handle, move |tx| {
                let current = leases::get_lease(tx, &caller, "a")?.unwrap();
                leases::check(tx, &caller, &current, &lineage, epoch)?;
                let mut write = TaskWrite::new(
                    sql(tx, &disk2, &state2, FailureSource::Writer),
                    &caller.principal,
                );
                let mut changed = task("a");
                changed.title = "fenced".into();
                write.update(changed, Revision::ONE)?;
                write.ensure_committable()
            });
            let reader =
                open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
            let stored: (String, i64) = reader
                .query_row(
                    "SELECT title,revision FROM tasks WHERE project_id='p' AND id='a'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            if case < 3 {
                assert!(matches!(
                    result,
                    Err(SqliteError::Store(StoreError::StaleEpoch))
                ));
                assert_eq!(stored, ("sample".into(), 1));
            } else {
                result.unwrap();
                assert_eq!(stored, ("fenced".into(), 2));
            }
            assert!(state.failure().is_none());
        }
    });
}
