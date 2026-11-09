//! Drivers for every frontend kind (T2a): CLI, TUI (real `orch tui` in a
//! private tmux), `orch web`, and `orch mcp` over stdio, all against a
//! [`Scene`] that fixes what the daemon looks like (absent, stale socket, or a
//! live daemon of a different protocol version).
//!
//! Hermeticity is inherited from [`orch_command`]: cleared environment,
//! private `HOME` and global config, a short socket path under a 0700 `/tmp`
//! dir. Nothing here touches a real project, a real daemon or a real AI CLI.
//! Every spawned child is in its own process group and is killed by `Drop`.
#![allow(dead_code, reason = "each daemon_* test crate uses a subset")]

use super::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// What the project's daemon looks like to the client under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    /// Nothing at the socket path.
    NoSocket,
    /// A socket inode a crashed owner left behind: connecting is refused.
    StaleSocket,
    /// A live daemon whose protocol is one ahead of this binary's.
    Skew,
}

struct HarnessKeep {
    runtime: tokio::runtime::Runtime,
    daemon: Option<orch_daemon::testing::DaemonHarness>,
}

enum Keep {
    Sandbox(Sandbox),
    Harness(Box<HarnessKeep>),
}

/// One isolated project + endpoint in a given [`Cond`].
pub struct Scene {
    pub root: PathBuf,
    pub socket: PathBuf,
    pub test_root: PathBuf,
    pub cond: Cond,
    keep: Keep,
}

impl Scene {
    pub fn new(cond: Cond) -> Scene {
        match cond {
            Cond::NoSocket | Cond::StaleSocket => {
                let sandbox = Sandbox::new();
                sandbox.init_project();
                if cond == Cond::StaleSocket {
                    drop(UnixListener::bind(sandbox.socket()).unwrap());
                    assert!(sandbox.socket().exists());
                }
                Scene {
                    root: sandbox.root(),
                    socket: sandbox.socket(),
                    test_root: sandbox.short_dir().to_path_buf(),
                    cond,
                    keep: Keep::Sandbox(sandbox),
                }
            }
            Cond::Skew => {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                let daemon = runtime
                    .block_on(
                        orch_daemon::testing::DaemonHarness::builder()
                            .on_socket()
                            .versions(orch_proto::LOCAL_PROTOCOL + 1, orch_proto::BINARY_VERSION)
                            .start(),
                    )
                    .unwrap();
                let root = daemon.project_root().to_path_buf();
                let socket = daemon.socket().unwrap().to_path_buf();
                let test_root = socket.parent().unwrap().to_path_buf();
                Scene {
                    root,
                    socket,
                    test_root,
                    cond,
                    keep: Keep::Harness(Box::new(HarnessKeep {
                        runtime,
                        daemon: Some(daemon),
                    })),
                }
            }
        }
    }

    pub fn sandbox(&self) -> &Sandbox {
        match &self.keep {
            Keep::Sandbox(sandbox) => sandbox,
            Keep::Harness(_) => panic!("a skew scene has no sandbox"),
        }
    }

    /// An `orch` command aimed at this scene.
    pub fn orch(&self) -> Command {
        orch_command(&self.root, &self.socket, &self.test_root)
    }

    /// The fragments every frontend's failure text must contain, whitespace
    /// normalised. `agent` selects the agent-context variant.
    pub fn expected_fragments(&self, agent: bool) -> Vec<String> {
        let root = self.root.display();
        if agent {
            return vec![format!(
                "The orch daemon for {root} is not running. Do not start it; \
                 report this to the owner and stop."
            )];
        }
        match self.cond {
            Cond::NoSocket | Cond::StaleSocket => vec![
                format!("No orch daemon is running for {root} ("),
                "Start it with: orch daemon start".to_owned(),
            ],
            Cond::Skew => vec![
                format!(
                    "No orch daemon is running for {root} (daemon is orch {bin} protocol {theirs}, \
                     this client is orch {bin} protocol {ours})",
                    bin = orch_proto::BINARY_VERSION,
                    theirs = orch_proto::LOCAL_PROTOCOL + 1,
                    ours = orch_proto::LOCAL_PROTOCOL,
                ),
                "Start it with: orch daemon stop, then orch daemon start".to_owned(),
            ],
        }
    }

