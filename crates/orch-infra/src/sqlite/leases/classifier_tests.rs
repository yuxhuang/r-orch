//! Standalone lease adapter classification; separate from caught-rollback F1 cases.
use super::{adapter::LeaseAdapter, tests::*, *};
use crate::sqlite::{
    ConnectionRole, ReadPool, Synchronous, Writer, WriterHandle, open_connection, test_data,
};
use std::time::{Duration, Instant};

struct ClockAt(i64);
impl orch_core::Clock for ClockAt {
    fn now_ms(&self) -> i64 {
        self.0
    }
    fn now_iso(&self) -> String {
        orch_core::clock::iso_from_epoch_ms(self.0)
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn sql_code<T: std::fmt::Debug>(result: crate::sqlite::Result<T>) -> rusqlite::ErrorCode {
    match result {
        Err(crate::sqlite::SqliteError::Sql(rusqlite::Error::SqliteFailure(failure, _))) => {
            failure.code
        }
        other => panic!("expected the driver error: {other:?}"),
    }
}

/// An executor over a database whose `leases` table SQLite finds damaged, with
/// the pool sharing the writer's failure state as the composed adapter does.
fn damaged_executor(
    test: impl FnOnce(&WriterHandle, &ReadPool<'_>, &std::sync::atomic::AtomicUsize),
) {
    let (_temp, data) = test_data();
    let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch(include_str!("../schema.sql")).unwrap();
    conn.execute_batch(
        "INSERT INTO leases (project_id, task_id, run_id, agent_id, worker_id, epoch, lineage,
                             state, granted_at, renewed_at, expires_at)
         VALUES ('p', 't', 'r', 'a', 'local_worker', 1, 'lineage', 'active', 0, 0, 1)",
    )
    .unwrap();
    drop(conn);
    crate::sqlite::corrupt_root_page(&data, "leases");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 16).unwrap();
    // Counts the COMMITs the writer attempts. A job that returns its error never
    // reaches one; a job that returns success on a damaged database does.
    let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = commits.clone();
    writer.set_commit_probe(Box::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    }));
    let pool =
        ReadPool::open_with_state(&data, Synchronous::Normal, 1, handle.failure_state()).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || writer.run());
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(&handle);
        test(&handle, &pool, &commits);
    });
}

/// Issue 127 (R140): a real `SQLITE_CORRUPT` in a claim reaches the receipt with
/// its code and latches the writer with that cause, not a backend string.
#[test]
fn a_real_corrupt_claim_keeps_its_code_and_latches_the_writer() {
    use crate::sqlite::{FailureSource, FatalCause, Role};
    damaged_executor(|writer, readers, commits| {
        let adapter = LeaseAdapter {
            writer,
            readers,
            caller: owner("p"),
            clock: &ClockAt(0),
        };
        let receipt = adapter.claim(None, new("t", "a", "r")).unwrap().wait();
        assert_eq!(sql_code(receipt), rusqlite::ffi::ErrorCode::DatabaseCorrupt);
        // The job itself returned the error: the executor never tried to commit.
        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 0);
        let failure = writer.failure_state().failure().expect("latched");
        assert_eq!(failure.source, FailureSource::Writer);
        assert_eq!(
            failure.causes,
            [(Role::Operation, FatalCause::SqliteCorrupt)]
        );
        // Refused at the door from now on.
        assert!(matches!(
            adapter.claim(None, new("t2", "a2", "r2")),
            Err(crate::sqlite::SqliteError::StorageUnavailable(_))
        ));
    });
}

/// The same damage met by a pooled lease read: the pool receives the driver's own
/// error, so the shared state is latched as the reader's.
#[test]
fn a_real_corrupt_snapshot_read_latches_the_state_as_the_readers() {
    use crate::sqlite::{FailureSource, FatalCause, Role};
    damaged_executor(|writer, readers, _| {
        let adapter = LeaseAdapter {
            writer,
            readers,
            caller: owner("p"),
            clock: &ClockAt(0),
        };
        let answer = adapter.snapshot(deadline(), list_leases);
        assert_eq!(sql_code(answer), rusqlite::ffi::ErrorCode::DatabaseCorrupt);
        let failure = writer.failure_state().failure().expect("latched");
        assert_eq!(failure.source, FailureSource::Reader);
        assert_eq!(
            failure.causes,
            [(Role::Operation, FatalCause::SqliteCorrupt)]
        );
        assert!(matches!(
            adapter.claim(None, new("t2", "a2", "r2")),
            Err(crate::sqlite::SqliteError::StorageUnavailable(_))
        ));
        // Signed issue 389 policy permits a fresh healthy read after the latch.
        assert_eq!(
            readers
                .read(deadline(), |tx| Ok(
                    tx.query_row("SELECT 7", [], |r| r.get::<_, i64>(0))?
                ))
                .unwrap(),
            7
        );
    });
}
