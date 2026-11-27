//! Trusted transaction executor, not a client-facing arbitrary SQL endpoint.
//! `SqliteStorage` uses this executor; the daemon switches from files at T24.
//! Producers own event aggregation and reconstruction after confirmed rollback.
//!
//! What this executor enforces: one writing connection, claims first and then the
//! earliest deadline, a bounded queue, a request deadline checked before BEGIN and
//! again once BEGIN IMMEDIATE holds the reservation, rollback of a failed or
//! panicking body, and admission closure on `close` or on dropping the last handle.
//! It also keeps a request atomic whatever its callback does (see `Gate`): a commit
//! that is not the executor's own is refused by SQLite, and a request whose
//! transaction ended before the callback returned (SQLite's own rollback, a
//! callback's COMMIT or ROLLBACK) answers `TransactionAborted` and commits nothing,
//! even when the callback swallowed the error and went on.
//!
//! It also owns the shared fatal state (`FailureState`, issue 127, R128, R140):
//! the result of BEGIN, the body, commit and rollback is classified before it
//! reaches a receipt; a fatal cause latches the state, even one that the body
//! caught before returning success; a latched state refuses every later
//! mutation (every lane, claims included) and a body that returned success after
//! a fatal cause, or after a failed savepoint rollback, is rolled back, never
//! committed. The read pool and the checkpoint job report to the same state.
//! An explicit rollback of the top-level transaction that fails and leaves the
//! connection inside a transaction latches `FatalCause::RollbackLeftTransactionOpen`
//! before the request is answered (ruling of issue 127, R128): no further statement
//! runs on that connection, every queued and later request answers
//! `StorageUnavailable`, and the worker then ends by a panic, which releases the
//! write lock the open transaction holds. The latch is checked before
//! `TransactionAborted` is chosen, so a latched request answers `StorageUnavailable`.
//!
//! The SQLite-private `contain` boundary contains a callback that must not end the writer
//! (spec Amendment 3, issue 379; `contain`): a panic of the callback rolls the
//! request back, is answered as `CallbackPanicked` for the caller to resume, and
//! latches nothing, and the writer serves what follows. The typed unwind that a
//! leaf raises when its savepoint cleanup fails is not that panic: it is adapter
//! code, reported to the failure state (so a fatal cause latches although the
//! outer rollback succeeds) and raised again, and the `Writer::run` behaviour
//! below applies. `submit` keeps that behaviour for every panic.
//!
//! Eligible BEGIN failures retain the original FnOnce and deadline. Only BUSY
//! COMMIT on the same active transaction is retried in place; other failures
//! are cleaned up and returned with an authoritative or unknown outcome.
//! The producer-specific retained API keeps one retry episode across rebuilt
//! callbacks and requires outcome reconciliation for unknown commits. Actual
//! SQL row changes and execution time are counted once per transaction.
//! External side effects and producer chunk/coalescing bounds remain the
//! producer's responsibility. Blocking receipts are waited off async workers.
pub(super) mod contain;
pub(super) mod failure;
pub(super) mod policy;
use super::{ConnectionHandle, ConnectionRole, Result, SqliteError, Synchronous, open_connection};
use crate::data_dir::DataDir;
#[cfg(test)]
use contain::contain;
use failure::{FailureSource, FailureState, FatalCause};
use policy::{Episode, RetryCode, WriteOutcome};
use rusqlite::{DropBehavior, Transaction};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
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
        self.pop_allowed(true)
    }

    fn pop_allowed(&mut self, claims: bool) -> Option<T> {
        let index = self
            .entries
            .iter()
            .position(|e| claims && e.lane == Lane::Claims)
            .or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| claims || e.lane != Lane::Claims)
                    .min_by_key(|(_, e)| e.deadline)
                    .map(|(i, _)| i)
            })?;
        Some(self.entries.remove(index).work)
    }
}

