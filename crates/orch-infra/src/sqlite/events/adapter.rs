//! Internal typed forwarding over the landed writer and read pool.
//! No raw connection escapes; caller.project selects every operation.
use super::*;
use crate::sqlite::{Lane, ReadPool, SqliteError, WriterHandle};
use std::sync::Arc;
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

impl EventAdapter<'_, '_> {
    /// One event-lane transaction, potentially containing several runs and gaps.
    /// A SQL failure poisons the whole job even when trusted code catches it.
    pub(super) fn write<T: Send + 'static>(
        &self,
        deadline: Option<Instant>,
        work: impl FnOnce(&mut EventBatch<'_>) -> orch_core::store::Result<T> + Send + 'static,
    ) -> orch_core::store::Result<T> {
        let caller = self.caller.clone();
        let blobs = Arc::clone(self.blobs);
        self.writer
            .submit(Lane::Events, deadline, move |tx| {
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
            })
            .map_err(port)?
            .wait()
            .map_err(port)
    }

    pub(super) fn prune(
        &self,
        run: String,
        max_rows: usize,
    ) -> orch_core::store::Result<PruneResult> {
        let caller = self.caller.clone();
        self.writer
            .submit(Lane::Maintenance, None, move |tx| {
                prune_events(tx, &caller.project, &run, max_rows).map_err(execution)
            })
            .map_err(port)?
            .wait()
            .map_err(port)
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
