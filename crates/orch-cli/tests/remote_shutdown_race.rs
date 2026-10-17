//! Regression: a second `stop()` must wait for the owner's shutdown.
//!
//! On SIGTERM the TUI's engine signal arm owns `stop()`; the dashboard then
//! exits and calls `stop()` again before dropping the peer runtime. When the
//! second call returned at once, the TUI exited mid-shutdown: the owner lock
//! was left behind and the wind-down was cut short (SRE H1 / code review
//! I-1, measured 2026-10-15).
//!
//! The lever is an agent whose child ignores SIGTERM, so the owner's own
//! wind-down sits in `kill_with_grace` for its full ten seconds — long
//! enough for an early exit to be unmistakable. It used to be a remote
//! cancellation, until a graceful shutdown stopped cancelling remote runs
//! and started detaching them instead.
//!
//! Real, isolated installations; no AI CLI, user configuration or fixed port.
//! Helpers are a minimal copy of `remote_peers.rs` so the two suites can be
//! edited independently. Run with:
//! `cargo test -p orch-cli --test remote_shutdown_race`.

#![cfg(unix)]

use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(25);
const POLL: Duration = Duration::from_millis(40);
/// The floor on a *completed* wind-down of a child that ignores SIGTERM.
/// Measured: the regression exits in ~1.5 s (the dashboard's own path) and a
/// shutdown that really waits for the kill grace takes ~5 s, so anything
/// above three seconds separates the two on a slow machine too.
const MIN_WIND_DOWN: Duration = Duration::from_secs(3);

