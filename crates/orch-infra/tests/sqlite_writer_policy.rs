//! The shared fatal-state policy of the SQLite backend (issue 127; R128, R140, R141),
//! proved with synthetic results: an error value stands for what a connection or a
//! Task read would report, so no database has to be damaged and no Task mapping
//! is needed. The structured `TaskCorrupt` carrier is not part of this slice: its
//! arm in `classify` and the proofs through real Task paths belong to the Task
//! integration, which this file's last section names.
use orch_core::store::StoreError;
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    Class, ConnectionRole, FailureSource, FailureState, FatalCause, Lane, ReadPool, Role,
    SqliteError, Synchronous, WriteReceipt, Writer, WriterHandle, classify, open_connection,
};
use rusqlite::ffi;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(20);

fn holder() -> Holder {
    Holder {
        kind: LockKind::Daemon,
        pid: std::process::id(),
        started_at: "synthetic".into(),
        binary_version: "test".into(),
        executable: "/synthetic/orch".into(),
    }
}

fn sql(code: i32) -> SqliteError {
    SqliteError::Sql(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
}
fn corrupt() -> SqliteError {
    sql(ffi::SQLITE_CORRUPT)
}
fn not_a_database() -> SqliteError {
    sql(ffi::SQLITE_NOTADB)
}
/// A failed connection that proves nothing.
fn io_error() -> SqliteError {
    sql(ffi::SQLITE_IOERR)
}
/// The request's own answer.
fn constraint() -> SqliteError {
    sql(ffi::SQLITE_CONSTRAINT)
}
fn ordinary() -> SqliteError {
    SqliteError::Store(StoreError::StaleEpoch)
}
fn rollback_failure(operation: SqliteError, rollback: SqliteError) -> SqliteError {
    let SqliteError::Sql(rollback) = rollback else {
        panic!("a rollback result is a SQLite error");
    };
    SqliteError::OperationRollback {
        operation: Box::new(operation),
        rollback,
    }
}

fn wait<T: Send + 'static>(receipt: WriteReceipt<T>) -> Result<T, SqliteError> {
    let (done, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(receipt.wait());
    });
    answer.recv_timeout(BOUND).expect("receipt not answered")
}

struct Env<'a> {
    data: &'a DataDir,
    handle: &'a WriterHandle,
    state: &'a FailureState,
}

impl Env<'_> {
    fn write<T: Send + 'static>(
        &self,
        work: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, SqliteError> + Send + 'static,
    ) -> Result<T, SqliteError> {
        wait(self.handle.submit(Lane::Entities, None, work)?)
    }

    fn rows(&self) -> Vec<i64> {
        let reader = open_connection(self.data, ConnectionRole::Reader, Synchronous::Normal)
            .expect("reader");
        let mut statement = reader.prepare("SELECT id FROM sample ORDER BY id").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn insert(&self, id: i64) -> Result<(), SqliteError> {
        self.write(move |tx| {
            tx.execute("INSERT INTO sample VALUES(?1)", [id])?;
            Ok(())
        })
    }
}

/// One writer on its own thread and the state it shares, for one scenario.
fn scenario(body: impl FnOnce(&Env<'_>)) {
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(&temp.path().join("data"), &holder()).unwrap();
    let state = FailureState::new();
    let (writer, handle) =
        Writer::open_with_state(&data, Synchronous::Normal, 8, state.clone()).expect("open writer");
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let env = Env {
            data: &data,
            handle: &handle,
            state: &state,
        };
        // Close on unwind too, or the scoped join would wait for ever.
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(&handle);
        env.write(|tx| {
            tx.execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY) STRICT")?;
            Ok(())
        })
        .expect("create the sample table");
        body(&env);
        handle.close();
        worker.join().unwrap();
    });
}

fn assert_unavailable<T>(outcome: &Result<T, SqliteError>) {
    assert!(
        matches!(outcome, Err(SqliteError::StorageUnavailable(_))),
        "expected storage_unavailable, got {:?}",
        outcome.as_ref().err()
    );
}

