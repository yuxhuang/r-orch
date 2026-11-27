//! One logical write episode. Only the writer may record its SQL outcomes.
//!
//! An episode is identified by itself (its `active` flag), never by the clock
//! reading of its first failure: episodes that first fail at the same instant
//! register and end independently (issue 448).
//!
//! Supported producer cardinality (issue 448): **one retained producer** per
//! writer, which is the event producer. The executor has a single continuation
//! slot for the rebuild of a retained write, and its Events lane admits one
//! queued entry. A second retained write whose rebuild finds the slot taken is
//! refused `Overloaded` with its episode still live: it may resubmit once the
//! worker has taken the first rebuild, and until it ends its registration keeps
//! the writer's lanes held. That is retryable contention, not damage. It would
//! become a restart-only failure only through the ordinary rule that an episode
//! open for 60 s without a commit latches `RetryWithoutCommit`. Several
//! concurrent retained producers are unsupported: a design that adds one gives
//! each its own slot (or a bounded queue) and does not rely on this refusal.
use super::{FailureSource, FailureState, FatalCause};
use crate::sqlite::{Result, SqliteError};
use rusqlite::ErrorCode;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Retryable SQLite primary result, retained before port errors become strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryCode {
    Busy,
    Locked,
    Full,
    Io,
}
impl RetryCode {
    pub(super) fn sql(error: &rusqlite::Error) -> Option<Self> {
        let rusqlite::Error::SqliteFailure(error, _) = error else {
            return None;
        };
        match error.code {
            ErrorCode::DatabaseBusy => Some(Self::Busy),
            ErrorCode::DatabaseLocked => Some(Self::Locked),
            ErrorCode::DiskFull => Some(Self::Full),
            ErrorCode::SystemIoFailure => Some(Self::Io),
            _ => None,
        }
    }
}

/// Durability of an attempt, established by the executor rather than autocommit alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    NotStarted,
    Committed,
    RolledBack,
    Unknown,
}

/// Actual SQL changes and execution time for one physical transaction, its COMMIT
/// included. It is over the bound when it changed more than 1,000 rows or worked
/// longer than 20 ms, whether or not a producer could have divided it. COMMIT
/// retries are part of this transaction, never another accounting event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteAccounting {
    pub rows: u64,
    pub work: Duration,
    pub over_bound: bool,
}

