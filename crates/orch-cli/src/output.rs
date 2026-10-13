//! Terminal output primitives — ported from `src/cli/output.ts`.
//!
//! Human output is split: **structural text is L0, decoration is L2**
//! (PARITY.md §5). Every literal word, column header, count line and the
//! *padding* of table cells must match exactly; colours and the ANSI codes
//! around them may differ.
//!
//! The glyph set and its `--ascii` fallback are L0 in both directions,
//! because `--ascii` is a documented flag (§5.2).

use std::io::Write;

/// Status / role glyphs and their `--ascii` fallbacks (PARITY.md §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Running,
    Todo,
    Review,
    Done,
    Failed,
    Retrying,
    Cancelled,
    Idle,
    Error,
    Disabled,
    AgentAction,
    OrchestratorEvent,
    Warning,
}

impl Icon {
    pub fn unicode(self) -> &'static str {
        match self {
            Icon::Running => "●",
            Icon::Todo | Icon::Cancelled | Icon::Idle => "○",
            Icon::Review => "◈",
            Icon::Done => "✓",
            Icon::Failed | Icon::Error => "✕",
            Icon::Retrying => "↻",
            Icon::Disabled => "─",
            Icon::AgentAction => "▸",
            Icon::OrchestratorEvent => "→",
            Icon::Warning => "⚠",
        }
    }

    pub fn ascii(self) -> &'static str {
        match self {
            Icon::Running => "*",
            Icon::Todo | Icon::Cancelled | Icon::Idle => "o",
            Icon::Review => "#",
            Icon::Done => "+",
            Icon::Failed | Icon::Error => "x",
            Icon::Retrying => "~",
            Icon::Disabled => "-",
            Icon::AgentAction => ">",
            Icon::OrchestratorEvent => "->",
            Icon::Warning => "!!",
        }
    }
}

/// ANSI 256 colour codes from the reference `output.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Amber,
    Green,
    Red,
    Blue,
    Yellow,
    Dim,
    Ghost,
    White,
    Purple,
}

impl Color {
    fn code(self) -> u8 {
        match self {
            Color::Amber => 214,
            Color::Green => 72,
            Color::Red => 167,
            Color::Blue => 74,
            Color::Yellow => 178,
            Color::Dim => 240,
            Color::Ghost => 236,
            Color::White => 255,
            Color::Purple => 141,
        }
    }
}

/// Resolved presentation settings for one command invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Styles {
    pub color: bool,
    pub ascii: bool,
}

impl Styles {
    /// Plain text: no colour, Unicode glyphs. What a pipe or the parity
    /// harness sees, because `chalk` disables colour on a non-TTY.
    pub fn plain() -> Self {
        Styles {
            color: false,
            ascii: false,
        }
    }

    pub fn icon(&self, icon: Icon) -> &'static str {
        if self.ascii {
            icon.ascii()
        } else {
            icon.unicode()
        }
    }

    /// Wrap in an ANSI 256 colour, or return the text unchanged when colour is
    /// off. The reset is `\x1b[39m`, matching chalk's foreground-only reset.
    pub fn paint(&self, color: Color, text: &str) -> String {
        if self.color {
            format!("\x1b[38;5;{}m{text}\x1b[39m", color.code())
        } else {
            text.to_owned()
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(Color::Dim, text)
    }

    pub fn agent_name(&self, name: &str) -> String {
        self.paint(Color::Green, name)
    }

    /// The coloured glyph for a task or agent status.
    pub fn status_icon(&self, status: &str) -> String {
        let (icon, color) = match status {
            "running" | "in_progress" => (Icon::Running, Color::Green),
            "todo" => (Icon::Todo, Color::Dim),
            "review" => (Icon::Review, Color::Blue),
            "done" => (Icon::Done, Color::Green),
            "failed" => (Icon::Failed, Color::Red),
            "retrying" => (Icon::Retrying, Color::Yellow),
            "cancelled" => (Icon::Cancelled, Color::Dim),
            "idle" => (Icon::Idle, Color::Dim),
            "error" => (Icon::Error, Color::Red),
            "disabled" => (Icon::Disabled, Color::Ghost),
            // An unknown status renders uncoloured, like the TS `default`.
            _ => return status.to_owned(),
        };
        self.paint(color, self.icon(icon))
    }

    /// `P1` red, `P2` yellow, `P3` plain, `P4` dim, anything else plain.
    pub fn priority_label(&self, priority: i64) -> String {
        match priority {
            1 => self.paint(Color::Red, "P1"),
            2 => self.paint(Color::Yellow, "P2"),
            3 => "P3".to_owned(),
            4 => self.paint(Color::Dim, "P4"),
            other => format!("P{other}"),
        }
    }
}

