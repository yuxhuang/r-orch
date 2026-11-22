//! Side-effect intents (R28, R37, R105, R106; ADR 0017 decision 10).
//!
//! An intent is a row written in the transaction that decides an effect outside
//! the database: starting or stopping a process, removing a workspace, deleting
//! a blob file. A performer, which is not in this port, does the effect after
//! the commit and then calls [`IntentWrite::finish_intent`]. The port records
//! and returns intents; it never signals, spawns, opens or removes anything.
//!
//! Payloads are closed and typed, and hold ids and hashes, never a path, so no
//! row can make a performer reach a file the daemon did not choose (R105,
//! R106). The checks here are the port's own and apply to both backends: one
//! name of `A-Z a-z 0-9 . _ -` that is not `.` or `..` and is at most
//! [`MAX_ID_BYTES`] long, or a 64-digit lowercase hex hash. Whether a named
//! directory really is that task's is R118's check, made by the performer.
//!
//! Times are UTC milliseconds given by the caller (`Clock::now_ms()`); the port
//! reads no clock. Ids start at 1 for each project, grow by one for each intent
//! and are never reused, not even after retention deleted every row, like the
//! epochs of [`super::leases`]. The refusal of an operation changes nothing,
//! even when the callback catches it and commits.
//!
//! Authority: like the rest of this port the operations do not authorize; the
//! dispatcher decides who may call. What they enforce is the lease boundary
//! (R132). `process.start` and `process.stop` name the run of a lease that
//! exists in the unit of work, under the current lineage, and a task principal
//! may name only its own run and only with its own token; otherwise the call is
//! refused with [`StoreError::StaleEpoch`], as a lease-bound call is. A restore
//! ends every lease, so it ends the right to add either; rows already written
//! stay. The lease is read when the intent is added, so a cancel adds the stop
//! intent before it releases the lease, in the same unit of work (R37).
//!
//! Which rows a restart performs again is the kind's rule
//! ([`IntentKind::repeats_after_restart`]). A pending `process.start` is never
//! performed again: recovery (R41, T13) ends or keeps its run and finishes the
//! intent. Intents of an earlier daemon generation are likewise recovery's to
//! judge; this port has no generation and keeps the rows as they were written.
//!
//! Retention: a finished intent is kept [`DONE_RETENTION_MS`] and then deleted
//! by [`IntentWrite::delete_finished_intents`], at most 1,000 rows a call.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{Result, StoreError};

#[cfg(feature = "test-support")]
pub mod fake;
#[cfg(test)]
mod tests;

/// A finished intent is kept this long, in milliseconds (7 days), then deleted.
pub const DONE_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
/// Longest run or task id a payload may name, in bytes.
pub const MAX_ID_BYTES: usize = 128;
/// Most rows one pending read returns and one retention call deletes.
pub const MAX_ROWS: usize = 1_000;

/// The four kinds of 1.1, named as the spec names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    ProcessStart,
    ProcessStop,
    WorkspaceRemove,
    BlobDelete,
}

impl IntentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessStart => "process.start",
            Self::ProcessStop => "process.stop",
            Self::WorkspaceRemove => "workspace.remove",
            Self::BlobDelete => "blob.delete",
        }
    }

    /// Whether a daemon that restarts with this kind pending performs it again.
    /// Only `process.start` is not: its process was created held and its pid is
    /// committed in a second transaction, so recovery already knows what exists.
    pub fn repeats_after_restart(self) -> bool {
        !matches!(self, Self::ProcessStart)
    }
}

/// What to do, by id or hash. Unknown kinds and fields are refused by serde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum IntentPayload {
    /// Written with the claim of the run's lease (R35).
    #[serde(rename = "process.start")]
    ProcessStart { run: String },
    /// Names the run of a lease; the process group comes from the run's `local` data.
    #[serde(rename = "process.stop")]
    ProcessStop { run: String },
    /// Names the task; the directory is `workspaces/<task id>`, found and judged by R106 and R118.
    #[serde(rename = "workspace.remove")]
    WorkspaceRemove { task: String },
    /// A blob's SHA-256 as 64 lowercase hex digits. The event-file variant of the
    /// spec's table is outside 1.1 (owner decision of 2026-11-21).
    #[serde(rename = "blob.delete")]
    BlobDelete { hash: String },
}

