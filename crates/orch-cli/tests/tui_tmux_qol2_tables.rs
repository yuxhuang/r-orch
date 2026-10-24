//! End to end in tmux: the list column headers, the sort keys and header
//! clicks, the Started/`Elapsed*` columns and the keyword filter (Ctrl+K), as
//! an operator sees them in a real terminal (AGENTS.md, "TUI end-to-end
//! tests").
//!
//! The real `orch` binary runs `orch tui --observe` in a private tmux server
//! over a temp project seeded on disk: two tasks, one goal and one run record.
//! Nothing here can start an agent — the project has no agents at all, the
//! adapter default is `shell`, `--observe` never starts the orchestrator, and
//! the session's `PATH` holds no AI CLI.
//!
//! Every timestamp is fixed, so the terminal task's `Elapsed*` is exactly the
//! hour between its first run and its last update, whenever the test runs.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Long enough for a debug build's first frame on a loaded CI runner.
const DEADLINE: Duration = Duration::from_secs(20);

/// The seeded run's start, and the hour to its task's last update.
const RUN_STARTED_AT: &str = "2024-01-01T00:00:00.000Z";
const TASK_UPDATED_AT: &str = "2024-01-01T01:00:00.000Z";
/// `TASK_UPDATED_AT - RUN_STARTED_AT`, as the Elapsed column renders it.
const FROZEN_ELAPSED: &str = "1h00m";

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
    /// `-u`: the screen holds `◆`, `↑↓` and box rules, and a cleared
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

    /// The keyword caret is a reversed blank after the typed value. Capture
    /// attributes as well as text so a missing caret cannot pass as a space.
    fn keyword_cursor_reversed(&self, keyword: &str) -> bool {
        let output = self.run(&["capture-pane", "-p", "-e", "-t", "dash"]);
        let screen = String::from_utf8_lossy(&output.stdout);
        let cursor = format!("{keyword}\x1b[7m ");
        screen
            .lines()
            .any(|line| line.contains("Filter tasks › ") && line.contains(&cursor))
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

/// A `shell`-adapter project with no agents, one finished task that has a run
/// record, one task that never started, and one goal.
///
/// The entities are written as files, the way the existing tmux test writes
/// its remote fixtures: these timestamps have to be exact, and `task add`
/// would stamp them with the wall clock. No `_index.json` exists yet, so the
/// stores rebuild their index from these files on the first read.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    orch(
        dir.path(),
        &["init", "--name", "proj", "--adapter", "shell"],
    );
    let root = dir.path().join(".orchestry");

    std::fs::write(
        root.join("tasks").join("tsk_done.yml"),
        format!(
            "id: tsk_done\ntitle: Finished task\ndescription: ''\nstatus: done\npriority: 2\n\
             labels: []\ndepends_on: []\ncreated_at: '{RUN_STARTED_AT}'\n\
             updated_at: '{TASK_UPDATED_AT}'\nattempts: 1\nmax_attempts: 3\n"
        ),
    )
    .expect("tsk_done.yml");
    std::fs::write(
        root.join("tasks").join("tsk_todo.yml"),
        format!(
            "id: tsk_todo\ntitle: Awaiting task\ndescription: ''\nstatus: todo\npriority: 4\n\
             labels: []\ndepends_on: []\ncreated_at: '{RUN_STARTED_AT}'\n\
             updated_at: '{RUN_STARTED_AT}'\nattempts: 0\nmax_attempts: 3\n"
        ),
    )
    .expect("tsk_todo.yml");
    std::fs::write(
        root.join("goals").join("gol_ship.yml"),
        format!(
            "id: gol_ship\ntitle: Shipping goal\ndescription: ''\nstatus: active\n\
             created_at: '{RUN_STARTED_AT}'\nupdated_at: '{RUN_STARTED_AT}'\n"
        ),
    )
    .expect("gol_ship.yml");
    std::fs::write(
        root.join("runs").join("run_0001.json"),
        format!(
            r#"{{"id":"run_0001","task_id":"tsk_done","agent_id":"agt_seed","attempt":1,"status":"succeeded","started_at":"{RUN_STARTED_AT}","workspace_path":"/tmp/ws","prompt":"p"}}"#
        ),
    )
    .expect("run_0001.json");
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
    // `--observe`: the orchestrator is never started, so no agent can be
    // dispatched from this session.
    let command = format!("'{}' tui --observe", orch_binary().display());
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

