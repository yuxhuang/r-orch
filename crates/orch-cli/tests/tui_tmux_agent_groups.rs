//! End to end in tmux: the Agents view's grouping (Team, Node, Status, ...),
//! switched by `b`/`B`, the header's `GROUP:` chip and the footer's
//! `b <Mode>` hint, as an operator sees it in a real terminal (AGENTS.md,
//! "TUI end-to-end tests").
//!
//! The real `orch` binary runs `orch tui` in a private tmux server over a
//! temp project with two local agents and one remote alias, `w1`, whose
//! cached status lists one agent. Nothing listens on `w1`'s address, so the
//! node is offline — exactly what its section header must say.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Long enough for a debug build's first frame on a loaded CI runner.
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
    /// `-u`: the screen holds `◈`, `●` and box rules, and a cleared
    /// environment has no UTF-8 locale for tmux to infer it from.
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

    /// Poll the screen until `ready` holds, with a bounded deadline.
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

    /// An SGR left click (press and release) at a 0-based cell.
    fn click(&self, x: usize, y: usize) {
        for end in ['M', 'm'] {
            let sequence = format!("\x1b[<0;{};{}{end}", x + 1, y + 1);
            let hex: Vec<String> = sequence.bytes().map(|b| format!("{b:02x}")).collect();
            let mut args = vec!["send-keys", "-t", "dash", "-H"];
            args.extend(hex.iter().map(String::as_str));
            self.run(&args);
        }
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

fn global_config(dir: &Path) -> PathBuf {
    dir.join("home").join("global.yml")
}

/// Two local agents (the default one `init` creates, and `local-bot`) and a
/// registered remote `w1` whose cached status lists `rover`.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    orch(
        dir.path(),
        &["init", "--name", "proj", "--adapter", "claude"],
    );
    orch(
        dir.path(),
        &[
            "agent",
            "add",
            "local-bot",
            "--adapter",
            "shell",
            "--command",
            "true",
        ],
    );
    let root = dir.path().join(".orchestry");
    std::fs::write(
        root.join("remotes.yml"),
        "- alias: w1\n  addr: 127.0.0.1:9\n  added_at: '2026-10-18T00:00:00Z'\n",
    )
    .expect("remotes.yml");
    std::fs::create_dir_all(root.join("remotes")).expect("remotes dir");
    std::fs::write(
        root.join("remotes").join("w1.json"),
        r#"{"alias":"w1","addr":"127.0.0.1:9","online":false,"last_seen":null,"last_error":null,"peer_name":null,"instance_id":null,"os":null,"agents":[{"id":"rover","name":"rover","adapter":"claude","status":"idle"}]}"#,
    )
    .expect("w1 status");
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

/// Whether every needle is on the screen. A wait predicate names every string
/// the assertions after it read: ratatui writes a frame as a diff, top to
/// bottom, so a capture can catch the section header painted and the row under
/// it not yet, and a weaker predicate hands that half frame to the asserts.
fn shows_all(screen: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| screen.contains(needle))
}

/// The 0-based (column, row) of `needle` on a captured screen, in cells.
/// Every glyph left of the chips is one cell wide.
fn locate(screen: &str, needle: &str) -> Option<(usize, usize)> {
    screen.lines().enumerate().find_map(|(row, line)| {
        line.find(needle)
            .map(|byte| (line[..byte].chars().count(), row))
    })
}

#[test]
fn agents_group_by_node_shows_local_then_remote_sections() {
    let dir = project();
    let tmux = launch(
        dir.path(),
        format!("orch-y1-{}-{}", std::process::id(), line!()),
    );
    tmux.wait_for("the dashboard", |s| s.contains("ORCH"));
    tmux.keys(&["a"]);

    // By team (the default): no teams, so one unassigned section. At 120
    // columns the header ends with the group chip, and the footer leads with
    // the mode too.
    let screen = tmux.wait_for("the Agents view By team", |s| {
        shows_all(
            s,
            &["GROUP: Team", "b Team", "rover@w1", "UNASSIGNED · 3 agents"],
        )
    });
    assert!(screen.contains("STATUS↑"), "{screen}");
    assert!(!screen.contains("◈ LOCAL"), "{screen}");
    assert!(!screen.contains("b group:"), "the old chip row:\n{screen}");

    // `b`: By node — Local first, then w1, marked offline.
    tmux.keys(&["b"]);
    let screen = tmux.wait_for("the By node sections", |s| {
        shows_all(
            s,
            &[
                "GROUP: Node",
                "b Node",
                "◈ LOCAL · 2 agents",
                "◈ w1 ○ offline · 1 agent",
                "rover@w1",
                "local-bot",
            ],
        )
    });
    let local = locate(&screen, "◈ LOCAL · 2 agents").expect(&screen);
    let remote = locate(&screen, "◈ w1 ○ offline · 1 agent").expect(&screen);
    let rover = locate(&screen, "rover@w1").expect(&screen);
    let bot = locate(&screen, "local-bot").expect(&screen);
    assert!(
        local.1 < bot.1 && bot.1 < remote.1 && remote.1 < rover.1,
        "sections out of order:\n{screen}"
    );
    assert!(!screen.contains("UNASSIGNED"), "{screen}");

    // The chip is a mouse path to the next mode: By status. The remote's
    // agent reports offline with its node.
    let (x, y) = locate(&screen, "GROUP: Node").expect(&screen);
    tmux.click(x, y);
    let screen = tmux.wait_for("By status", |s| {
        shows_all(
            s,
            &["GROUP: Status", "◈ IDLE · 2 agents", "◈ OFFLINE · 1 agent"],
        )
    });
    let idle = locate(&screen, "◈ IDLE").expect(&screen);
    let offline = locate(&screen, "◈ OFFLINE").expect(&screen);
    assert!(idle.1 < offline.1, "{screen}");
    assert!(!screen.contains("◈ LOCAL"), "{screen}");

    // `B` walks back: Node, then Team.
    tmux.keys(&["B", "B"]);
    tmux.wait_for("By team again", |s| {
        shows_all(s, &["GROUP: Team", "UNASSIGNED · 3 agents"])
    });

    // At 80 columns there is no chip; the footer's hint names the mode and
    // is the mouse path.
    tmux.run(&["resize-window", "-t", "dash", "-x", "80", "-y", "24"]);
    let screen = tmux.wait_for("the 80-column table", |s| {
        shows_all(s, &["b Team", "UNASSIGNED · 3 agents"]) && !s.contains("GROUP:")
    });
    let (x, y) = locate(&screen, "b Team").expect(&screen);
    tmux.click(x, y);
    tmux.wait_for("By node from the footer", |s| {
        shows_all(s, &["b Node", "◈ LOCAL · 2 agents"])
    });

    // At the minimum width the hint and the section headers still fit.
    tmux.run(&["resize-window", "-t", "dash", "-x", "40", "-y", "20"]);
    let screen = tmux.wait_for("the 40-column table", |s| {
        shows_all(
            s,
            &["b Node", "◈ LOCAL · 2 agents", "◈ w1 ○ offline · 1 agent"],
        )
    });
    assert!(!screen.contains("GROUP:"), "{screen}");

    tmux.keys(&["q"]);
}
