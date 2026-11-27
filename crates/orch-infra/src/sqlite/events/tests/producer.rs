//! The first production event submission through the real composed
//! `SqliteStorage` and its retained writer (plan "Writer retry policy": WP1, WP5,
//! WP9, WP10; spec R26, R27, R128). Every fault is injected at the writer's own
//! seams (the commit probe, a real second job that holds the writer thread);
//! the producer under test is the production one. No outcome depends on a sleep:
//! the stalls are released by the test, the clock of the 20 ms cut is injected,
//! and the one pause (the write lock held for 300 ms) only widens the window in
//! which a wrongly early commit would show. The FULL exhaustion test also waits
//! through the real sixty-second production retry episode; its watchdog is only
//! a hang guard, not a latency target.

use super::super::producer::{
    EventBatch, EventProducer, EventProducerOptions, Expected, Rejected, Submission, SubmitFailure,
    TransactionReport, accounted_bytes, encoded_events, rows_are_durable,
};
use super::{event, owner};
use crate::sqlite::{SqliteStorage, StorageOptions};
use orch_core::store::events::{AppendResult, SequencedEvent};
use orch_core::store::leases::{ClaimOutcome, NewLease};
use orch_core::store::{Caller, Lane, Storage, StorageExt, StoreError};
use orch_domain::{RunEvent, RunEventType};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const SAFETY: Duration = Duration::from_secs(20);

/// A submission's answer. A batch that is never answered fails the test within
/// `SAFETY`; it does not hang it.
async fn answered(submission: Submission) -> Result<AppendResult, SubmitFailure> {
    tokio::time::timeout(SAFETY, submission)
        .await
        .unwrap_or_else(|_| panic!("the batch was not answered within {SAFETY:?}"))
}

struct Fixture {
    _root: tempfile::TempDir,
    database: std::path::PathBuf,
    storage: SqliteStorage,
    reports: Arc<Mutex<Vec<TransactionReport>>>,
    options: EventProducerOptions,
}

fn fixture_with(options: StorageOptions) -> Fixture {
    let (root, data) = crate::sqlite::test_data();
    let database = data.database_path();
    let storage = SqliteStorage::open(Arc::new(data), options).unwrap();
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    // The 20 ms cut reads a clock, and a loaded host moves it. A test that looks
    // at where a transaction ends therefore starts from a clock that stands
    // still; the stepped clock and the real one are chosen by the tests of the cut.
    Fixture {
        _root: root,
        database,
        storage,
        reports,
        options: EventProducerOptions {
            now: frozen_clock(),
            observer: Some(Arc::new(move |report| {
                sink.lock().unwrap().push(report.clone());
            })),
            ..EventProducerOptions::default()
        },
    }
}

fn fixture() -> Fixture {
    fixture_with(StorageOptions::default())
}

impl Fixture {
    fn producer(&self) -> EventProducer {
        EventProducer::new(self.storage.clone(), self.options.clone())
    }

    fn reports(&self) -> Vec<TransactionReport> {
        self.reports.lock().unwrap().clone()
    }

    /// A real active lease of `run`, as a claim grants it; the lineage and the
    /// epoch are the database's.
    async fn lease(&self, project: &str, run: &str) -> (String, u64) {
        let (task, agent, run) = (
            format!("task-{run}"),
            format!("agent-{run}"),
            run.to_owned(),
        );
        self.storage
            .transaction(owner(project), Lane::Claims, move |unit| {
                match unit.claim_lease(&NewLease { task, agent, run }, 0)? {
                    ClaimOutcome::Granted(lease) => Ok((lease.lineage, lease.epoch)),
                    ClaimOutcome::Held(lease) => panic!("fixture lease is held: {lease:?}"),
                }
            })
            .await
            .unwrap()
    }

    async fn batch(
        &self,
        project: &str,
        run: &str,
        seqs: std::ops::RangeInclusive<u64>,
    ) -> EventBatch {
        let (lineage, epoch) = self.lease(project, run).await;
        EventBatch {
            caller: owner(project),
            run: run.into(),
            lineage,
            epoch,
            events: seqs.map(event).collect(),
        }
    }

    /// A batch with no events: a lease check.
    async fn empty_batch(&self, project: &str, run: &str) -> EventBatch {
        let mut batch = self.batch(project, run, 1..=1).await;
        batch.events.clear();
        batch
    }

    async fn stored(&self, project: &str, run: &str) -> Vec<SequencedEvent> {
        let snapshot = self.storage.read(owner(project)).await.unwrap();
        snapshot.read_events(run, 0, 1000).await.unwrap()
    }

    /// Holds the one writer thread until `Stall::end`, so what is submitted
    /// meanwhile waits behind it.
    async fn stall(&self) -> Stall {
        let (entered, running) = tokio::sync::oneshot::channel();
        let (release, blocked) = mpsc::sync_channel::<()>(1);
        let storage = self.storage.clone();
        let job = tokio::spawn(async move {
            storage
                .write(
                    owner("stall"),
                    Lane::Maintenance,
                    Box::new(move |_| {
                        let _ = entered.send(());
                        blocked.recv_timeout(SAFETY).expect("released by the test");
                        Ok(())
                    }),
                )
                .await
        });
        running.await.unwrap();
        Stall { release, job }
    }
}

struct Stall {
    release: mpsc::SyncSender<()>,
    job: tokio::task::JoinHandle<orch_core::store::Result<()>>,
}

impl Stall {
    async fn end(self) {
        self.release.send(()).unwrap();
        self.job.await.unwrap().unwrap();
    }
}

fn counts(stored: usize, skipped: usize) -> AppendResult {
    AppendResult { stored, skipped }
}

fn big_event(seq: u64, bytes: usize) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!({ "pad": "x".repeat(bytes) }),
        },
    }
}

fn sql(code: i32) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

/// Waits until the producer has taken its first transaction, so that the next
/// batches wait in the producer and not in it. A yield count cannot bound how
/// long another runtime worker takes to run on a loaded host. Observe the state
/// instead, with half the stall's watchdog left for assertions and release.
/// This is only a deadlock watchdog; no production deadline or clock changes.
async fn in_flight(producer: &EventProducer) {
    tokio::time::timeout(SAFETY / 2, async {
        while !producer.waiting().1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the producer never took a transaction: waiting={:?}, retained={:?}",
            producer.waiting(),
            producer.retained()
        )
    });
}

#[tokio::test]
async fn waiting_event_batches_share_one_transaction() {
    let fx = fixture();
    let producer = fx.producer();
    let first = fx.batch("p", "run-1", 1..=3).await;
    let rest = [
        fx.batch("p", "run-2", 1..=4).await,
        fx.batch("p", "run-3", 1..=5).await,
    ];
    let later = EventBatch {
        events: (4..=6).map(event).collect(),
        ..first.clone()
    };
    let stall = fx.stall().await;
    let one = producer.submit(first).unwrap();
    in_flight(&producer).await;
    let waiting: Vec<_> = rest
        .into_iter()
        .chain([later])
        .map(|b| producer.submit(b).unwrap())
        .collect();
    // Only the one transaction is in the writer's queue; the rest wait here.
    assert_eq!(producer.waiting().0, 4);
    stall.end().await;
    assert_eq!(one.await.unwrap(), counts(3, 0));
    let mut answers = Vec::new();
    for submission in waiting {
        answers.push(submission.await.unwrap());
    }
    assert_eq!(answers, [counts(4, 0), counts(5, 0), counts(3, 0)]);

    let reports = fx.reports();
    assert_eq!(
        reports.len(),
        2,
        "one transaction for the first, one for all the rest"
    );
    let runs: Vec<_> = reports[1]
        .runs
        .iter()
        .map(|(_, run)| run.as_str())
        .collect();
    assert_eq!(
        runs,
        ["run-2", "run-3", "run-1"],
        "in the order they were handed over"
    );
    assert_eq!(reports[1].events, 12);
    assert_eq!(reports[1].accounting.rows, 12);
    assert_eq!(
        reports[1].accounting.over_bound,
        reports[1].accounting.work > Duration::from_millis(20),
        "the signed accounting includes actual COMMIT time, even for a small planned transaction"
    );
    assert_eq!(
        fx.stored("p", "run-1").await,
        (1..=6).map(event).collect::<Vec<_>>()
    );
    assert_eq!(fx.stored("p", "run-2").await.len(), 4);
    assert_eq!(fx.stored("p", "run-3").await.len(), 5);
    producer.close().await;
}

#[tokio::test]
async fn bulk_work_is_cut_at_1000_rows() {
    let fx = fixture();
    let producer = fx.producer();
    let batch = fx.batch("p", "run", 1..=1001).await;
    let before = fx.storage.failure_state().writer_txn_over_bound_total();
    let answer = producer.submit(batch).unwrap().await.unwrap();
    assert_eq!(answer, counts(1001, 0));
    let reports = fx.reports();
    let shape: Vec<_> = reports
        .iter()
        .map(|r| (r.events, r.accounting.rows))
        .collect();
    assert_eq!(
        shape,
        [(1000, 1000), (1, 1)],
        "the 1,001st row is the tail of the next transaction"
    );
    assert!(reports.iter().all(|r| !r.cut_by_time));
    assert_counted_by_time_alone(&fx, before, &reports);
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=1000).map(event).collect::<Vec<_>>()
    );
    let tail = fx
        .storage
        .read(owner("p"))
        .await
        .unwrap()
        .read_events("run", 1000, 10)
        .await
        .unwrap();
    assert_eq!(tail, vec![event(1001)]);
    producer.close().await;
}

#[tokio::test]
async fn exactly_1000_rows_is_one_transaction() {
    let fx = fixture();
    let producer = fx.producer();
    let batch = fx.batch("p", "run", 1..=1000).await;
    let before = fx.storage.failure_state().writer_txn_over_bound_total();
    let answer = producer.submit(batch).unwrap().await.unwrap();
    assert_eq!(answer, counts(1000, 0));
    let reports = fx.reports();
    let shape: Vec<_> = reports
        .iter()
        .map(|r| (r.events, r.accounting.rows))
        .collect();
    assert_eq!(shape, [(1000, 1000)]);
    assert_counted_by_time_alone(&fx, before, &reports);
    producer.close().await;
}

#[tokio::test]
async fn runs_are_cut_together_in_order_and_each_keeps_its_own_tail() {
    let fx = fixture();
    let producer = fx.producer();
    let lead = fx.batch("p", "z", 1..=1).await;
    let batches = [
        fx.batch("p", "a", 1..=400).await,
        fx.batch("p", "b", 1..=400).await,
        fx.batch("p", "c", 1..=400).await,
    ];
    let stall = fx.stall().await;
    let first = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let submissions: Vec<_> = batches
        .into_iter()
        .map(|b| producer.submit(b).unwrap())
        .collect();
    stall.end().await;
    first.await.unwrap();
    for submission in submissions {
        assert_eq!(submission.await.unwrap(), counts(400, 0));
    }
    let shape: Vec<_> = fx
        .reports()
        .iter()
        .skip(1)
        .map(|r| (r.events, r.accounting.rows))
        .collect();
    assert_eq!(shape, [(1000, 1000), (200, 200)]);
    for run in ["a", "b", "c"] {
        assert_eq!(fx.stored("p", run).await.len(), 400);
    }
    producer.close().await;
}

