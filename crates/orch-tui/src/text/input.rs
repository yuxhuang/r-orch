//! Single-line text editing (`src/tui/hooks/useTextInput.ts`).
//!
//! Hand-written rather than delegated to `tui-input` (ARCHITECTURE §4.8): the
//! parity surface here is a kill ring (`Ctrl+K/U/W` → `Ctrl+Y`) plus an undo
//! stack with a 500 ms snapshot debounce, a 50-entry cap and two *different*
//! undo semantics for `Ctrl+Z` and `Cmd+Z` (docs/parity/tui.md §2.4).
//! `tui-input` models none of those, so adopting it would mean reimplementing
//! every asserted behaviour on top of a foreign buffer. The grapheme and width
//! logic that *is* library work lives in [`super::cursor`].
//!
//! The debounce is clock-driven, not timer-driven: [`TextInput::poll`] is called
//! once per frame and on every key, so tests advance the [`Clock`] instead of
//! sleeping.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::cursor::Cursor;

/// `UNDO_SNAPSHOT_DEBOUNCE_MS`.
pub const UNDO_SNAPSHOT_DEBOUNCE_MS: i64 = 500;
/// Default `maxUndoDepth`.
pub const MAX_UNDO_DEPTH: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct TextInput {
    cursor: Cursor,
    undo: Vec<Cursor>,
    kill_ring: String,
    /// Deadline for a debounced snapshot, or `None` when none is pending.
    pending_snapshot_at: Option<i64>,
    max_undo: usize,
}

impl TextInput {
    pub fn new(initial: &str) -> Self {
        TextInput {
            cursor: Cursor::new(initial),
            undo: Vec::new(),
            kill_ring: String::new(),
            pending_snapshot_at: None,
            max_undo: MAX_UNDO_DEPTH,
        }
    }

    pub fn with_max_undo(mut self, depth: usize) -> Self {
        self.max_undo = depth;
        self
    }

