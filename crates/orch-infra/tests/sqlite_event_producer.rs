//! The first production event submission through the public API only: what the
//! collector of T16 calls. Faults and the exact transaction shapes are proved in
//! the crate's own tests (`sqlite::events::tests::producer`), which can reach the
//! writer's fault seams; this file proves the path is reachable and exact from
//! outside the crate, on a real private database.
use orch_core::store::events::{AppendResult, SequencedEvent};
use orch_core::store::leases::{ClaimOutcome, NewLease};
use orch_core::store::{Caller, Lane, Principal, Storage, StorageExt};
use orch_domain::{RunEvent, RunEventType};
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    EventBatch, EventProducer, EventProducerOptions, SqliteStorage, StorageOptions,
    TransactionReport,
};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn owner() -> Caller {
    Caller {
        project: "synthetic".into(),
        principal: Principal::Owner {
            identity: "test".into(),
        },
    }
}

fn event(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: RunEvent {
            timestamp: "2026-01-01T00:00:00.000Z".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!({ "seq": seq }),
        },
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    storage: SqliteStorage,
    reports: Arc<Mutex<Vec<TransactionReport>>>,
    producer: EventProducer,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = Arc::new(
            DataDir::acquire(
                &root.path().join("data"),
                &Holder {
                    kind: LockKind::Daemon,
                    pid: std::process::id(),
                    started_at: "synthetic".into(),
                    binary_version: "test".into(),
                    executable: "/synthetic/orch".into(),
                },
            )
            .unwrap(),
        );
        let storage = SqliteStorage::open(data, StorageOptions::default()).unwrap();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reports);
        // A clock that never moves leaves the 1,000 rows as the only cut; the 20 ms
        // cut has its own tests, and a slow build must not change where this ends.
        let frozen = Instant::now();
        let producer = EventProducer::new(
            storage.clone(),
            EventProducerOptions {
                now: Arc::new(move || frozen),
                observer: Some(Arc::new(move |report| {
                    sink.lock().unwrap().push(report.clone());
                })),
                ..EventProducerOptions::default()
            },
        );
        Self {
            _root: root,
            storage,
            reports,
            producer,
        }
    }

    async fn batch(&self, run: &str, seqs: std::ops::RangeInclusive<u64>) -> EventBatch {
        let (task, agent, name) = (format!("t-{run}"), format!("a-{run}"), run.to_owned());
        let (lineage, epoch) = self
            .storage
            .transaction(owner(), Lane::Claims, move |unit| {
                match unit.claim_lease(
                    &NewLease {
                        task,
                        agent,
                        run: name,
                    },
                    0,
                )? {
                    ClaimOutcome::Granted(lease) => Ok((lease.lineage, lease.epoch)),
                    ClaimOutcome::Held(lease) => panic!("held: {lease:?}"),
                }
            })
            .await
            .unwrap();
        EventBatch {
            caller: owner(),
            run: run.into(),
            lineage,
            epoch,
            events: seqs.map(event).collect(),
        }
    }
}

#[tokio::test]
async fn collector_batches_of_several_runs_reach_storage_exactly_once_and_in_order() {
    let fx = Fixture::new();
    let batches = [
        fx.batch("run-a", 1..=300).await,
        fx.batch("run-b", 1..=300).await,
        fx.batch("run-c", 1..=500).await,
    ];
    let submissions: Vec<_> = batches
        .into_iter()
        .map(|batch| fx.producer.submit(batch).unwrap())
        .collect();
    let mut answers = Vec::new();
    for submission in submissions {
        answers.push(submission.await.unwrap());
    }
    let whole = |n| AppendResult {
        stored: n,
        skipped: 0,
    };
    assert_eq!(answers, [whole(300), whole(300), whole(500)]);
    let snapshot = fx.storage.read(owner()).await.unwrap();
    for (run, n) in [("run-a", 300), ("run-b", 300), ("run-c", 500)] {
        let stored = snapshot.read_events(run, 0, 1000).await.unwrap();
        assert_eq!(stored, (1..=n).map(event).collect::<Vec<_>>(), "{run}");
    }
    let reports = fx.reports.lock().unwrap().clone();
    let rows: u64 = reports.iter().map(|r| r.accounting.rows).sum();
    assert_eq!(
        rows, 1100,
        "every event is one row, and none is written twice"
    );
    assert!(reports.iter().all(|r| r.accounting.rows <= 1000));
    fx.producer.flush().await;
}

#[tokio::test]
async fn a_batch_over_the_row_bound_commits_in_order_with_its_tail_in_the_next_transaction() {
    let fx = Fixture::new();
    let batch = fx.batch("run", 1..=1001).await;
    let answer = fx.producer.submit(batch).unwrap().await.unwrap();
    assert_eq!(
        answer,
        AppendResult {
            stored: 1001,
            skipped: 0
        }
    );
    let shape: Vec<_> = fx
        .reports
        .lock()
        .unwrap()
        .iter()
        .map(|r| (r.events, r.accounting.rows))
        .collect();
    assert_eq!(shape, [(1000, 1000), (1, 1)]);
    let snapshot = fx.storage.read(owner()).await.unwrap();
    assert_eq!(
        snapshot.read_events("run", 1000, 10).await.unwrap(),
        vec![event(1001)]
    );
}