/// The 0-based (column, row) of `needle` on a captured screen, in cells.
/// Every glyph left of the chips is one cell wide.
#[track_caller]
fn locate(screen: &str, needle: &str) -> (usize, usize) {
    screen
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            line.find(needle)
                .map(|byte| (line[..byte].chars().count(), row))
        })
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{screen}"))
}

/// The screen row holding `needle`, with tmux's trailing blanks already gone.
#[track_caller]
fn row<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{screen}"))
        .trim_end()
}

#[test]
fn tables_headers_sort_and_elapsed_in_tmux() {
    let dir = project();
    let tmux = launch(
        dir.path(),
        format!("orch-y2-{}-{}", std::process::id(), line!()),
    );
    let screen = tmux.wait_for("the dashboard", |s| {
        s.contains("ORCH") && s.contains("Finished task")
    });
    // No orchestrator, so nothing in this session can dispatch an agent.
    assert!(screen.contains("OBSERVING"), "{screen}");

    // The Tasks table has a header row, with the default sort marked on it.
    let header = row(&screen, "STATUS");
    for label in ["STATUS↑", "PRI", "TITLE", "GOAL", "ASSIGNEE", "STARTED"] {
        assert!(header.contains(label), "{label} is missing from {header:?}");
    }
    assert!(header.contains("ELAPSED*"), "{header:?}");

    // The run record is the task's start, and a terminal row's span stops at
    // its last update — exactly the hour between the two seeded timestamps.
    assert!(
        row(&screen, "Finished task").ends_with(FROZEN_ELAPSED),
        "{screen}"
    );
    // Never started: both cells are blank, so the row ends at its assignee.
    assert!(row(&screen, "Awaiting task").ends_with('—'), "{screen}");

    // `o` takes the sort to the next column and marks it there; priority 2
    // sorts ahead of priority 4.
    tmux.keys(&["o"]);
    let screen = tmux.wait_for("the Priority sort", |s| s.contains("PRI↑"));
    assert!(!screen.contains("STATUS↑"), "{screen}");
    assert!(
        locate(&screen, "Finished task").1 < locate(&screen, "Awaiting task").1,
        "{screen}"
    );

    // `O` keeps the column and flips the direction.
    tmux.keys(&["O"]);
    let screen = tmux.wait_for("the reversed Priority sort", |s| s.contains("PRI↓"));
    assert!(
        locate(&screen, "Awaiting task").1 < locate(&screen, "Finished task").1,
        "{screen}"
    );

    // Clicking an inactive header sorts by it, ascending.
    let (x, y) = locate(&screen, "TITLE");
    tmux.click(x, y);
    let screen = tmux.wait_for("the Title sort", |s| s.contains("TITLE↑"));
    assert!(!screen.contains("PRI↓"), "{screen}");
    assert!(
        locate(&screen, "Awaiting task").1 < locate(&screen, "Finished task").1,
        "{screen}"
    );

    // Clicking the active header flips it instead.
    tmux.click(x, y);
    let screen = tmux.wait_for("the reversed Title sort", |s| s.contains("TITLE↓"));
    assert!(
        locate(&screen, "Finished task").1 < locate(&screen, "Awaiting task").1,
        "{screen}"
    );

    // `0` restores the default order.
    tmux.keys(&["0"]);
    let screen = tmux.wait_for("the default sort", |s| s.contains("STATUS↑"));
    assert!(
        !screen.contains("TITLE↑") && !screen.contains("TITLE↓"),
        "{screen}"
    );

    // `?` carries the Elapsed* disclosure, which is what the asterisk means.
    tmux.keys(&["?"]);
    let screen = tmux.wait_for("the help overlay", |s| s.contains("KEYBOARD SHORTCUTS"));
    assert!(
        screen.contains(
            "Elapsed*: start to now; terminal rows end at last update (edits change it)."
        ),
        "{screen}"
    );
    assert!(screen.contains("Includes waiting/retries."), "{screen}");
    tmux.keys(&["Escape"]);

    // At 80x24, the default terminal size, the overlay is taller than the
    // area under the header, and the disclosure must still be on screen.
    tmux.run(&["resize-window", "-t", "dash", "-x", "80", "-y", "24"]);
    tmux.keys(&["?"]);
    let help = [
        "Elapsed*: start to now; terminal rows end at last update (edits change it).",
        "Includes waiting/retries. Elapsed order updates on refresh or",
        "list-control changes; displayed time is live.",
        "Agents: b/B change the grouping. Caps: grouped by first capability; use",
        "the filter to find every agent with a capability. UPDATED: which agent's",
        "run or assignment record changed most recently. R: run; T: assigned task",
        "record updated; may be an administrative change, not proof the agent ran.",
        "Press any key to dismiss",
    ];
    let screen = tmux.wait_for("the help overlay at 80x24", |s| {
        s.lines().all(|line| line.chars().count() <= 80) && help.iter().all(|line| s.contains(line))
    });
    assert!(screen.contains("KEYBOARD SHORTCUTS"), "{screen}");
    tmux.keys(&["Escape"]);
    tmux.run(&["resize-window", "-t", "dash", "-x", "120", "-y", "40"]);

    // The Goals table has the same two columns, over the goal's creation time.
    tmux.keys(&["Left"]);
    let screen = tmux.wait_for("the Goals view", |s| s.contains("Shipping goal"));
    let header = row(&screen, "STATUS");
    assert!(header.contains("STARTED"), "{header:?}");
    assert!(header.contains("ELAPSED*"), "{header:?}");

    tmux.keys(&["q"]);
}

