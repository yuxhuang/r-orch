//! The terminal event loop (`src/cli/commands/tui.ts`'s `render(...)` plus
//! `waitUntilExit()`).
//!
//! Lives here rather than in the CLI so `orch tui` is one call: build a
//! [`TuiBackend`] and hand it to [`run`]. Nothing in this module is reachable
//! from `TestBackend`, so it stays as thin as possible — all behaviour worth
//! testing is in [`crate::app`], [`crate::keys`] and [`crate::views`].

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
use crate::tty::{Ready, TtyInput};
use crate::views;

const MAX_EVENTS_PER_FRAME: usize = 64;
const MAX_DRAIN: Duration = Duration::from_millis(40);

/// Set to `0`, `off`, `false` or `no` to start with mouse capture off.
///
/// `ORCH_*` is the user-facing runtime prefix (`ORCH_WEB_PASSWORD`,
/// `ORCH_AGENT_ID`), while `ORCHESTRY_*` is storage and debug internals
/// (`ORCHESTRY_DIR`, `ORCHESTRY_DEBUG`). Capture is user-facing runtime. It
/// reaches `orch web`'s child for free: the pty builder inherits the parent
/// environment and removes only the password variable (`web/pty.rs`).
///
/// An env var rather than a flag or a config key on purpose: the flag surface
/// is **L0** (PARITY.md §1.1 item 2, asserted by `tree.rs`'s `SURFACE` table)
/// and `config.yml` is a closed **L0** schema both binaries read (§6.5).
pub const MOUSE_ENV: &str = "ORCH_TUI_MOUSE";

/// The DEC private modes mouse capture turns on: normal tracking (1000) and
/// SGR extended coordinates (1006).
///
/// Deliberately **not** `crossterm::EnableMouseCapture`, which also sets 1002
/// and **1003**. 1003 reports every pointer move — a continuous stream
/// through tmux and the `orch web` websocket for a consumer we do not have,
/// since there is no hover state. 1015 is redundant under 1006, ambiguous
/// with other CSI sequences, and unimplemented by xterm.js. 1006 is not
/// optional: legacy X10 encodes a coordinate in one byte, so it cannot
/// address past column 223.
pub const ENABLE_MOUSE: &str = "\x1b[?1000h\x1b[?1006h";

/// The reverse of [`ENABLE_MOUSE`], innermost mode first — mirroring
/// crossterm's own `DisableMouseCapture`.
pub const DISABLE_MOUSE: &str = "\x1b[?1006l\x1b[?1000l";

/// Does the environment forbid mouse capture at startup?
fn mouse_env_allows() -> bool {
    mouse_allowed_by(std::env::var(MOUSE_ENV).ok().as_deref())
}

/// The parsing, split out so it is testable without mutating the process
/// environment — which this crate could not do anyway, since `set_var` is
/// `unsafe` and `unsafe_code` is forbidden here.
fn mouse_allowed_by(value: Option<&str>) -> bool {
    match value {
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ),
        None => true,
    }
}

/// Write the narrow mode pair ourselves. The enable/disable *write* is ours;
/// **parsing stays crossterm's**, which is what ARCHITECTURE §4.8 protects.
#[cfg(unix)]
fn set_mouse_capture(stdout: &mut Stdout, on: bool) -> io::Result<()> {
    stdout.write_all(if on { ENABLE_MOUSE } else { DISABLE_MOUSE }.as_bytes())?;
    stdout.flush()
}

/// On Windows `EnableMouseCapture` writes no ANSI at all — it reports
/// `is_ansi_code_supported() == false` and calls the console API — so a
/// hand-written sequence would silently do nothing. Delegate there and accept
/// 1003's motion traffic, which has no tmux or websocket hop to flood.
#[cfg(windows)]
fn set_mouse_capture(stdout: &mut Stdout, on: bool) -> io::Result<()> {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    if on {
        execute!(stdout, EnableMouseCapture)
    } else {
        execute!(stdout, DisableMouseCapture)
    }
}

/// Why the draw loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// `q`, or the `stop` flag: someone asked to leave, and there is still a
    /// terminal to show the shutdown on.
    Requested,
    /// The terminal went away mid-session (PARITY.md §14.39). Nobody is
    /// watching the shutdown and nobody is waiting out an agent's stop grace,
    /// which is what lets the caller bound the wind-down (§14.40).
    TerminalGone,
}

/// Run the dashboard until the user quits.
///
/// Restores the terminal on every exit path, including a panic in the draw
/// loop: the guard's `Drop` leaves the alternate screen before the panic
/// unwinds past it.
pub fn run<B: TuiBackend>(backend: B) -> io::Result<Exit> {
    run_until(backend, &AtomicBool::new(false))
}

