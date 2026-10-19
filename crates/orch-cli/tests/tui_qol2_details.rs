//! End to end in tmux: detail views as an operator sees them in a real
//! terminal (AGENTS.md, "TUI end-to-end tests"): the section rail keeps its
//! order while focus moves (R2), and the task, goal and agent details' Activity
//! rows have the bottom feed's columns (R1, R12).
//!
//! The real `orch` binary runs `orch tui` in a private tmux server over a
//! temp project with one shell agent, `bot`, one unassigned goal, `Launch`,
//! and one cancelled task of that goal whose failed run left four events. The
//! project is initialised with the shell adapter, the goal has no assignee and
//! the task is terminal, so the dashboard has nothing it could dispatch to an
//! AI CLI.

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
    /// `-u`: the screen holds `▍`, `✎` and box rules, and a cleared
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

fn orch(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn global_config(dir: &Path) -> PathBuf {
    dir.join("home").join("global.yml")
}

fn json_id(stdout: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(stdout).expect("--json output");
    value["id"].as_str().expect("an id").to_owned()
}

/// `bot`, a goal `Launch`, its cancelled task `Ship it`, and a failed run of
/// the task (the dashboard
/// loads succeeded and failed runs into the feed) whose events are three and
/// a half hours old, so the time column reads a stable `3h`.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    orch(
        dir.path(),
        &["init", "--name", "proj", "--adapter", "shell"],
    );
    let agent = json_id(&orch(
        dir.path(),
        &[
            "--json",
            "agent",
            "add",
            "bot",
            "--adapter",
            "shell",
            "--command",
            "true",
        ],
    ));
    let goal = json_id(&orch(dir.path(), &["--json", "goal", "add", "Launch"]));
    let task = json_id(&orch(
        dir.path(),
        &["--json", "task", "add", "Ship it", "--goal-id", &goal],
    ));
    orch(dir.path(), &["task", "cancel", &task]);

    let at = |seconds: i64| {
        (jiff::Timestamp::now() - jiff::SignedDuration::from_mins(210)
            + jiff::SignedDuration::from_secs(seconds))
        .to_string()
    };
    let runs = dir.path().join(".orchestry").join("runs");
    std::fs::create_dir_all(&runs).expect("runs dir");
    let run = serde_json::json!({
        "id": "run_e2e1",
        "task_id": task,
        "agent_id": agent,
        "attempt": 1,
        "status": "failed",
        "started_at": at(0),
        "finished_at": at(60),
        "workspace_path": dir.path().display().to_string(),
        "prompt": "p",
    });
    std::fs::write(runs.join("run_e2e1.json"), run.to_string()).expect("run");
    let events: Vec<String> = [
        ("agent_output", "planning the change", 1),
        ("file_changed", "src/lib.rs", 2),
        ("error", "lint failed once", 3),
        ("done", "", 60),
    ]
    .into_iter()
    .map(|(kind, data, seconds)| {
        serde_json::json!({"timestamp": at(seconds), "type": kind, "data": data}).to_string()
    })
    .collect();
    std::fs::write(runs.join("run_e2e1.jsonl"), events.join("\n") + "\n").expect("events");
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

/// The detail rail: the row that names the Description section.
fn rail(screen: &str) -> &str {
    screen
        .lines()
        .find(|line| line.contains("Description │"))
        .unwrap_or_else(|| panic!("no detail rail on screen:\n{screen}"))
}

/// The rail's titles, in screen order, without the focus marker or the
/// Activity follow suffix.
fn titles(rail: &str) -> Vec<String> {
    rail.split('│')
        .map(|title| {
            let title = title.trim().trim_start_matches("> ");
            title.split(" · ").next().unwrap_or(title).to_owned()
        })
        .collect()
}

/// The title the rail marks as focused, if any.
fn marker(rail: &str) -> Option<String> {
    rail.split('│')
        .find_map(|title| title.trim().strip_prefix("> "))
        .map(|title| title.split(" · ").next().unwrap_or(title).to_owned())
}

