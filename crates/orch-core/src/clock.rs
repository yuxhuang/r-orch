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

/// `new Date(value).getTime()` for the UTC-designated ISO-8601 forms — the
/// inverse of [`iso_from_epoch_ms`].
///
/// `None` stands for JS's `NaN`, which matters because every `NaN` comparison
/// is false: a stall check against an unparseable `last_event_at` must come out
/// "not stalled", not "stalled forever", and an `Option` forces the caller to
/// decide (PARITY.md §7.3).
///
/// Accepts what `Date.prototype.toISOString` produces and the neighbouring
/// spec forms, all measured against node:
///
/// - `YYYY-MM-DDTHH:MM:SS.sssZ` — the canonical form, the only one ORCH writes
/// - a `±HH:MM` or `±HHMM` offset in place of `Z`
/// - any number of fractional digits (truncated toward zero, so `.426789` is
///   426 ms and `.4` is 400 ms), or none at all
/// - omitted seconds (`T16:37Z`), or a date with no time at all, which is UTC
///   midnight
/// - expanded years (`+002026`), lowercase `t`/`z`, and a space separator,
///   all of which V8 accepts
///
/// Validation matches the measurements exactly, including where JS is
/// inconsistent: the month must be 1-12 and the day 1-31, but the day is
/// **not** checked against the month, so `2026-02-30` parses and rolls forward
/// to March 2 rather than failing. Hour 24 is legal only at exactly
/// `24:00:00.000`.
///
/// **One deliberate deviation.** A time with no zone designator
/// (`2026-10-13T16:37:33`) is *local* time in JS, so its value depends on the
/// machine's timezone. This returns `None` instead: resolving it would need a
/// timezone database in `orch-core`, which deliberately has no date
/// dependency, and nothing ORCH writes can produce the form — reaching it
/// takes a hand-edited `state.json`. Recorded in docs/TRAITS-FROZEN.md.
pub fn epoch_ms_from_iso(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    let mut at = 0usize;

    // Year: `±YYYYYY` or `YYYY`.
    let (year, year_sign) = match bytes.first() {
        Some(b'+') => (digits(bytes, 1, 6)?, 1),
        Some(b'-') => (digits(bytes, 1, 6)?, -1),
        _ => (digits(bytes, 0, 4)?, 1),
    };
    at += if matches!(bytes.first(), Some(b'+' | b'-')) {
        7
    } else {
        4
    };
    let year = year_sign * year;

    let month = separated(bytes, &mut at, b'-', 2)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let day = separated(bytes, &mut at, b'-', 2)?;
    if !(1..=31).contains(&day) {
        return None;
    }

    // A bare date is UTC midnight.
    if at == bytes.len() {
        return days_from_civil(year, month, day).checked_mul(86_400_000);
    }

    // `T`, `t`, or a space.
    match bytes.get(at) {
        Some(b'T' | b't' | b' ') => at += 1,
        _ => return None,
    }

    let hour = digits(bytes, at, 2)?;
    at += 2;
    let minute = separated(bytes, &mut at, b':', 2)?;
    let mut second = 0;
    let mut milli = 0;

    if bytes.get(at) == Some(&b':') {
        second = separated(bytes, &mut at, b':', 2)?;
        if bytes.get(at) == Some(&b'.') {
            at += 1;
            let start = at;
            while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            // A `.` with no digits after it is NaN in JS.
            if at == start {
                return None;
            }
            // Truncate toward zero at millisecond precision, padding short
            // fractions: `.4` is 400 ms, `.426789` is 426 ms.
            let mut fraction = [b'0'; 3];
            for (slot, digit) in fraction.iter_mut().zip(&bytes[start..at]) {
                *slot = *digit;
            }
            milli = (fraction[0] - b'0') as i64 * 100
                + (fraction[1] - b'0') as i64 * 10
                + (fraction[2] - b'0') as i64;
        }
    }

    if !(0..=59).contains(&minute) || !(0..=59).contains(&second) {
        return None;
    }
    // Hour 24 means midnight the next day, and only at exactly 24:00:00.000.
    if hour == 24 {
        if minute != 0 || second != 0 || milli != 0 {
            return None;
        }
    } else if hour > 23 {
        return None;
    }

    let offset_minutes = match bytes.get(at) {
        Some(b'Z' | b'z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            at += 1;
            let offset_hour = digits(bytes, at, 2)?;
            at += 2;
            if bytes.get(at) == Some(&b':') {
                at += 1;
            }
            let offset_minute = digits(bytes, at, 2)?;
            at += 2;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            sign * (offset_hour * 60 + offset_minute)
        }
        // No designator: JS would read this as local time. See the note above.
        _ => return None,
    };

    // Trailing anything — including a newline — is NaN.
    if at != bytes.len() {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let ms = days
        .checked_mul(86_400_000)?
        .checked_add(hour * 3_600_000 + minute * 60_000 + second * 1_000 + milli)?
        .checked_sub(offset_minutes * 60_000)?;
    Some(ms)
}

/// Exactly `count` ASCII digits at `from`, as a number.
fn digits(bytes: &[u8], from: usize, count: usize) -> Option<i64> {
    let slice = bytes.get(from..from + count)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        slice
            .iter()
            .fold(0i64, |acc, b| acc * 10 + (b - b'0') as i64),
    )
}