    /// `text` is the uniform failure message of this scene.
    pub fn assert_uniform(&self, who: &str, text: &str, agent: bool) {
        let seen = normalize(text);
        for fragment in self.expected_fragments(agent) {
            assert!(
                seen.contains(&normalize(&fragment)),
                "{who}: missing {fragment:?} in:\n{text}"
            );
        }
        if agent {
            assert!(
                !seen.contains("Start it with") && !seen.contains("orch daemon start"),
                "{who}: an agent was told how to start the daemon:\n{text}"
            );
        }
    }

    /// The project tree and endpoint, to prove a failed client changed nothing.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            project: tree(&self.root),
            sockets: harness_free(tree(self.socket.parent().unwrap())),
            endpoint: endpoint_identity(&self.socket),
        }
    }

    pub fn assert_untouched(&self, who: &str, before: &Snapshot) {
        let after = self.snapshot();
        assert_eq!(
            after.endpoint, before.endpoint,
            "{who} touched the endpoint"
        );
        assert_eq!(
            after.project, before.project,
            "{who} changed the project tree (a direct-store write or a repair)"
        );
        assert_eq!(
            after.sockets, before.sockets,
            "{who} changed the socket directory"
        );
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        if let Keep::Harness(keep) = &mut self.keep
            && let Some(daemon) = keep.daemon.take()
        {
            let _ = keep.runtime.block_on(daemon.shutdown());
        }
    }
}

/// The TUI pane fixture (`TuiPane`) writes its own wrapper, exit file and tmux
/// server socket into the socket directory; those are the harness's, not the
/// client's, so they are not part of "the client changed nothing".
fn harness_free(mut tree: BTreeMap<String, (bool, u64)>) -> BTreeMap<String, (bool, u64)> {
    tree.retain(|name, _| {
        !(name == "pane.sh" || name.starts_with("tui.exit") || name.starts_with("tmux-"))
    });
    tree
}

