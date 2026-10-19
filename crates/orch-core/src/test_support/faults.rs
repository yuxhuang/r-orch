//! Failure injection — turning store calls into errors on demand.
//!
//! The orchestrator resilience tests turn store calls into rejections to check
//! that a tick survives them. Every fake runs its calls
//! through a [`Faults`] so those tests have one way to do it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use orch_domain::DomainError;

use crate::error::{Error, Result};

#[derive(Debug, Default)]
struct FaultState {
    /// Fails every call until cleared.
    persistent: Option<DomainError>,
    /// Fails the next N calls, one per entry.
    queued: VecDeque<DomainError>,
}

/// Shared, cloneable failure injection for a test double.
#[derive(Debug, Clone, Default)]
pub struct Faults {
    state: Arc<Mutex<FaultState>>,
}

impl Faults {
    pub fn new() -> Self {
        Faults::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FaultState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fail every call from now on with `DomainError::Other(message)`.
    pub fn fail_always(&self, message: impl Into<String>) {
        self.fail_always_with(DomainError::Other(message.into()));
    }

    pub fn fail_always_with(&self, error: DomainError) {
        self.lock().persistent = Some(error);
    }

    /// Fail only the next call. Calling it repeatedly queues several failures.
    pub fn fail_once(&self, message: impl Into<String>) {
        self.fail_once_with(DomainError::Other(message.into()));
    }

    pub fn fail_once_with(&self, error: DomainError) {
        self.lock().queued.push_back(error);
    }

    /// Fail the next `count` calls with the same message.
    pub fn fail_next(&self, count: usize, message: impl Into<String>) {
        let message = message.into();
        let mut state = self.lock();
        for _ in 0..count {
            state.queued.push_back(DomainError::Other(message.clone()));
        }
    }

    pub fn clear(&self) {
        let mut state = self.lock();
        state.persistent = None;
        state.queued.clear();
    }

    /// Whether any failure is still pending.
    pub fn is_armed(&self) -> bool {
        let state = self.lock();
        state.persistent.is_some() || !state.queued.is_empty()
    }

    /// Called by a fake at the top of every method. Queued failures are
    /// consumed first, then the persistent one applies.
    pub fn check(&self) -> Result<()> {
        let mut state = self.lock();
        if let Some(error) = state.queued.pop_front() {
            return Err(Error::Domain(error));
        }
        match &state.persistent {
            Some(error) => Err(Error::Domain(error.clone())),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_faults_lets_every_call_through() {
        let faults = Faults::new();
        assert!(!faults.is_armed());
        assert!(faults.check().is_ok());
        assert!(faults.check().is_ok());
    }

    #[test]
    fn fail_once_affects_only_the_next_call() {
        let faults = Faults::new();
        faults.fail_once("disk full");
        assert!(faults.is_armed());

        let err = faults.check().expect_err("first call fails");
        assert_eq!(err.to_string(), "disk full");
        assert_eq!(err.exit_code(), 1);
        assert!(faults.check().is_ok());
        assert!(!faults.is_armed());
    }

    #[test]
    fn queued_failures_are_consumed_in_order() {
        let faults = Faults::new();
        faults.fail_once("first");
        faults.fail_once("second");
        assert_eq!(faults.check().expect_err("1").to_string(), "first");
        assert_eq!(faults.check().expect_err("2").to_string(), "second");
        assert!(faults.check().is_ok());
    }

    #[test]
    fn fail_next_queues_n_identical_failures() {
        let faults = Faults::new();
        faults.fail_next(2, "flaky");
        assert!(faults.check().is_err());
        assert!(faults.check().is_err());
        assert!(faults.check().is_ok());
    }

    #[test]
    fn fail_always_persists_until_cleared() {
        let faults = Faults::new();
        faults.fail_always("gone");
        for _ in 0..3 {
            assert_eq!(faults.check().expect_err("fails").to_string(), "gone");
        }
        faults.clear();
        assert!(faults.check().is_ok());
    }

    #[test]
    fn queued_failures_take_priority_over_the_persistent_one() {
        let faults = Faults::new();
        faults.fail_always("persistent");
        faults.fail_once("queued");
        assert_eq!(
            faults.check().expect_err("queued first").to_string(),
            "queued"
        );
        assert_eq!(
            faults.check().expect_err("then persistent").to_string(),
            "persistent"
        );
    }

    #[test]
    fn a_typed_domain_error_keeps_its_exit_code() {
        let faults = Faults::new();
        faults.fail_once_with(DomainError::NotInitialized);
        let err = faults.check().expect_err("fails");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn clones_share_the_arming() {
        let faults = Faults::new();
        let handle = faults.clone();
        handle.fail_once("boom");
        assert!(faults.check().is_err());
    }
}