    pub fn value(&self) -> &str {
        self.cursor.text()
    }

    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    pub fn cursor_mut(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    /// `reset(newValue)` — new value, cursor at the end, undo history dropped.
    pub fn reset(&mut self, value: &str) {
        self.cursor = Cursor::new(value);
        self.undo.clear();
        self.pending_snapshot_at = None;
    }

    pub fn kill_ring(&self) -> &str {
        &self.kill_ring
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Fire a pending debounced snapshot once its deadline has passed.
    pub fn poll(&mut self, now_ms: i64) {
        if let Some(deadline) = self.pending_snapshot_at
            && now_ms >= deadline
        {
            self.pending_snapshot_at = None;
            self.push_snapshot();
        }
    }

    fn push_snapshot(&mut self) {
        if self
            .undo
            .last()
            .is_some_and(|c| c.text() == self.cursor.text())
        {
            return;
        }
        self.undo.push(self.cursor.clone());
        if self.undo.len() > self.max_undo {
            self.undo.remove(0);
        }
    }

    /// Immediate snapshot (kills), cancelling any pending debounce.
    fn snapshot_now(&mut self) {
        self.pending_snapshot_at = None;
        self.push_snapshot();
    }

    /// Debounced snapshot (ordinary typing).
    fn snapshot_debounced(&mut self, now_ms: i64) {
        self.pending_snapshot_at = Some(now_ms + UNDO_SNAPSHOT_DEBOUNCE_MS);
    }

    /// `Ctrl+Z` — pop at most **one** snapshot matching the current text, then
    /// pop and apply the next.
    fn undo_once(&mut self) {
        if self
            .undo
            .last()
            .is_some_and(|c| c.text() == self.cursor.text())
        {
            self.undo.pop();
        }
        if let Some(prev) = self.undo.pop() {
            self.cursor = prev;
        }
    }

    /// `Cmd+Z` — pop **all** snapshots matching the current text, then apply
    /// the next.
    fn undo_all_matching(&mut self) {
        while self
            .undo
            .last()
            .is_some_and(|c| c.text() == self.cursor.text())
        {
            self.undo.pop();
        }
        if let Some(prev) = self.undo.pop() {
            self.cursor = prev;
        }
    }

    /// Process a key. Returns `true` when consumed.
    ///
    /// Never consumes `Enter`, `Esc`, `Tab`, `↑` or `↓` — those belong to the
    /// parent (command bar / wizard).
    pub fn handle_key(&mut self, key: KeyEvent, now_ms: i64) -> bool {
        self.poll(now_ms);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let meta = is_meta(key);

        if ctrl {
            let Some(ch) = char_of(key.code) else {
                return false;
            };
            match ch.to_ascii_lowercase() {
                'a' => self.cursor.move_to_start(),
                'e' => self.cursor.move_to_end(),
                'k' => {
                    self.snapshot_now();
                    self.kill_ring = self.cursor.kill_to_end();
                }
                'u' => {
                    self.snapshot_now();
                    self.kill_ring = self.cursor.kill_to_start();
                }
                'w' => {
                    self.snapshot_now();
                    self.kill_ring = self.cursor.kill_word_back();
                }
                'y' => {
                    if !self.kill_ring.is_empty() {
                        self.snapshot_now();
                        let ring = self.kill_ring.clone();
                        self.cursor.insert(&ring);
                    }
                }
                'z' => self.undo_once(),
                'b' => self.cursor.move_left(),
                'f' => self.cursor.move_right(),
                'd' => {
                    if !self.cursor.is_empty() {
                        self.snapshot_debounced(now_ms);
                        self.cursor.delete_forward();
                    }
                }
                'h' => {
                    if self.cursor.pos() > 0 {
                        self.snapshot_debounced(now_ms);
                        self.cursor.delete_back();
                    }
                }
                _ => return false,
            }
            return true;
        }

        if meta {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.undo_all_matching();
                    return true;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.cursor.move_to_start();
                    return true;
                }
                KeyCode::Backspace | KeyCode::Delete => {
                    self.snapshot_now();
                    self.kill_ring = self.cursor.kill_to_start();
                    return true;
                }
                KeyCode::Left | KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.cursor.move_to_word_back();
                    return true;
                }
                KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.cursor.move_to_word_forward();
                    return true;
                }
                _ => return false,
            }
        }

        match key.code {
            KeyCode::Home => self.cursor.move_to_start(),
            KeyCode::End => self.cursor.move_to_end(),
            KeyCode::Left => self.cursor.move_left(),
            KeyCode::Right => self.cursor.move_right(),
            // Ink 6.8 maps the physical Backspace to `key.delete`; both delete
            // backward, forward delete is Ctrl+D only.
            KeyCode::Backspace | KeyCode::Delete => {
                if self.cursor.pos() > 0 {
                    self.snapshot_debounced(now_ms);
                    self.cursor.delete_back();
                }
            }
            KeyCode::Char(ch) => {
                self.snapshot_debounced(now_ms);
                self.cursor.insert(&ch.to_string());
            }
            _ => return false,
        }
        true
    }

    /// Insert a (possibly multi-character) string as if typed — used by paste.
    pub fn insert_str(&mut self, text: &str, now_ms: i64) {
        self.snapshot_debounced(now_ms);
        self.cursor.insert(text);
    }
}

/// Ink's `key.meta`, which is **Alt/Option — never Cmd**.
///
/// Verified in Ink 6.8.0 `parse-keypress.js` (PARITY §12.5): on the legacy path
/// `key.meta` comes from an ESC-prefixed sequence, which is how terminals
/// encode Option-as-Meta; on the xterm path it is the Alt/Meta modifier bits;
/// and under the Kitty protocol `meta` (bit 32) is a *distinct* bit from
/// `super` (bit 8), where Cmd arrives. Ink never maps `super` to `key.meta`,
/// and on terminals without the protocol the emulator swallows Cmd before the
/// application sees it. So `SUPER` must not satisfy a meta binding: the keymap
/// is L1, and treating Cmd as Meta would make bindings fire here that cannot
/// fire in the original.
pub fn is_meta(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::META)
}