#[tokio::test]
async fn events_with_a_blob_are_planned_by_the_rows_they_change() {
    let fx = fixture();
    let producer = fx.producer();
    let (lineage, epoch) = fx.lease("p", "run").await;
    let big = 64 * 1024 + 1;
    let batch = EventBatch {
        caller: owner("p"),
        run: "run".into(),
        lineage,
        epoch,
        events: vec![event(1), big_event(2, big), event(3), big_event(4, big)],
    };
    let answer = producer.submit(batch).unwrap().await.unwrap();
    assert_eq!(answer, counts(4, 0));
    let reports = fx.reports();
    assert_eq!(reports.len(), 1);
    // Two events, and a blob row for each of the other two: the actual rows
    // of the writer are the rows the plan counted.
    assert_eq!(reports[0].accounting.rows, 6);
    assert_eq!(fx.stored("p", "run").await.len(), 4);
    producer.close().await;
}

/// The writer counts a transaction over the bound once, from its own measure of
/// the rows and the time (it does not know the producer's plan). A transaction of
/// 1,000 rows is inside the row bound; on a slow build its real time can still
/// pass 20 ms, and then the writer counts it, once, and the producer does not.
fn assert_counted_by_time_alone(fx: &Fixture, before: u64, reports: &[TransactionReport]) {
    for report in reports {
        assert!(report.accounting.rows <= 1000, "{report:?}");
        assert_eq!(
            report.accounting.over_bound,
            report.accounting.work > Duration::from_millis(20),
            "{report:?}"
        );
    }
    let counted = fx.storage.failure_state().writer_txn_over_bound_total() - before;
    let slow = reports.iter().filter(|r| r.accounting.over_bound).count() as u64;
    assert_eq!(counted, slow, "once for each transaction, by the writer");
}

/// A clock that never moves, so that only the 1,000 rows cut a transaction. The
/// real 20 ms cut is exercised by the stepped clock below, and by the real
/// clock in the tests that do not look at where a transaction ends.
fn frozen_clock() -> Arc<dyn Fn() -> Instant + Send + Sync> {
    let now = Instant::now();
    Arc::new(move || now)
}

/// A clock that moves 7 ms at every reading: the first statement is free, the
/// next two start at 7 and 14 ms, and the fourth would start at 21.
fn stepped_clock(step: Duration) -> Arc<dyn Fn() -> Instant + Send + Sync> {
    let base = Instant::now();
    let reads = Arc::new(AtomicU64::new(0));
    Arc::new(move || base + step * u32::try_from(reads.fetch_add(1, Ordering::SeqCst)).unwrap())
}

#[tokio::test]
async fn the_twenty_millisecond_cut_falls_between_statements_and_keeps_the_tail() {
    let mut fx = fixture();
    let reads = stepped_clock(Duration::from_millis(7));
    fx.options.now = reads;
    let producer = fx.producer();
    let answer = producer
        .submit(fx.batch("p", "run", 1..=200).await)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(answer, counts(200, 0));
    let reports = fx.reports();
    // 32 events a statement; three statements fit before the clock reads 20 ms.
    let shape: Vec<_> = reports.iter().map(|r| (r.events, r.cut_by_time)).collect();
    assert_eq!(shape, [(96, true), (96, true), (8, false)]);
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=200).map(event).collect::<Vec<_>>()
    );
    producer.close().await;
}

#[tokio::test]
async fn a_cut_always_lets_one_statement_run_so_the_tail_keeps_moving() {
    let mut fx = fixture();
    fx.options.now = stepped_clock(Duration::from_secs(3600));
    let producer = fx.producer();
    let answer = producer
        .submit(fx.batch("p", "run", 1..=70).await)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(answer, counts(70, 0));
    let events: Vec<_> = fx.reports().iter().map(|r| r.events).collect();
    assert_eq!(events, [32, 32, 6]);
    producer.close().await;
}

#[tokio::test]
async fn an_atomic_operation_over_the_bound_runs_whole_and_is_counted() {
    let mut busy = 0;
    let fx = fixture_with(StorageOptions {
        // The event COMMIT is busy twice and then succeeds on the same transaction.
        writer_commit_probe: Some(Box::new(move |tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events == 0 || busy == 2 {
                return None;
            }
            busy += 1;
            Some(Err(sql(5)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let (lineage, epoch) = fx.lease("p", "run").await;
    let before = fx.storage.failure_state().writer_txn_over_bound_total();
    // One event whose payload cannot be cut: a statement that stores 48 MiB.
    let batch = EventBatch {
        caller: owner("p"),
        run: "run".into(),
        lineage,
        epoch,
        events: vec![big_event(1, 48 * 1024 * 1024)],
    };
    let answer = producer.submit(batch).unwrap().await.unwrap();
    assert_eq!(answer, counts(1, 0));
    let reports = fx.reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].attempts, 1,
        "the commit retry is the writer's, not a rebuild"
    );
    assert_eq!(
        reports[0].accounting.rows, 2,
        "the event and its blob row, once"
    );
    assert!(reports[0].accounting.over_bound);
    assert!(reports[0].accounting.work > Duration::from_millis(20));
    assert_eq!(
        fx.storage.failure_state().writer_txn_over_bound_total() - before,
        1,
        "once for the transaction, not for each commit attempt"
    );
    producer.close().await;
}

#[tokio::test]
async fn a_known_rollback_is_rebuilt_from_the_retained_input_and_commits_once() {
    let mut first = true;
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            // Skip the claim's commit: it has no event table rows.
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events > 0 && first {
                first = false;
                return Some(Err(sql(13)));
            }
            None
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let answer = producer
        .submit(fx.batch("p", "run", 1..=10).await)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(answer, counts(10, 0), "exactly one receipt for the batch");
    let reports = fx.reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].attempts, 2,
        "the work was rebuilt once, as a new request"
    );
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=10).map(event).collect::<Vec<_>>()
    );
    assert!(!fx.storage.failure_state().claims_suppressed());
    producer.close().await;
}

/// The probe commits for real and then reports that the commit failed, so the
/// writer knows nothing: the producer must read the database.
fn commit_then_error() -> StorageOptions {
    let mut once = true;
    StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events == 0 || !once {
                return None;
            }
            once = false;
            tx.execute_batch("COMMIT").unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    }
}

#[tokio::test]
async fn an_unknown_commit_that_was_durable_is_reconciled_and_never_replayed() {
    let fx = fixture_with(commit_then_error());
    let producer = fx.producer();
    let answer = producer
        .submit(fx.batch("p", "run", 1..=10).await)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        answer,
        counts(10, 0),
        "stored once, not stored and then skipped"
    );
    let reports = fx.reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].attempts, 1, "nothing was resubmitted");
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=10).map(event).collect::<Vec<_>>()
    );
    assert!(!fx.storage.failure_state().claims_suppressed());
    producer.close().await;
}

#[tokio::test]
async fn an_unknown_commit_that_was_lost_is_rebuilt_only_after_the_database_says_so() {
    let mut once = true;
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events == 0 || !once {
                return None;
            }
            once = false;
            // The commit that ends the transaction loses what it carried, and
            // the writer is told only that it failed. The transaction is over
            // without a rollback, so the writer knows nothing: the producer must
            // read the database, find nothing, and only then build it again.
            tx.execute_batch("DELETE FROM run_events; COMMIT").unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let answer = producer
        .submit(fx.batch("p", "run", 1..=10).await)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(answer, counts(10, 0));
    assert_eq!(
        fx.reports()[0].attempts,
        2,
        "reconciled absent, then rebuilt once"
    );
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=10).map(event).collect::<Vec<_>>()
    );
    producer.close().await;
}

/// The expected rows of a hand-made transaction: `events` of `run`, from `from`.
fn expected(run: &str, events: Vec<SequencedEvent>) -> Expected {
    let to = events.len();
    Expected {
        run: run.into(),
        events: Arc::from(events),
        from: 0,
        to,
    }
}

#[tokio::test]
async fn reconciliation_answers_for_all_none_or_neither_of_the_expected_rows() {
    let fx = fixture();
    let batch = fx.batch("p", "run", 1..=3).await;
    let (lineage, epoch) = (batch.lineage.clone(), batch.epoch);
    fx.storage
        .transaction(owner("p"), Lane::Events, move |unit| {
            unit.append_events("run", &lineage, epoch, &[event(1), event(2)])
        })
        .await
        .unwrap();
    let check = |rows: Vec<SequencedEvent>| {
        let storage = fx.storage.clone();
        async move {
            let snapshot = storage.read(owner("p")).await.unwrap();
            rows_are_durable(snapshot, &[expected("run", rows)]).await
        }
    };
    assert!(
        check(vec![event(1), event(2)]).await.unwrap(),
        "all of them"
    );
    assert!(!check(vec![event(3)]).await.unwrap(), "none of them");
    assert!(
        check(vec![event(1), event(2), event(3)]).await.is_err(),
        "some of them"
    );
    let mut changed = event(2);
    changed.event.data = serde_json::json!({ "other": true });
    assert!(
        check(vec![event(1), changed]).await.is_err(),
        "the same number with other bytes"
    );
    let storage = fx.storage.clone();
    let snapshot = storage.read(owner("p")).await.unwrap();
    assert!(
        rows_are_durable(snapshot, &[]).await.unwrap(),
        "a transaction that stored no row has nothing a lost commit could lose"
    );
}

#[tokio::test]
async fn a_second_consecutive_io_error_fails_the_writer_and_answers_every_waiting_batch() {
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(|tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            (events > 0).then(|| Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let (a, b) = (
        fx.batch("p", "a", 1..=3).await,
        fx.batch("p", "b", 1..=2).await,
    );
    let stall = fx.stall().await;
    let first = producer.submit(a).unwrap();
    in_flight(&producer).await;
    let second = producer.submit(b).unwrap();
    stall.end().await;
    let failure: SubmitFailure = first.await.unwrap_err();
    assert!(
        failure.error.to_string().contains("storage_unavailable"),
        "{failure:?}"
    );
    assert_eq!(failure.committed, counts(0, 0));
    assert_eq!(failure.not_stored, 3);
    assert!(!failure.outcome_unknown, "a known rollback is not in doubt");
    let failure = second.await.unwrap_err();
    assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 2));
    assert!(fx.storage.failure_state().failure().is_some());
    // A failed writer is final: the batch goes back to its collector.
    let batch = fx.batch_after_failure();
    match producer.submit(batch) {
        Err(Rejected::Closed(batch, cause)) => {
            assert_eq!(batch.events.len(), 1);
            assert!(cause.contains("storage_unavailable"), "{cause}");
        }
        other => panic!("expected the batch back, got {other:?}"),
    }
    for run in ["a", "b"] {
        assert!(
            fx.stored("p", run).await.is_empty(),
            "nothing is stored after a failed commit"
        );
    }
    producer.close().await;
}

impl Fixture {
    fn batch_after_failure(&self) -> EventBatch {
        EventBatch {
            caller: owner("p"),
            run: "a".into(),
            lineage: "any".into(),
            epoch: 1,
            events: vec![event(9)],
        }
    }
}

