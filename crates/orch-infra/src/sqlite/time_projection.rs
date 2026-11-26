//! The one rule that turns a stored time string into the native millisecond
//! column of the SQLite entity formats (issue 335).

/// UTC milliseconds of `0000-01-01T00:00:00.000Z`, the lower end of the range.
pub(super) const MIN_MILLIS: i64 = -62_167_219_200_000;
/// UTC milliseconds of `9999-12-30T22:00:00.999Z`, the upper end of the range.
pub(super) const MAX_MILLIS: i64 = 253_402_207_200_999;

/// The millisecond projection of `text`: `Some` only when it is an RFC 3339
/// section 5.6 `date-time` naming an instant that is a whole number of
/// milliseconds inside the range, and `None` (NULL) otherwise. A fraction is
/// never rounded and a leap second names no instant of this timeline.
///
/// The syntax is checked here, byte by byte, because the parser of a library
/// accepts a wider ISO 8601 grammar and would make the stored format depend on
/// its version. Only the calendar and the arithmetic are left to `jiff`
/// (library-first, ARCHITECTURE 4.8), and only after the shape is known.
pub(super) fn rfc3339_millis(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    let mut rest = &b[19..];
    let mut milli = 0;
    if rest.first() == Some(&b'.') {
        let len = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
        let fraction = &rest[1..=len];
        // `1*DIGIT`; whole milliseconds, so only the first three may be set.
        if fraction.is_empty() || fraction[3.min(len)..].iter().any(|c| *c != b'0') {
            return None;
        }
        let mut padded = [b'0'; 3];
        let head = &fraction[..3.min(len)];
        padded[..head.len()].copy_from_slice(head);
        milli = digits(&padded)?;
        rest = &rest[1 + len..];
    }
    let offset = match rest {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let hours = digits(&[*h1, *h2])?;
            let minutes = digits(&[*m1, *m2])?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'-' { -seconds } else { seconds }
        }
        _ => return None,
    };
    // The calendar refuses a day the month lacks and a second of 60: a leap
    // second names no instant of the millisecond timeline.
    let civil = jiff::civil::DateTime::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
        i8::try_from(hour).ok()?,
        i8::try_from(minute).ok()?,
        i8::try_from(second).ok()?,
        milli * 1_000_000,
    )
    .ok()?;
    let instant = jiff::tz::Offset::from_seconds(offset)
        .ok()?
        .to_timestamp(civil)
        .ok()?;
    let millis = instant.as_millisecond();
    (MIN_MILLIS..=MAX_MILLIS)
        .contains(&millis)
        .then_some(millis)
}

/// The value of exactly the ASCII digits of `bytes`.
fn digits(bytes: &[u8]) -> Option<i32> {
    bytes.iter().try_fold(0i32, |value, c| {
        c.is_ascii_digit().then(|| value * 10 + i32::from(c - b'0'))
    })
}

