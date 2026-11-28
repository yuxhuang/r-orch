//! Issue 127 (R140; PR 202 review I2): a statement that really fails inside the
//! request-key, lease and event adapters reaches the shared failure state with
//! its SQLite code and class intact, before any callback can catch it or any port
//! converts it to text. Every operation below is driven through the router's
//! `observed` boundary on the real writer or a real pooled reader, against a
//! database that SQLite itself makes fail:
//!
//! * `SQLITE_CORRUPT`: the root page of the one table the operation reads or
//!   writes is damaged on disk, with no connection open.
//! * `SQLITE_FULL`: `max_page_count` is pinned at the current size, and the
//!   operation stores a payload that needs a new page.
//! * `SQLITE_CONSTRAINT`: a trigger of the database refuses the statement.
//! * a typed refusal of the adapter itself (`StaleEpoch`, `InvalidRequestKey`).
//!
//! `SQLITE_IOERR` and `SQLITE_NOMEM` are not provoked here: deterministic
//! statement failures would need a fault-injecting VFS or allocator.
//! These tests prove actual CORRUPT/FULL/CONSTRAINT propagation on the named
//! paths only; they do not establish every code on every adapter. The code-to-class
//! table, including both of
//! those codes, is `failure::tests::sqlite_result_codes_classify_by_primary_code_*`.
//! The old inactive adapters are in their own test modules.
use super::tests::owner;
use super::*;
use crate::sqlite::corrupt_root_page;
use crate::sqlite::writer::failure::{Class, FatalCause, Role, classify};
use crate::sqlite::writer::policy::RetryCode;
use crate::sqlite::{
    ConnectionRole, Lane, ReadPool, Synchronous, Writer, WriterHandle, migrations, open_connection,
};
use orch_core::store::events::SequencedEvent;
use orch_core::store::leases::{ClaimOutcome, NewLease};
use orch_core::store::request_keys::{KEY_TTL_MS, MAX_RESULT_BYTES, RequestScope};
use rusqlite::ErrorCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn event(seq: u64, text: &str) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: orch_domain::RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: orch_domain::RunEventType::AgentOutput,
            data: serde_json::json!({ "text": text }),
        },
    }
}

fn hash() -> String {
    "a".repeat(64)
}

/// What the committed database holds before any damage: one lease, two events,
/// one gap and one request key.
#[derive(Clone)]
struct Seed {
    lineage: String,
    epoch: u64,
}