// ---- classification --------------------------------------------------------------

#[test]
fn both_causes_of_an_operation_rollback_are_classified_recursively_and_kept() {
    // The original operation nests another failed rollback.
    let nested = rollback_failure(rollback_failure(constraint(), io_error()), not_a_database());
    let seen = classify(&nested);
    assert_eq!(
        seen.leaves(),
        [
            (Role::Operation, Class::Ordinary),
            (Role::Rollback, Class::Transient),
            (Role::Rollback, Class::Fatal(FatalCause::SqliteNotADatabase)),
        ]
    );
    assert!(seen.is_fatal() && seen.savepoint_rollback_failed());

    // The operation side alone, then the rollback side alone, then both.
    let operation_fatal = classify(&rollback_failure(corrupt(), io_error()));
    assert_eq!(
        operation_fatal.fatal_causes(),
        [(Role::Operation, FatalCause::SqliteCorrupt)]
    );
    let rollback_fatal = classify(&rollback_failure(constraint(), corrupt()));
    assert_eq!(
        rollback_fatal.fatal_causes(),
        [(Role::Rollback, FatalCause::SqliteCorrupt)]
    );
    let both = classify(&rollback_failure(corrupt(), not_a_database()));
    assert_eq!(
        both.fatal_causes(),
        [
            (Role::Operation, FatalCause::SqliteCorrupt),
            (Role::Rollback, FatalCause::SqliteNotADatabase)
        ]
    );

    // Neither side fatal: still a failed rollback, never fatal.
    let neither = classify(&rollback_failure(ordinary(), io_error()));
    assert!(!neither.is_fatal() && neither.savepoint_rollback_failed());
    // A plain operation error has no rollback of its own.
    assert!(!classify(&constraint()).savepoint_rollback_failed());
}

#[test]
fn only_corrupt_and_notadb_are_fatal_and_a_constraint_or_a_busy_connection_is_not() {
    assert!(classify(&corrupt()).is_fatal());
    assert!(classify(&not_a_database()).is_fatal());
    for error in [
        constraint(),
        io_error(),
        sql(ffi::SQLITE_BUSY),
        sql(ffi::SQLITE_FULL),
        ordinary(),
        SqliteError::IntentCorrupt("intent payload"),
        SqliteError::Overloaded,
        SqliteError::TransactionAborted,
    ] {
        assert!(!classify(&error).is_fatal(), "{error:?}");
    }
    // The error that a latched state answers with does not latch again by itself.
    let state = FailureState::new();
    state.latch(FailureSource::Writer, FatalCause::LogicalCorruption);
    let refusal = state.ensure_available().unwrap_err();
    assert!(!classify(&refusal).is_fatal());
}

#[test]
fn every_live_connection_latches_the_state_and_an_external_artifact_does_not() {
    for source in [
        FailureSource::Writer,
        FailureSource::Reader,
        FailureSource::Checkpoint,
        FailureSource::Tool,
    ] {
        for error in [corrupt(), not_a_database()] {
            let state = FailureState::new();
            assert!(state.ensure_available().is_ok());
            state.observe(source, &error);
            assert_eq!(state.failure().expect("latched").source, source);
            assert_unavailable(&state.ensure_available());
        }
        // The latch is by name too, for a producer that has no error value.
        let state = FailureState::new();
        state.latch(source, FatalCause::LogicalCorruption);
        assert_unavailable(&state.ensure_available());
    }
    for error in [corrupt(), not_a_database()] {
        let state = FailureState::new();
        let seen = state.observe(FailureSource::Artifact, &error);
        assert!(seen.is_fatal(), "the artifact's damage is still classified");
        state.latch(FailureSource::Artifact, FatalCause::LogicalCorruption);
        assert!(state.failure().is_none());
        assert!(state.ensure_available().is_ok());
    }
    // Ordinary and transient results never latch, on any connection.
    let state = FailureState::new();
    for error in [constraint(), io_error(), ordinary()] {
        state.observe(FailureSource::Writer, &error);
        state.observe(FailureSource::Reader, &error);
    }
    assert!(state.failure().is_none());
}