#[tokio::test]
async fn a_full_disk_is_retried_and_then_fails_the_writer() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = attempts.clone();
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events == 0 {
                return None;
            }
            // Fail every reconstructed transaction, not just one callback or
            // the policy object in isolation. The rows must roll back each time.
            assert_eq!(events, 3);
            seen.lock().unwrap().push(Instant::now());
            Some(Err(sql(13)))
        })),
        ..StorageOptions::default()
    });
    let idle = fixture();
    let producer = fx.producer();
    let first = fx.batch("p", "a", 1..=3).await;
    let second = fx.batch("p", "b", 1..=2).await;
    let stall = fx.stall().await;
    let first = producer.submit(first).unwrap();
    in_flight(&producer).await;
    let second = producer.submit(second).unwrap();
    stall.end().await;

    // Exercise the unmodified production sixty-second episode. This timeout is
    // only a hang bound; there is no upper latency expectation or filled disk.
    let failure = tokio::time::timeout(Duration::from_secs(120), first)
        .await
        .expect("FULL episode did not produce a terminal receipt")
        .unwrap_err();
    assert!(failure.error.to_string().contains("storage_unavailable"));
    assert_eq!(failure.committed, counts(0, 0));
    assert_eq!(failure.not_stored, 3);
    assert!(!failure.outcome_unknown);
    let queued = answered(second).await.unwrap_err();
    assert!(queued.error.to_string().contains("storage_unavailable"));
    assert_eq!((queued.committed, queued.not_stored), (counts(0, 0), 2));
    assert!(!queued.outcome_unknown);
    assert_eq!(
        fx.storage.failure_state().failure().unwrap().causes,
        vec![(
            crate::sqlite::Role::Operation,
            crate::sqlite::FatalCause::RetryWithoutCommit
        )]
    );
    {
        let attempts = attempts.lock().unwrap();
        assert!(
            attempts.len() >= 8,
            "FULL must survive the exponential backoff"
        );
        assert!(attempts[0].elapsed() >= Duration::from_secs(60));
    }
    assert!(fx.reports().is_empty(), "no transaction committed");
    for run in ["a", "b"] {
        assert!(fx.stored("p", run).await.is_empty());
    }
    let later = fx
        .storage
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(|_| panic!("failed writer invoked a later mutation")),
        )
        .await;
    assert!(
        later
            .unwrap_err()
            .to_string()
            .contains("storage_unavailable")
    );

    // This independent real writer stayed idle across the same full episode.
    // Idle time alone must neither latch failure nor prevent a later commit.
    assert!(idle.storage.failure_state().failure().is_none());
    let control = idle.producer();
    assert_eq!(
        answered(
            control
                .submit(idle.batch("p", "idle", 1..=1).await)
                .unwrap()
        )
        .await
        .unwrap(),
        counts(1, 0)
    );
    assert_eq!(idle.stored("p", "idle").await, vec![event(1)]);
    assert!(idle.storage.failure_state().failure().is_none());
    control.close().await;
    producer.close().await;
}

#[tokio::test]
async fn a_closed_producer_returns_an_oversized_batch_as_closed() {
    let mut fx = fixture();
    fx.options.queue_limit = 1;
    fx.options.observer = Some(Arc::new(|_| panic!("close intake after the known commit")));
    let lead = fx.batch("p", "lead", 1..=1).await;
    let batch = fx.batch("p", "run", 1..=2).await;
    let expected = batch.clone();
    let producer = fx.producer();
    assert_eq!(
        answered(producer.submit(lead).unwrap()).await.unwrap(),
        counts(1, 0)
    );
    // On this current-thread runtime the observer unwinds and closes intake
    // before the submitting task resumes from its delivered receipt.
    match producer.submit(batch) {
        Err(Rejected::Closed(batch, _)) => same_batch(&batch, &expected),
        other => panic!("Closed must precede TooLarge: {other:?}"),
    }
    producer.close().await;
}

#[tokio::test]
async fn a_stale_run_is_refused_alone_and_the_others_in_its_transaction_commit() {
    let fx = fixture();
    let producer = fx.producer();
    let good = fx.batch("p", "good", 1..=3).await;
    let mut stale = fx.batch("p", "stale", 1..=4).await;
    stale.epoch += 1;
    let also_good = fx.batch("p", "also-good", 1..=2).await;
    let lead = fx.batch("p", "lead", 1..=1).await;
    let stall = fx.stall().await;
    let first = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let submissions = [good, stale, also_good].map(|b| producer.submit(b).unwrap());
    stall.end().await;
    first.await.unwrap();
    let [good, stale, also_good] = submissions;
    assert_eq!(good.await.unwrap(), counts(3, 0));
    let failure = stale.await.unwrap_err();
    assert!(matches!(failure.error, StoreError::StaleEpoch));
    assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 4));
    assert_eq!(also_good.await.unwrap(), counts(2, 0));
    let reports = fx.reports();
    assert_eq!(
        reports.len(),
        2,
        "the refusal did not split the transaction"
    );
    assert_eq!(reports[1].events, 5, "the refused batch stored nothing");
    assert!(fx.stored("p", "stale").await.is_empty());
    assert_eq!(fx.stored("p", "good").await.len(), 3);
    assert_eq!(fx.stored("p", "also-good").await.len(), 2);
    producer.close().await;
}

#[tokio::test]
async fn an_empty_batch_still_checks_its_lease() {
    let fx = fixture();
    let producer = fx.producer();
    let mut batch = fx.batch("p", "run", 1..=1).await;
    batch.events.clear();
    assert_eq!(
        answered(producer.submit(batch.clone()).unwrap())
            .await
            .unwrap(),
        counts(0, 0)
    );
    batch.epoch += 1;
    let failure = answered(producer.submit(batch).unwrap()).await.unwrap_err();
    assert!(matches!(failure.error, StoreError::StaleEpoch));
    producer.close().await;
}

#[tokio::test]
async fn batches_of_another_caller_never_share_a_transaction_and_runs_are_not_overtaken() {
    let fx = fixture();
    let producer = fx.producer();
    let lead = fx.batch("p", "lead", 1..=1).await;
    let first = fx.batch("p", "run", 1..=2).await;
    let other_project = fx.batch("q", "run", 1..=2).await;
    let other_principal = EventBatch {
        caller: Caller {
            principal: orch_core::store::Principal::Owner {
                identity: "second".into(),
            },
            ..first.caller.clone()
        },
        events: (3..=4).map(event).collect(),
        ..first.clone()
    };
    let third = EventBatch {
        events: (5..=6).map(event).collect(),
        ..first.clone()
    };
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let submissions: Vec<_> = [first, other_project, other_principal, third]
        .into_iter()
        .map(|b| producer.submit(b).unwrap())
        .collect();
    stall.end().await;
    lead.await.unwrap();
    for submission in submissions {
        assert_eq!(submission.await.unwrap(), counts(2, 0));
    }
    let reports = fx.reports();
    let groups: Vec<Vec<(&str, &str)>> = reports
        .iter()
        .skip(1)
        .map(|r| {
            r.runs
                .iter()
                .map(|(p, run)| (p.as_str(), run.as_str()))
                .collect()
        })
        .collect();
    // Batch 1 (project p), then q's, then the second principal's; the third
    // batch of "run" waited behind the second principal's, whose run it shares.
    assert_eq!(
        groups,
        [
            vec![("p", "run")],
            vec![("q", "run")],
            vec![("p", "run")],
            vec![("p", "run")]
        ]
    );
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=6).map(event).collect::<Vec<_>>()
    );
    assert_eq!(fx.stored("q", "run").await.len(), 2);
    producer.close().await;
}

#[tokio::test]
async fn an_admission_the_writer_refuses_is_kept_and_committed_later() {
    let fx = fixture_with(StorageOptions {
        writer_capacity: 1,
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let batch = fx.batch("p", "run", 1..=5).await;
    let stall = fx.stall().await;
    // One request waits behind the stall: the writer's queue is full.
    let storage = fx.storage.clone();
    let mut filler = Box::pin(async move {
        storage
            .transaction(owner("filler"), Lane::Maintenance, |_| Ok(()))
            .await
    });
    assert!(futures::poll!(&mut filler).is_pending());
    let submission = producer.submit(batch).unwrap();
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(futures::poll!(&mut filler).is_pending());
    stall.end().await;
    filler.await.unwrap();
    assert_eq!(submission.await.unwrap(), counts(5, 0));
    assert_eq!(fx.stored("p", "run").await.len(), 5);
    assert!(fx.storage.failure_state().failure().is_none());
    producer.close().await;
}

#[tokio::test]
async fn too_many_waiting_events_return_the_batch_to_its_collector() {
    let mut fx = fixture();
    fx.options.queue_limit = 10;
    let producer = fx.producer();
    let lead = fx.batch("p", "lead", 1..=1).await;
    let (a, b) = (
        fx.batch("p", "a", 1..=8).await,
        fx.batch("p", "b", 1..=8).await,
    );
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let a = producer.submit(a).unwrap();
    let encoded = encoded_events();
    let Err(Rejected::Full(returned)) = producer.submit(b) else {
        panic!("the second batch does not fit");
    };
    assert_eq!(
        encoded_events(),
        encoded,
        "a batch that cannot fit is returned before it is encoded"
    );
    assert_eq!(
        returned.events.len(),
        8,
        "the caller still owns every event"
    );
    stall.end().await;
    lead.await.unwrap();
    a.await.unwrap();
    // Once it has drained, the same batch is accepted.
    assert_eq!(
        producer.submit(*returned).unwrap().await.unwrap(),
        counts(8, 0)
    );
    producer.close().await;
}

#[tokio::test]
async fn closing_answers_what_waits_and_refuses_more() {
    let fx = fixture();
    let producer = fx.producer();
    let waiting = producer.submit(fx.batch("p", "run", 1..=5).await).unwrap();
    producer.flush().await;
    assert_eq!(waiting.await.unwrap(), counts(5, 0));
    let late = fx.batch("p", "late", 1..=1).await;
    producer.close().await;
    assert_eq!(fx.stored("p", "run").await.len(), 5);
    // A new producer on the same storage owns the lane again.
    let again = fx.producer();
    assert_eq!(again.submit(late).unwrap().await.unwrap(), counts(1, 0));
    again.close().await;
}

#[tokio::test]
async fn a_batch_behind_another_connections_write_lock_commits_once_it_is_released() {
    let fx = fixture();
    let producer = fx.producer();
    let batch = fx.batch("p", "run", 1..=20).await;
    // A real second connection holds the write lock: the writer's BEGIN waits.
    let rival = rusqlite::Connection::open(&fx.database).unwrap();
    rival.execute_batch("BEGIN IMMEDIATE").unwrap();
    let submission = producer.submit(batch).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        fx.stored("p", "run").await.is_empty(),
        "nothing is durable while the lock is held"
    );
    assert_eq!(fx.reports().len(), 0, "no transaction has committed yet");
    rival.execute_batch("ROLLBACK").unwrap();
    assert_eq!(submission.await.unwrap(), counts(20, 0));
    assert_eq!(
        fx.stored("p", "run").await,
        (1..=20).map(event).collect::<Vec<_>>()
    );
    assert_eq!(fx.reports().len(), 1);
    assert_eq!(
        fx.reports()[0].attempts,
        1,
        "the writer waited; the batch was not rebuilt"
    );
    assert!(!fx.storage.failure_state().claims_suppressed());
    producer.close().await;
}

#[tokio::test]
async fn a_batch_the_store_cannot_hold_fails_alone_and_the_runs_beside_it_commit() {
    let fx = fixture();
    let producer = fx.producer();
    let lead = fx.batch("p", "lead", 1..=1).await;
    let a = fx.batch("p", "a", 1..=3).await;
    let mut poison = fx.batch("p", "poison", 1..=1).await;
    // One event past what a blob may hold: a refusal the transaction cannot survive.
    poison.events.push(big_event(
        2,
        usize::try_from(crate::blob::MAX_BLOB_BYTES).unwrap() + 1,
    ));
    let c = fx.batch("p", "c", 1..=2).await;
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let [a, poison, c] = [a, poison, c].map(|b| producer.submit(b).unwrap());
    stall.end().await;
    answered(lead).await.unwrap();
    assert_eq!(answered(a).await.unwrap(), counts(3, 0));
    let failure = answered(poison).await.unwrap_err();
    assert!(
        matches!(failure.error, StoreError::Backend(_)),
        "{failure:?}"
    );
    assert_eq!(
        (failure.committed, failure.not_stored),
        (counts(0, 0), 2),
        "the first event rolled back with the transaction, and is the caller's to keep"
    );
    assert_eq!(answered(c).await.unwrap(), counts(2, 0));
    assert!(fx.stored("p", "poison").await.is_empty());
    assert_eq!(fx.stored("p", "a").await.len(), 3);
    assert_eq!(fx.stored("p", "c").await.len(), 2);
    assert!(
        fx.storage.failure_state().failure().is_none(),
        "an ordinary refusal is not fatal"
    );
    producer.close().await;
}

#[tokio::test]
async fn a_commit_that_cannot_be_reconciled_fails_closed_and_says_its_outcome_is_unknown() {
    let mut once = true;
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events == 0 || !once {
                return None;
            }
            once = false;
            // Nine of the ten rows become durable: an atomic transaction cannot
            // do that, so the producer cannot say what happened and must not guess.
            tx.execute_batch("DELETE FROM run_events WHERE seq = 1; COMMIT")
                .unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let other = fx.batch("p", "other", 1..=2).await;
    let failure = producer
        .submit(fx.batch("p", "run", 1..=10).await)
        .unwrap()
        .await
        .unwrap_err();
    assert!(failure.outcome_unknown, "{failure:?}");
    assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 10));
    // Nothing more is submitted behind a transaction nobody can account for.
    assert!(matches!(producer.submit(other), Err(Rejected::Closed(..))));
    assert_eq!(
        fx.stored("p", "run").await.len(),
        9,
        "the database is left as it is"
    );
    producer.close().await;
}

