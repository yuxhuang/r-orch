//! Names are data. They are escaped before they reach a report, compared only by
//! their ASCII fold key, and never turned into a path (R103, R104, R117).
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

/// Longest path text a report keeps; the rest of a name is cut and marked.
const MAX_TEXT: usize = 1024;

fn needs_escape(c: char) -> bool {
    c == '\\'
        || c.is_control()
        // Format characters that can reorder or hide text in a terminal or a log.
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// Escapes one name: valid text stays readable, a backslash, a control character and
/// a bidirectional or zero-width format character become `\u{..}`, and every byte that
/// is not UTF-8 becomes `\xNN`. The result has no raw control or newline character.
pub(super) fn escape(name: &OsStr) -> String {
    let mut out = String::new();
    for chunk in name.as_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            if c == '\\' {
                out.push_str("\\\\");
            } else if needs_escape(c) {
                out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
            } else {
                out.push(c);
            }
            if out.len() > MAX_TEXT {
                return cut(out);
            }
        }
        for byte in chunk.invalid() {
            out.push_str(&format!("\\x{byte:02x}"));
        }
        if out.len() > MAX_TEXT {
            return cut(out);
        }
    }
    out
}

fn cut(text: String) -> String {
    cut_at(text, MAX_TEXT)
}

fn cut_at(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('…');
    text
}

/// An identity inside a longer sentence: escaped once and cut to 64 bytes, so that the
/// facts around it (pid, what was found) are never pushed out of a bounded message.
pub(super) fn short(text: &str) -> String {
    cut_at(escape(OsStr::new(text)), 64)
}

/// A path relative to the source `.orchestry/`, one escaped component at a time, joined by `/`.
pub(super) fn relative(parts: &[&OsStr]) -> String {
    let mut text = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            text.push('/');
        }
        text.push_str(&escape(part));
        if text.len() > MAX_TEXT {
            return cut(text);
        }
    }
    text
}

/// R104: an id is the stem of its file and holds only `A-Z a-z 0-9 . _ -`.
pub(super) fn is_id(stem: &[u8]) -> bool {
    !stem.is_empty()
        && stem
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// R117: `A` to `Z` read as `a` to `z`, and nothing else. The key is only ever compared.
pub(super) fn fold(bytes: &[u8]) -> Vec<u8> {
    bytes.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_what_could_forge_or_hide_a_report_line() {
        let name = OsStr::from_bytes(b"a\nb\x1b[0m\\\xff\xfe\xe2\x80\xae.yml");
        assert_eq!(
            escape(name),
            "a\\u{a}b\\u{1b}[0m\\\\\\xff\\xfe\\u{202e}.yml"
        );
        assert!(!escape(name).contains(['\n', '\u{1b}', '\u{202e}']));
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        let name = "é".repeat(2000);
        let text = escape(OsStr::new(&name));
        assert!(text.ends_with('…') && text.len() <= MAX_TEXT + 3);
    }

    #[test]
    fn fold_is_ascii_only() {
        assert_eq!(fold(b"tsk_Case"), fold(b"TSK_cASE"));
        assert_ne!(fold("É".as_bytes()), fold("é".as_bytes()));
    }

    #[test]
    fn the_alphabet_is_exact() {
        // `.` and `..` are inside the alphabet; R104 refuses them for task ids only.
        for good in ["tsk_a-1.b", "..", "A"] {
            assert!(is_id(good.as_bytes()), "{good}");
        }
        for bad in ["", "a b", "a/b", "é", "a\0b", "a:b"] {
            assert!(!is_id(bad.as_bytes()), "{bad:?}");
        }
    }
}