#[test]
fn the_first_fatal_cause_is_kept_and_its_text_holds_no_value() {
    let state = FailureState::new();
    state.observe(
        FailureSource::Reader,
        &rollback_failure(corrupt(), not_a_database()),
    );
    state.observe(FailureSource::Writer, &not_a_database());
    let failure = state.failure().unwrap();
    assert_eq!(failure.source, FailureSource::Reader);
    assert_eq!(failure.causes.len(), 2);
    let text = state.ensure_available().unwrap_err().to_string();
    assert_eq!(
        text,
        "storage_unavailable: reader connection: operation SQLITE_CORRUPT; rollback SQLITE_NOTADB"
    );
}

// ---- the writer ------------------------------------------------------------------

#[test]
fn a_caught_fatal_cause_rolls_back_every_write_and_fails_the_writer() {
    scenario(|env| {
        let state = env.state.clone();
        let outcome = env.write(move |tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            tx.execute("INSERT INTO sample VALUES(2)", [])?;
            // The callback catches the fatal error and carries on to success.
            state.observe(FailureSource::Writer, &corrupt());
            tx.execute("INSERT INTO sample VALUES(3)", [])?;
            Ok(())
        });
        assert_unavailable(&outcome);
        assert!(
            env.rows().is_empty(),
            "nothing of the transaction committed"
        );
        assert_eq!(env.state.failure().unwrap().source, FailureSource::Writer);
    });
}

#[test]
fn an_uncaught_fatal_error_is_the_operations_own_answer_and_fails_the_writer() {
    scenario(|env| {
        let outcome: Result<(), _> = env.write(|tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            Err(corrupt())
        });
        // The operation sees its own error; the refusal is for what follows.
        assert!(matches!(outcome, Err(SqliteError::Sql(_))));
        assert!(env.rows().is_empty());
        assert_unavailable(&env.insert(2));
        assert!(env.rows().is_empty());
    });
}

#[test]
fn a_latched_writer_refuses_every_lane_at_the_door_and_what_was_queued_before() {
    scenario(|env| {
        let (started_tx, started) = mpsc::channel();
        let (go, proceed) = mpsc::channel::<()>();
        let state = env.state.clone();
        let first = env
            .handle
            .submit(Lane::Entities, None, move |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                started_tx.send(()).unwrap();
                proceed.recv_timeout(BOUND).unwrap();
                state.observe(FailureSource::Writer, &corrupt());
                Ok(())
            })
            .unwrap();
        started.recv_timeout(BOUND).unwrap();
        // Both are accepted while the writer is healthy, and queued behind the first.
        let ran = Arc::new(AtomicBool::new(false));
        let queued: Vec<_> = [Lane::Claims, Lane::Entities]
            .into_iter()
            .map(|lane| {
                let ran = Arc::clone(&ran);
                env.handle
                    .submit(lane, None, move |tx| {
                        ran.store(true, Ordering::SeqCst);
                        tx.execute("INSERT INTO sample VALUES(9)", [])?;
                        Ok(())
                    })
                    .unwrap()
            })
            .collect();
        go.send(()).unwrap();
        assert_unavailable(&wait(first));
        for receipt in queued {
            assert_unavailable(&wait(receipt));
        }
        assert!(!ran.load(Ordering::SeqCst), "no body ran after the latch");
        // And at the door, every lane.
        for lane in [
            Lane::Claims,
            Lane::Entities,
            Lane::Events,
            Lane::Maintenance,
        ] {
            assert!(
                matches!(
                    env.handle.submit(lane, None, |_| Ok(())),
                    Err(SqliteError::StorageUnavailable(_))
                ),
                "{lane:?}"
            );
        }
        assert!(env.rows().is_empty());
    });
}

