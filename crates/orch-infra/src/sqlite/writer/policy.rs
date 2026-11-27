//! One logical write episode. Only the writer may record its SQL outcomes.
use super::{FailureSource, FailureState, FatalCause};
use crate::sqlite::{Result, SqliteError};
use rusqlite::ErrorCode;
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
    pub(super) fn error(error: &SqliteError) -> Option<Self> {
        match error {
            SqliteError::Sql(error) => Self::sql(error),
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

/// Actual SQL changes and execution time for one indivisible transaction.
/// COMMIT retries are part of this transaction, never another accounting event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteAccounting {
    pub rows: u64,
    pub work: Duration,
    pub over_bound: bool,
}

struct Progress {
    since: Option<Instant>,
    delay: Duration,
    ioerr: bool,
    outcome: WriteOutcome,
    code: Option<RetryCode>,
    accounting: WriteAccounting,
    pending_delay: Option<Duration>,
}
struct Inner {
    state: FailureState,
    progress: Mutex<Progress>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(since) = self
            .progress
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .since
        {
            self.state.retry_finished(since);
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
                progress: Mutex::new(Progress {
                    since: None,
                    delay: Duration::from_millis(50),
                    ioerr: false,
                    outcome: WriteOutcome::NotStarted,
                    code: None,
                    accounting: WriteAccounting::default(),
                    pending_delay: None,
                }),
            }),
            deadline,
        }
    }
    pub(super) fn retrying(&self) -> bool {
        self.inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .since
            .is_some()
    }
    pub(super) fn prepare(&self) {
        let mut p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        p.outcome = WriteOutcome::NotStarted;
        p.code = None;
        p.accounting = WriteAccounting::default();
    }
    pub(in crate::sqlite) fn snapshot(&self) -> (WriteOutcome, Option<RetryCode>, WriteAccounting) {
        let p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (p.outcome, p.code, p.accounting)
    }
    pub(super) fn outcome(&self, outcome: WriteOutcome, code: Option<RetryCode>) {
        let mut p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        p.outcome = outcome;
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
        if let Some(since) = p.since.take() {
            self.inner.state.retry_finished(since);
        }
        p.ioerr = false;
    }
    fn check_policy(&self, now: Instant) -> Result<()> {
        self.inner.state.ensure_available()?;
        let p = self
            .inner
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if p.since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(60))
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
            self.inner.state.retry_started(now);
        }
        if code == RetryCode::Io && p.ioerr {
            self.inner
                .state
                .latch(FailureSource::Writer, FatalCause::ConsecutiveIoErrors);
        }
        p.ioerr = code == RetryCode::Io;
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
        assert!(queue.pop_allowed(!state.claims_suppressed()).is_none());
        assert!(matches!(
            episode.check(now + Duration::from_millis(75)),
            Err(SqliteError::Overloaded)
        ));
        drop(episode);
        assert_eq!(queue.pop_allowed(!state.claims_suppressed()), Some(1));
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
}
