//! The async waits of the event adapter (issue 127 item 4, spec R128). The
//! stall is a synthetic job that holds the one writer thread until the test
//! releases it; no test depends on a sleep for its outcome. The only clock is
//! the request deadline, and every assertion is made after it has passed.

use super::super::adapter::{EventAdapter, Handover};
use super::*;
use std::sync::{Arc, mpsc};

const SHORT: Duration = Duration::from_millis(100);
/// A stall nobody released ends here, so a regression fails instead of hanging.
const SAFETY: Duration = Duration::from_secs(10);

/// A job on the writer that reports it started and then holds the writer until
/// `release` is sent. The receipt answers whether it was released in time.
struct Stall {
    release: mpsc::Sender<()>,
    receipt: crate::sqlite::WriteReceipt<bool>,
}

fn stall(writer: &WriterHandle) -> Stall {
    let (started, hold) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let receipt = writer
        .submit(Lane::Maintenance, None, move |_| {
            started.send(()).unwrap();
            Ok(released.recv_timeout(SAFETY).is_ok())
        })
        .unwrap();
    hold.recv_timeout(SAFETY).unwrap();
    Stall { release, receipt }
}

impl Stall {
    /// Lets the writer go and waits for it, so that whatever it had queued has run.
    fn end(self) {
        self.release.send(()).unwrap();
        assert!(self.receipt.wait().unwrap(), "released by the test");
        // A later lane's job runs after every earlier one, including a withdrawn request.
    }
}

/// A runtime must be dropped only after the writer is let go: dropping it waits
/// for the blocking thread that still holds a withdrawn request's receipt.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

fn adapter<'a>(
    writer: &'a WriterHandle,
    pool: &'a ReadPool<'a>,
    blobs: &'a Arc<DiskBlobStore>,
) -> EventAdapter<'a, 'a> {
    EventAdapter {
        writer,
        readers: pool,
        caller: owner("p"),
        blobs,
    }
}

/// Runs a no-op on the writer after everything already queued.
fn drained(writer: &WriterHandle) {
    submit(writer, Lane::Maintenance, |_| Ok(())).unwrap();
}

fn overloaded() -> String {
    StoreError::Backend(orch_core::Error::other(format!(
        "SQLite events: {}",
        SqliteError::Overloaded
    )))
    .to_string()
}

#[test]
fn a_stalled_writer_does_not_block_the_async_executor() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let p = adapter(w, &pool, &blobs);
        let rt = runtime();
        let stalled = stall(w);
        let release = stalled.release.clone();
        let lineage = fx.lineage.clone();
        // One executor thread. The ticker can only run while `write_async` is
        // awaiting, so its release of the writer proves the thread was free.
        let stored = rt.block_on(async {
            let ticker = tokio::spawn(async move {
                for _ in 0..3 {
                    tokio::task::yield_now().await;
                }
                release.send(()).unwrap();
            });
            let stored = p
                .write_async(Instant::now() + SAFETY, move |unit| {
                    unit.append("run", &lineage, 2, &[event(1)])
                })
                .await
                .unwrap();
            ticker.await.unwrap();
            stored
        });
        assert_eq!(stored.stored, 1);
        assert!(
            stalled.receipt.wait().unwrap(),
            "released by the executor's ticker"
        );
        assert_eq!(all(&pool, "p", "run"), vec![event(1)]);
    });
}

#[test]
fn a_write_that_times_out_in_the_queue_is_answered_overloaded_and_never_commits() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let p = adapter(w, &pool, &blobs);
        let rt = runtime();
        let stalled = stall(w);
        let lineage = fx.lineage.clone();
        let started = Instant::now();
        let error = rt
            .block_on(p.write_async(started + SHORT, move |unit| {
                unit.append("run", &lineage, 2, &[event(1)])
            }))
            .unwrap_err();
        assert_eq!(error.to_string(), overloaded());
        assert!(started.elapsed() < SAFETY, "answered by its own deadline");
        stalled.end();
        drained(w);
        assert!(
            all(&pool, "p", "run").is_empty(),
            "the withdrawn request committed"
        );
    });
}

