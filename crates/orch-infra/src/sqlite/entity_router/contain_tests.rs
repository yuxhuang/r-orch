//! Issue 379: the callback panic and the leaf's failed savepoint cleanup, on a
//! real `Writer`, a real database and the four real leaves (lease, intent, retry,
//! message). A callback's own panic is contained by `submit_contained`; the typed
//! unwind of a leaf whose cleanup failed is observed, raised again, and ends the
//! writer. Each test names what the running and the queued receipt answer, and
//! what the database holds afterwards.
//!
//! Requests are queued before `Writer::run` is called on the test's own thread,
//! so the order is fixed without timing and an unwinding worker is caught here.
use super::tests::{owner, payload, setup};
use super::*;
use crate::sqlite::corrupt_root_page;
use crate::sqlite::writer::contain::CleanupUnwind;
use crate::sqlite::writer::failure::{FatalCause, Role};
use crate::sqlite::{
    ConnectionRole, Lane, Synchronous, WriteReceipt, Writer, WriterHandle, open_connection,
};
use orch_core::store::intents::IntentPayload;
use orch_core::store::leases::NewLease;
use orch_core::store::retries::Retry;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// One leaf, driven the way production reaches it: through the router's unit.
/// `id` keeps the rows of different requests apart.
struct Leaf {
    name: &'static str,
    table: &'static str,
    write: fn(&Transaction<'_>, &DiskBlobStore, FailureState, &str) -> Result<()>,
}

fn unit<'a, 'conn>(
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
    disk: &'a DiskBlobStore,
    state: FailureState,
) -> EntityWrite<'a, 'conn> {
    EntityWrite::new(tx, caller, disk, state, 0)
}

const LEAVES: [Leaf; 5] = [
    Leaf {
        name: "lease through the router",
        table: "leases",
        write: |tx, disk, state, id| {
            let caller = owner("p");
            unit(tx, &caller, disk, state)
                .claim_lease(
                    &NewLease {
                        task: id.into(),
                        agent: format!("agent-{id}"),
                        run: format!("run-{id}"),
                    },
                    0,
                )
                .map(drop)
        },
    },
    Leaf {
        name: "lease called on the borrowed transaction",
        table: "leases",
        write: |tx, _, _, id| {
            crate::sqlite::leases::claim_lease(
                tx,
                &owner("p"),
                &NewLease {
                    task: id.into(),
                    agent: format!("agent-{id}"),
                    run: format!("run-{id}"),
                },
                0,
            )
            .map(drop)
            .map_err(SqliteError::from)
        },
    },
    Leaf {
        name: "intent",
        table: "intents",
        write: |tx, disk, state, id| {
            let caller = owner("p");
            unit(tx, &caller, disk, state)
                .add_intent(&IntentPayload::WorkspaceRemove { task: id.into() }, 0)
                .map(drop)
        },
    },
    Leaf {
        name: "retry",
        table: "retry_queue",
        write: |tx, disk, state, id| {
            let caller = owner("p");
            unit(tx, &caller, disk, state).put_retry(
                &Retry {
                    task: id.into(),
                    attempt: 1,
                    due_at: 0,
                    error: "synthetic".into(),
                },
                0,
            )
        },
    },
    Leaf {
        name: "message",
        table: "messages",
        write: |tx, disk, state, id| {
            let caller = owner("p");
            unit(tx, &caller, disk, state)
                .create(payload(Family::Message, id, "subject"))
                .map(drop)
        },
    },
];

fn rows(data: &crate::data_dir::DataDir, table: &str) -> i64 {
    let conn = open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// Refuse the leaf's `ROLLBACK TO`, standing in for a cleanup that SQLite cannot complete.
fn refuse_savepoint_rollbacks(writer: &mut Writer<'_>) {
    writer
        .connection_for_test()
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Savepoint {
                operation: TransactionOperation::Rollback,
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
}

/// Make the leaf's operation fail inside its savepoint with SQLite ending the whole
/// transaction (`RAISE(ROLLBACK)`), so that the savepoint is gone when the leaf's
/// cleanup looks for it.
fn lose_the_savepoint(tx: &Transaction<'_>, table: &str) {
    tx.execute_batch(&format!(
        "CREATE TEMP TRIGGER synthetic_fault BEFORE INSERT ON main.{table}
         BEGIN SELECT RAISE(ROLLBACK, 'synthetic fault'); END"
    ))
    .unwrap();
}

/// `Writer::run` on this thread, its unwind caught. A writer that wrongly survives
/// would wait for work for ever, so a watchdog closes admission after a bound: a
/// safety net that turns a regression into a failed assertion, never an oracle.
fn run_bounded(writer: Writer<'_>, handle: &WriterHandle) -> std::thread::Result<()> {
    let watchdog = handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        watchdog.close();
    });
    catch_unwind(AssertUnwindSafe(|| writer.run()))
}

fn open_writer(data: &crate::data_dir::DataDir) -> (Writer<'_>, WriterHandle) {
    Writer::open(data, Synchronous::Normal, 8).unwrap()
}

#[test]
fn a_callback_panic_is_contained_rolled_back_and_the_writer_serves_the_next_request() {
    for leaf in &LEAVES {
        let (_temp, data) = crate::sqlite::test_data();
        setup(&data);
        let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
        let (writer, handle) = open_writer(&data);
        let write = leaf.write;
        let (state, blobs) = (handle.failure_state(), disk.clone());
        let running = handle
            .submit_contained::<(), _>(Lane::Entities, None, move |tx| {
                write(tx, &blobs, state, "ran")?;
                panic!("synthetic callback panic")
            })
            .unwrap();
        let (state, blobs) = (handle.failure_state(), disk.clone());
        let queued = handle
            .submit_contained(Lane::Entities, None, move |tx| {
                write(tx, &blobs, state, "queued")?;
                Ok(7)
            })
            .unwrap();
        handle.close();
        // No unwind leaves the worker: the callback's panic never reached it.
        writer.run();

        // The running receipt carries the panic for the caller to resume ...
        let answer = running.wait().unwrap_err();
        let payload = answer
            .callback_panic()
            .unwrap_or_else(|| panic!("{}: {answer:?}", leaf.name))
            .take()
            .unwrap();
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"synthetic callback panic"),
            "{}",
            leaf.name
        );
        // ... the queued receipt commits ...
        assert_eq!(queued.wait().unwrap(), 7, "{}", leaf.name);
        // ... and only that one left a row: the panicking request rolled back.
        assert_eq!(rows(&data, leaf.table), 1, "{}", leaf.name);
        // Nothing was latched for it (record L).
        assert!(handle.failure_state().failure().is_none(), "{}", leaf.name);
    }
}

