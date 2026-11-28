use super::*;
use crate::sqlite::{ConnectionRole, SqliteError, Synchronous, open_connection, test_data};
use failure::{Failure, Role};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
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
// parent-owned directory, removed after reaping on ordinary exit/unwind. If the
// parent itself dies, the child and fixtures can survive; this is not a
// parent-death/process-group or uninterruptible-I/O cleanup guarantee.
const SCENARIO_BOUND: Duration = Duration::from_secs(20);
const CHILD_SCENARIO: &str = "ORCH_WRITER_TEST_SCENARIO";
const SCENARIO_ENTRY: &str = "ORCH_WRITER_TEST_ENTRY";
const OUTPUT_TAIL_BYTES: u64 = 64 * 1024;

struct ScenarioChild(Child);

impl Drop for ScenarioChild {
    fn drop(&mut self) {
        // Child::kill only targets the child we own. Kill is harmless if it
        // already exited; wait reaps it before the parent removes its fixtures.
        let _ = self.0.kill();
        check_reap(self.0.wait().map(|_| ()));
    }
}

fn check_reap(result: std::io::Result<()>) {
    if let Err(error) = result {
        // Preserve the original panic instead of aborting on a second panic.
        if !std::thread::panicking() {
            panic!("reap writer scenario child: {error}");
        }
    }
}

fn scenario_output(path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let start = end.saturating_sub(OUTPUT_TAIL_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut output = Vec::new();
        file.take(end - start).read_to_end(&mut output)?;
        Ok(output)
    };
    match read() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => format!("<scenario output unavailable: {error}>"),
    }
}

fn scenario_ran_once(name: &str, entry: &[u8], output: &str) -> bool {
    entry == format!("{name}\n").as_bytes()
        && output
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;"))
}

#[track_caller]
fn in_scenario_child(name: &str) -> bool {
    if std::env::var(CHILD_SCENARIO).as_deref() == Ok(name) {
        let path = std::env::var_os(SCENARIO_ENTRY).expect("parent entry path");
        let mut entry = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(entry, "{name}").unwrap();
        return true;
    }
    run_scenario_child(name).unwrap_or_else(|error| panic!("{error}"));
    false
}

fn run_scenario_child(name: &str) -> std::result::Result<(), String> {
    let root = tempfile::tempdir().unwrap();
    // Keep nested libtest summaries out of the outer gate's test inventory.
    // A regular file avoids a pipe filling and blocking the child's failure path.
    let entry_path = root.path().join("entry");
    let output_path = root.path().join("scenario.log");
    // The diagnostic read is byte-bounded; the retained file can still grow
    // throughout SCENARIO_BOUND. This is not a disk-usage quota.
    let output = File::create(&output_path).unwrap();
    let mut child = ScenarioChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env(CHILD_SCENARIO, name)
            .env(SCENARIO_ENTRY, &entry_path)
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
            let output = scenario_output(&output_path);
            let entry = std::fs::read(&entry_path).unwrap_or_default();
            return if status.success() && scenario_ran_once(name, &entry, &output) {
                Ok(())
            } else {
                Err(format!(
                    "{name}: expected exactly one named scenario; status {status}\n{output}"
                ))
            };
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{name}: scenario exceeded {SCENARIO_BOUND:?}; killing stuck worker\n{}",
                scenario_output(&output_path)
            ));
        }
        // Poll process liveness only; no correctness oracle depends on this delay.
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn scenario_parent_accepts_one_correct_child() {
    run_scenario_child("sqlite::writer::tests::writes_run_one_at_a_time_on_one_connection")
        .unwrap();
}

#[test]
fn scenario_parent_rejects_a_stale_filter_even_when_libtest_succeeds() {
    assert!(run_scenario_child("sqlite::writer::tests::nonexistent_stale_scenario").is_err());
}

#[test]
fn scenario_proof_rejects_missing_duplicate_and_wrong_entries() {
    let summary = "test result: ok. 1 passed; 0 failed; 0 ignored;";
    for entry in [b"".as_slice(), b"expected\nexpected\n", b"other\n"] {
        assert!(!scenario_ran_once("expected", entry, summary));
    }
}

#[test]
fn scenario_proof_rejects_zero_or_multiple_tests() {
    for summary in [
        "test result: ok. 0 passed; 0 failed;",
        "test result: ok. 2 passed; 0 failed;",
    ] {
        assert!(!scenario_ran_once("expected", b"expected\n", summary));
    }
}

#[test]
fn scenario_diagnostics_keep_the_byte_tail_and_decode_a_split_character_lossily() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    let mut bytes = vec![b'x'; OUTPUT_TAIL_BYTES as usize];
    bytes.extend_from_slice("€".as_bytes());
    bytes.extend(std::iter::repeat_n(b'y', OUTPUT_TAIL_BYTES as usize - 2));
    std::fs::write(&path, bytes).unwrap();
    let output = scenario_output(&path);
    assert_eq!(
        output,
        format!("��{}", "y".repeat(OUTPUT_TAIL_BYTES as usize - 2))
    );
}

#[test]
fn scenario_diagnostics_handle_empty_short_and_invalid_utf8_output() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    for bytes in [b"".as_slice(), b"short", b"failure: \xff"] {
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(scenario_output(&path), String::from_utf8_lossy(bytes));
    }
}