#[test]
fn a_prune_that_times_out_is_withdrawn_and_deletes_nothing_later() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let p = adapter(w, &pool, &blobs);
        let lineage = fx.lineage.clone();
        p.write(None, move |unit| {
            unit.append("run", &lineage, 2, &[event(1), event(2)])
        })
        .unwrap();
        let rt = runtime();
        let stalled = stall(w);
        let error = rt
            .block_on(p.prune_async("run".into(), 100, Instant::now() + SHORT))
            .unwrap_err();
        assert_eq!(error.to_string(), overloaded());
        stalled.end();
        drained(w);
        assert_eq!(all(&pool, "p", "run"), vec![event(1), event(2)]);
        // The same call with room to run is the pruning itself.
        let pruned = rt
            .block_on(p.prune_async("run".into(), 100, Instant::now() + SAFETY))
            .unwrap();
        assert_eq!((pruned.deleted, pruned.complete), (2, true));
    });
}

#[test]
fn a_request_stuck_behind_another_connections_write_lock_cannot_commit_after_its_timeout() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let p = adapter(w, &pool, &blobs);
        // The writer reaches BEGIN IMMEDIATE and waits on this lock.
        let rt = runtime();
        let rival = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        rival.execute_batch("BEGIN IMMEDIATE").unwrap();
        let lineage = fx.lineage.clone();
        let error = rt
            .block_on(p.write_async(Instant::now() + SHORT, move |unit| {
                unit.append("run", &lineage, 2, &[event(1)])
            }))
            .unwrap_err();
        assert_eq!(error.to_string(), overloaded());
        rival.execute_batch("ROLLBACK").unwrap();
        drained(w);
        assert!(
            all(&pool, "p", "run").is_empty(),
            "committed after the timeout"
        );
    });
}

#[test]
fn a_deadline_that_passes_inside_a_running_transaction_reports_an_unknown_outcome() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let p = adapter(w, &pool, &blobs);
        let rt = runtime();
        let (started, running) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let lineage = fx.lineage.clone();
        let error = rt
            .block_on(p.write_async(Instant::now() + SHORT, move |unit| {
                started.send(()).unwrap();
                let _ = released.recv_timeout(SAFETY);
                unit.append("run", &lineage, 2, &[event(1)])
            }))
            .unwrap_err();
        running.recv_timeout(SAFETY).unwrap();
        assert!(error.to_string().contains("outcome is unknown"), "{error}");
        // Not withdrawn: the body had started, so it finishes and commits.
        release.send(()).unwrap();
        drained(w);
        assert_eq!(all(&pool, "p", "run"), vec![event(1)]);
    });
}

#[test]
fn a_second_waiting_event_transaction_is_refused_at_once_not_awaited() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let p = adapter(w, &pool, &blobs);
        let rt = runtime();
        let stalled = stall(w);
        let lineage = fx.lineage.clone();
        // One event transaction may wait (R26); the second is refused with no wait.
        let waiting = w.submit(Lane::Events, None, |_| Ok(())).unwrap();
        let error = rt
            .block_on(p.write_async(Instant::now() + SAFETY, move |unit| {
                unit.append("run", &lineage, 2, &[event(1)])
            }))
            .unwrap_err();
        assert_eq!(error.to_string(), overloaded());
        stalled.end();
        waiting.wait().unwrap();
    });
}

#[test]
fn the_worker_and_the_waiter_cannot_both_take_a_request() {
    let withdrawn = Handover::new();
    assert!(withdrawn.withdraw());
    assert!(!withdrawn.start(), "a withdrawn body must not run");
    let started = Handover::new();
    assert!(started.start());
    assert!(!started.withdraw(), "a started body cannot be withdrawn");
    assert!(!started.start(), "a body starts once");
}
