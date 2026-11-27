use super::*;
use std::cell::Cell;
use std::sync::atomic::AtomicUsize;

#[test]
fn persistent_busy_commit_rolls_back_at_original_deadline_without_replay() {
    if !in_scenario_child(
        "sqlite::writer::tests::retained_bounds::persistent_busy_commit_rolls_back_at_original_deadline_without_replay",
    ) {
        return;
    }
    let (_root, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection
        .execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY) STRICT")
        .unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    let commits = Arc::new(AtomicUsize::new(0));
    let seen = commits.clone();
    *gate.commit_probe.lock().unwrap() = Some(Box::new(move |tx| {
        // A connection replacement or rollback/re-BEGIN loses this uncommitted row.
        assert!(!tx.is_autocommit());
        assert_eq!(count(tx), 1);
        seen.fetch_add(1, Ordering::SeqCst);
        Some(Err(injected_sql(5)))
    }));
    let state = FailureState::new();
    let start = Instant::now();
    let deadline = start + Duration::from_millis(175);
    let clock = Cell::new(start);
    let episode = Episode::new(state.clone(), Some(deadline));
    let calls = Cell::new(0);
    let waits = Cell::new(Duration::ZERO);
    let result = execute_episode(
        &mut connection,
        &gate,
        &state,
        &episode,
        &|| clock.get(),
        &|piece| {
            waits.set(waits.get() + piece);
            clock.set(clock.get() + piece);
        },
        |tx| {
            calls.set(calls.get() + 1);
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            Ok(())
        },
    );
    assert!(matches!(result, Err(SqliteError::Overloaded)), "{result:?}");
    assert_eq!(clock.get(), deadline);
    assert_eq!(waits.get(), Duration::from_millis(175));
    assert_eq!(commits.load(Ordering::SeqCst), 3);
    assert_eq!(calls.get(), 1);
    assert_eq!(episode.snapshot().0, WriteOutcome::RolledBack);
    assert_eq!(episode.snapshot().1, Some(RetryCode::Busy));
    assert!(connection.is_autocommit());
    assert_eq!(count(&connection), 0);
    assert!(state.failure().is_none());
}

#[test]
fn elapsed_work_alone_counts_one_indivisible_transaction_across_busy_commits() {
    let (_root, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    connection
        .execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY) STRICT")
        .unwrap();
    let gate = Gate::install(&mut connection).unwrap();
    let commits = Arc::new(AtomicUsize::new(0));
    let seen = commits.clone();
    *gate.commit_probe.lock().unwrap() = Some(Box::new(move |tx| {
        assert!(!tx.is_autocommit());
        assert_eq!(count(tx), 1);
        (seen.fetch_add(1, Ordering::SeqCst) < 3).then(|| Err(injected_sql(5)))
    }));
    let state = FailureState::new();
    let episode = Episode::new(state.clone(), None);
    let calls = Cell::new(0);
    execute_episode(
        &mut connection,
        &gate,
        &state,
        &episode,
        &Instant::now,
        &|_| {},
        |tx| {
            calls.set(calls.get() + 1);
            tx.execute("INSERT INTO sample VALUES(1)", [])?;
            // Accounting uses the physical monotonic clock, not the retry clock.
            // Do real work until its lower bound is established; no timing sleep
            // or upper latency expectation makes this depend on scheduling luck.
            let work = Instant::now();
            while work.elapsed() < Duration::from_millis(21) {
                std::hint::black_box(count(tx));
            }
            Ok(())
        },
    )
    .unwrap();
    let (outcome, _, accounting) = episode.snapshot();
    assert_eq!(outcome, WriteOutcome::Committed);
    assert_eq!(accounting.rows, 1);
    assert!(
        accounting.work > Duration::from_millis(20),
        "{accounting:?}"
    );
    assert!(accounting.over_bound);
    assert_eq!(state.writer_txn_over_bound_total(), 1);
    assert_eq!(calls.get(), 1);
    assert_eq!(commits.load(Ordering::SeqCst), 4);
    assert_eq!(count(&connection), 1);
}

#[test]
fn idle_worker_retires_live_settled_token_at_its_original_deadline() {
    if !in_scenario_child(
        "sqlite::writer::tests::retained_bounds::idle_worker_retires_live_settled_token_at_its_original_deadline",
    ) {
        return;
    }
    let (_root, data) = test_data();
    let state = FailureState::new();
    let (mut writer, handle) =
        Writer::open_with_state(&data, Synchronous::Normal, 8, state.clone()).unwrap();
    // Drive an actual failed physical transaction on the worker connection with
    // a synthetic clock. Its original deadline is already past for run(), so
    // retirement needs neither a sleep nor a producer resubmission/Drop.
    let deadline = Instant::now() - Duration::from_secs(1);
    let start = deadline - Duration::from_secs(1);
    let episode = Episode::new(state.clone(), Some(deadline));
    let calls = Cell::new(0);
    let result = execute_episode(
        &mut writer.connection,
        &writer.gate,
        &state,
        &episode,
        &|| start,
        &|_| {},
        |_| {
            calls.set(calls.get() + 1);
            Err::<(), _>(SqliteError::Sql(injected_sql(13)))
        },
    );
    assert!(result.is_err());
    assert_eq!(episode.snapshot().0, WriteOutcome::RolledBack);
    assert!(state.claims_suppressed());
    state.check_retry_timeout(deadline - Duration::from_nanos(1));
    assert!(
        state.claims_suppressed(),
        "not eligible before the original deadline"
    );
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let _close = CloseOnDrop(&handle);
        // No queued job or producer action wakes this worker. The watchdog is
        // only a failure bound; the observed registry transition is the oracle.
        let watchdog = Instant::now() + BOUND;
        while state.claims_suppressed() {
            assert!(
                Instant::now() < watchdog,
                "idle worker did not retire token"
            );
            std::thread::yield_now();
        }
        let receipt = handle.submit(Lane::Maintenance, None, |_| Ok(73)).unwrap();
        assert_eq!(receipt.wait_bounded().unwrap(), 73);
        assert!(!state.claims_suppressed());
        assert!(state.failure().is_none());
        assert_eq!(calls.get(), 1);
        assert!(matches!(
            episode.check(deadline),
            Err(SqliteError::Overloaded)
        ));
        drop(episode);
        assert!(!state.claims_suppressed(), "expiry then Drop is idempotent");
        handle.close();
        worker.join().unwrap();
    });
}

impl WriterHandle {
    /// Admit a fixture on the real worker using the executor's existing clock
    /// seam. Only admission is fixture setup: SQL, hooks, retry registration and
    /// the single receipt all use the production executor. This lets retained
    /// Storage tests cross an original deadline without sleeps or a short real
    /// deadline that might expire before the test callback is scheduled.
    pub(in crate::sqlite) fn submit_at_test_time<T, F>(
        &self,
        episode: Episode,
        now: Instant,
        work: F,
    ) -> WriteReceipt<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    {
        let (response, receiver) = mpsc::sync_channel(1);
        let failure = self.state.clone();
        let job: Job = Box::new(move |connection, gate| {
            let _ = response.send(execute_episode(
                connection,
                gate,
                &failure,
                &episode,
                &|| now,
                &|_| {},
                work,
            ));
        });
        let mut queue = self.admission.shared.state.lock().unwrap();
        assert!(queue.accepting);
        assert!(queue.continuation.is_none());
        queue.continuation = Some(job);
        self.admission.shared.available.notify_one();
        WriteReceipt {
            response: receiver,
            state: self.state.clone(),
        }
    }
}