/// A separator byte followed by exactly `count` digits, advancing `at`.
fn separated(bytes: &[u8], at: &mut usize, separator: u8, count: usize) -> Option<i64> {
    if bytes.get(*at) != Some(&separator) {
        return None;
    }
    let value = digits(bytes, *at + 1, count)?;
    *at += 1 + count;
    Some(value)
}

/// Howard Hinnant's `days_from_civil`: (y, m, d) → days since 1970-01-01.
///
/// The inverse of [`civil_from_days`]. Pure arithmetic, so a day past the end
/// of its month rolls forward — which is exactly what JS does with
/// `2026-02-30`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
    fn matches_javascript_date_get_time() {
        // Every expectation produced by
        // `node -e 'console.log(new Date(s).getTime())'`.
        let cases: &[(&str, Option<i64>)] = &[
            // The canonical form, the only one ORCH writes.
            ("2026-10-13T16:37:33.426Z", Some(1_791_909_453_426)),
            ("1970-01-01T00:00:00.000Z", Some(0)),
            ("1969-12-31T23:59:59.999Z", Some(-1)),
            // Omitted components.
            ("2026-10-13T16:37:33Z", Some(1_791_909_453_000)),
            ("2026-10-13", Some(1_791_849_600_000)),
            // Offsets, with and without the colon.
            ("2026-10-13T16:37:33.426+02:00", Some(1_791_902_253_426)),
            ("2026-10-13T16:37:33.426-05:00", Some(1_791_927_453_426)),
            ("2026-10-13T16:37:33.426+0200", Some(1_791_902_253_426)),
            // Fractional digits are truncated toward zero, and padded.
            ("2026-10-13T16:37:33.4Z", Some(1_791_909_453_400)),
            ("2026-10-13T16:37:33.426789Z", Some(1_791_909_453_426)),
            // V8 extras: lowercase designators and a space separator.
            ("2026-10-13T16:37:33.426z", Some(1_791_909_453_426)),
            ("2026-10-13t16:37:33.426Z", Some(1_791_909_453_426)),
            ("2026-10-13 16:37:33.426Z", Some(1_791_909_453_426)),
            // Expanded years.
            ("+002026-10-13T16:37:33.426Z", Some(1_791_909_453_426)),
            ("-000001-10-13T00:00:00.000Z", Some(-62_174_131_200_000)),
            ("+275760-09-13T00:00:00.000Z", Some(8_640_000_000_000_000)),
            // Hour 24 is midnight the next day, but only on the dot.
            ("2026-10-13T24:00:00.000Z", Some(1_791_936_000_000)),
            ("2026-10-13T24:00:00.001Z", None),
            ("2026-10-13T24:01:00.000Z", None),
            // The day is range-checked but NOT checked against the month, so
            // February 30th parses and rolls forward. The month is checked.
            ("2026-02-30T00:00:00.000Z", Some(1_772_409_600_000)),
            ("2026-10-32T00:00:00.000Z", None),
            ("2026-10-00T00:00:00.000Z", None),
            ("2026-00-10T00:00:00.000Z", None),
            ("2026-13-01T00:00:00.000Z", None),
            // Out-of-range time components.
            ("2026-10-13T16:60:00.000Z", None),
            ("2026-10-13T25:00:00.000Z", None),
            ("2026-10-13T23:59:60.000Z", None),
            // Malformed.
            ("", None),
            ("not a date", None),
            ("2026-1-13T00:00:00.000Z", None),
            ("26-10-13T00:00:00.000Z", None),
            ("2026-10-13T16:37:33.Z", None),
            ("2026-10-13T16:37:33.426+02", None),
            // Surrounding whitespace is not trimmed.
            ("  2026-10-13T16:37:33.426Z  ", None),
            ("2026-10-13T16:37:33.426Z\n", None),
        ];
        for (input, expected) in cases {
            assert_eq!(epoch_ms_from_iso(input), *expected, "input={input:?}");
        }
    }

    #[test]
    fn a_zoneless_time_is_rejected_rather_than_read_as_local() {
        // The one deliberate deviation: node gives these a value that depends
        // on the machine's timezone (`2026-10-13T16:37:33` is 1791923853000 in
        // America/New_York). Resolving that needs a timezone database, which
        // orch-core deliberately does not have, and nothing ORCH writes can
        // produce the form.
        for zoneless in [
            "2026-10-13T16:37:33",
            "2026-10-13T16:37",
            "2026-10-13T16:37:33.426",
        ] {
            assert_eq!(epoch_ms_from_iso(zoneless), None, "{zoneless}");
        }
    }

    #[test]
    fn parsing_round_trips_the_formatter() {
        // The property that actually matters: whatever the clock writes, this
        // reads back unchanged. Those two functions are each other's inverse
        // over every value ORCH can produce.
        for ms in [
            0,
            1,
            -1,
            999,
            86_399_999,
            951_782_400_000,
            1_760_373_453_426,
            1_791_995_878_959,
            4_102_444_800_000,
            -86_400_000,
        ] {
            let iso = iso_from_epoch_ms(ms);
            assert_eq!(epoch_ms_from_iso(&iso), Some(ms), "iso={iso}");
        }
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        // Both directions over a long span, so neither algorithm can drift.
        for days in (-25_000..25_000).step_by(37) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m as i64, d as i64), days, "days={days}");
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
