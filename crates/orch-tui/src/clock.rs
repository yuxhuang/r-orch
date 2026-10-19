//! The injected clock (ARCHITECTURE §5.6).
//!
//! Every timer in the TUI — toasts, the soft-delete undo window, the tab flash,
//! message batching, the animation tick — is expressed as a deadline in
//! milliseconds read from this trait, so the tests advance time instead of
//! sleeping.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock {
    /// Wall-clock milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// Production clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Test clock — starts at a fixed instant and only moves when advanced.
#[derive(Debug, Clone)]
pub struct TestClock {
    now: Rc<Cell<i64>>,
}

impl Default for TestClock {
    fn default() -> Self {
        // An arbitrary but stable instant: 2026-10-13T00:00:00.000Z.
        TestClock::new(1_791_849_600_000)
    }
}

impl TestClock {
    pub fn new(start_ms: i64) -> Self {
        TestClock {
            now: Rc::new(Cell::new(start_ms)),
        }
    }

    pub fn advance_ms(&self, ms: i64) {
        self.now.set(self.now.get() + ms);
    }

    pub fn set_ms(&self, ms: i64) {
        self.now.set(ms);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.now.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_only_moves_when_advanced() {
        let clock = TestClock::new(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        assert_eq!(clock.now_ms(), 1_000);
        clock.advance_ms(500);
        assert_eq!(clock.now_ms(), 1_500);
    }

    #[test]
    fn clones_share_the_same_instant() {
        let clock = TestClock::new(0);
        let clone = clock.clone();
        clock.advance_ms(42);
        assert_eq!(clone.now_ms(), 42);
    }
}