/// What the hooks on the writer's connection share with the executor, so that
/// only the executor's own COMMIT can make a request durable. The callback gets
/// the transaction itself, and SQLite can end that transaction (a full disk, an
/// I/O error, `RAISE(ROLLBACK)`) while the callback goes on: its later
/// statements, and the release of a savepoint it opened, would then run
/// outside a transaction and commit one by one, whatever the request answers.
/// Hooks run on the worker's thread inside SQLite's own calls.
struct Gate {
    /// Armed only around the executor's COMMIT. The commit hook turns every
    /// other commit into a rollback and the statement into an error.
    committing: AtomicBool,
    /// A transaction ended by rollback since the request began. A rollback
    /// hook does not run for `ROLLBACK TO`, so a savepoint's rollback is not one.
    rolled_back: AtomicBool,
    #[cfg(test)]
    commit_probe: Mutex<Option<CommitProbe>>,
}
#[cfg(test)]
pub(in crate::sqlite) type CommitProbe =
    Box<dyn FnMut(&Transaction<'_>) -> Option<rusqlite::Result<()>> + Send>;

impl Gate {
    fn commit(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
        #[cfg(test)]
        if let Some(result) = self
            .commit_probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .and_then(|probe| probe(tx))
        {
            return result;
        }
        tx.execute_batch("COMMIT")
    }
    fn install(connection: &mut ConnectionHandle<'_>) -> Result<Arc<Self>> {
        let gate = Arc::new(Self {
            committing: AtomicBool::new(false),
            rolled_back: AtomicBool::new(false),
            #[cfg(test)]
            commit_probe: Mutex::new(None),
        });
        let connection = connection.for_migration();
        let commit = Arc::clone(&gate);
        // Returning true turns the commit into a rollback.
        connection.commit_hook(Some(move || !commit.committing.load(Ordering::SeqCst)))?;
        let rollback = Arc::clone(&gate);
        connection.rollback_hook(Some(move || {
            rollback.rolled_back.store(true, Ordering::SeqCst);
        }))?;
        Ok(gate)
    }
}

/// Run one admitted request. The deadline is read twice: before BEGIN, to skip
/// a request that expired in the queue, and again once BEGIN IMMEDIATE holds the
/// write reservation, because that statement can itself wait up to the busy
/// timeout. A request found expired at the second look is rolled back unrun and
/// answers `Overloaded`, so a request already refused at its deadline cannot
/// commit later (R128). The deadline still does not interrupt a body or commit
/// that has started. `now` is a seam so tests can place the deadline between
/// the two reads without timing luck.
///
/// The body's work commits only if the transaction it began with is still the
/// open one when it returns: otherwise whatever is open now is not the
/// request's, and the answer is `TransactionAborted`.
#[cfg(test)]
fn execute<T>(
    connection: &mut ConnectionHandle<'_>,
    gate: &Gate,
    state: &FailureState,
    deadline: Option<Instant>,
    now: impl Fn() -> Instant,
    work: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let episode = Episode::new(state.clone(), deadline);
    execute_episode(
        connection,
        gate,
        state,
        &episode,
        &now,
        &std::thread::sleep,
        work,
    )
}

fn execute_episode<T>(
    connection: &mut ConnectionHandle<'_>,
    gate: &Gate,
    state: &FailureState,
    episode: &Episode,
    now: &dyn Fn() -> Instant,
    wait: &dyn Fn(Duration),
    work: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    episode.outcome(WriteOutcome::NotStarted, None);
    episode.before_attempt(now, wait)?;
    gate.rolled_back.store(false, Ordering::SeqCst);
    state.begin_transaction();
    // A shared borrow lets an unsuccessful BEGIN release its temporary before
    // the next attempt. No statement runs concurrently on the sole writer.
    let mut transaction = loop {
        episode.check(now())?;
        match Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate) {
            Ok(transaction) => break transaction,
            Err(error) => {
                let code = RetryCode::sql(&error);
                let error = observed(state, error.into());
                let Some(code) = code else { return Err(error) };
                episode.outcome(WriteOutcome::NotStarted, Some(code));
                let delay = episode.failed(code, now())?;
                episode.pause(delay, now, wait)?;
            }
        }
    };
    state.begin_transaction();
    episode.outcome(WriteOutcome::Unknown, None);
    transaction.set_drop_behavior(DropBehavior::Ignore);
    if let Err(error) = episode.check(now()) {
        return Err(finish_abandoned(
            connection,
            gate,
            state,
            episode,
            transaction,
            error,
        ));
    }
    // The guard accounts actual SQL changes even if adapter code unwinds. It
    // outlives transaction cleanup and counts a COMMIT retry only once.
    struct Accounting<'a> {
        connection: &'a rusqlite::Connection,
        episode: &'a Episode,
        rows: u64,
        started: Instant,
        waiting: Duration,
    }
    impl Drop for Accounting<'_> {
        fn drop(&mut self) {
            self.episode.accounting(
                self.connection.total_changes().saturating_sub(self.rows),
                self.started.elapsed().saturating_sub(self.waiting),
            );
        }
    }
    let mut accounting = Accounting {
        connection,
        episode,
        rows: connection.total_changes(),
        started: Instant::now(),
        waiting: Duration::ZERO,
    };
    // Ignore was set only to avoid a second rollback after a failed explicit
    // cleanup. Restore rollback while arbitrary adapter/callback code runs.
    transaction.set_drop_behavior(DropBehavior::Rollback);
    let value = match work(&transaction) {
        Ok(value) => value,
        Err(error) => {
            state.observe(FailureSource::Writer, &error);
            let error = finish_abandoned(connection, gate, state, episode, transaction, error);
            // A failed callback is never replayed. Only a producer retaining its
            // input may reconstruct after this confirmed rollback.
            if let Some(code) = state.writer_retry() {
                // Count the failed physical attempt even without rollback-hook
                // evidence. Reconciliation settles durability, never recounts
                // the SQL failure. Unknown work takes its delay only on rebuild.
                let delay = episode.failed(code, now())?;
                if episode.snapshot().0 == WriteOutcome::RolledBack {
                    let waiting = Instant::now();
                    let paused = episode.pause(delay, now, wait);
                    accounting.waiting += waiting.elapsed();
                    paused?;
                }
            }
            return Err(error);
        }
    };
    let must_abort = state.transaction_must_abort().or_else(|| {
        gate.rolled_back
            .load(Ordering::SeqCst)
            .then_some(SqliteError::TransactionAborted)
    });
    if let Some(error) = must_abort {
        let error = finish_abandoned(connection, gate, state, episode, transaction, error);
        if let Some(code) = state.writer_retry() {
            let delay = episode.failed(code, now())?;
            if episode.snapshot().0 == WriteOutcome::RolledBack {
                let waiting = Instant::now();
                let paused = episode.pause(delay, now, wait);
                accounting.waiting += waiting.elapsed();
                paused?;
            }
        }
        return Err(error);
    }
    transaction.set_drop_behavior(DropBehavior::Ignore);
    loop {
        episode.mark_commit_attempted();
        gate.committing.store(true, Ordering::SeqCst);
        // Do not consume Transaction: a documented BUSY keeps exactly this
        // transaction active (sqlite.org/lang_transaction.html section 2.3).
        let committed = gate.commit(&transaction);
        gate.committing.store(false, Ordering::SeqCst);
        match committed {
            Ok(()) => {
                episode.outcome(WriteOutcome::Committed, None);
                state.writer_committed();
                episode.finish();
                return Ok(value);
            }
            Err(error) => {
                let code = RetryCode::sql(&error);
                let error = observed(state, error.into());
                let safe = code == Some(RetryCode::Busy)
                    && !transaction.is_autocommit()
                    && !gate.rolled_back.load(Ordering::SeqCst);
                if safe {
                    let delay = episode.failed(RetryCode::Busy, now());
                    let waiting = Instant::now();
                    let paused = delay.and_then(|delay| episode.pause(delay, now, wait));
                    accounting.waiting += waiting.elapsed();
                    if let Err(error) = paused {
                        return Err(finish_abandoned(
                            connection,
                            gate,
                            state,
                            episode,
                            transaction,
                            error,
                        ));
                    }
                } else {
                    let error =
                        finish_abandoned(connection, gate, state, episode, transaction, error);
                    if let Some(code) = code {
                        // Count once after cleanup, even when the outcome needs
                        // reconciliation; never resubmit an unknown commit.
                        episode.failed(code, now())?;
                    }
                    return Err(error);
                }
            }
        }
    }
}

