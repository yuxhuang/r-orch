//! The shared intent scenarios of the storage contract (`intents_v2.rs`: three
//! intents, rollback, signed retention edges, a caught refusal) run through the
//! real `Writer` and `ReadPool`. These component fixtures cover the same intent
//! refusals; the registered public SQLite conformance case additionally creates
//! and reads a Task marker to prove that unrelated work commits. This leaf-only
//! fixture has no Task adapter and leaves that composition assertion to the
//! shared suite (`an_error_caught_inside_the_callback_changes_nothing`).
use super::*;
use crate::sqlite::{
    ConnectionRole, Lane, ReadPool, Synchronous, Writer, WriterHandle, migrations, open_connection,
    test_data,
};
use orch_core::store::{Principal, leases::NewLease};
use std::time::{Duration, Instant};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn owner() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn remove(task: &str) -> IntentPayload {
    IntentPayload::WorkspaceRemove { task: task.into() }
}

fn blob() -> IntentPayload {
    IntentPayload::BlobDelete { hash: HASH.into() }
}

fn stop(run: &str) -> IntentPayload {
    IntentPayload::ProcessStop { run: run.into() }
}

/// The single writer and one pooled reader on a fresh migrated data directory.
fn scenario(body: impl FnOnce(&WriterHandle, &ReadPool<'_>)) {
    struct Close<'a>(&'a WriterHandle);
    impl Drop for Close<'_> {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let (_temp, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut connection).unwrap();
    drop(connection);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 4).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    std::thread::scope(|threads| {
        let _close = Close(&handle);
        threads.spawn(move || writer.run());
        body(&handle, &pool);
    });
}

fn write<T: Send + 'static>(
    handle: &WriterHandle,
    who: Caller,
    work: impl FnOnce(&mut Intents<'_, '_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    handle
        .submit(
            Lane::Entities,
            Some(Instant::now() + Duration::from_secs(10)),
            move |tx| work(&mut Intents::new(tx, &who)),
        )?
        .wait()
}

fn read<T>(pool: &ReadPool<'_>, work: impl FnOnce(&Intents<'_, '_>) -> Result<T>) -> T {
    pool.read(Instant::now() + Duration::from_secs(10), |tx| {
        let who = owner();
        work(&Intents::new(tx, &who))
    })
    .unwrap()
}

fn ids(rows: Vec<Intent>) -> Vec<u64> {
    rows.into_iter().map(|i| i.id).collect()
}

/// Issue 167: add three intents, finish the middle one, read it back inside the
/// writer transaction, page the pending rows by `after` and `limit`, then fail
/// the unit of work. A fresh snapshot must hold nothing and the ids must be
/// unspent.
#[test]
fn three_intents_read_inside_the_writer_and_vanish_on_rollback() {
    scenario(|handle, pool| {
        let failed = write(handle, owner(), |intents| {
            // Creation times deliberately oppose id order.
            let first = intents.add_intent(&blob(), 30)?;
            let mut middle = intents.add_intent(&remove("t1"), 20)?;
            let last = intents.add_intent(&remove("t2"), 10)?;
            assert_eq!(
                intents.finish_intent(middle.id, 40)?,
                FinishOutcome::Finished
            );
            middle.done_at = Some(40);
            assert_eq!(intents.get_intent(middle.id)?, Some(middle));
            assert_eq!(intents.list_pending_intents(0, 1)?, vec![first.clone()]);
            assert_eq!(
                intents.list_pending_intents(0, 3)?,
                vec![first.clone(), last.clone()]
            );
            assert_eq!(
                intents.list_pending_intents(first.id, 1)?,
                vec![last.clone()]
            );
            assert!(intents.list_pending_intents(last.id, 1)?.is_empty());
            Err::<(), _>(StoreError::CounterExhausted.into())
        });
        assert!(matches!(
            failed,
            Err(SqliteError::Store(StoreError::CounterExhausted))
        ));
        read(pool, |intents| {
            for id in 1..=3 {
                assert_eq!(intents.get_intent(id)?, None);
            }
            assert!(intents.list_pending_intents(0, 3)?.is_empty());
            Ok(())
        });
        // The counter rolled back with the rows.
        assert_eq!(
            write(handle, owner(), |intents| intents.add_intent(&blob(), 50))
                .unwrap()
                .id,
            1
        );
    });
}

/// The same sequence, committed: what the writer saw in its own transaction is
/// what a fresh snapshot then reads (R36), and `done_at` is the finishing time.
#[test]
fn the_committed_three_intents_read_as_the_writer_saw_them() {
    scenario(|handle, pool| {
        let seen = write(handle, owner(), |intents| {
            let first = intents.add_intent(&blob(), 30)?;
            let middle = intents.add_intent(&remove("t1"), 20)?;
            let last = intents.add_intent(&remove("t2"), 10)?;
            intents.finish_intent(middle.id, 40)?;
            Ok((first, last, intents.list_pending_intents(0, 3)?))
        })
        .unwrap();
        let (first, last, in_writer) = seen;
        assert_eq!(in_writer, vec![first.clone(), last.clone()]);
        read(pool, |intents| {
            assert_eq!(intents.list_pending_intents(0, 3)?, in_writer);
            assert_eq!(intents.list_pending_intents(first.id, 1)?, vec![last]);
            let middle = intents.get_intent(2)?.unwrap();
            assert_eq!((middle.created_at, middle.done_at), (20, Some(40)));
            Ok(())
        });
    });
}

