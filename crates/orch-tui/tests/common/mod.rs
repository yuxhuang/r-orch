//! The `TestBackend` harness: push a key, draw, assert — synchronously, never
//! with a sleep.
//!
//! Each test binary compiles this module separately and uses a subset of it,
//! so the unused-method lint would fire in every one of them.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of this module"
)]

use orch_tui::App;
use orch_tui::backend::{Call, FakeBackend};
use orch_tui::clock::{Clock, TestClock};
use orch_tui::hit::{HitMap, Zone};
use orch_tui::views;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

pub const WIDTH: u16 = 100;
pub const HEIGHT: u16 = 30;

pub struct Harness {
    pub app: App<FakeBackend, TestClock>,
    pub terminal: Terminal<TestBackend>,
    pub clock: TestClock,
}

impl Harness {
    pub fn new() -> Self {
        Harness::with(FakeBackend::new())
    }

    pub fn with(backend: FakeBackend) -> Self {
        Harness::sized(backend, WIDTH, HEIGHT)
    }

    pub fn sized(backend: FakeBackend, width: u16, height: u16) -> Self {
        let clock = TestClock::new(1_000_000);
        let mut app = App::new(backend, clock.clone());
        app.state.width = width;
        app.state.height = height;
        Harness {
            app,
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
            clock,
        }
    }

    /// Push a bare key.
    pub fn press(&mut self, code: KeyCode) -> &mut Self {
        self.app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        self
    }

    /// Push a key with modifiers.
    pub fn press_mod(&mut self, code: KeyCode, modifiers: KeyModifiers) -> &mut Self {
        self.app.handle_key(KeyEvent::new(code, modifiers));
        self
    }

    /// Push a character.
    pub fn key(&mut self, ch: char) -> &mut Self {
        self.press(KeyCode::Char(ch))
    }