fn seeded(data: &crate::data_dir::DataDir) -> Seed {
    let mut conn = open_connection(data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(data, &mut conn).unwrap();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner("p");
    let mut unit = EntityWrite::new(&tx, &caller, &disk, FailureState::new(), 0);
    let ClaimOutcome::Granted(lease) = unit
        .claim_lease(
            &NewLease {
                task: "t".into(),
                agent: "a".into(),
                run: "r".into(),
            },
            0,
        )
        .unwrap()
    else {
        panic!("the seed lease is granted")
    };
    unit.append_events(
        "r",
        &lease.lineage,
        lease.epoch,
        &[event(1, "one"), event(2, "two")],
    )
    .unwrap();
    unit.record_event_gap("r", 2, 1).unwrap();
    let scope = RequestScope::of(&caller, "0123456789abcdef").unwrap();
    unit.store_request_key(&scope, "task.update", &hash(), "result", 0)
        .unwrap();
    drop(unit);
    tx.commit().unwrap();
    Seed {
        lineage: lease.lineage,
        epoch: lease.epoch,
    }
}

type WriteFn = for<'a, 'c> fn(&mut EntityWrite<'a, 'c>, &Seed) -> Result<()>;
type ReadFn = for<'a, 'c> fn(&mut EntityRead<'a, 'c>, &Seed) -> Result<()>;

/// One adapter operation, run on a view, with the table it uses.
struct Op<F> {
    name: &'static str,
    table: &'static str,
    run: F,
}

fn scope() -> RequestScope {
    RequestScope::of(&owner("p"), "fedcba9876543210").unwrap()
}

/// Every operation of the three adapters a request can reach through a writer
/// transaction.
fn write_ops() -> Vec<Op<WriteFn>> {
    vec![
        Op {
            name: "request key check",
            table: "request_keys",
            run: |u, _| {
                u.check_request_key(&scope(), "task.update", &hash(), 0)
                    .map(drop)
            },
        },
        Op {
            name: "request key store",
            table: "request_keys",
            run: |u, _| u.store_request_key(&scope(), "task.update", &hash(), "x", 0),
        },
        Op {
            name: "request key expiry",
            table: "request_keys",
            run: |u, _| u.delete_expired_request_keys(2 * KEY_TTL_MS, 10).map(drop),
        },
        Op {
            name: "lease claim",
            table: "leases",
            run: |u, _| {
                u.claim_lease(
                    &NewLease {
                        task: "t2".into(),
                        agent: "a2".into(),
                        run: "r2".into(),
                    },
                    0,
                )
                .map(drop)
            },
        },
        Op {
            name: "lease renew",
            table: "leases",
            run: |u, s| u.renew_lease("t", &s.lineage, s.epoch, 1).map(drop),
        },
        Op {
            name: "lease release",
            table: "leases",
            run: |u, s| u.release_lease("t", &s.lineage, s.epoch).map(drop),
        },
        Op {
            name: "lease get",
            table: "leases",
            run: |u, _| u.get_lease("t").map(drop),
        },
        Op {
            name: "lease get by agent",
            table: "leases",
            run: |u, _| u.get_agent_lease("a").map(drop),
        },
        Op {
            name: "lease list",
            table: "leases",
            run: |u, _| u.list_leases().map(drop),
        },
        Op {
            name: "event append",
            table: "run_events",
            run: |u, s| {
                u.append_events("r", &s.lineage, s.epoch, &[event(3, "three")])
                    .map(drop)
            },
        },
        Op {
            name: "event gap",
            table: "run_event_gaps",
            run: |u, _| u.record_event_gap("r", 3, 1),
        },
        Op {
            name: "event prune",
            table: "run_events",
            run: |u, _| u.prune_events("r", 10).map(drop),
        },
        Op {
            name: "event read",
            table: "run_events",
            run: |u, _| u.read_events("r", 0, 10).map(drop),
        },
        Op {
            name: "event tail",
            table: "run_events",
            run: |u, _| u.read_events_tail("r", 10).map(drop),
        },
        Op {
            name: "event gaps read",
            table: "run_event_gaps",
            run: |u, _| u.read_event_gaps("r").map(drop),
        },
    ]
}

/// The operations a pooled snapshot offers.
fn read_ops() -> Vec<Op<ReadFn>> {
    vec![
        Op {
            name: "lease get",
            table: "leases",
            run: |r, _| r.get_lease("t").map(drop),
        },
        Op {
            name: "lease get by agent",
            table: "leases",
            run: |r, _| r.get_agent_lease("a").map(drop),
        },
        Op {
            name: "lease list",
            table: "leases",
            run: |r, _| r.list_leases().map(drop),
        },
        Op {
            name: "event read",
            table: "run_events",
            run: |r, _| r.read_events("r", 0, 10).map(drop),
        },
        Op {
            name: "event tail",
            table: "run_events",
            run: |r, _| r.read_events_tail("r", 10).map(drop),
        },
        Op {
            name: "event gaps read",
            table: "run_event_gaps",
            run: |r, _| r.read_event_gaps("r").map(drop),
        },
    ]
}

fn sqlite_code(error: &SqliteError) -> Option<ErrorCode> {
    match error {
        SqliteError::Sql(rusqlite::Error::SqliteFailure(failure, _)) => Some(failure.code),
        _ => None,
    }
}

fn is_unavailable<T>(result: &Result<T>) -> bool {
    matches!(result, Err(SqliteError::StorageUnavailable(_)))
}

/// A running writer over a database prepared by `prepare`, which may damage it
/// before the writer opens. The body gets the handle, a blob store and the seed.
fn with_writer(
    prepare: impl FnOnce(&crate::data_dir::DataDir, &Seed),
    body: impl FnOnce(&crate::data_dir::DataDir, &WriterHandle, &Arc<DiskBlobStore>, &Seed),
) {
    let (_temp, data) = crate::sqlite::test_data();
    let seed = seeded(&data);
    prepare(&data, &seed);
    let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let thread = scope.spawn(|| writer.run());
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let close = Close(&handle);
        body(&data, &handle, &disk, &seed);
        drop(close);
        thread.join().unwrap();
    });
}

fn submit_now<T: Send + 'static>(
    handle: &WriterHandle,
    work: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    handle
        .submit(
            Lane::Entities,
            Some(Instant::now() + Duration::from_secs(10)),
            work,
        )?
        .wait()
}

