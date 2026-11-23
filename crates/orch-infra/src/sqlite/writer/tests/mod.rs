use super::*;
use crate::sqlite::{ConnectionRole, SqliteError, Synchronous, open_connection, test_data};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Receipt deadline; the outer process deadline also bounds run() and scoped joins.
const BOUND: Duration = Duration::from_secs(10);

// A close-on-unwind guard cannot stop a worker that regresses to ignoring close,
// nor interrupt a stuck callback/commit. Contain each scenario in its own process:
// Rust must join scoped threads on unwind, but the parent can kill and reap that
// entire process. The child creates no subprocesses. Its temp data is under a
// parent-owned directory, so killing it cannot leak fixtures or lock holders.
const SCENARIO_BOUND: Duration = Duration::from_secs(20);
const CHILD_SCENARIO: &str = "ORCH_WRITER_TEST_SCENARIO";

struct ScenarioChild(Child);

impl Drop for ScenarioChild {
    fn drop(&mut self) {
        // Child::kill only targets the child we own. Kill is harmless if it
        // already exited; wait reaps it before the parent removes its fixtures.
        let _ = self.0.kill();
        self.0.wait().expect("reap writer scenario child");
    }
}

fn scenario_output(path: &Path) -> String {
    let mut output = String::new();
    File::open(path)
        .unwrap()
        .take(64 * 1024)
        .read_to_string(&mut output)
        .unwrap();
    output
}

#[track_caller]
fn in_scenario_child(name: &str) -> bool {
    if std::env::var(CHILD_SCENARIO).as_deref() == Ok(name) {
        return true;
    }
    let root = tempfile::tempdir().unwrap();
    // Keep nested libtest summaries out of the outer gate's test inventory.
    // A regular file avoids a pipe filling and blocking the child's failure path.
    let output_path = root.path().join("scenario.log");
    let output = File::create(&output_path).unwrap();
    let mut child = ScenarioChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env(CHILD_SCENARIO, name)
            .env("HOME", root.path())
            .env("TMPDIR", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_DATA_HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                root.path().join("global.yml"),
            )
            .env("ORCH_DATA_HOME", root.path().join("daemons"))
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .expect("spawn writer scenario child"),
    );
    let deadline = Instant::now() + SCENARIO_BOUND;
    loop {
        if let Some(status) = child.0.try_wait().expect("poll writer scenario child") {
            assert!(
                status.success(),
                "{name}: scenario failed: {status}\n{}",
                scenario_output(&output_path)
            );
            return false;
        }
        assert!(
            Instant::now() < deadline,
            "{name}: scenario exceeded {SCENARIO_BOUND:?}; killing stuck worker\n{}",
            scenario_output(&output_path)
        );
        // Poll process liveness only; no correctness oracle depends on this delay.
        std::thread::sleep(Duration::from_millis(10));
    }
}

trait WaitBounded<T> {
    fn wait_bounded(self) -> Result<T>;
}

impl<T> WaitBounded<T> for WriteReceipt<T> {
    fn wait_bounded(self) -> Result<T> {
        match self.response.recv_timeout(BOUND) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(SqliteError::WriterClosed),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("receipt not answered within bound"),
        }
    }
}