pub struct Snapshot {
    project: BTreeMap<String, (bool, u64)>,
    sockets: BTreeMap<String, (bool, u64)>,
    endpoint: Option<(u64, u32, u64, i64)>,
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `(relative path → (is_dir, size))`; mtimes excluded on purpose.
pub fn tree(root: &Path) -> BTreeMap<String, (bool, u64)> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (bool, u64)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.insert(
                rel,
                (meta.is_dir(), if meta.is_dir() { 0 } else { meta.len() }),
            );
            if meta.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

pub fn endpoint_identity(socket: &Path) -> Option<(u64, u32, u64, i64)> {
    let meta = fs::symlink_metadata(socket).ok()?;
    Some((meta.ino(), meta.mode(), meta.len(), meta.mtime()))
}

// ── the no-spawn proof ─────────────────────────────────────────────────────

/// The command line plus environment of a live process (`ps` can show both
/// for the caller's own processes); `None` once it has exited.
fn process_env_line(pid: u32) -> Option<String> {
    let flag = if cfg!(target_os = "macos") { "-E" } else { "e" };
    let out = std::process::Command::new("/bin/ps")
        .args([flag, "-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    out.status.success().then(|| text(&out.stdout))
}

/// A process-table baseline taken before a client runs.
pub struct Watch {
    known: HashSet<u32>,
}

impl Watch {
    pub fn begin() -> Watch {
        Watch {
            known: process_table().iter().map(|p| p.pid).collect(),
        }
    }

    /// New `orch` processes right now (the binary under test, by exact path).
    pub fn new_orch(&self) -> Vec<Proc> {
        orch_processes(&process_table())
            .into_iter()
            .filter(|p| !self.known.contains(&p.pid))
            .collect()
    }

    /// [`Watch::new_orch`] restricted to processes carrying this sandbox's
    /// `ORCH_SOCKET` in their environment, for tests that run beside siblings
    /// in one binary (a sibling's daemon or `init` is not this test's spawn).
    pub fn new_orch_of(&self, sandbox: &Sandbox) -> Vec<Proc> {
        let marker = format!("ORCH_SOCKET={}", sandbox.socket().display());
        self.new_orch()
            .into_iter()
            .filter(|p| process_env_line(p.pid).is_some_and(|env| env.contains(&marker)))
            .collect()
    }

    /// While a client is alive, the only new `orch` process is the client
    /// itself: `<exe> <subcommand>` with the given pid(s).
    pub fn assert_only_clients(&self, who: &str, clients: &[u32]) {
        let offenders: Vec<_> = self
            .new_orch()
            .into_iter()
            .filter(|p| !clients.contains(&p.pid))
            .collect();
        assert!(
            offenders.is_empty(),
            "{who} spawned an orch process: {offenders:?}"
        );
    }

    /// After every client has exited, nothing new remains: not an orch
    /// process, and no process (other than tmux scaffolding) that mentions
    /// the project or the socket directory.
    pub fn assert_nothing_left(&self, who: &str, scene: &Scene) {
        let table = process_table();
        let exe = env!("CARGO_BIN_EXE_orch");
        let markers = [
            scene.root.to_string_lossy().into_owned(),
            scene.test_root.to_string_lossy().into_owned(),
        ];
        let left: Vec<_> = table
            .iter()
            .filter(|p| !self.known.contains(&p.pid))
            .filter(|p| {
                p.command == exe
                    || p.command.starts_with(&format!("{exe} "))
                    || (markers.iter().any(|m| p.command.contains(m.as_str()))
                        && !p.command.starts_with("tmux")
                        && !p.command.contains("/tmux ")
                        && !p.command.contains("pane.sh")
                        && !p.command.starts_with("sleep"))
            })
            .collect();
        assert!(left.is_empty(), "{who} left a process behind: {left:?}");
    }
}

// ── CLI ────────────────────────────────────────────────────────────────────

pub struct CliRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub pid: u32,
}

pub fn run_cli(scene: &Scene, args: &[&str], env: &[(&str, &str)]) -> CliRun {
    let mut cmd = scene.orch();
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let (out, pid) = output_and_pid(cmd, CALL);
    CliRun {
        code: out.status.code(),
        stdout: text(&out.stdout),
        stderr: text(&out.stderr),
        pid,
    }
}

// ── web ────────────────────────────────────────────────────────────────────

/// `orch web --no-auth` on an OS-assigned loopback port, run to completion.
pub fn run_web_preflight(scene: &Scene) -> CliRun {
    run_cli(scene, &["web", "--no-auth", "--bind", "127.0.0.1:0"], &[])
}

/// One HTTP/1.1 GET with a deadline. Returns `(status, body)`.
pub fn http_get(addr: &str, path: &str) -> std::io::Result<(u16, String)> {
    use std::io::Read;
    use std::net::TcpStream;
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(CALL))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    Ok((status, body))
}

pub fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// ── MCP over stdio ─────────────────────────────────────────────────────────

/// A minimal hand-rolled JSON-RPC client for `orch mcp`: black box on stdout.
pub struct Mcp {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    lines: Receiver<String>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    next_id: u64,
    pub pid: u32,
}

pub const MCP_PROTOCOL: &str = "2025-11-25";

impl Mcp {
    pub fn spawn(mut cmd: Command, extra_args: &[&str]) -> Mcp {
        cmd.arg("mcp").args(extra_args);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `orch mcp`");
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            while let Ok(read) = std::io::Read::read(&mut stderr_pipe, &mut buf) {
                if read == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..read]));
            }
        });
        Mcp {
            child,
            stdin,
            lines,
            stderr,
            next_id: 1,
            pid,
        }
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    fn write(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{message}").expect("write to orch mcp");
        stdin.flush().expect("flush to orch mcp");
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Send a request and wait (bounded) for the response with its id.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + CALL;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default();
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|err| panic!("no response to {method}: {err}; {}", self.stderr()));
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                panic!("non-JSON on the MCP stdout: {line}");
            };
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    pub fn initialize(&mut self) -> Value {
        self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "t2a-matrix", "version": "0.0.0"},
            }),
        )
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    /// Close stdin (EOF) and wait for the exit status under a deadline.
    pub fn finish(&mut self) -> Option<i32> {
        self.stdin = None;
        wait_for(CALL, "orch mcp to exit on EOF", || {
            self.child.try_wait().unwrap()
        })
        .code()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if let Ok(pid) = i32::try_from(self.pid) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = self.child.wait();
    }
}

