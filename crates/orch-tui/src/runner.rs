//! The terminal event loop.
//!
//! Lives here rather than in the CLI so `orch tui` is one call: build a
//! [`TuiBackend`] and hand it to [`run`]. The driver boundary also lets tests
//! exercise the production bootstrap and view-intent hand-off without a tty.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
use crate::state::{KeywordFilters, ViewIntent};
use crate::tty::{Ready, TtyInput};
use crate::views;

const MAX_EVENTS_PER_FRAME: usize = 64;
const MAX_DRAIN: Duration = Duration::from_millis(40);

/// How long [`has_parsed_event`] lets crossterm look. Not a wait for input:
/// any non-zero value gets crossterm to check what it has already parsed, and
/// this is the most a check can add before the next frame when it has nothing.
const PARSED_EVENT_PROBE: Duration = Duration::from_millis(1);

/// Set to `0`, `off`, `false` or `no` to start with mouse capture off.
///
/// `ORCH_*` is the user-facing runtime prefix (`ORCH_WEB_PASSWORD`,
/// `ORCH_AGENT_ID`), while `ORCHESTRY_*` is storage and debug internals
/// (`ORCHESTRY_DIR`, `ORCHESTRY_DEBUG`). Capture is user-facing runtime. It
/// reaches `orch web`'s child for free: the pty builder inherits the parent
/// environment and removes only the password variable (`web/pty.rs`).
///
/// An env var rather than a flag or a config key on purpose: the flag surface
/// is fixed (asserted by `tree.rs`'s `SURFACE` table) and `config.yml` is a
/// closed schema both binaries read.
pub const MOUSE_ENV: &str = "ORCH_TUI_MOUSE";

/// The DEC private modes mouse capture turns on: normal tracking (1000),
/// button-motion tracking (1002) and SGR extended coordinates (1006).
///
/// Deliberately **not** `crossterm::EnableMouseCapture`, which also sets
/// **1003**. 1003 reports every pointer move — a continuous stream
/// through tmux and the `orch web` websocket for a consumer we do not have,
/// since there is no hover state. 1002 reports drags for text selection. 1015 is redundant under 1006, ambiguous
/// with other CSI sequences, and unimplemented by xterm.js. 1006 is not
/// optional: legacy X10 encodes a coordinate in one byte, so it cannot
/// address past column 223.
pub const ENABLE_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";

/// The reverse of [`ENABLE_MOUSE`], innermost mode first — mirroring
/// crossterm's own `DisableMouseCapture`.
pub const DISABLE_MOUSE: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

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

/// Write the narrow mode set ourselves. The enable/disable *write* is ours;
/// **parsing stays crossterm's**, which is what ARCHITECTURE §4.8 protects.
#[cfg(unix)]
fn set_mouse_capture(stdout: &mut Stdout, on: bool) -> io::Result<()> {
    write_mouse_capture(stdout, on)
}

#[cfg(any(unix, test))]
fn write_mouse_capture(writer: &mut impl Write, on: bool) -> io::Result<()> {
    let result = writer
        .write_all(if on { ENABLE_MOUSE } else { DISABLE_MOUSE }.as_bytes())
        .and_then(|()| writer.flush());
    if result.is_err() && on {
        // An enable can fail after its first mode reached the terminal. The
        // guard has not marked capture active yet, so undo partial writes here.
        let _ = writer.write_all(DISABLE_MOUSE.as_bytes());
        let _ = writer.flush();
    }
    result
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
    /// The terminal went away mid-session. Nobody is
    /// watching the shutdown and nobody is waiting out an agent's stop grace,
    /// which is what lets the caller bound the wind-down.
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
    run_until_with_filters(backend, stop, &mut KeywordFilters::default())
}