/// Closes admission on unwind so a working executor can finish its scoped join.
/// The process deadline covers a regressed worker that ignores closure.
struct CloseOnDrop<'a>(&'a WriterHandle);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn count(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn writes_run_one_at_a_time_on_one_connection() {
    if !in_scenario_child("sqlite::writer::tests::writes_run_one_at_a_time_on_one_connection") {
        return;
    }
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
        let _close = CloseOnDrop(&handle);
        setup.wait_bounded().unwrap();
        let mut jobs = Vec::new();
        for id in 0..16 {
            let handle = handle.clone();
            jobs.push(scope.spawn(move || handle.submit(Lane::Entities, None, move |tx| {
                assert_eq!(tx.transaction_state(None::<&str>)?, rusqlite::TransactionState::Write);
                tx.execute("INSERT INTO sample VALUES(?1)", [id])?;
                // A connection-local table proves the same connection serves every job.
                tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS seen(n INTEGER); INSERT INTO seen VALUES(1)")?;
                Ok(())
            }).unwrap().wait_bounded().unwrap()));
        }
        for job in jobs {
            job.join().unwrap();
        }
        let total = handle
            .submit(Lane::Entities, None, |tx| {
                Ok(tx.query_row("SELECT count(*) FROM temp.seen", [], |r| r.get::<_, i64>(0))?)
            })
            .unwrap()
            .wait_bounded()
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
    if !in_scenario_child(
        "sqlite::writer::tests::body_and_commit_errors_keep_nothing_and_writer_continues",
    ) {
        return;
    }
    let (_temp, data) = test_data();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        handle.submit(Lane::Entities, None, |tx| {
            tx.execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY) STRICT;
                CREATE TABLE child(id INTEGER REFERENCES sample(id) DEFERRABLE INITIALLY DEFERRED) STRICT")?;
            Ok(())
        }).unwrap().wait_bounded().unwrap();
        let body = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(matches!(body, Err(SqliteError::Sql(_))));
        let commit = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(2)", [])?;
                tx.execute("INSERT INTO child VALUES(99)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(matches!(commit, Err(SqliteError::Sql(_))));
        let total = handle
            .submit(Lane::Entities, None, |tx| Ok(count(tx)))
            .unwrap()
            .wait_bounded()
            .unwrap();
        assert_eq!(total, 0);
        handle.close();
        worker.join().unwrap();
    });
}

#[test]
fn claims_precede_queued_work_and_other_lanes_use_deadlines() {
    if !in_scenario_child(
        "sqlite::writer::tests::claims_precede_queued_work_and_other_lanes_use_deadlines",
    ) {
        return;
    }
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
    if !in_scenario_child(
        "sqlite::writer::tests::bounded_queue_and_single_pending_event_transaction",
    ) {
        return;
    }
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
    assert!(matches!(
        first.wait_bounded(),
        Err(SqliteError::WriterClosed)
    ));
    assert!(matches!(
        second.wait_bounded(),
        Err(SqliteError::WriterClosed)
    ));
    assert!(matches!(
        handle.submit(Lane::Claims, None, |_| Ok(())),
        Err(SqliteError::WriterClosed)
    ));
}

#[test]
fn expired_queued_request_never_runs_and_close_drains_accepted_work() {
    if !in_scenario_child(
        "sqlite::writer::tests::expired_queued_request_never_runs_and_close_drains_accepted_work",
    ) {
        return;
    }
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
    assert!(matches!(
        expired.wait_bounded(),
        Err(SqliteError::Overloaded)
    ));
    assert!(received.try_recv().is_err());
    assert_eq!(accepted.wait_bounded().unwrap(), 42);
    assert!(matches!(
        handle.submit(Lane::Claims, None, |_| Ok(())),
        Err(SqliteError::WriterClosed)
    ));
}

#[test]
fn a_panicking_transaction_rolls_back_and_closes_the_worker() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_panicking_transaction_rolls_back_and_closes_the_worker",
    ) {
        return;
    }
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
    assert!(matches!(
        failed.wait_bounded(),
        Err(SqliteError::WriterClosed)
    ));
    assert!(matches!(
        queued.wait_bounded(),
        Err(SqliteError::WriterClosed)
    ));
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 0);
}

#[test]
fn executor_runs_claims_first_and_dropped_receipts_still_commit() {
    if !in_scenario_child(
        "sqlite::writer::tests::executor_runs_claims_first_and_dropped_receipts_still_commit",
    ) {
        return;
    }
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
    if !in_scenario_child(
        "sqlite::writer::tests::a_deadline_that_passes_behind_active_work_withdraws_the_request",
    ) {
        return;
    }
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
        let _close = CloseOnDrop(&handle);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let limit = Instant::now() + Duration::from_millis(10);
        let expired = handle
            .submit::<(), _>(Lane::Claims, Some(limit), |_| {
                panic!("expired mutation ran")
            })
            .unwrap();
        std::thread::sleep(limit.saturating_duration_since(Instant::now()));
        release_tx.send(()).unwrap();
        active.wait_bounded().unwrap();
        assert!(matches!(
            expired.wait_bounded(),
            Err(SqliteError::Overloaded)
        ));
        handle.close();
        worker.join().unwrap();
    });
}