    /// Type a string, character by character.
    pub fn type_str(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            self.press(KeyCode::Char(ch));
        }
        self
    }

    /// A bracketed paste, as the runner's `Event::Paste` arm delivers it.
    pub fn paste(&mut self, text: &str) -> &mut Self {
        self.app.handle_paste(text);
        self
    }

    /// A key *release* event — the dispatcher must ignore it.
    pub fn release(&mut self, code: KeyCode) -> &mut Self {
        let mut event = KeyEvent::new(code, KeyModifiers::NONE);
        event.kind = KeyEventKind::Release;
        self.app.handle_key(event);
        self
    }

    /// Draw a frame and adopt its hit map, exactly as the runner does — so a
    /// test has to draw before it can click, which mirrors reality.
    pub fn draw(&mut self) -> &mut Self {
        let app = &self.app;
        let mut hits = None;
        self.terminal
            .draw(|frame| hits = Some(views::draw(frame, app)))
            .unwrap();
        self.app.set_hits(hits.expect("draw ran the callback"));
        self
    }

    // ── Mouse ──────────────────────────────────────────────────────────────

    fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) -> &mut Self {
        self.app.handle_mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
        self
    }

    pub fn press_at(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    pub fn release_at(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::Up(MouseButton::Left), x, y)
    }

    /// A full click: press and release on the same cell.
    pub fn click(&mut self, x: u16, y: u16) -> &mut Self {
        self.press_at(x, y).release_at(x, y)
    }

    /// Two clicks with no clock movement between them, so they land inside
    /// the double-click window.
    pub fn double_click(&mut self, x: u16, y: u16) -> &mut Self {
        self.click(x, y).click(x, y)
    }

    /// A right-button press with no release, for the legacy-encoding case.
    pub fn press_right_at(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::Down(MouseButton::Right), x, y)
    }

    pub fn right_click(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::Down(MouseButton::Right), x, y)
            .mouse(MouseEventKind::Up(MouseButton::Right), x, y)
    }

    pub fn middle_click(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::Down(MouseButton::Middle), x, y)
            .mouse(MouseEventKind::Up(MouseButton::Middle), x, y)
    }

    pub fn wheel_up(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::ScrollUp, x, y)
    }

    pub fn wheel_down(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::ScrollDown, x, y)
    }

    pub fn scroll_left(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::ScrollLeft, x, y)
    }

    pub fn scroll_right(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::ScrollRight, x, y)
    }

    pub fn drag_to(&mut self, x: u16, y: u16) -> &mut Self {
        self.mouse(MouseEventKind::Drag(MouseButton::Left), x, y)
    }

    /// Where `needle` was drawn, as a cell. The column is a **display width**,
    /// not a byte offset: these frames carry `↑↓`, `━` and box glyphs.
    pub fn find_text(&mut self, needle: &str) -> Option<(u16, u16)> {
        let text = self.text();
        for (y, line) in text.lines().enumerate() {
            if let Some(byte_offset) = line.find(needle) {
                let column = unicode_width::UnicodeWidthStr::width(&line[..byte_offset]) as u16;
                return Some((column, y as u16));
            }
        }
        None
    }

    /// Draw, find `needle` and click its first cell.
    ///
    /// The highest-value affordance in this harness: a test reads as "click
    /// the `R run` hint" rather than carrying coordinates that a layout change
    /// would silently invalidate.
    #[track_caller]
    pub fn click_text(&mut self, needle: &str) -> &mut Self {
        let (x, y) = self
            .find_text(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{}", self.text()));
        self.click(x, y)
    }

    #[track_caller]
    pub fn double_click_text(&mut self, needle: &str) -> &mut Self {
        let (x, y) = self
            .find_text(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{}", self.text()));
        self.double_click(x, y)
    }

    /// The hit map of the last drawn frame.
    pub fn hits(&self) -> &HitMap {
        &self.app.state.mouse.hits
    }

    /// The zone under a cell, for asserting *and* for printing what was
    /// actually found when an assert fails.
    pub fn zone_at(&self, x: u16, y: u16) -> Option<&Zone> {
        self.app.state.mouse.hits.resolve(x, y)
    }

    /// One rendered row, as text. Does not redraw.
    pub fn line(&self, y: u16) -> String {
        let buffer = self.terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    /// Draw and flatten the buffer to text, one line per row.
    pub fn text(&mut self) -> String {
        self.draw();
        let buffer = self.terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[track_caller]
    pub fn assert_text(&mut self, needle: &str) {
        let text = self.text();
        assert!(
            text.contains(needle),
            "expected {needle:?} in frame:\n{text}"
        );
    }

    #[track_caller]
    pub fn refute_text(&mut self, needle: &str) {
        let text = self.text();
        assert!(
            !text.contains(needle),
            "expected no {needle:?} in frame:\n{text}"
        );
    }

    /// Advance the clock and run one frame tick.
    pub fn advance(&mut self, ms: i64) -> &mut Self {
        self.clock.advance_ms(ms);
        self.app.tick();
        self
    }

    pub fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Flush the batched message queue and return the feed texts.
    pub fn messages(&mut self) -> Vec<String> {
        let now = self.clock.now_ms();
        self.app.state.flush_messages(now);
        self.app
            .state
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    #[track_caller]
    pub fn assert_message(&mut self, needle: &str) {
        let messages = self.messages();
        assert!(
            messages.iter().any(|m| m.contains(needle)),
            "expected a message containing {needle:?}, got {messages:#?}"
        );
    }

    #[track_caller]
    pub fn refute_message(&mut self, needle: &str) {
        let messages = self.messages();
        assert!(
            !messages.iter().any(|m| m.contains(needle)),
            "expected no message containing {needle:?}, got {messages:#?}"
        );
    }

    pub fn calls(&self) -> &[Call] {
        &self.app.backend.calls
    }

    #[track_caller]
    pub fn assert_called(&self, pred: impl Fn(&Call) -> bool) {
        assert!(
            self.app.backend.calls.iter().any(pred),
            "expected a matching backend call, got {:#?}",
            self.app.backend.calls
        );
    }

    #[track_caller]
    pub fn refute_called(&self, pred: impl Fn(&Call) -> bool) {
        assert!(
            !self.app.backend.calls.iter().any(pred),
            "expected no matching backend call, got {:#?}",
            self.app.backend.calls
        );
    }

    /// Run a command through the command bar, as `/` + text + Enter would.
    pub fn run_command(&mut self, command: &str) -> &mut Self {
        self.app.execute_command(command);
        self
    }
}

impl Default for Harness {
    fn default() -> Self {
        Harness::new()
    }
}