#[test]
fn scenario_diagnostic_tail_does_not_cap_the_retained_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    std::fs::write(&path, vec![b'a'; OUTPUT_TAIL_BYTES as usize + 1]).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"later failure").unwrap();
    assert!(scenario_output(&path).ends_with("later failure"));
    assert!(file.metadata().unwrap().len() > OUTPUT_TAIL_BYTES);
}

#[test]
fn scenario_reap_error_during_unwind_preserves_the_original_panic() {
    if !in_scenario_child(
        "sqlite::writer::tests::scenario_reap_error_during_unwind_preserves_the_original_panic",
    ) {
        return;
    }
    struct FailedReap;
    impl Drop for FailedReap {
        fn drop(&mut self) {
            check_reap(Err(std::io::Error::other("synthetic reap failure")));
        }
    }
    let panic = std::panic::catch_unwind(|| {
        let _reap = FailedReap;
        panic!("original scenario failure");
    })
    .unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original scenario failure")
    );
}

#[test]
fn scenario_reap_error_outside_unwind_is_reported() {
    assert!(
        std::panic::catch_unwind(|| {
            check_reap(Err(std::io::Error::other("synthetic reap failure")));
        })
        .is_err()
    );
}

trait WaitBounded<T> {
    fn wait_bounded(self) -> Result<T>;
}

