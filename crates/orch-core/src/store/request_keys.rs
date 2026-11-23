//! Request keys (R45, R46, R47, R134; ADR 0017 decision 8): a repeated change runs once.
//!
//! A mutate or destructive request that carries a key is looked up by
//! [`RequestKeyWrite::check_request_key`] inside the unit of work that would make
//! the change. [`KeyCheck::New`] means run the change and store the key beside it
//! with [`RequestKeyWrite::store_request_key`], in the same unit of work, so the
//! key and the change commit or roll back together. A key seen again with the
//! same operation and request hash is a replay of the first result and the change
//! is not made again; the same key with another operation or hash is
//! [`KeyCheck::Mismatch`], and the consumer refuses the request with `conflict`
//! (R46). The port records and returns; it never runs the change, hashes a
//! request, resends one or decides what a refusal says.
//!
//! **Scope.** A stored result is found only by the whole scope: identity,
//! project, run, epoch and key (R134). A [`RequestScope`] is built from the
//! [`Caller`] of the unit of work and nowhere else: its identity, its project
//! and, for a task principal, the run and epoch of the lease it holds. An owner
//! or worker session has no lease, so its run is empty and its epoch is 0, and a
//! task principal is refused if its run is empty or its epoch is 0, so that a
//! task can never be given an owner's scope. A request frame supplies the key and
//! nothing else of the scope: a run or epoch in a request is not an input here
//! (the daemon ignores it, R134), and a scope built from another caller is
//! refused by the unit of work.
//!
//! **Authorization comes first, in the consumer.** The port does not authorize,
//! as no operation of this port does. The consumer (the dispatcher, T17 and T18)
//! must have checked that the principal is live and allowed the operation, and
//! that the operation is not one that issues a credential (R134), before it calls
//! [`RequestKeyWrite::check_request_key`]. A refused principal must get no stored
//! result, so it must never reach the lookup. The task principal's run and epoch
//! come from the lease the daemon authenticated for the connection.
//!
//! **Time.** Milliseconds are UTC, given by the caller (`Clock::now_ms()`); the
//! port reads no clock. A key is stored at `now_ms` and is replayable while
//! `now_ms < stored_at + KEY_TTL_MS`: exactly one hour, the boundary millisecond
//! is already expired. An expired row is as good as absent: a check finds
//! [`KeyCheck::New`] and a store replaces it, even before
//! [`RequestKeyWrite::delete_expired_request_keys`] (the `request_keys.expire`
//! job, T15) has removed it. That job deletes only rows that are already not
//! replayable, at most [`MAX_ROWS`] per call.
//!
//! **Results.** A result is UTF-8 text. One of at most [`MAX_RESULT_BYTES`] bytes
//! is kept whole. A longer one is not kept at all, not even a prefix, and its
//! replay is [`KeyCheck::ReplayWithoutResult`], which the consumer must answer as
//! a replay that says the result was not kept (R47). Request keys are never
//! exported or imported (R114) and no read path shows them (R74), which is why
//! this port has no snapshot read: the only reader is the writer, inside the
//! transaction of the change.
//!
//! A refusal changes nothing, even when the callback catches it and commits. A
//! restore ends leases but does not delete a key; rows scoped to an epoch that no
//! lease holds any more are never matched by a live principal, as epochs are not
//! reused, and the expiry job removes them.

use std::fmt;

use super::{Caller, Principal, Result, StoreError};

#[cfg(feature = "test-support")]
pub mod fake;
#[cfg(test)]
mod tests;

/// A stored key is replayable for one hour, in milliseconds.
pub const KEY_TTL_MS: i64 = 60 * 60 * 1_000;
/// The longest result kept, in bytes (64 KiB). A longer one is not stored.
pub const MAX_RESULT_BYTES: usize = 64 * 1024;
/// Most rows one expiry call deletes.
pub const MAX_ROWS: usize = 1_000;
/// Key length in bytes; its characters are `A-Z a-z 0-9 _ -`.
pub const MIN_KEY_BYTES: usize = 16;
pub const MAX_KEY_BYTES: usize = 128;
/// The longest operation name, in bytes.
pub const MAX_OP_BYTES: usize = 128;

fn invalid(detail: impl Into<String>) -> StoreError {
    StoreError::InvalidRequestKey(detail.into())
}

/// Where a stored result is found: identity, project, run, epoch and key.
/// Fields are private, so the only way to make one is [`RequestScope::of`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestScope {
    identity: String,
    project: String,
    run: String,
    epoch: u64,
    key: String,
}

impl fmt::Debug for RequestScope {
    // The key is a bearer of the stored result, so it is not printed whole.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestScope")
            .field("identity", &self.identity)
            .field("project", &self.project)
            .field("run", &self.run)
            .field("epoch", &self.epoch)
            .field("key", &"..")
            .finish()
    }
}

