//! Grapheme-aware text cursor (`src/tui/text-cursor.ts`).
//!
//! ARCHITECTURE §4.8: `unicode-segmentation` replaces `Intl.Segmenter`,
//! `unicode-normalization` replaces `String.normalize('NFC')` and
//! `unicode-width` replaces the hand-written CJK/emoji width table. The TS
//! table only ever answers 1 or 2, and a terminal cell grid cannot render a
//! cluster wider than 2, so [`display_width`] clamps the library answer to
//! `1..=2` — that reproduces every asserted vector while leaving the
//! classification itself to the Unicode tables.
//!
//! Unlike the TS class this is *not* immutable-with-shared-segments: the
//! `_withPos` identity trick exists to avoid re-segmenting in React render, and
//! the "returns the same instance at a boundary" assertions are JS identity
//! details (docs/parity/tui.md §10). Rust mutates in place and no-ops at a
//! boundary.

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Split a string into grapheme clusters.
pub fn grapheme_segments(text: &str) -> Vec<String> {
    text.graphemes(true).map(|g| g.to_string()).collect()
}

/// Count grapheme clusters.
pub fn grapheme_length(text: &str) -> usize {
    text.graphemes(true).count()
}

/// Terminal display width of a single grapheme cluster: 2 for CJK, Hangul,
/// fullwidth forms and emoji, 1 otherwise.
pub fn display_width(grapheme: &str) -> usize {
    if grapheme.is_empty() {
        return 0;
    }
    UnicodeWidthStr::width(grapheme).clamp(1, 2)
}

/// Total display width of a string, summed per cluster.
pub fn text_display_width(text: &str) -> usize {
    text.graphemes(true).map(display_width).sum()
}

fn nfc(text: &str) -> String {
    text.nfc().collect()
}

/// Word boundary backward from a grapheme position: skip spaces, then skip the
/// word. Space-delimited, exactly as the TS helper.
fn word_boundary_back(segs: &[String], pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while i > 0 && segs[i - 1] == " " {
        i -= 1;
    }
    while i > 0 && segs[i - 1] != " " {
        i -= 1;
    }
    i
}

/// Word boundary forward: skip the word, then skip spaces.
fn word_boundary_forward(segs: &[String], pos: usize) -> usize {
    let len = segs.len();
    if pos >= len {
        return len;
    }
    let mut i = pos;
    while i < len && segs[i] != " " {
        i += 1;
    }
    while i < len && segs[i] == " " {
        i += 1;
    }
    i
}

/// A cursor over NFC-normalised text, positioned in grapheme clusters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cursor {
    text: String,
    segs: Vec<String>,
    pos: usize,
}

impl Cursor {
    /// Cursor at the end of `text`.
    pub fn new(text: &str) -> Self {
        let text = nfc(text);
        let segs = grapheme_segments(&text);
        let pos = segs.len();
        Cursor { text, segs, pos }
    }

