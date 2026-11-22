//! Trusted transaction executor, not a client-facing arbitrary SQL endpoint.
//! This independent foundation is not wired into `Storage` or the daemon yet.
//! Retry/fatal policy and bulk/event aggregation belong to the adapter integration.
//!
//! What this executor enforces: one writing connection, claims first and then the
//! earliest deadline, a bounded queue, a request deadline checked before BEGIN and
//! again once BEGIN IMMEDIATE holds the reservation, rollback of a failed or
//! panicking body, and admission closure on `close` or on dropping the last handle.
//!
//! What it does not enforce, and the adapter integration must (issue 127, R128,
//! R130, R140): a callback is trusted not to issue COMMIT, BEGIN, SAVEPOINT
//! control or external side effects, and nothing here detects that it did, or
//! that a failed rollback left the connection outside autocommit; a failed
//! `BEGIN`/commit/rollback is returned to its receipt, not classified, retried or
//! made a shared fatal state (`storage_unavailable`) that other connections can
//! also set; and the producer invariants of R26 (bounded claim production, event
//! coalescing) are the producer's. Blocking `WriteReceipt::wait` must not be called on
//! an async runtime thread.
use super::{ConnectionHandle, ConnectionRole, Result, SqliteError, Synchronous, open_connection};
use crate::data_dir::DataDir;
use rusqlite::Transaction;
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Claims,
    Entities,
    Events,
    Maintenance,
}

impl Lane {
    fn wait_budget(self) -> Duration {
        match self {
            Self::Claims => Duration::from_millis(50),
            Self::Entities => Duration::from_millis(200),
            Self::Events => Duration::from_secs(1),
            Self::Maintenance => Duration::from_secs(5),
        }
    }
}

struct Entry<T> {
    lane: Lane,
    deadline: Instant,
    work: T,
}

// A bounded vector keeps equal-deadline order stable and permits selecting a
// claim ahead of an overdue maintenance transaction. No separate lane can grow.
struct Schedule<T> {
    entries: Vec<Entry<T>>,
    capacity: usize,
}

impl<T> Schedule<T> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity,
        }
    }

    fn admits(&self, lane: Lane) -> bool {
        self.entries.len() < self.capacity
            && !(lane == Lane::Events && self.entries.iter().any(|e| e.lane == Lane::Events))
    }

    fn push(&mut self, lane: Lane, arrival: Instant, work: T) -> Result<()> {
        if !self.admits(lane) {
            return Err(SqliteError::Overloaded);
        }
        self.entries.push(Entry {
            lane,
            deadline: arrival + lane.wait_budget(),
            work,
        });
        Ok(())
    }

    fn pop(&mut self) -> Option<T> {
        let index = self
            .entries
            .iter()
            .position(|e| e.lane == Lane::Claims)
            .or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.deadline)
                    .map(|(i, _)| i)
            })?;
        Some(self.entries.remove(index).work)
    }
}

/// Run one admitted request. The deadline is read twice: before BEGIN, to skip
/// a request that expired in the queue, and again once BEGIN IMMEDIATE holds the
/// write reservation, because that statement can itself wait up to the busy
/// timeout. A request found expired at the second look is rolled back unrun and
/// answers `Overloaded`, so a request already refused at its deadline cannot
/// commit later (R128). The deadline still does not interrupt a body or commit
/// that has started. `now` is a seam so tests can place the deadline between
/// the two reads without timing luck.
fn execute<T>(
    connection: &mut ConnectionHandle<'_>,
    deadline: Option<Instant>,
    now: impl Fn() -> Instant,
    work: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let expired = || deadline.is_some_and(|limit| now() >= limit);
    if expired() {
        return Err(SqliteError::Overloaded);
    }
    // T9 configures this connection for BEGIN IMMEDIATE.
    let transaction = connection.transaction()?;
    if expired() {
        // Dropping the transaction rolls back and releases the reservation.
        return Err(SqliteError::Overloaded);
    }
    let value = work(&transaction)?;
    transaction.commit()?;
    Ok(value)
}

type Job = Box<dyn FnOnce(&mut ConnectionHandle<'_>) + Send>;

struct State {
    accepting: bool,
    queue: Schedule<Job>,
}

struct Shared {
    state: Mutex<State>,
    available: Condvar,
}

/// Owns the sole writing connection of this executor. Run it on a dedicated,
/// scoped thread: its borrow keeps the data directory lock alive through exit.
/// Close admission or drop every client handle before joining that thread. Construct one
/// executor per adapter after migrations, never one per request.
pub struct Writer<'a> {
    connection: ConnectionHandle<'a>,
    shared: Arc<Shared>,
}