impl<T> WaitBounded<T> for WriteReceipt<T> {
    fn wait_bounded(self) -> Result<T> {
        match self.response.recv_timeout(BOUND) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.dropped()),
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
    // A second connection probes the physical write reservation without waiting.
    // Two pre-BEGIN reads would both acquire it and leave the deadline unexpired.
    let observer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    observer.busy_timeout(Duration::ZERO).unwrap();
    let reads = std::cell::Cell::new(0);
    let clock = || {
        reads.set(reads.get() + 1);
        match observer.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => {
                observer.execute_batch("ROLLBACK").unwrap();
                limit - Duration::from_secs(1)
            }
            Err(error) => {
                assert_eq!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy)
                );
                limit
            }
        }
    };
    let ran = std::cell::Cell::new(false);
    let gate = Gate::install(&mut connection).unwrap();
    let state = FailureState::new();
    let outcome = execute(&mut connection, &gate, &state, Some(limit), clock, |tx| {
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
    assert_eq!(state.failure(), None, "a clean rollback is no failure");
    assert_eq!(count(&connection), 0);
    // Without a deadline the same path commits, so the refusal came from the recheck.
    execute(&mut connection, &gate, &state, None, Instant::now, |tx| {
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
    // 600 ms is scheduling allowance, not proof of entry into BEGIN. SQLite's
    // configured 5 s busy timeout is a safety net, longer than this deadline.
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
        // No early answer while contended; the worker may still be unscheduled.
        // The deterministic reservation probe above proves post-BEGIN ordering.
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

// ---- Atomic failure (issue 127, F1/F2 of the writer-recovery review) ----
//
// SQLite can end a transaction by itself (RAISE(ROLLBACK), SQLITE_FULL, IOERR,
// NOMEM, INTERRUPT) while the callback is running. The synthetic trigger below
// is the deterministic stand-in: it ends the whole transaction exactly as those
// results do, and the callback then carries on, as a caller that swallowed the
// error would.

const LOSE_THE_TRANSACTION: &str = "CREATE TABLE sample(id INTEGER) STRICT;
    CREATE TABLE scratch(x INTEGER) STRICT;
    CREATE TRIGGER scratch_rollback BEFORE INSERT ON scratch
    BEGIN SELECT RAISE(ROLLBACK, 'synthetic SQLite rollback'); END;";

/// A data directory whose database already holds `schema`, no writer yet.
fn data_with(schema: &str) -> (tempfile::TempDir, crate::data_dir::DataDir) {
    let (temp, data) = test_data();
    let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch(schema).unwrap();
    drop(conn);
    (temp, data)
}

/// Make SQLite end the open transaction, as a full disk or an I/O error does.
fn lose_the_transaction(tx: &rusqlite::Connection) {
    assert!(tx.execute("INSERT INTO scratch VALUES(1)", []).is_err());
    assert!(tx.is_autocommit(), "SQLite ended the transaction itself");
}

#[test]
fn a_sqlite_rollback_in_the_body_leaves_no_later_write_durable() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_sqlite_rollback_in_the_body_leaves_no_later_write_durable",
    ) {
        return;
    }
    let (_temp, data) = data_with(LOSE_THE_TRANSACTION);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        let lost = handle
            .submit(Lane::Claims, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                lose_the_transaction(tx);
                // The caller swallows the failure and writes on: each statement
                // now runs alone, and an unguarded one would commit by itself.
                let _ = tx.execute("INSERT INTO sample VALUES(2)", []);
                let _ = tx
                    .execute_batch("SAVEPOINT later; INSERT INTO sample VALUES(3); RELEASE later");
                let _ = tx.execute("INSERT INTO sample VALUES(4)", []);
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(
            matches!(lost, Err(SqliteError::TransactionAborted)),
            "{lost:?}"
        );
        let total = handle
            .submit(Lane::Entities, None, |tx| Ok(count(tx)))
            .unwrap()
            .wait_bounded()
            .unwrap();
        assert_eq!(total, 0, "nothing of the lost transaction is durable");
        // The failure was the request's, not the writer's: the next one commits.
        handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(5)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded()
            .unwrap();
        handle.close();
        worker.join().unwrap();
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 1);
}

#[test]
fn a_callback_cannot_commit_or_restart_the_transaction_behind_the_executor() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_callback_cannot_commit_or_restart_the_transaction_behind_the_executor",
    ) {
        return;
    }
    let (_temp, data) = data_with(LOSE_THE_TRANSACTION);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        // A manual COMMIT reports its refusal to the callback, which returns it.
        let committed = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                tx.execute_batch("COMMIT")?;
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(
            matches!(committed, Err(SqliteError::Sql(_))),
            "{committed:?}"
        );
        // A callback that swallows the refusal and opens a transaction of its own
        // must not have that one committed by the executor in the request's name.
        let restarted = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(2)", [])?;
                let _ = tx.execute_batch("COMMIT");
                tx.execute_batch("BEGIN IMMEDIATE")?;
                tx.execute("INSERT INTO sample VALUES(3)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(
            matches!(restarted, Err(SqliteError::TransactionAborted)),
            "{restarted:?}"
        );
        // The same after SQLite itself, or an explicit ROLLBACK, ended it.
        for end in ["ROLLBACK", "lose"] {
            let outcome = handle
                .submit(Lane::Entities, None, move |tx| {
                    tx.execute("INSERT INTO sample VALUES(4)", [])?;
                    if end == "lose" {
                        lose_the_transaction(tx);
                    } else {
                        tx.execute_batch(end)?;
                    }
                    tx.execute_batch("BEGIN IMMEDIATE")?;
                    tx.execute("INSERT INTO sample VALUES(5)", [])?;
                    Ok(())
                })
                .unwrap()
                .wait_bounded();
            assert!(
                matches!(outcome, Err(SqliteError::TransactionAborted)),
                "{end}: {outcome:?}"
            );
        }
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
fn savepoints_and_their_rollbacks_inside_a_request_still_commit() {
    if !in_scenario_child(
        "sqlite::writer::tests::savepoints_and_their_rollbacks_inside_a_request_still_commit",
    ) {
        return;
    }
    // Adapter operations (a lease claim) wrap their writes in a savepoint and
    // roll it back on their own failure. That is not the transaction's end.
    let (_temp, data) = data_with(LOSE_THE_TRANSACTION);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        handle
            .submit(Lane::Claims, None, |tx| {
                tx.execute_batch(
                    "SAVEPOINT kept; INSERT INTO sample VALUES(1); RELEASE kept;
                     SAVEPOINT undone; INSERT INTO sample VALUES(2);
                     ROLLBACK TO undone; RELEASE undone;",
                )?;
                // A statement that fails alone leaves the transaction open.
                assert!(
                    tx.execute("INSERT INTO nowhere VALUES(1)", []).is_err(),
                    "synthetic statement error"
                );
                tx.execute("INSERT INTO sample VALUES(3)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded()
            .unwrap();
        handle.close();
        worker.join().unwrap();
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    let ids: Vec<i64> = reader
        .prepare("SELECT id FROM sample ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ids, [1, 3]);
}

#[test]
fn a_failed_rollback_fails_the_writer_instead_of_wedging_it() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    if !in_scenario_child(
        "sqlite::writer::tests::a_failed_rollback_fails_the_writer_instead_of_wedging_it",
    ) {
        return;
    }
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    // The refusal stands in for a rollback that SQLite cannot complete.
    writer
        .connection
        .for_migration()
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Transaction {
                operation: TransactionOperation::Rollback,
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        let _close = CloseOnDrop(&handle);
        let first = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                Err::<(), _>(SqliteError::Migration("synthetic body error".into()))
            })
            .unwrap()
            .wait_bounded();
        // The request keeps both of its failures (PR 249 classifies each side) ...
        assert!(
            matches!(
                &first,
                Err(SqliteError::OperationRollback { operation, .. })
                    if matches!(**operation, SqliteError::Migration(_))
            ),
            "{first:?}"
        );
        // ... and the writer is refused for everyone, from the shared state (R128),
        // rather than answering "cannot start a transaction within a transaction"
        // for ever, or as a clean close.
        let next = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(2)", [])?;
                Ok(())
            })
            .and_then(|receipt| receipt.wait_bounded());
        assert!(
            matches!(next, Err(SqliteError::StorageUnavailable(_))),
            "{next:?}"
        );
        assert!(
            worker.join().is_err(),
            "the writer fails loudly, not as a clean close"
        );
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 0);
}

/// A writer whose ROLLBACK statement is refused `refusals` times, standing in for a
/// rollback that SQLite cannot complete.
fn refuse_rollbacks(writer: &mut Writer<'_>, refusals: usize) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    use std::sync::atomic::AtomicUsize;
    let left = Arc::new(AtomicUsize::new(refusals));
    writer
        .connection
        .for_migration()
        .authorizer(Some(move |context: AuthContext<'_>| match context.action {
            AuthAction::Transaction {
                operation: TransactionOperation::Rollback,
            } if left
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok() =>
            {
                Authorization::Deny
            }
            _ => Authorization::Allow,
        }))
        .unwrap();
}

fn failing_body(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute("INSERT INTO sample VALUES(1)", [])?;
    Err(SqliteError::Migration("synthetic body error".into()))
}

