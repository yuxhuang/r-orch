//! Internal typed forwarding only; not a Storage facade or daemon entry point.
use super::*;
use crate::sqlite::{Lane, ReadPool, WriteReceipt, WriterHandle};
use orch_core::Clock;
use orch_core::store::Lane as PortLane;
use orch_core::store::Result as StoreResult;
use std::time::Instant;

/// The one place that names how a port lane (`orch_core::store::Lane`, what a
/// storage caller asks for) becomes a writer lane (`sqlite::Lane`, what the
/// writer schedules). Four lanes, one for one, with no default arm: a lane
/// added on either side must be mapped here before this compiles.
pub(super) const fn writer_lane(lane: PortLane) -> Lane {
    match lane {
        PortLane::Claims => Lane::Claims,
        PortLane::Entities => Lane::Entities,
        PortLane::Events => Lane::Events,
        PortLane::Maintenance => Lane::Maintenance,
    }
}

/// Lane of a lease change submitted on its own: a claim, a renewal and a
/// release. Spec "Writer lanes", lane 1: "Claims, renewals, releases, lease
/// expiry". The start of a run (R35, which composes `claim_lease`) is a Claims
/// transaction as well. The writer takes a queued Claims job before every other
/// lane (R26), so a lease change does not wait behind entity or event work.
pub(super) const LEASE_LANE: Lane = writer_lane(PortLane::Claims);

/// Lane of a job that releases a lease as one step of a larger change: the
/// finish of a run (R36), a cancel (R37), a pause (R38) and a permanent failure
/// (R39). The spec's table of transactions puts each on Entities, and the
/// release inside it is `release_lease` on that job's transaction, not a second
/// submission. Such a job must be submitted on this lane and not on
/// `LEASE_LANE`: lane 1 is bounded by the number of tasks and agents, and a
/// multi-entity finish would break that bound.
pub(super) const COMPOSED_RELEASE_LANE: Lane = writer_lane(PortLane::Entities);

/// Typed refusals stay inner outcomes. A SQL failure, or damaged stored data,
/// leaves the job as `Err`, so the executor rolls the transaction back, never
/// commits it, and can classify the SQLite code instead of a backend string.
fn outcome<T>(result: Result<T>) -> crate::sqlite::Result<StoreResult<T>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(LeaseError::Store(error)) => Ok(Err(error)),
        Err(error @ (LeaseError::Sql(_) | LeaseError::Corrupt(_))) => Err(error.into()),
    }
}

/// Caller is bound once by trusted adapter code; authorization remains at the
/// dispatcher. No request may supply SQL or change this adapter's project.
///
/// Each submission takes the R128 request `deadline`. A request still queued
/// at its deadline, or still waiting for `BEGIN IMMEDIATE` at it, is answered
/// `SqliteError::Overloaded` without running and cannot commit afterwards. A
/// deadline does not interrupt a transaction that has started. `None` is for
/// the daemon's own work (spec "When a transaction fails": "the daemon's own
/// work (finishes, lease changes, events) stays queued"). The clock is read
/// when the request is submitted, not when it runs, so a request that waited
/// gets an earlier expiry than one run at once: the conservative side.
pub(super) struct LeaseAdapter<'a, 'data> {
    pub(super) writer: &'a WriterHandle,
    pub(super) readers: &'a ReadPool<'data>,
    pub(super) caller: Caller,
    pub(super) clock: &'a dyn Clock,
}

impl LeaseAdapter<'_, '_> {
    pub(super) fn claim(
        &self,
        deadline: Option<Instant>,
        new: NewLease,
    ) -> crate::sqlite::Result<WriteReceipt<StoreResult<ClaimOutcome>>> {
        let caller = self.caller.clone();
        let now = self.clock.now_ms();
        self.writer.submit(LEASE_LANE, deadline, move |tx| {
            outcome(claim_lease(tx, &caller, &new, now))
        })
    }

    pub(super) fn renew(
        &self,
        deadline: Option<Instant>,
        task: String,
        lineage: String,
        epoch: u64,
    ) -> crate::sqlite::Result<WriteReceipt<StoreResult<Lease>>> {
        let caller = self.caller.clone();
        let now = self.clock.now_ms();
        self.writer.submit(LEASE_LANE, deadline, move |tx| {
            outcome(renew_lease(tx, &caller, &task, &lineage, epoch, now))
        })
    }

    pub(super) fn release(
        &self,
        deadline: Option<Instant>,
        task: String,
        lineage: String,
        epoch: u64,
    ) -> crate::sqlite::Result<WriteReceipt<StoreResult<Lease>>> {
        let caller = self.caller.clone();
        self.writer.submit(LEASE_LANE, deadline, move |tx| {
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