struct Progress {
    since: Option<Instant>,
    delay: Duration,
    outcome: WriteOutcome,
    code: Option<RetryCode>,
    accounting: WriteAccounting,
    commit_attempted: bool,
    pending_delay: Option<Duration>,
}
struct Inner {
    state: FailureState,
    progress: Mutex<Progress>,
    active: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        if self
            .progress
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .since
            .is_some()
        {
            self.state.retry_finished(&self.active);
        }
    }
}
#[derive(Clone)]
pub(in crate::sqlite) struct Episode {
    inner: Arc<Inner>,
    pub(in crate::sqlite) deadline: Option<Instant>,
}
impl Episode {
    pub(in crate::sqlite) fn new(state: FailureState, deadline: Option<Instant>) -> Self {
        Self {
            inner: Arc::new(Inner {
                state,
                active: Arc::new(AtomicBool::new(false)),
                phase: Arc::new(AtomicU8::new(WriteOutcome::NotStarted as u8)),
                progress: Mutex::new(Progress {
                    since: None,
                    delay: Duration::from_millis(50),
                    outcome: WriteOutcome::NotStarted,
                    code: None,
                    accounting: WriteAccounting::default(),
                    commit_attempted: false,
                    pending_delay: None,
                }),
            }),
            deadline,
        }
    }
    pub(super) fn retrying(&self) -> bool {
        self.inner.active.load(Ordering::SeqCst)
    }
    pub(super) fn prepare(&self) {
        let mut p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        p.outcome = WriteOutcome::NotStarted;
        self.inner
            .phase
            .store(WriteOutcome::NotStarted as u8, Ordering::SeqCst);
        p.code = None;
        p.accounting = WriteAccounting::default();
        p.commit_attempted = false;
    }
    pub(super) fn mark_commit_attempted(&self) {
        self.inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit_attempted = true;
    }
    pub(in crate::sqlite) fn commit_attempted(&self) -> bool {
        self.inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit_attempted
    }
    pub(in crate::sqlite) fn snapshot(&self) -> (WriteOutcome, Option<RetryCode>, WriteAccounting) {
        let p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (p.outcome, p.code, p.accounting)
    }
    pub(in crate::sqlite) fn outcome(&self, outcome: WriteOutcome, code: Option<RetryCode>) {
        let mut p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        p.outcome = outcome;
        self.inner.phase.store(outcome as u8, Ordering::SeqCst);
        p.code = code;
    }
    pub(super) fn accounting(&self, rows: u64, work: Duration) {
        let over_bound = rows > 1000 || work > Duration::from_millis(20);
        self.inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .accounting = WriteAccounting {
            rows,
            work,
            over_bound,
        };
        if over_bound {
            self.inner.state.count_over_bound();
        }
    }
    pub(in crate::sqlite) fn finish(&self) {
        let mut p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if p.since.take().is_some() {
            self.inner.state.retry_finished(&self.inner.active);
        }
    }
    fn check_policy(&self, now: Instant) -> Result<()> {
        self.inner.state.ensure_available()?;
        let p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.inner.active.load(Ordering::SeqCst)
            && p.since.is_some_and(|since| {
                now.saturating_duration_since(since) >= Duration::from_secs(60)
            })
        {
            self.inner
                .state
                .latch(FailureSource::Writer, FatalCause::RetryWithoutCommit);
            return self.inner.state.ensure_available();
        }
        Ok(())
    }
    pub(super) fn check(&self, now: Instant) -> Result<()> {
        self.check_policy(now)?;
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Err(SqliteError::Overloaded);
        }
        Ok(())
    }
    /// Count one SQL failure, not every conversion/observation of that failure.
    pub(super) fn failed(&self, code: RetryCode, now: Instant) -> Result<Duration> {
        let mut p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        p.code = Some(code);
        if p.since.is_none() {
            p.since = Some(now);
            self.inner.state.retry_started(
                now,
                self.deadline,
                self.inner.active.clone(),
                self.inner.phase.clone(),
            );
        }
        if self.inner.state.retry_failure(code) {
            self.inner
                .state
                .latch(FailureSource::Writer, FatalCause::ConsecutiveIoErrors);
        }
        let delay = p.delay;
        p.delay = (delay * 2).min(Duration::from_secs(2));
        p.pending_delay = Some(delay);
        drop(p);
        self.check_policy(now)?;
        Ok(delay)
    }
    pub(super) fn before_attempt(
        &self,
        now: &dyn Fn() -> Instant,
        wait: &dyn Fn(Duration),
    ) -> Result<()> {
        let pending = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending_delay;
        if let Some(delay) = pending {
            self.pause(delay, now, wait)?;
        }
        Ok(())
    }
    pub(super) fn pause(
        &self,
        delay: Duration,
        now: &dyn Fn() -> Instant,
        wait: &dyn Fn(Duration),
    ) -> Result<()> {
        self.inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending_delay = None;
        // Check withdrawal and failure at most 50 ms apart, without renewing the deadline.
        let mut remaining = delay;
        while !remaining.is_zero() {
            self.check(now())?;
            let piece = remaining.min(Duration::from_millis(50));
            let piece = self
                .deadline
                .map_or(piece, |d| piece.min(d.saturating_duration_since(now())));
            wait(piece);
            remaining = remaining.saturating_sub(piece);
        }
        self.check(now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backoff_caps_and_sixty_second_episode_excludes_idle_time() {
        let now = Instant::now();
        let state = FailureState::new();
        let episode = Episode::new(state.clone(), None);
        episode.check(now + Duration::from_secs(600)).unwrap();
        assert!(state.failure().is_none());
        let start = now + Duration::from_secs(600);
        for (i, ms) in [50, 100, 200, 400, 800, 1600, 2000, 2000]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                episode
                    .failed(RetryCode::Full, start + Duration::from_secs(i as u64))
                    .unwrap(),
                Duration::from_millis(ms)
            );
        }
        assert!(state.claims_suppressed());
        state.check_retry_timeout(start + Duration::from_secs(59));
        assert!(state.failure().is_none());
        state.check_retry_timeout(start + Duration::from_secs(60));
        assert_eq!(
            state.failure().unwrap().causes[0].1,
            FatalCause::RetryWithoutCommit
        );
    }
    #[test]
    fn consecutive_io_errors_span_submissions_but_busy_breaks_the_streak() {
        let state = FailureState::new();
        let episode = Episode::new(state.clone(), None);
        let now = Instant::now();
        episode.failed(RetryCode::Io, now).unwrap();
        episode.failed(RetryCode::Busy, now).unwrap();
        episode.failed(RetryCode::Io, now).unwrap();
        assert!(state.failure().is_none());
        assert!(episode.failed(RetryCode::Io, now).is_err());
        assert_eq!(
            state.failure().unwrap().causes[0].1,
            FatalCause::ConsecutiveIoErrors
        );
        state.latch(FailureSource::Writer, FatalCause::RetryWithoutCommit);
        assert_eq!(
            state.failure().unwrap().causes[0].1,
            FatalCause::ConsecutiveIoErrors
        );
    }
    #[test]
    fn expiry_withdraws_before_another_attempt_and_drop_releases_claims() {
        let state = FailureState::new();
        let now = Instant::now();
        let episode = Episode::new(state.clone(), Some(now + Duration::from_millis(75)));
        let mut queue = super::super::Schedule::new(2);
        queue.push(super::super::Lane::Claims, now, 1).unwrap();
        episode.failed(RetryCode::Busy, now).unwrap();
        // The worker takes nothing from the queue while a retry is registered.
        assert!(state.claims_suppressed());
        assert!(matches!(
            episode.check(now + Duration::from_millis(75)),
            Err(SqliteError::Overloaded)
        ));
        drop(episode);
        assert!(!state.claims_suppressed());
        assert_eq!(queue.pop(), Some(1));
    }
    #[test]
    fn reader_and_artifact_sql_failures_cannot_contaminate_transaction_observation() {
        let state = FailureState::new();
        let error = SqliteError::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(10),
            None,
        ));
        state.begin_transaction();
        state.observe(FailureSource::Reader, &error);
        state.observe(FailureSource::Artifact, &error);
        assert_eq!(state.writer_retry(), None);
        state.observe(FailureSource::Writer, &error);
        assert_eq!(state.writer_retry(), Some(RetryCode::Io));
        state.begin_transaction();
        assert_eq!(state.writer_retry(), None);
    }
    #[test]
    fn expired_known_noncommit_releases_claims_between_producer_submissions() {
        let state = FailureState::new();
        let now = Instant::now();
        let episode = Episode::new(state.clone(), Some(now + Duration::from_millis(200)));
        episode.outcome(WriteOutcome::RolledBack, Some(RetryCode::Full));
        episode.failed(RetryCode::Full, now).unwrap();
        state.check_retry_timeout(now + Duration::from_millis(199));
        assert!(state.claims_suppressed());
        state.check_retry_timeout(now + Duration::from_millis(200));
        assert!(
            !state.claims_suppressed(),
            "known-noncommit withdrawal must release the queue without another submission"
        );
        assert!(state.failure().is_none());
        assert!(matches!(
            episode.check(now + Duration::from_millis(200)),
            Err(SqliteError::Overloaded)
        ));
        assert!(matches!(
            episode.check(now + Duration::from_secs(61)),
            Err(SqliteError::Overloaded)
        ));
        assert!(
            state.failure().is_none(),
            "a withdrawn episode does not become a sixty-second failure later"
        );
        drop(episode);
        assert!(
            !state.claims_suppressed(),
            "expiry followed by Drop must not decrement twice"
        );
    }
    #[test]
    fn unknown_commit_remains_fenced_past_the_original_deadline() {
        let state = FailureState::new();
        let now = Instant::now();
        let episode = Episode::new(state.clone(), Some(now + Duration::from_millis(200)));
        episode.outcome(WriteOutcome::Unknown, Some(RetryCode::Io));
        episode.failed(RetryCode::Io, now).unwrap();
        state.check_retry_timeout(now + Duration::from_millis(201));
        assert!(
            state.claims_suppressed(),
            "uncertainty is not an overloaded/noncommit receipt"
        );
        assert!(state.failure().is_none());
        episode.finish();
        assert!(!state.claims_suppressed());
    }
    #[test]
    fn consecutive_io_errors_of_separate_logical_requests_still_latch() {
        let state = FailureState::new();
        let now = Instant::now();
        {
            let first = Episode::new(state.clone(), None);
            first.failed(RetryCode::Io, now).unwrap();
        }
        assert!(!state.claims_suppressed());
        let second = Episode::new(state.clone(), None);
        assert!(
            second.failed(RetryCode::Io, now).is_err(),
            "ending a logical request cannot reset the writer IOERR streak"
        );
        assert_eq!(
            state.failure().unwrap().causes[0].1,
            FatalCause::ConsecutiveIoErrors
        );
    }
    #[test]
    fn a_different_sql_failure_breaks_the_consecutive_ioerr_streak() {
        let state = FailureState::new();
        let now = Instant::now();
        {
            let first = Episode::new(state.clone(), None);
            first.failed(RetryCode::Io, now).unwrap();
        }
        state.observe(
            FailureSource::Writer,
            &SqliteError::Sql(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(19),
                None,
            )),
        );
        let second = Episode::new(state.clone(), None);
        second
            .failed(RetryCode::Io, now)
            .expect("a different SQL failure interrupts consecutive IOERRs");
        assert!(state.failure().is_none());
    }
    // Issue 448. A registration belongs to its episode, not to the clock reading
    // of its first failure. The clock is controlled, so both episodes fail at
    // exactly the same instant, and each order of ending is checked in turn. The
    // oracle is separate for the two things that went wrong: the surviving
    // episode's own flag (its continuation eligibility) and the global registry
    // that suppresses claims, which stays nonempty after the first removal.
    #[test]
    fn episodes_that_first_fail_at_the_same_instant_end_independently() {
        for finish_by_drop in [false, true] {
            for earlier_ends_first in [false, true] {
                let state = FailureState::new();
                let now = Instant::now();
                let earlier = Episode::new(state.clone(), None);
                let later = Episode::new(state.clone(), None);
                earlier.failed(RetryCode::Busy, now).unwrap();
                later.failed(RetryCode::Busy, now).unwrap();
                assert!(earlier.retrying() && later.retrying());
                assert!(state.claims_suppressed());
                let (ending, remaining) = if earlier_ends_first {
                    (earlier, later)
                } else {
                    (later, earlier)
                };
                let case = format!("drop={finish_by_drop} earlier_first={earlier_ends_first}");
                if finish_by_drop {
                    drop(ending);
                } else {
                    ending.finish();
                    assert!(!ending.retrying(), "{case}");
                }
                assert!(
                    remaining.retrying(),
                    "{case}: the other episode lost its own flag"
                );
                assert!(
                    state.claims_suppressed(),
                    "{case}: the registry is empty after the first removal"
                );
                remaining.finish();
                assert!(!remaining.retrying(), "{case}");
                assert!(!state.claims_suppressed(), "{case}");
                assert!(state.failure().is_none(), "{case}");
            }
        }
    }
    #[test]
    fn a_withdrawn_episode_leaves_the_same_instant_registration_of_another() {
        let state = FailureState::new();
        let now = Instant::now();
        let bounded = Episode::new(state.clone(), Some(now + Duration::from_millis(100)));
        let open = Episode::new(state.clone(), None);
        // The unbounded episode is registered first, so a lookup by instant finds it.
        open.failed(RetryCode::Busy, now).unwrap();
        bounded.failed(RetryCode::Busy, now).unwrap();
        bounded.outcome(WriteOutcome::RolledBack, Some(RetryCode::Busy));
        state.check_retry_timeout(now + Duration::from_millis(100));
        assert!(!bounded.retrying(), "expired known noncommit is withdrawn");
        assert!(open.retrying() && state.claims_suppressed());
        bounded.finish();
        assert!(
            open.retrying() && state.claims_suppressed(),
            "ending a withdrawn episode removes nothing of another"
        );
        open.finish();
        assert!(!state.claims_suppressed());
    }
}