#[test]
fn a_failed_leaf_cleanup_is_not_contained_even_when_the_callback_swallows_the_error() {
    for leaf in &LEAVES {
        for contained in [true, false] {
            let label = format!("{} (contained: {contained})", leaf.name);
            let (_temp, data) = crate::sqlite::test_data();
            setup(&data);
            let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
            let (writer, handle) = open_writer(&data);
            let write = leaf.write;
            let (table, state, blobs) = (leaf.table, handle.failure_state(), disk.clone());
            let body = move |tx: &Transaction<'_>| {
                lose_the_savepoint(tx, table);
                // A callback that handles the leaf's error and goes on.
                let _handled = write(tx, &blobs, state, "ran");
                Ok(())
            };
            let running = if contained {
                handle.submit_contained(Lane::Entities, None, body)
            } else {
                handle.submit(Lane::Entities, None, body)
            }
            .unwrap();
            let queued = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();

            let unwound = run_bounded(writer, &handle).unwrap_err();
            // The worker ended by the leaf's own typed unwind, not by a contained panic.
            assert!(unwound.is::<CleanupUnwind>(), "{label}");
            // Both receipts are closed, and nothing latched: not a fatal cause.
            assert!(
                matches!(running.wait(), Err(SqliteError::WriterClosed)),
                "{label}"
            );
            assert!(
                matches!(queued.wait(), Err(SqliteError::WriterClosed)),
                "{label}"
            );
            assert!(handle.failure_state().failure().is_none(), "{label}");
            // Nothing later is admitted.
            assert!(
                matches!(
                    handle.submit(Lane::Entities, None, |_| Ok(())),
                    Err(SqliteError::WriterClosed)
                ),
                "{label}"
            );
            // The outer rollback succeeded: nothing was committed, and a new writer works.
            assert_eq!(rows(&data, leaf.table), 0, "{label}");
            drop(handle);
            let (writer, handle) = open_writer(&data);
            let (state, blobs) = (handle.failure_state(), disk.clone());
            let next = handle
                .submit_contained(Lane::Entities, None, move |tx| {
                    write(tx, &blobs, state, "next")
                })
                .unwrap();
            handle.close();
            writer.run();
            next.wait().unwrap();
            assert_eq!(rows(&data, leaf.table), 1, "{label}");
        }
    }
}