/// [`run_until`] with list keywords carried across explicit retries within one
/// dashboard lifetime. Each call bootstraps a fresh app from `backend`; only
/// keywords are restored, and selection is clamped to its filtered live rows.
/// The caller discards `filters` on quit and starts a new dashboard with default.
pub fn run_until_with_filters<B: TuiBackend>(
    backend: B,
    stop: &AtomicBool,
    filters: &mut KeywordFilters,
) -> io::Result<Exit> {
    let mut driver = TerminalDashboard::default();
    driver.enter()?;
    with_keyword_filters(backend, filters, |app| driver.draw(app, stop))
}

fn with_keyword_filters<B: TuiBackend, T>(
    backend: B,
    filters: &mut KeywordFilters,
    draw: impl FnOnce(&mut App<B>) -> T,
) -> T {
    let mut app = App::new_with_filters(backend, SystemClock, filters);
    let outcome = draw(&mut app);
    *filters = app.state.keyword_filters();
    outcome
}

/// The presentation half of a dashboard session. Embedders can drive the same
/// runner entry with deterministic input instead of opening a terminal.
pub trait DashboardDriver {
    /// Enter terminal mode before bootstrap. Non-terminal drivers need no setup.
    fn enter(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Retire input belonging to the previous screen before the new frame.
    fn discard_pending(&mut self) -> io::Result<()>;

    fn draw<B: TuiBackend>(self, app: &mut App<B>, stop: &AtomicBool) -> io::Result<Exit>;
}

/// The interactive terminal driver used by the CLI and the legacy runners.
#[derive(Default)]
pub struct TerminalDashboard {
    session: Option<(TerminalGuard, TtyInput)>,
}

impl DashboardDriver for TerminalDashboard {
    fn enter(&mut self) -> io::Result<()> {
        self.session = Some((TerminalGuard::enter()?, TtyInput::open()));
        Ok(())
    }

    fn discard_pending(&mut self) -> io::Result<()> {
        let Some((_, input)) = self.session.as_mut() else {
            return Err(io::Error::other("dashboard terminal was not entered"));
        };
        let result = discard_pending_events(|| {
            if input.wait(Duration::ZERO)? == Ready::HungUp {
                // The draw loop handles hangup without re-entering crossterm.
                return Ok(None);
            }
            if has_parsed_event()? {
                event::read().map(Some)
            } else {
                Ok(None)
            }
        });
        match result {
            Err(_) if input.wait(Duration::ZERO)? == Ready::HungUp => Ok(()),
            other => other,
        }
    }

