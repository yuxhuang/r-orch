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

use orch_engine::container::OperationBoundary;
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::frame::{Store, limits};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Semaphore, SemaphorePermit, broadcast};

pub type OpResult<T> = Result<T, ErrorBody>;
pub type OpFuture<'p, T> = Pin<Box<dyn Future<Output = OpResult<T>> + Send + 'p>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Ordinary,
    Control,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed<T> {
    pub value: T,
    pub commit_seq: u64,
}

/// The outer operation's context. Nested service operations reuse the engine
/// task-local context, so there is only one state permit, including for ticks.
pub struct OpPermit {
    boundary: Arc<OperationBoundary>,
}

impl OpPermit {
    /// Call before a potentially partial write. The coarse subscriber seam
    /// invalidates all views; the push hub may refine this by domain events.
    pub fn invalidate(&mut self, store: Store) {
        self.boundary.note_store(crate::hub::engine_store(store));
    }
}

pub struct ProjectCoordinator {
    boundary: Arc<OperationBoundary>,
    ordinary: Semaphore,
    bytes: Semaphore,
    stopping: AtomicBool,
}

impl Default for ProjectCoordinator {
    fn default() -> Self {
        Self::new(Arc::new(OperationBoundary::new()))
    }
}

/// Admission capacity, independent of the state permit. Phased work may hold
/// this across external preparation/observation while the state gate is free.
pub struct Admission<'a> {
    _slot: Option<SemaphorePermit<'a>>,
    _bytes: Option<SemaphorePermit<'a>>,
}

impl ProjectCoordinator {
    /// Use the engine's EventBus::operation_boundary(), never a separate gate
    /// for the same project. Internal work shares it without RPC admission.
    pub fn new(boundary: Arc<OperationBoundary>) -> Self {
        Self {
            boundary,
            ordinary: Semaphore::new(limits::MAX_QUEUED_OPERATIONS),
            bytes: Semaphore::new(limits::MAX_QUEUED_REQUEST_BYTES),
            stopping: AtomicBool::new(false),
        }
    }

    pub fn stop_admission(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn admit(&self, lane: Lane, bytes: usize) -> OpResult<Admission<'_>> {
        if self.is_stopping() && lane == Lane::Ordinary {
            return Err(ErrorBody::new(
                ErrorCode::ShuttingDown,
                "daemon is stopping",
                Execution::NotExecuted,
            ));
        }
        if lane != Lane::Ordinary {
            return Ok(Admission {
                _slot: None,
                _bytes: None,
            });
        }
        let slot = self.ordinary.try_acquire().map_err(|_| overloaded())?;
        let count = u32::try_from(bytes).map_err(|_| overloaded())?;
        let bytes = self
            .bytes
            .try_acquire_many(count)
            .map_err(|_| overloaded())?;
        Ok(Admission {
            _slot: Some(slot),
            _bytes: Some(bytes),
        })
    }

    pub async fn run_op<T, F>(&self, lane: Lane, op: F) -> OpResult<Committed<T>>
    where
        T: Send,
        F: for<'p> FnOnce(&'p mut OpPermit) -> OpFuture<'p, T> + Send,
    {
        self.run_op_sized(lane, 0, op).await
    }

    /// Admission owns both a slot and the encoded request's byte budget until
    /// completion. Control/internal work does not consume ordinary capacity.
    /// Socket dispatch must own the task independently of its observer.
    pub async fn run_op_sized<T, F>(
        &self,
        lane: Lane,
        bytes: usize,
        op: F,
    ) -> OpResult<Committed<T>>
    where
        T: Send,
        F: for<'p> FnOnce(&'p mut OpPermit) -> OpFuture<'p, T> + Send,
    {
        let _admission = self.admit(lane, bytes)?;
        let started = AtomicBool::new(false);
        let boundary = Arc::clone(&self.boundary);
        let work = self.boundary.run(false, async {
            started.store(true, Ordering::SeqCst);
            let mut permit = OpPermit { boundary };
            op(&mut permit).await
        });
        tokio::pin!(work);
        let outcome = if lane == Lane::Ordinary {
            tokio::select! {
                biased;
                result = &mut work => result,
                () = tokio::time::sleep(limits::ADMISSION_TIMEOUT) => {
                    if !started.load(Ordering::SeqCst) { return Err(overloaded()); }
                    work.await
                }
            }
        } else {
            work.await
        };
        outcome.value.map(|value| Committed {
            value,
            commit_seq: outcome.commit_seq,
        })
    }

    pub fn current_commit_seq(&self) -> u64 {
        self.boundary.current_commit_seq()
    }
    pub fn subscribe_invalidations(&self) -> broadcast::Receiver<u64> {
        self.boundary.subscribe_commits()
    }
}

fn overloaded() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::Overloaded,
        "daemon operation queue is full or admission timed out",
        Execution::NotExecuted,
    )
}
