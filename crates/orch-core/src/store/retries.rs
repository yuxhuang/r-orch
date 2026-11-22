//! The retry table (R35, R36, R37, R40; ADR 0017, AP10).
//!
//! One row per task, kept apart from the task so that a finish writes the next
//! status and the retry row in one transaction (R36) and a start or a cancel
//! removes the row in the same one (R35, R37). Unlike 1.0's `state.json` queue
//! there is no cap and nothing is dropped for lack of room: a row leaves only
//! when [`RetryWrite::delete_retry`] is called or [`RetryWrite::put_retry`]
//! replaces it. `attempt` is not interpreted here.
//!
//! Like the rest of this port the operations do not authorize and do not
//! fence: the dispatcher decides who may call, and a retry row is a task's,
//! not a lease's, so none of them reads or changes a lease. Rows are per
//! project; another project's row is simply absent.
//!
//! Deadlines are UTC milliseconds from the caller's `Clock::now_ms()`; the
//! port reads no clock. A due row is one with `due_at <= now_ms`, the 1.0
//! rule (`now >= due_at`). Due rows come back in the order the scheduler
//! should take them: earliest deadline first, ties by task id. 1.0 took them
//! in queue order, which the ordering by deadline replaces; the order is
//! stable, so a repeated read of an unchanged table gives the same rows.

use async_trait::async_trait;

use super::Result;

#[cfg(feature = "test-support")]
pub mod fake;

/// The most rows one [`RetryRead::read_due_retries`] call returns.
pub const MAX_DUE_RETRIES: usize = 1_000;
/// The longest `error` a row may hold, in bytes (text up to 64 KiB, decision 7).
pub const MAX_RETRY_ERROR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub task: String,
    /// At least 1.
    pub attempt: u32,
    /// UTC milliseconds, matching `Clock::now_ms()`; not before the epoch.
    pub due_at: i64,
    /// What failed; may be empty. At most [`MAX_RETRY_ERROR_BYTES`].
    pub error: String,
}

impl Retry {
    /// The checks every backend applies before it changes anything.
    pub fn validate(&self) -> Result<()> {
        use super::StoreError::InvalidRetry;
        if self.task.is_empty() {
            return Err(InvalidRetry("the task id is empty".into()));
        }
        if self.attempt == 0 {
            return Err(InvalidRetry("attempts start at 1".into()));
        }
        if self.due_at < 0 {
            return Err(InvalidRetry("the deadline is before the epoch".into()));
        }
        if self.error.len() > MAX_RETRY_ERROR_BYTES {
            return Err(InvalidRetry(format!(
                "the error is over {MAX_RETRY_ERROR_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

/// `limit` must be in `1..=MAX_DUE_RETRIES`.
pub fn check_limit(limit: usize) -> Result<()> {
    if (1..=MAX_DUE_RETRIES).contains(&limit) {
        Ok(())
    } else {
        Err(super::StoreError::InvalidRetry(format!(
            "the limit must be in 1..={MAX_DUE_RETRIES}"
        )))
    }
}

/// Retries as of the enclosing read snapshot.
#[async_trait]
pub trait RetryRead: Send + Sync {
    async fn read_retry(&self, task: &str) -> Result<Option<Retry>>;
    /// Rows with `due_at <= now_ms`, earliest first, ties by task id.
    async fn read_due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>>;
}

/// Operations share the enclosing unit of work, so a retry row commits or
/// rolls back with the task status and the run the engine writes beside it.
pub trait RetryWrite: Send {
    /// Inserts the row, or replaces the task's row whole. Refused rows change
    /// nothing, and a refusal caught by the caller still leaves nothing behind.
    fn put_retry(&mut self, retry: &Retry) -> Result<()>;
    /// Removes the task's row; true if there was one. A missing row is not an
    /// error, so a start and a cancel may both call it.
    fn delete_retry(&mut self, task: &str) -> Result<bool>;
    fn get_retry(&self, task: &str) -> Result<Option<Retry>>;
    /// As [`RetryRead::read_due_retries`], within this transaction's own writes.
    fn due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>>;
}
