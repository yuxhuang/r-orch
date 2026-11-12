//! The real [`Clock`]: wall time, formatted exactly as
//! `new Date().toISOString()`.

use jiff::Timestamp;
use orch_core::clock::{Clock, iso_from_epoch_ms};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        Timestamp::now().as_millisecond()
    }

    /// Formatted through [`iso_from_epoch_ms`] rather than `jiff`'s own
    /// printer: millisecond precision with exactly three fractional digits is
    /// the contract, and that formatter is the one pinned by test vectors.
    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}