#[tokio::test]
async fn the_real_clock_may_cut_anywhere_and_every_event_still_commits_once_in_order() {
    let mut fx = fixture();
    fx.options.now = Arc::new(Instant::now);
    let producer = fx.producer();
    let batches = [
        fx.batch("p", "a", 1..=700).await,
        fx.batch("p", "b", 1..=700).await,
        fx.batch("p", "c", 1..=700).await,
    ];
    let submissions: Vec<_> = batches
        .into_iter()
        .map(|b| producer.submit(b).unwrap())
        .collect();
    for submission in submissions {
        assert_eq!(submission.await.unwrap(), counts(700, 0));
    }
    let reports = fx.reports();
    assert_eq!(
        reports.iter().map(|r| r.events).sum::<usize>(),
        2100,
        "no event twice, none lost"
    );
    assert!(reports.iter().all(|r| r.accounting.rows <= 1000));
    for run in ["a", "b", "c"] {
        assert_eq!(fx.stored("p", run).await.len(), 700);
        let snapshot = fx.storage.read(owner("p")).await.unwrap();
        let tail = snapshot.read_events(run, 600, 1000).await.unwrap();
        assert_eq!(tail, (601..=700).map(event).collect::<Vec<_>>());
    }
    producer.close().await;
}

// ---- Admission: what the producer may hold (security review M1) ----

fn same_batch(returned: &EventBatch, sent: &EventBatch) {
    assert_eq!(returned.caller, sent.caller);
    assert_eq!(returned.run, sent.run);
    assert_eq!(returned.lineage, sent.lineage);
    assert_eq!(returned.epoch, sent.epoch);
    assert_eq!(
        returned.events, sent.events,
        "the caller still owns every event"
    );
}

#[tokio::test]
async fn a_first_batch_over_the_event_limit_is_returned_whole_and_unencoded() {
    let mut fx = fixture();
    fx.options.queue_limit = 10;
    let producer = fx.producer();
    let over = fx.batch("p", "over", 1..=11).await;
    let encoded = encoded_events();
    let Err(Rejected::TooLarge(returned)) = producer.submit(over.clone()) else {
        panic!("eleven events can never fit a limit of ten");
    };
    same_batch(&returned, &over);
    assert_eq!(encoded_events(), encoded, "nothing was encoded");
    assert_eq!(producer.retained(), (0, 0, 0), "nothing is kept");
    // Exactly the limit is admitted on an empty queue, and its capacity comes back.
    let exact = fx.batch("p", "exact", 1..=10).await;
    assert_eq!(
        answered(producer.submit(exact).unwrap()).await.unwrap(),
        counts(10, 0)
    );
    assert_eq!(producer.retained(), (0, 0, 0));
    assert!(fx.stored("p", "over").await.is_empty());
    producer.close().await;
}

#[tokio::test]
async fn empty_batches_saturate_their_own_limit_and_the_capacity_comes_back() {
    let mut fx = fixture();
    fx.options.batch_limit = 3;
    let producer = fx.producer();
    let [lead, e1, e2, e3] = [
        fx.batch("p", "lead", 1..=1).await,
        fx.empty_batch("p", "e1").await,
        fx.empty_batch("p", "e2").await,
        fx.batch("p", "e3", 1..=3).await,
    ];
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let e1 = producer.submit(e1.clone()).unwrap();
    let e2 = producer.submit(e2).unwrap();
    // The batch in the transaction counts: three batches wait, none of them for
    // many events, and the limit on events (16,384) is nowhere near.
    let (batches, events, _) = producer.retained();
    assert_eq!((batches, events), (3, 1));
    let encoded = encoded_events();
    let Err(Rejected::Full(returned)) = producer.submit(e3.clone()) else {
        panic!("a fourth batch does not fit a limit of three");
    };
    same_batch(&returned, &e3);
    assert_eq!(encoded_events(), encoded, "refused before it was encoded");
    stall.end().await;
    answered(lead).await.unwrap();
    assert_eq!(answered(e1).await.unwrap(), counts(0, 0));
    assert_eq!(answered(e2).await.unwrap(), counts(0, 0));
    assert_eq!(producer.retained(), (0, 0, 0), "all capacity came back");
    assert_eq!(
        answered(producer.submit(*returned).unwrap()).await.unwrap(),
        counts(3, 0),
        "the returned batch is admitted once there is room"
    );
    producer.close().await;
}

#[tokio::test]
async fn metadata_alone_counts_against_the_byte_limit() {
    let mut fx = fixture();
    let name = |i: u32| format!("{i}-{}", "r".repeat(4096));
    let [a, b] = [
        fx.empty_batch("p", &name(1)).await,
        fx.empty_batch("p", &name(2)).await,
    ];
    let held = accounted_bytes(&a);
    assert!(
        held > 2 * 4096,
        "the run name is held, and held again by its plan"
    );
    assert_eq!(held, accounted_bytes(&b));
    // Room for one and a half: the second empty batch waits for the first.
    fx.options.byte_limit = held + held / 2;
    let producer = fx.producer();
    let stall = fx.stall().await;
    let first = producer.submit(a).unwrap();
    assert_eq!(producer.retained(), (1, 0, held));
    let Err(Rejected::Full(returned)) = producer.submit(b.clone()) else {
        panic!("the second batch's strings do not fit beside the first's");
    };
    same_batch(&returned, &b);
    stall.end().await;
    assert_eq!(answered(first).await.unwrap(), counts(0, 0));
    assert_eq!(producer.retained(), (0, 0, 0));
    assert_eq!(
        answered(producer.submit(*returned).unwrap()).await.unwrap(),
        counts(0, 0)
    );
    producer.close().await;
}

#[tokio::test]
async fn the_byte_limit_is_exact_and_a_batch_past_it_is_returned_whole() {
    let mut fx = fixture();
    let batch = fx.batch("p", "run", 1..=5).await;
    let held = accounted_bytes(&batch);
    // Exactly the envelope is admitted on an empty queue.
    fx.options.byte_limit = held;
    let producer = fx.producer();
    assert_eq!(
        answered(producer.submit(batch.clone()).unwrap())
            .await
            .unwrap(),
        counts(5, 0)
    );
    assert_eq!(producer.retained(), (0, 0, 0));
    producer.close().await;
    // One byte less can never be admitted, however little waits.
    let other = fx.batch("p", "other", 1..=5).await;
    let held_other = accounted_bytes(&other);
    fx.options.byte_limit = held_other - 1;
    let producer = fx.producer();
    let Err(Rejected::TooLarge(returned)) = producer.submit(other.clone()) else {
        panic!("one byte over the envelope");
    };
    same_batch(&returned, &other);
    assert_eq!(producer.retained(), (0, 0, 0));
    assert!(fx.stored("p", "other").await.is_empty());
    producer.close().await;
}

#[tokio::test]
async fn an_event_that_does_not_fit_stops_the_encoding_of_the_rest() {
    let mut fx = fixture();
    let (lineage, epoch) = fx.lease("p", "run").await;
    // Five events of one size: the envelope holds the strings and two of them,
    // less a byte, so the second is the first that does not fit.
    let big = |seq| big_event(seq, 10_000);
    let batch = EventBatch {
        caller: owner("p"),
        run: "run".into(),
        lineage,
        epoch,
        events: (1..=5).map(big).collect(),
    };
    let fixed = batch.fixed_bytes();
    let one = (accounted_bytes(&batch) - fixed) / 5;
    fx.options.byte_limit = fixed + 2 * one - 1;
    let producer = fx.producer();
    let encoded = encoded_events();
    let Err(Rejected::TooLarge(returned)) = producer.submit(batch.clone()) else {
        panic!("five events do not fit an envelope of fewer than two");
    };
    same_batch(&returned, &batch);
    assert_eq!(
        encoded_events() - encoded,
        2,
        "the first event fits, the second does not, the other three are never encoded"
    );
    assert_eq!(producer.retained(), (0, 0, 0));
    producer.close().await;
}

#[tokio::test]
async fn a_full_queue_refuses_a_large_batch_without_encoding_it_and_the_default_envelope_takes_a_blob()
 {
    let mut fx = fixture();
    let (lineage, epoch) = fx.lease("p", "run").await;
    let large = EventBatch {
        caller: owner("p"),
        run: "run".into(),
        lineage,
        epoch,
        events: vec![big_event(1, 1024 * 1024)],
    };
    let held = accounted_bytes(&large);
    let lead = fx.batch("p", "lead", 1..=1).await;
    let lead_held = accounted_bytes(&lead);
    // Room for the lead and not for the large batch beside it.
    fx.options.byte_limit = lead_held + held - 1;
    let producer = fx.producer();
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let encoded = encoded_events();
    let Err(Rejected::Full(returned)) = producer.submit(large.clone()) else {
        panic!("it fits the envelope alone, not beside the lead");
    };
    // The strings did not exclude it, so its one event was measured and found
    // not to fit what is free; nothing past that was looked at.
    assert!(encoded_events() - encoded <= 1);
    same_batch(&returned, &large);
    stall.end().await;
    answered(lead).await.unwrap();
    assert_eq!(
        answered(producer.submit(*returned).unwrap()).await.unwrap(),
        counts(1, 0),
        "alone, it is admitted"
    );
    producer.close().await;
    // The default envelope admits one event of the size of a blob, committed
    // whole (`an_atomic_operation_over_the_bound_runs_whole_and_is_counted`
    // stores 48 MiB through it) and an event one byte past what a blob holds, which
    // fails alone (`a_batch_the_store_cannot_hold_fails_alone_...`).
    assert!(EventProducerOptions::default().byte_limit >= 3 * 64 * 1024 * 1024);
}