#[test]
fn task_detail_rail_keeps_order_and_activity_rows_match_the_feed() {
    let dir = project();
    let tmux = launch(
        dir.path(),
        format!("orch-x2-{}-{}", std::process::id(), line!()),
    );
    tmux.wait_for("the dashboard", |s| s.contains("ORCH"));
    tmux.keys(&["t"]);
    tmux.wait_for("the task row", |s| s.contains("Ship it"));
    tmux.keys(&["Enter"]);
    // The Activity section joins the rail once the run history has loaded.
    let screen = tmux.wait_for("the Activity section", |s| {
        s.contains("Description │ Activity") || s.contains("> Activity")
    });
    let ring = ["Overview", "Description", "Activity"];
    assert_eq!(titles(rail(&screen)), ring, "{screen}");

    // Tab through the sections: the titles never move; only the marker does.
    let mut marked = marker(rail(&screen));
    while marked.as_deref() != Some("Activity") {
        tmux.keys(&["Tab"]);
        let screen = tmux.wait_for("focus to move", |s| {
            let now = s
                .lines()
                .find(|line| line.contains("Description │"))
                .and_then(marker);
            now.is_some() && now != marked
        });
        let line = rail(&screen);
        assert_eq!(titles(line), ring, "{screen}");
        assert_eq!(line.matches("> ").count(), 1, "{line}");
        marked = marker(line);
    }
    let screen = tmux.wait_for("the Activity section focused", |s| {
        s.contains("> Activity · following")
    });
    assert_eq!(
        rail(&screen).trim_end(),
        "Overview │ Description │ > Activity · following",
        "{screen}"
    );
    // Task Activity keeps the feed's chronology: its live edge is the bottom.
    assert_eq!(
        footer_hint(&screen),
        "↑↓ scroll; End follows; Up pauses; Tab panes; Enter expands; F1 help",
        "{screen}"
    );

    // Feed columns: accent, a 5-cell time, a 9-cell agent badge, the type
    // icon, the text. The badge and time show once per agent group, and
    // following selects the newest row.
    let rows: Vec<&str> = screen
        .lines()
        .skip_while(|line| !line.contains("> Activity"))
        .skip(1)
        .filter(|line| {
            line.contains(" ▶ ")
                || line.contains(" ✎ ")
                || line.contains(" ✕ ")
                || line.contains("planning")
        })
        .collect();
    assert_eq!(
        rows.iter().map(|r| r.trim_end()).collect::<Vec<_>>(),
        [
            format!("▍{:>5}{:<9}│ planning the change", "3h", "bot").as_str(),
            format!("▏{:14}✎ src/lib.rs", "").as_str(),
            format!("▏{:14}✕ lint failed once", "").as_str(),
            format!(">{:14}▶ Completed", "").as_str(),
        ],
        "{screen}"
    );

    // At the minimum width the rail drops the title farthest from focus and
    // keeps the rest in order.
    tmux.run(&["resize-window", "-t", "dash", "-x", "40", "-y", "14"]);
    let screen = tmux.wait_for("the 40-column rail", |s| {
        s.lines()
            .any(|line| line.trim_end() == "Description │ > Activity · following")
    });
    assert!(screen.contains("▶ Completed"), "{screen}");

    tmux.keys(&["q"]);
}

/// The feed-format rows under the focused Activity title, in screen order.
fn activity_rows(screen: &str) -> Vec<String> {
    screen
        .lines()
        .skip_while(|line| !line.contains("> Activity"))
        .skip(1)
        .filter(|line| {
            line.contains(" ▶ ")
                || line.contains(" ✎ ")
                || line.contains(" ✕ ")
                || line.contains("planning")
        })
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// The footer's follow hint for a newest-first detail Activity pane.
const NEWEST_FIRST_HINT: &str =
    "↑↓ scroll; Home follows; Down pauses; Tab panes; Enter expands; F1 help";

/// The footer's follow hint line, trimmed.
fn footer_hint(screen: &str) -> &str {
    screen
        .lines()
        .find(|line| line.contains("↑↓ scroll;"))
        .unwrap_or_else(|| panic!("no follow hint in the footer: {screen}"))
        .trim()
}

/// R12: goal and agent details end their rail with Activity, which lists the
/// same events in the feed's row format, newest first, following at the top;
/// moving down pauses, and the title and the footer name Home as the way back.
#[test]
fn goal_and_agent_detail_activity_rows_are_newest_first_in_feed_format() {
    let dir = project();
    let tmux = launch(
        dir.path(),
        format!("orch-x2-{}-{}", std::process::id(), line!()),
    );
    tmux.wait_for("the dashboard", |s| s.contains("ORCH"));
    // Newest first: one bot group, so the time and badge show on the top row,
    // which following selects.
    let newest_first = [
        format!(">{:>5}{:<9}▶ Completed", "3h", "bot"),
        format!("▏{:14}✕ lint failed once", ""),
        format!("▏{:14}✎ src/lib.rs", ""),
        format!("▏{:14}│ planning the change", ""),
    ];
    for (view, row) in [("a", "bot"), ("g", "Launch")] {
        tmux.keys(&[view]);
        tmux.wait_for(row, |s| s.contains(row));
        // Wait for the run history: the feed shows its newest event.
        tmux.wait_for("the feed", |s| s.contains("▶ Completed"));
        tmux.keys(&["Enter"]);
        tmux.wait_for("the detail", |s| s.contains("Overview"));
        // Tab through the detail sections to Activity, the ring's last entry.
        let mut tabs = 0;
        while !tmux.screen().contains("> Activity · following") {
            assert!(tabs < 8, "{view}: no Activity section: {}", tmux.screen());
            let before = tmux.screen();
            tmux.keys(&["Tab"]);
            tmux.wait_for("focus to move", |s| s != before);
            tabs += 1;
        }
        let screen = tmux.screen();
        let title = screen
            .lines()
            .find(|line| line.contains("> Activity · following"))
            .unwrap();
        assert!(
            title.trim_end().ends_with("│ > Activity · following"),
            "Activity ends the rail: {screen}"
        );
        assert_eq!(activity_rows(&screen), newest_first, "{view}: {screen}");
        // The footer names the same live edge as the pane title.
        assert_eq!(footer_hint(&screen), NEWEST_FIRST_HINT, "{view}: {screen}");

        tmux.keys(&["Down"]);
        let screen = tmux.wait_for("the paused title", |s| {
            s.contains("> Activity · paused · Home follows")
        });
        let rows = activity_rows(&screen);
        assert!(rows[1].starts_with('>'), "{view}: {screen}");
        assert_eq!(footer_hint(&screen), NEWEST_FIRST_HINT, "{view}: {screen}");
        tmux.keys(&["Home"]);
        tmux.wait_for("following again", |s| s.contains("> Activity · following"));
        tmux.keys(&["Escape"]);
        tmux.wait_for("the detail closed", |s| !s.contains("Activity · following"));
    }
    tmux.keys(&["q"]);
}
