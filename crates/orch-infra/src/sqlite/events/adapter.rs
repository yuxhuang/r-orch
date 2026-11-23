//! Internal typed forwarding over the landed writer and read pool.
//! No raw connection escapes; caller.project selects every operation.
use super::*;
use crate::sqlite::{Lane, ReadPool, SqliteError, WriterHandle};
use rusqlite::Transaction as SqlTransaction;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

fn execution(error: EventsError) -> SqliteError {
    match error {
        EventsError::Store(error) => SqliteError::Store(error),
        EventsError::Sql(error) => SqliteError::Sql(error),
        EventsError::Blob(error) => SqliteError::Io(std::io::Error::other(error)),
        EventsError::Corrupt(error) => SqliteError::EventCorrupt(error),
    }
}

fn port(error: SqliteError) -> StoreError {
    match error {
        SqliteError::Store(error) => error,
        other => StoreError::Backend(orch_core::Error::other(format!("SQLite events: {other}"))),
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
                let refusal =
                    StoreError::Backend(orch_core::Error::other(format!("SQLite events: {error}")));
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
}

impl EventSnapshot<'_> {
    pub(super) fn events(
        &self,
        run: &str,
        after: u64,
        limit: usize,
    ) -> orch_core::store::Result<Vec<SequencedEvent>> {
        read_with_blobs(
            self.tx,
            &self.caller.project,
            run,
            after,
            limit,
            Some(self.blobs),
        )
        .map_err(execution)
        .map_err(port)
    }
    pub(super) fn tail(
        &self,
        run: &str,
        count: usize,
    ) -> orch_core::store::Result<Vec<SequencedEvent>> {
        tail_with_blobs(self.tx, &self.caller.project, run, count, Some(self.blobs))
            .map_err(execution)
            .map_err(port)
    }
    pub(super) fn gaps(&self, run: &str) -> orch_core::store::Result<Vec<EventGap>> {
        read_event_gaps(self.tx, &self.caller.project, run)
            .map_err(execution)
            .map_err(port)
    }
}