#[test]
fn dropping_the_last_handle_closes_admission_and_drains() {
    if !in_scenario_child(
        "sqlite::writer::tests::dropping_the_last_handle_closes_admission_and_drains",
    ) {
        return;
    }
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
    assert_eq!(receipt.wait_bounded().unwrap(), 17);
}

fn tx_state_is_autocommit(connection: &ConnectionHandle<'_>) -> bool {
    connection.is_autocommit()
}

#[test]
fn a_deadline_that_passes_while_begin_waits_is_rechecked_before_the_body() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_deadline_that_passes_while_begin_waits_is_rechecked_before_the_body",
    ) {
        return;
    }
    let (_temp, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    let limit = Instant::now() + Duration::from_secs(60);
    // First read (before BEGIN): in time. Second (reservation held): expired.
    let reads = std::cell::Cell::new(0);
    let clock = || {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            limit - Duration::from_secs(1)
        } else {
            limit
        }
    };
    let ran = std::cell::Cell::new(false);
    let outcome = execute(&mut connection, Some(limit), clock, |tx| {
        ran.set(true);
        tx.execute("INSERT INTO sample VALUES(1)", [])?;
        Ok(())
    });
    assert!(matches!(outcome, Err(SqliteError::Overloaded)));
    assert_eq!(reads.get(), 2, "deadline read before and after BEGIN");
    assert!(
        !ran.get(),
        "the body must not run after the recheck refuses it"
    );
    assert!(tx_state_is_autocommit(&connection), "reservation released");
    assert_eq!(count(&connection), 0);
    // Without a deadline the same path commits, so the refusal came from the recheck.
    execute(&mut connection, None, Instant::now, |tx| {
        tx.execute("INSERT INTO sample VALUES(2)", [])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(count(&connection), 1);
}

#[test]
fn a_withdrawn_request_is_never_committed() {
    if !in_scenario_child("sqlite::writer::tests::a_withdrawn_request_is_never_committed") {
        return;
    }
    // A second connection holds the write lock, so the worker's BEGIN IMMEDIATE
    // waits inside SQLite. The deadline passes during that wait; the request
    // must be refused and must not commit once the lock frees.
    let (_temp, data) = test_data();
    let holder = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    holder
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let limit = Instant::now() + Duration::from_millis(600);
    let (ran_tx, ran_rx) = mpsc::channel();
    let receipt = handle
        .submit(Lane::Claims, Some(limit), move |tx| {
            ran_tx.send(()).unwrap();
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            Ok(())
        })
        .unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        // Still blocked in BEGIN: it passed the pre-BEGIN check and has no answer.
        assert!(
            receipt
                .response
                .recv_timeout(Duration::from_millis(150))
                .is_err(),
            "request answered while the write lock was held"
        );
        std::thread::sleep(
            limit.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
        );
        holder.execute_batch("COMMIT").unwrap();
        assert!(matches!(
            receipt.wait_bounded(),
            Err(SqliteError::Overloaded)
        ));
        handle.close();
        worker.join().unwrap();
    });
    assert!(ran_rx.try_recv().is_err(), "expired body ran");
    assert_eq!(count(&holder), 0);
    // The writer's reservation was released: a later request commits.
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let later = handle.submit(Lane::Claims, None, |tx| {
        tx.execute("INSERT INTO sample VALUES(2)", [])?;
        Ok(())
    });
    handle.close();
    writer.run();
    later.unwrap().wait_bounded().unwrap();
    assert_eq!(count(&holder), 1);
}