/// A real `SQLITE_CORRUPT` in the statement of any writer operation: the error
/// still carries its code when the operation returns, the failure state names
/// the writer and the cause, and the request is refused whether or not the
/// callback caught the error and returned success.
#[test]
fn a_real_corrupt_statement_of_each_write_operation_latches_the_writer_with_its_code() {
    for op in write_ops() {
        with_writer(
            |data, _| corrupt_root_page(data, op.table),
            |_, handle, disk, seed| {
                let state = handle.failure_state();
                let (blobs, seed_in, run) = (disk.clone(), seed.clone(), op.run);
                let seen = Arc::new(Mutex::new(None));
                let slot = seen.clone();
                let answer = submit_now(handle, move |tx| {
                    let caller = owner("p");
                    let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                    let error = run(&mut unit, &seed_in).expect_err("the statement fails");
                    let latched = state.failure();
                    let committable = unit.ensure_committable();
                    *slot.lock().unwrap() = Some((error, latched, committable));
                    // Caught, and the callback goes on to succeed.
                    Ok(())
                });
                let (error, latched, committable) = seen.lock().unwrap().take().unwrap();
                assert_eq!(
                    sqlite_code(&error),
                    Some(ErrorCode::DatabaseCorrupt),
                    "{}: {error:?}",
                    op.name
                );
                assert_eq!(
                    classify(&error).leaves(),
                    [(Role::Operation, Class::Fatal(FatalCause::SqliteCorrupt))],
                    "{}",
                    op.name
                );
                let failure = latched.unwrap_or_else(|| panic!("{}: not latched", op.name));
                assert_eq!(failure.source, FailureSource::Writer, "{}", op.name);
                assert_eq!(
                    failure.causes,
                    [(Role::Operation, FatalCause::SqliteCorrupt)],
                    "{}",
                    op.name
                );
                assert!(is_unavailable(&committable), "{}", op.name);
                assert!(is_unavailable(&answer), "{}: {answer:?}", op.name);
                assert!(
                    is_unavailable(&submit_now(handle, |_| Ok(()))),
                    "{}: the next request is refused",
                    op.name
                );
            },
        );
    }
}

/// The same damage met by a pooled read: attributed to the reader, and it fails
/// the writer that shares the state.
#[test]
fn a_real_corrupt_statement_of_each_pooled_read_latches_the_shared_state_as_the_readers() {
    for op in read_ops() {
        with_writer(
            |data, _| corrupt_root_page(data, op.table),
            |data, handle, disk, seed| {
                let pool =
                    ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
                        .unwrap();
                let mut seen = None;
                let answer = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
                    let caller = owner("p");
                    let mut read =
                        EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
                    seen = Some((op.run)(&mut read, seed).expect_err("the statement fails"));
                    // Observe before returning: pool cleanup must not supply the latch.
                    let failure = handle.failure_state().failure().expect(op.name);
                    assert_eq!(failure.source, FailureSource::Reader, "{}", op.name);
                    assert_eq!(
                        failure.causes,
                        [(Role::Operation, FatalCause::SqliteCorrupt)],
                        "{}",
                        op.name
                    );
                    // Caught, and the callback goes on to succeed.
                    Ok(())
                });
                let error = seen.unwrap();
                assert_eq!(
                    sqlite_code(&error),
                    Some(ErrorCode::DatabaseCorrupt),
                    "{}: {error:?}",
                    op.name
                );
                // The snapshot hands back no value. Its answer is the unavailable
                // refusal, or the damaged connection's own error when closing the
                // snapshot fails on it first: both are errors, and both carry the latch.
                assert!(
                    is_unavailable(&answer)
                        || answer.as_ref().err().and_then(sqlite_code)
                            == Some(ErrorCode::DatabaseCorrupt),
                    "{}: {answer:?}",
                    op.name
                );
                let failure = handle.failure_state().failure().expect(op.name);
                assert_eq!(failure.source, FailureSource::Reader, "{}", op.name);
                assert_eq!(
                    failure.causes,
                    [(Role::Operation, FatalCause::SqliteCorrupt)],
                    "{}",
                    op.name
                );
                assert!(
                    is_unavailable(&submit_now(handle, |_| Ok(()))),
                    "{}: the writer shares the latch",
                    op.name
                );
            },
        );
    }
}