#[tokio::test]
async fn capacity_returns_after_completion_after_a_refusal_and_exactly_fills_again() {
    let mut fx = fixture();
    fx.options.queue_limit = 10;
    let producer = fx.producer();
    for run in ["x", "y", "z"] {
        let batch = fx.batch("p", run, 1..=5).await;
        assert_eq!(
            answered(producer.submit(batch).unwrap()).await.unwrap(),
            counts(5, 0)
        );
        assert_eq!(
            producer.retained(),
            (0, 0, 0),
            "{run} gave its capacity back"
        );
    }
    // A refused batch (its lease is stale) leaves nothing behind either.
    let mut stale = fx.batch("p", "stale", 1..=4).await;
    stale.epoch += 1;
    let failure = answered(producer.submit(stale).unwrap()).await.unwrap_err();
    assert!(matches!(failure.error, StoreError::StaleEpoch));
    assert_eq!(producer.retained(), (0, 0, 0));
    // So all ten events of the limit are free again, while one is in flight.
    let lead = fx.batch("p", "lead", 1..=1).await;
    let (a, b) = (
        fx.batch("p", "a", 1..=4).await,
        fx.batch("p", "b", 1..=5).await,
    );
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let a = producer.submit(a).unwrap();
    let b = producer.submit(b).unwrap();
    assert_eq!(producer.retained().1, 10, "exactly the limit");
    stall.end().await;
    answered(lead).await.unwrap();
    assert_eq!(answered(a).await.unwrap(), counts(4, 0));
    assert_eq!(answered(b).await.unwrap(), counts(5, 0));
    assert_eq!(producer.retained(), (0, 0, 0));
    producer.close().await;
}

// ---- The producer's task ends: nothing is left waiting (code review I1) ----

/// A second producer on the same storage commits a batch: the writer that the
/// first one used is healthy, and the failure was the task's, not the store's.
async fn the_writer_still_serves(fx: &Fixture) {
    assert!(fx.storage.failure_state().failure().is_none());
    let next = EventProducer::new(fx.storage.clone(), EventProducerOptions::default());
    let batch = fx.batch("p", "after", 1..=2).await;
    assert_eq!(
        answered(next.submit(batch).unwrap()).await.unwrap(),
        counts(2, 0)
    );
    assert_eq!(fx.stored("p", "after").await.len(), 2);
    fx.storage
        .transaction(owner("p"), Lane::Maintenance, |_| Ok(()))
        .await
        .unwrap();
    next.close().await;
}

#[tokio::test]
async fn a_panic_in_the_clock_answers_every_held_batch_and_closes_intake() {
    let mut fx = fixture();
    // The clock runs inside the transaction, on the writer's thread; the retained
    // write resumes its panic on the producer's task.
    fx.options.now = Arc::new(|| panic!("synthetic clock panic"));
    let producer = fx.producer();
    let lead = fx.batch("p", "lead", 1..=3).await;
    let waiting = fx.batch("p", "waiting", 1..=4).await;
    let late = fx.batch("p", "late", 1..=2).await;
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let waiting = producer.submit(waiting).unwrap();
    stall.end().await;
    let started = answered(lead).await.unwrap_err();
    assert!(
        started.outcome_unknown,
        "the transaction it was in did not report: its outcome is unknown"
    );
    assert_eq!((started.committed, started.not_stored), (counts(0, 0), 3));
    let queued = answered(waiting).await.unwrap_err();
    assert!(!queued.outcome_unknown, "it never reached a transaction");
    assert_eq!((queued.committed, queued.not_stored), (counts(0, 0), 4));
    assert!(matches!(queued.error, StoreError::Backend(_)), "{queued:?}");
    // Intake is closed and gives the batch back, unencoded; flush does not wait.
    let encoded = encoded_events();
    let Err(Rejected::Closed(returned, cause)) = producer.submit(late.clone()) else {
        panic!("a producer whose task ended takes nothing");
    };
    same_batch(&returned, &late);
    assert!(cause.contains("ended"), "{cause}");
    assert_eq!(encoded_events(), encoded);
    tokio::time::timeout(SAFETY, producer.flush())
        .await
        .expect("flush returns once nothing is held");
    assert_eq!(producer.retained(), (0, 0, 0));
    assert!(fx.stored("p", "lead").await.is_empty());
    assert!(fx.stored("p", "waiting").await.is_empty());
    the_writer_still_serves(&fx).await;
    tokio::time::timeout(SAFETY, producer.close())
        .await
        .expect("close returns");
}

#[tokio::test]
async fn a_panic_in_the_observer_answers_what_it_completed_and_then_everything_held() {
    let mut fx = fixture();
    // The first transaction is reported quietly; the second one's report panics.
    let reports = Arc::new(AtomicU64::new(0));
    fx.options.observer = Some(Arc::new(move |_| {
        assert!(
            reports.fetch_add(1, Ordering::SeqCst) == 0,
            "synthetic observer panic"
        );
    }));
    let producer = fx.producer();
    let lead = fx.batch("p", "lead", 1..=1).await;
    let short = fx.batch("p", "short", 1..=3).await;
    let long = fx.batch("p", "long", 1..=1200).await;
    let behind = fx.batch("p", "behind", 1..=2).await;
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let [short, long, behind] = [short, long, behind].map(|b| producer.submit(b).unwrap());
    stall.end().await;
    answered(lead).await.unwrap();
    // The second transaction holds `short` and the first 997 events of `long`,
    // and commits. Its observer panics only after that is known and the batch
    // it completed has been answered.
    assert_eq!(answered(short).await.unwrap(), counts(3, 0));
    let cut = answered(long).await.unwrap_err();
    assert_eq!((cut.committed, cut.not_stored), (counts(997, 0), 203));
    assert!(
        !cut.outcome_unknown,
        "the commit of its prefix was known, and nothing of its tail started"
    );
    let queued = answered(behind).await.unwrap_err();
    assert!(!queued.outcome_unknown);
    assert_eq!((queued.committed, queued.not_stored), (counts(0, 0), 2));
    assert!(matches!(
        producer.submit(fx.batch("p", "late", 1..=1).await),
        Err(Rejected::Closed(..))
    ));
    tokio::time::timeout(SAFETY, producer.flush())
        .await
        .expect("flush returns once nothing is held");
    assert_eq!(producer.retained(), (0, 0, 0));
    assert_eq!(fx.stored("p", "short").await.len(), 3);
    assert_eq!(fx.stored("p", "long").await.len(), 997);
    assert!(fx.stored("p", "behind").await.is_empty());
    the_writer_still_serves(&fx).await;
    tokio::time::timeout(SAFETY, producer.close())
        .await
        .expect("close returns");
}

// ---- Order, plans and accounting that only the database can show ----

#[tokio::test]
async fn a_lost_unknown_commit_that_also_carried_stored_duplicates_stays_unknown() {
    // Three of the ten events are already stored and are skipped as duplicates
    // in the same statement as the seven new ones. The COMMIT then loses the
    // seven and reports an error nobody can classify. Reconciliation finds three
    // of the ten expected rows: neither all nor none. The producer does not turn
    // that into "not stored" and rebuild; it says the outcome is unknown, closes,
    // and leaves the collector to decide (code review N1, kept as it is).
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&armed);
    let mut once = true;
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            if !flag.load(Ordering::SeqCst) || !once {
                return None;
            }
            once = false;
            tx.execute_batch("DELETE FROM run_events WHERE seq > 3; COMMIT")
                .unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let batch = fx.batch("p", "run", 1..=10).await;
    let (lineage, epoch) = (batch.lineage.clone(), batch.epoch);
    fx.storage
        .transaction(owner("p"), Lane::Events, move |unit| {
            unit.append_events("run", &lineage, epoch, &[event(1), event(2), event(3)])
        })
        .await
        .unwrap();
    armed.store(true, Ordering::SeqCst);
    let failure = answered(producer.submit(batch.clone()).unwrap())
        .await
        .unwrap_err();
    assert!(failure.outcome_unknown, "{failure:?}");
    assert!(
        failure.error.to_string().contains("only some rows"),
        "{failure:?}"
    );
    assert_eq!(fx.reports().len(), 0, "no commit was ever reported");
    assert!(matches!(producer.submit(batch), Err(Rejected::Closed(..))));
    assert_eq!(fx.stored("p", "run").await.len(), 3, "only the duplicates");
    producer.close().await;
}

#[tokio::test]
async fn an_event_with_a_blob_is_a_statement_of_its_own_and_the_cut_falls_around_it() {
    // 7 ms a reading: the first statement is free and three fit before 20 ms.
    // Light events, one that needs a blob, light events again, another blob:
    // [3 light] [blob] [3 light] and the fourth statement is cut. If a blob event
    // shared its statement, the whole batch would be one statement and no cut.
    let mut fx = fixture();
    fx.options.now = stepped_clock(Duration::from_millis(7));
    let producer = fx.producer();
    let (lineage, epoch) = fx.lease("p", "run").await;
    let big = 64 * 1024 + 1;
    let mut events: Vec<_> = (1..=3).map(event).collect();
    events.push(big_event(4, big));
    events.extend((5..=7).map(event));
    events.push(big_event(8, big));
    events.extend((9..=12).map(event));
    let batch = EventBatch {
        caller: owner("p"),
        run: "run".into(),
        lineage,
        epoch,
        events,
    };
    assert_eq!(
        answered(producer.submit(batch).unwrap()).await.unwrap(),
        counts(12, 0)
    );
    let shape: Vec<_> = fx
        .reports()
        .iter()
        .map(|r| (r.events, r.cut_by_time))
        .collect();
    assert_eq!(shape, [(7, true), (5, false)]);
    assert_eq!(fx.stored("p", "run").await.len(), 12);
    producer.close().await;
}

#[tokio::test]
async fn the_planned_rows_equal_the_actual_rows_at_the_inline_threshold() {
    // 65,536 bytes of JSON stay in the row, one more goes behind a blob (N5).
    let fx = fixture();
    let producer = fx.producer();
    let (lineage, epoch) = fx.lease("p", "run").await;
    let base = serde_json::to_vec(&big_event(1, 0).event).unwrap().len();
    let to = |seq, total: usize| big_event(seq, total - base);
    let batch = EventBatch {
        caller: owner("p"),
        run: "run".into(),
        lineage,
        epoch,
        events: vec![to(1, 64 * 1024 - 1), to(2, 64 * 1024), to(3, 64 * 1024 + 1)],
    };
    assert_eq!(
        answered(producer.submit(batch).unwrap()).await.unwrap(),
        counts(3, 0)
    );
    // One row each for the first two, two for the third: the plan's cost is the
    // writer's own count of rows it changed.
    assert_eq!(fx.reports()[0].accounting.rows, 4);
    producer.close().await;
}

