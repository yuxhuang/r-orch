//! Shared fixtures for the binary-level daemon lifecycle tests (T1b).
//!
//! Hermeticity rules this module enforces for every test that uses it:
//!
//! * every `orch` child gets an explicitly **cleared** environment plus an
//!   isolated `HOME`, `ORCHESTRY_GLOBAL_CONFIG_PATH` and a short
//!   `ORCH_SOCKET` (≤100 bytes, under a 0700 `/tmp/dl-*` dir), so it cannot
//!   reach a developer's daemon, `~/.orchestry` or a real project;
//! * `ORCH_DAEMON_DEVELOPMENTAL=1` exists only in the child env, never in
//!   the test process;
//! * agents are `sleep` stand-ins on `PATH=/usr/bin:/bin`, never a real AI CLI;
//! * every spawned daemon is owned by a [`DaemonChild`] whose `Drop` kills its
//!   process group, so a failed assertion cannot leak a daemon;
//! * every wait has a deadline and is driven by an observable event (the
//!   `listening <socket>` line, an exit status, a file), never a fixed sleep.
#![allow(dead_code, reason = "each daemon_lifecycle* test crate uses a subset")]

use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub const READY: Duration = Duration::from_secs(30);
pub const CALL: Duration = Duration::from_secs(30);

/// An isolated project dir + short socket dir.
pub struct Sandbox {
    project: tempfile::TempDir,
    short: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join("home")).unwrap();
        let short = tempfile::Builder::new()
            .prefix("dl-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        Sandbox { project, short }
    }

    /// An uninitialised project root (no `.orchestry`).
    pub fn raw_root(&self) -> PathBuf {
        self.project.path().canonicalize().unwrap()
    }

    pub fn root(&self) -> PathBuf {
        self.raw_root()
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root().join(".orchestry")
    }

    pub fn short_dir(&self) -> &Path {
        self.short.path()
    }

    pub fn socket(&self) -> PathBuf {
        let socket = self.short.path().join("s");
        assert!(socket.as_os_str().len() <= 100);
        socket
    }

    /// The only constructor of an `orch` command in these tests.
    pub fn orch(&self) -> Command {
        orch_command(&self.root(), &self.socket(), self.short.path())
    }

    /// Run `orch <args>` to completion under a deadline.
    pub fn run(&self, args: &[&str]) -> Output {
        let mut cmd = self.orch();
        cmd.args(args);
        output_with_deadline(cmd, CALL)
    }

    /// A start command; `extra` follows `daemon start`.
    pub fn start_cmd(&self, extra: &[&str]) -> Command {
        let mut cmd = self.orch();
        cmd.args(["daemon", "start"]).args(extra);
        cmd
    }

    /// `orch init` plus a fast dispatch tick.
    pub fn init_project(&self) {
        let out = self.run(&["init", "--name", "proj", "--adapter", "shell"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let config = self.state_dir().join("config.yml");
        let body = fs::read_to_string(&config).unwrap();
        assert!(body.contains("poll_interval_ms: 10000"), "{body}");
        fs::write(
            &config,
            body.replace("poll_interval_ms: 10000", "poll_interval_ms: 300"),
        )
        .unwrap();
    }

    /// Seed one task assigned to a `sleep 120` stand-in, with the legacy
    /// direct-store CLI (no daemon exists yet). Returns `(agent, task)` ids.
    pub fn seed_sleeper(&self) -> (String, String) {
        let out = self.run(&[
            "agent",
            "add",
            "sleeper",
            "--adapter",
            "shell",
            "--command",
            "sleep 120",
            "--json",
        ]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let agent: Value = serde_json::from_slice(&out.stdout).unwrap();
        let agent = agent["id"].as_str().unwrap().to_owned();
        let out = self.run(&["task", "add", "long job", "--assignee", &agent, "--quiet"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        (agent, text(&out.stdout).trim().to_owned())
    }

    pub fn state_json(&self) -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(self.state_dir().join("state.json")).ok()?).ok()
    }

    /// The agent pid the daemon recorded for `task`, once it is running.
    pub fn running_pid(&self, task: &str) -> Option<u32> {
        let state = self.state_json()?;
        u32::try_from(state["running"][task]["pid"].as_u64()?).ok()
    }

    pub fn running_run_id(&self, task: &str) -> Option<String> {
        let state = self.state_json()?;
        Some(state["running"][task]["run_id"].as_str()?.to_owned())
    }

    pub fn run_status(&self, run_id: &str) -> Option<String> {
        let body = fs::read_to_string(self.state_dir().join(format!("runs/{run_id}.json"))).ok()?;
        let run: Value = serde_json::from_str(&body).ok()?;
        Some(run["status"].as_str()?.to_owned())
    }

    pub fn daemon_json(&self) -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(self.state_dir().join("daemon.json")).ok()?).ok()
    }

    /// `orch --json daemon status` as a typed result; `Err` is the output.
    pub fn status(&self) -> Result<orch_proto::ops::results::DaemonStatus, Output> {
        let out = self.run(&["--json", "daemon", "status"]);
        if out.status.success() {
            Ok(serde_json::from_slice(&out.stdout).unwrap())
        } else {
            Err(out)
        }
    }
}