fn finish_abandoned(
    connection: &ConnectionHandle<'_>,
    gate: &Gate,
    state: &FailureState,
    episode: &Episode,
    transaction: Transaction<'_>,
    error: SqliteError,
) -> SqliteError {
    let active = !transaction.is_autocommit();
    let abandoned = abandon(state, transaction, error);
    let known = gate.rolled_back.load(Ordering::SeqCst) || (active && !abandoned.rollback_failed);
    episode.outcome(
        if known {
            WriteOutcome::RolledBack
        } else {
            WriteOutcome::Unknown
        },
        state.writer_retry(),
    );
    abandoned.settle(connection, state)
}

fn observed(state: &FailureState, error: SqliteError) -> SqliteError {
    state.observe(FailureSource::Writer, &error);
    error
}

/// The end of an abandoned request: its error, and whether its explicit rollback failed.
struct Abandoned {
    error: SqliteError,
    rollback_failed: bool,
}

impl Abandoned {
    /// The request's `error` and the result of the rollback made for it. A failed
    /// rollback keeps both causes in one `OperationRollback`.
    fn after_rollback(
        state: &FailureState,
        error: SqliteError,
        rollback: rusqlite::Result<()>,
    ) -> Self {
        match rollback {
            Ok(()) => Self {
                error,
                rollback_failed: false,
            },
            Err(rollback) => Self {
                error: observed(
                    state,
                    SqliteError::OperationRollback {
                        operation: Box::new(error),
                        rollback,
                    },
                ),
                rollback_failed: true,
            },
        }
    }

