//! Waiting for terminal input, with the hang-up detection crossterm does not
//! do (PARITY.md §14.39).
//!
//! crossterm 0.29 compiles one of two Unix event sources, and which one it is
//! decides the shape of this bug. This module was written against the default,
//! with the `use-dev-tty` feature **off**: the source is
//! `src/event/source/unix/mio.rs`, and its `TTY_TOKEN` arm is:
//!
//! ```text
//! loop {                                       // mio.rs:93
//!     match self.tty_fd.read(&mut buf) {
//!         Ok(n) => if n > 0 { parser.advance(…) }   // n == 0: nothing, no break
//!         Err(e) => if WouldBlock { break } else if Interrupted { continue }
//!                   // any other error: nothing, no break
//!     }
//!     if let Some(ev) = self.parser.next() { return Ok(Some(ev)) }
//! }                                            // → read again, forever
//! ```
//!
//! On a hung-up pty the fd is readable forever and every read is `Ok(0)` or a
//! latched error, so neither arm breaks and `try_read` **never returns at
//! all**. `event::poll` therefore never returns either, and the draw loop is
//! stuck inside it: it cannot reach its own stop flag, redraw, or notice
//! anything. Measured under `pty.fork()`: thread `orch-tui` at 101 % of a
//! core, `syscall=running`, the process re-parented to init with
//! `.orchestry/orchestry.lock` still held and its agents orphaned.
//!
//! The consequence for the fix is the load-bearing part: **checking for a
//! hang-up once per loop iteration is not enough**, because there is no next
//! iteration. The blocking wait itself has to be ours. So this module is a
//! `poll(2)` that reports `POLLHUP`, and crossterm is entered only once the
//! terminal has data and has not hung up. Reading and parsing stay
//! crossterm's, which is what ARCHITECTURE §4.8 protects; `nix` is the
//! sanctioned route to real POSIX semantics here (§4.8 keeps it for
//! `kill(pid, 0)` liveness, the same class of check).
//!
//! The build now turns `use-dev-tty` **on** (workspace `Cargo.toml`), because
//! the mio source also leaves a key unread when it arrives in the same wakeup
//! as SIGWINCH. On a hung-up tty that `poll(2)` source spins for the requested
//! timeout and returns `Ok(false)` — indistinguishable from idle — so owning
//! the wait is still what makes a hang-up visible. This module is correct
//! under either source.

use std::io;
use std::time::Duration;

/// What one wait found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ready {
    /// Input is waiting, so `event::read` has something to parse.
    Input,
    /// The timeout expired. Draw a frame and wait again.
    Idle,
    /// The terminal is gone and nothing will ever be readable again. The
    /// caller must leave; it must not ask crossterm anything else.
    HungUp,
}

