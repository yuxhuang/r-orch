//! The shared fatal state of the SQLite backend (issue 127; spec R128, R140, R141).
//!
//! One `FailureState` is shared by the writer, the read pool and every other
//! live connection. Whoever sees a result that proves the live database or its
//! data damaged reports it through `observe`, at the place where the error is
//! produced and before it is converted to a public error. The first fatal
//! observation latches the state until the process restarts: the writer then
//! refuses every mutation, no claim is granted, and a checkpoint is not called
//! (`ensure_available` is the one gate for all three). A read is not behind it: a
//! pooled snapshot read is served after a latch and refused only for a fatal
//! observation made during that read (R128 and R140; issue 389).
//!
//! What is fatal is decided here, by the structure of the error, and nowhere
//! else. `classify` matches every `SqliteError` variant with no wildcard, so a
//! new variant (the structured `TaskCorrupt` of R141, for one) cannot be added
//! without a decision about its class. `classify_sql` cannot do the same for
//! `rusqlite::Error`, which is `#[non_exhaustive]`: a SQLite result code is
//! decided by its code, and every other rusqlite error is the request's own
//! answer (`Ordinary`). Task corruption and the structured
//! corruption of an Agent, Team, Run, Context, Config, Template or Goal
//! (`EntityCorrupt`, one variant for the seven families) are fatal;
//! object access and publication failures of either (`TaskAccess`,
//! `TaskPublication`, `EntityAccess`, `EntityPublication`) alone prove nothing
//! about the live database.
//!
//! Corruption found in an external import, backup or export artifact is not
//! the live database: it is classified, reported to the caller, and never
//! latched (`FailureSource::Artifact`).
use crate::sqlite::{Result, SqliteError};
use rusqlite::ErrorCode;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Which connection saw the result (R140). Only `Artifact` is not live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureSource {
    /// The sole writing connection: callbacks, BEGIN, commit and rollback.
    Writer,
    /// A pooled read-only snapshot connection.
    Reader,
    /// The checkpoint job's own connection.
    Checkpoint,
    /// A live-source `orch db query`, backup, export or verification.
    Tool,
    /// An external import, backup or export file. Never latches.
    Artifact,
}

impl fmt::Display for FailureSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Writer => "writer",
            Self::Reader => "reader",
            Self::Checkpoint => "checkpoint",
            Self::Tool => "tool",
            Self::Artifact => "artifact",
        })
    }
}

/// Why the state is latched. No variant carries a value, key, path or message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatalCause {
    SqliteCorrupt,
    SqliteNotADatabase,
    ConsecutiveIoErrors,
    RetryWithoutCommit,
    /// Proven logical corruption of live data (R141), though SQLite reports no error.
    LogicalCorruption,
    /// An explicit rollback of the writer's top-level transaction failed and the
    /// connection is still inside a transaction (R128). A rollback error alone, with
    /// the connection outside a transaction, is not this cause.
    RollbackLeftTransactionOpen,
}

impl fmt::Display for FatalCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SqliteCorrupt => "SQLITE_CORRUPT",
            Self::SqliteNotADatabase => "SQLITE_NOTADB",
            Self::ConsecutiveIoErrors => "second consecutive SQLITE_IOERR",
            Self::RetryWithoutCommit => "sixty seconds of retry without a commit",
            Self::LogicalCorruption => "proven logical corruption of live data",
            Self::RollbackLeftTransactionOpen => {
                "rollback left the writer connection inside a transaction"
            }
        })
    }
}

/// Which side of an `OperationRollback` a result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Operation,
    Rollback,
}

/// The class of one result under the writer's policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The request's own answer (a refusal, a stale revision, an absent row).
    Ordinary,
    /// A backend failure that proves nothing: counted on a pooled read, and the
    /// connection is opened afresh.
    Transient,
    Fatal(FatalCause),
}

/// Every result found in one error, the operation's and the rollback's kept
/// apart (an `OperationRollback` can nest). A fatal cause on either side wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    leaves: Vec<(Role, Class)>,
    savepoint_rollback_failed: bool,
}

impl Observation {
    pub fn leaves(&self) -> &[(Role, Class)] {
        &self.leaves
    }

    pub fn fatal_causes(&self) -> Vec<(Role, FatalCause)> {
        self.leaves
            .iter()
            .filter_map(|(role, class)| match class {
                Class::Fatal(cause) => Some((*role, *cause)),
                _ => None,
            })
            .collect()
    }