/// [`run`], but also returns once `stop` is set (checked at least once per
/// animation tick). `orch tui` sets it on SIGHUP or SIGTERM so a closed
/// terminal takes the same graceful path as `q`.
///
/// Also returns when the terminal itself hangs up, which is *not* the same
/// event: a closed pty master sets no flag of ours, and a SIGHUP that does is
/// useless if the loop never reaches this check again. See `src/tty.rs`.
pub fn run_until<B: TuiBackend>(backend: B, stop: &AtomicBool) -> io::Result<Exit> {
    let mut guard = TerminalGuard::enter()?;
    let mut input = TtyInput::open();
    let mut app = App::new(backend, SystemClock);
    // The env var is the startup switch; `F2` and `/mouse` move the same flag
    // from here on, and the guard follows it.
    app.state.mouse.enabled = guard.mouse;

    loop {
        // A terminal that goes away mid-frame fails the ioctl or the write
        // instead of the wait, so both of those ask whether it is still there
        // before reporting an error nobody is left to read.
        let size = match guard.terminal().size() {
            Ok(size) => size,
            Err(err) => return hung_up_or(&mut input, err),
        };
        app.state.width = size.width;
        app.state.height = size.height;
        app.tick();
        // The map describes the frame just drawn, so it is captured out of
        // the closure rather than rebuilt: `Terminal::draw` wants a callback
        // returning `()`.
        let mut hits = None;
        if let Err(err) = guard
            .terminal()
            .draw(|frame| hits = Some(views::draw(frame, &app)))
        {
            return hung_up_or(&mut input, err);
        }
        if let Some(hits) = hits {
            app.set_hits(hits);
        }
        guard.reconcile_mouse(app.state.mouse.enabled);

        if app.state.should_exit || stop.load(Ordering::Relaxed) {
            return Ok(Exit::Requested);
        }

        // Events crossterm has already parsed are invisible to our own poll —
        // they are no longer on the wire — so ask it before waiting. Without
        // this, a batch the drain broke out of mid-way (below) would sit
        // unread until the user did something else.
        //
        // Hangup is checked first, because `event::poll` on a dead tty is the
        // spin this module exists to avoid.
        let queued = match input.wait(Duration::ZERO)? {
            Ready::HungUp => return hung_up(),
            _ => event::poll(Duration::ZERO)?,
        };
        // Never wait longer than one animation tick, so spinners and the
        // scanner bar keep moving while the user is idle.
        if !queued {
            match input.wait(Duration::from_millis(ANIM_TICK_MS as u64))? {
                Ready::HungUp => return hung_up(),
                Ready::Idle => continue,
                Ready::Input => {}
            }
        }

        // Drain what is already queued before drawing again, so a flood of
        // input (a big paste) cannot keep a stop request or `q` waiting
        // behind one frame per key, but not so long that the screen
        // freezes under a continuous one.
        let started = Instant::now();
        let mut handled = 0;
        loop {
            handled += 1;
            // Residual window, considered and accepted: `read()` blocks inside
            // crossterm's own loop — the one whose `Ok(0)`-forever behaviour
            // §14.39 is about — so if the terminal hangs up *after* our wait
            // said `Input` but before crossterm has an event, the spin is back
            // and the hang-up check below is unreachable. It needs the last
            // bytes on the wire to be a truncated sequence, and every other
            // path is checked, so this is narrower than the bug it replaces
            // rather than a gap left open by accident.
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                Event::Mouse(mouse) => {
                    app.handle_mouse(mouse);
                    // An action that changed the layout retires the geometry,
                    // so stop draining and redraw: the rest of this batch is
                    // still queued in crossterm and gets handled against the
                    // new frame instead of being dropped against the old one.
                    // This is what design §3.2.3 means by "the drain loop then
                    // breaks to redraw".
                    if !app.state.mouse.hits.is_valid() {
                        break;
                    }
                }
                // Retires the map only if the frame we drew is not already
                // this size; see `App::resize`.
                Event::Resize(width, height) => app.resize(width, height),
                _ => {}
            }
            if app.state.should_exit
                || stop.load(Ordering::Relaxed)
                || handled >= MAX_EVENTS_PER_FRAME
                || started.elapsed() >= MAX_DRAIN
            {
                break;
            }
            // Check for a hangup before every re-entry into crossterm, which
            // is where a dead terminal spins. The `event::poll` that follows
            // is still crossterm's, because it is the only thing that can see
            // events crossterm has already parsed but not handed over — they
            // are no longer on the wire for `wait` to find.
            if input.wait(Duration::ZERO)? == Ready::HungUp {
                return hung_up();
            }
            if !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

/// A terminal that went away ends the loop the way `q` does: the guard's
/// `Drop` restores what it can, and the caller stops the orchestrator and
/// releases its lock. Not an error — there is nobody left to report one to.
fn hung_up() -> io::Result<Exit> {
    tracing::info!("the terminal hung up; shutting the dashboard down");
    Ok(Exit::TerminalGone)
}

/// Terminal I/O that failed *because* the terminal hung up is the hangup, not
/// a dashboard failure: reporting it would exit non-zero and print to a
/// terminal that is not there. Anything else is a real error and propagates.
fn hung_up_or(input: &mut TtyInput, err: io::Error) -> io::Result<Exit> {
    match input.wait(Duration::ZERO) {
        Ok(Ready::HungUp) => hung_up(),
        _ => Err(err),
    }
}

struct TerminalGuard {
    /// `Some` for the guard's whole life; `Drop` takes it to decide how it
    /// goes, which is the only reason this is an `Option`.
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    /// Whether the terminal reported support for the keyboard enhancement
    /// protocol, i.e. whether flags were actually pushed and must be popped.
    enhanced_keys: bool,
    /// Whether mouse capture is on **right now**. The guard's flag always
    /// matches what the terminal was told, so `Drop` never writes a disable
    /// for a mode that is already off, and the toggle is one-way data flow:
    /// `App` owns the desire, this owns the reality.
    mouse: bool,
}

impl TerminalGuard {
    fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        self.terminal
            .as_mut()
            .expect("the terminal is only taken by Drop, after the loop")
    }

    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        // Everything from here on has to be undone if a later step fails, and
        // `Drop` is what undoes it — so the guard exists *before* the next
        // write rather than after the last one. Its `terminal` is filled in
        // last; `Drop` restores the modes whether or not it is there, because
        // the restore writes come before it takes the terminal.
        //
        // Previously a failed mouse-enable (or a failed `Terminal::new`, or a
        // failed keyboard-flag push) returned `Err` with raw mode still on and
        // the alternate screen still entered — a shell the user could not see
        // their own typing in.
        let mut guard = TerminalGuard {
            terminal: None,
            enhanced_keys: false,
            mouse: false,
        };
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
        if supports_keyboard_enhancement().unwrap_or(false) {
            queue!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            stdout.flush()?;
            // Recorded only once the push actually went out, so `Drop` never
            // pops flags that were never set.
            guard.enhanced_keys = true;
        }
        if mouse_env_allows() {
            set_mouse_capture(&mut stdout, true)?;
            guard.mouse = true;
        }
        guard.terminal = Some(Terminal::new(CrosstermBackend::new(stdout))?);
        Ok(guard)
    }

    /// Bring the terminal in line with what `App` wants. One-way: state
    /// decides, the terminal follows, so `F2` and `/mouse` are testable
    /// without a terminal.
    fn reconcile_mouse(&mut self, wanted: bool) {
        if self.mouse == wanted {
            return;
        }
        let mut stdout = io::stdout();
        if set_mouse_capture(&mut stdout, wanted).is_ok() {
            self.mouse = wanted;
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        // Mouse first: a leaked capture mode is the most visible thing to the
        // terminal's next user, who would see clicks arrive as escape
        // sequences. Every write here is best-effort, so an EIO on a dead
        // terminal cannot abort the rest of the sequence.
        if self.mouse {
            let _ = set_mouse_capture(&mut stdout, false);
        }
        if self.enhanced_keys {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let Some(mut terminal) = self.terminal.take() else {
            return;
        };
        if terminal.show_cursor().is_err() {
            // Every restore write above failed too: the terminal is gone.
            // ratatui's own `Drop` would retry `show_cursor` and `eprintln!`
            // the error — and `eprintln!` to a closed stderr *panics*, which
            // turns "the user closed their terminal" into "the dashboard
            // panicked" and a non-zero exit. Skipping that drop releases
            // nothing: the backend is `Stdout`, which we never owned, and the
            // frame buffers go with the process that is already leaving.
            std::mem::forget(terminal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mode set is the whole of §3.5's capture policy, and it is the one
    /// part of this module that can be tested without a terminal — which also
    /// covers the restore *ordering* requirement.
    #[test]
    fn the_enable_set_is_exactly_normal_tracking_plus_sgr() {
        assert_eq!(ENABLE_MOUSE, "\x1b[?1000h\x1b[?1006h");
        for unwanted in ["1002", "1003", "1015"] {
            assert!(
                !ENABLE_MOUSE.contains(unwanted),
                "mode {unwanted} must never be enabled: 1003 streams a report \
                 per pointer move through tmux and the websocket for a \
                 consumer we do not have"
            );
            assert!(!DISABLE_MOUSE.contains(unwanted));
        }
    }

    #[test]
    fn the_disable_set_is_the_enable_set_reversed() {
        assert_eq!(DISABLE_MOUSE, "\x1b[?1006l\x1b[?1000l");
        let enabled: Vec<&str> = ENABLE_MOUSE.split("\x1b[?").skip(1).collect();
        let disabled: Vec<&str> = DISABLE_MOUSE.split("\x1b[?").skip(1).collect();
        let modes = |seq: &[&str]| -> Vec<String> {
            seq.iter()
                .map(|m| m.trim_end_matches(['h', 'l']).to_string())
                .collect()
        };
        let mut reversed = modes(&enabled);
        reversed.reverse();
        assert_eq!(
            modes(&disabled),
            reversed,
            "the innermost mode is turned off first, as crossterm's own \
             DisableMouseCapture does"
        );
    }

    #[test]
    fn the_env_var_recognises_the_usual_falsey_spellings() {
        for value in ["0", "off", "false", "no", "OFF", " no "] {
            assert!(
                !mouse_allowed_by(Some(value)),
                "{value:?} must disable capture"
            );
        }
        for value in ["1", "on", "true", "yes", ""] {
            assert!(
                mouse_allowed_by(Some(value)),
                "{value:?} must leave capture on"
            );
        }
        assert!(mouse_allowed_by(None), "unset means on by default");
    }
}