/// Allocation capacities, not just visible JSON bytes, stay inside the envelope.
#[tokio::test]
async fn metadata_spares_are_refused_and_payload_spares_are_released() {
    let mut fx = fixture();
    fx.options.byte_limit = 8192;
    let producer = fx.producer();
    for kind in 0..9 {
        let mut batch = fx.empty_batch("p", &format!("capacity-{kind}")).await;
        let mut spare = String::with_capacity(32768);
        spare.push('x');
        match kind {
            0 => batch.run = spare,
            1 => batch.lineage = spare,
            2 => batch.caller.project = spare,
            3 => batch.caller.principal = orch_core::store::Principal::Owner { identity: spare },
            _ => {
                let mut item = event(1);
                match kind {
                    4 => item.event.timestamp = spare,
                    5 => item.event.data = serde_json::Value::String(spare),
                    6 => item.event.data = serde_json::Value::Array(Vec::with_capacity(32768)),
                    7 => {
                        item.event.data =
                            serde_json::Value::Array(vec![serde_json::Value::Null; 200])
                    }
                    _ => {
                        let mut object = serde_json::Map::new();
                        object.insert(spare, serde_json::Value::Null);
                        item.event.data = serde_json::Value::Object(object);
                    }
                }
                batch.events.push(item);
            }
        }
        if kind >= 4 {
            let expected = serde_json::to_string(&batch.events[0].event).unwrap();
            let charged = accounted_bytes(&batch);
            assert!(charged < 8192, "only the live encoding is retained: {kind}");
            assert_eq!(
                answered(producer.submit(batch).unwrap()).await.unwrap(),
                counts(1, 0)
            );
            let stored = fx.stored("p", &format!("capacity-{kind}")).await;
            assert_eq!(serde_json::to_string(&stored[0].event).unwrap(), expected);
            assert_eq!(producer.retained(), (0, 0, 0));
            continue;
        }
        let pointer = batch.run.as_ptr();
        let capacity = batch.run.capacity();
        let expected = batch.clone();
        let Err(Rejected::TooLarge(returned)) = producer.submit(batch) else {
            panic!("capacity-heavy input {kind} escaped the retained-byte envelope");
        };
        same_batch(&returned, &expected);
        assert_eq!(
            returned.run.as_ptr(),
            pointer,
            "refusal preserves allocation"
        );
        assert_eq!(returned.run.capacity(), capacity);
        assert_eq!(producer.retained(), (0, 0, 0));
    }
    producer.close().await;
}

#[tokio::test]
async fn principal_spare_fields_are_returned_whole_before_encoding() {
    use orch_core::store::Principal;

    fn selected(principal: &Principal, kind: usize) -> &String {
        match principal {
            Principal::Worker { identity } => identity,
            Principal::Task {
                identity,
                task,
                run,
                lineage,
                ..
            } => [identity, task, run, lineage][kind - 1],
            _ => unreachable!("the fixture supplies Worker or Task"),
        }
    }

    let mut fx = fixture();
    fx.options.byte_limit = 8192;
    let producer = fx.producer();
    for kind in 0..5 {
        let mut batch = fx.empty_batch("p", &format!("principal-{kind}")).await;
        let mut spare = String::with_capacity(32768);
        spare.push('x');
        batch.caller.principal = if kind == 0 {
            Principal::Worker { identity: spare }
        } else {
            let mut fields = std::array::from_fn::<_, 4, _>(|_| String::from("x"));
            fields[kind - 1] = spare;
            let [identity, task, run, lineage] = fields;
            Principal::Task {
                identity,
                task,
                run,
                lineage,
                epoch: 1,
            }
        };
        batch.events.push(event(1));
        let pointer = selected(&batch.caller.principal, kind).as_ptr();
        let capacity = selected(&batch.caller.principal, kind).capacity();
        let expected = batch.clone();
        let encoded = encoded_events();
        let Err(Rejected::TooLarge(returned)) = producer.submit(batch) else {
            panic!("principal field {kind} escaped the metadata envelope");
        };
        same_batch(&returned, &expected);
        assert_eq!(selected(&returned.caller.principal, kind).as_ptr(), pointer);
        assert_eq!(
            selected(&returned.caller.principal, kind).capacity(),
            capacity
        );
        assert_eq!(encoded_events(), encoded, "metadata refusal is cheap");
        assert_eq!(producer.retained(), (0, 0, 0));
    }
    producer.close().await;
}

#[tokio::test]
async fn observer_panic_on_the_last_commit_closes_intake() {
    let mut fx = fixture();
    fx.options.observer = Some(Arc::new(|_| panic!("synthetic observer panic, last batch")));
    let producer = fx.producer();
    let b = fx.batch("p", "only", 1..=3).await;
    assert_eq!(
        answered(producer.submit(b).unwrap()).await.unwrap(),
        counts(3, 0)
    );
    // Nothing left to answer. Is intake closed, or does it accept work nobody will process?
    let late = fx.batch("p", "late", 1..=2).await;
    match producer.submit(late) {
        Err(Rejected::Closed(..)) => {}
        Ok(s) => {
            let r = tokio::time::timeout(Duration::from_secs(3), s).await;
            panic!(
                "PROBE-A FAIL: submit accepted after the task ended; answer within 3s: {:?}",
                r.is_ok()
            );
        }
        Err(other) => panic!("unexpected {other:?}"),
    }
    tokio::time::timeout(SAFETY, producer.flush())
        .await
        .expect("flush");
    producer.close().await;
}

#[tokio::test]
async fn a_waiting_flush_is_released_when_the_task_panics() {
    let mut fx = fixture();
    fx.options.now = Arc::new(|| panic!("synthetic clock panic"));
    let producer = Arc::new(fx.producer());
    let lead = fx.batch("p", "lead", 1..=3).await;
    let stall = fx.stall().await;
    let lead = producer.submit(lead).unwrap();
    in_flight(&producer).await;
    let mut flusher = Box::pin(producer.flush());
    assert!(
        futures::poll!(flusher.as_mut()).is_pending(),
        "flush must register and wait while a transaction is held"
    );
    stall.end().await;
    let started = answered(lead).await.unwrap_err();
    assert!(started.outcome_unknown);
    tokio::time::timeout(Duration::from_secs(5), flusher)
        .await
        .expect("PROBE-B FAIL: a flush that was already waiting never returns");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_submissions_never_exceed_the_byte_limit() {
    for round in 0..6 {
        let mut fx = fixture();
        let big = |seq| big_event(seq, 2 * 1024 * 1024);
        let lead = fx.batch("p", "lead", 1..=1).await;
        let lead_held = accounted_bytes(&lead);
        let mut cands = Vec::new();
        for i in 0..16 {
            let (lineage, epoch) = fx.lease("p", &format!("c{i}")).await;
            cands.push(EventBatch {
                caller: owner("p"),
                run: format!("c{i}"),
                lineage,
                epoch,
                events: vec![big(1)],
            });
        }
        let held = accounted_bytes(&cands[0]);
        fx.options.byte_limit = lead_held + held + held / 2;
        let producer = Arc::new(fx.producer());
        let stall = fx.stall().await;
        let lead = producer.submit(lead).unwrap();
        in_flight(&producer).await;
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = cands
            .into_iter()
            .map(|b| {
                let (p, bar) = (Arc::clone(&producer), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    let expected = b.clone();
                    bar.wait();
                    (expected, p.submit(b))
                })
            })
            .collect();
        let candidates: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let ok = candidates
            .iter()
            .filter(|(_, answer)| answer.is_ok())
            .count();
        let (_, _, bytes) = producer.retained();
        assert!(
            bytes <= fx.options.byte_limit,
            "round {round}: PROBE-D FAIL held {bytes} > {} ({ok} admitted)",
            fx.options.byte_limit
        );
        assert_eq!(
            ok, 1,
            "round {round}: PROBE-D exactly one candidate fits ({ok})"
        );
        stall.end().await;
        assert_eq!(answered(lead).await.unwrap(), counts(1, 0));
        for (expected, answer) in candidates {
            match answer {
                Ok(submission) => {
                    assert_eq!(answered(submission).await.unwrap(), counts(1, 0));
                    assert_eq!(fx.stored("p", &expected.run).await, expected.events);
                }
                Err(Rejected::Full(returned)) => {
                    same_batch(&returned, &expected);
                    assert!(fx.stored("p", &expected.run).await.is_empty());
                }
                Err(other) => panic!("round {round}: unexpected refusal: {other:?}"),
            }
        }
        assert_eq!(producer.retained(), (0, 0, 0));
        Arc::try_unwrap(producer)
            .unwrap_or_else(|_| panic!("all submitters have joined"))
            .close()
            .await;
    }
}

#[tokio::test]
async fn an_unknown_nonretryable_commit_with_no_durable_rows_is_never_rebuilt() {
    let mut once = true;
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            let events: i64 = tx
                .query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))
                .unwrap();
            if events == 0 || !once {
                return None;
            }
            once = false;
            // Nothing becomes durable, the transaction is no longer open, and the
            // error (SQLITE_ERROR) is one that allows no retry.
            tx.execute_batch("DELETE FROM run_events; COMMIT").unwrap();
            Some(Err(sql(1)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let first = fx.batch("p", "run", 1..=4).await;
    let other = fx.batch("p", "other", 1..=2).await;
    let failure = answered(producer.submit(first).unwrap()).await.unwrap_err();
    assert!(
        !failure.outcome_unknown,
        "found none of its rows: {failure:?}"
    );
    assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 4));
    assert!(fx.stored("p", "run").await.is_empty(), "never replayed");
    // The producer survives and serves the next batch.
    assert_eq!(
        answered(producer.submit(other).unwrap()).await.unwrap(),
        counts(2, 0)
    );
    producer.close().await;
}

#[tokio::test]
async fn encoded_dense_shapes_fit_the_default_producer_and_store() {
    for shape in 0..2 {
        let fx = fixture();
        let producer = fx.producer();
        let mut batch = fx.batch("p", "dense", 1..=1).await;
        batch.events[0].event.data = if shape == 0 {
            serde_json::Value::Array(
                (0..100_000)
                    .map(|_| serde_json::json!({"a": 1, "b": 2, "c": 3}))
                    .collect(),
            )
        } else {
            serde_json::Value::Array(vec![serde_json::json!(1); 4_000_000])
        };
        let expected = serde_json::to_string(&batch.events[0].event).unwrap();
        let submission = producer
            .submit(batch)
            .unwrap_or_else(|_| panic!("dense shape {shape} below 64MiB must fit"));
        assert_eq!(answered(submission).await.unwrap(), counts(1, 0));
        let stored = fx.stored("p", "dense").await;
        assert_eq!(serde_json::to_string(&stored[0].event).unwrap(), expected);
        assert_eq!(producer.retained(), (0, 0, 0));
        producer.close().await;
    }
}