/// `orch` with its trimmed stdout, for the ids `--quiet` prints.
fn orch_out(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// For the keyword filter: three cancelled tasks: `Alpha deploy` without a goal, `Bravo docs` and
/// `Charlie deploy` in the goal `Release train`.
fn keyword_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    orch(
        dir.path(),
        &["init", "--name", "proj", "--adapter", "shell"],
    );
    let goal = orch_out(dir.path(), &["goal", "add", "Release train", "--quiet"]);
    for (title, goal) in [
        ("Alpha deploy", None),
        ("Bravo docs", Some(goal.as_str())),
        ("Charlie deploy", Some(goal.as_str())),
    ] {
        let mut args = vec!["task", "add", title, "--quiet"];
        if let Some(goal) = goal {
            args.extend(["--goal-id", goal]);
        }
        let id = orch_out(dir.path(), &args);
        orch(dir.path(), &["task", "cancel", &id]);
    }
    dir
}

/// Type `text` into the keyword prompt one key at a time, waiting for each
/// to show, so the check is about the filter and not about key batching.
fn type_keyword(tmux: &Tmux, text: &str) {
    let mut typed = String::new();
    for ch in text.chars() {
        typed.push(ch);
        tmux.run(&["send-keys", "-t", "dash", "-l", &ch.to_string()]);
        let prompt = format!("Filter tasks › {typed} ");
        tmux.wait_for(&prompt, |s| {
            s.contains(&prompt) && tmux.keyword_cursor_reversed(&typed)
        });
    }
}

