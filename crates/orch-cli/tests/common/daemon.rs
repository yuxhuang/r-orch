//! The one place binary-level fixtures get an explicit test daemon (T2b).
//!
//! Every fixture that runs the `orch` binary against a throwaway project
//! starts its own daemon here, on an isolated socket, and stops it before the
//! test ends. No test may reach a real project's daemon or `~/.orchestry`.
//!
//! # Socket-path scheme
//!
//! * The socket lives at `<short>/s`, where `<short>` is a fresh `0700`
//!   directory `/tmp/otd-XXXXXX` (`mkdtemp`-style, 11 characters after
//!   `/tmp/`), so the full path is ≈ 19 bytes — far under the 100-byte
//!   budget (macOS `sun_path` is 104). The project directory is *not* used:
//!   `$TMPDIR` on macOS (`/var/folders/…`) plus a project path overflows it.
//! * `ORCH_SOCKET=<short>/s` is the only endpoint the daemon binds and the
//!   clients dial. `ORCH_TEST_SOCKET_ROOT=<short>` makes the client refuse any
//!   socket outside that directory, so a stray environment cannot send a
//!   fixture to someone else's daemon.
//! * A fixture's child commands get exactly those two variables through
//!   [`TestDaemon::apply`]; nothing else about their environment is changed.
//!
//! # Lifecycle
//!
//! [`TestDaemon::start`] spawns a **foreground** `orch daemon start` in its
//! own process group (dispatch stays OFF, the daemon default), waits for its
//! `listening <socket>` line, and fails fast if the child exits first.
//! [`TestDaemon::shutdown`] (also run by `Drop`) sends SIGTERM (the daemon's
//! documented clean exit, ~0.1 s; `orch daemon stop` polls for ~1 s, which adds
//! minutes to fixtures that start hundreds of daemons), falls back to SIGKILL
//! of the process group, and then takes a **process-table** snapshot: nothing
//! in the daemon's group and nothing whose command line names the unique
//! socket directory may survive. A survivor is killed and the teardown panics
//! (unless the test is already unwinding).
//!
//! # Using it from a fixture
//!
//! * Start the daemon **last**, after everything the fixture writes by hand
//!   (a cached peer snapshot, a `remotes.yml`): a running daemon owns the
//!   state and its peer runtime rewrites `remotes/<alias>.json`.
//! * Own it next to the project directory so it is dropped first
//!   ([`OwnedDir`], or a struct with the daemon declared before the `TempDir`).
//! * Fixtures keyed by working directory (`orch(cwd)`) call
//!   [`register_project`] once and [`apply_for`] in their command constructor;
//!   launchers that take an env list (tmux `-e`) use [`env_pairs_for`].
//! * Every command is a daemon client since S2a-cli (`orch run <id>`, `run
//!   --all`, `run --once`, `serve --once` dispatch through the socket), so a
//!   fixture that runs agents needs the daemon, and a fixture that runs
//!   `orch serve` / `run --watch` in the foreground *is* its own daemon and
//!   must not own a second one ("already running", exit 4).
//! * A fixture that builds a command before it has a daemon uses
//!   [`apply_env`]; a fixture that wants "no daemon" simply does not apply
//!   the socket env.
#![allow(dead_code, reason = "each test crate uses a subset of this module")]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// Upper bound on readiness, `daemon stop` and group exit. Finite so a daemon
/// regression fails the test instead of hanging CI; generous because the whole
/// test binary shares the machine with other threads.
const WAIT: Duration = Duration::from_secs(30);
/// Slice between `try_wait` checks (an upper bound on exit-detection latency,
/// not a pacing delay).
const POLL: Duration = Duration::from_millis(20);
/// The longest socket path the daemon accepts.
const MAX_SOCKET_BYTES: usize = 100;

/// A running foreground test daemon plus its isolated socket directory.
pub struct TestDaemon {
    child: Child,
    pgid: i32,
    root: PathBuf,
    home: PathBuf,
    socket: PathBuf,
    short: tempfile::TempDir,
    /// Set once [`TestDaemon::shutdown`] has run to completion.
    done: bool,
}

impl TestDaemon {
    /// Start `orch daemon start` for the initialised project at `root`.
    ///
    /// `home` is the fixture's private `HOME`; the daemon also gets
    /// `ORCHESTRY_GLOBAL_CONFIG_PATH=<home>/global.yml` so it cannot read the
    /// developer's global config. The environment is otherwise cleared, with a
    /// `PATH` that holds no agent CLI (dispatch is OFF, so none is run).
    pub fn start(root: &Path, home: &Path) -> TestDaemon {
        Self::start_with_env(root, home, &[])
    }