#[test]
fn an_ordinary_refusal_rolled_back_to_its_savepoint_lets_unrelated_work_commit() {
    scenario(|env| {
        let state = env.state.clone();
        let outcome = env.write(move |tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            tx.execute_batch("SAVEPOINT operation")?;
            let refused = tx.execute("INSERT INTO sample VALUES(1)", []).unwrap_err();
            tx.execute_batch("ROLLBACK TO operation; RELEASE operation")?;
            // The caller catches the refusal, as the policy allows.
            let seen = state.observe(FailureSource::Writer, &SqliteError::Sql(refused));
            assert!(!seen.is_fatal() && !seen.savepoint_rollback_failed());
            let seen = state.observe(FailureSource::Writer, &ordinary());
            assert!(!seen.is_fatal());
            tx.execute("INSERT INTO sample VALUES(2)", [])?;
            Ok(())
        });
        outcome.expect("an ordinary caught refusal commits");
        assert_eq!(env.rows(), [1, 2]);
        assert!(env.state.failure().is_none());
        env.insert(3).expect("the writer goes on");
    });
}

#[test]
fn a_failed_savepoint_rollback_forbids_the_enclosing_commit_even_when_neither_cause_is_fatal() {
    scenario(|env| {
        let state = env.state.clone();
        let outcome = env.write(move |tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            let seen = state.observe(
                FailureSource::Writer,
                &rollback_failure(constraint(), io_error()),
            );
            assert!(!seen.is_fatal());
            tx.execute("INSERT INTO sample VALUES(2)", [])?;
            Ok(())
        });
        assert!(
            matches!(outcome, Err(SqliteError::TransactionAborted)),
            "{:?}",
            outcome.err()
        );
        assert!(
            env.rows().is_empty(),
            "the enclosing transaction rolled back"
        );
        assert!(env.state.failure().is_none(), "no cause was fatal");
        // The abort belongs to that one transaction.
        env.insert(5).expect("the next transaction commits");
        assert_eq!(env.rows(), [5]);
    });
}

#[test]
fn a_fatal_cause_on_either_side_of_a_caught_rollback_failure_fails_the_writer() {
    for (operation, rollback, expected) in [
        (
            corrupt(),
            io_error(),
            vec![(Role::Operation, FatalCause::SqliteCorrupt)],
        ),
        (
            constraint(),
            not_a_database(),
            vec![(Role::Rollback, FatalCause::SqliteNotADatabase)],
        ),
        (
            corrupt(),
            not_a_database(),
            vec![
                (Role::Operation, FatalCause::SqliteCorrupt),
                (Role::Rollback, FatalCause::SqliteNotADatabase),
            ],
        ),
    ] {
        scenario(move |env| {
            let state = env.state.clone();
            let error = rollback_failure(operation, rollback);
            let outcome = env.write(move |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                state.observe(FailureSource::Writer, &error);
                Ok(())
            });
            assert_unavailable(&outcome);
            assert!(env.rows().is_empty());
            assert_eq!(env.state.failure().unwrap().causes, expected);
            assert_unavailable(&env.insert(2));
        });
    }
}

#[test]
fn a_logical_corruption_latched_by_name_aborts_the_enclosing_transaction() {
    // What a Task read that proves its data corrupt will do through the typed hook
    // once the Task integration classifies `TaskCorrupt`: the cause has no SQLite code.
    scenario(|env| {
        let state = env.state.clone();
        let outcome = env.write(move |tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            state.latch(FailureSource::Writer, FatalCause::LogicalCorruption);
            Ok(())
        });
        assert_unavailable(&outcome);
        assert!(env.rows().is_empty());
        assert_eq!(
            env.state.failure().unwrap().causes,
            [(Role::Operation, FatalCause::LogicalCorruption)]
        );
    });
}

#[test]
fn an_external_artifact_failure_refuses_its_operation_and_leaves_the_writer_usable() {
    scenario(|env| {
        let state = env.state.clone();
        let outcome = env.write(move |tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            state.observe(FailureSource::Artifact, &corrupt());
            state.observe(FailureSource::Artifact, &not_a_database());
            Ok(())
        });
        outcome.expect("an artifact's damage does not abort a live transaction");
        assert_eq!(env.rows(), [1]);
        assert!(env.state.failure().is_none());
        env.insert(2).unwrap();
    });
}

