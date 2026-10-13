//! The `TestBackend` harness: push a key, draw, assert — synchronously, never
//! with a sleep (PARITY.md §14.9.3).
//!
//! Each test binary compiles this module separately and uses a subset of it,
//! so the unused-method lint would fire in every one of them.
#![allow(dead_code)]

use orch_tui::App;
use orch_tui::backend::{Call, FakeBackend};
use orch_tui::clock::{Clock, TestClock};
use orch_tui::views;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

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

    /// A key *release* event — the dispatcher must ignore it.
    pub fn release(&mut self, code: KeyCode) -> &mut Self {
        let mut event = KeyEvent::new(code, KeyModifiers::NONE);
        event.kind = KeyEventKind::Release;
        self.app.handle_key(event);
        self
    }

    pub fn draw(&mut self) -> &mut Self {
        let app = &self.app;
        self.terminal.draw(|frame| views::draw(frame, app)).unwrap();
        self
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