    /// A failed rollback that leaves the connection inside a transaction fails
    /// the writer in the shared state before the request is answered (R128). A
    /// rollback error that leaves the connection outside a transaction does not
    /// latch. `Transaction` borrows the connection, so this check follows its
    /// consumption in `abandon`.
    fn settle(self, connection: &ConnectionHandle<'_>, state: &FailureState) -> SqliteError {
        if self.rollback_failed && !connection.is_autocommit() {
            state.latch(
                FailureSource::Writer,
                FatalCause::RollbackLeftTransactionOpen,
            );
        }
        self.error
    }
}

/// Roll back, classifying the rollback's own result independently of `error`.
fn abandon(
    state: &FailureState,
    mut transaction: Transaction<'_>,
    error: SqliteError,
) -> Abandoned {
    // SQLite can end the transaction itself (RAISE(ROLLBACK), a failed statement
    // under some conflict clauses): there is then nothing left to roll back.
    if transaction.is_autocommit() {
        return Abandoned {
            error,
            rollback_failed: false,
        };
    }
    // `Drop` would retry a failed rollback once and ignore its result: R128 allows
    // no further statement on a connection whose rollback failed, so it must not.
    transaction.set_drop_behavior(DropBehavior::Ignore);
    Abandoned::after_rollback(state, error, transaction.rollback())
}