#[test]
fn keyword_sort_group_and_reset() {
    let dir = keyword_project();
    let tmux = launch(
        dir.path(),
        format!("orch-y3-{}-{}", std::process::id(), line!()),
    );
    tmux.wait_for("the dashboard", |s| s.contains("ORCH"));
    tmux.keys(&["t"]);
    let screen = tmux.wait_for("the three tasks", |s| {
        s.contains("Alpha deploy") && s.contains("Bravo docs") && s.contains("Charlie deploy")
    });
    assert!(screen.contains("^K filter"), "{screen}");

    // `/` is still the command bar (R5).
    tmux.keys(&["/"]);
    tmux.wait_for("the command bar", |s| s.contains("COMMANDS"));
    tmux.keys(&["Escape"]);
    tmux.wait_for("the command bar to close", |s| !s.contains("COMMANDS"));

    // Ctrl+K: the list narrows as the keyword is typed (R3).
    tmux.keys(&["C-k"]);
    tmux.wait_for("the keyword prompt", |s| s.contains("Filter tasks ›"));
    type_keyword(&tmux, "DePloy");
    let screen = tmux.wait_for("two matches", |s| s.contains("2/3"));
    assert!(screen.contains("Alpha deploy"), "{screen}");
    assert!(screen.contains("Charlie deploy"), "{screen}");
    assert!(!screen.contains("Bravo docs"), "{screen}");

    // Enter keeps it, shown as a chip on the list's title rule (R4).
    tmux.keys(&["Enter"]);
    tmux.wait_for("the keyword chip", |s| {
        s.contains("filter: DePloy · 2/3") && !s.contains("Filter tasks ›")
    });

    // A header sort orders the matches only: Title ascending, then
    // descending.
    let screen = tmux.screen();
    let (x, y) = locate(&screen, "TITLE");
    tmux.click(x, y);
    tmux.wait_for("Title ascending", |s| s.contains("TITLE↑"));
    tmux.click(x, y);
    let screen = tmux.wait_for("Title descending", |s| s.contains("TITLE↓"));
    assert!(
        locate(&screen, "Charlie deploy").1 < locate(&screen, "Alpha deploy").1,
        "{screen}"
    );

    // Goal grouping keeps the filter; the goal's section counts its listed
    // tasks against all of them.
    tmux.keys(&["g"]);
    let screen = tmux.wait_for("grouped matches", |s| {
        s.contains("RELEASE TRAIN · 1/2 tasks") && s.contains("UNGROUPED · 1 task")
    });
    assert!(!screen.contains("Bravo docs"), "{screen}");
    assert!(
        locate(&screen, "Charlie deploy").1 < locate(&screen, "Alpha deploy").1,
        "{screen}"
    );

    // Ctrl+U clears it; sort and grouping stay.
    tmux.keys(&["C-u"]);
    let screen = tmux.wait_for("the whole list", |s| s.contains("Bravo docs"));
    assert!(!screen.contains("filter:"), "{screen}");
    assert!(screen.contains("RELEASE TRAIN · 2 tasks"), "{screen}");
    assert!(screen.contains("TITLE↓"), "{screen}");

    // A paste loses its control characters, ESC and newline included (R13):
    // `ESC c` would reset a terminal it reached.
    let paste = dir.path().join("paste.txt");
    std::fs::write(&paste, "\u{1b}char\nlie\t").expect("paste file");
    tmux.keys(&["C-k"]);
    tmux.wait_for("the keyword prompt", |s| s.contains("Filter tasks ›"));
    tmux.run(&[
        "load-buffer",
        "-b",
        "kw",
        paste.to_str().expect("utf-8 temp path"),
    ]);
    // `-S`: hand the raw bytes over, not tmux's own `^[` rendering of them.
    // A tmux without `-S` (CI's Ubuntu package) predates that rendering and
    // already pastes the raw bytes.
    let raw = tmux
        .command()
        .args(["paste-buffer", "-p", "-S", "-b", "kw", "-t", "dash"])
        .stdin(Stdio::null())
        .output()
        .expect("tmux runs");
    if !raw.status.success() {
        let stderr = String::from_utf8_lossy(&raw.stderr);
        assert!(
            stderr.contains("unknown flag"),
            "paste-buffer failed: {stderr}"
        );
        tmux.run(&["paste-buffer", "-p", "-b", "kw", "-t", "dash"]);
    }
    let screen = tmux.wait_for("the sanitized paste", |s| {
        s.contains("Filter tasks › charlie ") && tmux.keyword_cursor_reversed("charlie")
    });
    assert!(screen.contains("1/3"), "{screen}");
    assert!(screen.contains("ORCH"), "{screen}");
    assert!(!screen.contains("Bravo docs"), "{screen}");

    // Esc in the prompt clears and closes it.
    tmux.keys(&["Escape"]);
    let screen = tmux.wait_for("the cleared list", |s| {
        s.contains("Bravo docs") && !s.contains("Filter tasks ›")
    });
    assert!(screen.contains("Alpha deploy"), "{screen}");

    // The help overlay documents the keys (R5).
    tmux.keys(&["?"]);
    let screen = tmux.wait_for("the help overlay", |s| s.contains("Ctrl+K"));
    assert!(screen.contains("Filter list"), "{screen}");
    assert!(screen.contains("Ctrl+U"), "{screen}");
    tmux.keys(&["Escape"]);

    tmux.keys(&["q"]);
}