fn read_json(path: impl AsRef<Path>) -> Value {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

fn text(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn signal_group(pid: u32, signal: Signal) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 1
    {
        let _ = kill(Pid::from_raw(-pid), signal);
    }
}

struct Process {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Process {
    fn spawn(mut command: Command, stdout: PathBuf, stderr: PathBuf) -> Self {
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).expect("create stdout capture"))
            .stderr(File::create(&stderr).expect("create stderr capture"));
        Self {
            child: command.spawn().expect("spawn isolated child"),
            stdout,
            stderr,
        }
    }

    fn output(mut self, limit: Duration) -> Output {
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll command") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "command timed out\nstdout: {}\nstderr: {}",
                text(&self.stdout),
                text(&self.stderr)
            );
            thread::sleep(POLL);
        };
        Output {
            status,
            stdout: fs::read(&self.stdout).unwrap_or_default(),
            stderr: fs::read(&self.stderr).unwrap_or_default(),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        signal_group(self.child.id(), Signal::SIGKILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Peer {
    dir: tempfile::TempDir,
    name: String,
    home: PathBuf,
    serial: Cell<usize>,
    owner: RefCell<Option<Process>>,
}

impl Peer {
    fn new(name: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("orch-shutdown-race-")
            .tempdir()
            .expect("sandbox project");
        let home = dir.path().join("test-home");
        fs::create_dir_all(&home).expect("sandbox home");
        let peer = Self {
            dir,
            name: name.into(),
            home,
            serial: Cell::new(0),
            owner: RefCell::new(None),
        };
        peer.ok(&["init", "--name", name, "--adapter", "shell"]);
        let mut config = serde_json::to_value(orch_domain::default_config()).unwrap();
        config["project"]["name"] = json!(name);
        config["defaults"]["agent"]["adapter"] = json!("shell");
        config["defaults"]["agent"]["workspace_mode"] = json!("shared");
        config["defaults"]["agent"]["stall_timeout_ms"] = json!(30_000);
        config["defaults"]["agent"]["timeout_ms"] = json!(60_000);
        config["scheduling"]["poll_interval_ms"] = json!(100);
        config["scheduling"]["retry_base_delay_ms"] = json!(100);
        // JSON is a YAML subset.
        fs::write(
            peer.path(".orchestry/config.yml"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .expect("fixture config");
        peer
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
        // The test runner can itself be an orchestrated agent: drop its
        // identity, config redirects and git environment.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("ORCH") || name.starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(self.dir.path())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", self.home.join("global.yml"))
            .env("ORCH_TEST_OS_CAP", std::env::consts::OS)
            .env("ORCH_TEST_PEER_BLOCK_FILE", self.path("peer-block"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("NO_COLOR", "1")
            .env_remove("RUST_LOG")
            .env("TERM", "dumb");
        command
    }

    fn spawn(&self, command: Command, label: &str) -> Process {
        let id = self.serial.get();
        self.serial.set(id + 1);
        Process::spawn(
            command,
            self.path(&format!("{label}-{id}.stdout")),
            self.path(&format!("{label}-{id}.stderr")),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let mut command = self.command();
        command.args(args);
        let output = self.spawn(command, "cli").output(Duration::from_secs(15));
        assert!(
            output.status.success(),
            "{} orch {args:?} failed: {:?}\nstdout: {}\nstderr: {}",
            self.name,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 CLI output")
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.ok(&args);
        serde_json::from_str(&output)
            .unwrap_or_else(|err| panic!("{} invalid CLI JSON: {err}: {output}", self.name))
    }

    fn add_agent(&self, name: &str, body: &str) -> String {
        let script = self.path(&format!("agent-{name}.sh"));
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nset -eu\nps -o pgid= -p $$ | tr -d ' ' >> {}\n{body}\n",
                quote(&self.path("adapter-groups"))
            ),
        )
        .expect("shell agent fixture");
        let command = format!("/bin/sh {}", quote(&script));
        self.json(&[
            "agent",
            "add",
            name,
            "--adapter",
            "shell",
            "--command",
            &command,
            "--approval-policy",
            "auto",
            "--workspace-mode",
            "shared",
        ])["id"]
            .as_str()
            .expect("agent id")
            .into()
    }

    fn task(&self, title: &str, assignee: &str) -> String {
        self.json(&[
            "task",
            "add",
            title,
            "--max-attempts",
            "3",
            "--assignee",
            assignee,
        ])["id"]
            .as_str()
            .expect("task id")
            .into()
    }

    fn tasks(&self) -> Vec<Value> {
        read_json(self.path(".orchestry/tasks/_index.json"))
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn task_status(&self, id: &str) -> Value {
        self.tasks()
            .into_iter()
            .find(|task| task["id"] == id)
            .map(|task| task["status"].clone())
            .unwrap_or(Value::Null)
    }

    fn diagnostics(&self) -> String {
        format!(
            "{} at {}\nstate: {}\ntasks: {:?}",
            self.name,
            self.dir.path().display(),
            text(self.path(".orchestry/state.json")),
            self.tasks(),
        )
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.owner.borrow_mut().take();
        // Adapters start their own process groups; each fixture records its
        // group before doing work.
        for group in text(self.path("adapter-groups"))
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
        {
            signal_group(group, Signal::SIGKILL);
        }
    }
}

fn wait(label: &str, peers: &[&Peer], mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        if predicate() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label}\n{}",
            peers
                .iter()
                .map(|peer| peer.diagnostics())
                .collect::<Vec<_>>()
                .join("\n")
        );
        thread::sleep(POLL);
    }
}

fn port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral port")
        .local_addr()
        .unwrap()
}

/// Ignores SIGTERM, so the owner's wind-down sits in its full kill grace.
/// Publishes the PID by rename: the test waits for `started` to exist and
/// then parses it, so it must never be observed empty.
fn slow_ack_body(peer: &Peer) -> String {
    format!(
        "trap '' TERM\necho $$ > {tmp}\nmv {tmp} {}\nwhile [ ! -f {} ]; do sleep 0.04; done\necho effect >> {}",
        quote(&peer.path("started")),
        quote(&peer.path("release")),
        quote(&peer.path("effect")),
        tmp = quote(&peer.path("started.tmp")),
    )
}

#[test]
fn tui_sigterm_exits_only_after_the_owners_own_shutdown() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    struct Dashboard {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    }
    impl Drop for Dashboard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.master.take();
        }
    }

    let controller = Peer::new("race-controller");
    let agent = controller.add_agent("executor", &slow_ack_body(&controller));
    let controller_addr = port();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let fixture = controller.command();
    let mut command = CommandBuilder::new(fixture.get_program());
    for (key, value) in fixture.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command.env("TERM", "xterm-256color");
    // The dashboard probes every adapter CLI for its model list on the first
    // tick: keep the developer's own agent CLIs off its PATH.
    command.env("PATH", "/usr/bin:/bin");
    command.cwd(controller.dir.path());
    command.args(["tui", "--listen", &controller_addr.to_string()]);
    let mut dashboard = Dashboard {
        child: pair.slave.spawn_command(command).unwrap(),
        master: Some(pair.master),
    };
    drop(pair.slave);
    let mut reader = dashboard
        .master
        .as_ref()
        .unwrap()
        .try_clone_reader()
        .unwrap();
    let capture = controller.path("tui-capture");
    let mut log = File::create(&capture).unwrap();
    let reading = thread::spawn(move || {
        let _ = std::io::copy(&mut reader, &mut log);
    });

    let peers = [&controller];
    let task = controller.task("slow shutdown under TUI", &agent);
    wait("running shell under TUI", &peers, || {
        controller.path("started").exists()
    });
    let pid = text(controller.path("started"))
        .trim()
        .parse::<i32>()
        .unwrap();

    let owner = dashboard.child.process_id().unwrap();
    kill(Pid::from_raw(owner as i32), Signal::SIGTERM).unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = dashboard.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "TUI SIGTERM did not exit within 25 s: {}",
            text(&capture)
        );
        thread::sleep(POLL);
    };
    let elapsed = started.elapsed();
    // Read the controller's state at the instant the process is gone, before
    // anything else can tidy it up.
    let lock_left = controller.path(".orchestry/orchestry.lock").exists();
    let task_at_exit = controller.task_status(&task);
    eprintln!(
        "TUI SIGTERM exit in {elapsed:?}, status {status:?}, lock left {lock_left}, \
         task at exit {task_at_exit}"
    );

    assert!(status.success(), "TUI exit {status:?}: {}", text(&capture));
    assert!(
        !lock_left,
        "TUI exited after {elapsed:?} with the owner lock still on disk"
    );
    // The wind-down cannot be quick: the child ignores TERM, so the owner
    // sits in its kill grace. Exiting sooner means the dashboard left while
    // the owner was still stopping.
    assert!(
        elapsed >= MIN_WIND_DOWN,
        "TUI exited after {elapsed:?}, before the owner's wind-down could finish"
    );
    assert!(
        matches!(task_at_exit.as_str(), Some("retrying") | Some("failed")),
        "the owner left its task in {task_at_exit}"
    );
    // The grace ends with SIGKILL, so the child is gone and never ran on.
    wait("shell killed", &peers, || {
        kill(Pid::from_raw(pid), None).is_err()
    });
    fs::write(controller.path("release"), "go").unwrap();
    thread::sleep(Duration::from_millis(500));
    assert!(
        !controller.path("effect").exists(),
        "the shell produced its effect after the owner killed it"
    );

    drop(TcpListener::bind(controller_addr).expect("TUI listener released"));
    dashboard.master.take();
    reading.join().unwrap();
}