/// Every spelling both families are tested against, on write and on read:
/// the text and its projection (`None` is NULL, the original text kept).
#[cfg(test)]
pub(super) const SPELLINGS: &[(&str, Option<i64>)] = &[
    // The RFC 3339 section 5.6 date-time, whole milliseconds, in range.
    ("2026-11-20T10:00:00Z", Some(1_795_168_800_000)),
    ("2026-11-20T10:00:00.000Z", Some(1_795_168_800_000)),
    ("2026-11-20T10:00:00.5Z", Some(1_795_168_800_500)),
    ("2026-11-20T10:00:00.50Z", Some(1_795_168_800_500)),
    ("2026-11-20T10:00:00.123Z", Some(1_795_168_800_123)),
    ("2026-11-20T10:00:00.500000000Z", Some(1_795_168_800_500)),
    // 1*DIGIT has no upper bound: a tail of zeros past nanoseconds is whole.
    (
        "2026-11-20T10:00:00.5000000000000Z",
        Some(1_795_168_800_500),
    ),
    (
        "2026-11-20T10:00:00.0000000000000Z",
        Some(1_795_168_800_000),
    ),
    // The RFC's own note: the T and the Z may be lower case.
    ("2026-11-20t10:00:00z", Some(1_795_168_800_000)),
    ("2026-11-20T10:00:00z", Some(1_795_168_800_000)),
    ("2026-11-20t10:00:00Z", Some(1_795_168_800_000)),
    // Numeric offsets, with a colon, two digits each.
    ("2026-11-20T10:00:00+00:00", Some(1_795_168_800_000)),
    ("2026-11-20T10:00:00-00:00", Some(1_795_168_800_000)),
    ("2026-11-20T10:00:00+02:00", Some(1_795_161_600_000)),
    ("2026-11-20T10:00:00-05:30", Some(1_795_188_600_000)),
    ("2026-11-20T10:00:00+23:59", Some(1_795_082_460_000)),
    ("2026-11-20T10:00:00-23:59", Some(1_795_255_140_000)),
    ("2026-11-20T12:00:00.250+02:00", Some(1_795_168_800_250)),
    // Calendar: leap days and the epoch neighbours.
    ("2024-02-29T00:00:00Z", Some(1_709_164_800_000)),
    ("2000-02-29T12:34:56.789Z", Some(951_827_696_789)),
    ("1970-01-01T00:00:00Z", Some(0)),
    ("1969-12-31T23:59:59.999Z", Some(-1)),
    // The endpoints of the range, reached directly and through an offset.
    ("0000-01-01T00:00:00.000Z", Some(MIN_MILLIS)),
    ("0000-01-01T00:00:00.001Z", Some(MIN_MILLIS + 1)),
    ("0000-01-01T01:00:00.000+01:00", Some(MIN_MILLIS)),
    ("9999-12-30T22:00:00.999Z", Some(MAX_MILLIS)),
    ("9999-12-31T00:00:00.999+02:00", Some(MAX_MILLIS)),
    ("9999-12-30T21:00:00.999-01:00", Some(MAX_MILLIS)),
    // Out of range by one millisecond, or by an offset.
    ("9999-12-30T22:00:01Z", None),
    ("9999-12-30T22:00:01.000Z", None),
    ("9999-12-31T23:59:59Z", None),
    ("0000-01-01T00:00:00.000+01:00", None),
    ("0000-01-01T00:00:00+00:01", None),
    // A fraction finer than a millisecond is never rounded.
    ("2026-11-20T10:00:00.0005Z", None),
    ("2026-11-20T10:00:00.0000000001Z", None),
    ("2026-11-20T10:00:00.1234Z", None),
    ("2026-11-20T10:00:00.5000000000001Z", None),
    // Wider than RFC 3339: the jiff probe spellings of issue 335.
    ("2026-11-20T10:00Z", None),
    ("2026-11-20T10Z", None),
    ("2026-11-20 10:00:00Z", None),
    ("20261120T100000Z", None),
    ("2026-11-20T10:00:00+0200", None),
    ("2026-11-20T10:00:00+02", None),
    ("2026-11-20T10:00:00,5Z", None),
    ("2026-11-20T10:00:00Z[UTC]", None),
    ("2026-11-20T10:00:00+02:00[Europe/Paris]", None),
    ("+002026-11-20T10:00:00Z", None),
    ("-002026-11-20T10:00:00Z", None),
    ("12026-11-20T10:00:00Z", None),
    ("2026-11-20T10:00:00", None),
    ("2026-11-20T10:00:00.Z", None),
    ("2026-11-20T10:00:00+2:00", None),
    ("2026-11-20T10:00:00+02:0", None),
    // A separator that is not the grammar's own.
    ("2026/11/20T10:00:00Z", None),
    ("2026-11/20T10:00:00Z", None),
    ("2026/11-20T10:00:00Z", None),
    ("2026-11-20T10-00:00Z", None),
    ("2026-11-20T10:00-00Z", None),
    ("2026-11-20T10:00:00.5+02-00", None),
    // A leap second names no instant of the millisecond timeline.
    ("2026-11-20T23:59:60Z", None),
    ("2016-12-31T23:59:60Z", None),
    // Not a date or a time of day.
    ("2026-13-20T10:00:00Z", None),
    ("2026-00-20T10:00:00Z", None),
    ("2026-11-00T10:00:00Z", None),
    ("2026-11-31T10:00:00Z", None),
    ("2026-02-29T00:00:00Z", None),
    ("1900-02-29T00:00:00Z", None),
    ("2026-11-20T24:00:00Z", None),
    ("2026-11-20T10:60:00Z", None),
    ("2026-11-20T10:00:00+24:00", None),
    ("2026-11-20T10:00:00+02:60", None),
    ("2026-11-20T10:00:00-24:00", None),
    // Not text of the grammar at all.
    ("", None),
    ("opaque", None),
    (" 2026-11-20T10:00:00Z", None),
    ("2026-11-20T10:00:00Z ", None),
    ("2026-11-20T10:00:00Z\n", None),
    ("2026-11-20T10:00:00ZZ", None),
    ("2026-11-20T10:00:00 Z", None),
    ("2026-11-20T10:00:00\u{2212}02:00", None),
    ("2026-11-20T10:00:00\u{0}Z", None),
    ("\u{ff12}\u{ff10}26-11-20T10:00:00Z", None),
    ("2026-11-20T10:00:00\u{ff10}Z", None),
    ("2026-11-20T10:00:00.\u{ff15}Z", None),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_projects_or_stays_null_exactly_as_the_table_says() {
        let wrong: Vec<_> = SPELLINGS
            .iter()
            .filter(|(text, want)| rfc3339_millis(text) != *want)
            .map(|(text, want)| format!("{text:?}: want {want:?}, got {:?}", rfc3339_millis(text)))
            .collect();
        assert!(
            wrong.is_empty(),
            "{} wrong:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

    #[test]
    fn the_table_has_no_duplicate_spelling() {
        let mut seen = std::collections::BTreeSet::new();
        for (text, _) in SPELLINGS {
            assert!(seen.insert(*text), "duplicate {text:?}");
        }
    }
}
