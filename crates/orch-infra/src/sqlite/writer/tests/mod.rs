use super::*;
use crate::sqlite::{ConnectionRole, SqliteError, Synchronous, open_connection, test_data};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn count(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn writes_run_one_at_a_time_on_one_connection() {
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 32).unwrap();
    let setup = handle
        .submit(Lane::Entities, None, |tx| {
            tx.execute_batch("CREATE TABLE sample(id INTEGER) STRICT")?;
            Ok(())
        })
        .unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        setup.wait().unwrap();
        let mut jobs = Vec::new();
        for id in 0..16 {
            let handle = handle.clone();
            jobs.push(scope.spawn(move || handle.submit(Lane::Entities, None, move |tx| {
                assert_eq!(tx.transaction_state(None::<&str>)?, rusqlite::TransactionState::Write);
                tx.execute("INSERT INTO sample VALUES(?1)", [id])?;
                // A connection-local table proves the same connection serves every job.
                tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS seen(n INTEGER); INSERT INTO seen VALUES(1)")?;
                Ok(())
            }).unwrap().wait().unwrap()));
        }
        for job in jobs {
            job.join().unwrap();
        }
        let total = handle
            .submit(Lane::Entities, None, |tx| {
                Ok(tx.query_row("SELECT count(*) FROM temp.seen", [], |r| r.get::<_, i64>(0))?)
            })
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(total, 16);
        handle.close();
        worker.join().unwrap();
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 16);
}

#[test]
fn body_and_commit_errors_keep_nothing_and_writer_continues() {
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        handle.submit(Lane::Entities, None, |tx| {
            tx.execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY) STRICT;
                CREATE TABLE child(id INTEGER REFERENCES sample(id) DEFERRABLE INITIALLY DEFERRED) STRICT")?;
            Ok(())
        }).unwrap().wait().unwrap();
        let body = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                Ok(())
            })
            .unwrap()
            .wait();
        assert!(matches!(body, Err(SqliteError::Sql(_))));
        let commit = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(2)", [])?;
                tx.execute("INSERT INTO child VALUES(99)", [])?;
                Ok(())
            })
            .unwrap()
            .wait();
        assert!(matches!(commit, Err(SqliteError::Sql(_))));
        let total = handle
            .submit(Lane::Entities, None, |tx| Ok(count(tx)))
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(total, 0);
        handle.close();
        worker.join().unwrap();
    });
}

#[test]
fn claims_precede_queued_work_and_other_lanes_use_deadlines() {
    // A manually advanced arrival clock tests ordering, never a latency budget.
    let start = Instant::now();
    let mut queue = Schedule::new(8);
    queue.push(Lane::Maintenance, start, 1).unwrap(); // deadline +5s
    queue
        .push(Lane::Events, start + Duration::from_secs(4), 2)
        .unwrap(); // same +5s, FIFO
    queue
        .push(Lane::Entities, start + Duration::from_secs(4), 3)
        .unwrap(); // +4.2s
    queue
        .push(Lane::Claims, start + Duration::from_secs(9), 4)
        .unwrap(); // still first
    assert_eq!(queue.pop(), Some(4));
    assert_eq!(queue.pop(), Some(3));
    assert_eq!(queue.pop(), Some(1));
    assert_eq!(queue.pop(), Some(2));
    assert_eq!(queue.pop(), None);
}

#[test]
fn bounded_queue_and_single_pending_event_transaction() {
    let (_temp, data) = test_data();
    assert!(Writer::open(&data, Synchronous::Normal, 0).is_err());
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 2).unwrap();
    let first = handle.submit(Lane::Events, None, |_| Ok(())).unwrap();
    assert!(matches!(
        handle.submit(Lane::Events, None, |_| Ok(())),
        Err(SqliteError::Overloaded)
    ));
    let second = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    assert!(matches!(
        handle.submit(Lane::Claims, None, |_| Ok(())),
        Err(SqliteError::Overloaded)
    ));
    drop(writer);
    assert!(matches!(first.wait(), Err(SqliteError::WriterClosed)));
    assert!(matches!(second.wait(), Err(SqliteError::WriterClosed)));
    assert!(matches!(
        handle.submit(Lane::Claims, None, |_| Ok(())),
        Err(SqliteError::WriterClosed)
    ));
}