/// `  ✕ <message>` plus an optional four-space-indented hint, both on
/// **stderr** (PARITY.md §3.1).
pub fn print_error(styles: &Styles, message: &str, hint: Option<&str>) {
    let glyph = styles.paint(Color::Red, styles.icon(Icon::Failed));
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "  {glyph} {message}");
    if let Some(hint) = hint {
        let _ = writeln!(err, "    {hint}");
    }
}

/// `  ✓ <message>` on stdout.
pub fn print_success(styles: &Styles, message: &str) {
    let glyph = styles.paint(Color::Green, styles.icon(Icon::Done));
    println!("  {glyph} {message}");
}

/// `  ⚠ <message>` on stdout.
pub fn print_warning(styles: &Styles, message: &str) {
    let glyph = styles.paint(Color::Yellow, styles.icon(Icon::Warning));
    println!("  {glyph} {message}");
}

/// Column width is `max(header, widest ANSI-stripped cell)`; **every** cell,
/// including the last in a row, is padded to `width + padding`, so rows carry
/// trailing spaces. Do not trim them — they are L0 (PARITY.md §5.1).
///
/// Returns the exact bytes the TS `printTable` writes, including the single
/// trailing newline, so the format can be asserted without capturing stdout.
pub fn render_table(
    styles: &Styles,
    headers: &[&str],
    rows: &[Vec<String>],
    padding: usize,
) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            rows.iter()
                .map(|row| row.get(i).map(|c| display_width(c)).unwrap_or(0))
                .chain(std::iter::once(header.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut lines = Vec::with_capacity(rows.len() + 1);

    let header_line: String = headers
        .iter()
        .enumerate()
        .map(|(i, header)| pad_end(header, widths[i] + padding))
        .collect();
    lines.push(format!("  {}", styles.dim(&header_line)));

    for row in rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let pad = (widths.get(i).copied().unwrap_or(0) + padding)
                    .saturating_sub(display_width(cell));
                format!("{cell}{}", " ".repeat(pad))
            })
            .collect();
        lines.push(format!("  {line}"));
    }

    format!("{}\n", lines.join("\n"))
}

pub fn print_table(styles: &Styles, headers: &[&str], rows: &[Vec<String>], padding: usize) {
    print!("{}", render_table(styles, headers, rows, padding));
}

/// `  ` + key padded to `maxKeyLen + 2` + value, one trailing newline.
pub fn render_key_value(styles: &Styles, pairs: &[(String, String)]) -> String {
    let max_key = pairs
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!("  {}{value}", styles.dim(&pad_end(key, max_key + 2))))
        .collect();
    format!("{}\n", lines.join("\n"))
}

pub fn print_key_value(styles: &Styles, pairs: &[(String, String)]) {
    print!("{}", render_key_value(styles, pairs));
}

/// `String.prototype.padEnd`, which counts UTF-16 code units. For the ASCII
/// headers and labels in this module that is the same as counting characters.
fn pad_end(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.to_owned()
    } else {
        format!("{text}{}", " ".repeat(width - len))
    }
}

/// Cell width as the TS computes it: ANSI stripped, then `.length`.
fn display_width(cell: &str) -> usize {
    strip_ansi(cell).chars().count()
}

/// The TS `stripAnsi` is `/\x1b\[[0-9;]*m/g` — SGR sequences only.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'm') {
                i = j + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `<n>s` under a minute, `m:ss` under an hour, `<h>h<mm>m` beyond.
pub fn format_duration(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds.div_euclid(60);
    let secs = seconds.rem_euclid(60);
    if minutes < 60 {
        return format!("{minutes}:{secs:02}");
    }
    let hours = minutes.div_euclid(60);
    let mins = minutes.rem_euclid(60);
    format!("{hours}h{mins:02}m")
}

/// `formatDurationSince(iso)` = `formatDuration(Date.now() - t)`.
///
/// **Unclamped**, so a future timestamp renders negative — which is how
/// `context list` shows a live TTL as `-60s` (PARITY.md §22.6).
pub fn format_duration_since(timestamp: &str) -> String {
    let now = jiff::Timestamp::now().as_millisecond();
    let then = timestamp
        .parse::<jiff::Timestamp>()
        .map(|ts| ts.as_millisecond())
        .unwrap_or(now);
    format_duration(now - then)
}

