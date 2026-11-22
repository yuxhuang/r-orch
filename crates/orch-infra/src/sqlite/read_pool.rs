//! Fixed-size read-only snapshot pool. It borrows the locked data directory;
//! no connection can escape a callback or outlive its exclusive directory guard.
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
}

impl<'a> ReadPool<'a> {
    pub const DEFAULT_CONNECTIONS: usize = 4;

    /// Open only after schema setup. Size is storage.read_connections; never
    /// open an extra connection when all configured slots are checked out.
    pub fn open(data: &'a DataDir, sync: Synchronous, size: usize) -> Result<Self> {
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
        })
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
        if result.is_err() {
            // A pooled failure is returned to its caller. Reopen this slot on
            // its next use; shared fatal/error reporting is added by T10's
            // failure-policy slice, before this is connected to Storage.
            lease.connection.take();
        }
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