    pub fn is_fatal(&self) -> bool {
        self.leaves
            .iter()
            .any(|(_, class)| matches!(class, Class::Fatal(_)))
    }

    /// True when any savepoint rollback in the error failed, fatal or not. The
    /// enclosing transaction can then never commit.
    pub fn savepoint_rollback_failed(&self) -> bool {
        self.savepoint_rollback_failed
    }

    fn has_transient(&self) -> bool {
        self.leaves
            .iter()
            .any(|(_, class)| *class == Class::Transient)
    }
}

/// Classify an error by its structure, before any public conversion.
pub fn classify(error: &SqliteError) -> Observation {
    let mut observation = Observation {
        leaves: Vec::new(),
        savepoint_rollback_failed: false,
    };
    walk(error, &mut observation);
    observation
}

fn walk(error: &SqliteError, out: &mut Observation) {
    use SqliteError::*;
    let class = match error {
        OperationRollback {
            operation,
            rollback,
        } => {
            out.savepoint_rollback_failed = true;
            walk(operation, out);
            out.leaves.push((Role::Rollback, classify_sql(rollback)));
            return;
        }
        Sql(error) => classify_sql(error),
        // Reading or opening a file can fail for reasons that prove nothing.
        Io(_) | Confined(_) | TaskAccess(_) | TaskPublication(_) | EntityAccess(_)
        | EntityPublication(_) => Class::Transient,
        TaskCorrupt { .. } | EntityCorrupt { .. } => Class::Fatal(FatalCause::LogicalCorruption),
        // The error that a latched state answers with: already latched.
        StorageUnavailable(_) => Class::Ordinary,
        // R140 names SQLITE_CORRUPT, SQLITE_NOTADB, `TaskCorrupt` and `EntityCorrupt`
        // (the arm above) as fatal. The stored-data errors below are not among them.
        EventCorrupt(_)
        | IntentCorrupt(_)
        | LeaseCorrupt(_)
        | RetryMessageCorrupt(_)
        | MessageCorrupt { .. } => Class::Ordinary,
        CounterMissing { .. }
        | CounterNegative { .. }
        | CounterBehind { .. }
        | Unrepresentable(_)
        | TransactionAborted
        | Overloaded
        | SnapshotExpired
        | Store(_)
        | WriterClosed
        | CallbackPanicked(_)
        | InvalidCapacity
        | Setting { .. }
        | NewerSchema { .. }
        | Migration(_)
        | DiskFloor { .. }
        | Backup(_) => Class::Ordinary,
    };
    out.leaves.push((Role::Operation, class));
}

fn classify_sql(error: &rusqlite::Error) -> Class {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => match failure.code {
            ErrorCode::DatabaseCorrupt => Class::Fatal(FatalCause::SqliteCorrupt),
            ErrorCode::NotADatabase => Class::Fatal(FatalCause::SqliteNotADatabase),
            // A constraint is the request's own answer (R128), not a failed connection.
            ErrorCode::ConstraintViolation => Class::Ordinary,
            _ => Class::Transient,
        },
        _ => Class::Ordinary,
    }
}

/// The latched failure: the first fatal observation, with every fatal cause it held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub source: FailureSource,
    pub causes: Vec<(Role, FatalCause)>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} connection:", self.source)?;
        for (index, (role, cause)) in self.causes.iter().enumerate() {
            let side = match role {
                Role::Operation => "operation",
                Role::Rollback => "rollback",
            };
            write!(f, "{} {side} {cause}", if index == 0 { "" } else { ";" })?;
        }
        Ok(())
    }
}

/// A failed pooled read is counted, and three in a row are reported (R140).
const POOL_FAILURES_REPORTED: u32 = 3;

#[derive(Default)]
struct Inner {
    failure: Mutex<Option<Failure>>,
    /// Every fatal observation, latched already or not. A read that straddles
    /// a change in this count discards its result.
    fatal_observations: AtomicU64,
    /// Set by the writer's own observations; cleared at each BEGIN.
    transaction_aborted: AtomicBool,
    pool_failure_streak: AtomicU32,
    writer_retry: Mutex<Option<super::policy::RetryCode>>,
    retry_starts: Mutex<Vec<RetryRegistration>>,
    consecutive_ioerr: AtomicBool,
    over_bound: AtomicU64,
}