/// `SQLITE_FULL` is not fatal: it keeps its code, retains the retry category
/// the writer needs, latches nothing, and the next request commits once the
/// space is back.
#[test]
fn a_real_full_database_in_a_write_operation_keeps_its_code_and_is_not_fatal() {
    type Fill = WriteFn;
    let fills: [(&str, Fill); 2] = [
        ("request key store", |u, _| {
            u.store_request_key(
                &scope(),
                "task.update",
                &hash(),
                &"x".repeat(MAX_RESULT_BYTES),
                0,
            )
        }),
        ("event append", |u, s| {
            u.append_events(
                "r",
                &s.lineage,
                s.epoch,
                &[event(3, &"y".repeat(48 * 1024))],
            )
            .map(drop)
        }),
    ];
    for (name, fill) in fills {
        with_writer(
            |_, _| {},
            |_, handle, disk, seed| {
                let state = handle.failure_state();
                let (blobs, seed_in) = (disk.clone(), seed.clone());
                let seen = Arc::new(Mutex::new(None));
                let slot = seen.clone();
                let answer = submit_now(handle, move |tx| {
                    let pages: i64 = tx.query_row("PRAGMA page_count", [], |r| r.get(0))?;
                    tx.execute_batch(&format!("PRAGMA max_page_count={pages}"))?;
                    let caller = owner("p");
                    let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                    let error = fill(&mut unit, &seed_in).expect_err("the database is full");
                    *slot.lock().unwrap() = Some((error, state.failure(), state.writer_retry()));
                    Ok(())
                });
                let (error, latched, retry) = seen.lock().unwrap().take().unwrap();
                assert_eq!(sqlite_code(&error), Some(ErrorCode::DiskFull), "{name}");
                assert_eq!(
                    classify(&error).leaves(),
                    [(Role::Operation, Class::Transient)],
                    "{name}"
                );
                assert_eq!(latched, None, "{name}");
                assert_eq!(retry, Some(RetryCode::Full), "{name}");
                // FULL ended the transaction; catching it cannot acknowledge a commit.
                assert!(
                    matches!(answer, Err(SqliteError::TransactionAborted)),
                    "{name}: {answer:?}"
                );
                assert_eq!(handle.failure_state().failure(), None, "{name}");
                let blobs = disk.clone();
                let state = handle.failure_state();
                submit_now(handle, move |tx| {
                    tx.execute_batch("PRAGMA max_page_count=1073741823")?;
                    let caller = owner("p");
                    let unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
                    unit.check_request_key(&scope(), "task.update", &hash(), 0)
                        .map(drop)
                })
                .unwrap_or_else(|error| panic!("{name}: the next request runs: {error:?}"));
            },
        );
    }
}