    /// Cursor at `pos`, clamped into range.
    pub fn at(text: &str, pos: usize) -> Self {
        let mut c = Cursor::new(text);
        c.pos = pos.min(c.segs.len());
        c
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Number of grapheme clusters.
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn before_segs(&self) -> &[String] {
        &self.segs[..self.pos]
    }

    pub fn after_segs(&self) -> &[String] {
        &self.segs[self.pos..]
    }

    pub fn before(&self) -> String {
        self.before_segs().concat()
    }

    pub fn after(&self) -> String {
        self.after_segs().concat()
    }

    // ── Navigation ─────────────────────────────────────────────────────────

    pub fn move_left(&mut self) {
        self.move_left_by(1);
    }

    pub fn move_left_by(&mut self, count: usize) {
        self.pos = self.pos.saturating_sub(count);
    }

    pub fn move_right(&mut self) {
        self.move_right_by(1);
    }

    pub fn move_right_by(&mut self, count: usize) {
        self.pos = (self.pos + count).min(self.segs.len());
    }

    pub fn move_to_start(&mut self) {
        self.pos = 0;
    }

    pub fn move_to_end(&mut self) {
        self.pos = self.segs.len();
    }

    pub fn move_to_word_back(&mut self) {
        self.pos = word_boundary_back(&self.segs, self.pos);
    }

    pub fn move_to_word_forward(&mut self) {
        self.pos = word_boundary_forward(&self.segs, self.pos);
    }

    // ── Editing ────────────────────────────────────────────────────────────

    fn rebuild(&mut self, text: String, pos: usize) {
        let text = nfc(&text);
        self.segs = grapheme_segments(&text);
        self.text = text;
        self.pos = pos.min(self.segs.len());
    }

    /// Insert at the cursor, advancing by the inserted cluster count.
    pub fn insert(&mut self, chars: &str) {
        let normalized = nfc(chars);
        let inserted = grapheme_length(&normalized);
        let text = format!("{}{}{}", self.before(), normalized, self.after());
        let pos = self.pos + inserted;
        self.rebuild(text, pos);
    }

    /// Backspace — delete one cluster before the cursor. No-op at the start.
    pub fn delete_back(&mut self) {
        if self.pos == 0 {
            return;
        }
        let text = format!(
            "{}{}",
            self.segs[..self.pos - 1].concat(),
            self.segs[self.pos..].concat()
        );
        let pos = self.pos - 1;
        self.rebuild(text, pos);
    }

    /// Forward delete — one cluster after the cursor. No-op at the end.
    pub fn delete_forward(&mut self) {
        if self.pos >= self.segs.len() {
            return;
        }
        let text = format!(
            "{}{}",
            self.segs[..self.pos].concat(),
            self.segs[self.pos + 1..].concat()
        );
        let pos = self.pos;
        self.rebuild(text, pos);
    }

    /// Ctrl+K — kill to end of line, returning the killed text.
    pub fn kill_to_end(&mut self) -> String {
        let killed = self.after();
        let text = self.before();
        let pos = self.pos;
        self.rebuild(text, pos);
        killed
    }

    /// Ctrl+U — kill to start of line, returning the killed text.
    pub fn kill_to_start(&mut self) -> String {
        let killed = self.before();
        let text = self.after();
        self.rebuild(text, 0);
        killed
    }

    /// Ctrl+W — kill the word before the cursor, returning the killed text.
    pub fn kill_word_back(&mut self) -> String {
        let new_pos = word_boundary_back(&self.segs, self.pos);
        let killed = self.segs[new_pos..self.pos].concat();
        let text = format!(
            "{}{}",
            self.segs[..new_pos].concat(),
            self.segs[self.pos..].concat()
        );
        self.rebuild(text, new_pos);
        killed
    }

    /// Replace all text, cursor at the end.
    pub fn replace_all(&mut self, text: &str) {
        *self = Cursor::new(text);
    }

    pub fn clear(&mut self) {
        *self = Cursor::new("");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── graphemeSegments / graphemeLength ──────────────────────────────────

    #[test]
    fn segments_ascii_into_single_characters() {
        assert_eq!(grapheme_segments("hello"), ["h", "e", "l", "l", "o"]);
    }

    #[test]
    fn segments_cyrillic() {
        assert_eq!(grapheme_segments("Привет").len(), 6);
    }

    #[test]
    fn keeps_emoji_clusters_whole() {
        assert_eq!(grapheme_segments("👍").len(), 1);
        assert_eq!(grapheme_segments("🇺🇸").len(), 1);
        assert_eq!(grapheme_segments("👋🏽").len(), 1);
        assert_eq!(grapheme_length("👨‍👩‍👧"), 1);
    }

    #[test]
    fn segments_cjk_and_hangul_per_character() {
        assert_eq!(grapheme_segments("你好"), ["你", "好"]);
        assert_eq!(grapheme_segments("안녕"), ["안", "녕"]);
    }

    #[test]
    fn empty_string_has_no_segments() {
        assert!(grapheme_segments("").is_empty());
        assert_eq!(grapheme_length("hi 👋"), 4);
    }

    // ── displayWidth ───────────────────────────────────────────────────────

    #[test]
    fn display_width_is_one_for_narrow_clusters() {
        assert_eq!(display_width("A"), 1);
        assert_eq!(display_width(" "), 1);
        assert_eq!(display_width("Я"), 1);
    }

    #[test]
    fn display_width_is_two_for_wide_clusters() {
        assert_eq!(display_width("你"), 2);
        assert_eq!(display_width("한"), 2);
        assert_eq!(display_width("Ａ"), 2);
        assert_eq!(display_width("😀"), 2);
        assert_eq!(display_width("👋🏽"), 2);
    }

    #[test]
    fn text_display_width_sums_clusters() {
        assert_eq!(text_display_width("hi你!"), 5);
        assert_eq!(text_display_width("hi你"), 4);
        assert_eq!(text_display_width(""), 0);
    }

    // ── construction ───────────────────────────────────────────────────────

    #[test]
    fn construction_defaults_to_the_end() {
        let c = Cursor::new("hello");
        assert_eq!(c.text(), "hello");
        assert_eq!(c.pos(), 5);
        assert_eq!(c.len(), 5);
    }

    #[test]
    fn construction_with_an_explicit_position() {
        let c = Cursor::at("hello", 2);
        assert_eq!(c.pos(), 2);
        assert_eq!(c.before(), "he");
        assert_eq!(c.after(), "llo");
    }

    #[test]
    fn construction_clamps_out_of_range_positions() {
        assert_eq!(Cursor::at("abc", 100).pos(), 3);
        assert_eq!(Cursor::at("abc", 0).pos(), 0);
    }

    #[test]
    fn construction_nfc_normalises() {
        let c = Cursor::new("e\u{0301}");
        assert_eq!(c.text(), "\u{e9}");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn empty_cursor() {
        let c = Cursor::new("");
        assert_eq!(c.text(), "");
        assert_eq!(c.pos(), 0);
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
    }

    // ── navigation ─────────────────────────────────────────────────────────

    #[test]
    fn move_left_and_right_clamp() {
        let mut c = Cursor::at("hello", 3);
        c.move_left();
        assert_eq!(c.pos(), 2);
        let mut c = Cursor::at("hello", 0);
        c.move_left();
        assert_eq!(c.pos(), 0);
        let mut c = Cursor::at("hello", 2);
        c.move_right();
        assert_eq!(c.pos(), 3);
        let mut c = Cursor::new("hello");
        c.move_right();
        assert_eq!(c.pos(), 5);
    }

    #[test]
    fn move_to_start_and_end() {
        let mut c = Cursor::at("hello", 3);
        c.move_to_start();
        assert_eq!(c.pos(), 0);
        c.move_to_end();
        assert_eq!(c.pos(), 5);
    }

    #[test]
    fn word_back_jumps_to_word_start_then_previous_word() {
        let mut c = Cursor::at("hello world", 8);
        c.move_to_word_back();
        assert_eq!(c.pos(), 6);
        let mut c = Cursor::at("hello world", 6);
        c.move_to_word_back();
        assert_eq!(c.pos(), 0);
    }

    #[test]
    fn word_forward_skips_word_then_whitespace() {
        let mut c = Cursor::at("hello world", 0);
        c.move_to_word_forward();
        assert_eq!(c.pos(), 6);
        let mut c = Cursor::at("hello world foo", 2);
        c.move_to_word_forward();
        assert_eq!(c.pos(), 6);
    }

    #[test]
    fn navigation_is_grapheme_aware() {
        let c = Cursor::at("A👍B", 2);
        assert_eq!(c.before(), "A👍");
        assert_eq!(c.after(), "B");
        let mut left = c.clone();
        left.move_left();
        assert_eq!(left.pos(), 1);
        assert_eq!(left.before(), "A");
        assert_eq!(left.after(), "👍B");

        let c = Cursor::at("你好世界", 2);
        assert_eq!(c.before(), "你好");
        assert_eq!(c.after(), "世界");

        let c = Cursor::at("안녕하세요", 3);
        assert_eq!(c.before(), "안녕하");
        assert_eq!(c.after(), "세요");
    }

    // ── editing ────────────────────────────────────────────────────────────

    #[test]
    fn insert_at_cursor_start_and_end() {
        let mut c = Cursor::at("helo", 2);
        c.insert("l");
        assert_eq!(c.text(), "hello");
        assert_eq!(c.pos(), 3);

        let mut c = Cursor::at("world", 0);
        c.insert("hello ");
        assert_eq!(c.text(), "hello world");
        assert_eq!(c.pos(), 6);

        let mut c = Cursor::new("hello");
        c.insert(" world");
        assert_eq!(c.text(), "hello world");
        assert_eq!(c.pos(), 11);
    }

    #[test]
    fn insert_normalises_and_advances_by_graphemes() {
        let mut c = Cursor::new("");
        c.insert("e\u{0301}");
        assert_eq!(c.text(), "\u{e9}");
        assert_eq!(c.len(), 1);

        let mut c = Cursor::at("hi ", 3);
        c.insert("👋");
        assert_eq!(c.text(), "hi 👋");
        assert_eq!(c.pos(), 4);
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn delete_back_removes_one_grapheme_and_no_ops_at_the_start() {
        let mut c = Cursor::at("hello", 3);
        c.delete_back();
        assert_eq!(c.text(), "helo");
        assert_eq!(c.pos(), 2);

        let mut c = Cursor::at("hello", 0);
        c.delete_back();
        assert_eq!(c.text(), "hello");
        assert_eq!(c.pos(), 0);

        let mut c = Cursor::at("A👍B", 2);
        c.delete_back();
        assert_eq!(c.text(), "AB");
        assert_eq!(c.pos(), 1);
    }

    #[test]
    fn delete_forward_removes_one_grapheme_and_no_ops_at_the_end() {
        let mut c = Cursor::at("hello", 2);
        c.delete_forward();
        assert_eq!(c.text(), "helo");
        assert_eq!(c.pos(), 2);

        let mut c = Cursor::new("hello");
        c.delete_forward();
        assert_eq!(c.text(), "hello");
    }

    #[test]
    fn clear_and_replace_all() {
        let mut c = Cursor::new("hello");
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.pos(), 0);

        let mut c = Cursor::at("old", 1);
        c.replace_all("new text");
        assert_eq!(c.text(), "new text");
        assert_eq!(c.pos(), 8);
    }

    // ── kills ──────────────────────────────────────────────────────────────

    #[test]
    fn kill_to_end() {
        let mut c = Cursor::at("hello world", 5);
        assert_eq!(c.kill_to_end(), " world");
        assert_eq!(c.text(), "hello");
        assert_eq!(c.pos(), 5);

        let mut c = Cursor::new("hello");
        assert_eq!(c.kill_to_end(), "");
        assert_eq!(c.text(), "hello");
    }

    #[test]
    fn kill_to_start() {
        let mut c = Cursor::at("hello world", 5);
        assert_eq!(c.kill_to_start(), "hello");
        assert_eq!(c.text(), " world");
        assert_eq!(c.pos(), 0);

        let mut c = Cursor::at("hello", 0);
        assert_eq!(c.kill_to_start(), "");
        assert_eq!(c.text(), "hello");
    }

    #[test]
    fn kill_word_back() {
        let mut c = Cursor::at("hello world", 11);
        assert_eq!(c.kill_word_back(), "world");
        assert_eq!(c.text(), "hello ");

        let mut c = Cursor::at("hello world", 8);
        assert_eq!(c.kill_word_back(), "wo");
        assert_eq!(c.text(), "hello rld");
    }

    // ── segment accessors ──────────────────────────────────────────────────

    #[test]
    fn segment_accessors() {
        assert_eq!(Cursor::at("A👍B", 2).before_segs(), ["A", "👍"]);
        assert_eq!(Cursor::at("A👍B", 1).after_segs(), ["👍", "B"]);
        assert!(Cursor::at("hello", 0).before_segs().is_empty());
        assert!(Cursor::new("hello").after_segs().is_empty());
    }
}