/// Overwrite the main database file under an idle writer, after moving every page into it.
fn damage_the_database_file(env: &Env<'_>, from: usize) {
    open_connection(env.data, ConnectionRole::Checkpoint, Synchronous::Normal)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let path = env.data.database_path();
    let mut bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() > from, "the file holds the pages to damage");
    bytes[from..].fill(0xA5);
    std::fs::write(&path, bytes).unwrap();
}

#[test]
fn a_real_damaged_database_fails_the_writer_at_begin_and_nothing_runs_after() {
    // Garbage over the whole file reports NOTADB; over the pages after the first, CORRUPT.
    for (from, expected) in [
        (0, FatalCause::SqliteNotADatabase),
        (4096, FatalCause::SqliteCorrupt),
    ] {
        scenario(move |env| {
            env.insert(1).unwrap();
            damage_the_database_file(env, from);
            let ran = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&ran);
            let outcome = env.write(move |tx| {
                flag.store(true, Ordering::SeqCst);
                let _ = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0))?;
                Ok(())
            });
            // The failure of BEGIN or of the first statement is the request's answer, classified.
            assert!(
                matches!(outcome, Err(SqliteError::Sql(_))),
                "{:?}",
                outcome.err()
            );
            assert_eq!(
                env.state.failure().expect("latched").causes,
                [(Role::Operation, expected)]
            );
            assert_unavailable(&env.insert(2));
            assert!(
                matches!(
                    env.handle.submit(Lane::Claims, None, |_| Ok(())),
                    Err(SqliteError::StorageUnavailable(_))
                ),
                "no claim after the failure"
            );
        });
    }
}

#[test]
fn a_checkpoint_and_a_claim_stop_at_their_gate_once_any_connection_has_failed() {
    scenario(|env| {
        assert!(
            env.state.ensure_available().is_ok(),
            "healthy: the gate is open"
        );
        env.state.observe(FailureSource::Checkpoint, &corrupt());
        // The checkpoint job asks the shared state before each call.
        assert_unavailable(&env.state.ensure_available());
        assert!(matches!(
            env.handle.submit(Lane::Claims, None, |_| Ok(())),
            Err(SqliteError::StorageUnavailable(_))
        ));
    });
}

// ---- the read pool ---------------------------------------------------------------

fn with_pool(body: impl FnOnce(&Env<'_>, &ReadPool<'_>)) {
    scenario(|env| {
        let pool = ReadPool::open_with_state(env.data, Synchronous::Normal, 2, env.state.clone())
            .expect("open pool");
        body(env, &pool);
    });
}

fn deadline() -> std::time::Instant {
    std::time::Instant::now() + BOUND
}

#[test]
fn a_fatal_read_on_a_pooled_snapshot_fails_the_shared_writer_at_once() {
    with_pool(|env, pool| {
        let outcome: Result<(), _> = pool.read(deadline(), |_| Err(corrupt()));
        // The read's own answer is its error, before any conversion.
        assert!(matches!(outcome, Err(SqliteError::Sql(_))));
        assert_eq!(env.state.failure().unwrap().source, FailureSource::Reader);
        assert_unavailable(&env.insert(1));
        assert!(env.rows().is_empty());
        assert!(matches!(
            env.handle.submit(Lane::Claims, None, |_| Ok(())),
            Err(SqliteError::StorageUnavailable(_))
        ));
    });
}

#[test]
fn a_caught_fatal_read_discards_the_partial_result() {
    with_pool(|env, pool| {
        env.insert(1).unwrap();
        let state = pool.failure_state().clone();
        let outcome = pool.read(deadline(), |tx| {
            let partial: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
            // The caller catches a proven corruption and returns what it had.
            state.observe(FailureSource::Reader, &not_a_database());
            Ok(partial)
        });
        assert_unavailable(&outcome);
        assert!(env.state.failure().is_some());
    });
}

