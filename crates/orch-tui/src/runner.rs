//! The terminal event loop (`src/cli/commands/tui.ts`'s `render(...)` plus
//! `waitUntilExit()`).
//!
//! Lives here rather than in the CLI so `orch tui` is one call: build a
//! [`TuiBackend`] and hand it to [`run`]. Nothing in this module is reachable
//! from `TestBackend`, so it stays as thin as possible — all behaviour worth
//! testing is in [`crate::app`], [`crate::keys`] and [`crate::views`].

use std::io::{self, Stdout, Write};
use std::time::Duration;

use ratatui::Terminal;
use ratatui::crossterm::event::{
    self, Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::crossterm::{execute, queue};
use ratatui::prelude::CrosstermBackend;

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::SystemClock;
use crate::messages::ANIM_TICK_MS;
use crate::views;

/// Run the dashboard until the user quits.
///
/// Restores the terminal on every exit path, including a panic in the draw
/// loop: the guard's `Drop` leaves the alternate screen before the panic
/// unwinds past it.
pub fn run<B: TuiBackend>(backend: B) -> io::Result<()> {
    let mut guard = TerminalGuard::enter()?;
    let mut app = App::new(backend, SystemClock);

    loop {
        let size = guard.terminal.size()?;
        app.state.width = size.width;
        app.state.height = size.height;
        app.tick();
        guard.terminal.draw(|frame| views::draw(frame, &app))?;

        if app.state.should_exit {
            return Ok(());
        }

        // Never wait longer than one animation tick, so spinners and the
        // scanner bar keep moving while the user is idle.
        if event::poll(Duration::from_millis(ANIM_TICK_MS as u64))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                // A resize is picked up from `terminal.size()` on the next
                // iteration; the event itself only wakes the loop.
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Whether the terminal reported support for the keyboard enhancement
    /// protocol, i.e. whether flags were actually pushed and must be popped.
    enhanced_keys: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        // `Ctrl+Enter` is only distinguishable from `Enter` under the Kitty
        // keyboard protocol; the wizard keeps its `Tab` confirm fallback for
        // terminals without it (docs/parity/tui.md §2.7).
        //
        // Ask the terminal rather than inferring support from a successful
        // write: the push is just bytes on a pipe, so it "succeeds" against a
        // terminal that ignores it, and we would then pop flags we never set.
        // The query blocks briefly, so it runs here and never in the loop.
        let enhanced_keys = supports_keyboard_enhancement().unwrap_or(false);
        if enhanced_keys {
            queue!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            stdout.flush()?;
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(TerminalGuard {
            terminal,
            enhanced_keys,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.enhanced_keys {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}
