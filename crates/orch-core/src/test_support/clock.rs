//! A clock tests can drive, so retry/backoff and stall-detection assertions
//! need no sleeping (ARCHITECTURE §5.6).

use std::sync::atomic::{AtomicI64, Ordering};

use crate::clock::{Clock, iso_from_epoch_ms};

/// `2026-10-13T16:37:33.426Z` — the instant the PARITY.md reference capture
/// was taken, so fixtures read naturally.
pub const FIXED_NOW_MS: i64 = 1_760_373_453_426;

/// [`FIXED_NOW_MS`] rendered.
pub const FIXED_NOW_ISO: &str = "2025-10-13T16:37:33.426Z";

/// A clock that only moves when a test moves it.
#[derive(Debug)]
pub struct FixedClock {
    now_ms: AtomicI64,
}

impl FixedClock {
    /// Starts at [`FIXED_NOW_MS`].
    pub fn new() -> Self {
        FixedClock::at(FIXED_NOW_MS)
    }

    pub fn at(now_ms: i64) -> Self {
        FixedClock {
            now_ms: AtomicI64::new(now_ms),
        }
    }

    /// Move the clock forward (or backward, with a negative delta) and return
    /// the new instant.
    pub fn advance_ms(&self, delta_ms: i64) -> i64 {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst) + delta_ms
    }

    pub fn set_ms(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        FixedClock::new()
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }

    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_the_fixed_instant() {
        let clock = FixedClock::new();
        assert_eq!(clock.now_ms(), FIXED_NOW_MS);
        assert_eq!(clock.now_iso(), FIXED_NOW_ISO);
    }

    #[test]
    fn does_not_move_on_its_own() {
        let clock = FixedClock::new();
        let first = clock.now_iso();
        for _ in 0..5 {
            assert_eq!(clock.now_iso(), first);
        }
    }

    #[test]
    fn advances_and_keeps_the_iso_rendering_in_step() {
        let clock = FixedClock::at(0);
        assert_eq!(clock.now_iso(), "1970-01-01T00:00:00.000Z");
        assert_eq!(clock.advance_ms(1_500), 1_500);
        assert_eq!(clock.now_iso(), "1970-01-01T00:00:01.500Z");
        clock.advance_ms(-500);
        assert_eq!(clock.now_iso(), "1970-01-01T00:00:01.000Z");
    }

    #[test]
    fn can_be_set_outright() {
        let clock = FixedClock::new();
        clock.set_ms(86_400_000);
        assert_eq!(clock.now_iso(), "1970-01-02T00:00:00.000Z");
    }

    #[test]
    fn is_usable_behind_a_shared_reference() {
        let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(FixedClock::new());
        assert_eq!(clock.now_ms(), FIXED_NOW_MS);
    }
}
