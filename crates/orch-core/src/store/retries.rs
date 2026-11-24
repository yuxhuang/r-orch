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
//! rule (`now >= due_at`). Due rows come back earliest deadline first, ties by
//! task id. That is a read order: it is stable, so a repeated read of an
//! unchanged table gives the same rows, and it is not a dispatch policy.
//!
//! What 1.0 did, for the code that maps or replaces it (T12, T14, T21):
//!
//! - The daemon never drained the queue in queue order. Only the test-only
//!   `process_retry_queue`, which does not run in daemon mode, did. In daemon
//!   mode retrying tasks are dispatched in ordinary candidate order and a row
//!   only holds its task back while `now < due_at`. Dispatch order and agent
//!   rotation stay with the scheduler's candidate order, not with this read.
//! - A `due_at` that did not parse never held a task back: the daemon treated
//!   it as already due, so a mapping must read it as due, not as never due.
//! - A second enqueue for a task that already had a row was ignored, so the
//!   first row stayed. [`RetryWrite::put_retry`] replaces. A consumer that
//!   needs 1.0's rule calls [`RetryWrite::get_retry`] first.
//!
//! Every row, future ones included, is read by [`RetryRead::read_retries`]:
//! the same order as the due read (deadline, then task id, task ids compared
//! as bytes) over all rows rather than the due ones, in pages of at most
//! [`MAX_DUE_RETRIES`] after a [`RetryCursor`]. That is the read for
//! `state.get`, recovery and export. Pages of one [`ReadSnapshot`] come from
//! one committed state, so a walk of them sees each row once and no row
//! committed since the snapshot was taken, however many pages it takes. A
//! cursor is only the key of the last row returned, so it also stays usable
//! when a later snapshot no longer holds that row; such a walk is not
//! atomic, because rows written meanwhile appear or vanish with their keys.
//!
//! [`ReadSnapshot`]: super::ReadSnapshot

use async_trait::async_trait;

use super::Result;

#[cfg(feature = "test-support")]
pub mod fake;

/// The most rows one [`RetryRead::read_due_retries`] or
/// [`RetryRead::read_retries`] call returns.
pub const MAX_DUE_RETRIES: usize = 1_000;
/// The largest inline `error`, in UTF-8 bytes. Larger diagnostics are stored
/// through the blob port and returned whole; this is not a diagnostic limit.
pub const MAX_RETRY_ERROR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub task: String,
    /// At least 1.
    pub attempt: u32,
    /// UTC milliseconds, matching `Clock::now_ms()`; not before the epoch.
    pub due_at: i64,
    /// Complete diagnostic, including when no run survives; may be empty.
    /// Above [`MAX_RETRY_ERROR_BYTES`], publish the complete UTF-8 bytes before
    /// storing their hash. Unresolvable references fail reads, never truncate.
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
        Ok(())
    }
}

/// Where a page of the complete read stops: the sort key of the last row it
/// returned, which the next page starts after. The key is the whole position,
/// so a cursor needs no row to still exist and holds no offset or snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryCursor {
    /// UTC milliseconds, as [`Retry::due_at`]; not before the epoch.
    pub due_at: i64,
    pub task: String,
}

impl RetryCursor {
    /// The cursor that continues after `retry`.
    pub fn after(retry: &Retry) -> Self {
        Self {
            due_at: retry.due_at,
            task: retry.task.clone(),
        }
    }

    /// The checks every backend applies before it reads anything. A key no
    /// row can have is a bad cursor, not an empty rest of the table.
    pub fn validate(&self) -> Result<()> {
        if self.task.is_empty() || self.due_at < 0 {
            return Err(super::StoreError::InvalidCursor);
        }
        Ok(())
    }
}

/// One page of [`RetryRead::read_retries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPage {
    pub rows: Vec<Retry>,
    /// `Some` exactly when at least one row follows `rows`: a walk that ends on
    /// `None` never ends with an empty page, even at an exact multiple of the limit.
    pub next: Option<RetryCursor>,
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
    /// Every row, due or not, earliest deadline first, ties by task id. Starts
    /// after `after` (exclusive) or at the first row, and returns at most
    /// `limit` rows, which must be in `1..=MAX_DUE_RETRIES`. The cursor must
    /// pass [`RetryCursor::validate`]. Another project's rows are not in it.
    async fn read_retries(&self, after: Option<&RetryCursor>, limit: usize) -> Result<RetryPage>;
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
    /// As [`RetryRead::read_retries`], within this transaction's own writes.
    /// A walk that writes between its pages sees those writes, unlike a walk
    /// of one read snapshot.
    fn list_retries(&self, after: Option<&RetryCursor>, limit: usize) -> Result<RetryPage>;
}
