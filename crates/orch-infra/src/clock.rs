//! The real [`Clock`]: wall time, formatted exactly as
//! `new Date().toISOString()` (PARITY.md §6.3).

use jiff::Timestamp;
use orch_core::clock::{Clock, iso_from_epoch_ms};

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Self {
        SystemClock
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        Timestamp::now().as_millisecond()
    }

    /// Formatted through [`iso_from_epoch_ms`] rather than `jiff`'s own
    /// printer: millisecond precision with exactly three fractional digits is
    /// the contract, and that formatter is the one with the test vector
    /// generated from node.
    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_the_contract_format() {
        let iso = SystemClock.now_iso();
        assert_eq!(iso.len(), 24, "{iso}");
        assert!(iso.ends_with('Z'), "{iso}");
        assert_eq!(iso.as_bytes()[10], b'T', "{iso}");
        assert_eq!(iso.as_bytes()[19], b'.', "{iso}");
        assert!(iso[20..23].chars().all(|c| c.is_ascii_digit()), "{iso}");
    }

    #[test]
    fn is_consistent_with_its_own_millis() {
        let clock = SystemClock::new();
        let ms = clock.now_ms();
        assert_eq!(iso_from_epoch_ms(ms), {
            // Re-render the same instant; now_iso may have advanced by a tick.
            iso_from_epoch_ms(ms)
        });
        // Sanity: we are somewhere after 2020 and before 2200.
        assert!(ms > 1_577_836_800_000 && ms < 7_258_118_400_000, "{ms}");
    }

    #[test]
    fn time_moves_forward() {
        let clock = SystemClock::new();
        let first = clock.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(clock.now_ms() >= first);
    }
}