/// The failure carried by an MCP response: `(text, whole response)`. A
/// JSON-RPC error and a tool result with `isError` are both failures; a
/// success is a test failure.
pub fn mcp_failure(who: &str, response: &Value) -> String {
    if response.get("error").is_some() {
        let error = &response["error"];
        return format!(
            "{} {}",
            error["message"].as_str().unwrap_or(""),
            error.get("data").map(Value::to_string).unwrap_or_default()
        );
    }
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "{who}: expected a failure, got {response}"
    );
    response["result"]["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The failure must say exit code 7 somewhere structured (error data or
/// structured content), not only in prose.
pub fn assert_mcp_exit_code_7(who: &str, response: &Value) {
    let flat = response.to_string().replace(' ', "");
    assert!(
        flat.contains("\"exit_code\":7"),
        "{who}: the MCP failure does not carry exit_code 7: {response}"
    );
}

// ── TUI in a private tmux ──────────────────────────────────────────────────

pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A private tmux server with `orch <args>` (e.g. `tui`) in one pane. The
/// pane records the client's exit status in a file and stays open so the last
/// screen can be read. Killed on drop.
pub struct TuiPane {
    name: String,
    dir: PathBuf,
    exit_file: PathBuf,
}

impl TuiPane {
    pub fn launch(scene: &Scene, args: &str, extra_env: &[(&str, &str)]) -> TuiPane {
        Self::launch_at(
            &scene.root,
            &scene.socket,
            &scene.test_root,
            args,
            extra_env,
        )
    }

    /// Like [`TuiPane::launch`] for a project that is not a [`Scene`] (a
    /// running daemon's sandbox).
    pub fn launch_at(
        root: &Path,
        socket: &Path,
        test_root: &Path,
        args: &str,
        extra_env: &[(&str, &str)],
    ) -> TuiPane {
        assert!(
            tmux_available(),
            "tmux is required for the TUI end-to-end tests and was not found on PATH \
             (macOS: brew install tmux; Linux: the distro's tmux package)"
        );
        let dir = test_root.to_path_buf();
        let name = format!("t2a{}", std::process::id());
        let exit_file = dir.join("tui.exit");
        let wrapper = dir.join("pane.sh");
        let extra: String = extra_env
            .iter()
            .map(|(k, v)| format!(" {k}='{v}'"))
            .collect();
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ncd '{root}'\nenv -i HOME='{root}/home' PATH=/usr/bin:/bin NO_COLOR=1 \
                 LANG=en_US.UTF-8 TERM=tmux-256color \
                 ORCHESTRY_GLOBAL_CONFIG_PATH='{root}/global.yml' ORCH_PROJECT_ROOT='{root}' \
                 ORCH_SOCKET='{sock}' ORCH_TEST_SOCKET_ROOT='{dir}'{extra} \
                 '{exe}' {args}\nstatus=$?\necho $status > '{exit}.tmp' && mv '{exit}.tmp' '{exit}'\n\
                 exec sleep 600\n",
                root = root.display(),
                sock = socket.display(),
                dir = dir.display(),
                exe = env!("CARGO_BIN_EXE_orch"),
                exit = exit_file.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(
            &wrapper,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let pane = TuiPane {
            name,
            dir,
            exit_file,
        };
        pane.run(&[
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
            "t",
            "-x",
            "200",
            "-y",
            "50",
            wrapper.to_str().unwrap(),
        ]);
        pane
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
            .env("TMUX_TMPDIR", &self.dir)
            .env("HOME", &self.dir)
            .env("SHELL", "/bin/sh")
            .args(["-u", "-L", &self.name, "-f", "/dev/null"]);
        cmd
    }

    fn run(&self, args: &[&str]) -> String {
        let mut cmd = self.cmd();
        cmd.args(args);
        let out = output_with_deadline(cmd, CALL);
        assert!(out.status.success(), "tmux {args:?}: {}", text(&out.stderr));
        text(&out.stdout)
    }

    /// The pane's current text with wrapped lines joined.
    pub fn screen(&self) -> String {
        self.run(&["capture-pane", "-p", "-J", "-t", "t"])
    }

    pub fn wait_screen(&self, what: &str, ready: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + READY;
        loop {
            let screen = self.screen();
            if ready(&screen) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; screen:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "t"];
        args.extend_from_slice(keys);
        self.run(&args);
    }

    pub fn type_text(&self, text: &str) {
        self.run(&["send-keys", "-t", "t", "-l", text]);
    }

    /// The pid of the client in the pane (the child of the wrapper shell).
    pub fn client_pids(&self) -> Vec<u32> {
        let exe = env!("CARGO_BIN_EXE_orch");
        orch_processes(&process_table())
            .into_iter()
            .filter(|p| p.command.starts_with(exe))
            .map(|p| p.pid)
            .collect()
    }

    /// The client's exit status, once it has exited.
    pub fn exit_code(&self) -> i32 {
        wait_for(READY, "the pane's client to exit", || {
            fs::read_to_string(&self.exit_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
        })
    }

    pub fn has_exited(&self) -> bool {
        self.exit_file.exists()
    }
}

impl Drop for TuiPane {
    fn drop(&mut self) {
        let _ = self.cmd().arg("kill-server").output();
    }
}
