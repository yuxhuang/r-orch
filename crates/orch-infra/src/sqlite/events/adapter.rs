//! Internal typed forwarding over the landed writer and read pool.
//! No raw connection escapes; caller.project selects every operation.
use super::*;
use crate::sqlite::{Lane, ReadPool, SqliteError, WriterHandle};
use rusqlite::Transaction as SqlTransaction;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

pub(in crate::sqlite) fn execution(error: EventsError) -> SqliteError {
    match error {
        EventsError::Store(error) => SqliteError::Store(error),
        EventsError::Sql(error) => SqliteError::Sql(error),
        EventsError::Blob(error) => SqliteError::Io(std::io::Error::other(error)),
        EventsError::Corrupt(error) => SqliteError::EventCorrupt(error),
    }
}

/// What a caller of the adapter is told of a failure that is not a refusal.
fn backend_text(error: &SqliteError) -> StoreError {
    StoreError::Backend(orch_core::Error::other(format!("SQLite events: {error}")))
}

fn port(error: SqliteError) -> StoreError {
    match error {
        SqliteError::Store(error) => error,
        other => backend_text(&other),
    }
}

pub(super) struct EventAdapter<'a, 'data> {
    pub(super) writer: &'a WriterHandle,
    pub(super) readers: &'a ReadPool<'data>,
    pub(super) caller: Caller,
    pub(super) blobs: &'a Arc<DiskBlobStore>,
}

/// Which side owns a submitted request. The worker takes it when the body
/// starts; the waiter takes it back when its deadline passes first. Exactly one
/// of the two wins, so an answered timeout is never followed by a commit.
pub(super) struct Handover(AtomicU8);

impl Handover {
    const PENDING: u8 = 0;
    const STARTED: u8 = 1;
    const WITHDRAWN: u8 = 2;

    pub(super) fn new() -> Self {
        Self(AtomicU8::new(Self::PENDING))
    }

