//! Trusted transaction executor, not a client-facing arbitrary SQL endpoint.
//! This independent foundation is not wired into `Storage` or the daemon yet.
//! Retry/fatal policy and bulk/event aggregation belong to the adapter integration.
use super::{ConnectionHandle, ConnectionRole, Result, SqliteError, Synchronous, open_connection};
use crate::data_dir::DataDir;
use rusqlite::Transaction;
use std::sync::{Arc, Condvar, Mutex, mpsc};
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
        self.shared
            .state
            .lock()
            .expect("writer queue lock poisoned")
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
/// are checked by the worker before BEGIN, so an expired queued request cannot
/// later commit. A deadline does not interrupt a transaction already running.
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

    /// Drain accepted work after `WriterHandle::close`. A callback panic rolls
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
            let mut state = self
                .shared
                .state
                .lock()
                .expect("writer queue lock poisoned");
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
            let result = if deadline.is_some_and(|limit| Instant::now() >= limit) {
                Err(SqliteError::Overloaded)
            } else {
                (|| {
                    // T9 configures this connection for BEGIN IMMEDIATE.
                    let transaction = connection.transaction()?;
                    let value = work(&transaction)?;
                    transaction.commit()?;
                    Ok(value)
                })()
            };
            let _ = response.send(result);
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