    /// [`start`](Self::start) with extra daemon environment, applied last (so
    /// it may override `PATH`): for fixtures whose agents are fakes on a
    /// private `PATH`, which a daemon-spawned agent will need to find.
    pub fn start_with_env(root: &Path, home: &Path, extra: &[(&str, &Path)]) -> TestDaemon {
        let root = root.canonicalize().expect("project root exists");
        fs::create_dir_all(home).expect("fixture HOME");
        let home = home.canonicalize().expect("fixture HOME exists");
        let short = tempfile::Builder::new()
            .prefix("otd-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .expect("short socket dir under /tmp");
        let socket = short.path().join("s");
        assert!(
            socket.as_os_str().len() <= MAX_SOCKET_BYTES,
            "socket path {} exceeds {MAX_SOCKET_BYTES} bytes",
            socket.display()
        );
        let err_path = short.path().join("daemon.err");
        let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
        command
            .env_clear()
            .current_dir(&root)
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", home.join("global.yml"))
            .env("ORCH_PROJECT_ROOT", &root)
            .env("ORCH_SOCKET", &socket)
            .env("ORCH_TEST_SOCKET_ROOT", short.path())
            .envs(extra.iter().copied())
            .args(["daemon", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&err_path).expect("daemon stderr file"))
            .process_group(0);
        let mut child = command.spawn().expect("spawn `orch daemon start`");
        let pgid = i32::try_from(child.id()).expect("pid fits i32");
        let stdout = child.stdout.take().expect("piped stdout");
        let lines = read_lines(stdout);
        let mut daemon = TestDaemon {
            child,
            pgid,
            root,
            home,
            socket,
            short,
            done: false,
        };
        daemon.await_ready(&lines, &err_path);
        daemon
    }

    fn await_ready(&mut self, lines: &Receiver<Option<String>>, err_path: &Path) {
        let deadline = Instant::now() + WAIT;
        let want = format!("listening {}", self.socket.display());
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left.min(POLL)) {
                Ok(Some(line)) if line == want => return,
                Ok(Some(_)) => {}
                Ok(None) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if self.child.try_wait().expect("try_wait").is_some() {
                        // A readiness line printed just before exit may still
                        // be queued; the loop above would have taken it.
                        break;
                    }
                    assert!(
                        left > Duration::ZERO,
                        "test daemon not ready within {WAIT:?}: {}",
                        fs::read_to_string(err_path).unwrap_or_default()
                    );
                }
            }
        }
        let status = self.child.wait().expect("wait for the daemon child");
        self.kill_group();
        panic!(
            "test daemon exited before readiness ({status}): {}",
            fs::read_to_string(err_path).unwrap_or_default()
        );
    }

    /// The isolated socket every client of this daemon must dial.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The directory holding the socket (`ORCH_TEST_SOCKET_ROOT`).
    pub fn socket_dir(&self) -> &Path {
        self.short.path()
    }

    /// The canonical project root the daemon owns.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Point `command` at this daemon: `ORCH_SOCKET`, `ORCH_TEST_SOCKET_ROOT`
    /// and the developmental gate. Touches nothing else in its environment.
    pub fn apply(&self, command: &mut Command) {
        apply_env(command, &self.socket, self.short.path());
    }

    /// The variables [`apply`](Self::apply) sets, as `KEY=value` strings, for
    /// launchers that take an environment list (`tmux new-session -e`).
    pub fn env_pairs(&self) -> Vec<String> {
        vec![
            format!("ORCH_SOCKET={}", self.socket.display()),
            format!("ORCH_TEST_SOCKET_ROOT={}", self.short.path().display()),
        ]
    }

    /// `orch <args>` against this daemon, with the same cleared environment
    /// the daemon runs in (for control calls such as `daemon status`).
    pub fn control(&self, args: &[&str]) -> std::process::Output {
        self.try_control(args).unwrap_or_else(|why| panic!("{why}"))
    }

    /// [`control`](Self::control) that reports a timeout instead of panicking
    /// (teardown must not panic while a test is already unwinding).
    fn try_control(&self, args: &[&str]) -> Result<std::process::Output, String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
        command
            .env_clear()
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", self.home.join("global.yml"))
            .env("ORCH_PROJECT_ROOT", &self.root)
            .args(args)
            .stdin(Stdio::null())
            .process_group(0);
        self.apply(&mut command);
        output_within(command, WAIT)
    }

    /// Stop the daemon and prove nothing survived. Panics on a survivor.
    pub fn shutdown(mut self) {
        self.teardown(true);
    }

    fn teardown(&mut self, may_panic: bool) {
        if self.done {
            return;
        }
        self.done = true;
        // Polite stop first: SIGTERM is the documented clean exit (the same
        // path as `daemon stop`, without that command's ~1 s client poll, which
        // would add minutes to fixtures that start hundreds of daemons). A
        // daemon that already died (a test that crashed it on purpose) is a
        // valid end state; the process-table check below is the verdict.
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = nix::sys::signal::kill(Pid::from_raw(self.pgid), Signal::SIGTERM);
        }
        if !self.wait_exit(WAIT) {
            self.kill_group();
            let _ = self.child.wait();
        }
        let survivors = self.survivors();
        if !survivors.is_empty() {
            for pid in &survivors {
                let _ = nix::sys::signal::kill(Pid::from_raw(*pid), Signal::SIGKILL);
            }
            if may_panic && !std::thread::panicking() {
                panic!(
                    "processes survived the test daemon teardown (pids {survivors:?}, \
                     socket dir {})",
                    self.short.path().display()
                );
            }
            eprintln!("test daemon teardown: killed survivors {survivors:?}");
        }
    }

    fn wait_exit(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if self.child.try_wait().expect("try_wait").is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    fn kill_group(&self) {
        let _ = killpg(Pid::from_raw(self.pgid), Signal::SIGKILL);
    }

    /// Pids of every process still in the daemon's group, or whose command line
    /// names the unique socket directory. The leader is reaped, so a surviving
    /// pgid member is a real leak, not the daemon itself.
    fn survivors(&self) -> Vec<i32> {
        let marker = self.short.path().to_string_lossy().into_owned();
        let leader = self.pgid;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let found: Vec<i32> = process_table()
                .into_iter()
                .filter(|p| p.pgid == leader || p.command.contains(&marker))
                // Never count the `ps` that took this very snapshot.
                .filter(|p| !p.command.starts_with("/bin/ps") && !p.command.starts_with("ps "))
                .map(|p| p.pid)
                .collect();
            // Group members may still be mid-exit right after SIGKILL: give the
            // kernel a bounded window to reap them before calling it a leak.
            if found.is_empty() || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.teardown(true);
    }
}

