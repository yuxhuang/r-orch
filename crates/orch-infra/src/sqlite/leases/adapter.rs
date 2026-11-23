//! Internal typed forwarding only; not a Storage facade or daemon entry point.
use super::*;
use crate::sqlite::{Lane, ReadPool, WriteReceipt, WriterHandle};
use orch_core::Clock;
use orch_core::store::Result as StoreResult;
use std::time::Instant;

/// Typed refusals stay inner outcomes. A SQL failure leaves the job as `Err`,
/// so the executor rolls the transaction back, never commits it, and can
/// classify the SQLite code instead of a backend string.
fn outcome<T>(result: Result<T>) -> crate::sqlite::Result<StoreResult<T>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(LeaseError::Store(error)) => Ok(Err(error)),
        Err(LeaseError::Sql(error)) => Err(error.into()),
    }
}

/// Caller is bound once by trusted adapter code; authorization remains at the
/// dispatcher. No request may supply SQL or change this adapter's project.
pub(super) struct LeaseAdapter<'a, 'data> {
    pub(super) writer: &'a WriterHandle,
    pub(super) readers: &'a ReadPool<'data>,
    pub(super) caller: Caller,
    pub(super) clock: &'a dyn Clock,
}

impl LeaseAdapter<'_, '_> {
    pub(super) fn claim(
        &self,
        new: NewLease,
    ) -> crate::sqlite::Result<WriteReceipt<StoreResult<ClaimOutcome>>> {
        let caller = self.caller.clone();
        let now = self.clock.now_ms();
        self.writer.submit(Lane::Claims, None, move |tx| {
            outcome(claim_lease(tx, &caller, &new, now))
        })
    }

    pub(super) fn renew(
        &self,
        task: String,
        lineage: String,
        epoch: u64,
    ) -> crate::sqlite::Result<WriteReceipt<StoreResult<Lease>>> {
        let caller = self.caller.clone();
        let now = self.clock.now_ms();
        self.writer.submit(Lane::Claims, None, move |tx| {
            outcome(renew_lease(tx, &caller, &task, &lineage, epoch, now))
        })
    }

    pub(super) fn release(
        &self,
        task: String,
        lineage: String,
        epoch: u64,
    ) -> crate::sqlite::Result<WriteReceipt<StoreResult<Lease>>> {
        let caller = self.caller.clone();
        self.writer.submit(Lane::Claims, None, move |tx| {
            outcome(release_lease(tx, &caller, &task, &lineage, epoch))
        })
    }

    pub(super) fn snapshot<T>(
        &self,
        deadline: Instant,
        read: impl FnOnce(&Transaction<'_>, &Caller) -> Result<T>,
    ) -> crate::sqlite::Result<StoreResult<T>> {
        self.readers
            .read(deadline, |tx| outcome(read(tx, &self.caller)))
    }
}
