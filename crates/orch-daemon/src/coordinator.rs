//! The single state-operation boundary (design D2, spec R9–R12/R30).
//!
//! Every reader and writer of project state — RPC handlers, dispatch ticks,
//! process collectors, peer callbacks, definition edits and context expiry —
//! runs its **complete** read/validate/modify/save as one [`run_op`] call. The
//! semantic unit is the whole operation, not a store method.
//!
//! Contract the implementation (S1b, senior-eng-2) must keep:
//! - Only the outer operation acquires the permit; nested service calls reuse
//!   the [`OpPermit`] instead of re-acquiring.
//! - Lock order: lifecycle/control intent → project permit → orchestrator
//!   state → collector/cancel locks → store/definition/index locks.
//! - Never hold the permit across a human editor, a remote reply, an agent
//!   lifetime, process exit or a slow socket consumer: use
//!   prepare → external work → revalidate/commit as separate operations.
//! - Invalidations are published after the commit and the permit release;
//!   events emitted inside an operation are buffered until then.
//! - Permits are owned by daemon tasks, never by a socket reader, so a client
//!   disconnect cannot abort a half-completed write.
//! - [`Lane::Control`] and [`Lane::Internal`] keep capacity when the ordinary
//!   queue is full, so stop/cancel and completions are never starved.
//!
//! [`run_op`]: ProjectCoordinator::run_op

use orch_proto::error::ErrorBody;
use std::future::Future;
use std::pin::Pin;

/// The result of a coordinated operation, as handlers return it.
pub type OpResult<T> = Result<T, ErrorBody>;

/// The future an operation body returns; it may borrow the permit.
pub type OpFuture<'p, T> = Pin<Box<dyn Future<Output = OpResult<T>> + Send + 'p>>;

/// Admission lane of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Client RPCs: bounded by the admission queue and byte budget
    /// (`orch_proto::frame::limits`), `not_executed` when full.
    Ordinary,
    /// Stop, `task.cancel`, `dispatch.set`: reserved capacity.
    Control,
    /// Engine-internal completions, ticks, collectors, peer callbacks and
    /// expiry: reserved capacity, never rejected for client saturation.
    Internal,
}

/// A committed operation's value and its commit ordinal: the ordinal assigned
/// to this mutation, or the one a read observed. Monotonic per daemon
/// instance, in memory only (it is T0's `write_seq`, not a stored revision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed<T> {
    pub value: T,
    pub commit_seq: u64,
}

/// Proof that the holder is inside the project operation. Handlers reach the
/// engine services through it; there is no other path to the stores.
pub struct OpPermit {
    _private: (),
}

/// The per-project coordinator. One instance per daemon, shared by every
/// writer.
pub struct ProjectCoordinator {
    _private: (),
}

impl ProjectCoordinator {
    /// Run one complete operation under the project permit. `op` receives the
    /// permit for its whole duration; the return value is committed with an
    /// ordinal, and invalidations are published after the permit is released.
    pub async fn run_op<T, F>(&self, lane: Lane, op: F) -> OpResult<Committed<T>>
    where
        T: Send,
        F: for<'p> FnOnce(&'p mut OpPermit) -> OpFuture<'p, T> + Send,
    {
        // S1b (senior-eng-2) implements the permit, queue and commit ordinal.
        let _ = (lane, op);
        Err(ErrorBody::unimplemented("ProjectCoordinator::run_op"))
    }

    /// The last assigned commit ordinal, for subscription baselines (D9).
    pub fn current_commit_seq(&self) -> u64 {
        // S1b (senior-eng-2).
        0
    }
}