#[tokio::test]
async fn encoded_boundaries_store_exact_physical_bytes_and_isolate_plus_one() {
    use sha2::{Digest, Sha256};
    let maximum = crate::blob::MAX_BLOB_BYTES as usize;
    for shape in 0..2 {
        let fx = fixture();
        let producer = fx.producer();
        let mut batch = fx.batch("p", "boundary", 1..=2).await;
        let mut item = event(1);
        item.event.data = if shape == 0 {
            serde_json::Value::String(String::new())
        } else {
            let mut items = vec![serde_json::json!(1); 4_000_000];
            items.push(serde_json::Value::String(String::new()));
            serde_json::Value::Array(items)
        };
        let base = super::super::encode(&item.event).unwrap().2.len();
        let padding = "x".repeat(maximum - base);
        match &mut item.event.data {
            serde_json::Value::String(s) => *s = padding,
            serde_json::Value::Array(a) => *a.last_mut().unwrap() = padding.into(),
            _ => unreachable!(),
        }
        let (at, kind, bytes) = super::super::encode(&item.event).unwrap();
        assert_eq!(bytes.len(), maximum);
        let expected_hash = format!("{:x}", Sha256::digest(bytes.as_bytes()));
        drop(bytes);
        batch.events[0] = item;
        assert!(accounted_bytes(&batch) < EventProducerOptions::default().byte_limit);
        assert_eq!(
            answered(producer.submit(batch).unwrap()).await.unwrap(),
            counts(2, 0)
        );
        let conn = rusqlite::Connection::open(&fx.database).unwrap();
        let (stored_at, stored_kind, tag): (i64, String, String) = conn.query_row(
            "SELECT at,type,payload FROM run_events WHERE project_id='p' AND run_id='boundary' AND seq=1",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        let tag: [String; 2] = serde_json::from_str(&tag).unwrap();
        assert_eq!(
            tag,
            ["orch.event.blob.v1".to_owned(), expected_hash.clone()]
        );
        assert_eq!((stored_at, stored_kind.as_str()), (at, kind));
        let path = fx
            .database
            .parent()
            .unwrap()
            .join("blobs/sha256")
            .join(&expected_hash[..2])
            .join(&expected_hash[2..4])
            .join(&expected_hash);
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len(), maximum);
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected_hash);
        drop(bytes);
        let order: Vec<i64> = conn
            .prepare("SELECT seq FROM run_events WHERE run_id='boundary' ORDER BY seq")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(order, [1, 2]);
        assert_eq!(producer.retained(), (0, 0, 0));
        producer.close().await;
    }
    let fx = fixture();
    let producer = fx.producer();
    let mut poison = fx.batch("p", "plus-one", 1..=1).await;
    let base = super::super::encode(&big_event(1, 0).event)
        .unwrap()
        .2
        .len();
    poison.events[0] = big_event(1, maximum + 1 - base);
    assert_eq!(
        super::super::encode(&poison.events[0].event)
            .unwrap()
            .2
            .len(),
        maximum + 1
    );
    let good = fx.batch("p", "beside", 1..=2).await;
    let stall = fx.stall().await;
    let poison = producer.submit(poison).unwrap();
    let good = producer.submit(good).unwrap();
    stall.end().await;
    let failure = answered(poison).await.unwrap_err();
    assert!(!failure.outcome_unknown);
    assert_eq!(failure.not_stored, 1);
    assert_eq!(answered(good).await.unwrap(), counts(2, 0));
    assert!(fx.stored("p", "plus-one").await.is_empty());
    assert_eq!(producer.retained(), (0, 0, 0));
    producer.close().await;
}

#[tokio::test]
async fn encoded_unknown_commit_reconciles_escaped_ordered_numbers_and_accepted_nesting() {
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = armed.clone();
    let mut once = true;
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            if !flag.load(Ordering::SeqCst) || !once {
                return None;
            }
            once = false;
            tx.execute_batch("COMMIT").unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let mut batch = fx.batch("p", "exact-nested", 1..=2).await;
    batch.events[0].event.timestamp = "2026-11-27T01:02:03.456-05:00".into();
    let mut data = serde_json::json!({"z": "\n\t\"\\é", "a": [18446744073709551615u64, -0.0, 1.25e100], "m": {"b": 1, "a": 2}});
    for _ in 0..160 {
        data = serde_json::json!([data]);
    }
    batch.events[0].event.data = data;
    let expected: Vec<_> = batch
        .events
        .iter()
        .map(|e| super::super::encode(&e.event).unwrap())
        .collect();
    armed.store(true, Ordering::SeqCst);
    assert_eq!(
        answered(producer.submit(batch).unwrap()).await.unwrap(),
        counts(2, 0)
    );
    assert_eq!(
        fx.reports()[0].attempts,
        1,
        "lost acknowledgement reconciles without replay"
    );
    let conn = rusqlite::Connection::open(&fx.database).unwrap();
    for (index, (at, kind, payload)) in expected.into_iter().enumerate() {
        let stored: (i64, String, String) = conn
            .query_row(
                "SELECT at,type,payload FROM run_events WHERE run_id='exact-nested' AND seq=?1",
                [(index + 1) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored, (at, kind.to_owned(), payload));
    }
    assert_eq!(producer.retained(), (0, 0, 0));
    producer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encoded_budget_stays_charged_through_in_flight_ownership() {
    let mut fx = fixture();
    let batch = fx.batch("p", "charged", 1..=1).await;
    let held = accounted_bytes(&batch);
    let next = fx.batch("p", "another", 1..=1).await;
    fx.options.byte_limit = held.max(accounted_bytes(&next));
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, blocked) = mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    fx.options.observer = Some(Arc::new(move |_| {
        if let Some(entered) = entered.lock().unwrap().take() {
            let _ = entered.send(());
            blocked.lock().unwrap().recv_timeout(SAFETY).unwrap();
        }
    }));
    let producer = fx.producer();
    let submission = producer.submit(batch).unwrap();
    tokio::time::timeout(SAFETY, observed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        producer.retained(),
        (0, 0, held),
        "completed plan still owns frozen bytes"
    );
    let Err(Rejected::Full(next)) = producer.submit(next) else {
        panic!("in-flight ownership must consume the budget");
    };
    release.send(()).unwrap();
    assert_eq!(answered(submission).await.unwrap(), counts(1, 0));
    tokio::time::timeout(SAFETY, producer.flush())
        .await
        .unwrap();
    assert_eq!(producer.retained(), (0, 0, 0));
    assert_eq!(
        answered(producer.submit(*next).unwrap()).await.unwrap(),
        counts(1, 0)
    );
    producer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answered_encoded_batch_has_already_returned_its_exact_budget() {
    let mut fx = fixture();
    let batch = fx.batch("p", "r", 1..=1).await;
    fx.options.byte_limit = accounted_bytes(&batch);
    let producer = fx.producer();
    for _ in 0..1000 {
        let result = producer
            .submit(batch.clone())
            .expect("a received receipt returns exact admission capacity");
        answered(result).await.unwrap();
        assert_eq!(
            producer.retained(),
            (0, 0, 0),
            "receipt must follow the final owner and refund"
        );
    }
    producer.close().await;
}

#[tokio::test]
async fn encoded_reconciliation_refuses_changed_payload_time_or_type_without_replay() {
    for changed in 0..3 {
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = armed.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let mut replacement = event(1).event;
        replacement.data = serde_json::json!("different durable bytes");
        let replacement = serde_json::to_string(&replacement).unwrap();
        let fx = fixture_with(StorageOptions {
            writer_commit_probe: Some(Box::new(move |tx| {
                if !flag.load(Ordering::SeqCst) {
                    return None;
                }
                seen.fetch_add(1, Ordering::SeqCst);
                match changed {
                    0 => {
                        tx.execute(
                            "UPDATE run_events SET payload=?1 WHERE run_id='changed' AND seq=1",
                            [&replacement],
                        )
                        .unwrap();
                    }
                    1 => {
                        tx.execute(
                            "UPDATE run_events SET at=at+1 WHERE run_id='changed' AND seq=1",
                            [],
                        )
                        .unwrap();
                    }
                    _ => {
                        tx.execute(
                            "UPDATE run_events SET type='error' WHERE run_id='changed' AND seq=1",
                            [],
                        )
                        .unwrap();
                    }
                }
                tx.execute_batch("COMMIT").unwrap();
                Some(Err(sql(10)))
            })),
            ..StorageOptions::default()
        });
        let producer = fx.producer();
        let batch = fx.batch("p", "changed", 1..=1).await;
        armed.store(true, Ordering::SeqCst);
        let failure = answered(producer.submit(batch.clone()).unwrap())
            .await
            .unwrap_err();
        assert!(
            failure.outcome_unknown,
            "changed field {changed} cannot reconcile as committed"
        );
        assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 1));
        assert!(
            failure
                .error
                .to_string()
                .contains("differs from the expected"),
            "{failure:?}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "unknown input is never replayed"
        );
        assert!(fx.reports().is_empty());
        assert!(matches!(producer.submit(batch), Err(Rejected::Closed(..))));
        assert_eq!(producer.retained(), (0, 0, 0));
        producer.close().await;
    }
}

#[tokio::test]
async fn encoded_unknown_commit_reconciles_the_first_duplicate_bytes() {
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = armed.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let fx = fixture_with(StorageOptions {
        writer_commit_probe: Some(Box::new(move |tx| {
            if !flag.load(Ordering::SeqCst) {
                return None;
            }
            seen.fetch_add(1, Ordering::SeqCst);
            tx.execute_batch("COMMIT").unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let producer = fx.producer();
    let mut batch = fx.batch("p", "duplicate-bytes", 1..=1).await;
    let first = batch.events[0].clone();
    let mut other = first.clone();
    other.event.data = serde_json::json!({"same sequence": "different bytes"});
    batch.events.push(other);
    armed.store(true, Ordering::SeqCst);
    assert_eq!(
        answered(producer.submit(batch).unwrap()).await.unwrap(),
        counts(1, 1)
    );
    assert_eq!(fx.stored("p", "duplicate-bytes").await, [first]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fx.reports()[0].attempts, 1);
    assert!(!fx.storage.failure_state().claims_suppressed());
    assert_eq!(producer.retained(), (0, 0, 0));
    producer.close().await;
}

#[tokio::test]
async fn encoded_unknown_empty_or_duplicate_commit_needs_no_rebuild() {
    for empty in [true, false] {
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = armed.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let fx = fixture_with(StorageOptions {
            writer_commit_probe: Some(Box::new(move |tx| {
                if !flag.load(Ordering::SeqCst) {
                    return None;
                }
                if seen.fetch_add(1, Ordering::SeqCst) > 0 {
                    return None;
                }
                tx.execute_batch("COMMIT").unwrap();
                Some(Err(sql(10)))
            })),
            ..StorageOptions::default()
        });
        let mut batch = fx.batch("p", "no-new-rows", 1..=1).await;
        if empty {
            batch.events.clear();
        } else {
            let seed = batch.clone();
            fx.storage
                .transaction(owner("p"), Lane::Events, move |unit| {
                    unit.append_events(&seed.run, &seed.lineage, seed.epoch, &seed.events)
                })
                .await
                .unwrap();
        }
        let producer = fx.producer();
        armed.store(true, Ordering::SeqCst);
        let skipped = usize::from(!empty);
        assert_eq!(
            answered(producer.submit(batch).unwrap()).await.unwrap(),
            counts(0, skipped)
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no durable rows expected; do not rebuild a consumed operation"
        );
        assert_eq!(fx.reports()[0].attempts, 1);
        assert!(!fx.storage.failure_state().claims_suppressed());
        assert_eq!(producer.retained(), (0, 0, 0));
        producer.close().await;
    }
}

#[tokio::test]
async fn encoded_invalid_sequences_refuse_the_whole_batch_and_leave_neighbors_committable() {
    for seq in [0, i64::MAX as u64 + 1] {
        let fx = fixture();
        let producer = fx.producer();
        let mut bad = fx.batch("p", "bad-sequence", 1..=2).await;
        bad.events[1].seq = seq;
        let good = fx.batch("p", "good-sequence", 1..=2).await;
        let stall = fx.stall().await;
        let bad = producer.submit(bad).unwrap();
        let good = producer.submit(good).unwrap();
        stall.end().await;
        let failure = answered(bad).await.unwrap_err();
        assert!(
            matches!(failure.error, StoreError::InvalidEvents(_)),
            "{failure:?}"
        );
        assert_eq!(
            (
                failure.committed,
                failure.not_stored,
                failure.outcome_unknown
            ),
            (counts(0, 0), 2, false)
        );
        assert_eq!(answered(good).await.unwrap(), counts(2, 0));
        assert!(fx.stored("p", "bad-sequence").await.is_empty());
        assert_eq!(fx.stored("p", "good-sequence").await, [event(1), event(2)]);
        assert_eq!(producer.retained(), (0, 0, 0));
        producer.close().await;
    }
}

#[test]
fn encoded_input_stays_charged_when_runtime_cancels_a_running_callback() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fx = fixture();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, blocked) = mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    let first = std::sync::atomic::AtomicBool::new(true);
    let at = Instant::now();
    fx.options.now = Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            let _ = entered.lock().unwrap().take().unwrap().send(());
            blocked.lock().unwrap().recv_timeout(SAFETY).unwrap();
        }
        at
    });
    let (producer, submission, held) = runtime.block_on(async {
        let batch = fx.batch("p", "cancelled-runtime", 1..=1).await;
        let held = accounted_bytes(&batch);
        let producer = fx.producer();
        let submission = producer.submit(batch).unwrap();
        tokio::time::timeout(SAFETY, observed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(producer.retained(), (1, 1, held));
        (producer, submission, held)
    });
    runtime.shutdown_background();
    let failure = futures::FutureExt::now_or_never(submission)
        .expect("runtime cancellation must answer unknown without waiting for the callback")
        .unwrap_err();
    assert!(failure.outcome_unknown);
    assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 1));
    assert_eq!(
        producer.retained(),
        (0, 0, held),
        "the real running callback still owns its encoded input"
    );
    release.send(()).unwrap();
    let deadline = Instant::now() + SAFETY;
    while producer.retained().2 != 0 {
        assert!(
            Instant::now() < deadline,
            "the final callback owner did not refund its budget"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(producer.retained(), (0, 0, 0));
    drop(producer);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fx.storage.close())
        .unwrap();
}