impl IntentPayload {
    pub fn kind(&self) -> IntentKind {
        match self {
            Self::ProcessStart { .. } => IntentKind::ProcessStart,
            Self::ProcessStop { .. } => IntentKind::ProcessStop,
            Self::WorkspaceRemove { .. } => IntentKind::WorkspaceRemove,
            Self::BlobDelete { .. } => IntentKind::BlobDelete,
        }
    }

    /// The run a process intent is about; none for the other kinds.
    pub fn run(&self) -> Option<&str> {
        match self {
            Self::ProcessStart { run } | Self::ProcessStop { run } => Some(run),
            Self::WorkspaceRemove { .. } | Self::BlobDelete { .. } => None,
        }
    }

    /// The closed-shape check both backends apply before they change anything.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::ProcessStart { run: id }
            | Self::ProcessStop { run: id }
            | Self::WorkspaceRemove { task: id } => check_name(id),
            Self::BlobDelete { hash } => check_hash(hash),
        }
    }
}

fn invalid(detail: impl Into<String>) -> StoreError {
    StoreError::InvalidIntent(detail.into())
}

fn check_name(id: &str) -> Result<()> {
    let one_name = !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if one_name {
        Ok(())
    } else {
        Err(invalid(format!(
            "an intent names one id of 1 to {MAX_ID_BYTES} bytes of A-Z a-z 0-9 . _ -, other than . and .."
        )))
    }
}

fn check_hash(hash: &str) -> Result<()> {
    if hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(invalid(
            "a blob intent names a SHA-256 as 64 lowercase hex digits",
        ))
    }
}

/// One row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Project-local, from 1, never reused.
    pub id: u64,
    pub payload: IntentPayload,
    /// UTC milliseconds of the transaction that added it.
    pub created_at: i64,
    /// UTC milliseconds of the first completion; none while pending.
    pub done_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishOutcome {
    /// This call completed the intent; its time is the one stored.
    Finished,
    /// Completed before, or completed and since deleted by retention. Nothing
    /// changed, and the first completion's time stands.
    AlreadyDone,
}

/// Rows as of the enclosing read snapshot.
#[async_trait]
pub trait IntentRead: Send + Sync {
    async fn read_intent(&self, id: u64) -> Result<Option<Intent>>;
    /// Pending intents with an id above `after`, oldest id first. `limit` must
    /// be in 1..=1,000. Resume from the last id returned to enumerate a backlog
    /// of any size; a performer that skips a failing row passes that row's id.
    async fn read_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<Intent>>;
}

/// Operations share the enclosing unit of work, so an intent commits or rolls
/// back with the run, the lease and the task status written beside it.
pub trait IntentWrite: Send {
    /// Stores a pending intent with the next id. Payloads are never merged: two
    /// adds of one payload are two intents. Refused as
    /// [`StoreError::InvalidIntent`] for a payload that fails
    /// [`IntentPayload::validate`], and as [`StoreError::StaleEpoch`] for a
    /// process intent without the lease the module documentation requires.
    fn add_intent(&mut self, payload: &IntentPayload, now_ms: i64) -> Result<Intent>;
    /// Completes an intent. Idempotent: a second call, or a call for an id that
    /// retention deleted, is `AlreadyDone`. An id that was never issued in this
    /// project is [`StoreError::InvalidIntent`].
    fn finish_intent(&mut self, id: u64, now_ms: i64) -> Result<FinishOutcome>;
    /// Deletes finished intents whose `done_at` is at least [`DONE_RETENTION_MS`]
    /// before `now_ms`, lowest id first, at most `max_rows` (1..=1,000), and
    /// returns how many. Pending intents are never deleted.
    fn delete_finished_intents(&mut self, now_ms: i64, max_rows: usize) -> Result<usize>;
    fn get_intent(&self, id: u64) -> Result<Option<Intent>>;
    fn list_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<Intent>>;
}

/// The bound shared by both backends' reads and deletions.
pub fn check_bound(value: usize) -> Result<()> {
    if (1..=MAX_ROWS).contains(&value) {
        Ok(())
    } else {
        Err(invalid("row bound must be in 1..=1000"))
    }
}
