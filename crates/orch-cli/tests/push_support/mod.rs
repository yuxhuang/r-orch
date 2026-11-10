//! Helpers for the PR 3 push acceptance tests (T3).
//!
//! The centrepiece is [`CountingProxy`], a transparent Unix-socket relay that
//! sits between a frontend and a test daemon. It lets a test observe what a
//! frontend does to the daemon *from the daemon's side of the wire*, without
//! any hook in the frontend: which operations it sends and in what order
//! across all of its connections, which connections stay open, and whether a
//! client closes its end when the daemon goes away.
//!
//! * Requests are counted **before** they are forwarded, so a caller that
//!   has just received a reply can rely on its request being in the log.
//! * When the upstream daemon closes a connection the proxy half-closes the
//!   client side (the client reads EOF) and leaves it open until the client
//!   closes its own end, which is recorded as `client_closed`. A frontend
//!   that never reads the socket while idle never closes it.
//! * [`CountingProxy::set_upstream`] retargets later connections, so a test
//!   can restart the daemon on a new socket without changing the path the
//!   frontend was told to dial.
//!
//! The proxy sits in a private `/tmp/otp-*` directory (0700, a short path:
//! the macOS limit is 104 bytes), the directory a frontend must also be told
//! is its `ORCH_TEST_SOCKET_ROOT`.
#![allow(
    dead_code,
    reason = "each push test crate uses a subset of this module"
)]

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Hang guard for every wait in this module; never a pacing delay.
pub const WAIT: Duration = Duration::from_secs(15);
/// Slice between condition checks in [`wait_until`] (an upper bound on how
/// long a satisfied condition goes unnoticed).
const SLICE: Duration = Duration::from_millis(10);

/// Poll `check` until it returns `Some`, or panic naming `what` after `limit`.
pub fn wait_until<T>(what: &str, limit: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(SLICE);
    }
}

/// Operations whose only purpose is to read daemon state. A frontend that is
/// kept up to date by push sends none of them while nothing changes.
pub const STATE_READS: &[&str] = &[
    "status.get",
    "state.get",
    "ui.bootstrap",
    "task.list",
    "task.get",
    "agent.list",
    "agent.get",
    "goal.list",
    "goal.get",
    "goal.tasks",
    "goal.progress",
    "team.list",
    "team.get",
    "team.for_agent",
    "run.list",
    "run.get",
    "run.history",
    "run.timings",
    "run.events",
    "run.events_tail",
    "message.list",
    "message.inbox",
    "context.list",
    "context.get",
    "config.get",
    "config.global.show",
    "remote.list",
    "remote.status",
    "daemon.status",
];

/// One request a client sent through the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub conn: usize,
    pub op: String,
}

/// The requests in `seen` that only read state.
pub fn state_reads(seen: &[Seen]) -> Vec<&Seen> {
    seen.iter()
        .filter(|entry| STATE_READS.contains(&entry.op.as_str()))
        .collect()
}

/// One client connection through the proxy.
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub id: usize,
    /// `hello.caller.kind` of a session hello, if one was sent.
    pub caller: Option<String>,
    /// The daemon closed (or could not be reached for) this connection.
    pub upstream_closed: bool,
    /// The client closed its end.
    pub client_closed: bool,
    /// Requests the client sent on it.
    pub requests: usize,
}

struct Shared {
    upstream: Mutex<PathBuf>,
    log: Mutex<Vec<Seen>>,
    conns: Mutex<Vec<ConnInfo>>,
    stop: AtomicBool,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A counting relay in front of a daemon socket (see the module docs).
pub struct CountingProxy {
    dir: tempfile::TempDir,
    socket: PathBuf,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl CountingProxy {
    /// Relay to the daemon socket at `upstream`.
    pub fn start(upstream: &Path) -> CountingProxy {
        let dir = tempfile::Builder::new()
            .prefix("otp-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .expect("proxy directory");
        let socket = dir.path().join("p");
        assert!(socket.as_os_str().len() <= 100, "socket path too long");
        let listener = UnixListener::bind(&socket).expect("bind the proxy socket");
        // Clients refuse a socket that is not 0600 (the daemon makes its own so).
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .expect("chmod the proxy socket");
        let shared = Arc::new(Shared {
            upstream: Mutex::new(upstream.to_path_buf()),
            log: Mutex::default(),
            conns: Mutex::default(),
            stop: AtomicBool::new(false),
        });
        let accepting = Arc::clone(&shared);
        let accept = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if accepting.stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(client) = stream else { continue };
                let shared = Arc::clone(&accepting);
                let id = {
                    let mut conns = locked(&shared.conns);
                    let id = conns.len();
                    conns.push(ConnInfo {
                        id,
                        caller: None,
                        upstream_closed: false,
                        client_closed: false,
                        requests: 0,
                    });
                    id
                };
                std::thread::spawn(move || relay(&shared, id, client));
            }
        });
        CountingProxy {
            dir,
            socket,
            shared,
            accept: Some(accept),
        }
    }

