//! End to end in tmux: every key of a burst is handled without a further
//! keystroke (AGENTS.md, "TUI end-to-end tests").
//!
//! `tmux send-keys -l` writes its text to the pane in one go, so the dashboard
//! reads the whole burst in one `read(2)` — the same thing a paste without
//! bracketed paste, an SSH link or `orch web` produces. crossterm parses it all
//! at once and hands the keys over one by one, and the keys after the first
//! used to sit inside crossterm until new bytes arrived: the dashboard showed
//! `h` for `hello`, or moved one row for `jj`, until the operator pressed
//! something else. See `has_parsed_event` in `crates/orch-tui/src/runner.rs`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Long enough for a debug build's first frame on a loaded CI runner. A burst
/// that is not handled never is, so the deadline only decides how long a
/// failure takes to report.
const DEADLINE: Duration = Duration::from_secs(20);

fn orch_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("orch")
}

/// tmux from `PATH`. Missing tmux fails the test: an end-to-end check that
/// silently passes where it cannot run proves nothing.
fn tmux_binary() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("tmux"))
        .find(|candidate| candidate.is_file())
        .expect(
            "tmux is required for the TUI end-to-end tests and was not found on PATH \
             (macOS: brew install tmux; Linux: the distro's tmux package)",
        )
}

/// A private tmux server, killed on drop so a failing assertion cannot leave
/// it (and the dashboard in it) running.
struct Tmux {
    bin: PathBuf,
    socket: String,
    home: PathBuf,
    /// The server's socket file, which `kill-server` leaves behind.
    socket_path: Option<PathBuf>,
}

impl Tmux {
    /// `-u`: the screen holds `▸`, `●` and box rules, and a cleared
    /// environment has no UTF-8 locale for tmux to infer it from. The cleared
    /// client environment matters too: the pane inherits the launching
    /// client's `PATH`, not the `-e PATH` given to `new-session`.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .args(["-u", "-L", &self.socket, "-f", "/dev/null"]);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("tmux runs");
        assert!(
            output.status.success(),
            "tmux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-p", "-t", "dash"]).stdout)
            .into_owned()
    }

    /// Poll the screen until `ready` holds, with a bounded deadline. Polling
    /// only reads the pane; it never writes to the dashboard's terminal, so it
    /// cannot be the "further keystroke" that releases a stuck burst.
    fn wait_for(&self, what: &str, ready: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let screen = self.screen();
            if ready(&screen) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; screen:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "dash"];
        args.extend_from_slice(keys);
        self.run(&args);
    }

    /// One write of `text` to the pane: the burst under test.
    fn burst(&self, text: &str) {
        self.run(&["send-keys", "-t", "dash", "-l", text]);
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self
            .command()
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(path) = &self.socket_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn global_config(dir: &Path) -> PathBuf {
    dir.join("home").join("global.yml")
}

fn orch(dir: &Path, args: &[&str]) {
    let output = Command::new(orch_binary())
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("HOME", dir.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", global_config(dir))
        .env("NO_COLOR", "1")
        .output()
        .expect("orch runs");
    assert!(
        output.status.success(),
        "orch {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A shell project (no default AI agent) with three shell agents, so the
/// wizard's Assignee step lists Auto-assign, alpha, bravo, charlie. There are
/// no tasks, so the orchestrator the dashboard starts has nothing to run.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    orch(
        dir.path(),
        &["init", "--name", "proj", "--adapter", "shell"],
    );
    for name in ["alpha", "bravo", "charlie"] {
        orch(
            dir.path(),
            &[
                "agent",
                "add",
                name,
                "--adapter",
                "shell",
                "--command",
                "true",
            ],
        );
    }
    dir
}

fn launch(dir: &Path, socket: String) -> Tmux {
    let mut tmux = Tmux {
        bin: tmux_binary(),
        socket,
        home: dir.join("home"),
        socket_path: None,
    };
    let home = format!("HOME={}", dir.join("home").display());
    let global = format!(
        "ORCHESTRY_GLOBAL_CONFIG_PATH={}",
        global_config(dir).display()
    );
    let cwd = dir.display().to_string();
    let command = format!("'{}' tui", orch_binary().display());
    // The adapters' model probes find no AI CLI on this PATH.
    tmux.run(&[
        "start-server",
        ";",
        "set-option",
        "-g",
        "default-terminal",
        "tmux-256color",
        ";",
        "new-session",
        "-d",
        "-s",
        "dash",
        "-x",
        "120",
        "-y",
        "40",
        "-c",
        &cwd,
        "-e",
        &home,
        "-e",
        &global,
        "-e",
        "PATH=/usr/bin:/bin",
        &command,
    ]);
    let path = tmux.run(&["display-message", "-p", "-t", "dash", "#{socket_path}"]);
    tmux.socket_path = Some(PathBuf::from(
        String::from_utf8_lossy(&path.stdout).trim().to_string(),
    ));
    tmux
}

/// The row the Select step's `▸` cursor is on.
fn cursor_row(screen: &str) -> Option<&str> {
    screen
        .lines()
        .find(|line| line.trim_start().starts_with('▸'))
}

#[test]
fn every_key_of_a_burst_is_handled_without_another_keystroke() {
    let dir = project();
    let tmux = launch(
        dir.path(),
        format!("orch-burst-{}-{}", std::process::id(), line!()),
    );
    tmux.wait_for("the dashboard", |s| s.contains("ORCH"));
    tmux.keys(&["t"]);
    tmux.wait_for("the Tasks view", |s| s.contains("NO TASKS"));
    tmux.keys(&["n"]);
    tmux.wait_for("the title step", |s| s.contains("Task title"));

    // Text step: five characters in one write land as five characters.
    tmux.burst("hello");
    tmux.wait_for("the whole title", |s| s.contains("> hello"));

    // Select step: Enter twice (title, then the default priority) reaches the
    // Assignee step with the cursor on Auto-assign; `jj` moves it two rows.
    tmux.keys(&["Enter"]);
    tmux.wait_for("the priority step", |s| s.contains("P3 Medium"));
    tmux.keys(&["Enter"]);
    tmux.wait_for("the assignee step", |s| {
        cursor_row(s).is_some_and(|row| row.contains("Auto-assign"))
    });
    tmux.burst("jj");
    tmux.wait_for("the cursor two rows down", |s| {
        cursor_row(s).is_some_and(|row| row.contains("bravo"))
    });
}
