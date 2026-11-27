use super::*;
use crate::sqlite::{ConnectionRole, SqliteError, Synchronous, open_connection, test_data};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn a_read_snapshot_does_not_see_a_later_commit() {
    let (_temp, data) = test_data();
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT; INSERT INTO sample VALUES(1)")
        .unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    pool.read(deadline(), |tx| {
        let first: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
        let change = writer.transaction()?;
        change.execute("INSERT INTO sample VALUES(2)", [])?;
        change.commit()?;
        let second: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
        assert_eq!((first, second), (1, 1));
        assert!(tx.execute("INSERT INTO sample VALUES(3)", []).is_err());
        Ok(())
    })
    .unwrap();
    let total: i64 = pool
        .read(deadline(), |tx| {
            Ok(tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(total, 2);
}

#[test]
fn exhaustion_waits_until_deadline_and_never_opens_an_extra_reader() {
    let (_temp, data) = test_data();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    assert!(ReadPool::open(&data, Synchronous::Normal, 0).is_err());
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let pool = &pool;
        let active = scope.spawn(move || {
            pool.read(deadline(), |_| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let result: Result<()> = pool.read(Instant::now() + Duration::from_millis(20), |_| {
            panic!("opened an extra reader")
        });
        assert!(matches!(result, Err(SqliteError::Overloaded)));
        release_tx.send(()).unwrap();
        active.join().unwrap().unwrap();
        pool.read(deadline(), |_| Ok(())).unwrap();
    });
}

#[test]
fn error_and_panic_release_the_slot_and_end_the_snapshot() {
    let (_temp, data) = test_data();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let result = pool.read(deadline(), |tx| {
        tx.execute("INSERT INTO sample VALUES(1)", [])?;
        Ok(())
    });
    assert!(matches!(result, Err(SqliteError::Sql(_))));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<()> = pool.read(deadline(), |tx| {
            let _: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
            panic!("synthetic reader panic")
        });
    }));
    assert!(panic.is_err());
    pool.read(deadline(), |tx| {
        assert!(!tx.is_autocommit());
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_waiting_reader_uses_the_released_slot_before_its_deadline() {
    let (_temp, data) = test_data();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let pool = &pool;
        let active = scope.spawn(move || {
            pool.read(deadline(), |_| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let waiting = scope.spawn(|| {
            pool.read(deadline(), |tx| {
                Ok(tx.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0))?)
            })
        });
        release_tx.send(()).unwrap();
        active.join().unwrap().unwrap();
        assert_eq!(waiting.join().unwrap().unwrap(), 0);
    });
}

#[test]
fn a_free_slot_is_usable_when_a_waiter_reacquires_after_its_deadline() {
    let (_temp, data) = test_data();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    // Model a late wake deterministically: a slot is already free, and the
    // admission limit has elapsed. Taking the slot avoids losing its wakeup.
    pool.read(Instant::now(), |_| Ok(())).unwrap();
    drop(writer);
}

#[test]
fn a_failed_read_discards_its_connection_local_state() {
    let (_temp, data) = test_data();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(id INTEGER) STRICT")
        .unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let error: Result<()> = pool.read(deadline(), |tx| {
        tx.execute_batch("CREATE TEMP TABLE marker(id INTEGER); COMMIT")?;
        Err(SqliteError::Sql(rusqlite::Error::InvalidQuery))
    });
    assert!(error.is_err());
    pool.read(deadline(), |tx| {
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_temp_master WHERE name='marker'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0, "failed read connection was reused");
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_lease_left_in_a_transaction_is_discarded() {
    let (_temp, data) = test_data();
    let writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    // Exercise lease cleanup directly: rusqlite normally rolls back before
    // this Drop runs, so a callback panic alone does not cover this branch.
    let lease = pool.acquire(deadline()).unwrap();
    lease
        .connection
        .as_ref()
        .unwrap()
        .execute_batch("CREATE TEMP TABLE marker(id INTEGER); BEGIN")
        .unwrap();
    drop(lease);
    pool.read(deadline(), |tx| {
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_temp_master WHERE name='marker'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    })
    .unwrap();
    drop(writer);
}

mod active;