    fn claim(&self, to: u8) -> bool {
        self.0
            .compare_exchange(Self::PENDING, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// The worker, inside the transaction and before the body. `false`: the
    /// waiter already answered, so the body must not run.
    pub(super) fn start(&self) -> bool {
        self.claim(Self::STARTED)
    }

    /// The waiter, at its deadline. `true`: the body will never run.
    pub(super) fn withdraw(&self) -> bool {
        self.claim(Self::WITHDRAWN)
    }
}

fn event_job<T>(
    caller: Caller,
    blobs: Arc<DiskBlobStore>,
    work: impl FnOnce(&mut EventBatch<'_>) -> orch_core::store::Result<T> + Send + 'static,
) -> impl FnOnce(&SqlTransaction<'_>) -> crate::sqlite::Result<T> + Send + 'static {
    move |tx| {
        let mut batch = EventBatch {
            tx,
            caller: &caller,
            failed: None,
            blobs: &blobs,
        };
        let result = work(&mut batch);
        if let Some(error) = batch.failed {
            return Err(error);
        }
        result.map_err(SqliteError::Store)
    }
}

fn prune_job(
    caller: Caller,
    run: String,
    max_rows: usize,
) -> impl FnOnce(&SqlTransaction<'_>) -> crate::sqlite::Result<PruneResult> + Send + 'static {
    move |tx| prune_events(tx, &caller.project, &run, max_rows).map_err(execution)
}

impl EventAdapter<'_, '_> {
    /// One event-lane transaction, potentially containing several runs and gaps.
    /// A SQL failure poisons the whole job even when trusted code catches it.
    ///
    /// Blocks until the writer answers, with no bound: for tests and threads
    /// that may block. An async caller uses `write_async`.
    pub(super) fn write<T: Send + 'static>(
        &self,
        deadline: Option<Instant>,
        work: impl FnOnce(&mut EventBatch<'_>) -> orch_core::store::Result<T> + Send + 'static,
    ) -> orch_core::store::Result<T> {
        let job = event_job(self.caller.clone(), Arc::clone(self.blobs), work);
        self.writer
            .submit(Lane::Events, deadline, job)
            .map_err(port)?
            .wait()
            .map_err(port)
    }

    pub(super) fn prune(
        &self,
        run: String,
        max_rows: usize,
    ) -> orch_core::store::Result<PruneResult> {
        self.writer
            .submit(
                Lane::Maintenance,
                None,
                prune_job(self.caller.clone(), run, max_rows),
            )
            .map_err(port)?
            .wait()
            .map_err(port)
    }

    /// `write` for an async caller: it never parks the executor thread and it
    /// answers by `deadline`. See `submit_async` for what the deadline means.
    pub(super) async fn write_async<T: Send + 'static>(
        &self,
        deadline: Instant,
        work: impl FnOnce(&mut EventBatch<'_>) -> orch_core::store::Result<T> + Send + 'static,
    ) -> orch_core::store::Result<T> {
        let job = event_job(self.caller.clone(), Arc::clone(self.blobs), work);
        self.submit_async(Lane::Events, deadline, job).await
    }

    /// `prune` for an async caller, bounded like `write_async`.
    pub(super) async fn prune_async(
        &self,
        run: String,
        max_rows: usize,
        deadline: Instant,
    ) -> orch_core::store::Result<PruneResult> {
        let job = prune_job(self.caller.clone(), run, max_rows);
        self.submit_async(Lane::Maintenance, deadline, job).await
    }

    /// Queue `job`, then wait for its receipt on a blocking-pool thread so that
    /// the awaiting executor thread stays free. Must run inside a Tokio runtime.
    ///
    /// `deadline` is the request's deadline (spec R128) and also the waiter's.
    /// If it passes while the request has not started, the request is withdrawn
    /// and the caller gets `Overloaded`: the worker refuses a withdrawn request
    /// even if it reaches it later, so it never commits. If the body had already
    /// started, the commit is not ours to stop and the answer is an error that
    /// says the outcome is unknown; the caller may retry, because events carry
    /// their own sequence numbers and a prune is repeatable. Either way the
    /// blocking thread ends once the writer reaches or drops the request.
    async fn submit_async<T: Send + 'static>(
        &self,
        lane: Lane,
        deadline: Instant,
        job: impl FnOnce(&SqlTransaction<'_>) -> crate::sqlite::Result<T> + Send + 'static,
    ) -> orch_core::store::Result<T> {
        let handover = Arc::new(Handover::new());
        let worker_side = Arc::clone(&handover);
        let receipt = self
            .writer
            .submit(lane, Some(deadline), move |tx| {
                if !worker_side.start() {
                    return Err(SqliteError::Overloaded);
                }
                job(tx)
            })
            .map_err(port)?;
        let mut waiting = tokio::task::spawn_blocking(move || receipt.wait());
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut waiting).await
        {
            Ok(Ok(answer)) => answer.map_err(port),
            Ok(Err(error)) => Err(StoreError::Backend(orch_core::Error::other(format!(
                "SQLite events: receipt wait failed: {error}"
            )))),
            Err(_elapsed) if handover.withdraw() => Err(port(SqliteError::Overloaded)),
            Err(_elapsed) => Err(StoreError::Backend(orch_core::Error::other(
                "SQLite events: deadline passed while the transaction was running; \
                 its outcome is unknown",
            ))),
        }
    }

    /// All calls in the callback see the same committed read snapshot.
    pub(super) fn snapshot<T>(
        &self,
        deadline: Instant,
        read: impl FnOnce(&EventSnapshot<'_>) -> orch_core::store::Result<T>,
    ) -> orch_core::store::Result<T> {
        self.readers
            .read(deadline, |tx| {
                read(&EventSnapshot {
                    tx,
                    caller: &self.caller,
                    blobs: self.blobs,
                    state: self.readers.failure_state(),
                })
                .map_err(SqliteError::Store)
            })
            .map_err(port)
    }
}

pub(super) struct EventBatch<'a> {
    tx: &'a Transaction<'a>,
    caller: &'a Caller,
    failed: Option<SqliteError>,
    blobs: &'a DiskBlobStore,
}

impl EventBatch<'_> {
    fn result<T>(&mut self, result: Result<T>) -> orch_core::store::Result<T> {
        if self.failed.is_some() {
            return Err(StoreError::Backend(orch_core::Error::other(
                "event transaction already failed",
            )));
        }
        match result {
            Ok(value) => Ok(value),
            Err(EventsError::Store(error)) => Err(error),
            Err(error) => {
                let error = execution(error);
                let refusal = backend_text(&error);
                self.failed = Some(error);
                Err(refusal)
            }
        }
    }

    pub(super) fn append(
        &mut self,
        run: &str,
        lineage: &str,
        epoch: u64,
        events: &[SequencedEvent],
    ) -> orch_core::store::Result<AppendResult> {
        // Do not run another operation after a failed SQL statement.
        if self.failed.is_some() {
            return self.result(Ok(AppendResult {
                stored: 0,
                skipped: 0,
            }));
        }
        let result = append_with_blobs(
            self.tx,
            self.caller,
            run,
            lineage,
            epoch,
            events,
            Some(self.blobs),
        );
        self.result(result)
    }

    pub(super) fn gap(
        &mut self,
        run: &str,
        after: u64,
        dropped: u64,
    ) -> orch_core::store::Result<()> {
        if self.failed.is_some() {
            return self.result(Ok(()));
        }
        let result = record_event_gap(self.tx, &self.caller.project, run, after, dropped);
        self.result(result)
    }
}

pub(super) struct EventSnapshot<'a> {
    tx: &'a Transaction<'a>,
    caller: &'a Caller,
    blobs: &'a DiskBlobStore,
    state: &'a crate::sqlite::FailureState,
}

impl EventSnapshot<'_> {
    /// Observe every structured failure before the port converts it to text.
    /// A caught ordinary failure must not hide a later fatal one in this snapshot.
    fn result<T>(&self, result: Result<T>) -> orch_core::store::Result<T> {
        result.map_err(|error| {
            let error = execution(error);
            self.state
                .observe(crate::sqlite::FailureSource::Reader, &error);
            port(error)
        })
    }

    pub(super) fn events(
        &self,
        run: &str,
        after: u64,
        limit: usize,
    ) -> orch_core::store::Result<Vec<SequencedEvent>> {
        self.result(read_with_blobs(
            self.tx,
            &self.caller.project,
            run,
            after,
            limit,
            Some(self.blobs),
        ))
    }
    pub(super) fn tail(
        &self,
        run: &str,
        count: usize,
    ) -> orch_core::store::Result<Vec<SequencedEvent>> {
        self.result(tail_with_blobs(
            self.tx,
            &self.caller.project,
            run,
            count,
            Some(self.blobs),
        ))
    }
    pub(super) fn gaps(&self, run: &str) -> orch_core::store::Result<Vec<EventGap>> {
        self.result(read_event_gaps(self.tx, &self.caller.project, run))
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::*;
    use crate::sqlite::{
        ConnectionRole, FailureSource, FatalCause, Role, Synchronous, Writer, migrations,
        open_connection,
    };
    use orch_core::store::Principal;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn event(seq: u64) -> SequencedEvent {
        SequencedEvent {
            seq,
            event: orch_domain::RunEvent {
                timestamp: "2026-01-01T00:00:00Z".into(),
                event_type: orch_domain::RunEventType::AgentOutput,
                data: serde_json::json!({"seq": seq}),
            },
        }
    }

    // Every connection is closed before damage, and only a fresh synthetic DB is touched.
    fn damaged(table: &str, test: impl FnOnce(EventAdapter<'_, '_>, &AtomicUsize)) {
        damaged_with(table, |_| {}, test);
    }

    fn damaged_with(
        table: &str,
        prepare: impl FnOnce(&Connection),
        test: impl FnOnce(EventAdapter<'_, '_>, &AtomicUsize),
    ) {
        let (_temp, data) = crate::sqlite::test_data();
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        migrations::migrate(&data, &mut conn).unwrap();
        let caller = Caller {
            project: "p".into(),
            principal: Principal::Owner {
                identity: "synthetic".into(),
            },
        };
        let tx = conn.transaction().unwrap();
        let lease = crate::sqlite::leases::claim_lease(
            &tx,
            &caller,
            &orch_core::store::leases::NewLease {
                task: "t".into(),
                agent: "a".into(),
                run: "r".into(),
            },
            0,
        )
        .unwrap();
        let orch_core::store::leases::ClaimOutcome::Granted(lease) = lease else {
            panic!("seed lease")
        };
        append_events(
            &tx,
            &caller,
            "r",
            &lease.lineage,
            lease.epoch,
            &[event(1), event(2)],
        )
        .unwrap();
        record_event_gap(&tx, "p", "r", 2, 1).unwrap();
        tx.commit().unwrap();
        prepare(&conn);
        drop(conn);
        crate::sqlite::corrupt_root_page(&data, table);
        let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
        let (mut writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
        let commits = Arc::new(AtomicUsize::new(0));
        let count = commits.clone();
        writer.set_commit_probe(Box::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            None
        }));
        let pool = ReadPool::open_with_state(&data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || writer.run());
            struct Close<'a>(&'a WriterHandle);
            impl Drop for Close<'_> {
                fn drop(&mut self) {
                    self.0.close();
                }
            }
            let close = Close(&handle);
            test(
                EventAdapter {
                    writer: &handle,
                    readers: &pool,
                    caller,
                    blobs: &blobs,
                },
                &commits,
            );
            drop(close);
            worker.join().unwrap();
        });
    }

    fn latched(adapter: &EventAdapter<'_, '_>, source: FailureSource) {
        let failure = adapter
            .writer
            .failure_state()
            .failure()
            .expect("SQLite category must survive public text conversion");
        assert_eq!(failure.source, source);
        assert_eq!(
            failure.causes,
            [(Role::Operation, FatalCause::SqliteCorrupt)]
        );
        assert!(matches!(
            adapter
                .writer
                .submit(crate::sqlite::Lane::Entities, None, |_| Ok(())),
            Err(SqliteError::StorageUnavailable(_))
        ));
        // A prior latch still permits healthy reads under the signed issue 389 policy.
        adapter
            .snapshot(Instant::now() + Duration::from_secs(10), |_| Ok(()))
            .unwrap();
    }

    #[test]
    fn real_corrupt_snapshot_statements_retain_reader_category_even_when_caught() {
        for (name, table) in [
            ("events", "run_events"),
            ("tail", "run_events"),
            ("gaps", "run_event_gaps"),
        ] {
            for caught in [false, true] {
                damaged(table, |adapter, commits| {
                    let answer =
                        adapter.snapshot(Instant::now() + Duration::from_secs(10), |read| {
                            let result = match name {
                                "events" => read.events("r", 0, 10).map(drop),
                                "tail" => read.tail("r", 10).map(drop),
                                _ => read.gaps("r").map(drop),
                            };
                            assert!(
                                matches!(&result, Err(StoreError::Backend(_))),
                                "{name}: {result:?}"
                            );
                            if caught { Ok(()) } else { result }
                        });
                    assert!(answer.is_err(), "{name}, caught {caught}: {answer:?}");
                    latched(&adapter, FailureSource::Reader);
                    assert_eq!(commits.load(Ordering::SeqCst), 0);
                });
            }
        }
    }

    #[test]
    fn a_caught_ordinary_event_format_error_allows_a_successful_snapshot() {
        damaged_with(
            "run_event_gaps",
            |conn| {
                conn.execute("UPDATE run_events SET payload='invalid json'", [])
                    .unwrap();
            },
            |adapter, _| {
                let value = adapter.snapshot(Instant::now() + Duration::from_secs(10), |read| {
                    assert!(read.events("r", 0, 10).is_err());
                    Ok(7)
                });
                assert_eq!(value.unwrap(), 7);
                assert!(adapter.writer.failure_state().failure().is_none());
            },
        );
    }

    #[test]
    fn a_caught_ordinary_event_error_cannot_hide_later_real_sqlite_corruption() {
        damaged_with(
            "run_event_gaps",
            |conn| {
                conn.execute("UPDATE run_events SET payload='invalid json'", [])
                    .unwrap();
            },
            |adapter, commits| {
                let answer = adapter.snapshot(Instant::now() + Duration::from_secs(10), |read| {
                    // Stored event-format corruption remains ordinary under issue 313.
                    assert!(read.events("r", 0, 10).is_err());
                    assert!(adapter.writer.failure_state().failure().is_none());
                    // This second statement encounters actual SQLite page damage.
                    assert!(read.gaps("r").is_err());
                    Ok(())
                });
                assert!(answer.is_err());
                latched(&adapter, FailureSource::Reader);
                assert_eq!(commits.load(Ordering::SeqCst), 0);
            },
        );
    }

    #[test]
    fn real_corrupt_batch_and_prune_statements_retain_writer_category() {
        for (name, table) in [
            ("append", "run_events"),
            ("gap", "run_event_gaps"),
            ("prune", "run_events"),
        ] {
            damaged(table, |adapter, commits| {
                let answer = if name == "prune" {
                    adapter.prune("r".into(), 10).map(drop)
                } else {
                    adapter.write(None, move |batch| {
                        let result = if name == "gap" {
                            batch.gap("r", 3, 1)
                        } else {
                            // Read the intact lease through the real transaction to use its fence.
                            let lease =
                                crate::sqlite::leases::get_lease(batch.tx, batch.caller, "t")
                                    .unwrap()
                                    .unwrap();
                            batch
                                .append("r", &lease.lineage, lease.epoch, &[event(3)])
                                .map(drop)
                        };
                        assert!(matches!(result, Err(StoreError::Backend(_))));
                        Ok(())
                    })
                };
                assert!(
                    matches!(answer, Err(StoreError::Backend(_))),
                    "{name}: {answer:?}"
                );
                latched(&adapter, FailureSource::Writer);
                assert_eq!(
                    commits.load(Ordering::SeqCst),
                    0,
                    "{name}: no COMMIT attempt"
                );
            });
        }
    }
}