#[test]
fn one_failure_state_serves_a_pool_and_a_writer_only_when_it_is_shared() {
    scenario(|env| {
        // A pool opened on its own state cannot fail this writer: the sharing is explicit.
        let alone = ReadPool::open(env.data, Synchronous::Normal, 1).unwrap();
        let _ = alone.read(deadline(), |_| -> Result<(), _> { Err(corrupt()) });
        assert!(env.state.failure().is_none());
        assert!(alone.failure_state().failure().is_some());
        // The writer's own handle hands out the writer's state.
        let state = env.handle.failure_state();
        state.observe(FailureSource::Tool, &corrupt());
        assert!(env.state.failure().is_some());
    });
}

#[test]
fn an_ordinary_failed_pooled_read_is_counted_and_three_in_a_row_are_reported() {
    with_pool(|env, pool| {
        let state = env.state;
        let fail = |error: SqliteError| {
            let _ = pool.read(deadline(), move |_| -> Result<(), _> { Err(error) });
        };
        fail(io_error());
        fail(io_error());
        assert_eq!(state.pool_failure_streak(), 2);
        assert!(!state.pool_degraded());
        // The request's own answers (an absent row, a revision refusal) are not failures.
        fail(ordinary());
        assert_eq!(state.pool_failure_streak(), 2);
        fail(io_error());
        assert!(state.pool_degraded());
        assert!(
            state.failure().is_none(),
            "a failed read alone is not fatal"
        );
        // The connection is opened afresh and serves the next read; success resets the count.
        let count = pool
            .read(deadline(), |tx| {
                Ok(tx.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0))?)
            })
            .expect("a reopened connection reads");
        assert_eq!(count, 0);
        assert_eq!(state.pool_failure_streak(), 0);
        assert!(!state.pool_degraded());
    });
}

#[test]
fn a_pooled_connection_that_finds_the_file_damaged_when_it_reopens_fails_the_writer() {
    scenario(|env| {
        let pool = ReadPool::open_with_state(env.data, Synchronous::Normal, 1, env.state.clone())
            .expect("open pool");
        // A failed read drops its connection, so the next use opens one afresh.
        let _ = pool.read(deadline(), |_| -> Result<(), _> { Err(io_error()) });
        assert!(env.state.failure().is_none());
        // Move every page into the database file, then damage its header, as a stray write would.
        open_connection(env.data, ConnectionRole::Checkpoint, Synchronous::Normal)
            .unwrap()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let path = env.data.database_path();
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() >= 100, "the database file has its header");
        bytes[..100].fill(0xA5);
        std::fs::write(&path, bytes).unwrap();
        let outcome = pool.read(deadline(), |_| Ok(()));
        assert!(outcome.is_err(), "the damaged file is refused");
        assert!(
            env.state.failure().is_some(),
            "a reopen that reports CORRUPT or NOTADB is fatal: {:?}",
            outcome.err()
        );
    });
}

// ---- outstanding combined proofs ---------------------------------------------------

/// Not provable in this slice, and named so that no later one is claimed to have
/// proved them. `TaskCorrupt` is not a `SqliteError` variant on this base; the
/// Task integration (tsk_Sw1wdBo) adds it, adds its arm to `classify` (the match has
/// no wildcard, so it cannot compile without one) and proves, through real Task paths
/// on SQLite: `get`, page hydration, snapshot, unit-of-work and live-source
/// verification each fail the writer and return no partial page; an absent task, a
/// stale revision, an admission timeout and a transient object-access failure do
/// not; a caught `TaskCorrupt` in a unit of work never commits. The R128 retry
/// rules (the retryable class, a second `IOERR`, 60 s without a commit) and the
/// checkpoint job itself are other slices.
#[test]
#[ignore = "outstanding: combined proofs through real Task paths (Task integration)"]
fn outstanding_task_corrupt_through_real_task_paths() {}