/// The intent leaf with the root page of `intents` damaged: its INSERT, inside the
/// leaf's savepoint, answers `SQLITE_CORRUPT`, and the authorizer refuses the cleanup.
fn fatal_intent_scenario(
    data: &crate::data_dir::DataDir,
    submit: impl FnOnce(
        &WriterHandle,
        FailureState,
        Arc<DiskBlobStore>,
        fn(&Transaction<'_>, &DiskBlobStore, FailureState, &str) -> Result<()>,
    ) -> WriteReceipt<()>,
) -> (Writer<'_>, WriterHandle, WriteReceipt<()>, WriteReceipt<()>) {
    setup(data);
    corrupt_root_page(data, "intents");
    let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let (mut writer, handle) = open_writer(data);
    refuse_savepoint_rollbacks(&mut writer);
    let running = submit(&handle, handle.failure_state(), disk, LEAVES[2].write);
    let queued = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    (writer, handle, running, queued)
}

#[test]
fn a_fatal_leaf_cleanup_latches_before_the_receipts_close_though_the_outer_rollback_succeeds() {
    let (_temp, data) = crate::sqlite::test_data();
    let (writer, handle, running, queued) =
        fatal_intent_scenario(&data, |handle, state, blobs, write| {
            handle
                .submit_contained(Lane::Entities, None, move |tx| {
                    let handled = write(tx, &blobs, state, "ran");
                    assert!(handled.is_err());
                    Ok(())
                })
                .unwrap()
        });
    let unwound = run_bounded(writer, &handle).unwrap_err();
    assert!(unwound.is::<CleanupUnwind>());
    // The cause is the structured one the leaf carried, not a message.
    let latched = handle.failure_state().failure().unwrap();
    assert_eq!(
        latched.causes,
        [(Role::Operation, FatalCause::SqliteCorrupt)]
    );
    // So the answers are `storage_unavailable`, not a clean close.
    for (name, answer) in [("running", running.wait()), ("queued", queued.wait())] {
        assert!(
            matches!(&answer, Err(SqliteError::StorageUnavailable(f)) if *f == latched),
            "{name}: {answer:?}"
        );
    }
    assert!(matches!(
        handle.submit(Lane::Entities, None, |_| Ok(())),
        Err(SqliteError::StorageUnavailable(_))
    ));
}

#[test]
fn without_containment_the_fatal_cause_of_a_failed_leaf_cleanup_is_lost_with_the_writer() {
    // `submit` keeps the documented `Writer::run` behaviour for every panic: the
    // worker ends and the receipts close, and no cause is classified. This pins
    // the difference that `submit_contained` makes (the test above).
    let (_temp, data) = crate::sqlite::test_data();
    let (writer, handle, running, queued) =
        fatal_intent_scenario(&data, |handle, state, blobs, write| {
            handle
                .submit(Lane::Entities, None, move |tx| {
                    let _handled = write(tx, &blobs, state, "ran");
                    Ok(())
                })
                .unwrap()
        });
    assert!(run_bounded(writer, &handle).is_err());
    assert!(matches!(running.wait(), Err(SqliteError::WriterClosed)));
    assert!(matches!(queued.wait(), Err(SqliteError::WriterClosed)));
    assert!(handle.failure_state().failure().is_none());
}

#[test]
fn a_fatal_cause_on_the_cleanup_side_latches_as_the_rollbacks_own() {
    // SQLite refuses a `ROLLBACK TO` with a damaged-database code. No SQL reaches
    // that, so the leaf's own raise is called with the error that it would carry.
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let (writer, handle) = open_writer(&data);
    let running = handle
        .submit_contained::<(), _>(Lane::Entities, None, |_| {
            crate::sqlite::writer::contain::cleanup_failed(
                SqliteError::Migration("synthetic operation failure".into()),
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
                    None,
                ),
            )
        })
        .unwrap();
    let queued = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    assert!(run_bounded(writer, &handle).is_err());
    let latched = handle.failure_state().failure().unwrap();
    assert_eq!(
        latched.causes,
        [(Role::Rollback, FatalCause::SqliteNotADatabase)]
    );
    assert!(matches!(
        running.wait(),
        Err(SqliteError::StorageUnavailable(_))
    ));
    assert!(matches!(
        queued.wait(),
        Err(SqliteError::StorageUnavailable(_))
    ));
}

#[test]
fn a_callback_panic_with_a_failed_outer_rollback_latches_and_still_hands_the_caller_its_payload() {
    for leaf in &LEAVES {
        let label = leaf.name;
        let (_temp, data) = crate::sqlite::test_data();
        setup(&data);
        let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
        let (mut writer, handle) = open_writer(&data);
        // The writer's own rollback cannot complete: an outer failure.
        writer
            .connection_for_test()
            .authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Transaction {
                    operation: TransactionOperation::Rollback,
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))
            .unwrap();
        let write = leaf.write;
        let (state, blobs) = (handle.failure_state(), disk.clone());
        let running = handle
            .submit_contained::<(), _>(Lane::Entities, None, move |tx| {
                write(tx, &blobs, state, "ran")?;
                panic!("synthetic callback panic")
            })
            .unwrap();
        let queued = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();

        // The writer ends loudly, as for any rollback that leaves the transaction open.
        assert!(run_bounded(writer, &handle).is_err());
        let latched = handle.failure_state().failure().expect(label);
        assert_eq!(
            latched.causes,
            [(Role::Operation, FatalCause::RollbackLeftTransactionOpen)],
            "{label}"
        );
        // The running request keeps both failures, and the panic is still reachable.
        let answer = running.wait().unwrap_err();
        assert!(
            matches!(&answer, SqliteError::OperationRollback { .. }),
            "{label}: {answer:?}"
        );
        let payload = answer.callback_panic().expect(label).take().unwrap();
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"synthetic callback panic"),
            "{label}"
        );
        assert!(
            matches!(queued.wait(), Err(SqliteError::StorageUnavailable(_))),
            "{label}"
        );
        assert_eq!(rows(&data, leaf.table), 0, "{label}");
    }
}