/// One retention rule for every backend: old enough is
/// `done_at <= now - 7 days`, and when the subtraction passes `i64::MIN` nothing
/// is old enough. The sequence is that of
/// `retention_never_underflows_below_the_signed_minimum`.
#[test]
fn signed_minimum_retention_matches_the_shared_contract_sequence() {
    scenario(|handle, pool| {
        let first = i64::MIN + DONE_RETENTION_MS;
        let delete = |now: i64| {
            write(handle, owner(), move |intents| {
                intents.delete_finished_intents(now, 10)
            })
            .unwrap()
        };
        write(handle, owner(), |intents| {
            for _ in 0..3 {
                intents.add_intent(&blob(), i64::MIN)?;
            }
            intents.finish_intent(1, i64::MIN)?;
            intents.finish_intent(2, i64::MIN + 1)?;
            Ok(())
        })
        .unwrap();
        for now in [i64::MIN, i64::MIN + 1, first - 1] {
            assert_eq!(delete(now), 0, "now {now}");
            read(pool, |intents| {
                assert_eq!(intents.get_intent(1)?.unwrap().done_at, Some(i64::MIN));
                Ok(())
            });
        }
        assert_eq!(delete(first), 1);
        assert_eq!(delete(first + 1), 1);
        read(pool, |intents| {
            assert_eq!(
                (intents.get_intent(1)?, intents.get_intent(2)?),
                (None, None)
            );
            Ok(())
        });
        // The pending row at the minimum stays however late it is.
        assert_eq!(delete(i64::MAX), 0);
        write(handle, owner(), |intents| {
            intents.finish_intent(3, i64::MAX)
        })
        .unwrap();
        assert_eq!(delete(i64::MAX), 0);
        read(pool, |intents| {
            assert!(intents.get_intent(3)?.is_some());
            Ok(())
        });
    });
}

/// The exact seven-day boundary away from the minimum, in one writer: kept one
/// millisecond before the cutoff, deleted at it, and the deleted id is not
/// handed out again.
#[test]
fn the_seven_day_boundary_is_exact_and_ids_stay_spent() {
    scenario(|handle, _pool| {
        let done_at = 1_000;
        write(handle, owner(), move |intents| {
            intents.add_intent(&blob(), 0)?;
            intents.finish_intent(1, done_at)?;
            Ok(())
        })
        .unwrap();
        let delete = |now: i64| {
            write(handle, owner(), move |intents| {
                intents.delete_finished_intents(now, 10)
            })
            .unwrap()
        };
        let cutoff = done_at + DONE_RETENTION_MS;
        assert_eq!(delete(cutoff - 1), 0);
        assert_eq!(delete(cutoff), 1);
        assert_eq!(
            write(handle, owner(), |intents| intents.add_intent(&blob(), 0))
                .unwrap()
                .id,
            2
        );
    });
}

/// R132 through the real writer: refusals caught inside a callback that then
/// commits leave no row, no time and no spent id, and a lease claimed in one
/// transaction is the authority of the next.
#[test]
fn caught_refusals_in_a_committing_callback_change_nothing() {
    scenario(|handle, pool| {
        let lease = write(handle, owner(), |intents| {
            let claim = leases::claim_lease(
                intents.tx,
                intents.caller,
                &NewLease {
                    task: "t1".into(),
                    agent: "a1".into(),
                    run: "r1".into(),
                },
                0,
            )?;
            match claim {
                orch_core::store::leases::ClaimOutcome::Granted(lease) => Ok(lease),
                other => panic!("fresh claim blocked: {other:?}"),
            }
        })
        .unwrap();
        let task = Caller {
            project: "p".into(),
            principal: Principal::Task {
                identity: "task".into(),
                task: lease.task.clone(),
                run: lease.run.clone(),
                lineage: lease.lineage.clone(),
                epoch: lease.epoch + 1,
            },
        };
        // A stale task token is refused for its own run, and the refusal is caught.
        let refused = write(handle, task, |intents| {
            let refused = intents.add_intent(&stop("r1"), 1).is_err();
            Ok(refused)
        })
        .unwrap();
        assert!(refused);
        let (probe, refusals) = write(handle, owner(), |intents| {
            let refusals = [
                intents.add_intent(&remove("../escape"), 1).map(drop),
                intents
                    .add_intent(&IntentPayload::BlobDelete { hash: "x".into() }, 1)
                    .map(drop),
                intents.add_intent(&stop("r-without-lease"), 1).map(drop),
                intents
                    .add_intent(
                        &IntentPayload::ProcessStart {
                            run: "r-without-lease".into(),
                        },
                        1,
                    )
                    .map(drop),
                intents.finish_intent(0, 1).map(drop),
                intents.finish_intent(99, 1).map(drop),
                intents.delete_finished_intents(1, 0).map(drop),
                intents.delete_finished_intents(1, 1_001).map(drop),
                intents.list_pending_intents(0, 0).map(drop),
            ];
            let refused = refusals.iter().filter(|r| r.is_err()).count();
            Ok((intents.add_intent(&stop("r1"), 2)?, refused))
        })
        .unwrap();
        assert_eq!(refusals, 9);
        // No refusal, including the stale token above, spent an id.
        assert_eq!(probe.id, 1);
        read(pool, |intents| {
            assert_eq!(intents.list_pending_intents(0, 10)?, vec![probe]);
            Ok(())
        });
    });
}