#[test]
fn encoded_input_stays_charged_while_cancelled_reconciliation_still_reads() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, blocked) = mpsc::sync_channel(1);
    let hook = Mutex::new(Some((entered, blocked)));
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = armed.clone();
    let mut once = true;
    let fx = fixture_with(StorageOptions {
        read_connections: 1,
        unbounded_read_admission: true,
        reader_setup: Some(Box::new(move |conn| {
            let (entered, blocked) = hook.lock().unwrap().take().unwrap();
            let mut entered = Some(entered);
            conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                if matches!(
                    ctx.action,
                    AuthAction::Read {
                        table_name: "run_events",
                        ..
                    }
                ) && let Some(entered) = entered.take()
                {
                    let _ = entered.send(());
                    let _ = blocked.recv_timeout(SAFETY);
                }
                Authorization::Allow
            }))
            .unwrap();
        })),
        writer_commit_probe: Some(Box::new(move |tx| {
            if !flag.load(Ordering::SeqCst) || !once {
                return None;
            }
            once = false;
            tx.execute_batch("COMMIT").unwrap();
            Some(Err(sql(10)))
        })),
        ..StorageOptions::default()
    });
    let (producer, submission, held) = runtime.block_on(async {
        let batch = fx.batch("p", "cancelled-reconciliation", 1..=1).await;
        let held = accounted_bytes(&batch);
        let producer = fx.producer();
        armed.store(true, Ordering::SeqCst);
        let submission = producer.submit(batch).unwrap();
        tokio::time::timeout(SAFETY, observed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(producer.retained(), (1, 1, held));
        (producer, submission, held)
    });
    runtime.shutdown_background();
    let failure = futures::FutureExt::now_or_never(submission)
        .expect("the unknown receipt must not wait for the selecting lookup")
        .unwrap_err();
    assert!(failure.outcome_unknown);
    assert_eq!(
        producer.retained(),
        (0, 0, held),
        "the real selecting read still owns its expected encoded bytes"
    );
    release.send(()).unwrap();
    let deadline = Instant::now() + SAFETY;
    while producer.retained().2 != 0 {
        assert!(
            Instant::now() < deadline,
            "the selecting lookup did not release its byte charge"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        fx.reports().is_empty(),
        "cancellation cannot invent a known commit"
    );
    drop(producer);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fx.storage.close())
        .unwrap();
}

/// A real SQLite rollback with its evidence deliberately hidden from the writer.
/// This simulates Unknown reachability; it does not claim SQLite loses its hook.
fn incomplete_callback_options(hide_rollback: bool) -> StorageOptions {
    StorageOptions {
        writer_setup: Some(Box::new(move |connection| {
            if hide_rollback {
                connection.rollback_hook(None::<fn()>).unwrap();
            }
            // Fixture DDL uses its own connection: the production writer's
            // commit hook continues to forbid callback-side commits.
            rusqlite::Connection::open(connection.path().unwrap())
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER fail_event BEFORE INSERT ON run_events
                 BEGIN SELECT RAISE(ROLLBACK, 'incomplete-event-callback'); END;",
                )
                .unwrap();
        })),
        ..Default::default()
    }
}

#[tokio::test]
async fn incomplete_unknown_callback_preserves_its_failure_and_releases_every_charge() {
    let fx = fixture_with(incomplete_callback_options(true));
    let batch = fx.batch("p", "incomplete", 1..=3).await;
    let producer = fx.producer();
    let bytes = accounted_bytes(&batch);
    let submission = producer.submit(batch).unwrap();
    assert_eq!(producer.retained(), (1, 3, bytes));
    let failure = answered(submission).await.unwrap_err();
    assert!(
        failure
            .error
            .to_string()
            .contains("SQLite storage operation failed"),
        "{failure:?}"
    );
    assert_eq!(failure.committed, counts(0, 0));
    assert_eq!(failure.not_stored, 3);
    assert!(!failure.outcome_unknown);
    producer.flush().await;
    assert_eq!(producer.retained(), (0, 0, 0));
    assert!(fx.reports().is_empty(), "no false commit report");
    assert!(fx.stored("p", "incomplete").await.is_empty());
    assert!(fx.storage.failure_state().failure().is_none());
    producer.close().await;
}

#[tokio::test]
async fn incomplete_known_callback_preserves_its_failure_without_a_false_commit() {
    let fx = fixture_with(incomplete_callback_options(false));
    let producer = fx.producer();
    let failure = answered(
        producer
            .submit(fx.batch("p", "known", 1..=3).await)
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        failure
            .error
            .to_string()
            .contains("SQLite storage operation failed")
    );
    assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 3));
    assert!(!failure.outcome_unknown);
    producer.flush().await;
    assert_eq!(producer.retained(), (0, 0, 0));
    assert!(fx.reports().is_empty());
    assert!(fx.stored("p", "known").await.is_empty());
    producer.close().await;
}

#[tokio::test]
async fn successful_empty_and_duplicate_only_callbacks_still_commit() {
    let fx = fixture();
    let batch = fx.batch("p", "duplicates", 1..=3).await;
    let duplicate = batch.clone();
    let empty = fx.empty_batch("p", "empty").await;
    let producer = fx.producer();
    assert_eq!(
        answered(producer.submit(batch).unwrap()).await.unwrap(),
        counts(3, 0)
    );
    assert_eq!(
        answered(producer.submit(duplicate).unwrap()).await.unwrap(),
        counts(0, 3)
    );
    assert_eq!(
        answered(producer.submit(empty).unwrap()).await.unwrap(),
        counts(0, 0)
    );
    producer.flush().await;
    assert_eq!(producer.retained(), (0, 0, 0));
    assert_eq!(fx.reports().len(), 3);
    assert_eq!(fx.reports()[1].accounting.rows, 0);
    assert_eq!(fx.reports()[2].accounting.rows, 0);
    assert_eq!(fx.stored("p", "duplicates").await.len(), 3);
    assert!(fx.stored("p", "empty").await.is_empty());
    producer.close().await;
}

async fn incomplete_callback_ioerr_recovery(hide_rollback: bool, failures: u64) {
    use std::sync::atomic::AtomicBool;
    let armed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU64::new(0));
    let path = Arc::new(Mutex::new(None));
    let mut options = incomplete_callback_options(hide_rollback);
    let setup = options.writer_setup.take().unwrap();
    let database = path.clone();
    options.writer_setup = Some(Box::new(move |connection| {
        *database.lock().unwrap() = Some(connection.path().unwrap().to_owned());
        setup(connection);
    }));
    let active = armed.clone();
    let seen = calls.clone();
    options.write_observation_probe = Some(Arc::new(move || {
        if !active.load(Ordering::SeqCst) {
            return None;
        }
        let call = seen.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            // The trigger's real ROLLBACK already released the writer lock.
            // Remove it so the next attempt has no intervening SQLite error
            // that could legitimately break the consecutive IOERR streak.
            rusqlite::Connection::open(path.lock().unwrap().as_ref().unwrap())
                .unwrap()
                .execute_batch("DROP TRIGGER fail_event")
                .unwrap();
        }
        (call < failures).then(|| crate::sqlite::SqliteError::Sql(sql(10)))
    }));
    let fx = fixture_with(options);
    let batch = fx.batch("p", "ioerr", 1..=3).await;
    let rejected_after_failure = batch.clone();
    let producer = fx.producer();
    armed.store(true, Ordering::SeqCst);
    let answer = answered(producer.submit(batch).unwrap()).await;
    if failures == 1 {
        assert_eq!(answer.unwrap(), counts(3, 0));
        assert_eq!(fx.reports().len(), 1, "no report for the failed callback");
        assert_eq!(fx.reports()[0].attempts, 2);
        assert_eq!(fx.reports()[0].accounting.rows, 3);
        assert_eq!(fx.stored("p", "ioerr").await.len(), 3);
        assert!(
            fx.storage.failure_state().failure().is_none(),
            "one IOERR counted once"
        );
    } else {
        let failure = answer.unwrap_err();
        assert!(failure.error.to_string().contains("storage_unavailable"));
        assert_eq!((failure.committed, failure.not_stored), (counts(0, 0), 3));
        assert!(!failure.outcome_unknown);
        assert!(fx.reports().is_empty(), "neither failed callback committed");
        assert!(fx.stored("p", "ioerr").await.is_empty());
        assert_eq!(
            fx.storage.failure_state().failure().unwrap().causes,
            vec![(
                crate::sqlite::Role::Operation,
                crate::sqlite::FatalCause::ConsecutiveIoErrors
            )]
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "two fresh callbacks, no replay"
    );
    producer.flush().await;
    assert_eq!(producer.retained(), (0, 0, 0));
    if failures == 2 {
        assert!(
            matches!(
                producer.submit(rejected_after_failure),
                Err(Rejected::Closed(..))
            ),
            "the fatal noncommit must close intake immediately"
        );
    }
    producer.close().await;
}

#[tokio::test]
async fn incomplete_unknown_callback_rebuilds_only_after_noncommit_and_accounts_ioerr_once() {
    incomplete_callback_ioerr_recovery(true, 1).await;
}

#[tokio::test]
async fn incomplete_unknown_callback_keeps_the_streak_until_the_second_ioerr_latches() {
    incomplete_callback_ioerr_recovery(true, 2).await;
}

#[tokio::test]
async fn incomplete_known_callback_keeps_the_streak_until_the_second_ioerr_latches() {
    incomplete_callback_ioerr_recovery(false, 2).await;
}
