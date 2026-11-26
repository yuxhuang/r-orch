//! Fenced lease operations (R35, R40, R132; ADR 0017, AP11, AP12).
//!
//! A lease is the pair of a task and an agent for one run. It is fenced by the
//! daemon's restore lineage and the task's epoch: a call that carries either
//! value other than the lease's is refused as [`StoreError::StaleEpoch`] and
//! changes nothing. Epochs only grow, per project and task, and outlive the
//! lease and the task row, so a stale token never matches a later claim.
//!
//! Rules taken from the prototype (`prototypes/fenced-lease`): the claim is the
//! only arbiter, so a task holds at most one lease and an agent at most one;
//! a refused claim consumes no epoch; release keeps the epoch counter.
//!
//! Rules that differ from the prototype on purpose. The prototype's holder
//! stops itself at a deadline and its claim expires what is past it. Release 1.1
//! has no such holder (AP12): after a suspend the daemon renews the runs it
//! still supervises before it expires any, so neither renew nor claim reads the
//! deadline. `expires_at` is stored for the expiry job (T15), which ends a
//! lease through [`LeaseWrite::release_lease`]. Sessions, reattachment, restart
//! and restore recovery belong to the generation work (T13, T23).
//!
//! Like the rest of this port the operations do not authorize: the dispatcher
//! decides who may call. What they check is that a task principal is the
//! holder of the lease it touches. Leases are per project, so a lease of
//! another project is simply absent.

use async_trait::async_trait;

use super::Result;

#[cfg(feature = "test-support")]
pub mod fake;
#[cfg(all(test, feature = "test-support"))]
mod tests;

/// Milliseconds added to the caller's UTC clock by a claim or renewal.
pub const LEASE_MS: i64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub task: String,
    pub agent: String,
    pub run: String,
    pub lineage: String,
    pub epoch: u64,
    /// UTC milliseconds, matching `Clock::now_ms()`; the port reads no clock.
    pub expires_at: i64,
}

/// The lineage and the epoch are given by the backend, never by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLease {
    pub task: String,
    pub agent: String,
    pub run: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Granted(Lease),
    /// The lease in the way, found in this order: the task's, the agent's,
    /// the one already naming this run. Nothing was changed, no epoch used.
    Held(Lease),
}

/// Leases as of the enclosing read snapshot.
#[async_trait]
pub trait LeaseRead: Send + Sync {
    async fn read_lease(&self, task: &str) -> Result<Option<Lease>>;
    async fn read_agent_lease(&self, agent: &str) -> Result<Option<Lease>>;
    /// Ordered by task id. Bounded by the number of agents, so not paged.
    async fn read_leases(&self) -> Result<Vec<Lease>>;
}

/// Operations share the enclosing unit of work, so a claim commits or rolls
/// back with the run row and the task status the engine writes beside it.
pub trait LeaseWrite {
    /// Grants a lease only when the task has none and the agent has none.
    /// `now_ms` is from `Clock::now_ms()`; expiry is checked `now_ms + LEASE_MS`.
    fn claim_lease(&mut self, new: &NewLease, now_ms: i64) -> Result<ClaimOutcome>;
    /// Moves `expires_at` to `now_ms + LEASE_MS`, whether or not the old one has
    /// passed (AP12). Refused as stale unless `lineage` and `epoch` are the
    /// lease's and the backend's current lineage.
    fn renew_lease(&mut self, task: &str, lineage: &str, epoch: u64, now_ms: i64) -> Result<Lease>;
    /// Ends the lease and returns it. The task's epoch counter is kept.
    fn release_lease(&mut self, task: &str, lineage: &str, epoch: u64) -> Result<Lease>;
    fn get_lease(&self, task: &str) -> Result<Option<Lease>>;
    fn get_agent_lease(&self, agent: &str) -> Result<Option<Lease>>;
    fn list_leases(&self) -> Result<Vec<Lease>>;
}