/// `1.2k` at or above 1000, the plain integer below.
pub fn format_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{}k", js_to_fixed_1(n as f64 / 1000.0))
    } else {
        n.to_string()
    }
}

/// `Number.prototype.toFixed(1)`.
///
/// **Not** `format!("{:.1}")`: Rust rounds half-to-even and JavaScript rounds
/// half **away from zero**, so an exact tie diverges — `(1250/1000).toFixed(1)`
/// is `"1.3"` in node and `{:.1}` gives `1.2`. Ties are reachable here
/// (`x.25` and `x.75` are exactly representable), and this is L0 human output.
///
/// Decides on the *binary* value rather than the decimal, which is what
/// `toFixed` does: `(1150/1000).toFixed(1)` is `"1.1"`, because 1.15 as a
/// double is just below the tie.
///
/// Positive, finite inputs only — token counts are unsigned.
fn js_to_fixed_1(x: f64) -> String {
    debug_assert!(x.is_finite() && x >= 0.0);
    // 25 decimals is far more than enough to separate "below the tie" from
    // "exactly the tie" for values of this magnitude.
    let expanded = format!("{x:.25}");
    let (integer, decimals) = expanded.split_once('.').expect("precision requested");
    let first = decimals.as_bytes()[0] - b'0';
    let rest = &decimals[1..];

    // Round up on greater-than *or* equal, which is half-away-from-zero.
    let tie = format!("5{}", "0".repeat(rest.len() - 1));
    let round_up = rest >= tie.as_str();

    if !round_up {
        return format!("{integer}.{first}");
    }
    if first < 9 {
        return format!("{integer}.{}", first + 1);
    }
    // Carry into the integer part.
    let integer: u128 = integer.parse().expect("non-negative integer part");
    format!("{}.0", integer + 1)
}

/// `JSON.stringify(value, null, 2)` + newline, on stdout.
pub fn print_json<T: serde::Serialize>(value: &T) -> orch_core::Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|err| orch_core::Error::other(format!("serialize JSON output: {err}")))?;
    println!("{text}");
    Ok(())
}

