//! Internal typed forwarding only; not a Storage facade or daemon entry point.
use super::*;
use crate::sqlite::{Lane, ReadPool, WriteReceipt, WriterHandle};
use orch_core::Clock;
use std::time::Instant;

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
    ) -> crate::sqlite::Result<WriteReceipt<Result<ClaimOutcome>>> {
        let caller = self.caller.clone();
        let now = self.clock.now_ms();
        self.writer.submit(Lane::Claims, None, move |tx| {
            Ok(claim_lease(tx, &caller, &new, now))
        })
    }

    pub(super) fn renew(
        &self,
        task: String,
        lineage: String,
        epoch: u64,
    ) -> crate::sqlite::Result<WriteReceipt<Result<Lease>>> {
        let caller = self.caller.clone();
        let now = self.clock.now_ms();
        self.writer.submit(Lane::Claims, None, move |tx| {
            Ok(renew_lease(tx, &caller, &task, &lineage, epoch, now))
        })
    }

    pub(super) fn release(
        &self,
        task: String,
        lineage: String,
        epoch: u64,
    ) -> crate::sqlite::Result<WriteReceipt<Result<Lease>>> {
        let caller = self.caller.clone();
        self.writer.submit(Lane::Claims, None, move |tx| {
            Ok(release_lease(tx, &caller, &task, &lineage, epoch))
        })
    }

    pub(super) fn snapshot<T>(
        &self,
        deadline: Instant,
        read: impl FnOnce(&Transaction<'_>, &Caller) -> Result<T>,
    ) -> crate::sqlite::Result<Result<T>> {
        self.readers.read(deadline, |tx| Ok(read(tx, &self.caller)))
    }
}
