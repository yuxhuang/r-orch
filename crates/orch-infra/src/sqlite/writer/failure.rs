//! The shared fatal state of the SQLite backend (issue 127; spec R128, R140, R141).
//!
//! One `FailureState` is shared by the writer, the read pool and every other
//! live connection. Whoever sees a result that proves the live database or its
//! data damaged reports it through `observe`, at the place where the error is
//! produced and before it is converted to a public error. The first fatal
//! observation latches the state until the process restarts: the writer then
//! refuses every mutation, no claim is granted, and a checkpoint is not called
//! (`ensure_available` is the one gate for all three).
//!
//! What is fatal is decided here, by the structure of the error, and nowhere
//! else. `classify` matches every `SqliteError` variant with no wildcard, so a
//! new variant (the structured `TaskCorrupt` of R141, for one) cannot be added
//! without a decision about its class. Task corruption and the structured
//! corruption of a Context, Config, Template or Goal (`EntityCorrupt`) are fatal;
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
        // R140 names SQLITE_CORRUPT, SQLITE_NOTADB and `TaskCorrupt` as fatal.
        // The stored-data errors below are not among them.
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
        | Store(_)
        | WriterClosed
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

    /// The gate for a mutation, a claim and a checkpoint: refuses once latched.
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

    pub(super) fn begin_transaction(&self) {
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

    #[test]
    fn only_a_writer_observation_aborts_the_enclosing_transaction() {
        let writer = FailureState::new();
        writer.begin_transaction();
        writer.observe(FailureSource::Writer, &entity_corrupt());
        // `ensure_available` already fails once latched; the abort flag is its own bit.
        assert!(writer.inner.transaction_aborted.load(Ordering::SeqCst));
        assert!(matches!(
            writer.transaction_must_abort(),
            Some(SqliteError::StorageUnavailable(_))
        ));

        let reader = FailureState::new();
        reader.observe(FailureSource::Reader, &entity_corrupt());
        assert!(!reader.inner.transaction_aborted.load(Ordering::SeqCst));
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