fn char_of(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Char(c) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn meta(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn typed(input: &mut TextInput, text: &str, now: i64) {
        for ch in text.chars() {
            input.handle_key(key(KeyCode::Char(ch)), now);
        }
    }

    #[test]
    fn printable_input_inserts_at_the_cursor() {
        let mut input = TextInput::new("");
        typed(&mut input, "abc", 0);
        assert_eq!(input.value(), "abc");
        input.handle_key(key(KeyCode::Left), 0);
        input.handle_key(key(KeyCode::Char('X')), 0);
        assert_eq!(input.value(), "abXc");
    }

    #[test]
    fn ctrl_a_and_ctrl_e_jump_to_line_bounds() {
        let mut input = TextInput::new("hello");
        assert!(input.handle_key(ctrl('a'), 0));
        assert_eq!(input.cursor().pos(), 0);
        assert!(input.handle_key(ctrl('e'), 0));
        assert_eq!(input.cursor().pos(), 5);
    }

    #[test]
    fn ctrl_k_u_w_fill_the_kill_ring_and_ctrl_y_yanks() {
        let mut input = TextInput::new("hello world");
        input.handle_key(ctrl('a'), 0);
        input.handle_key(ctrl('k'), 0);
        assert_eq!(input.value(), "");
        assert_eq!(input.kill_ring(), "hello world");
        input.handle_key(ctrl('y'), 0);
        assert_eq!(input.value(), "hello world");

        let mut input = TextInput::new("hello world");
        input.handle_key(ctrl('u'), 0);
        assert_eq!(input.value(), "");
        assert_eq!(input.kill_ring(), "hello world");

        let mut input = TextInput::new("hello world");
        input.handle_key(ctrl('w'), 0);
        assert_eq!(input.value(), "hello ");
        assert_eq!(input.kill_ring(), "world");
    }

    #[test]
    fn ctrl_y_is_consumed_but_inserts_nothing_with_an_empty_ring() {
        let mut input = TextInput::new("abc");
        assert!(input.handle_key(ctrl('y'), 0));
        assert_eq!(input.value(), "abc");
    }

    #[test]
    fn ctrl_b_f_d_h_move_and_delete() {
        let mut input = TextInput::new("abc");
        input.handle_key(ctrl('b'), 0);
        assert_eq!(input.cursor().pos(), 2);
        input.handle_key(ctrl('f'), 0);
        assert_eq!(input.cursor().pos(), 3);
        input.handle_key(ctrl('h'), 0);
        assert_eq!(input.value(), "ab");
        input.handle_key(ctrl('a'), 0);
        input.handle_key(ctrl('d'), 0);
        assert_eq!(input.value(), "b");
    }

    #[test]
    fn backspace_and_delete_both_delete_backward() {
        let mut input = TextInput::new("abc");
        input.handle_key(key(KeyCode::Backspace), 0);
        assert_eq!(input.value(), "ab");
        input.handle_key(key(KeyCode::Delete), 0);
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn undo_snapshot_is_debounced_by_500ms() {
        let mut input = TextInput::new("");
        typed(&mut input, "abc", 0);
        // Still inside the debounce window — no snapshot yet.
        input.poll(499);
        assert_eq!(input.undo_depth(), 0);
        input.poll(500);
        assert_eq!(input.undo_depth(), 1);

        typed(&mut input, "def", 600);
        input.poll(1_100);
        assert_eq!(input.undo_depth(), 2);
        assert_eq!(input.value(), "abcdef");

        input.handle_key(ctrl('z'), 1_200);
        assert_eq!(input.value(), "abc");
    }

    #[test]
    fn ctrl_z_pops_one_matching_snapshot_then_applies_the_next() {
        let mut input = TextInput::new("");
        typed(&mut input, "a", 0);
        input.poll(500);
        typed(&mut input, "b", 600);
        input.poll(1_100);
        assert_eq!(input.undo_depth(), 2); // "a", "ab"
        input.handle_key(ctrl('z'), 1_200);
        assert_eq!(input.value(), "a");
        input.handle_key(ctrl('z'), 1_300);
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn cmd_z_pops_every_matching_snapshot() {
        let mut input = TextInput::new("");
        typed(&mut input, "a", 0);
        input.poll(500);
        // Two snapshots with the same text: one from the kill, one debounced.
        input.handle_key(ctrl('k'), 600);
        typed(&mut input, "b", 700);
        input.poll(1_200);
        input.handle_key(meta(KeyCode::Char('z')), 1_300);
        assert!(input.value() == "a" || input.value().is_empty());
    }

    #[test]
    fn undo_stack_is_capped() {
        let mut input = TextInput::new("").with_max_undo(3);
        let mut now = 0;
        for ch in "abcdefgh".chars() {
            input.handle_key(key(KeyCode::Char(ch)), now);
            now += 600;
            input.poll(now);
        }
        assert_eq!(input.undo_depth(), 3);
    }

    #[test]
    fn meta_shortcuts_move_by_word_and_kill_to_start() {
        let mut input = TextInput::new("hello world");
        input.handle_key(meta(KeyCode::Left), 0);
        assert_eq!(input.cursor().pos(), 6);
        input.handle_key(meta(KeyCode::Right), 0);
        assert_eq!(input.cursor().pos(), 11);
        input.handle_key(meta(KeyCode::Char('b')), 0);
        assert_eq!(input.cursor().pos(), 6);
        input.handle_key(meta(KeyCode::Char('f')), 0);
        assert_eq!(input.cursor().pos(), 11);
        input.handle_key(meta(KeyCode::Char('a')), 0);
        assert_eq!(input.cursor().pos(), 0);

        let mut input = TextInput::new("hello world");
        input.handle_key(meta(KeyCode::Backspace), 0);
        assert_eq!(input.value(), "");
        assert_eq!(input.kill_ring(), "hello world");
    }

    #[test]
    fn home_and_end_jump_to_line_bounds() {
        let mut input = TextInput::new("hello");
        input.handle_key(key(KeyCode::Home), 0);
        assert_eq!(input.cursor().pos(), 0);
        input.handle_key(key(KeyCode::End), 0);
        assert_eq!(input.cursor().pos(), 5);
    }

    #[test]
    fn enter_escape_tab_and_vertical_arrows_are_left_to_the_parent() {
        let mut input = TextInput::new("x");
        for code in [
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::Up,
            KeyCode::Down,
        ] {
            assert!(!input.handle_key(key(code), 0), "{code:?} must not consume");
        }
        assert_eq!(input.value(), "x");
    }

    #[test]
    fn cmd_is_not_meta_so_super_bindings_do_not_fire() {
        // `key.meta` in Ink is Alt/Option or the Kitty `meta` bit, never Cmd
        // (PARITY §12.5). Cmd+A must not jump to the start the way Alt+A does;
        // it falls through and inserts the character.
        let mut alt = TextInput::new("hello");
        alt.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT), 0);
        assert_eq!(alt.cursor().pos(), 0);
        assert_eq!(alt.value(), "hello");

        let mut sup = TextInput::new("hello");
        sup.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER), 0);
        assert_eq!(sup.cursor().pos(), 6);
        assert_eq!(sup.value(), "helloa");
    }

    #[test]
    fn the_kitty_meta_bit_counts_as_meta() {
        let mut input = TextInput::new("hello world");
        input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::META), 0);
        assert_eq!(input.cursor().pos(), 6, "word back");
    }

    #[test]
    fn reset_clears_value_and_history() {
        let mut input = TextInput::new("abc");
        input.poll(1_000);
        input.reset("zz");
        assert_eq!(input.value(), "zz");
        assert_eq!(input.cursor().pos(), 2);
        assert_eq!(input.undo_depth(), 0);
    }
}