    /// The path a frontend must dial (`ORCH_SOCKET`).
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The directory holding the proxy socket (`ORCH_TEST_SOCKET_ROOT`).
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Dial `upstream` for connections accepted from now on.
    pub fn set_upstream(&self, upstream: &Path) {
        *locked(&self.shared.upstream) = upstream.to_path_buf();
    }

    /// A position in the request log, for [`CountingProxy::ops_since`].
    pub fn mark(&self) -> usize {
        locked(&self.shared.log).len()
    }

    pub fn all_ops(&self) -> Vec<Seen> {
        locked(&self.shared.log).clone()
    }

    pub fn ops_since(&self, mark: usize) -> Vec<Seen> {
        locked(&self.shared.log)[mark..].to_vec()
    }

    pub fn conns(&self) -> Vec<ConnInfo> {
        locked(&self.shared.conns).clone()
    }
}

impl Drop for CountingProxy {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Unblock `accept` so the thread can notice the flag.
        let _ = UnixStream::connect(&self.socket);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn update(shared: &Shared, id: usize, change: impl FnOnce(&mut ConnInfo)) {
    change(&mut locked(&shared.conns)[id]);
}

/// Relay one client connection until both directions end.
fn relay(shared: &Arc<Shared>, id: usize, client: UnixStream) {
    let upstream_path = locked(&shared.upstream).clone();
    let Ok(upstream) = UnixStream::connect(&upstream_path) else {
        update(shared, id, |conn| conn.upstream_closed = true);
        let _ = client.shutdown(std::net::Shutdown::Write);
        return;
    };
    let (Ok(upstream_read), Ok(client_write), Ok(client_read)) =
        (upstream.try_clone(), client.try_clone(), client.try_clone())
    else {
        return;
    };

    // Daemon -> client; on EOF half-close the client so it reads EOF.
    let down = {
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let (mut from, mut to) = (upstream_read, client_write);
            let mut buf = [0u8; 8192];
            loop {
                match from.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if to.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            update(&shared, id, |conn| conn.upstream_closed = true);
            let _ = to.shutdown(std::net::Shutdown::Write);
        })
    };

    // Client -> daemon, line by line so requests can be counted.
    let mut reader = BufReader::new(client_read);
    let mut to_daemon = upstream;
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Ok(frame) = serde_json::from_slice::<Value>(&line) {
            if let Some(kind) = frame.pointer("/hello/caller/kind").and_then(Value::as_str) {
                update(shared, id, |conn| conn.caller = Some(kind.to_owned()));
            }
            if let Some(op) = frame.pointer("/req/op").and_then(Value::as_str) {
                locked(&shared.log).push(Seen {
                    conn: id,
                    op: op.to_owned(),
                });
                update(shared, id, |conn| conn.requests += 1);
            }
        }
        if to_daemon.write_all(&line).is_err() {
            break;
        }
    }
    update(shared, id, |conn| conn.client_closed = true);
    let _ = to_daemon.shutdown(std::net::Shutdown::Write);
    let _ = down.join();
}

// ---------------------------------------------------------------------------
// A throwaway binary-level project: real `orch` processes, one explicit test
// daemon, and a counting proxy in front of it for the frontends under test
// ---------------------------------------------------------------------------

#[path = "../common/daemon.rs"]
pub mod daemon;

use std::process::{Command, Output, Stdio};

/// A project with its own `HOME`, one explicit test daemon (never the
/// owner's) and a [`CountingProxy`] in front of that daemon.
///
/// Two ways to reach the daemon, so a test can tell the frontend under test
/// from everything else it does:
///
/// * [`Project::frontend`] / [`Project::frontend_env`] dial the proxy: what
///   the frontend sends is counted;
/// * [`Project::owner`] dials the daemon directly: seeding and "external
///   mutations" by another client, invisible to the counter.
pub struct Project {
    // Declared first: dropped first, so the proxy and the daemon go before
    // the directory.
    pub proxy: CountingProxy,
    daemon: Option<daemon::TestDaemon>,
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
}