#[test]
fn a_failed_top_level_rollback_that_leaves_the_transaction_open_fails_the_writer() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_failed_top_level_rollback_that_leaves_the_transaction_open_fails_the_writer",
    ) {
        return;
    }
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    refuse_rollbacks(&mut writer, usize::MAX);
    let (release, released) = mpsc::channel::<()>();
    let (started, running) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        let _close = CloseOnDrop(&handle);
        // The first request holds the worker while three more queue behind it.
        let first = handle
            .submit(Lane::Entities, None, move |tx| {
                started.send(()).unwrap();
                released.recv_timeout(BOUND).expect("released");
                failing_body(tx)
            })
            .unwrap();
        // Claims outrank everything: queue them only once the first request runs.
        running.recv_timeout(BOUND).expect("running");
        let queued: Vec<_> = [Lane::Claims, Lane::Events, Lane::Maintenance]
            .into_iter()
            .map(|lane| {
                handle
                    .submit(lane, None, |tx| {
                        tx.execute("INSERT INTO sample VALUES(9)", [])?;
                        Ok(())
                    })
                    .unwrap()
            })
            .collect();
        release.send(()).unwrap();
        assert!(
            matches!(
                first.wait_bounded(),
                Err(SqliteError::OperationRollback { .. })
            ),
            "the request keeps its own answer"
        );
        // The shared state is latched with the new cause, and reports it from memory.
        let latched = handle.failure_state().failure().expect("latched");
        assert_eq!(latched.source, FailureSource::Writer);
        assert_eq!(
            latched.causes,
            [(Role::Operation, FatalCause::RollbackLeftTransactionOpen)]
        );
        // What was queued is refused as a latched writer refuses, never as a clean
        // close, and no statement of it ran.
        for receipt in queued {
            let outcome = receipt.wait_bounded();
            assert!(
                matches!(&outcome, Err(SqliteError::StorageUnavailable(f)) if *f == latched),
                "{outcome:?}"
            );
        }
        // So is every later request, at the door.
        for lane in [
            Lane::Claims,
            Lane::Entities,
            Lane::Events,
            Lane::Maintenance,
        ] {
            let outcome = handle.submit(lane, None, |_| Ok(())).err();
            assert!(
                matches!(&outcome, Some(SqliteError::StorageUnavailable(f)) if *f == latched),
                "{outcome:?}"
            );
        }
        assert!(
            worker.join().is_err(),
            "the writer fails loudly, not as a clean close"
        );
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 0, "nothing of any request is durable");
}

#[test]
fn a_failed_rollback_is_not_retried_before_the_connection_is_judged() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_failed_rollback_is_not_retried_before_the_connection_is_judged",
    ) {
        return;
    }
    // Only the first ROLLBACK is refused. A retry (rusqlite's `Drop` makes one)
    // would succeed and hide the failure; R128 allows no further statement.
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    refuse_rollbacks(&mut writer, 1);
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        let _close = CloseOnDrop(&handle);
        let first = handle
            .submit(Lane::Entities, None, failing_body)
            .unwrap()
            .wait_bounded();
        assert!(
            matches!(first, Err(SqliteError::OperationRollback { .. })),
            "{first:?}"
        );
        assert_eq!(
            handle.failure_state().failure().map(|f| f.causes),
            Some(vec![(
                Role::Operation,
                FatalCause::RollbackLeftTransactionOpen
            )])
        );
        assert!(worker.join().is_err());
    });
}

#[test]
fn a_rollback_error_that_leaves_the_connection_outside_a_transaction_is_not_that_cause() {
    // SQLite cannot be made to fail a ROLLBACK and end the transaction anyway, so
    // the condition is proved where it is decided: a failed rollback answered with
    // the connection in each state, and a successful one inside a transaction.
    let (_temp, data) = test_data();
    let connection = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let abandoned = |rollback_failed| Abandoned {
        error: SqliteError::Migration("synthetic".into()),
        rollback_failed,
    };
    let state = FailureState::new();
    let error = abandoned(true).settle(&connection, &state);
    assert!(matches!(error, SqliteError::Migration(_)), "{error:?}");
    assert_eq!(
        state.failure(),
        None,
        "a rollback error alone is not the cause"
    );
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    abandoned(false).settle(&connection, &state);
    assert_eq!(
        state.failure(),
        None,
        "a successful rollback is not the cause"
    );
    abandoned(true).settle(&connection, &state);
    assert_eq!(
        state.failure().map(|f| f.causes),
        Some(vec![(
            Role::Operation,
            FatalCause::RollbackLeftTransactionOpen
        )])
    );
    connection.execute_batch("ROLLBACK").unwrap();
}

