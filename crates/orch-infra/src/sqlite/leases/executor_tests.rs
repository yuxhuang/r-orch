use super::{adapter::LeaseAdapter, tests::*, *};
use crate::sqlite::{
    ConnectionRole, Lane, ReadPool, Synchronous, Writer, WriterHandle, open_connection, test_data,
};
use orch_core::Clock;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::time::{Duration, Instant};

struct ClockAt(i64);
impl Clock for ClockAt {
    fn now_ms(&self) -> i64 {
        self.0
    }
    fn now_iso(&self) -> String {
        orch_core::clock::iso_from_epoch_ms(self.0)
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn executor(test: impl FnOnce(&WriterHandle, &ReadPool<'_>)) {
    let (_temp, data) = test_data();
    let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch(include_str!("../schema.sql")).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 16).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || writer.run());
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(&handle);
        test(&handle, &pool);
    });
}

#[test]
fn receipts_preserve_typed_outcomes_and_project_bound_reads() {
    executor(|writer, readers| {
        let adapter = LeaseAdapter {
            writer,
            readers,
            caller: owner("p"),
            clock: &ClockAt(-1),
        };
        let lease = match adapter
            .claim(new("t", "a", "r"))
            .unwrap()
            .wait()
            .unwrap()
            .unwrap()
        {
            ClaimOutcome::Granted(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(lease.expires_at, LEASE_MS - 1);
        assert_eq!(
            adapter
                .claim(new("t", "b", "s"))
                .unwrap()
                .wait()
                .unwrap()
                .unwrap(),
            ClaimOutcome::Held(lease.clone())
        );
        assert!(matches!(
            adapter
                .renew("t".into(), "wrong".into(), 1)
                .unwrap()
                .wait()
                .unwrap(),
            Err(StoreError::StaleEpoch)
        ));
        assert_eq!(
            adapter
                .snapshot(deadline(), |tx, c| get_lease(tx, c, "t"))
                .unwrap()
                .unwrap(),
            Some(lease.clone())
        );
        let other = LeaseAdapter {
            caller: owner("other"),
            ..adapter
        };
        assert!(
            other
                .snapshot(deadline(), list_leases)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        let holder = LeaseAdapter {
            caller: holder(&owner("p"), &lease),
            clock: &ClockAt(100),
            ..other
        };
        assert_eq!(
            holder
                .renew("t".into(), lease.lineage.clone(), 1)
                .unwrap()
                .wait()
                .unwrap()
                .unwrap()
                .expires_at,
            LEASE_MS + 100
        );
        holder
            .release("t".into(), lease.lineage.clone(), 1)
            .unwrap()
            .wait()
            .unwrap()
            .unwrap();
        assert!(
            holder
                .snapshot(deadline(), list_leases)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            holder
                .claim(new("t", "a", "r2"))
                .unwrap()
                .wait()
                .unwrap()
                .unwrap(),
            ClaimOutcome::Granted(Lease {
                run: "r2".into(),
                epoch: 2,
                expires_at: LEASE_MS + 100,
                ..lease
            })
        );
    });
}

#[test]
fn pooled_snapshot_stays_fixed_while_writer_commits() {
    executor(|writer, readers| {
        let adapter = LeaseAdapter {
            writer,
            readers,
            caller: owner("p"),
            clock: &ClockAt(0),
        };
        adapter
            .snapshot(deadline(), |tx, c| {
                assert!(list_leases(tx, c)?.is_empty());
                adapter.claim(new("t", "a", "r")).unwrap().wait().unwrap()?;
                assert!(list_leases(tx, c)?.is_empty());
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            adapter
                .snapshot(deadline(), list_leases)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
    });
}

#[test]
fn caught_insert_failure_preserves_outer_work_but_not_epoch() {
    executor(|writer, readers| {
        writer.submit(Lane::Claims, None, |tx| {
            tx.execute_batch("CREATE TRIGGER refuse_lease BEFORE INSERT ON leases BEGIN SELECT RAISE(FAIL,'synthetic insert failure'); END;")?;
            tx.execute("INSERT INTO daemon_config(key,value,revision) VALUES('outer','kept',1)",[])?;
            assert!(claim_lease(tx,&owner("p"),&new("t","a","r"),0).is_err());
            Ok(())
        }).unwrap().wait().unwrap();
        readers
            .read(deadline(), |tx| {
                assert_eq!(
                    tx.query_row(
                        "SELECT value FROM daemon_config WHERE key='outer'",
                        [],
                        |r| r.get::<_, String>(0)
                    )?,
                    "kept"
                );
                assert_eq!(
                    tx.query_row("SELECT count(*) FROM task_epochs", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .unwrap();
    });
}

#[test]
fn failed_cleanup_closes_writer_even_when_callback_handles_operation_errors() {
    let (_temp, data) = test_data();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch(include_str!("../schema.sql")).unwrap();
    // Reject only rollback-to-savepoint, leaving the partial epoch increment
    // in the outer transaction. The writer must unwind and close it.
    conn.for_migration()
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Savepoint {
                operation: rusqlite::hooks::TransactionOperation::Rollback,
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute_batch("CREATE TRIGGER refuse_lease BEFORE INSERT ON leases BEGIN SELECT RAISE(FAIL,'synthetic insert failure'); END;").unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Handling an ordinary operation error cannot turn cleanup failure
        // into a successful callback or reach the outer commit.
        let _handled = claim_lease(&tx, &owner("p"), &new("t", "a", "r"), 0);
        panic!("operation returned after failed cleanup");
    }))
    .unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(message.contains("synthetic insert failure"), "{message}");
    assert!(message.contains("cleanup failed"), "{message}");
    drop(tx); // same rollback-on-unwind used by Writer::submit
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    pool.read(deadline(), |tx| {
        assert_eq!(
            tx.query_row("SELECT count(*) FROM task_epochs", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();

    // Also exercise the real worker's unwind path when SQLite ends the
    // transaction itself, so cleanup can no longer find the savepoint.
    conn.execute_batch("CREATE TRIGGER refuse_lease BEFORE INSERT ON leases BEGIN SELECT RAISE(ROLLBACK,'synthetic rollback'); END;").unwrap();
    drop(conn);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 16).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        assert!(matches!(
            handle
                .submit(Lane::Claims, None, |tx| {
                    let _handled = claim_lease(tx, &owner("p"), &new("t", "a", "r"), 0);
                    Ok(())
                })
                .unwrap()
                .wait(),
            Err(crate::sqlite::SqliteError::WriterClosed)
        ));
        assert!(worker.join().is_err());
        assert!(matches!(
            handle.submit(Lane::Claims, None, |_| Ok(())),
            Err(crate::sqlite::SqliteError::WriterClosed)
        ));
    });
}

#[test]
fn outer_error_rolls_back_a_successful_claim() {
    executor(|writer, readers| {
        assert!(
            writer
                .submit(Lane::Claims, None, |tx| -> crate::sqlite::Result<()> {
                    claim_lease(tx, &owner("p"), &new("t", "a", "r"), 0).unwrap();
                    Err(crate::sqlite::SqliteError::WriterClosed)
                })
                .unwrap()
                .wait()
                .is_err()
        );
        readers
            .read(deadline(), |tx| {
                assert!(list_leases(tx, &owner("p")).unwrap().is_empty());
                assert_eq!(
                    tx.query_row("SELECT count(*) FROM task_epochs", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .unwrap();
    });
}

#[tokio::test]
async fn registered_lease_sequence_matches_file_reference() {
    use orch_core::store::{Lane as StoreLane, Storage};
    let root = tempfile::tempdir().unwrap();
    let file = crate::storage::v2::FileStorage::new(root.path());
    file.restore_to_lineage("synthetic-lineage").await.unwrap();
    file.write(
        owner("p"),
        StoreLane::Claims,
        Box::new(|u| {
            let first = match u.claim_lease(&new("t", "a", "r"), -1)? {
                ClaimOutcome::Granted(l) => l,
                other => panic!("{other:?}"),
            };
            assert_eq!(
                u.claim_lease(&new("t", "b", "s"), 0)?,
                ClaimOutcome::Held(first.clone())
            );
            assert!(matches!(
                u.renew_lease("t", "wrong", 1, 0),
                Err(StoreError::StaleEpoch)
            ));
            u.release_lease("t", &first.lineage, first.epoch)?;
            u.claim_lease(&new("t", "a", "r2"), 120_001)?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    let expected = file
        .read(owner("p"))
        .await
        .unwrap()
        .read_leases()
        .await
        .unwrap();
    executor(|writer, readers| {
        writer
            .submit(Lane::Claims, None, |tx| {
                tx.execute(
                    "UPDATE daemon_config SET value='synthetic-lineage' WHERE key='lineage'",
                    [],
                )?;
                let first = grant(tx, &owner("p"), new("t", "a", "r"), -1);
                assert_eq!(
                    claim_lease(tx, &owner("p"), &new("t", "b", "s"), 0).unwrap(),
                    ClaimOutcome::Held(first.clone())
                );
                assert!(matches!(
                    renew_lease(tx, &owner("p"), "t", "wrong", 1, 0),
                    Err(StoreError::StaleEpoch)
                ));
                release_lease(tx, &owner("p"), "t", &first.lineage, first.epoch).unwrap();
                claim_lease(tx, &owner("p"), &new("t", "a", "r2"), 120_001).unwrap();
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        let actual = readers
            .read(deadline(), |tx| Ok(list_leases(tx, &owner("p")).unwrap()))
            .unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn imported_high_water_issues_once_then_returns_typed_exhaustion() {
    executor(|writer, readers| {
        writer.submit(Lane::Claims, None, |tx| {
            tx.execute("INSERT INTO task_epochs(project_id,task_id,next_epoch,lost_streak) VALUES('p','t',?1,7)", [i64::MAX - 1])?;
            Ok(())
        }).unwrap().wait().unwrap();
        let adapter = LeaseAdapter {
            writer,
            readers,
            caller: owner("p"),
            clock: &ClockAt(i64::MAX - LEASE_MS),
        };
        let lease = match adapter
            .claim(new("t", "a", "r"))
            .unwrap()
            .wait()
            .unwrap()
            .unwrap()
        {
            ClaimOutcome::Granted(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(lease.epoch, (i64::MAX - 1) as u64);
        assert_eq!(lease.expires_at, i64::MAX);
        adapter
            .release("t".into(), lease.lineage, lease.epoch)
            .unwrap()
            .wait()
            .unwrap()
            .unwrap();
        assert!(matches!(
            adapter.claim(new("t", "a", "r2")).unwrap().wait().unwrap(),
            Err(StoreError::CounterExhausted)
        ));
        adapter
            .snapshot(deadline(), |tx, _| {
                assert_eq!(
                    tx.query_row(
                        "SELECT next_epoch,lost_streak FROM task_epochs",
                        [],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    )
                    .unwrap(),
                    (i64::MAX, 7)
                );
                Ok(())
            })
            .unwrap()
            .unwrap();
    });
}