type Job = Box<dyn FnOnce(&mut ConnectionHandle<'_>, &Gate) + Send>;

struct State {
    #[cfg(test)]
    park_generation: u64,
    accepting: bool,
    queue: Schedule<Job>,
    // A retained logical operation still owns the executor's in-progress slot
    // between producer reconstructions. Its continuation cannot be trapped
    // behind a full queue of claims that this retry must suppress.
    continuation: Option<Job>,
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
    gate: Arc<Gate>,
    shared: Arc<Shared>,
    state: FailureState,
}

#[derive(Clone)]
pub struct WriterHandle {
    admission: Arc<Admission>,
    state: FailureState,
}

// Only client handles own Admission. The worker owns Shared separately, so
// dropping the final client closes admission even on an early return/unwind.
struct Admission {
    shared: Arc<Shared>,
}

impl Admission {
    fn close(&self) {
        // Runs from Drop, possibly during an unwind: a poisoned lock must not
        // become a second panic. No callback ever runs under this lock.
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
/// are checked by the worker before BEGIN and again once BEGIN IMMEDIATE holds
/// the write reservation, so an expired request cannot later commit, even one
/// that waited on another connection's write lock. A deadline does not
/// interrupt a body or commit that has already started.
pub struct WriteReceipt<T> {
    response: mpsc::Receiver<Result<T>>,
    state: FailureState,
}

impl<T> WriteReceipt<T> {
    pub fn wait(self) -> Result<T> {
        self.response.recv().unwrap_or_else(|_| Err(self.dropped()))
    }

    /// The answer of a request whose job was dropped unanswered, because the worker
    /// is gone. A worker that failed latched the state first, so the latched failure
    /// wins over the ordinary-shutdown answer.
    fn dropped(&self) -> SqliteError {
        self.state
            .ensure_available()
            .err()
            .unwrap_or(SqliteError::WriterClosed)
    }
}

impl<'a> Writer<'a> {
    /// A writer with a failure state of its own. Share one state with the read
    /// pool and the checkpoint job through `open_with_state`.
    pub fn open(
        data: &'a DataDir,
        sync: Synchronous,
        capacity: usize,
    ) -> Result<(Self, WriterHandle)> {
        Self::open_with_state(data, sync, capacity, FailureState::new())
    }

    pub fn open_with_state(
        data: &'a DataDir,
        sync: Synchronous,
        capacity: usize,
        state: FailureState,
    ) -> Result<(Self, WriterHandle)> {
        if capacity == 0 {
            return Err(SqliteError::InvalidCapacity);
        }
        let mut connection = open_connection(data, ConnectionRole::Writer, sync)?;
        let gate = Gate::install(&mut connection)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                #[cfg(test)]
                park_generation: 0,
                accepting: true,
                queue: Schedule::new(capacity),
                continuation: None,
            }),
            available: Condvar::new(),
        });
        let handle = WriterHandle {
            admission: Arc::new(Admission {
                shared: Arc::clone(&shared),
            }),
            state: state.clone(),
        };
        Ok((
            Self {
                connection,
                gate,
                shared,
                state,
            },
            handle,
        ))
    }

    /// Drain accepted work, then return once admission is closed, either by
    /// `WriterHandle::close` or by dropping the final client handle. A panic of a callback
    /// submitted with `submit`, or the unwind of a leaf's failed savepoint cleanup under
    /// either entry, rolls back its transaction and drops this worker, closing every queued
    /// receipt (`contain` contains the former and observes the latter).
    /// So does a request that leaves the connection inside a transaction (a failed
    /// rollback, latched as `FatalCause::RollbackLeftTransactionOpen` before the
    /// answer): the worker refuses what is queued with `StorageUnavailable`, then
    /// panics, and the connection is dropped, never served again. SQL/body errors go
    /// to that receipt; this foundation does not replay work.
    pub fn run(mut self) {
        loop {
            let job = {
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .expect("writer queue lock poisoned");
                loop {
                    self.state.check_retry_timeout(Instant::now());
                    if let Some(job) = state.continuation.take() {
                        break Some(job);
                    }
                    if (!self.state.claims_suppressed() || self.state.failure().is_some())
                        && let Some(job) = state.queue.pop()
                    {
                        break Some(job);
                    }
                    if !state.accepting {
                        // No running SQL remains: a retained producer can no
                        // longer submit. Close queued receipts on worker drop.
                        break None;
                    }
                    #[cfg(test)]
                    {
                        state.park_generation += 1;
                        self.shared.available.notify_all();
                    }
                    state = self
                        .shared
                        .available
                        .wait_timeout(state, Duration::from_millis(50))
                        .expect("writer queue lock poisoned")
                        .0;
                }
            };
            match job {
                Some(job) => {
                    job(&mut self.connection, &self.gate);
                    if !self.connection.is_autocommit() {
                        self.fail();
                    }
                }
                None => break,
            }
        }
    }

    #[cfg(test)]
    pub(in crate::sqlite) fn set_commit_probe(&mut self, probe: CommitProbe) {
        *self
            .gate
            .commit_probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(probe);
    }

    /// The writer's own connection, for a test that injects a failure into it.
    #[cfg(test)]
    pub(in crate::sqlite) fn connection_for_test(&mut self) -> &mut rusqlite::Connection {
        self.connection.for_migration()
    }

    /// The request has its answer, and a rollback that failed left this connection
    /// inside the transaction: every later request would fail to begin and nothing
    /// could commit. When that failure latched the state, what is queued is
    /// refused first, as a latched writer refuses everything: the gate in `execute`
    /// answers `storage_unavailable` before any statement runs. Then the worker
    /// fails, and dropping it closes the connection and releases the write lock.
    fn fail(&mut self) -> ! {
        if self.state.failure().is_some() {
            loop {
                let job = self
                    .shared
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .queue
                    .pop();
                match job {
                    Some(job) => job(&mut self.connection, &self.gate),
                    None => break,
                }
            }
        }
        panic!("SQLite writer connection is still inside a transaction after a request");
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        // rusqlite 0.41.1 frees a hook's closure when the connection is closed,
        // before SQLite rolls back a transaction that is still open, and that
        // rollback then calls the freed closure (a segfault, reproduced with
        // an in-memory database). A writer dropped inside a transaction is the
        // failed-rollback case, so the hooks go first.
        let connection = self.connection.for_migration();
        let _ = connection.rollback_hook(None::<fn()>);
        let _ = connection.commit_hook(None::<fn() -> bool>);
        let abandoned = {
            // Drop runs during the unwind of a panicking callback; see Admission::close.
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.accepting = false;
            (
                std::mem::take(&mut state.queue.entries),
                state.continuation.take(),
            )
        };
        // A captured value's destructor must never run while holding admission.
        drop(abandoned);
        self.shared.available.notify_all();
    }
}

