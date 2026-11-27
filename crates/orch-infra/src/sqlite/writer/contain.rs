//! Containment of a panic in a callback (spec Amendment 3, issue 379).
//!
//! Two kinds of unwind can leave a callback, and the writer must tell them apart:
//!
//! - **A callback's own panic** is the caller's bug. Nothing it did is committed,
//!   the writer's thread survives and serves what follows, no cause is latched, and
//!   the awaiting caller resumes the panic ([`SqliteError::CallbackPanicked`]).
//! - **A leaf's failed savepoint cleanup** is adapter code that unwinds from inside
//!   the callback that called it. A leaf raises it with [`cleanup_failed`] when
//!   `ROLLBACK TO` or `RELEASE` of its own savepoint fails: the savepoint's writes
//!   are then still in the outer transaction, and a callback that caught an error
//!   could go on and commit them. The unwind carries both errors. [`contain`]
//!   reports them to the failure state first, so `SQLITE_CORRUPT` or
//!   `SQLITE_NOTADB` latch even though the outer rollback may succeed, and then
//!   raises the unwind again: `Writer::run` keeps its documented behaviour for it
//!   (the worker is dropped, the connection is never served again, the running and
//!   every queued receipt are closed, `storage_unavailable` when a cause latched
//!   and `WriterClosed` when none did, and nothing later is admitted).
//!
//! A single `catch_unwind` around a callback cannot tell the two apart, which is
//! why the leaf's unwind has a type. A callback is trusted not to swallow an unwind
//! of its own (the same trust that lets it catch an error and report it with
//! `FailureState::observe`, `writer.rs`).
use super::failure::{FailureSource, FailureState};
use crate::sqlite::{Result, SqliteError};
use std::any::Any;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any, resume_unwind};
use std::sync::{Mutex, PoisonError};

/// The payload of the unwind of a leaf whose savepoint cleanup failed: the
/// operation's error and the cleanup's, as the failure state classifies a failed
/// savepoint rollback (`SqliteError::OperationRollback`).
pub(in crate::sqlite) struct CleanupUnwind(SqliteError);

#[cfg(test)]
thread_local! {
    static CLEANUP_ERROR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
pub(in crate::sqlite) fn set_cleanup_error(code: i32) {
    CLEANUP_ERROR.set(Some(code));
}

/// A leaf's savepoint cleanup failed after `operation` failed. Never returns an
/// error a callback could catch and commit past: it unwinds with a
/// [`CleanupUnwind`], which [`contain`] observes and re-raises.
pub(in crate::sqlite) fn cleanup_failed(
    operation: impl Into<SqliteError>,
    cleanup: rusqlite::Error,
) -> ! {
    #[cfg(test)]
    let cleanup = CLEANUP_ERROR.take().map_or(cleanup, |code| {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    });
    let error = SqliteError::OperationRollback {
        operation: Box::new(operation.into()),
        rollback: cleanup,
    };
    // The default panic hook prints a typed payload as "Box<dyn Any>".
    tracing::error!(%error, "SQLite savepoint cleanup failed; the transaction cannot commit");
    panic_any(CleanupUnwind(error))
}

#[cfg(test)]
impl CleanupUnwind {
    pub(in crate::sqlite) fn error(&self) -> &SqliteError {
        &self.0
    }
}

/// A callback's own panic, held until the awaiting caller resumes it. The answer
/// of a request whose callback panicked, in place of a result.
pub struct CallbackPanic(Mutex<Option<Box<dyn Any + Send>>>);

impl CallbackPanic {
    fn new(payload: Box<dyn Any + Send>) -> Self {
        Self(Mutex::new(Some(payload)))
    }

    /// The panic's payload, once. `std::panic::resume_unwind` it on the caller.
    pub fn take(&self) -> Option<Box<dyn Any + Send>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl fmt::Debug for CallbackPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The payload is the callback's own and may hold anything: name only its kind.
        f.write_str("CallbackPanic")
    }
}

/// Run a callback so that its own panic is contained and a leaf's failed cleanup is not.
///
/// A callback's own panic is returned as `CallbackPanicked`: the request's transaction
/// then rolls back like any other error, and the writer goes on. A [`CleanupUnwind`]
/// is observed through `state` (a fatal cause latches) and raised again.
/// Storage uses this only around user `WriteWork`; adapter setup and completion
/// must remain outside the catch.
pub(in crate::sqlite) fn contain<T>(
    state: &FailureState,
    callback: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(result) => result,
        Err(payload) => match payload.downcast::<CleanupUnwind>() {
            Ok(unwind) => {
                state.observe(FailureSource::Writer, &unwind.0);
                // `resume_unwind` runs no panic hook: the unwind goes on as it came.
                resume_unwind(unwind)
            }
            Err(payload) => Err(SqliteError::CallbackPanicked(CallbackPanic::new(payload))),
        },
    }
}