impl RequestScope {
    /// The scope of `key` for `caller`. The run and epoch are the caller's lease
    /// (a task principal) or empty and 0 (any other principal); a request cannot
    /// give either. Refused as [`StoreError::InvalidRequestKey`] for a key that
    /// is not 16 to 128 characters of `A-Z a-z 0-9 _ -`, an empty identity or
    /// project, or a task principal without a run or with epoch 0.
    pub fn of(caller: &Caller, key: &str) -> Result<Self> {
        check_key(key)?;
        let (identity, run, epoch) = match &caller.principal {
            Principal::Owner { identity } | Principal::Worker { identity } => {
                (identity.as_str(), "", 0)
            }
            Principal::Task {
                identity,
                run,
                epoch,
                ..
            } => {
                if run.is_empty() || *epoch == 0 {
                    return Err(invalid(
                        "a task principal holds a run and an epoch of 1 or more",
                    ));
                }
                if i64::try_from(*epoch).is_err() {
                    return Err(invalid("epoch is out of range"));
                }
                (identity.as_str(), run.as_str(), *epoch)
            }
        };
        if identity.is_empty() || caller.project.is_empty() {
            return Err(invalid("a scope names an identity and a project"));
        }
        Ok(Self {
            identity: identity.into(),
            project: caller.project.clone(),
            run: run.into(),
            epoch,
            key: key.into(),
        })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn project(&self) -> &str {
        &self.project
    }
    /// Empty for an owner or worker session.
    pub fn run(&self) -> &str {
        &self.run
    }
    /// 0 for an owner or worker session.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The check both backends make before they read or write anything: the
    /// scope was built from the caller of this unit of work, so a scope of
    /// another identity, project, run or epoch is refused rather than looked up.
    pub fn check_caller(&self, caller: &Caller) -> Result<()> {
        if Self::of(caller, &self.key)? == *self {
            Ok(())
        } else {
            Err(invalid("the scope belongs to another caller"))
        }
    }
}

/// The key shape: 16 to 128 characters of `A-Z a-z 0-9 _ -`.
pub fn check_key(key: &str) -> Result<()> {
    let shaped = (MIN_KEY_BYTES..=MAX_KEY_BYTES).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if shaped {
        Ok(())
    } else {
        Err(invalid(format!(
            "a key is {MIN_KEY_BYTES} to {MAX_KEY_BYTES} characters of A-Z a-z 0-9 _ -"
        )))
    }
}

/// What a lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// No live row: run the change and store the key with it.
    New,
    /// The same operation and hash were stored: return this first result as a
    /// replay and make no change.
    Replay(String),
    /// As `Replay`, but the result was over [`MAX_RESULT_BYTES`] and was not kept.
    ReplayWithoutResult,
    /// The key is live with another operation or another request hash: refuse
    /// the request with `conflict` (R46) and make no change.
    Mismatch,
}

/// Operations share the enclosing unit of work, so a key commits or rolls back
/// with the change it guards.
pub trait RequestKeyWrite: Send {
    /// Looks the scope up as of `now_ms`. `op` is the operation name and
    /// `req_sha256` the request hash, 64 lowercase hex digits, computed by the
    /// caller (the canonical form is T17's). Sees this unit of work's own
    /// stores. Reads only.
    fn check_request_key(
        &self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        now_ms: i64,
    ) -> Result<KeyCheck>;
    /// Stores the key at `now_ms`, replayable until `now_ms + KEY_TTL_MS`, with
    /// `result` (not kept if over [`MAX_RESULT_BYTES`]). A live row of the scope
    /// is never replaced, whatever it holds: the caller checks first, in this
    /// unit of work, and stores only on [`KeyCheck::New`]; storing over a live
    /// key is [`StoreError::InvalidRequestKey`], as is a scope of another
    /// caller, an operation that is not 1 to 128 bytes of `A-Z a-z 0-9 . _ -`, a
    /// hash that is not 64 lowercase hex digits, a negative `now_ms` or one
    /// whose expiry overflows. An expired row is replaced. A refusal changes
    /// nothing.
    fn store_request_key(
        &mut self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        result: &str,
        now_ms: i64,
    ) -> Result<()>;
    /// Deletes rows that are no longer replayable (`expires_at <= now_ms`),
    /// earliest expiry first, ties by scope, at most `max_rows` (1..=1,000), and
    /// returns how many. Rows of this unit of work's project only.
    fn delete_expired_request_keys(&mut self, now_ms: i64, max_rows: usize) -> Result<usize>;
}

/// The shape checks both backends make before they read or change anything.
pub fn check_request(op: &str, req_sha256: &str, now_ms: i64) -> Result<()> {
    let named = (1..=MAX_OP_BYTES).contains(&op.len())
        && op
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !named {
        return Err(invalid(format!(
            "an operation is 1 to {MAX_OP_BYTES} bytes of A-Z a-z 0-9 . _ -"
        )));
    }
    if req_sha256.len() != 64
        || !req_sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(invalid("a request hash is 64 lowercase hex digits"));
    }
    check_now(now_ms)
}

/// Time is UTC milliseconds, not before the epoch.
pub fn check_now(now_ms: i64) -> Result<()> {
    if now_ms >= 0 {
        Ok(())
    } else {
        Err(invalid("time is UTC milliseconds, not before the epoch"))
    }
}

/// When a key stored at `now_ms` stops being replayable.
pub fn expires_at(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(KEY_TTL_MS)
        .ok_or_else(|| invalid("expiry is past the last millisecond"))
}

/// The bound on one expiry call.
pub fn check_bound(value: usize) -> Result<()> {
    if (1..=MAX_ROWS).contains(&value) {
        Ok(())
    } else {
        Err(invalid("row bound must be in 1..=1000"))
    }
}
