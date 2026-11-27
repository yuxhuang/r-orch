//! Fixed-size read-only snapshot pool. It borrows the locked data directory;
//! no connection can escape a callback or outlive its exclusive directory guard.
//!
//! Every read reports to the shared `FailureState` (R140): a fatal result on any
//! pooled connection fails the writer at once, and a read whose callback
//! observed one returns the refusal instead of its partial result.
use super::writer::failure::{FailureSource, FailureState};
use super::{ConnectionHandle, ConnectionRole, Result, SqliteError, Synchronous, open_connection};
use crate::data_dir::DataDir;
use rusqlite::Transaction;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

pub struct ReadPool<'a> {
    data: &'a DataDir,
    sync: Synchronous,
    // Each vector element is one FREE slot; None is a slot whose failed
    // connection must be reopened, not permission to grow the pool.
    free: Mutex<Vec<Option<ConnectionHandle<'a>>>>,
    available: Condvar,
    state: FailureState,
}

impl<'a> ReadPool<'a> {
    pub const DEFAULT_CONNECTIONS: usize = 4;

    /// Open only after schema setup. Size is storage.read_connections; never
    /// open an extra connection when all configured slots are checked out.
    pub fn open(data: &'a DataDir, sync: Synchronous, size: usize) -> Result<Self> {
        Self::open_with_state(data, sync, size, FailureState::new())
    }

    /// Share the writer's `FailureState`, so that a fatal read fails the writer.
    pub fn open_with_state(
        data: &'a DataDir,
        sync: Synchronous,
        size: usize,
        state: FailureState,
    ) -> Result<Self> {
        if size == 0 {
            return Err(SqliteError::InvalidCapacity);
        }
        let mut free = Vec::new();
        for _ in 0..size {
            free.push(Some(open_connection(data, ConnectionRole::Reader, sync)?));
        }
        Ok(Self {
            data,
            sync,
            free: Mutex::new(free),
            available: Condvar::new(),
            state,
        })
    }

    // Install test SQL hooks before the owned workers borrow the pool. The
    // production path exposes neither mutable connections nor hook callbacks.
    #[cfg(test)]
    pub(super) fn configure_test_readers(&self, setup: impl Fn(&mut rusqlite::Connection)) {
        for connection in self.free.lock().unwrap().iter_mut().flatten() {
            setup(connection.for_migration());
        }
    }

    /// The state this pool reports to. A callback captures it to report what it catches.
    pub fn failure_state(&self) -> &FailureState {
        &self.state
    }

    /// Admission waits only until the supplied deadline. The deferred read
    /// transaction pins a snapshot at its first statement through its last.
    /// Callbacks are trusted adapter code, never untrusted SQL or user code.
    pub fn read<T>(
        &self,
        deadline: Instant,
        read: impl FnOnce(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut lease = self.acquire(deadline)?;
        let observed = self.state.fatal_observations();
        let result = (|| {
            if lease.connection.is_none() {
                lease.connection = Some(open_connection(
                    self.data,
                    ConnectionRole::Reader,
                    self.sync,
                )?);
            }
            let transaction = lease
                .connection
                .as_mut()
                .expect("read slot opened")
                .transaction()?;
            let value = read(&transaction)?;
            transaction.commit()?;
            Ok(value)
        })();
        if let Err(error) = &result {
            // The error is classified here, before the caller converts it. The
            // connection is opened afresh on its next use, fatal or not.
            self.state.observe(FailureSource::Reader, error);
            lease.connection.take();
        }
        // A fatal observation since this read began, its own callback's or a
        // concurrent one: a value that the callback returned may come from
        // damaged data, so none is returned. A read's own error stays its answer.
        let result = match result {
            Ok(value) if self.state.fatal_observations() != observed => {
                match self.state.ensure_available() {
                    Err(refusal) => Err(refusal),
                    Ok(()) => Ok(value),
                }
            }
            other => other,
        };
        self.state.pool_read_finished(result.as_ref().err());
        result
    }

    fn acquire(&self, deadline: Instant) -> Result<ReadLease<'_, 'a>> {
        let mut free = self.free.lock().expect("read pool lock poisoned");
        loop {
            // A late woken waiter must consume a released slot rather than
            // swallow the sole wakeup and leave other waiters asleep.
            if let Some(connection) = free.pop() {
                return Ok(ReadLease {
                    pool: self,
                    connection,
                });
            }
            if Instant::now() >= deadline {
                return Err(SqliteError::Overloaded);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            (free, _) = self
                .available
                .wait_timeout(free, remaining)
                .expect("read pool lock poisoned");
        }
    }
}

struct ReadLease<'p, 'a> {
    pool: &'p ReadPool<'a>,
    connection: Option<ConnectionHandle<'a>>,
}

impl Drop for ReadLease<'_, '_> {
    fn drop(&mut self) {
        // Includes unwinding: a failed implicit rollback must never return an
        // open transaction to the next reader.
        if self
            .connection
            .as_ref()
            .is_some_and(|conn| !conn.is_autocommit())
        {
            self.connection.take();
        }
        self.pool
            .free
            .lock()
            .expect("read pool lock poisoned")
            .push(self.connection.take());
        self.pool.available.notify_one();
    }
}

#[cfg(test)]
mod tests;