/// A connection whose COMMIT is refused `commits` times and whose ROLLBACK is refused
/// `rollbacks` times, standing in for a commit and a rollback SQLite cannot complete.
fn refuse_commits_and_rollbacks(writer: &mut Writer<'_>, commits: usize, rollbacks: usize) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    use std::sync::atomic::AtomicUsize;
    let commits = Arc::new(AtomicUsize::new(commits));
    let rollbacks = Arc::new(AtomicUsize::new(rollbacks));
    let take = |left: &AtomicUsize| {
        left.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    };
    writer
        .connection
        .for_migration()
        .authorizer(Some(move |context: AuthContext<'_>| match context.action {
            // rusqlite reports COMMIT as the unknown operation.
            AuthAction::Transaction {
                operation: TransactionOperation::Unknown,
            } if take(&commits) => Authorization::Deny,
            AuthAction::Transaction {
                operation: TransactionOperation::Rollback,
            } if take(&rollbacks) => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
}

#[test]
fn a_failed_commit_whose_rollback_also_fails_fails_the_writer() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_failed_commit_whose_rollback_also_fails_fails_the_writer",
    ) {
        return;
    }
    // The commit is refused, so SQLite leaves the transaction open; rusqlite's
    // implicit rollback (`Drop`) would fail without a trace and the worker would
    // panic with nothing latched. The executor rolls back once, explicitly.
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    refuse_commits_and_rollbacks(&mut writer, usize::MAX, usize::MAX);
    let (release, released) = mpsc::channel::<()>();
    let (started, running) = mpsc::channel::<()>();
    let ran = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        let _close = CloseOnDrop(&handle);
        let first = handle
            .submit(Lane::Entities, None, move |tx| {
                started.send(()).unwrap();
                released.recv_timeout(BOUND).expect("released");
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                Ok(())
            })
            .unwrap();
        running.recv_timeout(BOUND).expect("running");
        let queued: Vec<_> = [Lane::Claims, Lane::Events]
            .into_iter()
            .map(|lane| {
                let ran = Arc::clone(&ran);
                handle
                    .submit(lane, None, move |tx| {
                        ran.store(true, Ordering::SeqCst);
                        tx.execute("INSERT INTO sample VALUES(9)", [])?;
                        Ok(())
                    })
                    .unwrap()
            })
            .collect();
        release.send(()).unwrap();
        // The request keeps both failures: its failed commit and its failed rollback.
        let first = first.wait_bounded();
        assert!(
            matches!(&first, Err(SqliteError::OperationRollback { .. })),
            "{first:?}"
        );
        let latched = handle.failure_state().failure().expect("latched");
        assert_eq!(latched.source, FailureSource::Writer);
        assert_eq!(
            latched.causes,
            [(Role::Operation, FatalCause::RollbackLeftTransactionOpen)]
        );
        for receipt in queued {
            let outcome = receipt.wait_bounded();
            assert!(
                matches!(&outcome, Err(SqliteError::StorageUnavailable(f)) if *f == latched),
                "{outcome:?}"
            );
        }
        assert!(
            !ran.load(Ordering::SeqCst),
            "no queued request runs after the latch"
        );
        let later = handle.submit(Lane::Entities, None, |_| Ok(())).err();
        assert!(
            matches!(&later, Some(SqliteError::StorageUnavailable(f)) if *f == latched),
            "{later:?}"
        );
        assert!(
            worker.join().is_err(),
            "the writer fails loudly, not as a clean close"
        );
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 0, "nothing of any request is durable");
}

#[test]
fn a_failed_commit_is_not_rolled_back_implicitly_before_the_connection_is_judged() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_failed_commit_is_not_rolled_back_implicitly_before_the_connection_is_judged",
    ) {
        return;
    }
    // Only the first ROLLBACK is refused. If rusqlite's `Drop` rolled back after the
    // failed commit it would take that refusal and the explicit rollback would then
    // succeed, hiding the failure from the latch (R128: one rollback, then judge).
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    refuse_commits_and_rollbacks(&mut writer, usize::MAX, 1);
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        let _close = CloseOnDrop(&handle);
        let first = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(
            matches!(first, Err(SqliteError::OperationRollback { .. })),
            "{first:?}"
        );
        assert_eq!(
            handle.failure_state().failure().map(|f| f.causes),
            Some(vec![(
                Role::Operation,
                FatalCause::RollbackLeftTransactionOpen
            )])
        );
        assert!(worker.join().is_err());
    });
}

#[test]
fn a_failed_commit_whose_rollback_succeeds_is_an_ordinary_failure() {
    if !in_scenario_child(
        "sqlite::writer::tests::a_failed_commit_whose_rollback_succeeds_is_an_ordinary_failure",
    ) {
        return;
    }
    // The control: the same refused COMMIT, with a rollback that works. Nothing
    // latches and the writer serves the next request, so only a rollback that
    // leaves the transaction open is the fatal cause.
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    refuse_commits_and_rollbacks(&mut writer, 1, 0);
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || writer.run());
        let _close = CloseOnDrop(&handle);
        let first = handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(1)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(
            first.is_err() && !matches!(first, Err(SqliteError::OperationRollback { .. })),
            "{first:?}"
        );
        assert_eq!(handle.failure_state().failure(), None);
        handle
            .submit(Lane::Entities, None, |tx| {
                tx.execute("INSERT INTO sample VALUES(2)", [])?;
                Ok(())
            })
            .unwrap()
            .wait_bounded()
            .unwrap();
        handle.close();
        worker.join().unwrap();
    });
    let reader = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(count(&reader), 1, "only the second request is durable");
}

#[test]
fn a_failed_commit_that_sqlite_ended_the_transaction_for_is_answered_as_it_is() {
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    // A commit hook that vetoes turns the COMMIT into a rollback: SQLite itself
    // ends the transaction, so there is nothing for the executor to roll back.
    connection
        .for_migration()
        .commit_hook(Some(|| true))
        .unwrap();
    let state = FailureState::new();
    let outcome = execute(&mut connection, &gate, &state, None, Instant::now, |tx| {
        tx.execute("INSERT INTO sample VALUES(1)", [])?;
        Ok(())
    });
    assert!(
        outcome.is_err() && !matches!(outcome, Err(SqliteError::OperationRollback { .. })),
        "{outcome:?}"
    );
    assert!(connection.is_autocommit());
    assert_eq!(state.failure(), None);
    assert_eq!(count(&connection), 0);
}