impl WriterHandle {
    /// The state this writer shares. A callback captures a clone to report what it catches.
    pub fn failure_state(&self) -> FailureState {
        self.state.clone()
    }

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
        self.submit_after(lane, deadline, work, || {})
    }

    /// `submit`, running `after_first_check` between the unlocked latch check and
    /// the admission lock: a seam so tests can place a failure and a closed door
    /// in that window without timing luck.
    fn submit_after<T, F>(
        &self,
        lane: Lane,
        deadline: Option<Instant>,
        work: F,
        after_first_check: impl FnOnce(),
    ) -> Result<WriteReceipt<T>>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    {
        self.submit_episode(
            lane,
            Episode::new(self.state.clone(), deadline),
            work,
            after_first_check,
        )
    }

    pub(in crate::sqlite) fn submit_episode<T, F>(
        &self,
        lane: Lane,
        episode: Episode,
        work: F,
        after_first_check: impl FnOnce(),
    ) -> Result<WriteReceipt<T>>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    {
        // Refuse at the door once latched: nothing is queued behind a failed writer.
        self.state.ensure_available()?;
        after_first_check();
        episode.prepare();
        let (response, receiver) = mpsc::sync_channel(1);
        let state = self.state.clone();
        let continuation = episode.retrying();
        let job: Job = Box::new(move |connection, gate| {
            if lane == Lane::Claims
                && state.claims_suppressed()
                && !continuation
                && state.failure().is_none()
            {
                let _ = response.send(Err(SqliteError::WriterClosed));
                return;
            }
            let _ = response.send(execute_episode(
                connection,
                gate,
                &state,
                &episode,
                &Instant::now,
                &std::thread::sleep,
                work,
            ));
        });
        let mut state = self
            .admission
            .shared
            .state
            .lock()
            .expect("writer queue lock poisoned");
        // The worker latches a failure before it closes admission, so a closed
        // door that raced the check above is read again: a latched writer is
        // never reported as an ordinary shutdown. Captures drop outside the lock.
        if let Err(error) = self.state.ensure_available() {
            drop(state);
            drop(job);
            return Err(error);
        }
        if !state.accepting {
            // Drop user captures outside the queue lock, including on refusal.
            drop(state);
            drop(job);
            return Err(SqliteError::WriterClosed);
        }
        if continuation {
            if state.continuation.is_some() {
                drop(state);
                drop(job);
                return Err(SqliteError::Overloaded);
            }
            state.continuation = Some(job);
            self.admission.shared.available.notify_one();
            return Ok(WriteReceipt {
                response: receiver,
                state: self.state.clone(),
            });
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
        Ok(WriteReceipt {
            response: receiver,
            state: self.state.clone(),
        })
    }

    /// `submit` for a callback whose own panic must not end the writer (spec
    /// Amendment 3, issue 379; `contain`): the panic rolls the request back and is
    /// answered as `CallbackPanicked`, which the caller resumes, and the writer
    /// serves what follows with no cause latched. The unwind of a leaf whose
    /// savepoint cleanup failed is not contained: it is reported to the failure
    /// state (a fatal cause latches) and raised again, and the worker ends as it
    /// does for any panic outside the callback.
    ///
    /// Supply only the caller's callback here, never adapter setup, a setup
    /// clock read or completion checks. Storage instead uses the SQLite-private
    /// `contain` inside its `submit` job around the user `WriteWork` alone.
    #[cfg(test)]
    pub(in crate::sqlite) fn submit_contained<T, F>(
        &self,
        lane: Lane,
        deadline: Option<Instant>,
        work: F,
    ) -> Result<WriteReceipt<T>>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    {
        let state = self.state.clone();
        self.submit(lane, deadline, move |tx| contain(&state, || work(tx)))
    }

    /// Close admission and drain previously accepted transactions. Idempotent.
    pub fn close(&self) {
        self.admission.close();
    }
}

#[cfg(test)]
mod tests;