// ── per-project registry ───────────────────────────────────────────────────
//
// Fixtures whose command constructors are keyed by the working directory
// (`orch(cwd)`, `orch_command(cwd)`) register their project's daemon here, so
// the constructors keep their signatures and still aim every child under that
// project at its own socket. Keys are unique temp roots, so concurrent tests
// never see each other's entries.

use std::sync::Mutex;

/// `(canonical project root, socket, socket dir)`.
static REGISTRY: Mutex<Vec<(PathBuf, PathBuf, PathBuf)>> = Mutex::new(Vec::new());

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Make [`apply_for`] aim commands under `root` at `daemon`.
pub fn register_project(root: &Path, daemon: &TestDaemon) {
    REGISTRY.lock().unwrap().push((
        canonical(root),
        daemon.socket().to_path_buf(),
        daemon.socket_dir().to_path_buf(),
    ));
}

/// Forget `root` (call before its daemon is dropped).
pub fn unregister_project(root: &Path) {
    let root = canonical(root);
    REGISTRY.lock().unwrap().retain(|(r, _, _)| *r != root);
}

/// If `cwd` is inside a registered project, point `command` at its daemon.
pub fn apply_for(cwd: &Path, command: &mut Command) {
    let cwd = canonical(cwd);
    let found = REGISTRY
        .lock()
        .unwrap()
        .iter()
        .find(|(root, _, _)| cwd.starts_with(root))
        .map(|(_, socket, dir)| (socket.clone(), dir.clone()));
    if let Some((socket, dir)) = found {
        apply_env(command, &socket, &dir);
    }
}