struct RetryRegistration {
    since: std::time::Instant,
    deadline: Option<std::time::Instant>,
    active: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
}

/// Shared by the writer, the read pool and every other live connection. Clone
/// it freely: every clone is the same state.
#[derive(Clone, Default)]
pub struct FailureState {
    inner: Arc<Inner>,
}

impl fmt::Debug for FailureState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FailureState")
            .field("failure", &self.failure())
            .finish_non_exhaustive()
    }
}

impl FailureState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Report one result, at the place that produced it. A callback that catches an
    /// error and carries on must call this first: a fatal cause is latched even when
    /// the callback then returns success. `Artifact` is classified and never latches.
    pub fn observe(&self, source: FailureSource, error: &SqliteError) -> Observation {
        let observation = classify(error);
        if source == FailureSource::Artifact {
            return observation;
        }
        if source == FailureSource::Writer {
            match error {
                SqliteError::Sql(error) => {
                    let code = super::policy::RetryCode::sql(error);
                    *self
                        .inner
                        .writer_retry
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = code;
                    if code.is_none() && matches!(error, rusqlite::Error::SqliteFailure(_, _)) {
                        self.inner.consecutive_ioerr.store(false, Ordering::SeqCst);
                    }
                }
                // Preserve the source observation through Unit's lossy port
                // conversion and through the callback-wide abort guard. A
                // cleanup unwind is classified by record O, never retried.
                SqliteError::Store(orch_core::store::StoreError::Backend(_))
                | SqliteError::TransactionAborted
                | SqliteError::OperationRollback { .. } => {}
                // A new logical refusal, non-retryable backend cause or caller
                // panic is its own answer, not the earlier caught SQL failure.
                _ => {
                    *self
                        .inner
                        .writer_retry
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = None
                }
            }
        }
        let causes = observation.fatal_causes();
        if !causes.is_empty() {
            self.record(source, causes);
        }
        if source == FailureSource::Writer
            && (observation.is_fatal() || observation.savepoint_rollback_failed())
        {
            self.inner.transaction_aborted.store(true, Ordering::SeqCst);
        }
        observation
    }

    /// Latch a fatal cause by name, for a producer that proved it without an error
    /// value to classify.
    pub fn latch(&self, source: FailureSource, cause: FatalCause) {
        if source == FailureSource::Artifact {
            return;
        }
        self.record(source, vec![(Role::Operation, cause)]);
        if source == FailureSource::Writer {
            self.inner.transaction_aborted.store(true, Ordering::SeqCst);
        }
    }

    fn record(&self, source: FailureSource, causes: Vec<(Role, FatalCause)>) {
        let mut failure = self
            .inner
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        failure.get_or_insert(Failure { source, causes });
        // After the latch is visible: a reader that sees the count change finds it.
        self.inner.fatal_observations.fetch_add(1, Ordering::SeqCst);
    }

    /// The gate for a mutation, a claim and a checkpoint: refuses once latched. Not the
    /// gate of a pooled read, which fails only for a fatal observation during it.
    pub fn ensure_available(&self) -> Result<()> {
        match self.failure() {
            Some(failure) => Err(SqliteError::StorageUnavailable(failure)),
            None => Ok(()),
        }
    }

    pub fn failure(&self) -> Option<Failure> {
        self.inner
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Failed pooled reads in a row; a successful read resets it.
    pub fn pool_failure_streak(&self) -> u32 {
        self.inner.pool_failure_streak.load(Ordering::SeqCst)
    }

    pub fn pool_degraded(&self) -> bool {
        self.pool_failure_streak() >= POOL_FAILURES_REPORTED
    }

    pub(in crate::sqlite) fn fatal_observations(&self) -> u64 {
        self.inner.fatal_observations.load(Ordering::SeqCst)
    }

    /// A pooled read ended, with the error it ended in, if any. A fatal result is
    /// already observed; this counts the rest.
    pub(in crate::sqlite) fn pool_read_finished(&self, error: Option<&SqliteError>) {
        match error.map(|error| classify(error).has_transient()) {
            None => self.inner.pool_failure_streak.store(0, Ordering::SeqCst),
            Some(true) => {
                self.inner
                    .pool_failure_streak
                    .fetch_add(1, Ordering::SeqCst);
            }
            Some(false) => {}
        }
    }

    pub(super) fn retry_started(
        &self,
        since: std::time::Instant,
        deadline: Option<std::time::Instant>,
        active: Arc<AtomicBool>,
        phase: Arc<AtomicU8>,
    ) {
        active.store(true, Ordering::SeqCst);
        self.inner
            .retry_starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RetryRegistration {
                since,
                deadline,
                active,
                phase,
            });
    }
    /// Remove the registration of this episode, and only this one. An episode
    /// is identified by its `active` flag, never by the instant of its first
    /// failure: two episodes can fail on the same clock reading (issue 448).
    pub(super) fn retry_finished(&self, active: &Arc<AtomicBool>) {
        let mut starts = self
            .inner
            .retry_starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = starts
            .iter()
            .position(|start| Arc::ptr_eq(&start.active, active))
        {
            starts.remove(i).active.store(false, Ordering::SeqCst);
        }
    }
    pub(super) fn check_retry_timeout(&self, now: std::time::Instant) {
        let mut starts = self
            .inner
            .retry_starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut timed_out = false;
        starts.retain(|entry| {
            // A known noncommit can be withdrawn without another producer
            // submission. An unknown COMMIT stays fenced until reconciled.
            if entry.deadline.is_some_and(|deadline| now >= deadline)
                && entry.phase.load(Ordering::SeqCst) != super::policy::WriteOutcome::Unknown as u8
            {
                entry.active.store(false, Ordering::SeqCst);
                return false;
            }
            timed_out |=
                now.saturating_duration_since(entry.since) >= std::time::Duration::from_secs(60);
            true
        });
        drop(starts);
        if timed_out && self.failure().is_none() {
            self.latch(FailureSource::Writer, FatalCause::RetryWithoutCommit);
        }
    }
    pub(in crate::sqlite) fn claims_suppressed(&self) -> bool {
        !self
            .inner
            .retry_starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }
    /// Count the executor's terminal SQL failure once, after raw observations
    /// have been deduplicated within its transaction. Logical request turnover
    /// is not a successful commit and cannot erase a preceding I/O failure.
    pub(super) fn retry_failure(&self, code: super::policy::RetryCode) -> bool {
        self.inner
            .consecutive_ioerr
            .swap(code == super::policy::RetryCode::Io, Ordering::SeqCst)
            && code == super::policy::RetryCode::Io
    }
    pub(in crate::sqlite) fn writer_committed(&self) {
        self.inner.consecutive_ioerr.store(false, Ordering::SeqCst);
    }
    pub(super) fn count_over_bound(&self) {
        self.inner.over_bound.fetch_add(1, Ordering::SeqCst);
    }
    /// Indivisible transactions over 1,000 actual changed rows or 20 ms of execution.
    pub fn writer_txn_over_bound_total(&self) -> u64 {
        self.inner.over_bound.load(Ordering::SeqCst)
    }
    pub(in crate::sqlite) fn writer_retry(&self) -> Option<super::policy::RetryCode> {
        *self
            .inner
            .writer_retry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
    pub(super) fn begin_transaction(&self) {
        *self
            .inner
            .writer_retry
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.inner
            .transaction_aborted
            .store(false, Ordering::SeqCst);
    }

    /// A leaf that may have partially written requires callback-wide rollback,
    /// even if trusted callback code catches its nonfatal error.
    pub(in crate::sqlite) fn abort_transaction(&self) {
        self.inner.transaction_aborted.store(true, Ordering::SeqCst);
    }

    /// The enclosing transaction may not commit: a leaf requested abort, a
    /// failed savepoint rollback or fatal writer result was observed, or the
    /// shared backend state is latched.
    pub(in crate::sqlite) fn transaction_must_abort(&self) -> Option<SqliteError> {
        if let Err(error) = self.ensure_available() {
            return Some(error);
        }
        self.inner
            .transaction_aborted
            .load(Ordering::SeqCst)
            .then_some(SqliteError::TransactionAborted)
    }
}

