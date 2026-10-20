//! Truncation and duration formatters.
//!
//! Line caps count **characters** (every call site is plain text), so these count `char`s, not bytes and not graphemes —
//! matching the arithmetic that the layout widths are derived from.

/// Max chars for goal descriptions / progress reports rendered in panels.
pub const MAX_PANEL_TEXT: usize = 10_000;

/// `capLine(s, n) = s.length > n ? s.slice(0, n - 1) + '…' : s`.
pub fn cap_line(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        if max_len == 0 {
            return String::new();
        }
        let mut out: String = s.chars().take(max_len - 1).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// [`cap_line`] measured in **display columns** rather than `char`s.
///
/// For text whose width is not its length: agent names may be CJK or emoji,
/// where a `char` cap of 32 is 64 columns and overflows its budget. The rest
/// of the bar keeps counting `char`s;
/// this is used where a name is involved.
pub fn cap_display(s: &str, max_cols: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;

    if crate::text::text_display_width(s) <= max_cols {
        return s.to_owned();
    }
    if max_cols == 0 {
        return String::new();
    }
    // One column is reserved for the ellipsis, and a wide grapheme that would
    // straddle the cap is dropped rather than half-drawn. Measured per
    // grapheme, like the check above: per `char`, `❤` + VS16 counts one
    // column but draws two.
    let mut out = String::new();
    let mut used = 0;
    for grapheme in s.graphemes(true) {
        let width = crate::text::display_width(grapheme);
        if used + width > max_cols - 1 {
            break;
        }
        used += width;
        out.push_str(grapheme);
    }
    out.push('…');
    out
}

/// Appends `"\n…[truncated]"` past the cap.
pub fn cap_text(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}\n…[truncated]")
    } else {
        s.to_string()
    }
}

/// `{s}s`, `{m}:{ss}`, `{h}h{mm}m`.
pub fn format_duration(ms: i64) -> String {
    let seconds = ms.div_euclid(1000).max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let secs = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}:{secs:02}");
    }
    let hours = minutes / 60;
    let mins = minutes % 60;
    format!("{hours}h{mins:02}m")
}

/// `1.2k` past a thousand.
pub fn format_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `now` / `{n}s` / `{n}m` / `{n}h`.
pub fn relative_time(ts_ms: i64, now_ms: i64) -> String {
    let diff = (now_ms - ts_ms).max(0);
    if diff < 3_000 {
        "now".to_string()
    } else if diff < 60_000 {
        format!("{}s", diff / 1000)
    } else if diff < 3_600_000 {
        format!("{}m", diff / 60_000)
    } else {
        format!("{}h", diff / 3_600_000)
    }
}

/// The Started column: [`relative_time`] under a day, then whole days
/// (`3d`), so an old start never reads as `120h`.
pub fn format_started(ts_ms: i64, now_ms: i64) -> String {
    let diff = now_ms.saturating_sub(ts_ms).max(0);
    if diff < 86_400_000 {
        relative_time(ts_ms, now_ms)
    } else {
        format!("{}d", diff / 86_400_000)
    }
}

/// Goal-row age column: `{n}d`, or `<1d` under a day.
pub fn format_age_days(created_ms: i64, now_ms: i64) -> String {
    let days = (now_ms - created_ms).max(0) / 86_400_000;
    if days > 0 {
        format!("{days}d")
    } else {
        "<1d".to_string()
    }
}