/// [`TestDaemon::env_pairs`] for the registered project containing `cwd`
/// (empty if it has none), for launchers keyed by directory.
pub fn env_pairs_for(cwd: &Path) -> Vec<String> {
    let cwd = canonical(cwd);
    REGISTRY
        .lock()
        .unwrap()
        .iter()
        .find(|(root, _, _)| cwd.starts_with(root))
        .map(|(_, socket, dir)| {
            vec![
                format!("ORCH_SOCKET={}", socket.display()),
                format!("ORCH_TEST_SOCKET_ROOT={}", dir.display()),
            ]
        })
        .unwrap_or_default()
}

/// A throwaway project directory that owns its test daemon. `path()` is the
/// project root, as for a `TempDir`; the daemon is stopped (and the process
/// table checked) before the directory is removed.
///
/// Every `orch` command is a daemon client, so a fixture that seeds its
/// project with `orch goal add …` / `orch task add …` adopts the daemon right
/// after `orch init` and seeds through [`apply_for`]-aware commands. Files a
/// fixture writes **by hand** (run records, a cached peer snapshot) go in
/// through [`OwnedDir::with_daemon_stopped`]: a daemon owns the state and
/// would not see, or would overwrite, an edit made behind its back.
pub struct OwnedDir {
    // Declared first: dropped first.
    daemon: Option<TestDaemon>,
    home: PathBuf,
    dir: tempfile::TempDir,
}

impl OwnedDir {
    /// Start a daemon for the already initialised project in `dir`, with
    /// `<dir>/home` as the fixture's HOME (created if absent).
    pub fn adopt(dir: tempfile::TempDir) -> OwnedDir {
        Self::adopt_with_home(dir, "home")
    }

    /// [`adopt`](Self::adopt) with the HOME at `<dir>/<home>`.
    pub fn adopt_with_home(dir: tempfile::TempDir, home: &str) -> OwnedDir {
        let home = dir.path().join(home);
        let daemon = TestDaemon::start(dir.path(), &home);
        register_project(dir.path(), &daemon);
        OwnedDir {
            daemon: Some(daemon),
            home,
            dir,
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn daemon(&self) -> &TestDaemon {
        self.daemon.as_ref().expect("the daemon is running")
    }

    /// Stop the daemon (process table checked), run `edit` on the quiescent
    /// project, then start a fresh daemon on a new isolated socket and
    /// re-register it. The new daemon re-reads everything `edit` wrote.
    pub fn with_daemon_stopped(&mut self, edit: impl FnOnce(&Path)) {
        unregister_project(self.dir.path());
        self.daemon
            .take()
            .expect("the daemon is running")
            .shutdown();
        edit(self.dir.path());
        let daemon = TestDaemon::start(self.dir.path(), &self.home);
        register_project(self.dir.path(), &daemon);
        self.daemon = Some(daemon);
    }
}

impl Drop for OwnedDir {
    fn drop(&mut self) {
        unregister_project(self.dir.path());
    }
}

/// Set the two variables that aim a command at `socket`. Free-standing so a
/// fixture that builds its command before it has a daemon can still use it.
pub fn apply_env(command: &mut Command, socket: &Path, socket_dir: &Path) {
    command
        .env("ORCH_SOCKET", socket)
        .env("ORCH_TEST_SOCKET_ROOT", socket_dir);
}

fn read_lines(stdout: std::process::ChildStdout) -> Receiver<Option<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if tx.send(Some(line)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(None);
    });
    rx
}

/// Run to completion under a deadline; on timeout kill the whole group.
fn output_within(mut command: Command, within: Duration) -> Result<std::process::Output, String> {
    use std::io::Read;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = command.spawn().expect("spawn orch");
    let pid = i32::try_from(child.id()).expect("pid fits i32");
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + within;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
            let _ = child.wait();
            return Err(format!("orch control call exceeded {within:?}"));
        }
        std::thread::sleep(POLL);
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
    })
}

/// One row of `ps -axo pid=,ppid=,pgid=,command=`.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: i32,
    pub pgid: i32,
    pub command: String,
}

/// The whole process table (`ps` is the portable macOS/Linux reader).
pub fn process_table() -> Vec<Proc> {
    let out = Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid=,command="])
        .output()
        .expect("ps runs");
    assert!(out.status.success(), "ps failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut number = || {
                let end = rest.find(char::is_whitespace)?;
                let (head, tail) = rest.split_at(end);
                rest = tail.trim_start();
                head.parse::<i32>().ok()
            };
            let pid = number()?;
            let pgid = number()?;
            Some(Proc {
                pid,
                pgid,
                command: rest.to_owned(),
            })
        })
        .collect()
}