#[cfg(test)]
mod tests {
    //! Component-level pins for the classes of the structured corruption, access and
    //! publication errors (issues 127 and 343). They exercise `classify` and
    //! `FailureState` only: that the combined adapter catches a corruption, latches,
    //! rolls back and refuses the next mutation is end-to-end evidence they do not give.
    use super::*;
    use crate::blob::BlobError;
    use std::io::{self, ErrorKind};

    fn task_corrupt() -> SqliteError {
        SqliteError::TaskCorrupt {
            task: "t".into(),
            check: "typed detail",
            locator: "detail",
        }
    }

    fn entity_corrupt() -> SqliteError {
        SqliteError::EntityCorrupt {
            family: "context",
            form: 1,
            check: "typed detail",
            locator: "detail".into(),
        }
    }

    fn publication() -> BlobError {
        BlobError::Io(io::Error::other("synthetic publication failure"))
    }

    /// One error per access or publication variant of the two families.
    fn access_and_publication() -> [(&'static str, SqliteError); 4] {
        [
            ("TaskAccess", SqliteError::TaskAccess(ErrorKind::NotFound)),
            (
                "TaskPublication",
                SqliteError::TaskPublication(publication()),
            ),
            (
                "EntityAccess",
                SqliteError::EntityAccess(ErrorKind::PermissionDenied),
            ),
            (
                "EntityPublication",
                SqliteError::EntityPublication(publication()),
            ),
        ]
    }

    fn operation(class: Class) -> Vec<(Role, Class)> {
        vec![(Role::Operation, class)]
    }

    #[test]
    fn entity_corrupt_is_fatal_logical_corruption_like_task_corrupt() {
        let fatal = operation(Class::Fatal(FatalCause::LogicalCorruption));
        assert_eq!(classify(&entity_corrupt()).leaves(), fatal);
        assert_eq!(classify(&task_corrupt()).leaves(), fatal);
        assert!(classify(&entity_corrupt()).is_fatal());
        assert_eq!(
            classify(&entity_corrupt()).fatal_causes(),
            [(Role::Operation, FatalCause::LogicalCorruption)]
        );
    }

    #[test]
    fn access_and_publication_failures_of_both_families_are_transient_not_fatal() {
        for (name, error) in access_and_publication() {
            let observation = classify(&error);
            assert_eq!(observation.leaves(), operation(Class::Transient), "{name}");
            assert!(!observation.is_fatal(), "{name}");
            assert!(observation.fatal_causes().is_empty(), "{name}");
            assert!(!observation.savepoint_rollback_failed(), "{name}");
        }
    }

    #[test]
    fn a_corrupt_entity_on_a_live_connection_latches_and_refuses_the_gate() {
        for source in [
            FailureSource::Writer,
            FailureSource::Reader,
            FailureSource::Checkpoint,
            FailureSource::Tool,
        ] {
            let state = FailureState::new();
            state.ensure_available().expect("fresh state is available");
            let observation = state.observe(source, &entity_corrupt());
            assert!(observation.is_fatal(), "{source}");
            let failure = state
                .failure()
                .unwrap_or_else(|| panic!("{source} latched"));
            assert_eq!(failure.source, source);
            assert_eq!(
                failure.causes,
                [(Role::Operation, FatalCause::LogicalCorruption)]
            );
            assert!(matches!(
                state.ensure_available(),
                Err(SqliteError::StorageUnavailable(latched)) if latched == failure
            ));
            assert_eq!(state.fatal_observations(), 1, "{source}");
        }
    }

    const LIVE_NON_WRITERS: [FailureSource; 3] = [
        FailureSource::Reader,
        FailureSource::Checkpoint,
        FailureSource::Tool,
    ];

    /// A nonfatal failed savepoint rollback: neither side is fatal, so no latch can
    /// make `transaction_must_abort` answer for the abort bit.
    fn nonfatal_operation_rollback() -> SqliteError {
        SqliteError::OperationRollback {
            operation: Box::new(SqliteError::Overloaded),
            rollback: sqlite_failure(rusqlite::ffi::SQLITE_BUSY),
        }
    }

    fn sqlite_failure(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    fn abort_bit(state: &FailureState) -> bool {
        state.inner.transaction_aborted.load(Ordering::SeqCst)
    }

    #[test]
    fn only_the_writer_observes_into_the_abort_bit_even_when_nothing_is_latched() {
        // Positive control: the writer's nonfatal failed rollback sets the bit and
        // latches nothing, so the bit alone is what forbids the commit.
        let writer = FailureState::new();
        writer.begin_transaction();
        let observation = writer.observe(FailureSource::Writer, &nonfatal_operation_rollback());
        assert!(observation.savepoint_rollback_failed());
        assert!(!observation.is_fatal());
        assert_eq!(writer.failure(), None);
        assert!(abort_bit(&writer));
        assert!(matches!(
            writer.transaction_must_abort(),
            Some(SqliteError::TransactionAborted)
        ));

        // Every other source, with a fresh state each: the bit stays clear.
        for source in LIVE_NON_WRITERS
            .into_iter()
            .chain([FailureSource::Artifact])
        {
            let state = FailureState::new();
            state.begin_transaction();
            let observation = state.observe(source, &nonfatal_operation_rollback());
            assert!(observation.savepoint_rollback_failed(), "{source}");
            assert_eq!(state.failure(), None, "{source}");
            assert!(!abort_bit(&state), "{source} must not set the abort bit");
            assert!(state.transaction_must_abort().is_none(), "{source}");
        }
    }

    #[test]
    fn a_fatal_observation_sets_the_abort_bit_for_the_writer_alone() {
        let state = FailureState::new();
        state.begin_transaction();
        state.observe(FailureSource::Writer, &entity_corrupt());
        assert!(abort_bit(&state));

        // A latched non-writer is refused by `ensure_available`, but the bit itself
        // is the writer's, so a later transaction that clears it is not poisoned twice.
        for source in LIVE_NON_WRITERS {
            let state = FailureState::new();
            state.begin_transaction();
            state.observe(source, &entity_corrupt());
            assert!(state.failure().is_some(), "{source} latches");
            assert!(!abort_bit(&state), "{source} must not set the abort bit");
        }
    }

    #[test]
    fn a_named_latch_sets_the_abort_bit_for_the_writer_alone() {
        let writer = FailureState::new();
        writer.begin_transaction();
        writer.latch(FailureSource::Writer, FatalCause::LogicalCorruption);
        assert!(abort_bit(&writer));

        for source in LIVE_NON_WRITERS {
            let state = FailureState::new();
            state.begin_transaction();
            state.latch(source, FatalCause::SqliteCorrupt);
            let failure = state
                .failure()
                .unwrap_or_else(|| panic!("{source} latched"));
            assert_eq!(failure.source, source);
            assert!(!abort_bit(&state), "{source} must not set the abort bit");
        }

        let artifact = FailureState::new();
        artifact.begin_transaction();
        artifact.latch(FailureSource::Artifact, FatalCause::SqliteCorrupt);
        assert_eq!(artifact.failure(), None);
        assert!(!abort_bit(&artifact));
    }

    #[test]
    fn begin_transaction_clears_only_the_abort_bit_and_not_the_latch() {
        let state = FailureState::new();
        state.observe(FailureSource::Writer, &nonfatal_operation_rollback());
        assert!(abort_bit(&state));
        state.begin_transaction();
        assert!(!abort_bit(&state));
        assert!(state.transaction_must_abort().is_none());

        state.observe(FailureSource::Writer, &entity_corrupt());
        state.begin_transaction();
        assert!(!abort_bit(&state));
        assert!(matches!(
            state.transaction_must_abort(),
            Some(SqliteError::StorageUnavailable(_))
        ));
    }

    #[test]
    fn sqlite_result_codes_classify_by_primary_code_with_extended_codes_included() {
        use rusqlite::ffi::*;
        let fatal_corrupt = Class::Fatal(FatalCause::SqliteCorrupt);
        let fatal_notadb = Class::Fatal(FatalCause::SqliteNotADatabase);
        let table = [
            ("SQLITE_CORRUPT", SQLITE_CORRUPT, fatal_corrupt),
            ("SQLITE_CORRUPT_VTAB", SQLITE_CORRUPT_VTAB, fatal_corrupt),
            ("SQLITE_CORRUPT_INDEX", SQLITE_CORRUPT_INDEX, fatal_corrupt),
            ("SQLITE_NOTADB", SQLITE_NOTADB, fatal_notadb),
            ("SQLITE_CONSTRAINT", SQLITE_CONSTRAINT, Class::Ordinary),
            (
                "SQLITE_CONSTRAINT_UNIQUE",
                SQLITE_CONSTRAINT_UNIQUE,
                Class::Ordinary,
            ),
            (
                "SQLITE_CONSTRAINT_FOREIGNKEY",
                SQLITE_CONSTRAINT_FOREIGNKEY,
                Class::Ordinary,
            ),
            ("SQLITE_FULL", SQLITE_FULL, Class::Transient),
            ("SQLITE_IOERR", SQLITE_IOERR, Class::Transient),
            ("SQLITE_IOERR_READ", SQLITE_IOERR_READ, Class::Transient),
            ("SQLITE_NOMEM", SQLITE_NOMEM, Class::Transient),
            ("SQLITE_BUSY", SQLITE_BUSY, Class::Transient),
            ("SQLITE_LOCKED", SQLITE_LOCKED, Class::Transient),
            ("SQLITE_READONLY", SQLITE_READONLY, Class::Transient),
            ("SQLITE_CANTOPEN", SQLITE_CANTOPEN, Class::Transient),
            ("SQLITE_ERROR", SQLITE_ERROR, Class::Transient),
        ];
        for (name, code, class) in table {
            let error = SqliteError::Sql(sqlite_failure(code));
            assert_eq!(classify(&error).leaves(), operation(class), "{name}");
            // The same result is a rollback's: classified on its own side.
            let nested = SqliteError::OperationRollback {
                operation: Box::new(SqliteError::Overloaded),
                rollback: sqlite_failure(code),
            };
            assert_eq!(
                classify(&nested).leaves(),
                [(Role::Operation, Class::Ordinary), (Role::Rollback, class)],
                "{name} as a rollback"
            );
        }
    }

    #[test]
    fn a_non_sqlite_failure_of_the_driver_is_the_requests_own_answer() {
        for error in [
            rusqlite::Error::QueryReturnedNoRows,
            rusqlite::Error::InvalidQuery,
            rusqlite::Error::InvalidParameterCount(1, 2),
        ] {
            let label = format!("{error:?}");
            assert_eq!(
                classify(&SqliteError::Sql(error)).leaves(),
                operation(Class::Ordinary),
                "{label}"
            );
        }
    }

    #[test]
    fn each_class_latches_from_each_live_source_by_its_class_alone() {
        use rusqlite::ffi::*;
        let live = [
            FailureSource::Writer,
            FailureSource::Reader,
            FailureSource::Checkpoint,
            FailureSource::Tool,
        ];
        let cases: [(&str, i32, Option<FatalCause>); 6] = [
            ("CORRUPT", SQLITE_CORRUPT, Some(FatalCause::SqliteCorrupt)),
            (
                "NOTADB",
                SQLITE_NOTADB,
                Some(FatalCause::SqliteNotADatabase),
            ),
            ("FULL", SQLITE_FULL, None),
            ("IOERR", SQLITE_IOERR, None),
            ("NOMEM", SQLITE_NOMEM, None),
            ("CONSTRAINT", SQLITE_CONSTRAINT, None),
        ];
        for (name, code, cause) in cases {
            for source in live {
                let state = FailureState::new();
                state.observe(source, &SqliteError::Sql(sqlite_failure(code)));
                match cause {
                    Some(cause) => {
                        let failure = state
                            .failure()
                            .unwrap_or_else(|| panic!("{name} from {source} latches"));
                        assert_eq!(failure.source, source, "{name}");
                        assert_eq!(failure.causes, [(Role::Operation, cause)], "{name}");
                    }
                    None => assert_eq!(state.failure(), None, "{name} from {source}"),
                }
            }
            // An external artifact is classified and reported to its caller, never latched.
            let artifact = FailureState::new();
            let observation = artifact.observe(
                FailureSource::Artifact,
                &SqliteError::Sql(sqlite_failure(code)),
            );
            assert_eq!(observation.is_fatal(), cause.is_some(), "{name}");
            assert_eq!(artifact.failure(), None, "{name} from artifact");
        }
    }

    #[test]
    fn the_first_fatal_observation_keeps_its_source_and_a_later_one_only_counts() {
        let state = FailureState::new();
        state.observe(FailureSource::Reader, &entity_corrupt());
        let first = state.failure().expect("latched");
        state.observe(
            FailureSource::Writer,
            &SqliteError::Sql(sqlite_failure(rusqlite::ffi::SQLITE_NOTADB)),
        );
        assert_eq!(state.failure(), Some(first.clone()));
        assert_eq!(first.source, FailureSource::Reader);
        assert_eq!(state.fatal_observations(), 2);
    }

    #[test]
    fn a_corrupt_entity_in_an_artifact_is_classified_and_never_latches() {
        let state = FailureState::new();
        let observation = state.observe(FailureSource::Artifact, &entity_corrupt());
        assert!(observation.is_fatal());
        assert_eq!(state.failure(), None);
        assert_eq!(state.fatal_observations(), 0);
        state.ensure_available().expect("an artifact never latches");
        assert!(!state.inner.transaction_aborted.load(Ordering::SeqCst));
    }

    #[test]
    fn access_and_publication_failures_never_latch_from_any_source() {
        for (name, error) in access_and_publication() {
            for source in [
                FailureSource::Writer,
                FailureSource::Reader,
                FailureSource::Checkpoint,
                FailureSource::Tool,
                FailureSource::Artifact,
            ] {
                let state = FailureState::new();
                state.begin_transaction();
                let observation = state.observe(source, &error);
                assert_eq!(observation.leaves(), operation(Class::Transient), "{name}");
                assert_eq!(state.failure(), None, "{name} from {source}");
                assert_eq!(state.fatal_observations(), 0, "{name} from {source}");
                state.ensure_available().expect("not latched");
                assert!(
                    state.transaction_must_abort().is_none(),
                    "{name} from {source} must leave the transaction committable"
                );
            }
        }
    }

    #[test]
    fn a_pooled_read_counts_access_and_publication_failures_but_not_corruption() {
        let state = FailureState::new();
        for (index, (name, error)) in access_and_publication().into_iter().enumerate() {
            state.pool_read_finished(Some(&error));
            assert_eq!(state.pool_failure_streak(), index as u32 + 1, "{name}");
        }
        assert!(state.pool_degraded());
        // Corruption is fatal, observed at its producer, and not a transient count.
        let before = state.pool_failure_streak();
        state.pool_read_finished(Some(&entity_corrupt()));
        assert_eq!(state.pool_failure_streak(), before);
        state.pool_read_finished(None);
        assert_eq!(state.pool_failure_streak(), 0);
        assert!(!state.pool_degraded());
    }

    #[test]
    fn a_pooled_read_is_reported_degraded_at_exactly_three_failures_in_a_row() {
        let state = FailureState::new();
        let failed = SqliteError::Sql(sqlite_failure(rusqlite::ffi::SQLITE_IOERR));
        for streak in 1..3 {
            state.pool_read_finished(Some(&failed));
            assert_eq!(state.pool_failure_streak(), streak);
            assert!(!state.pool_degraded(), "{streak} in a row is not reported");
        }
        state.pool_read_finished(Some(&failed));
        assert_eq!(state.pool_failure_streak(), 3);
        assert!(state.pool_degraded(), "the third in a row is reported");
        // A request's own refusal neither counts nor resets the streak.
        state.pool_read_finished(Some(&SqliteError::Sql(sqlite_failure(
            rusqlite::ffi::SQLITE_CONSTRAINT,
        ))));
        assert_eq!(state.pool_failure_streak(), 3);
        state.pool_read_finished(None);
        assert!(!state.pool_degraded());
    }

    #[test]
    fn the_fatal_side_of_an_operation_rollback_wins_for_either_role() {
        let rollback = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        let corrupt_operation = classify(&SqliteError::OperationRollback {
            operation: Box::new(entity_corrupt()),
            rollback,
        });
        assert_eq!(
            corrupt_operation.leaves(),
            [
                (Role::Operation, Class::Fatal(FatalCause::LogicalCorruption)),
                (Role::Rollback, Class::Transient),
            ]
        );
        assert!(corrupt_operation.is_fatal());
        assert!(corrupt_operation.savepoint_rollback_failed());

        let rollback = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None,
        );
        let access_operation = classify(&SqliteError::OperationRollback {
            operation: Box::new(SqliteError::EntityAccess(ErrorKind::NotFound)),
            rollback,
        });
        assert_eq!(
            access_operation.leaves(),
            [
                (Role::Operation, Class::Transient),
                (Role::Rollback, Class::Fatal(FatalCause::SqliteCorrupt)),
            ]
        );
        assert_eq!(
            access_operation.fatal_causes(),
            [(Role::Rollback, FatalCause::SqliteCorrupt)]
        );
    }
}