/// `SQLITE_CONSTRAINT` raised by the database in a statement of each adapter,
/// and the adapters' own typed refusals: neither latches, and the same request
/// (event writes excepted, which the router dooms by design) can still commit
/// other work.
#[test]
fn a_real_constraint_failure_or_a_typed_refusal_in_each_adapter_stays_recoverable() {
    type Refuse = WriteFn;
    let cases: [(&str, &str, bool, Refuse); 8] = [
        (
            "request key store",
            "CREATE TEMP TRIGGER refuse BEFORE INSERT ON main.request_keys \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            false,
            |u, _| u.store_request_key(&scope(), "task.update", &hash(), "x", 0),
        ),
        (
            "lease claim",
            "CREATE TEMP TRIGGER refuse BEFORE INSERT ON main.leases \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            false,
            |u, _| {
                u.claim_lease(
                    &NewLease {
                        task: "t2".into(),
                        agent: "a2".into(),
                        run: "r2".into(),
                    },
                    0,
                )
                .map(drop)
            },
        ),
        (
            "lease renew",
            "CREATE TEMP TRIGGER refuse BEFORE UPDATE ON main.leases \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            false,
            |u, s| u.renew_lease("t", &s.lineage, s.epoch, 1).map(drop),
        ),
        (
            "lease release",
            "CREATE TEMP TRIGGER refuse BEFORE DELETE ON main.leases \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            false,
            |u, s| u.release_lease("t", &s.lineage, s.epoch).map(drop),
        ),
        (
            "event append",
            "CREATE TEMP TRIGGER refuse BEFORE INSERT ON main.run_events \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            true,
            |u, s| {
                u.append_events("r", &s.lineage, s.epoch, &[event(3, "three")])
                    .map(drop)
            },
        ),
        (
            "event gap",
            "CREATE TEMP TRIGGER refuse BEFORE INSERT ON main.run_event_gaps \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            true,
            |u, _| u.record_event_gap("r", 3, 1),
        ),
        (
            "event prune",
            "CREATE TEMP TRIGGER refuse BEFORE DELETE ON main.run_events \
             BEGIN SELECT RAISE(ABORT,'synthetic'); END",
            true,
            |u, _| u.prune_events("r", 10).map(drop),
        ),
        ("request key duplicate", "SELECT 1", false, |u, _| {
            u.store_request_key(&scope(), "task.update", &hash(), "x", 0)
        }),
    ];
    for (name, trigger, dooms, refuse) in cases {
        with_writer(
            |_, _| {},
            |_, handle, disk, seed| {
                let state = handle.failure_state();
                let (blobs, seed_in) = (disk.clone(), seed.clone());
                let seen = Arc::new(Mutex::new(None));
                let slot = seen.clone();
                let answer = submit_now(handle, move |tx| {
                    let caller = owner("p");
                    let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                    // The duplicate case needs the key to exist first.
                    if name == "request key duplicate" {
                        unit.store_request_key(&scope(), "task.update", &hash(), "first", 0)?;
                    }
                    tx.execute_batch(trigger)?;
                    let error = refuse(&mut unit, &seed_in).expect_err(name);
                    *slot.lock().unwrap() = Some((
                        error,
                        state.failure(),
                        state.transaction_must_abort().is_some(),
                    ));
                    Ok(())
                });
                let (error, latched, aborts) = seen.lock().unwrap().take().unwrap();
                if name == "request key duplicate" {
                    assert!(
                        matches!(
                            error,
                            SqliteError::Store(orch_core::store::StoreError::InvalidRequestKey(_))
                        ),
                        "{name}: {error:?}"
                    );
                    assert_eq!(
                        classify(&error).leaves(),
                        [(Role::Operation, Class::Ordinary)]
                    );
                } else {
                    assert_eq!(
                        sqlite_code(&error),
                        Some(ErrorCode::ConstraintViolation),
                        "{name}: {error:?}"
                    );
                    assert_eq!(
                        classify(&error).leaves(),
                        [(Role::Operation, Class::Ordinary)],
                        "{name}"
                    );
                }
                assert_eq!(latched, None, "{name}: never latches");
                assert_eq!(
                    aborts, dooms,
                    "{name}: only an event statement dooms its transaction"
                );
                match (&answer, dooms) {
                    (Ok(()), false) => {}
                    (Err(SqliteError::TransactionAborted), true) => {}
                    other => panic!("{name}: unexpected answer {other:?}"),
                }
                // The writer serves the next request, and its work commits.
                let blobs = disk.clone();
                let state = handle.failure_state();
                submit_now(handle, move |tx| {
                    // TEMP triggers survive a successful commit on this connection.
                    // Remove the fixture before asking whether the writer recovered.
                    tx.execute_batch("DROP TRIGGER IF EXISTS temp.refuse")?;
                    let caller = owner("p");
                    let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
                    unit.read_events("r", 0, 10)?;
                    let next = RequestScope::of(&caller, "nextrequestkey01").unwrap();
                    unit.store_request_key(&next, "task.update", &hash(), "next", 0)?;
                    unit.ensure_committable()
                })
                .unwrap_or_else(|error| panic!("{name}: the next request: {error:?}"));
            },
        );
    }
}

/// The refusals the event adapter itself raises before any statement.
#[test]
fn the_event_adapters_typed_refusals_latch_nothing_and_doom_nothing() {
    with_writer(
        |_, _| {},
        |_, handle, disk, seed| {
            let state = handle.failure_state();
            let (blobs, seed_in) = (disk.clone(), seed.clone());
            submit_now(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                let stale = unit
                    .append_events("r", "other-lineage", seed_in.epoch, &[event(3, "x")])
                    .unwrap_err();
                assert!(
                    matches!(
                        stale,
                        SqliteError::Store(orch_core::store::StoreError::StaleEpoch)
                    ),
                    "{stale:?}"
                );
                assert_eq!(
                    classify(&stale).leaves(),
                    [(Role::Operation, Class::Ordinary)]
                );
                assert_eq!(state.failure(), None);
                assert!(state.transaction_must_abort().is_none());
                unit.ensure_committable()
            })
            .unwrap();
        },
    );
}