#[test]
fn a_deadline_that_passes_after_begin_whose_rollback_is_denied_fails_the_writer() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    if !in_scenario_child(
        "sqlite::writer::tests::a_deadline_that_passes_after_begin_whose_rollback_is_denied_fails_the_writer",
    ) {
        return;
    }
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    connection
        .for_migration()
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Transaction {
                operation: TransactionOperation::Rollback,
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let start = Instant::now();
    let limit = start + Duration::from_secs(60);
    // In time before BEGIN; expired once it holds the reservation.
    let reads = std::cell::Cell::new(0);
    let clock = || {
        reads.set(reads.get() + 1);
        if reads.get() == 1 { start } else { limit }
    };
    let state = FailureState::new();
    let outcome = execute(
        &mut connection,
        &gate,
        &state,
        Some(limit),
        clock,
        |_| -> Result<()> { panic!("an expired request never runs") },
    );
    // Both failures are kept, and the unrolled-back transaction is the cause.
    assert!(
        matches!(
            &outcome,
            Err(SqliteError::OperationRollback { operation, .. })
                if matches!(**operation, SqliteError::Overloaded)
        ),
        "{outcome:?}"
    );
    assert!(
        !connection.is_autocommit(),
        "the denied rollback left the transaction open"
    );
    assert_eq!(
        state.failure().map(|f| f.causes),
        Some(vec![(
            Role::Operation,
            FatalCause::RollbackLeftTransactionOpen
        )])
    );
    // Whatever follows is refused as a latched writer refuses it, unrun.
    let next = execute(
        &mut connection,
        &gate,
        &state,
        None,
        Instant::now,
        |_| -> Result<()> { panic!("a latched writer runs nothing") },
    );
    assert!(
        matches!(next, Err(SqliteError::StorageUnavailable(_))),
        "{next:?}"
    );
    // Leave the connection as the worker's failure would: hooks off, rolled back.
    connection
        .for_migration()
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
    connection.execute_batch("ROLLBACK").unwrap();
    connection
        .for_migration()
        .rollback_hook(None::<fn()>)
        .unwrap();
    connection
        .for_migration()
        .commit_hook(None::<fn() -> bool>)
        .unwrap();
}

fn latch_for_test(state: &FailureState) -> Failure {
    state.latch(
        FailureSource::Writer,
        FatalCause::RollbackLeftTransactionOpen,
    );
    state.failure().expect("latched")
}