/// Eleven cancelled tasks: `Item 01`..`Item 10`, then `Zulu shipit`, which
/// sorts last by Title and so starts behind the ten-row cap.
fn capped_keyword_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    orch(
        dir.path(),
        &["init", "--name", "proj", "--adapter", "shell"],
    );
    let titles = (1..=10)
        .map(|i| format!("Item {i:02}"))
        .chain(["Zulu shipit".to_string()]);
    for title in titles {
        let id = orch_out(dir.path(), &["task", "add", &title, "--quiet"]);
        orch(dir.path(), &["task", "cancel", &id]);
    }
    dir
}

/// The keyword filters before the ten-row cap (R3, D1): a match that was
/// behind `Show all` is listed as soon as it is typed, without show-all.
#[test]
fn keyword_finds_a_match_behind_the_ten_row_cap() {
    let dir = capped_keyword_project();
    let tmux = launch(
        dir.path(),
        format!("orch-y3-{}-{}", std::process::id(), line!()),
    );
    tmux.wait_for("the dashboard", |s| s.contains("ORCH"));
    tmux.keys(&["t"]);
    // Newest first: `Zulu shipit` leads and `Item 01` is behind the cap.
    let screen = tmux.wait_for("the task list", |s| s.contains("Item 02"));

    // Title ascending puts `Zulu shipit` eleventh: behind the cap.
    let (x, y) = locate(&screen, "TITLE");
    tmux.click(x, y);
    let screen = tmux.wait_for("Title ascending", |s| {
        s.contains("TITLE↑") && s.contains("Show all (1 more)")
    });
    assert!(screen.contains("Item 10"), "{screen}");
    assert!(!screen.contains("Zulu shipit"), "{screen}");

    // Typed one key at a time, the list narrows to the hidden match.
    tmux.keys(&["C-k"]);
    tmux.wait_for("the keyword prompt", |s| s.contains("Filter tasks ›"));
    type_keyword(&tmux, "shipit");
    let screen = tmux.wait_for("the match behind the cap", |s| {
        s.contains("Zulu shipit") && s.contains("1/11")
    });
    assert!(!screen.contains("Item 01"), "{screen}");
    assert!(!screen.contains("Show all"), "{screen}");

    // Kept, the chip counts it against the whole list.
    tmux.keys(&["Enter"]);
    let screen = tmux.wait_for("the keyword chip", |s| {
        s.contains("filter: shipit · 1/11") && !s.contains("Filter tasks ›")
    });
    assert!(screen.contains("Zulu shipit"), "{screen}");

    // Cleared, the cap applies again and the match goes back behind it.
    tmux.keys(&["C-u"]);
    let screen = tmux.wait_for("the capped list", |s| {
        s.contains("Item 01") && s.contains("Show all (1 more)")
    });
    assert!(!screen.contains("filter:"), "{screen}");
    assert!(!screen.contains("Zulu shipit"), "{screen}");

    tmux.keys(&["q"]);
}
