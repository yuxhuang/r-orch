use super::*;
use crate::sqlite::read_pool::active::{MAX_AGE, MAX_BACKLOG};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn grow(writer: &mut crate::sqlite::ConnectionHandle<'_>, rows: usize) {
    let tx = writer.transaction().unwrap();
    for _ in 0..rows {
        tx.execute("INSERT INTO sample VALUES(zeroblob(1048576))", [])
            .unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn actual_checkpoint_backlog_and_age_are_both_required_and_a_long_file_is_not_backlog() {
    let (_temp, data) = test_data();
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(value BLOB) STRICT")
        .unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    pool.read(deadline(), |tx| {
        let count: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        grow(&mut writer, 1);
        pool.active.age_for_test(MAX_AGE + Duration::from_secs(1));
        let below = pool.checkpoint()?;
        assert!(below.backlog_bytes > 0 && below.backlog_bytes < MAX_BACKLOG);
        assert_eq!(below.interrupted, 0, "old alone is insufficient");
        grow(&mut writer, 257);
        pool.active.age_for_test(Duration::ZERO);
        let young = pool.checkpoint()?;
        assert!(young.backlog_bytes > MAX_BACKLOG);
        assert_eq!(young.interrupted, 0, "backlog alone is insufficient");
        let same: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
        assert_eq!(
            same, 0,
            "the original selecting snapshot survived both controls"
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(pool.checkpoint().unwrap().backlog_bytes, 0);
    assert!(
        std::fs::metadata(data.database_path().with_extension("db-wal"))
            .unwrap()
            .len()
            > MAX_BACKLOG
    );
    pool.read(deadline(), |tx| {
        let count: i64 = tx.query_row("SELECT count(*) FROM sample", [], |r| r.get(0))?;
        assert_eq!(count, 258);
        pool.active.age_for_test(MAX_AGE + Duration::from_secs(1));
        let empty = pool.checkpoint()?;
        assert_eq!((empty.backlog_bytes, empty.interrupted), (0, 0));
        assert_eq!(
            tx.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0))?,
            count
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn both_limits_interrupt_current_sql_and_caught_interrupt_cannot_publish_a_snapshot() {
    let (_temp, data) = test_data();
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(value BLOB) STRICT")
        .unwrap();
    let mut pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let running = Arc::new(AtomicBool::new(false));
    let signal = running.clone();
    let budget = Arc::new(AtomicUsize::new(usize::MAX));
    let fallback = Arc::new(AtomicBool::new(false));
    let remaining = budget.clone();
    let safety = fallback.clone();
    pool.configure_progress_for_test(Box::new(move || {
        signal.store(true, Ordering::SeqCst);
        if remaining.load(Ordering::SeqCst) != usize::MAX
            && remaining.fetch_sub(1, Ordering::SeqCst) == 0
        {
            safety.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }));
    let (entered, pinned) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let pool = &pool;
        let read = scope.spawn(move || pool.read(deadline(), |tx| {
            tx.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0))?;
            entered.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(20)).unwrap();
            let result = tx.query_row("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0));
            assert!(matches!(result, Err(rusqlite::Error::SqliteFailure(ref e, _)) if e.code == rusqlite::ErrorCode::OperationInterrupted));
            Ok(123) // Deliberately catch the raw SQL error; retirement still wins.
        }));
        pinned.recv_timeout(Duration::from_secs(20)).unwrap();
        grow(&mut writer, 258);
        pool.active.age_for_test(MAX_AGE + Duration::from_secs(1));
        resume.send(()).unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while !running.load(Ordering::SeqCst) {
            assert!(Instant::now() < end);
            std::thread::yield_now();
        }
        let observation = pool.checkpoint().unwrap();
        // A broken interrupt guard fails finitely after this many VM callbacks.
        budget.store(1024, Ordering::SeqCst);
        assert!(observation.backlog_bytes > MAX_BACKLOG);
        assert_eq!(observation.interrupted, 1);
        assert!(observation.oldest_read.unwrap() > MAX_AGE);
        assert!(matches!(
            read.join().unwrap(),
            Err(SqliteError::SnapshotExpired)
        ));
    });
    assert!(
        !fallback.load(Ordering::SeqCst),
        "the production interrupt must stop SQL before the safety hook"
    );
    assert!(pool.failure_state().failure().is_none());
    assert_eq!(pool.checkpoint().unwrap().backlog_bytes, 0);
    assert_eq!(
        pool.read(deadline(), |tx| Ok(tx.query_row(
            "SELECT count(*) FROM sample",
            [],
            |r| r.get::<_, i64>(0)
        )?))
        .unwrap(),
        258
    );
}

#[test]
fn sql_started_after_checkpoint_expiry_cannot_escape_the_progress_guard() {
    let (_temp, data) = test_data();
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    writer
        .execute_batch("CREATE TABLE sample(value BLOB) STRICT")
        .unwrap();
    let mut pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let fallback = Arc::new(AtomicBool::new(false));
    let safety = fallback.clone();
    let mut calls = 0;
    pool.configure_progress_for_test(Box::new(move || {
        calls += 1;
        if calls > 1024 {
            safety.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }));
    let result = pool.read(deadline(), |tx| {
        tx.query_row("SELECT count(*) FROM sample", [], |r| r.get::<_, i64>(0))?;
        grow(&mut writer, 258);
        pool.active.age_for_test(MAX_AGE + Duration::from_secs(1));
        assert_eq!(pool.checkpoint()?.interrupted, 1);
        // sqlite3_interrupt ran between statements. The persistent progress
        // guard must also stop this later SQL on the same selecting transaction.
        let raw = tx.query_row("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0));
        assert!(matches!(raw, Err(rusqlite::Error::SqliteFailure(ref e, _)) if e.code == rusqlite::ErrorCode::OperationInterrupted));
        Ok(())
    });
    assert!(matches!(result, Err(SqliteError::SnapshotExpired)));
    assert!(
        !fallback.load(Ordering::SeqCst),
        "the expiry guard must precede the finite safety limit"
    );
}
