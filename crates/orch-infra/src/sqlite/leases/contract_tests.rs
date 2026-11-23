//! Contracts of the lease adapter that issue 127 pins by name: the error for a
//! damaged epoch counter (PR156 N1), the lane of each lease submission (N2) and
//! the R128 deadline (N3). Every case runs through the real `Writer`.
use super::adapter::{COMPOSED_RELEASE_LANE, LEASE_LANE, LeaseAdapter, writer_lane};
use super::executor_tests::{ClockAt, exec, executor, executor_with_data, lease_state};
use super::{tests::*, *};
use crate::sqlite::{
    ConnectionRole, Lane, SqliteError, Synchronous, WriteReceipt, WriterHandle, open_connection,
};
use orch_core::store::Lane as PortLane;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(20);

fn adapter<'a, 'd>(
    writer: &'a WriterHandle,
    readers: &'a crate::sqlite::ReadPool<'d>,
    clock: &'a ClockAt,
) -> LeaseAdapter<'a, 'd> {
    LeaseAdapter {
        writer,
        readers,
        caller: owner("p"),
        clock,
    }
}

/// A receipt's answer, bounded: a defect fails the test instead of hanging it.
fn answer<T: Send + 'static>(receipt: WriteReceipt<T>) -> mpsc::Receiver<crate::sqlite::Result<T>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(receipt.wait());
    });
    receiver
}

/// Only whether the writer answered with an error, for a test that takes the
/// typed outcome of each lease operation from the state it reads.
fn answer_unit<T: Send + 'static>(
    receipt: WriteReceipt<T>,
) -> mpsc::Receiver<crate::sqlite::Result<()>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(receipt.wait().map(|_| ()));
    });
    receiver
}

fn bounded<T: Send + 'static>(receipt: WriteReceipt<T>) -> crate::sqlite::Result<T> {
    answer(receipt)
        .recv_timeout(BOUND)
        .expect("the writer did not answer in time")
}

/// Keeps the single writer busy so that later submissions queue behind it. The
/// writer is known to be inside this job before `block` returns, so what is
/// submitted afterwards is chosen by the queue's rules and not by arrival luck.
struct Blocker {
    release: mpsc::Sender<()>,
    receipt: Option<WriteReceipt<()>>,
}