/// An `orch` command with an explicit, cleared environment aimed at `socket`
/// and confined to `test_root`. `root` also receives `HOME`.
pub fn orch_command(root: &Path, socket: &Path, test_root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orch"));
    cmd.env_clear()
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("NO_COLOR", "1")
        .env("TERM", "xterm-256color")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("global.yml"))
        .env("ORCH_PROJECT_ROOT", root)
        .env("ORCH_SOCKET", socket)
        .env("ORCH_TEST_SOCKET_ROOT", test_root)
        .env("ORCH_DAEMON_DEVELOPMENTAL", "1")
        .process_group(0);
    cmd
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Run to completion; on timeout kill the whole group and panic.
pub fn output_with_deadline(cmd: Command, within: Duration) -> Output {
    output_and_pid(cmd, within).0
}

/// Like [`output_with_deadline`], also returning the (reaped) child's pid.
pub fn output_and_pid(mut cmd: Command, within: Duration) -> (Output, u32) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let pid = child.id();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + within;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.wait();
            panic!("command exceeded {within:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    (
        Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        },
        pid,
    )
}

/// Poll `probe` until it yields a value (an external effect with no push
/// notification: file content, process death). Bounded, never a bare sleep.
pub fn wait_for<T>(within: Duration, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

pub fn wait_dead(pid: u32, what: &str) {
    wait_for(READY, what, || (!is_alive(pid)).then_some(()));
}

pub fn signal(pid: u32, signal: nix::sys::signal::Signal) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        signal,
    )
    .unwrap();
}

/// SIGKILL a pid we own the record of (best effort; used by guards only).
pub fn force_kill(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

/// What a start child did.
#[derive(Debug)]
pub enum Outcome {
    /// Printed `listening <socket>`.
    Ready(String),
    /// Exited before readiness.
    Exited(ExitStatus),
}

/// A foreground `orch daemon start` child: readiness by its `listening`
/// line, process group killed on drop.
pub struct DaemonChild {
    child: Child,
    lines: Receiver<Option<String>>,
    stderr_path: PathBuf,
    pub pid: u32,
}

impl DaemonChild {
    pub fn spawn(mut cmd: Command, stderr_file: PathBuf) -> DaemonChild {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&stderr_file).unwrap());
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
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
        DaemonChild {
            child,
            lines,
            stderr_path: stderr_file,
            pid,
        }
    }

    /// Block until `listening ...` or exit, under a deadline.
    pub fn outcome(&mut self, within: Duration) -> Outcome {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(Some(line)) if line.starts_with("listening ") => return Outcome::Ready(line),
                Ok(Some(_)) => {}
                Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                    return Outcome::Exited(self.wait_exit(left.max(CALL)));
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "no readiness line or exit within {within:?}: {}",
                        self.stderr()
                    )
                }
            }
        }
    }

    pub fn expect_ready(&mut self, socket: &Path) {
        match self.outcome(READY) {
            Outcome::Ready(line) => assert_eq!(line, format!("listening {}", socket.display())),
            Outcome::Exited(status) => panic!("start exited early ({status}): {}", self.stderr()),
        }
    }

    pub fn wait_exit(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "daemon child did not exit in {within:?}: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn signal(&self, sig: nix::sys::signal::Signal) {
        signal(self.pid, sig);
    }

    /// SIGKILL and reap: the "crash" of the crash tests.
    pub fn crash(&mut self) -> ExitStatus {
        self.signal(nix::sys::signal::Signal::SIGKILL);
        self.wait_exit(READY)
    }

    pub fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        // Own process group (`process_group(0)` in `Sandbox::orch`).
        if let Ok(pgid) = i32::try_from(self.pid) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pgid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Kills a bare pid (an orphaned agent stand-in) on drop.
pub struct KillOnDrop(pub u32);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        force_kill(self.0);
    }
}

/// Starts a foreground daemon and waits for readiness plus one successful
/// handshake (`daemon status`).
pub fn start_ready(sandbox: &Sandbox, extra: &[&str], tag: &str) -> DaemonChild {
    let mut daemon = DaemonChild::spawn(
        sandbox.start_cmd(extra),
        sandbox.short_dir().join(format!("{tag}.err")),
    );
    daemon.expect_ready(&sandbox.socket());
    let status = sandbox
        .status()
        .unwrap_or_else(|out| panic!("handshake failed: {}", text(&out.stderr)));
    assert_eq!(status.pid, daemon.pid);
    daemon
}

// ── process table ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub command: String,
}

pub fn process_table() -> Vec<Proc> {
    let out = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,pgid=,command="])
        .output()
        .expect("ps runs");
    assert!(out.status.success());
    text(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut next = || {
                let end = rest.find(char::is_whitespace)?;
                let (head, tail) = rest.split_at(end);
                rest = tail.trim_start();
                head.parse::<u32>().ok()
            };
            let pid = next()?;
            let ppid = next()?;
            let pgid = next()?;
            Some(Proc {
                pid,
                ppid,
                pgid,
                command: rest.to_owned(),
            })
        })
        .collect()
}

/// Processes of the `orch` binary under test (`argv[0]` is its exact path).
pub fn orch_processes(table: &[Proc]) -> Vec<Proc> {
    let exe = env!("CARGO_BIN_EXE_orch");
    table
        .iter()
        .filter(|p| p.command == exe || p.command.starts_with(&format!("{exe} ")))
        .cloned()
        .collect()
}

/// A process group with no members (ESRCH): the client left nothing behind.
pub fn process_group_empty(pgid: u32) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return true;
    };
    matches!(
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), None),
        Err(nix::errno::Errno::ESRCH)
    )
}