/// `JSON.stringify(value)` + newline — the compact form four commands use
/// (PARITY.md §4.1).
pub fn print_json_compact<T: serde::Serialize>(value: &T) -> orch_core::Result<()> {
    let text = serde_json::to_string(value)
        .map_err(|err| orch_core::Error::other(format!("serialize JSON output: {err}")))?;
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glyph_set_and_its_ascii_fallback_match_the_contract() {
        // PARITY.md §5.2.
        let table: &[(Icon, &str, &str)] = &[
            (Icon::Running, "●", "*"),
            (Icon::Todo, "○", "o"),
            (Icon::Cancelled, "○", "o"),
            (Icon::Idle, "○", "o"),
            (Icon::Review, "◈", "#"),
            (Icon::Done, "✓", "+"),
            (Icon::Failed, "✕", "x"),
            (Icon::Error, "✕", "x"),
            (Icon::Retrying, "↻", "~"),
            (Icon::Disabled, "─", "-"),
            (Icon::AgentAction, "▸", ">"),
            (Icon::OrchestratorEvent, "→", "->"),
            (Icon::Warning, "⚠", "!!"),
        ];
        for (icon, unicode, ascii) in table {
            assert_eq!(icon.unicode(), *unicode);
            assert_eq!(icon.ascii(), *ascii);
            assert_eq!(
                Styles {
                    color: false,
                    ascii: false
                }
                .icon(*icon),
                *unicode
            );
            assert_eq!(
                Styles {
                    color: false,
                    ascii: true
                }
                .icon(*icon),
                *ascii
            );
        }
    }

    #[test]
    fn plain_styles_emit_no_escape_codes() {
        let styles = Styles::plain();
        assert_eq!(styles.dim("x"), "x");
        assert_eq!(styles.status_icon("todo"), "○");
        assert_eq!(styles.priority_label(1), "P1");
    }

    #[test]
    fn colour_uses_the_reference_ansi_256_codes() {
        let styles = Styles {
            color: true,
            ascii: false,
        };
        assert_eq!(styles.dim("x"), "\x1b[38;5;240mx\x1b[39m");
        assert_eq!(styles.paint(Color::Green, "x"), "\x1b[38;5;72mx\x1b[39m");
        assert_eq!(styles.status_icon("failed"), "\x1b[38;5;167m✕\x1b[39m");
        // P3 is deliberately uncoloured.
        assert_eq!(styles.priority_label(3), "P3");
        assert_eq!(styles.priority_label(9), "P9");
    }

    #[test]
    fn an_unknown_status_renders_as_its_own_text() {
        assert_eq!(Styles::plain().status_icon("weird"), "weird");
    }

    #[test]
    fn strip_ansi_removes_only_sgr_sequences() {
        assert_eq!(strip_ansi("\x1b[38;5;240mdim\x1b[39m"), "dim");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[0m"), "");
        // Not an SGR sequence, so it survives.
        assert_eq!(strip_ansi("\x1b[2Jclear"), "\x1b[2Jclear");
        assert_eq!(strip_ansi("üñî\x1b[39m"), "üñî");
    }

    #[test]
    fn table_cells_are_all_padded_including_the_last() {
        // PARITY.md §5.1: every cell padded to width + padding, so rows carry
        // trailing spaces.
        assert_eq!(
            render_table(
                &Styles::plain(),
                &["STATUS", "PRI"],
                &rows_of(&[&["○ todo", "P3"]]),
                2
            ),
            "  STATUS  PRI  \n  ○ todo  P3   \n"
        );
    }

    #[test]
    fn column_width_is_the_widest_ansi_stripped_cell_or_the_header() {
        assert_eq!(
            render_table(
                &Styles::plain(),
                &["A"],
                &rows_of(&[&["longer-than-header"]]),
                2
            ),
            "  A                   \n  longer-than-header  \n"
        );
        // A coloured cell pads by its visible width.
        let coloured = format!("\x1b[38;5;72m{}\x1b[39m", "ab");
        let rendered = render_table(&Styles::plain(), &["H"], &[vec![coloured]], 2);
        assert_eq!(strip_ansi(&rendered), "  H   \n  ab  \n");
    }

    #[test]
    fn an_empty_table_still_prints_its_header_row() {
        // Column widths come from the headers alone: 1 + 2 and 2 + 2.
        assert_eq!(
            render_table(&Styles::plain(), &["A", "BB"], &[], 2),
            "  A  BB  \n"
        );
    }

    #[test]
    fn key_value_pads_keys_to_the_longest_plus_two() {
        assert_eq!(
            render_key_value(
                &Styles::plain(),
                &[
                    ("ID".to_owned(), "tsk_1".to_owned()),
                    ("TITLE".to_owned(), "t".to_owned())
                ]
            ),
            "  ID     tsk_1\n  TITLE  t\n"
        );
    }

    #[test]
    fn duration_formatting_matches_the_reference() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(999), "0s");
        assert_eq!(format_duration(1_000), "1s");
        assert_eq!(format_duration(59_000), "59s");
        assert_eq!(format_duration(60_000), "1:00");
        assert_eq!(format_duration(61_000), "1:01");
        assert_eq!(format_duration(3_599_000), "59:59");
        assert_eq!(format_duration(3_600_000), "1h00m");
        assert_eq!(format_duration(3_660_000), "1h01m");
        assert_eq!(format_duration(90_000_000), "25h00m");
    }

    /// Every expectation produced by
    /// `node -e "console.log((n/1000).toFixed(1))"`.
    #[test]
    fn token_formatting_matches_the_reference() {
        let cases: &[(u64, &str)] = &[
            (0, "0"),
            (1, "1"),
            (999, "999"),
            (1_000, "1.0k"),
            (1_050, "1.1k"),
            // 1.15 as a double is below the tie, so node rounds *down*.
            (1_150, "1.1k"),
            (1_234, "1.2k"),
            // 1.25 is exactly representable, so node rounds half away from
            // zero — where Rust's `{:.1}` would round half to even and give
            // "1.2k".
            (1_250, "1.3k"),
            (1_350, "1.4k"),
            (2_250, "2.3k"),
            (1_000_000, "1000.0k"),
            (1_234_567, "1234.6k"),
        ];
        for (n, expected) in cases {
            assert_eq!(format_tokens(*n), *expected, "n={n}");
        }
    }

    #[test]
    fn to_fixed_1_carries_into_the_integer_part() {
        assert_eq!(js_to_fixed_1(0.96), "1.0");
        assert_eq!(js_to_fixed_1(9.99), "10.0");
        assert_eq!(js_to_fixed_1(0.0), "0.0");
    }

    fn rows_of(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|c| (*c).to_owned()).collect())
            .collect()
    }
}
