//! The first production event submission through the real composed
//! `SqliteStorage` and its retained writer (plan "Writer retry policy": WP1, WP5,
//! WP9, WP10; spec R26, R27, R128). Every fault is injected at the writer's own
//! seams (the commit probe, a real second job that holds the writer thread);
//! the producer under test is the production one. No outcome depends on a sleep:
//! the stalls are released by the test, the clock of the 20 ms cut is injected,
//! and the one pause (the write lock held for 300 ms) only widens the window in
//! which a wrongly early commit would show.

use super::super::producer::{
    EventBatch, EventProducer, EventProducerOptions, Expected, Rejected, SubmitFailure,
    TransactionReport, rows_are_durable,
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
/// batches wait in the producer and not in it.
async fn in_flight(producer: &EventProducer) {
    for _ in 0..10_000 {
        if producer.waiting().1 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the producer never took a transaction");
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
    assert!(!reports[1].accounting.over_bound);
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
        producer.submit(batch.clone()).unwrap().await.unwrap(),
        counts(0, 0)
    );
    batch.epoch += 1;
    let failure = producer.submit(batch).unwrap().await.unwrap_err();
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
    let Err(Rejected::Full(returned)) = producer.submit(b) else {
        panic!("the second batch does not fit");
    };
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
    lead.await.unwrap();
    assert_eq!(a.await.unwrap(), counts(3, 0));
    let failure = poison.await.unwrap_err();
    assert!(
        matches!(failure.error, StoreError::Backend(_)),
        "{failure:?}"
    );
    assert_eq!(
        (failure.committed, failure.not_stored),
        (counts(0, 0), 2),
        "the first event rolled back with the transaction, and is the caller's to keep"
    );
    assert_eq!(c.await.unwrap(), counts(2, 0));
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