/// Parse an RFC 3339 / ISO-8601 timestamp to epoch milliseconds.
///
/// Hand-rolled rather than pulled from `jiff`: the TUI only ever consumes
/// timestamps this crate also writes (`YYYY-MM-DDTHH:MM:SS.mmmZ`)
/// and only needs a millisecond delta, so a date-library
/// dependency here would buy nothing. Unparseable input yields `None`, which
/// every call site renders as `—`.
pub fn parse_timestamp_ms(ts: &str) -> Option<i64> {
    let bytes = ts.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> { ts.get(range)?.parse::<i64>().ok() };
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    let hour = num(11..13)?;
    let minute = num(14..16)?;
    let second = num(17..19)?;
    let millis = if bytes.len() >= 23 && bytes[19] == b'.' {
        num(20..23)?
    } else {
        0
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil epoch — Howard Hinnant's `days_from_civil`.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) + millis)
}

/// How far past the reference clock an observation may lie and still count:
/// modest clock skew between machines, not an absurd date.
pub const OBSERVATION_SKEW_MS: i64 = 86_400_000;

/// [`parse_timestamp_ms`] behind a strict gate, for timestamps the Agents
/// table treats as evidence (UPDATED and the Overview's `live` row).
///
/// Accepts exactly the form ORCH writes — `YYYY-MM-DDTHH:MM:SS`, an optional
/// `.` and 1–9 fractional digits, then `Z` (`iso_from_epoch_ms` writes
/// `.mmmZ`; `jiff::Timestamp`'s `Display` writes other fraction lengths) — for
/// a real calendar date and time of day, between the epoch and `now_ms` plus
/// [`OBSERVATION_SKEW_MS`]. Anything else is `None`, never clamped: no
/// offsets, no trailing bytes, no Feb 30, no year 9999. The shared parser is
/// left as permissive as its other callers expect.
pub fn parse_observation_ms(ts: &str, now_ms: i64) -> Option<i64> {
    let b = ts.as_bytes();
    if !(20..=30).contains(&b.len()) || b[b.len() - 1] != b'Z' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    let shape = digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    if !shape {
        return None;
    }
    // Sub-millisecond digits are dropped; `.5` is 500 ms.
    let fraction_ms = match &b[19..b.len() - 1] {
        [] => 0,
        [b'.', fraction @ ..]
            if (1..=9).contains(&fraction.len()) && fraction.iter().all(u8::is_ascii_digit) =>
        {
            fraction
                .iter()
                .chain(std::iter::repeat(&b'0'))
                .take(3)
                .fold(0, |ms, d| ms * 10 + i64::from(d - b'0'))
        }
        _ => return None,
    };
    let field = |range: std::ops::Range<usize>| {
        b[range]
            .iter()
            .fold(0_i64, |n, d| n * 10 + i64::from(d - b'0'))
    };
    let (year, month, day) = (field(0..4), field(5..7), field(8..10));
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=month_days).contains(&day)
        || field(11..13) > 23
        || field(14..16) > 59
        || field(17..19) > 59
    {
        return None;
    }
    // Whole seconds from the shared parser (the slice is checked ASCII).
    let at = parse_timestamp_ms(&ts[..19])? + fraction_ms;
    (0..=now_ms.saturating_add(OBSERVATION_SKEW_MS))
        .contains(&at)
        .then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_line_truncates_with_an_ellipsis() {
        assert_eq!(cap_line("hello", 10), "hello");
        assert_eq!(cap_line("hello", 5), "hello");
        assert_eq!(cap_line("hello", 4), "hel…");
        assert_eq!(cap_line("hello", 1), "…");
        // 60 chars exactly is not truncated; 61 becomes 59 + '…'.
        let sixty = "x".repeat(60);
        assert_eq!(cap_line(&sixty, 60), sixty);
        let sixty_one = "x".repeat(61);
        let capped = cap_line(&sixty_one, 60);
        assert_eq!(capped.chars().count(), 60);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn cap_text_appends_a_truncation_notice() {
        let long = "a".repeat(MAX_PANEL_TEXT + 1);
        let out = cap_text(&long, MAX_PANEL_TEXT);
        assert!(out.ends_with("\n…[truncated]"));
        assert_eq!(cap_text("short", MAX_PANEL_TEXT), "short");
    }

    #[test]
    fn duration_formats_match_the_ts_formatter() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59_999), "59s");
        assert_eq!(format_duration(60_000), "1:00");
        assert_eq!(format_duration(61_000), "1:01");
        assert_eq!(format_duration(3_599_000), "59:59");
        assert_eq!(format_duration(3_600_000), "1h00m");
        assert_eq!(format_duration(3_900_000), "1h05m");
    }

    #[test]
    fn token_counts_switch_to_k_at_a_thousand() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(12_345), "12.3k");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(1_000, 1_000), "now");
        assert_eq!(relative_time(0, 2_999), "now");
        assert_eq!(relative_time(0, 3_000), "3s");
        assert_eq!(relative_time(0, 59_999), "59s");
        assert_eq!(relative_time(0, 60_000), "1m");
        assert_eq!(relative_time(0, 3_599_999), "59m");
        assert_eq!(relative_time(0, 3_600_000), "1h");
    }

    #[test]
    fn age_days_renders_sub_day_as_lt_1d() {
        assert_eq!(format_age_days(0, 86_399_000), "<1d");
        assert_eq!(format_age_days(0, 86_400_000), "1d");
        assert_eq!(format_age_days(0, 3 * 86_400_000), "3d");
    }

    #[test]
    fn cap_display_counts_columns_and_never_splits_a_wide_character() {
        assert_eq!(cap_display("hello", 10), "hello");
        assert_eq!(cap_display("hello", 5), "hello");
        assert_eq!(cap_display("hello", 4), "hel…");
        assert_eq!(cap_display("", 0), "");
        assert_eq!(cap_display("hello", 0), "");
        // Each of these is two columns, so a cap of 5 fits two of them plus
        // the ellipsis — and never half of one.
        assert_eq!(cap_display("名前です", 5), "名前…");
        assert_eq!(cap_display("名前です", 8), "名前です");
        // Seven columns fit three wide characters plus the ellipsis exactly.
        assert_eq!(cap_display("名前です", 7), "名前で…");
        for cap in 0..10 {
            assert!(
                crate::text::text_display_width(&cap_display("🚀🚀🚀", cap)) <= cap,
                "cap {cap}"
            );
        }
        // An emoji presentation sequence is two columns as a whole, so it
        // is kept or dropped whole.
        assert_eq!(
            cap_display("\u{2764}\u{FE0F}heart", 5),
            "\u{2764}\u{FE0F}he…"
        );
        assert_eq!(cap_display("\u{2764}\u{FE0F}heart", 2), "…");
    }

    #[test]
    fn timestamps_parse_to_epoch_millis() {
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_timestamp_ms("2026-10-13T16:37:33.426Z"),
            Some(1_791_909_453_426)
        );
        assert_eq!(
            parse_timestamp_ms("2026-10-13T16:37:33Z"),
            Some(1_791_909_453_000)
        );
        assert_eq!(parse_timestamp_ms("nope"), None);
        assert_eq!(parse_timestamp_ms(""), None);
    }

    // ── parse_observation_ms (adjudication v3, Ruling 1) ──

    /// 2026-10-19T00:00:00Z, the reference clock for these cases.
    const NOW: i64 = 1_792_368_000_000;

    #[test]
    fn observation_rejects_text_that_is_not_a_timestamp() {
        assert_eq!(parse_observation_ms("not-a-date", NOW), None);
        assert_eq!(parse_observation_ms("", NOW), None);
        assert_eq!(parse_observation_ms("😀😀😀😀😀😀😀😀😀😀", NOW), None);
    }

    #[test]
    fn observation_rejects_year_9999() {
        assert_eq!(parse_observation_ms("9999-12-31T23:59:59.999Z", NOW), None);
    }

    #[test]
    fn observation_rejects_a_time_before_the_epoch() {
        assert_eq!(parse_observation_ms("1969-12-31T23:59:59Z", NOW), None);
    }

    #[test]
    fn observation_rejects_an_impossible_date_and_time() {
        assert_eq!(parse_observation_ms("2026-02-31T99:99:99Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-02-31T00:00:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-19T24:00:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-19T00:60:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-19T00:00:60Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-13-01T00:00:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-00-01T00:00:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-04-31T00:00:00Z", NOW), None);
    }

    #[test]
    fn observation_rejects_wrong_separators_and_trailing_bytes() {
        assert_eq!(
            parse_observation_ms("2026-10-19T00x00x00garbage", NOW),
            None
        );
        assert_eq!(parse_observation_ms("2026-10-19 00:00:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-19T00:00:00", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-19T00:00:00.Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-19T00:00:00Zx", NOW), None);
        assert_eq!(parse_observation_ms("+026-10-19T00:00:00Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-+1-19T00:00:00Z", NOW), None);
    }

    #[test]
    fn observation_rejects_a_long_suffix() {
        let long = format!("2026-10-19T00:00:00Z{}", "x".repeat(2000));
        assert_eq!(parse_observation_ms(&long, NOW), None);
        let long_fraction = format!("2026-10-19T00:00:00.{}Z", "1".repeat(2000));
        assert_eq!(parse_observation_ms(&long_fraction, NOW), None);
    }

    #[test]
    fn observation_accepts_the_epoch() {
        assert_eq!(parse_observation_ms("1970-01-01T00:00:00Z", NOW), Some(0));
        assert_eq!(
            parse_observation_ms("1970-01-01T00:00:00.000Z", NOW),
            Some(0)
        );
    }

    #[test]
    fn observation_accepts_what_orch_writes() {
        assert_eq!(
            parse_observation_ms("2026-10-19T00:00:00.000Z", NOW),
            Some(NOW)
        );
        assert_eq!(
            parse_observation_ms("2026-10-18T23:59:59.250Z", NOW),
            Some(NOW - 750)
        );
    }

    #[test]
    fn observation_accepts_an_hour_ahead_but_not_25_hours() {
        let hour = 3_600_000;
        assert_eq!(
            parse_observation_ms("2026-10-19T01:00:00Z", NOW),
            Some(NOW + hour)
        );
        // The tolerance's edge is inclusive.
        assert_eq!(
            parse_observation_ms("2026-10-20T00:00:00Z", NOW),
            Some(NOW + 24 * hour)
        );
        assert_eq!(parse_observation_ms("2026-10-20T00:00:00.001Z", NOW), None);
        assert_eq!(parse_observation_ms("2026-10-20T01:00:00Z", NOW), None);
    }

    #[test]
    fn observation_knows_leap_years() {
        assert_eq!(
            parse_observation_ms("2024-02-29T00:00:00Z", NOW),
            parse_timestamp_ms("2024-02-29T00:00:00Z")
        );
        assert!(parse_observation_ms("2024-02-29T00:00:00Z", NOW).is_some());
        assert_eq!(parse_observation_ms("2023-02-29T00:00:00Z", NOW), None);
        assert!(parse_observation_ms("2000-02-29T00:00:00Z", NOW).is_some());
        assert_eq!(parse_observation_ms("2100-02-29T00:00:00Z", i64::MAX), None);
    }

    #[test]
    fn observation_accepts_one_to_nine_fractional_digits_only() {
        let base = parse_timestamp_ms("2026-10-18T12:00:00Z").unwrap();
        assert_eq!(
            parse_observation_ms("2026-10-18T12:00:00.5Z", NOW),
            Some(base + 500)
        );
        assert_eq!(
            parse_observation_ms("2026-10-18T12:00:00.12Z", NOW),
            Some(base + 120)
        );
        assert_eq!(
            parse_observation_ms("2026-10-18T12:00:00.123456789Z", NOW),
            Some(base + 123)
        );
        assert_eq!(
            parse_observation_ms("2026-10-18T12:00:00.1234567890Z", NOW),
            None
        );
    }

    #[test]
    fn observation_rejects_an_offset_form() {
        // No producer in `crates/` writes these fields with an offset.
        assert_eq!(parse_observation_ms("2026-10-18T12:00:00+00:00", NOW), None);
        assert_eq!(
            parse_observation_ms("2026-10-18T12:00:00.000+00:00", NOW),
            None
        );
    }
}