impl Project {
    /// `orch init`, start the daemon, put the proxy in front of it.
    pub fn start() -> Project {
        let dir = tempfile::Builder::new()
            .prefix("orch-push-")
            .tempdir()
            .expect("project directory");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let init = bare_command(dir.path(), &home)
            .args(["init", "--name", "proj", "--adapter", "shell"])
            .output()
            .expect("run orch init");
        assert!(
            init.status.success(),
            "orch init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let daemon = daemon::TestDaemon::start(dir.path(), &home);
        let proxy = CountingProxy::start(daemon.socket());
        Project {
            proxy,
            daemon: Some(daemon),
            dir,
            home,
        }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().expect("project root")
    }

    fn running(&self) -> &daemon::TestDaemon {
        self.daemon.as_ref().expect("the daemon is stopped")
    }

    /// An `orch` command aimed at the daemon *through the proxy*.
    pub fn frontend(&self, args: &[&str]) -> Command {
        let mut command = bare_command(&self.root(), &self.home);
        command.args(args);
        self.frontend_env(&mut command);
        command
    }

    /// Point `command` at the proxy (`ORCH_SOCKET`, `ORCH_TEST_SOCKET_ROOT`).
    pub fn frontend_env(&self, command: &mut Command) {
        daemon::apply_env(command, self.proxy.socket(), self.proxy.dir());
    }

    /// An `orch` command aimed straight at the daemon (not counted).
    pub fn owner_command(&self, args: &[&str]) -> Command {
        let mut command = bare_command(&self.root(), &self.home);
        command.args(args);
        self.running().apply(&mut command);
        command
    }

    /// Run `orch <args>` straight against the daemon; panics on failure.
    pub fn owner(&self, args: &[&str]) -> Output {
        let output = self
            .owner_command(args)
            .output()
            .unwrap_or_else(|error| panic!("run orch {args:?}: {error}"));
        assert!(
            output.status.success(),
            "orch {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// `orch task add` straight against the daemon; returns the task id.
    pub fn add_task(&self, title: &str) -> String {
        let output = self.owner(&["task", "add", title, "--quiet"]);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    pub fn daemon_pid(&self) -> u32 {
        self.running().pid()
    }

    pub fn daemon_socket(&self) -> PathBuf {
        self.running().socket().to_path_buf()
    }

    /// Stop the daemon the clean way (process table checked).
    pub fn stop_daemon(&mut self) {
        self.daemon
            .take()
            .expect("the daemon is stopped")
            .shutdown();
    }

    /// An explicit owner restart: a new daemon on a new socket; the proxy
    /// dials it from now on (frontends keep their configured path).
    pub fn start_daemon(&mut self) {
        assert!(self.daemon.is_none(), "the daemon is running");
        let daemon = daemon::TestDaemon::start(self.dir.path(), &self.home);
        self.proxy.set_upstream(daemon.socket());
        self.daemon = Some(daemon);
    }
}

/// An `orch` command in `cwd` with a cleared environment, a private `HOME`
/// and a `PATH` that holds no agent CLI. No daemon variables.
pub fn bare_command(cwd: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
    command
        .env_clear()
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("NO_COLOR", "1")
        .env("TERM", "xterm-256color")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", home.join("global.yml"))
        .env("ORCH_PROJECT_ROOT", cwd)
        .stdin(Stdio::null());
    command
}

// ---------------------------------------------------------------------------
// A long-lived child with an output reader (orch web, orch mcp)
// ---------------------------------------------------------------------------

use std::os::unix::process::CommandExt;
use std::process::Child;
use std::sync::mpsc::{self, Receiver};

/// A spawned process in its own process group, killed (group and all) on drop.
pub struct Guarded {
    pub child: Child,
}

impl Guarded {
    pub fn spawn(command: &mut Command) -> Guarded {
        command.process_group(0);
        Guarded {
            child: command.spawn().expect("spawn"),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn kill(&mut self) {
        if let Ok(pgid) = i32::try_from(self.child.id()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pgid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Lines of a child's stdout, delivered by a reader thread.
pub fn stdout_lines(child: &mut Child) -> Receiver<String> {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

/// A minimal black-box MCP client over `orch mcp`'s stdio that, unlike the
/// request/response harness in `mcp_common`, keeps server notifications.
pub struct McpSession {
    guard: Guarded,
    stdin: Option<std::process::ChildStdin>,
    lines: Receiver<String>,
    notifications: std::collections::VecDeque<Value>,
    next_id: u64,
    /// The whole `initialize` response, once [`McpSession::initialize`] ran.
    pub initialize_result: Value,
}

/// Deadline for a single MCP read.
pub const MCP_WAIT: Duration = Duration::from_secs(10);

impl McpSession {
    /// Spawn `orch mcp` (extra `args` after it) with `command`'s environment.
    pub fn spawn(mut command: Command) -> McpSession {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut guard = Guarded::spawn(&mut command);
        let stdin = guard.child.stdin.take();
        let lines = stdout_lines(&mut guard.child);
        McpSession {
            guard,
            stdin,
            lines,
            notifications: Default::default(),
            next_id: 1,
            initialize_result: Value::Null,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        let mut line = serde_json::to_vec(message).unwrap();
        line.push(b'\n');
        stdin.write_all(&line).expect("write to orch mcp");
        stdin.flush().expect("flush");
    }

    fn read_message(&mut self, limit: Duration, what: &str) -> Value {
        let line = self
            .lines
            .recv_timeout(limit)
            .unwrap_or_else(|_| panic!("timed out after {limit:?} waiting for {what}"));
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("non-JSON stdout line: {line}"))
    }

    /// Send a request and return the whole response object. Notifications
    /// that arrive first are kept for [`McpSession::notification`].
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(
            &serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        loop {
            let message = self.read_message(MCP_WAIT, &format!("the response to {method}"));
            if message.get("method").is_some() && message.get("id").is_none() {
                self.notifications.push_back(message);
            } else if message["id"] == id {
                return message;
            }
        }
    }

    pub fn initialize(&mut self) -> Value {
        let response = self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "orch-push-test", "version": "0.0.0"},
            }),
        );
        self.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}));
        self.initialize_result = response.clone();
        response
    }

    /// The `uri` of the next `notifications/resources/updated`, within `limit`.
    pub fn updated_uri(&mut self, limit: Duration, what: &str) -> String {
        loop {
            let message = match self.notifications.pop_front() {
                Some(message) => message,
                None => self.read_message(limit, what),
            };
            if message["method"] == "notifications/resources/updated" {
                return message["params"]["uri"]
                    .as_str()
                    .unwrap_or_else(|| panic!("updated without a uri: {message}"))
                    .to_owned();
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.guard.pid()
    }
}

// ---------------------------------------------------------------------------
// A tmux pane running a frontend (the repo's TUI end-to-end standard)
// ---------------------------------------------------------------------------

/// A private tmux server with one session running `orch <args>` against the
/// project's proxy. Killed on drop.
pub struct Pane {
    bin: PathBuf,
    socket: String,
    home: PathBuf,
    socket_path: Option<PathBuf>,
}

fn tmux_binary() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("tmux"))
        .find(|candidate| candidate.is_file())
        .expect("tmux is required for the TUI end-to-end tests and was not found on PATH")
}

impl Pane {
    /// Run `orch <args>` (for example `"tui"`) in a 120x40 pane whose
    /// `ORCH_SOCKET` is the project's proxy. `name` must be unique per test.
    pub fn launch(project: &Project, args: &str, name: &str) -> Pane {
        let mut pane = Pane {
            bin: tmux_binary(),
            socket: format!("orch-push-{}-{name}", std::process::id()),
            home: project.home.clone(),
            socket_path: None,
        };
        let root = project.root();
        let env = [
            format!("HOME={}", project.home.display()),
            format!(
                "ORCHESTRY_GLOBAL_CONFIG_PATH={}",
                project.home.join("global.yml").display()
            ),
            "PATH=/usr/bin:/bin".to_owned(),
            format!("ORCH_PROJECT_ROOT={}", root.display()),
            format!("ORCH_SOCKET={}", project.proxy.socket().display()),
            format!("ORCH_TEST_SOCKET_ROOT={}", project.proxy.dir().display()),
        ];
        let mut tmux_args: Vec<String> = [
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
            &root.display().to_string(),
        ]
        .map(String::from)
        .into();
        for pair in env {
            tmux_args.extend(["-e".to_owned(), pair]);
        }
        tmux_args.push(format!("'{}' {args}", env!("CARGO_BIN_EXE_orch")));
        pane.run(&tmux_args.iter().map(String::as_str).collect::<Vec<_>>());
        let path = pane.run(&["display-message", "-p", "-t", "dash", "#{socket_path}"]);
        pane.socket_path = Some(PathBuf::from(
            String::from_utf8_lossy(&path.stdout).trim().to_owned(),
        ));
        pane
    }

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

    pub fn screen(&self) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-p", "-t", "dash"]).stdout)
            .into_owned()
    }

    /// Poll the screen (reads only; never writes to the pane) until `ready`
    /// holds, or panic with the screen after `limit`.
    pub fn wait_for_within(
        &self,
        limit: Duration,
        what: &str,
        ready: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + limit;
        loop {
            let screen = self.screen();
            if ready(&screen) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {limit:?} waiting for {what}; screen:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_for(&self, what: &str, ready: impl Fn(&str) -> bool) -> String {
        self.wait_for_within(WAIT, what, ready)
    }

    /// Let `period` of real time pass while watching the pane (reads only).
    /// For the tests that must observe "nothing happens for a while".
    pub fn watch_for(&self, period: Duration) {
        let until = Instant::now() + period;
        while Instant::now() < until {
            let _ = self.screen();
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    pub fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "dash"];
        args.extend_from_slice(keys);
        self.run(&args);
    }
}

impl Drop for Pane {
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
