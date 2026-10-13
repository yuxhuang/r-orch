//! The clock seam — a deliberate addition over the TS original
//! (ARCHITECTURE §5.6).
//!
//! The TS test suite has no clock seam: timestamps come from real `new Date()`,
//! which is a large part of why its timing tests are flaky. Threading a
//! `Clock` through the container lets the differential harness assert exact
//! values and lets retry/backoff and stall-detection tests run instantly.

pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch — `Date.now()`.
    fn now_ms(&self) -> i64;

    /// `new Date().toISOString()`: ISO-8601 UTC, **exactly 3 fractional
    /// digits**, `Z` suffix (PARITY.md §6.3).
    fn now_iso(&self) -> String;
}

/// Format epoch milliseconds the way `Date.prototype.toISOString` does:
/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, always UTC, always exactly three fractional
/// digits (PARITY.md §6.3).
///
/// Implemented here rather than in `orch-infra` so the real clock and the
/// test clock share one tested formatter, and so `orch-core` needs no date
/// dependency. Negative inputs (pre-1970) floor correctly, which matters only
/// because a corrupt file could contain one.
pub fn iso_from_epoch_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem_ms = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second, milli) = (
        rem_ms / 3_600_000,
        (rem_ms / 60_000) % 60,
        (rem_ms / 1_000) % 60,
        rem_ms % 1_000,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (y, m, d).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_javascript_to_iso_string() {
        // Every expectation below was produced by
        // `node -e "console.log(new Date(ms).toISOString())"`.
        let cases: &[(i64, &str)] = &[
            (0, "1970-01-01T00:00:00.000Z"),
            (1, "1970-01-01T00:00:00.001Z"),
            (999, "1970-01-01T00:00:00.999Z"),
            (1_000, "1970-01-01T00:00:01.000Z"),
            (86_399_999, "1970-01-01T23:59:59.999Z"),
            (86_400_000, "1970-01-02T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_760_373_453_426, "2025-10-13T16:37:33.426Z"),
            (1_791_995_878_959, "2026-10-14T16:37:58.959Z"),
            (4_102_444_800_000, "2100-01-01T00:00:00.000Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (-86_400_000, "1969-12-31T00:00:00.000Z"),
        ];
        for (ms, expected) in cases {
            assert_eq!(iso_from_epoch_ms(*ms), *expected, "ms={ms}");
        }
    }

    #[test]
    fn always_has_exactly_three_fractional_digits_and_a_z() {
        for ms in [0, 7, 70, 700, 1_760_373_453_000, 1_760_373_453_007] {
            let iso = iso_from_epoch_ms(ms);
            assert_eq!(iso.len(), 24, "{iso}");
            assert!(iso.ends_with('Z'), "{iso}");
            assert_eq!(&iso[20..23].len(), &3, "{iso}");
        }
    }

    #[test]
    fn leap_years_and_month_boundaries() {
        // 1900 is not a leap year; 2000 is.
        assert_eq!(
            iso_from_epoch_ms(951_868_800_000),
            "2000-03-01T00:00:00.000Z"
        );
        assert_eq!(
            iso_from_epoch_ms(1_583_020_800_000),
            "2020-03-01T00:00:00.000Z"
        );
        assert_eq!(
            iso_from_epoch_ms(1_582_934_400_000),
            "2020-02-29T00:00:00.000Z"
        );
    }
}