#[test]
fn a_submit_racing_the_latch_answers_the_latched_failure_not_a_clean_close() {
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (_writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let state = handle.failure_state();
    // The submit has passed its first check; the worker then latches a failure and
    // closes admission, the order a failed worker takes. The closed door must not
    // hide the latch behind an ordinary shutdown.
    let mut latched = None;
    let outcome = handle
        .submit_after(
            Lane::Entities,
            None,
            |_| Ok(()),
            || {
                latched = Some(latch_for_test(&state));
                handle.close();
            },
        )
        .err();
    assert!(
        matches!(&outcome, Some(SqliteError::StorageUnavailable(f)) if Some(f) == latched.as_ref()),
        "{outcome:?}"
    );
}

#[test]
fn a_submit_racing_an_ordinary_shutdown_still_answers_writer_closed() {
    // The control for the race above: with nothing latched the same window is an
    // ordinary shutdown, which keeps its own answer.
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (_writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let outcome = handle
        .submit_after(Lane::Entities, None, |_| Ok(()), || handle.close())
        .err();
    assert!(
        matches!(outcome, Some(SqliteError::WriterClosed)),
        "{outcome:?}"
    );
}

#[test]
fn a_request_dropped_unanswered_by_a_failed_worker_answers_the_latched_failure() {
    // A request accepted in the instant the worker fails is dropped with the
    // queue, and its receipt disconnects. The latch precedes that, so it is the answer.
    let (_temp, data) = data_with("CREATE TABLE sample(id INTEGER) STRICT");
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let receipt = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    let latched = latch_for_test(&handle.failure_state());
    drop(writer);
    let outcome = receipt.wait();
    assert!(
        matches!(&outcome, Err(SqliteError::StorageUnavailable(f)) if *f == latched),
        "{outcome:?}"
    );
    // Control: dropped by an ordinary close, the same receipt answers WriterClosed.
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let receipt = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    drop(writer);
    let outcome = receipt.wait();
    assert!(
        matches!(outcome, Err(SqliteError::WriterClosed)),
        "{outcome:?}"
    );
}

// The lease helpers are what a composed transaction calls. They run here on the
// real executor, after a SQLite rollback the caller swallowed, with real tables.

const LEASE_SCHEMA_WITH_LOSS: &str = concat!(
    include_str!("../../schema.sql"),
    "; CREATE TABLE scratch(x INTEGER) STRICT;
    CREATE TRIGGER scratch_rollback BEFORE INSERT ON scratch
    BEGIN SELECT RAISE(ROLLBACK, 'synthetic SQLite rollback'); END;"
);

fn lease_caller() -> orch_core::store::Caller {
    orch_core::store::Caller {
        project: "p".into(),
        principal: orch_core::store::Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn lease_request(task: &str) -> orch_core::store::leases::NewLease {
    orch_core::store::leases::NewLease {
        task: task.into(),
        agent: format!("agent-{task}"),
        run: format!("run-{task}"),
    }
}

/// (lease count, expiry of task t's lease, epoch counters) as committed.
fn lease_state(data: &crate::data_dir::DataDir) -> (i64, Option<i64>, i64) {
    let reader = open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    let one = |sql: &str| {
        reader
            .query_row(sql, [], |r| r.get::<_, Option<i64>>(0))
            .unwrap()
    };
    (
        one("SELECT count(*) FROM leases").unwrap(),
        one("SELECT expires_at FROM leases WHERE task_id='t'"),
        one("SELECT count(*) FROM task_epochs").unwrap(),
    )
}

#[test]
fn lease_renew_release_and_claim_after_a_sqlite_rollback_are_not_durable() {
    use crate::sqlite::leases::{claim_lease, release_lease, renew_lease};
    use orch_core::store::leases::ClaimOutcome;
    if !in_scenario_child(
        "sqlite::writer::tests::lease_renew_release_and_claim_after_a_sqlite_rollback_are_not_durable",
    ) {
        return;
    }
    let (_temp, data) = data_with(LEASE_SCHEMA_WITH_LOSS);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let granted = std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        let granted = handle
            .submit(Lane::Claims, None, |tx| {
                match claim_lease(tx, &lease_caller(), &lease_request("t"), 0)? {
                    ClaimOutcome::Granted(lease) => Ok(lease),
                    other => panic!("{other:?}"),
                }
            })
            .unwrap()
            .wait_bounded()
            .unwrap();
        let (leases, expiry, epochs) = lease_state(&data);
        assert_eq!((leases, expiry, epochs), (1, Some(granted.expires_at), 1));
        // Renew, then release, after the transaction was lost and the error swallowed.
        for operation in ["renew", "release"] {
            let lease = granted.clone();
            let outcome = handle
                .submit(Lane::Claims, None, move |tx| {
                    lose_the_transaction(tx);
                    let caller = lease_caller();
                    let _ = match operation {
                        "renew" => {
                            renew_lease(tx, &caller, "t", &lease.lineage, lease.epoch, 5_000)
                        }
                        _ => release_lease(tx, &caller, "t", &lease.lineage, lease.epoch),
                    };
                    Ok(())
                })
                .unwrap()
                .wait_bounded();
            assert!(
                matches!(outcome, Err(SqliteError::TransactionAborted)),
                "{operation}: {outcome:?}"
            );
            assert_eq!(
                lease_state(&data),
                (1, Some(granted.expires_at), 1),
                "{operation}"
            );
        }
        // A claim after the loss opens its own implicit transaction, and the
        // executor refuses the release that would commit it. No lease and no
        // consumed epoch may be left behind. The claim's own cleanup cannot find
        // its savepoint, but SQLite ended the transaction, so the executor answers
        // `TransactionAborted` and nothing is latched: the writer serves on.
        let claim = handle
            .submit(Lane::Claims, None, |tx| {
                lose_the_transaction(tx);
                let _ = claim_lease(tx, &lease_caller(), &lease_request("u"), 0);
                Ok(())
            })
            .unwrap()
            .wait_bounded();
        assert!(
            matches!(claim, Err(SqliteError::TransactionAborted)),
            "{claim:?}"
        );
        assert_eq!(handle.failure_state().failure(), None);
        assert_eq!(lease_state(&data), (1, Some(granted.expires_at), 1));
        handle.close();
        worker.join().expect("the writer exits cleanly");
        granted
    });
    assert_eq!(granted.epoch, 1);
}

#[test]
fn begin_busy_retains_the_original_callback_until_the_lock_clears() {
    let (_root, data) = test_data();
    let mut other = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    other
        .execute_batch("CREATE TABLE retained_begin(id INTEGER PRIMARY KEY) STRICT")
        .unwrap();
    let blocker = std::cell::RefCell::new(Some(other.transaction().unwrap()));
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    let state = FailureState::new();
    let now = Instant::now();
    let ticks = std::cell::Cell::new(0);
    let calls = std::cell::Cell::new(0);
    let result = execute(
        &mut connection,
        &gate,
        &state,
        Some(now + BOUND),
        || {
            let tick = ticks.get() + 1;
            ticks.set(tick);
            if tick >= 2 {
                drop(blocker.borrow_mut().take());
            }
            now
        },
        |tx| {
            calls.set(calls.get() + 1);
            tx.execute("INSERT INTO retained_begin VALUES(1)", [])?;
            Ok(())
        },
    );
    assert!(
        result.is_ok(),
        "BEGIN should retain the unconsumed callback: {result:?}"
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM retained_begin", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

fn injected_sql(code: i32) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

#[test]
fn busy_commit_retries_the_same_transaction_and_counts_over_bound_once() {
    let (_root, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection
        .execute_batch("CREATE TABLE retry_rows(id INTEGER PRIMARY KEY) STRICT")
        .unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    let mut attempts = 0;
    *gate.commit_probe.lock().unwrap() = Some(Box::new(move |tx| {
        attempts += 1;
        assert!(!tx.is_autocommit());
        assert_eq!(
            tx.query_row("SELECT count(*) FROM retry_rows", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1001
        );
        if attempts <= 3 {
            Some(Err(injected_sql(5)))
        } else {
            None
        }
    }));
    let state = FailureState::new();
    let episode = Episode::new(state.clone(), None);
    let calls = std::cell::Cell::new(0);
    let waits = std::cell::RefCell::new(Vec::new());
    execute_episode(&mut connection, &gate, &state, &episode, &Instant::now, &|delay| waits.borrow_mut().push(delay), |tx| {
        calls.set(calls.get() + 1);
        tx.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) INSERT INTO retry_rows SELECT x FROM n")?;
        Ok(())
    }).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(episode.snapshot().0, WriteOutcome::Committed);
    assert_eq!(episode.snapshot().2.rows, 1001);
    assert_eq!(state.writer_txn_over_bound_total(), 1);
    assert_eq!(
        waits.borrow().iter().copied().sum::<Duration>(),
        Duration::from_millis(350)
    );
    assert!(!state.claims_suppressed());
}

#[test]
fn commit_error_after_real_commit_is_unknown_until_producer_reconciliation() {
    let (_root, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection
        .execute_batch("CREATE TABLE unknown_commit(id INTEGER PRIMARY KEY) STRICT")
        .unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    *gate.commit_probe.lock().unwrap() = Some(Box::new(|tx| {
        tx.execute_batch("COMMIT").unwrap();
        Some(Err(injected_sql(10)))
    }));
    let state = FailureState::new();
    let start = Instant::now();
    let clock = std::cell::Cell::new(start);
    let episode = Episode::new(state.clone(), Some(start + Duration::from_secs(1)));
    let error = execute_episode(
        &mut connection,
        &gate,
        &state,
        &episode,
        &|| clock.get(),
        &|_| {},
        |tx| {
            tx.execute("INSERT INTO unknown_commit VALUES(1)", [])?;
            clock.set(start + Duration::from_secs(2));
            Ok(())
        },
    )
    .unwrap_err();
    assert!(matches!(error, SqliteError::Sql(_)));
    assert!(connection.is_autocommit());
    assert_eq!(
        episode.snapshot().0,
        WriteOutcome::Unknown,
        "autocommit cannot prove noncommit"
    );
    assert_eq!(episode.snapshot().1, Some(RetryCode::Io));
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM unknown_commit", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(state.claims_suppressed());
}

#[test]
fn unsafe_commit_failure_is_rolled_back_and_never_replays_callback() {
    let (_root, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection
        .execute_batch("CREATE TABLE rollback_commit(id INTEGER PRIMARY KEY) STRICT")
        .unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    *gate.commit_probe.lock().unwrap() = Some(Box::new(|_| Some(Err(injected_sql(13)))));
    let state = FailureState::new();
    let episode = Episode::new(state.clone(), None);
    let calls = std::cell::Cell::new(0);
    assert!(
        execute_episode(
            &mut connection,
            &gate,
            &state,
            &episode,
            &Instant::now,
            &|_| {},
            |tx| {
                calls.set(calls.get() + 1);
                tx.execute("INSERT INTO rollback_commit VALUES(1)", [])?;
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(episode.snapshot().0, WriteOutcome::RolledBack);
    assert_eq!(episode.snapshot().1, Some(RetryCode::Full));
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM rollback_commit", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(state.failure().is_none());
}

#[test]
fn a_known_commit_breaks_the_writer_ioerr_streak_between_requests() {
    let (_root, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    let state = FailureState::new();
    let now = Instant::now();
    {
        let failed = Episode::new(state.clone(), None);
        failed.failed(RetryCode::Io, now).unwrap();
    }
    execute(&mut connection, &gate, &state, None, Instant::now, |tx| {
        tx.execute_batch("CREATE TABLE ioerr_commit_break(id INTEGER PRIMARY KEY) STRICT")?;
        Ok(())
    })
    .unwrap();
    let failed = Episode::new(state.clone(), None);
    failed.failed(RetryCode::Io, now).unwrap();
    assert!(state.failure().is_none());
}

// Issue 448. One retained producer is the supported cardinality: the Events lane
// admits one queued entry and the executor has one continuation slot. A second
// retained write that rebuilds while the slot is taken is refused `Overloaded`
// with its episode kept live (the producer may resubmit); it is neither queued
// behind the claims its retry suppresses nor a failure of the database.
#[test]
fn a_second_retained_rebuild_is_refused_until_the_continuation_slot_is_taken() {
    let (_temp, data) = test_data();
    let state = FailureState::new();
    let (writer, handle) =
        Writer::open_with_state(&data, Synchronous::Normal, 8, state.clone()).unwrap();
    // Both episodes failed at the same instant: each must stay its own episode.
    let now = Instant::now();
    let first = Episode::new(state.clone(), None);
    let second = Episode::new(state.clone(), None);
    first.failed(RetryCode::Busy, now).unwrap();
    second.failed(RetryCode::Busy, now).unwrap();
    let held = handle
        .submit_episode(Lane::Events, first.clone(), |_| Ok(1), || {})
        .unwrap();
    let refused = handle.submit_episode(Lane::Events, second.clone(), |_| Ok(2), || {});
    assert!(matches!(refused, Err(SqliteError::Overloaded)));
    assert!(
        state.failure().is_none(),
        "contention is not a database failure"
    );
    assert!(second.retrying(), "the refused episode stays live");
    assert!(state.claims_suppressed());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        assert_eq!(held.wait_bounded().unwrap(), 1);
        // The first episode ended with its commit; the other one is untouched.
        assert!(!first.retrying());
        assert!(second.retrying(), "an episode ended another's registration");
        assert!(state.claims_suppressed());
        let resubmitted = handle
            .submit_episode(Lane::Events, second.clone(), |_| Ok(2), || {})
            .unwrap();
        assert_eq!(resubmitted.wait_bounded().unwrap(), 2);
        assert!(!second.retrying());
        assert!(!state.claims_suppressed());
        handle.close();
        worker.join().unwrap();
    });
}

mod retained_bounds;