    fn draw<B: TuiBackend>(self, app: &mut App<B>, stop: &AtomicBool) -> io::Result<Exit> {
        let Some((mut guard, mut input)) = self.session else {
            return Err(io::Error::other("dashboard terminal was not entered"));
        };
        app.state.mouse.enabled = guard.mouse;
        run_loop(&mut guard, &mut input, app, stop)
    }
}

/// Bootstrap a fresh dashboard, restore its plain list intent, and return the
/// updated intent on exit. `discard_pending` is true only after an explicit
/// retry: the driver retires input queued before the first new frame. It must
/// handle input arriving after that frame through the retry-gesture boundary.
/// First starts pass false and have no gesture.
/// The caller drops `intent` on quit; an independent dashboard uses defaults.
pub fn run_until_with_view_intent<B: TuiBackend>(
    backend: B,
    stop: &AtomicBool,
    intent: &mut ViewIntent,
    discard_pending: bool,
    mut driver: impl DashboardDriver,
) -> io::Result<Exit> {
    driver.enter()?;
    let mut app = App::new_with_view_intent(backend, SystemClock, intent);
    if discard_pending {
        driver.discard_pending()?;
        app.begin_retry_gesture();
    }
    let outcome = driver.draw(&mut app, stop);
    *intent = app.state.view_intent();
    outcome
}

/// Consume the currently pending batch once, before publishing the new frame.
/// No frame/time quota: even a batch larger than MAX_EVENTS_PER_FRAME belongs
/// to the previous screen. `next` is a readiness probe, not a quiet-period
/// timer; the terminal uses the same nonzero crossterm probe as the draw loop.
fn discard_pending_events(mut next: impl FnMut() -> io::Result<Option<Event>>) -> io::Result<()> {
    while next()?.is_some() {}
    Ok(())
}

fn run_loop<B: TuiBackend>(
    guard: &mut TerminalGuard,
    input: &mut TtyInput,
    app: &mut App<B>,
    stop: &AtomicBool,
) -> io::Result<Exit> {
    loop {
        // A terminal that goes away mid-frame fails the ioctl or the write
        // instead of the wait, so both of those ask whether it is still there
        // before reporting an error nobody is left to read.
        let size = match guard.terminal().size() {
            Ok(size) => size,
            Err(err) => return hung_up_or(input, err),
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
            .draw(|frame| hits = Some(views::draw(frame, app)))
        {
            return hung_up_or(input, err);
        }
        if let Some(hits) = hits {
            app.set_hits(hits);
        }
        guard.reconcile_mouse(app.state.mouse.enabled);

        if stop.load(Ordering::Relaxed) {
            app.request_exit(crate::keys::ExitCause::Signal);
        }
        if app.state.should_exit {
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
            _ => has_parsed_event()?,
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
            // crossterm's own loop — the one that spins on `Ok(0)` forever once
            // the terminal hangs up — so if the terminal hangs up *after* our wait
            // said `Input` but before crossterm has an event, the spin is back
            // and the hang-up check below is unreachable. It needs the last
            // bytes on the wire to be a truncated sequence, and every other
            // path is checked, so this is narrower than the bug it replaces
            // rather than a gap left open by accident.
            let event = event::read()?;
            let mouse = matches!(event, Event::Mouse(_));
            app.handle_terminal_event(event);
            // An action that changes the layout retires its geometry. Redraw
            // before handling the remaining mouse input against a new frame.
            if mouse && !app.state.mouse.hits.is_valid() {
                break;
            }
            if app.state.should_exit
                || stop.load(Ordering::Relaxed)
                || handled >= MAX_EVENTS_PER_FRAME
                || started.elapsed() >= MAX_DRAIN
            {
                break;
            }
            // Check for a hangup before every re-entry into crossterm, which
            // is where a dead terminal spins. The check that follows is still
            // crossterm's, because it is the only thing that can see events
            // crossterm has already parsed but not handed over — they are no
            // longer on the wire for `wait` to find.
            if input.wait(Duration::ZERO)? == Ready::HungUp {
                return hung_up();
            }
            if !has_parsed_event()? {
                break;
            }
        }
    }
}

/// Whether `event::read` would return without blocking: crossterm holds an
/// event it has parsed but not handed over, or the terminal has bytes.
///
/// This cannot be `event::poll(Duration::ZERO)`. crossterm 0.29's
/// `use-dev-tty` source (`src/event/source/unix/tty.rs`, `try_read`) only
/// looks at its parser inside `while timeout.leftover() is not zero`, so a
/// zero timeout skips the loop and reports nothing. Its parser holds every
/// event from the last read, so a burst read in one go — `hello` from
/// `tmux send-keys -l`, `jj` over SSH or `orch web` — handed over its first
/// key, and the rest stayed parsed and unreported until new bytes arrived:
/// the screen showed `h` until the operator pressed something else.
///
/// Any non-zero timeout enters the loop, whose first step returns a parsed
/// event. With nothing parsed it waits up to [`PARSED_EVENT_PROBE`] on the
/// terminal, which bounds what this costs a frame. On a hung-up terminal that
/// source spins for the timeout and returns `false` (see `crate::tty`), so the
/// callers still check for a hang-up first. Residual: a thread descheduled
/// for longer than the probe between crossterm's start of the timeout and that
/// first check would miss the parsed event, as every check did before.
fn has_parsed_event() -> io::Result<bool> {
    event::poll(PARSED_EVENT_PROBE)
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
    /// Whether bracketed paste was enabled, so `Drop` disables exactly what
    /// was turned on. Best-effort like the mouse: a terminal that ignores the
    /// mode must not fail launch.
    bracketed_paste: bool,
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
            bracketed_paste: false,
            mouse: false,
        };
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        // `Ctrl+Enter` is only distinguishable from `Enter` under the Kitty
        // keyboard protocol; the wizard keeps its `Tab` confirm fallback for
        // terminals without it.
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
        // Bracketed paste, so a pasted body arrives as one `Event::Paste`
        // rather than as keys. Without it the first pasted newline is an
        // `Enter` — which submits a real message — and the remaining
        // characters land in normal mode, where `q` quits and `d` on the
        // Agents tab deletes an agent (design-v2 §3.9).
        if execute!(stdout, EnableBracketedPaste).is_ok() {
            guard.bracketed_paste = true;
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
        if self.bracketed_paste {
            let _ = execute!(stdout, DisableBracketedPaste);
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

    use crate::backend::{FakeBackend, Read, make_agent, make_goal, make_task};
    use crate::state::{InputMode, ViewId};
    use orch_domain::{AgentStatus, GoalStatus, TaskStatus};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn keyword(app: &mut App<FakeBackend>, view: ViewId, text: &str) {
        app.state.view = view;
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        app.handle_paste(text);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    fn filtered_backend(count: usize) -> FakeBackend {
        let mut backend = FakeBackend::new()
            .with_tasks(
                (0..count)
                    .map(|i| make_task(&format!("t_{i}"), "Élan 🚀", TaskStatus::Todo))
                    .collect(),
            )
            .with_goals(
                (0..count)
                    .map(|i| make_goal(&format!("g_{i}"), "東京", GoalStatus::Active))
                    .collect(),
            )
            .with_agents(
                (0..count)
                    .map(|i| make_agent(&format!("a_{i}"), "مرحبا", AgentStatus::Idle))
                    .collect(),
            );
        backend.can_create_task = false;
        backend
    }

    #[test]
    fn reconnected_dashboard_discards_keys_queued_before_it_is_ready() {
        struct QueuedInput(std::collections::VecDeque<Event>);
        impl DashboardDriver for QueuedInput {
            fn discard_pending(&mut self) -> io::Result<()> {
                discard_pending_events(|| Ok(self.0.pop_front()))
            }
            fn draw<B: TuiBackend>(mut self, app: &mut App<B>, _: &AtomicBool) -> io::Result<Exit> {
                while let Some(event) = self.0.pop_front() {
                    app.handle_terminal_event(event);
                }
                assert_eq!(app.state.view, ViewId::Tasks, "queued view key was drained");
                assert_eq!(app.state.task_selected, 0, "queued move key was drained");
                app.state.flush_messages(app.now());
                // A run key emits Running/Dispatched on success or Failed on
                // refusal; none belongs to a reconnect.
                assert!(
                    !app.state
                        .messages
                        .iter()
                        .any(|m| m.text.contains("Running ")
                            || m.text.contains("Dispatched")
                            || m.text.contains("Failed:")),
                    "queued retry keys emitted a task action or error"
                );
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('j'),
                    KeyModifiers::NONE,
                )));
                assert_eq!(app.state.task_selected, 1, "input after readiness acts");
                Ok(Exit::Requested)
            }
        }
        for count in [1, 2, 10, 130] {
            let mut pending = std::collections::VecDeque::from(vec![
                Event::Key(KeyEvent::new(
                    KeyCode::Char('r'),
                    KeyModifiers::NONE
                ));
                count
            ]);
            // The retry guard can retire r independently; these prove the drain.
            pending.push_back(Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            )));
            pending.push_back(Event::Key(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
            )));
            run_until_with_view_intent(
                filtered_backend(2),
                &AtomicBool::new(false),
                &mut ViewIntent::default(),
                true,
                QueuedInput(pending),
            )
            .unwrap();
        }
    }

    #[test]
    fn first_start_keeps_pending_input() {
        struct InitialInput;
        impl DashboardDriver for InitialInput {
            fn discard_pending(&mut self) -> io::Result<()> {
                panic!("a first dashboard start must not discard input");
            }
            fn draw<B: TuiBackend>(self, app: &mut App<B>, _: &AtomicBool) -> io::Result<Exit> {
                app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
                assert_eq!(app.state.task_selected, 1);
                Ok(Exit::Requested)
            }
        }
        run_until_with_view_intent(
            filtered_backend(2),
            &AtomicBool::new(false),
            &mut ViewIntent::default(),
            false,
            InitialInput,
        )
        .unwrap();
    }

    #[test]
    fn retry_handoff_retains_keywords_and_resets_selection_on_fresh_rows() {
        let mut filters = KeywordFilters::default();
        with_keyword_filters(filtered_backend(3), &mut filters, |app| {
            keyword(app, ViewId::Tasks, "Élan 🚀");
            keyword(app, ViewId::Goals, "東京");
            keyword(app, ViewId::Agents, "مرحبا");
            app.state.task_selected = 2;
            app.state.goal_selected = 2;
            app.state.agent_selected = 2;
            app.state.command_bar.input.reset("/task pending draft");
            app.request_exit(crate::keys::ExitCause::Signal);
        });
        for count in [1, 0, 2] {
            let mut backend = filtered_backend(count);
            // Live data no longer includes the former matching rows. Unrelated
            // rows stay in the total but must not enter the filtered selection.
            backend
                .tasks
                .push(make_task("other", "Other", TaskStatus::Todo));
            backend
                .goals
                .push(make_goal("other", "Other", GoalStatus::Active));
            backend
                .agents
                .push(make_agent("other", "Other", AgentStatus::Idle));
            with_keyword_filters(backend, &mut filters, |app| {
                for (view, text) in [
                    (ViewId::Tasks, "Élan 🚀"),
                    (ViewId::Goals, "東京"),
                    (ViewId::Agents, "مرحبا"),
                ] {
                    assert_eq!(
                        app.state.keyword(view),
                        text,
                        "retained keyword after retry"
                    );
                    assert_eq!(app.state.keyword_counts(view), (count, count + 1));
                }
                assert_eq!(app.state.task_selected, 0);
                assert_eq!(app.state.goal_selected, 0);
                assert_eq!(app.state.agent_selected, 0);
                assert!(!app.state.should_exit);
                assert_eq!(app.state.input_mode, InputMode::None);
                assert!(app.state.command_bar.input.value().is_empty());
                assert!(app.armed_broadcast.is_none());
                for read in [Read::Tasks, Read::Goals, Read::Agents] {
                    assert!(app.backend.reads.contains(&read), "fresh {read:?} read");
                }
                for (width, height) in [(80, 24), (40, 14)] {
                    app.resize(width, height);
                    app.state.view = ViewId::Tasks;
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|frame| {
                            views::draw(frame, app);
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let rendered: String = buffer.content.iter().map(|c| c.symbol()).collect();
                    assert!(rendered.contains("filter:"), "{rendered}");
                    assert!(
                        rendered.contains(&format!("{count}/{}", count + 1)),
                        "{rendered}"
                    );
                    assert_eq!(app.state.task_selected, 0);
                }
            });
        }
    }

    #[test]
    fn retry_handoff_keeps_empty_and_no_match_keywords() {
        for texts in [["", "", ""], ["missing 🚀", "不存在", "لا تطابق"]] {
            let views = [ViewId::Tasks, ViewId::Goals, ViewId::Agents];
            let mut filters = KeywordFilters::default();
            with_keyword_filters(filtered_backend(1), &mut filters, |app| {
                for (view, text) in views.into_iter().zip(texts) {
                    keyword(app, view, text);
                }
            });
            for _ in 0..3 {
                with_keyword_filters(filtered_backend(1), &mut filters, |app| {
                    for (view, text) in views.into_iter().zip(texts) {
                        assert_eq!(app.state.keyword(view), text);
                        assert_eq!(
                            app.state.keyword_counts(view),
                            (usize::from(text.is_empty()), 1)
                        );
                    }
                });
            }
        }
    }

    #[test]
    fn quit_and_a_new_dashboard_reset_keywords() {
        let mut filters = KeywordFilters::default();
        with_keyword_filters(filtered_backend(1), &mut filters, |app| {
            keyword(app, ViewId::Tasks, "Élan 🚀");
            keyword(app, ViewId::Goals, "東京");
            keyword(app, ViewId::Agents, "مرحبا");
            app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
            assert!(app.state.should_exit);
        });
        assert_ne!(filters, KeywordFilters::default());
        // The legacy runner/new dashboard owns a new default value.
        with_keyword_filters(filtered_backend(1), &mut KeywordFilters::default(), |app| {
            for view in [ViewId::Tasks, ViewId::Goals, ViewId::Agents] {
                assert_eq!(app.state.keyword(view), "");
                assert_eq!(app.state.keyword_counts(view), (1, 1));
            }
        });
    }

    /// The mode set is the whole of §3.5's capture policy, and it is the one
    /// part of this module that can be tested without a terminal — which also
    /// covers the restore *ordering* requirement.
    #[test]
    fn the_enable_set_is_exactly_normal_button_motion_and_sgr() {
        assert_eq!(ENABLE_MOUSE, "\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        for unwanted in ["1003", "1015"] {
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
        assert_eq!(DISABLE_MOUSE, "\x1b[?1006l\x1b[?1002l\x1b[?1000l");
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
    fn capture_writer_emits_exact_enable_and_reverse_disable_modes() {
        let mut output = Vec::new();
        write_mouse_capture(&mut output, true).unwrap();
        write_mouse_capture(&mut output, false).unwrap();
        assert_eq!(output, format!("{ENABLE_MOUSE}{DISABLE_MOUSE}").as_bytes());
    }

    #[derive(Default)]
    struct FailingWriter {
        bytes: Vec<u8>,
        fail_after: Option<usize>,
        fail_flush: bool,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(left) = self.fail_after {
                if left == 0 {
                    self.fail_after = None;
                    return Err(io::Error::other("injected capture write failure"));
                }
                let count = left.min(buf.len());
                self.bytes.extend_from_slice(&buf[..count]);
                self.fail_after = Some(left - count);
                return Ok(count);
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if std::mem::take(&mut self.fail_flush) {
                Err(io::Error::other("injected capture flush failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn partial_enable_failure_disables_every_mode_before_returning_error() {
        for fail_after in 0..ENABLE_MOUSE.len() {
            let mut output = FailingWriter {
                fail_after: Some(fail_after),
                ..Default::default()
            };
            let error = write_mouse_capture(&mut output, true).unwrap_err();
            assert_eq!(error.to_string(), "injected capture write failure");
            let mut expected = ENABLE_MOUSE.as_bytes()[..fail_after].to_vec();
            expected.extend_from_slice(DISABLE_MOUSE.as_bytes());
            assert_eq!(output.bytes, expected);
        }
    }

    #[test]
    fn enable_flush_failure_still_tears_down_all_mouse_modes() {
        let mut output = FailingWriter {
            fail_flush: true,
            ..Default::default()
        };
        let error = write_mouse_capture(&mut output, true).unwrap_err();
        assert_eq!(error.to_string(), "injected capture flush failure");
        assert_eq!(
            output.bytes,
            format!("{ENABLE_MOUSE}{DISABLE_MOUSE}").as_bytes()
        );
    }

    #[test]
    fn disable_failure_never_reenables_capture() {
        let mut output = FailingWriter {
            fail_after: Some(0),
            ..Default::default()
        };
        assert!(write_mouse_capture(&mut output, false).is_err());
        assert!(output.bytes.is_empty());
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