#[test]
fn expired_queued_request_never_runs_and_close_drains_accepted_work() {
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let (sent, received) = mpsc::channel();
    let expired = handle
        .submit(Lane::Claims, Some(Instant::now()), move |_| {
            sent.send(()).unwrap();
            Ok(())
        })
        .unwrap();
    let accepted = handle.submit(Lane::Entities, None, |_| Ok(42)).unwrap();
    handle.close();
    writer.run();
    assert!(matches!(expired.wait(), Err(SqliteError::Overloaded)));
    assert!(received.try_recv().is_err());
    assert_eq!(accepted.wait().unwrap(), 42);
    assert!(matches!(
        handle.submit(Lane::Claims, None, |_| Ok(())),
        Err(SqliteError::WriterClosed)
    ));
}

#[test]
fn a_panicking_transaction_rolls_back_and_closes_the_worker() {
    let (_temp, data) = test_data();
    let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    drop(conn);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let failed = handle
        .submit::<(), _>(Lane::Entities, None, |tx| {
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            panic!("synthetic transaction panic")
        })
        .unwrap();
    let queued = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.run()));
    assert!(result.is_err());
    assert!(matches!(failed.wait(), Err(SqliteError::WriterClosed)));
    assert!(matches!(queued.wait(), Err(SqliteError::WriterClosed)));
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 0);
}

#[test]
fn executor_runs_claims_first_and_dropped_receipts_still_commit() {
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let (sent, received) = mpsc::channel();
    for (lane, id) in [
        (Lane::Maintenance, 4),
        (Lane::Events, 3),
        (Lane::Entities, 2),
        (Lane::Claims, 1),
    ] {
        let sent = sent.clone();
        drop(
            handle
                .submit(lane, None, move |_| {
                    sent.send(id).unwrap();
                    Ok(())
                })
                .unwrap(),
        );
    }
    handle.close();
    writer.run();
    assert_eq!(received.try_iter().collect::<Vec<_>>(), [1, 2, 3, 4]);
}

#[test]
fn a_deadline_that_passes_behind_active_work_withdraws_the_request() {
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let active = handle
        .submit(Lane::Entities, None, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
        .unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let limit = Instant::now() + Duration::from_millis(10);
        let expired = handle
            .submit::<(), _>(Lane::Claims, Some(limit), |_| {
                panic!("expired mutation ran")
            })
            .unwrap();
        std::thread::sleep(limit.saturating_duration_since(Instant::now()));
        release_tx.send(()).unwrap();
        active.wait().unwrap();
        assert!(matches!(expired.wait(), Err(SqliteError::Overloaded)));
        handle.close();
        worker.join().unwrap();
    });
}

#[test]
fn dropping_the_last_handle_closes_admission_and_drains() {
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let clone = handle.clone();
    let receipt = handle.submit(Lane::Entities, None, |_| Ok(17)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            writer.run();
            done_tx.send(()).unwrap();
        });
        drop(handle);
        assert!(
            done_rx.try_recv().is_err(),
            "another handle still owns admission"
        );
        // Keep an independent emergency close so even a regression fails rather
        // than hanging the scoped join forever. It is not a client handle.
        let shared = Arc::clone(&clone.admission.shared);
        drop(clone);
        let returned = done_rx.recv_timeout(Duration::from_secs(2));
        if returned.is_err() {
            shared.state.lock().unwrap().accepting = false;
            shared.available.notify_all();
        }
        worker.join().unwrap();
        assert!(
            returned.is_ok(),
            "worker did not stop after the last handle"
        );
    });
    assert_eq!(receipt.wait().unwrap(), 17);
}