fn block(writer: &WriterHandle) -> Blocker {
    let (started, started_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let receipt = writer
        .submit(Lane::Maintenance, None, move |_| {
            started.send(()).unwrap();
            let _ = release_rx.recv_timeout(BOUND);
            Ok(())
        })
        .unwrap();
    started_rx
        .recv_timeout(BOUND)
        .expect("the writer never started the blocking job");
    Blocker {
        release,
        receipt: Some(receipt),
    }
}

impl Blocker {
    fn open(mut self) {
        let _ = self.release.send(());
        bounded(self.receipt.take().unwrap()).unwrap();
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

fn granted(adapter: &LeaseAdapter<'_, '_>, task: &str, agent: &str, run: &str) -> Lease {
    match bounded(adapter.claim(None, new(task, agent, run)).unwrap())
        .unwrap()
        .unwrap()
    {
        ClaimOutcome::Granted(lease) => lease,
        other => panic!("{other:?}"),
    }
}

fn wait_until(limit: Instant) {
    while Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- N1: damaged epoch counter, distinct from exhaustion -------------------

#[test]
fn a_nonpositive_stored_next_epoch_is_corruption_and_not_exhaustion() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    for stored in [0, -1, i64::MIN] {
        let tx = db.transaction().unwrap();
        tx.execute(
            "INSERT OR REPLACE INTO task_epochs(project_id,task_id,next_epoch) VALUES('p','t',?1)",
            [stored],
        )
        .unwrap();
        let before = tx.total_changes();
        match claim_lease(&tx, &owner("p"), &new("t", "a", "r"), 0) {
            Err(LeaseError::Corrupt(detail)) => {
                assert!(detail.contains(&stored.to_string()), "{detail}");
                assert!(detail.contains("task t"), "{detail}");
            }
            other => panic!("stored {stored}: expected Corrupt, got {other:?}"),
        }
        assert_eq!(tx.total_changes(), before, "a refused claim writes nothing");
        assert!(list_leases(&tx, &owner("p")).unwrap().is_empty());
        assert_eq!(
            tx.query_row("SELECT next_epoch FROM task_epochs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            stored,
            "the damaged value is left for `orch db verify`, not repaired"
        );
        tx.rollback().unwrap();
    }
}

#[test]
fn the_smallest_valid_next_epoch_grants_and_the_largest_is_exhaustion() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO task_epochs(project_id,task_id,next_epoch) VALUES('p','t',1)",
        [],
    )
    .unwrap();
    assert_eq!(grant(&tx, &owner("p"), new("t", "a", "r"), 0).epoch, 1);
    assert_eq!(
        tx.query_row("SELECT next_epoch FROM task_epochs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    release_lease(
        &tx,
        &owner("p"),
        "t",
        &get_lease(&tx, &owner("p"), "t").unwrap().unwrap().lineage,
        1,
    )
    .unwrap();
    tx.execute("UPDATE task_epochs SET next_epoch=?1", [i64::MAX])
        .unwrap();
    assert!(matches!(
        claim_lease(&tx, &owner("p"), &new("t", "a", "r2"), 0),
        Err(LeaseError::Store(StoreError::CounterExhausted))
    ));
}

#[test]
fn a_damaged_epoch_counter_reaches_the_receipt_as_lease_corruption_and_the_writer_continues() {
    executor(|writer, readers| {
        exec(
            writer,
            "INSERT INTO task_epochs(project_id,task_id,next_epoch) VALUES('p','t',0)",
        );
        let adapter = adapter(writer, readers, &ClockAt(0));
        let receipt = bounded(adapter.claim(None, new("t", "a", "r")).unwrap());
        assert!(
            matches!(&receipt, Err(SqliteError::LeaseCorrupt(detail)) if detail.contains("task t")),
            "{receipt:?}"
        );
        // Nothing was granted or counted, and the damaged row is untouched.
        assert_eq!(lease_state(readers), (vec![], 1, 0));
        // Exhaustion of another task is still the typed refusal, not corruption.
        exec(
            writer,
            "INSERT INTO task_epochs(project_id,task_id,next_epoch) VALUES('p','full',9223372036854775807)",
        );
        assert!(matches!(
            bounded(adapter.claim(None, new("full", "a", "r")).unwrap()),
            Ok(Err(StoreError::CounterExhausted))
        ));
        // The writer survived both, and a healthy task still claims.
        assert_eq!(granted(&adapter, "ok", "a", "r2").epoch, 1);
    });
}

// ---- N2: lanes -------------------------------------------------------------

#[test]
fn port_lanes_map_one_to_one_onto_writer_lanes() {
    assert_eq!(writer_lane(PortLane::Claims), Lane::Claims);
    assert_eq!(writer_lane(PortLane::Entities), Lane::Entities);
    assert_eq!(writer_lane(PortLane::Events), Lane::Events);
    assert_eq!(writer_lane(PortLane::Maintenance), Lane::Maintenance);
}

#[test]
fn a_standalone_lease_change_is_a_claims_lane_job_and_a_composed_release_an_entities_job() {
    assert_eq!(LEASE_LANE, Lane::Claims);
    assert_eq!(COMPOSED_RELEASE_LANE, Lane::Entities);
}

/// Runs `lease_op` after an entity job that was queued first, behind a busy
/// writer, and returns what that entity job read when it ran. Lane 1 goes first
/// whatever arrived earlier (R26), so the entity job sees the lease change.
fn entity_read_after_queued_lease_op<T: Send + 'static>(
    writer: &WriterHandle,
    sql: &'static str,
    lease_op: impl FnOnce() -> crate::sqlite::Result<WriteReceipt<T>>,
) -> i64 {
    let blocker = block(writer);
    let probe = writer
        .submit(Lane::Entities, None, move |tx| {
            Ok(tx.query_row(sql, [], |r| r.get::<_, i64>(0))?)
        })
        .unwrap();
    let op = lease_op().unwrap();
    blocker.open();
    bounded(op).unwrap();
    bounded(probe).unwrap()
}

#[test]
fn claim_runs_on_the_claims_lane() {
    executor(|writer, readers| {
        let adapter = adapter(writer, readers, &ClockAt(0));
        let seen = entity_read_after_queued_lease_op(writer, "SELECT count(*) FROM leases", || {
            adapter.claim(None, new("t", "a", "r"))
        });
        assert_eq!(seen, 1, "the claim ran before the older Entities job");
    });
}

#[test]
fn renew_runs_on_the_claims_lane() {
    executor(|writer, readers| {
        let lease = granted(&adapter(writer, readers, &ClockAt(0)), "t", "a", "r");
        let later = ClockAt(500);
        let adapter = adapter(writer, readers, &later);
        let seen =
            entity_read_after_queued_lease_op(writer, "SELECT expires_at FROM leases", || {
                adapter.renew(None, "t".into(), lease.lineage.clone(), lease.epoch)
            });
        assert_eq!(seen, LEASE_MS + 500, "the renewal ran first");
    });
}

#[test]
fn release_runs_on_the_claims_lane() {
    executor(|writer, readers| {
        let adapter = adapter(writer, readers, &ClockAt(0));
        let lease = granted(&adapter, "t", "a", "r");
        let seen = entity_read_after_queued_lease_op(writer, "SELECT count(*) FROM leases", || {
            adapter.release(None, "t".into(), lease.lineage.clone(), lease.epoch)
        });
        assert_eq!(seen, 0, "the release ran first");
    });
}

#[test]
fn a_composed_finish_releases_its_lease_from_an_entities_lane_job() {
    executor(|writer, readers| {
        let adapter = adapter(writer, readers, &ClockAt(0));
        let finishing = granted(&adapter, "finishing", "a1", "r1");
        let blocker = block(writer);
        // The finish is queued first, as the spec's lane table places it: on
        // Entities, with its release inside its own transaction.
        let finish = writer
            .submit(COMPOSED_RELEASE_LANE, None, move |tx| {
                release_lease(
                    tx,
                    &owner("p"),
                    "finishing",
                    &finishing.lineage,
                    finishing.epoch,
                )?;
                Ok(tx.query_row("SELECT count(*) FROM leases", [], |r| r.get::<_, i64>(0))?)
            })
            .unwrap();
        let claim = adapter.claim(None, new("other", "a2", "r2")).unwrap();
        blocker.open();
        assert!(matches!(bounded(claim), Ok(Ok(ClaimOutcome::Granted(_)))));
        assert_eq!(
            bounded(finish).unwrap(),
            1,
            "a claim jumps the queued finish, and the finish then left only that lease"
        );
        assert_eq!(lease_state(readers).0.len(), 1);
    });
}

// ---- N3: deadlines and withdrawal ------------------------------------------

#[test]
fn a_lease_request_that_expires_in_the_queue_is_answered_overloaded_and_never_commits() {
    for operation in ["claim", "renew", "release"] {
        executor(|writer, readers| {
            let adapter = adapter(writer, readers, &ClockAt(0));
            let lease = granted(&adapter, "t", "a", "r");
            let before = lease_state(readers);
            let blocker = block(writer);
            let limit = Instant::now() + Duration::from_millis(60);
            let lineage = lease.lineage.clone();
            let receipt = match operation {
                "claim" => answer_unit(adapter.claim(Some(limit), new("u", "b", "s")).unwrap()),
                "renew" => answer_unit(
                    LeaseAdapter {
                        clock: &ClockAt(9_000),
                        ..adapter
                    }
                    .renew(Some(limit), "t".into(), lineage, lease.epoch)
                    .unwrap(),
                ),
                _ => answer_unit(
                    adapter
                        .release(Some(limit), "t".into(), lineage, lease.epoch)
                        .unwrap(),
                ),
            };
            // Still queued at its deadline; only then may the writer take it.
            wait_until(limit);
            blocker.open();
            assert!(
                matches!(
                    receipt.recv_timeout(BOUND).expect("no answer"),
                    Err(SqliteError::Overloaded)
                ),
                "{operation}"
            );
            // A job accepted later runs after anything the withdrawn one could
            // still do, and the state shows that it did nothing.
            exec(writer, "SELECT 1");
            assert_eq!(lease_state(readers), before, "{operation}");
        });
    }
}

#[test]
fn a_lease_request_without_a_deadline_stays_queued_and_commits() {
    executor(|writer, readers| {
        let adapter = adapter(writer, readers, &ClockAt(0));
        let blocker = block(writer);
        let claim = adapter.claim(None, new("t", "a", "r")).unwrap();
        // Far past any lane's wait budget: the daemon's own work is not withdrawn.
        std::thread::sleep(Duration::from_millis(300));
        blocker.open();
        assert!(matches!(bounded(claim), Ok(Ok(ClaimOutcome::Granted(_)))));
        assert_eq!(lease_state(readers).0.len(), 1);
    });
}

#[test]
fn a_lease_request_refused_at_admission_never_runs_later() {
    executor(|writer, readers| {
        let adapter = adapter(writer, readers, &ClockAt(0));
        let blocker = block(writer);
        // The executor of these tests admits 16 waiting jobs.
        let fillers: Vec<_> = (0..16)
            .map(|_| writer.submit(Lane::Maintenance, None, |_| Ok(())).unwrap())
            .collect();
        let refused = adapter.claim(Some(Instant::now() + BOUND), new("t", "a", "r"));
        assert!(matches!(refused, Err(SqliteError::Overloaded)));
        blocker.open();
        for filler in fillers {
            bounded(filler).unwrap();
        }
        exec(writer, "SELECT 1");
        assert_eq!(lease_state(readers), (vec![], 0, 0));
    });
}

#[test]
fn a_lease_request_whose_begin_waits_past_its_deadline_is_answered_overloaded_and_never_commits() {
    executor_with_data(|writer, readers, data| {
        let adapter = adapter(writer, readers, &ClockAt(0));
        // A second connection holds the write lock, so the worker's BEGIN
        // IMMEDIATE waits inside SQLite after it passed the pre-BEGIN check.
        let holder = open_connection(data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let limit = Instant::now() + Duration::from_millis(600);
        let receipt = answer(adapter.claim(Some(limit), new("t", "a", "r")).unwrap());
        assert!(
            receipt.recv_timeout(Duration::from_millis(150)).is_err(),
            "answered while the write lock was held"
        );
        wait_until(limit + Duration::from_millis(50));
        holder.execute_batch("COMMIT").unwrap();
        assert!(matches!(
            receipt.recv_timeout(BOUND).expect("no answer"),
            Err(SqliteError::Overloaded)
        ));
        exec(writer, "SELECT 1");
        assert_eq!(lease_state(readers), (vec![], 0, 0));
        // The reservation was released: the same claim without a deadline commits.
        assert_eq!(granted(&adapter, "t", "a", "r").epoch, 1);
    });
}
