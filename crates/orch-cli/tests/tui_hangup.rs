//! `orch tui` must leave when its terminal does.
//!
//! crossterm's unix event source answers `poll`/`read` by looping on the tty
//! fd, and a read of 0 bytes on a hung-up pty neither produces an event nor
//! breaks that loop. So a closed pty master used to leave `orch tui` alive at
//! 100 % of a core, re-parented to init, with `.orchestry/orchestry.lock`
//! still held and its agents orphaned — the SIGHUP the kernel sent having
//! been noted into a flag the draw loop never reached again. See
//! `crates/orch-tui/src/tty.rs`.
//!
//! Only a real pty shows this: `TestBackend` never goes near crossterm's
//! event source, and a pipe has no hangup semantics to get wrong. So this is
//! a binary-level test, like `watch_shutdown.rs`, for the same reason.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

/// Long enough that a slow debug build on a loaded machine is not flaky, short
/// enough that only "it never comes back" fails it. The fix exits in ~0.2 s.
const EXIT_BUDGET: Duration = Duration::from_secs(10);

/// With no agents running there is nothing for the wind-down to wait for, so
/// the exit is the orderly sequence and nothing else. Bounds that, separately
/// from `EXIT_BUDGET`'s "did it come back at all" — `DEAD_TTY_WIND_DOWN` in
/// `tui/command.rs` is 5 s, so a regression that waits out the whole grace
/// with nothing to stop fails here.
const NO_AGENT_EXIT_BUDGET: Duration = Duration::from_secs(2);

/// Time spent not reading the pty, so the dashboard finishes its frame and
/// parks in its input wait. It redraws every animation tick (~100 ms) and the
/// pty's own buffer absorbs that many frames easily, so nothing blocks.
const SETTLE: Duration = Duration::from_millis(500);

fn orch_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("orch")
}

/// A project with its own `HOME`, so nothing here reads or writes the real
/// `~/.orchestry`.
fn initialized_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    let status = Command::new(orch_binary())
        .args(["init", "--name", "proj", "--adapter", "claude"])
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .status()
        .expect("init runs");
    assert!(status.success(), "init failed");
    dir
}

fn lock_path(dir: &Path) -> PathBuf {
    dir.join(".orchestry").join("orchestry.lock")
}

struct Dashboard {
    child: Box<dyn Child + Send + Sync>,
    /// Both master fds, held so the test can decide when the terminal goes
    /// away. `try_clone_reader` dups the master, so a hangup needs both.
    master: Option<Box<dyn MasterPty + Send>>,
    reader: Option<Box<dyn Read + Send>>,
}

/// Launch the dashboard on a 100×30 pty. The project has no tasks, so the
/// orchestrator it starts never runs an agent and no AI CLI is executed.
fn launch(dir: &Path, args: &[&str]) -> Dashboard {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");

    let mut command = CommandBuilder::new(orch_binary());
    command.args(args);
    command.cwd(dir);
    command.env("HOME", dir.join("home"));
    command.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(command).expect("spawn orch tui");
    // The child holds its own copy; keeping ours would mean the pty never
    // reports a hangup to it.
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .expect("clone the pty reader");
    Dashboard {
        child,
        master: Some(pair.master),
        reader: Some(reader),
    }
}

impl Dashboard {
    /// Drain the pty until the dashboard has drawn a frame, so that what the
    /// test does next lands on the event loop rather than on startup. Draining
    /// also keeps the pty from filling, which would block the child in
    /// `write` before it could act on anything.
    ///
    /// A child that dies instead of drawing closes the pty, which ends the
    /// read rather than hanging the test.
    fn screen_once_drawn(&mut self) -> String {
        let reader = self.reader.as_mut().expect("the reader is still open");
        let deadline = Instant::now() + EXIT_BUDGET;
        let mut seen = String::new();
        let mut buf = [0u8; 8192];
        while !seen.contains("ORCH") && Instant::now() < deadline {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        assert!(seen.contains("ORCH"), "the dashboard never drew:\n{seen}");
        seen
    }

    /// Close every master fd — what closing a terminal window does. With none
    /// left the child's writes fail rather than block, so nothing has to keep
    /// draining for it to get out.
    fn hang_up(&mut self) {
        self.reader.take();
        self.master.take();
    }

    /// Hand the draining to a thread, for the cases that leave the pty open:
    /// the child keeps drawing frames while it shuts down.
    fn drain_in_background(&mut self) {
        let mut reader = self.reader.take().expect("the reader is still open");
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
        });
    }

    fn wait_for_exit(&mut self, screen: &str) -> (Duration, ExitStatus) {
        let started = Instant::now();
        while started.elapsed() < EXIT_BUDGET {
            if let Some(status) = self.child.try_wait().expect("poll the child") {
                return (started.elapsed(), status);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        panic!(
            "orch tui outlived its terminal by {EXIT_BUDGET:?} and had to be killed; \
             last frame:\n{screen}"
        );
    }
}

#[test]
fn the_dashboard_exits_when_its_pty_master_closes() {
    let dir = initialized_project();
    let mut dashboard = launch(dir.path(), &["tui"]);
    let screen = dashboard.screen_once_drawn();

    // Not `--observe`: this process started the orchestrator, so it holds the
    // lock, and releasing it is the half of the exit that matters most.
    assert!(
        lock_path(dir.path()).exists(),
        "the dashboard should hold the lock while it runs; frame:\n{screen}"
    );

    // Let it settle into the wait before pulling the terminal out from under
    // it. That is the state the bug needed: a close that lands while the loop
    // is inside `event::poll` has no draw to fail and no flag anyone will look
    // at, so the loop spun there forever. A close that lands on the draw
    // instead merely failed the write, which the loop always noticed.
    std::thread::sleep(SETTLE);

    dashboard.hang_up();

    let (elapsed, status) = dashboard.wait_for_exit(&screen);
    assert!(
        status.success(),
        "a terminal going away is not a dashboard failure, got {status:?} after {elapsed:?}"
    );
    assert!(
        !lock_path(dir.path()).exists(),
        "the lock was still held after {elapsed:?}"
    );
    assert!(
        elapsed < NO_AGENT_EXIT_BUDGET,
        "nothing was running, so the wind-down had nothing to wait for, but the \
         exit took {elapsed:?}"
    );
}

#[test]
fn the_dashboard_exits_on_sighup_with_its_pty_still_open() {
    let dir = initialized_project();
    let mut dashboard = launch(dir.path(), &["tui"]);
    let screen = dashboard.screen_once_drawn();
    let pid = dashboard.child.process_id().expect("a pid");
    dashboard.drain_in_background();

    let killed = Command::new("kill")
        .args(["-HUP", &pid.to_string()])
        .status()
        .expect("kill runs");
    assert!(killed.success(), "could not signal {pid}");

    let (elapsed, status) = dashboard.wait_for_exit(&screen);
    assert!(status.success(), "got {status:?} after {elapsed:?}");
    assert!(
        !lock_path(dir.path()).exists(),
        "the lock was still held after {elapsed:?}"
    );
}