#[derive(Clone)]
pub struct WriterHandle {
    admission: Arc<Admission>,
}

// Only client handles own Admission. The worker owns Shared separately, so
// dropping the final client closes admission even on an early return/unwind.
struct Admission {
    shared: Arc<Shared>,
}

impl Admission {
    fn close(&self) {
        // Runs from Drop, possibly during an unwind: a poisoned lock must not
        // become a second panic. No callback ever runs under this lock.
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .accepting = false;
        self.shared.available.notify_all();
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.close();
    }
}

/// Dropping a receipt does not cancel an accepted mutation. Request deadlines
/// are checked by the worker before BEGIN and again once BEGIN IMMEDIATE holds
/// the write reservation, so an expired request cannot later commit, even one
/// that waited on another connection's write lock. A deadline does not
/// interrupt a body or commit that has already started.
pub struct WriteReceipt<T> {
    response: mpsc::Receiver<Result<T>>,
}

impl<T> WriteReceipt<T> {
    pub fn wait(self) -> Result<T> {
        self.response
            .recv()
            .unwrap_or(Err(SqliteError::WriterClosed))
    }
}

impl<'a> Writer<'a> {
    pub fn open(
        data: &'a DataDir,
        sync: Synchronous,
        capacity: usize,
    ) -> Result<(Self, WriterHandle)> {
        if capacity == 0 {
            return Err(SqliteError::InvalidCapacity);
        }
        let connection = open_connection(data, ConnectionRole::Writer, sync)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                accepting: true,
                queue: Schedule::new(capacity),
            }),
            available: Condvar::new(),
        });
        let handle = WriterHandle {
            admission: Arc::new(Admission {
                shared: Arc::clone(&shared),
            }),
        };
        Ok((Self { connection, shared }, handle))
    }

    /// Drain accepted work, then return once admission is closed, either by
    /// `WriterHandle::close` or by dropping the final client handle. A callback panic rolls
    /// back its transaction and drops this worker, closing every queued receipt.
    /// SQL/body errors go to that receipt; this foundation does not replay work.
    pub fn run(mut self) {
        loop {
            let job = {
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .expect("writer queue lock poisoned");
                loop {
                    if let Some(job) = state.queue.pop() {
                        break Some(job);
                    }
                    if !state.accepting {
                        break None;
                    }
                    state = self
                        .shared
                        .available
                        .wait(state)
                        .expect("writer queue lock poisoned");
                }
            };
            match job {
                Some(job) => job(&mut self.connection),
                None => break,
            }
        }
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        let abandoned = {
            // Drop runs during the unwind of a panicking callback; see Admission::close.
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.accepting = false;
            std::mem::take(&mut state.queue.entries)
        };
        // A captured value's destructor must never run while holding admission.
        drop(abandoned);
        self.shared.available.notify_all();
    }
}

impl WriterHandle {
    /// Queue one trusted, atomic transaction. The adapter must validate caller
    /// and project before producing this closure. Capacity bounds transactions,
    /// not their captured bytes; bulk row/time bounds and event coalescing are
    /// the producer's responsibility. A second waiting event transaction is
    /// refused so producers retain its batch rather than grow a per-run queue.
    pub fn submit<T, F>(
        &self,
        lane: Lane,
        deadline: Option<Instant>,
        work: F,
    ) -> Result<WriteReceipt<T>>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    {
        let (response, receiver) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |connection| {
            let _ = response.send(execute(connection, deadline, Instant::now, work));
        });
        let mut state = self
            .admission
            .shared
            .state
            .lock()
            .expect("writer queue lock poisoned");
        if !state.accepting {
            // Drop user captures outside the queue lock, including on refusal.
            drop(state);
            drop(job);
            return Err(SqliteError::WriterClosed);
        }
        // Check capacity before moving the callback into push: refusal must not
        // drop a user capture under the lock (it might itself close admission).
        if !state.queue.admits(lane) {
            drop(state);
            drop(job);
            return Err(SqliteError::Overloaded);
        }
        state.queue.push(lane, Instant::now(), job)?;
        self.admission.shared.available.notify_one();
        Ok(WriteReceipt { response: receiver })
    }

    /// Close admission and drain previously accepted transactions. Idempotent.
    pub fn close(&self) {
        self.admission.close();
    }
}

#[cfg(test)]
mod tests;