#[cfg(unix)]
pub(crate) use unix::TtyInput;

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{self, IsTerminal};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::time::Duration;

    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

    use super::Ready;

    /// The terminal the draw loop waits on.
    pub(crate) struct TtyInput {
        source: Source,
        /// Cleared once `poll(2)` reports `POLLNVAL` **on `/dev/tty`** — that
        /// it cannot watch that file, which is documented behaviour on macOS.
        /// From then on the wait is crossterm's again and a hang-up goes
        /// undetected: the behaviour this module replaces, not a new way to
        /// fail. See [`TtyInput::wait`] for why the same report on *stdin* is
        /// treated as a hang-up instead.
        pollable: bool,
    }

    enum Source {
        /// crossterm reads stdin when it is a terminal and opens `/dev/tty`
        /// otherwise (its `tty_fd`), so we watch whichever file it will read,
        /// or we would report readiness for the wrong one.
        Stdin,
        DevTty(File),
        /// Not a terminal and `/dev/tty` would not open: there is no
        /// controlling terminal to wait on, which §3.9 counts as hung up.
        Gone,
    }

    impl TtyInput {
        pub(crate) fn open() -> Self {
            let source = if io::stdin().is_terminal() {
                Source::Stdin
            } else {
                match File::options().read(true).write(true).open("/dev/tty") {
                    Ok(file) => Source::DevTty(file),
                    Err(err) => {
                        tracing::warn!(%err, "no controlling terminal");
                        Source::Gone
                    }
                }
            };
            TtyInput {
                source,
                pollable: true,
            }
        }

        pub(crate) fn wait(&mut self, timeout: Duration) -> io::Result<Ready> {
            let stdin = io::stdin();
            let fd: BorrowedFd<'_> = match &self.source {
                Source::Stdin => stdin.as_fd(),
                Source::DevTty(file) => file.as_fd(),
                Source::Gone => return Ok(Ready::HungUp),
            };
            if !self.pollable {
                return super::crossterm_wait(timeout);
            }
            // `ANIM_TICK_MS` is well inside `PollTimeout`'s range; a value
            // that is not falls back to blocking, which the caller's own
            // hang-up handling still escapes.
            let timeout_ms = PollTimeout::try_from(timeout).unwrap_or(PollTimeout::NONE);
            let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
            match poll(&mut fds, timeout_ms) {
                Ok(0) => Ok(Ready::Idle),
                Ok(_) => {
                    let revents = fds[0].revents().unwrap_or(PollFlags::empty());
                    // `POLLNVAL` means "this fd is not open", and what that
                    // implies depends on *which* fd it is:
                    //
                    // * On **stdin** — the fd used whenever stdin is a tty,
                    //   which is every terminal, every tmux pane and
                    //   `orch web`'s pty child — a closed stdin is a terminal
                    //   that is gone. Treat it as a hang-up.
                    // * On **`/dev/tty`** — opened only when stdin is *not* a
                    //   tty — it may instead be the documented macOS
                    //   limitation that `poll(2)` does not work on that file.
                    //   Falling back costs only the detection this module
                    //   adds; exiting would be wrong.
                    //
                    // The split matters because the common macOS path is
                    // stdin, so it keeps dead-tty detection there rather than
                    // making every Mac session take the fallback.
                    if revents.contains(PollFlags::POLLNVAL) {
                        if matches!(self.source, Source::DevTty(_)) {
                            self.pollable = false;
                            return super::crossterm_wait(timeout);
                        }
                        return Ok(Ready::HungUp);
                    }
                    Ok(classify(revents))
                }
                // A signal — SIGWINCH, most often — cuts `poll` short however
                // the handler was installed, because `poll` is never
                // restarted. Report it as a timeout: the caller redraws,
                // which is what it would have done for the resize anyway.
                Err(nix::errno::Errno::EINTR) => Ok(Ready::Idle),
                Err(err) => Err(io::Error::from(err)),
            }
        }
    }

    /// `POLLHUP` on a pty slave is the master being closed; `POLLERR` is an fd
    /// that will not work again. `poll(2)` reports both in `revents` whether
    /// or not they were requested in `events`, which is why asking for
    /// `POLLIN` alone is enough.
    ///
    /// Bytes may still be buffered under either, but they were typed at a
    /// terminal that is already gone — and reading them is exactly what
    /// spins, so a hang-up wins over pending input.
    fn classify(revents: PollFlags) -> Ready {
        if revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
            Ready::HungUp
        } else if revents.contains(PollFlags::POLLIN) {
            Ready::Input
        } else {
            Ready::Idle
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_hangup_wins_over_pending_input() {
            assert_eq!(
                classify(PollFlags::POLLIN | PollFlags::POLLHUP),
                Ready::HungUp,
                "buffered bytes from a terminal that is gone must not keep us \
                 in crossterm, which is where the spin is"
            );
            assert_eq!(classify(PollFlags::POLLHUP), Ready::HungUp);
            assert_eq!(classify(PollFlags::POLLERR), Ready::HungUp);
        }

        #[test]
        fn readable_is_input_and_nothing_is_idle() {
            assert_eq!(classify(PollFlags::POLLIN), Ready::Input);
            assert_eq!(classify(PollFlags::empty()), Ready::Idle);
        }

        #[test]
        fn the_animation_tick_survives_the_poll_timeout_conversion() {
            let tick = Duration::from_millis(crate::messages::ANIM_TICK_MS as u64);
            let converted = PollTimeout::try_from(tick).expect("a tick fits a poll timeout");
            assert_eq!(
                i32::from(converted),
                crate::messages::ANIM_TICK_MS as i32,
                "a tick that silently became PollTimeout::NONE would block the \
                 animation until a key arrived"
            );
        }

        /// `/dev/tty` is only ever opened when stdin is not a tty, so the
        /// macOS fallback cannot take a normal terminal, tmux pane or
        /// `orch web` session with it.
        #[test]
        fn the_polled_fd_is_stdin_whenever_stdin_is_a_terminal() {
            let input = TtyInput::open();
            if std::io::stdin().is_terminal() {
                assert!(matches!(input.source, Source::Stdin));
            } else {
                // Under `cargo test` stdin is not a terminal, so this is the
                // other branch: `/dev/tty` if the runner has a controlling
                // terminal, `Gone` if it does not.
                assert!(matches!(input.source, Source::DevTty(_) | Source::Gone));
            }
        }

        #[test]
        fn a_missing_controlling_terminal_reads_as_hung_up() {
            let mut input = TtyInput {
                source: Source::Gone,
                pollable: true,
            };
            assert_eq!(input.wait(Duration::ZERO).unwrap(), Ready::HungUp);
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct TtyInput;

#[cfg(not(unix))]
impl TtyInput {
    pub(crate) fn open() -> Self {
        TtyInput
    }

    /// The windows console source has neither the Unix source's loop nor a
    /// `POLLHUP` to look for, and it has its own error path, so the wait
    /// stays crossterm's.
    pub(crate) fn wait(&mut self, timeout: Duration) -> io::Result<Ready> {
        crossterm_wait(timeout)
    }
}

fn crossterm_wait(timeout: Duration) -> io::Result<Ready> {
    if ratatui::crossterm::event::poll(timeout)? {
        Ok(Ready::Input)
    } else {
        Ok(Ready::Idle)
    }
}
