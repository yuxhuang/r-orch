//! A hand-rolled, black-box MCP client for `orch mcp`.
//!
//! It speaks newline-delimited JSON-RPC 2.0 over the child's stdio with
//! `serde_json` and `std::process` only, deliberately *not* the `rmcp` crate the
//! server is built on: a test that shares the server's protocol library would
//! agree with its bugs. Every read has a deadline, so a hung server fails the
//! test instead of blocking CI.

#![allow(dead_code, reason = "each `mcp_*` test crate uses a different subset")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Deadline for every single read from, and wait on, the server.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// How long a whole `orch mcp` process may take to exit after EOF or a signal.
/// Distinct from [`TIMEOUT`]: shutdown drains the SDK (design D16, D21 bound).
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// The MCP revision the harness announces in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// The previous revision, which the server must keep accepting (design D21).
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-06-18";

/// Deadline for a one-shot CLI invocation the fixtures make (`orch init`,
/// `orch --json task add`, `mkfifo`, ...). Generous: they share the machine
/// with every other test thread, but finite, so a CLI regression fails a test
/// instead of hanging CI.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

// ── bounded blocking operations ───────────────────────────────────────────
//
// Nothing a fixture does may block without a deadline: a child that stops
// reading its stdin, a CLI that never returns, a peer thread that panicked
// before a rendezvous. Each helper below turns "waits forever" into a panic
// that says what it was waiting for.

/// SIGKILL the whole process group led by `leader`. Used only on failure or
/// timeout paths, before the leader is reaped, so the pgid cannot have been
/// recycled; children are spawned with their own group (`process_group(0)`).
#[cfg(unix)]
fn kill_group(leader: u32) {
    if let Ok(pid) = i32::try_from(leader) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

/// Put the child in a process group of its own, so a timeout can kill it and
/// every descendant it started.
#[cfg(unix)]
pub fn own_process_group(command: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt;
    command.process_group(0)
}

/// Kill `child` and its whole process group, then reap it.
pub fn kill_and_reap(child: &mut Child) {
    #[cfg(unix)]
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// A background thread whose completion can be awaited with a deadline.
pub struct Worker {
    handle: JoinHandle<()>,
    done: Receiver<()>,
}

impl Worker {
    pub fn spawn(work: impl FnOnce() + Send + 'static) -> Self {
        let (finished, done) = mpsc::channel();
        let handle = thread::spawn(move || {
            work();
            let _ = finished.send(());
        });
        Self { handle, done }
    }

    /// Whether the thread finished within `limit`. A thread that did not is
    /// left detached rather than joined, because joining is what blocks.
    pub fn finish_within(self, limit: Duration) -> bool {
        match self.done.recv_timeout(limit) {
            Err(RecvTimeoutError::Timeout) => false,
            // Finished, or died by panicking; either way it is over.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = self.handle.join();
                true
            }
        }
    }
}

/// A thread that returns a value, joined with a deadline. A panic in the
/// thread is re-raised in the joiner, like `JoinHandle::join().unwrap()`.
pub struct Bounded<T> {
    result: Receiver<thread::Result<T>>,
    what: String,
}

/// Run `work` on a thread; [`Bounded::join`] collects it within a limit.
pub fn spawn_bounded<T: Send + 'static>(
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Bounded<T> {
    let (sender, result) = mpsc::channel();
    thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        let _ = sender.send(outcome);
    });
    Bounded {
        result,
        what: what.to_owned(),
    }
}

impl<T> Bounded<T> {
    pub fn join(self, limit: Duration) -> T {
        match self.result.recv_timeout(limit) {
            Ok(Ok(value)) => value,
            Ok(Err(payload)) => std::panic::resume_unwind(payload),
            Err(_) => panic!("{} did not finish within {limit:?}", self.what),
        }
    }
}

/// A rendezvous of `parties` threads that a failing participant can cancel, so
/// its peers fail fast instead of waiting forever (`std::sync::Barrier` cannot
/// be cancelled). Every wait also has a deadline.
pub struct Rendezvous {
    parties: usize,
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Rendezvous {
    pub fn new(parties: usize) -> Arc<Self> {
        Arc::new(Self {
            parties,
            state: Mutex::new((0, false)),
            changed: Condvar::new(),
        })
    }

    /// A guard to hold for the participant's whole life: if it unwinds before
    /// (or without) arriving, the rendezvous is cancelled for everyone.
    pub fn participant(self: &Arc<Self>) -> Participant {
        Participant {
            rendezvous: Arc::clone(self),
        }
    }

    /// Arrive and wait for every party. `Err` when cancelled or timed out.
    pub fn arrive(&self, limit: Duration) -> Result<(), &'static str> {
        let mut state = self.state.lock().expect("rendezvous lock");
        state.0 += 1;
        self.changed.notify_all();
        let deadline = Instant::now() + limit;
        while state.0 < self.parties {
            if state.1 {
                return Err("the rendezvous was cancelled by a failed peer");
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("the rendezvous timed out waiting for a peer");
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .expect("rendezvous wait")
                .0;
        }
        Ok(())
    }

    fn cancel(&self) {
        self.state.lock().expect("rendezvous lock").1 = true;
        self.changed.notify_all();
    }
}

/// See [`Rendezvous::participant`].
pub struct Participant {
    rendezvous: Arc<Rendezvous>,
}

impl Drop for Participant {
    fn drop(&mut self) {
        if thread::panicking() {
            self.rendezvous.cancel();
        }
    }
}

/// Run `command` to completion within `limit` and capture its output, like
/// `Command::output` but bounded: the child leads its own process group and,
/// on a timeout, that whole group is killed and reaped before this panics.
pub fn output_within(command: &mut Command, limit: Duration) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    own_process_group(command);
    let described = format!("{command:?}");
    let mut child = command.spawn().expect("spawn a fixture command");
    let collect = |mut pipe: Box<dyn Read + Send>| {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&bytes);
        let worker = Worker::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                sink.lock()
                    .expect("capture lock")
                    .extend_from_slice(&chunk[..read]);
            }
        });
        (bytes, worker)
    };
    let (stdout, stdout_worker) = collect(Box::new(child.stdout.take().expect("piped stdout")));
    let (stderr, stderr_worker) = collect(Box::new(child.stderr.take().expect("piped stderr")));

    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll a fixture command") {
            break status;
        }
        if Instant::now() >= deadline {
            kill_and_reap(&mut child);
            panic!("{described} did not finish within {limit:?}; its process group was killed");
        }
        thread::sleep(Duration::from_millis(5));
    };
    // The pipes close with the command; if a descendant still holds them, it
    // outlived its parent: kill the group (it still has members) and fail.
    let drained = stdout_worker.finish_within(TIMEOUT) & stderr_worker.finish_within(TIMEOUT);
    if !drained {
        #[cfg(unix)]
        kill_group(child.id());
        panic!("{described} exited but left a descendant holding its output pipes");
    }
    let take = |bytes: Arc<Mutex<Vec<u8>>>| std::mem::take(&mut *bytes.lock().expect("capture"));
    Output {
        status,
        stdout: take(stdout),
        stderr: take(stderr),
    }
}

/// [`output_within`] with the fixtures' standard [`CLI_TIMEOUT`].
pub fn output_of(command: &mut Command) -> Output {
    output_within(command, CLI_TIMEOUT)
}

/// Tool names the default mode must list, sorted, no more and no fewer
/// (design-v2 §9, "Exact EXPECTED list for mcp_stdio.rs"). This is an
/// independent oracle: it is never generated from production registration.
pub const EXPECTED: &[&str] = &[
    "agent_add",
    "agent_autonomous",
    "agent_disable",
    "agent_enable",
    "agent_list",
    "agent_patch",
    "agent_show",
    "agent_update",
    "config_get",
    "config_patch",
    "config_set",
    "config_update",
    "context_get",
    "context_list",
    "context_set",
    "doctor",
    "goal_add",
    "goal_list",
    "goal_show",
    "goal_status",
    "goal_update",
    "logs",
    "msg_broadcast",
    "msg_inbox",
    "msg_list",
    "msg_send",
    "remote_list",
    "remote_status",
    "status",
    "task_add",
    "task_approve",
    "task_assign",
    "task_list",
    "task_patch",
    "task_reject",
    "task_retry",
    "task_show",
    "task_update",
    "team_add_task",
    "team_create",
    "team_join",
    "team_leave",
    "team_list",
    "team_set_lead",
    "team_show",
];

/// The five tools only `--allow-destructive` adds.
pub const DESTRUCTIVE: &[&str] = &[
    "agent_remove",
    "context_delete",
    "goal_delete",
    "task_cancel",
    "team_disband",
];

/// What `--read-only` lists.
pub const READ_ONLY: &[&str] = &[
    "agent_list",
    "agent_show",
    "config_get",
    "context_get",
    "context_list",
    "doctor",
    "goal_list",
    "goal_show",
    "logs",
    "msg_inbox",
    "msg_list",
    "remote_list",
    "remote_status",
    "status",
    "task_list",
    "task_show",
    "team_list",
    "team_show",
];

/// A directory with no `.orchestry` anywhere above it, which is what makes the
/// exit-3 assertion mean anything — the project-root lookup walks up.
pub fn empty_project() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("orch-mcp-")
        .tempdir()
        .expect("tempdir");
    assert!(
        !dir.path()
            .ancestors()
            .any(|ancestor| ancestor.join(".orchestry").is_dir()),
        "the tmpdir has an .orchestry ancestor; point TMPDIR outside any ORCH project"
    );
    dir
}

/// `orch init` in a fresh directory, so the initialized-project cases have
/// something to run against.
pub fn initialized_project() -> tempfile::TempDir {
    initialized_project_named("p")
}

/// [`initialized_project`] with a chosen project name, to tell two apart.
pub fn initialized_project_named(name: &str) -> tempfile::TempDir {
    let dir = empty_project();
    let output =
        output_of(orch_command(dir.path()).args(["init", "--name", name, "--adapter", "shell"]));
    assert!(
        output.status.success(),
        "`orch init` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    dir
}

/// The `orch` binary, isolated from the developer's machine: a private `HOME`,
/// a private global config (set per child, never in this process) and a `PATH`
/// with no agent CLIs on it (so nothing real is ever exec'd).
pub fn orch_command(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
    command
        .current_dir(cwd)
        .env("HOME", cwd.join(".test-home"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            cwd.join(".test-home").join("global.yml"),
        )
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("ORCHESTRY_DEBUG")
        .env_remove("ORCH_AGENT_ID");
    command
}

/// How the server process ended, and everything it wrote on the way.
#[derive(Debug)]
pub struct Exit {
    pub status: ExitStatus,
    /// Every stdout line the server wrote, in order, whether or not a request
    /// was waiting for it.
    pub stdout_lines: Vec<String>,
    pub stderr: String,
}

/// A running `orch mcp` and the channels reading its output.
pub struct McpServer {
    child: Child,
    /// Lines for the writer thread; `None` once stdin is closed. Writes never
    /// run on the test thread, so a server that stops reading cannot block it.
    stdin: Option<Sender<Vec<u8>>>,
    /// Why the writer thread gave up, if it did (a broken pipe: the server is gone).
    write_failure: Arc<Mutex<Option<String>>>,
    frames: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    /// The two output readers and the stdin writer.
    threads: Vec<Worker>,
    seen: Vec<String>,
    /// Responses that arrived while another id was being waited for.
    pending: HashMap<u64, Value>,
    next_id: u64,
}

impl McpServer {
    /// Spawn `orch mcp` with `cwd` as the working directory.
    pub fn spawn(cwd: &Path) -> Self {
        Self::spawn_with_args(cwd, &[])
    }

    pub fn spawn_with_args(cwd: &Path, extra_args: &[&str]) -> Self {
        Self::spawn_with(cwd, extra_args, &[])
    }

    /// Spawn with extra environment for the child only. `None` removes a
    /// variable; `PATH` may be overridden to put fake adapters first.
    pub fn spawn_with(cwd: &Path, extra_args: &[&str], envs: &[(&str, Option<&str>)]) -> Self {
        let mut command = orch_command(cwd);
        command.arg("mcp").args(extra_args);
        for (key, value) in envs {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        // Its own process group, so a failed test can kill the server and
        // everything it started (a probe, an adapter) in one go.
        #[cfg(unix)]
        own_process_group(&mut command);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `orch mcp`");

        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");

        // Reads happen on threads so the test thread can put a deadline on them.
        let (sender, frames) = mpsc::channel();
        let stdout_reader = Worker::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let stderr_reader = Worker::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = stderr_pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                sink.lock()
                    .expect("stderr lock")
                    .push_str(&String::from_utf8_lossy(&chunk[..read]));
            }
        });

        // Writes happen on a thread too: a full pipe blocks the writer, never
        // the test, and a dead server surfaces as a recorded failure.
        let (lines, queued) = mpsc::channel::<Vec<u8>>();
        let write_failure = Arc::new(Mutex::new(None));
        let failure = Arc::clone(&write_failure);
        let writer = Worker::spawn(move || {
            while let Ok(bytes) = queued.recv() {
                if let Err(err) = stdin.write_all(&bytes).and_then(|()| stdin.flush()) {
                    *failure.lock().expect("write failure lock") = Some(err.to_string());
                    break;
                }
            }
            // Dropping `stdin` here is the EOF the server sees.
        });

        Self {
            child,
            stdin: Some(lines),
            write_failure,
            frames,
            stderr,
            threads: vec![stdout_reader, stderr_reader, writer],
            seen: Vec::new(),
            pending: HashMap::new(),
            next_id: 1,
        }
    }

    /// What the server has written to stderr so far.
    pub fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr lock").clone()
    }

    /// Write one JSON-RPC message as a single line.
    pub fn send(&mut self, message: &Value) {
        let mut line = serde_json::to_string(message).expect("serialise request");
        line.push('\n');
        self.write_bytes(line.into_bytes());
    }

    /// Queue bytes for the writer thread; panics if an earlier write failed.
    fn write_bytes(&mut self, bytes: Vec<u8>) {
        let failure = self
            .write_failure
            .lock()
            .expect("write failure lock")
            .clone();
        let queued = self.stdin.as_ref().expect("stdin already closed");
        if failure.is_some() || queued.send(bytes).is_err() {
            panic!(
                "could not write to the server ({}); it has probably exited. stderr: {}",
                failure.unwrap_or_else(|| "the writer stopped".to_owned()),
                self.stderr()
            );
        }
    }

    /// Send a request and return its whole response object (`result` or
    /// `error`), skipping any notification the server interleaves.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        self.read_response(id)
    }

    /// Send a request without waiting; returns its id for [`Self::read_response`].
    pub fn send_request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Send a request whose `params` carry no `arguments` key at all.
    pub fn send_bare_call(&mut self, name: &str) -> u64 {
        self.send_request("tools/call", json!({"name": name}))
    }

    /// `tools/call`, returning the whole response object.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// The server's own pid, for signalling and leftover checks.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Send a signal to the server process.
    #[cfg(unix)]
    pub fn signal(&self, signal: nix::sys::signal::Signal) {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.pid()).expect("pid fits i32"));
        nix::sys::signal::kill(pid, signal).expect("signal the server");
    }

    /// Send a request and return its `result`, failing if it carried an `error`.
    pub fn expect_result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        match response.get("result") {
            Some(result) => result.clone(),
            None => panic!("`{method}` returned no result: {response}"),
        }
    }

    /// Write `line` plus a newline verbatim (for malformed-input cases).
    pub fn send_raw_line(&mut self, line: &str) {
        self.write_bytes(format!("{line}\n").into_bytes());
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// The handshake: `initialize`, then `notifications/initialized`. Returns
    /// the `initialize` result.
    pub fn initialize(&mut self) -> Value {
        self.initialize_with(PROTOCOL_VERSION)
    }

    /// The handshake announcing `protocol_version`.
    pub fn initialize_with(&mut self, protocol_version: &str) -> Value {
        let result = self.expect_result(
            "initialize",
            json!({
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "orch-mcp-test-harness", "version": "0.0.0"},
            }),
        );
        self.notify("notifications/initialized", json!({}));
        result
    }

    /// Read lines until the response with `id` arrives, within [`TIMEOUT`].
    /// Responses to other ids are kept for their own `read_response`.
    pub fn read_response(&mut self, id: u64) -> Value {
        if let Some(message) = self.pending.remove(&id) {
            return message;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let (got, message) =
                self.next_response_before(deadline, &format!("the response to request {id}"));
            if got == id {
                return message;
            }
            self.pending.insert(got, message);
        }
    }

    /// The next response to arrive, whichever request it answers.
    pub fn next_response(&mut self, waiting_for: &str) -> (u64, Value) {
        if let Some(&id) = self.pending.keys().min() {
            let message = self.pending.remove(&id).expect("pending id present");
            return (id, message);
        }
        self.next_response_before(Instant::now() + TIMEOUT, waiting_for)
    }

    fn next_response_before(&mut self, deadline: Instant, waiting_for: &str) -> (u64, Value) {
        loop {
            let line = self.read_line_before(deadline, waiting_for);
            // A non-JSON line is kept in `seen` for the stdout-discipline
            // assertion; it can never be a response, so keep looking.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method").is_some() {
                continue;
            }
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                return (id, message);
            }
        }
    }

    fn read_line_before(&mut self, deadline: Instant, waiting_for: &str) -> String {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.frames.recv_timeout(remaining) {
            Ok(line) => {
                self.seen.push(line.clone());
                line
            }
            Err(RecvTimeoutError::Timeout) => panic!(
                "timed out after {TIMEOUT:?} waiting for {waiting_for}. stdout so far: {:?}; stderr: {}; write failure: {:?}",
                self.seen,
                self.stderr(),
                self.write_failure.lock().expect("write failure lock")
            ),
            Err(RecvTimeoutError::Disconnected) => panic!(
                "the server closed stdout before {waiting_for}. stdout so far: {:?}; stderr: {}",
                self.seen,
                self.stderr()
            ),
        }
    }

    /// Close stdin (EOF) without waiting.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Wait for the process to exit within [`SHUTDOWN_TIMEOUT`], then collect
    /// all of its output. A process that does not exit is killed and the test
    /// fails.
    pub fn wait_for_exit(mut self) -> Exit {
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll the child") {
                break status;
            }
            if Instant::now() >= deadline {
                kill_and_reap(&mut self.child);
                panic!(
                    "the server did not exit within {SHUTDOWN_TIMEOUT:?}. stdout: {:?}; stderr: {}",
                    self.seen,
                    self.stderr()
                );
            }
            thread::sleep(Duration::from_millis(10));
        };

        // The pipes close with the process, so the threads finish promptly. A
        // descendant that outlived the server and still holds a pipe keeps a
        // reader alive: that is a leak, so kill its group and fail loudly
        // instead of blocking until the descendant happens to exit.
        self.stdin = None; // the server is gone; let the writer thread end
        let mut stuck = 0;
        for thread in self.threads.drain(..) {
            if !thread.finish_within(TIMEOUT) {
                stuck += 1;
            }
        }
        if stuck > 0 {
            #[cfg(unix)]
            kill_group(self.child.id());
            panic!(
                "{stuck} of the server's pipe threads outlived it for {TIMEOUT:?}: a descendant still holds its pipes. stderr: {}",
                self.stderr()
            );
        }
        let mut stdout_lines = std::mem::take(&mut self.seen);
        stdout_lines.extend(self.frames.try_iter());
        Exit {
            status,
            stdout_lines,
            stderr: self.stderr(),
        }
    }

    /// [`Self::close_stdin`] then [`Self::wait_for_exit`].
    pub fn shutdown(mut self) -> Exit {
        self.close_stdin();
        self.wait_for_exit()
    }
}

impl Drop for McpServer {
    /// Never leave a server behind when an assertion fails mid-test.
    fn drop(&mut self) {
        // Let the writer finish; only a server still running is killed (with
        // its process group). One that already exited and was reaped is left
        // alone, so a test can still look for descendants it leaked.
        self.stdin = None;
        if matches!(self.child.try_wait(), Ok(None)) {
            kill_and_reap(&mut self.child);
        }
    }
}

/// Run `orch mcp <args>` to completion with stdin closed, and return how it
/// ended. For startup failures, where no frame should ever be written.
pub fn run_mcp(cwd: &Path, args: &[&str], envs: &[(&str, Option<&str>)]) -> Exit {
    McpServer::spawn_with(cwd, args, envs).shutdown()
}

/// Poll `condition` until it holds or `limit` passes; returns whether it held.
/// For waiting on an external effect (a file appearing, a process dying), never
/// as an oracle for a race.
pub fn wait_until(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// The JSON-RPC error code of a response, if it is an error.
pub fn error_code(response: &Value) -> Option<i64> {
    response["error"]["code"].as_i64()
}

/// The text of the first content block of a `tools/call` result.
pub fn tool_text(response: &Value) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content in {response}"))
}

/// A deterministic stall for the server's store lane: while engaged, reading
/// `.orchestry/config.yml` blocks, and every tool/resource call reads it when
/// it builds its Engine. Nothing here is a timing oracle — the file is a FIFO
/// with no writer until [`Self::release`].
#[cfg(unix)]
pub struct ConfigStall {
    config: std::path::PathBuf,
    real: std::path::PathBuf,
    fifo: std::path::PathBuf,
    content: Vec<u8>,
    writer: Option<Worker>,
}

#[cfg(unix)]
impl ConfigStall {
    /// Swap `config.yml` for a FIFO (hard-linked, so the test keeps a second
    /// name for it after it restores the real file).
    pub fn engage(project: &Path) -> Self {
        let dir = project.join(".orchestry");
        let config = dir.join("config.yml");
        let real = dir.join("config.yml.real");
        let fifo = dir.join("config.fifo");
        let content = std::fs::read(&config).expect("read config.yml");
        std::fs::rename(&config, &real).expect("move the real config aside");
        let made = output_of(Command::new("mkfifo").arg(&fifo));
        assert!(made.status.success(), "mkfifo failed");
        std::fs::hard_link(&fifo, &config).expect("link the fifo as config.yml");
        Self {
            config,
            real,
            fifo,
            content,
            writer: None,
        }
    }

    /// Put the real config back for every later read, then feed the one read
    /// that may already be blocked on the FIFO.
    pub fn release(&mut self) {
        std::fs::rename(&self.real, &self.config).expect("restore the real config");
        let (fifo, content) = (self.fifo.clone(), self.content.clone());
        self.writer = Some(Worker::spawn(move || {
            // Blocks until the stalled reader (if any) opens its end.
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(fifo) {
                let _ = file.write_all(&content);
            }
        }));
    }

    /// Open (and immediately keep) a non-blocking read end, so a feeder whose
    /// `open(write)` is still waiting for a reader returns.
    fn open_read_end(&self) -> std::io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(&self.fifo)
    }

    /// Join the feeder. If the stalled reader never reached the FIFO (it
    /// opened the restored file instead), open a read end so the feeder's
    /// `open` returns.
    pub fn finish(mut self) {
        if let Some(writer) = self.writer.take() {
            let _unblock = self.open_read_end();
            assert!(
                writer.finish_within(TIMEOUT),
                "the fifo feeder did not finish within {TIMEOUT:?}"
            );
        }
    }
}

#[cfg(unix)]
impl Drop for ConfigStall {
    /// A test that fails before [`Self::finish`] must not leave the feeder
    /// blocked in `open` for the rest of the test binary's life.
    fn drop(&mut self) {
        if self.writer.is_some() {
            let _unblock = self.open_read_end();
            if let Some(writer) = self.writer.take() {
                let _ = writer.finish_within(TIMEOUT);
            }
        }
    }
}

/// Fake adapter CLIs that never answer `--version`: each records its pid and
/// then sleeps, so a `doctor` probe stalls until the test (or the server's
/// shutdown) ends it. Put [`Self::path`] first on the child's `PATH`. Dropping
/// kills every stalled process, so a failing test leaves nothing behind.
#[cfg(unix)]
pub struct StallingAdapters {
    bin: tempfile::TempDir,
    pids: tempfile::TempDir,
}

#[cfg(unix)]
impl StallingAdapters {
    /// Every adapter binary `doctor` probes.
    const BINARIES: &'static [&'static str] = &[
        "claude",
        "codex",
        "opencode",
        "cursor",
        "cursor-agent",
        "agy",
        "grok",
        "pi",
    ];

    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::Builder::new()
            .prefix("orch-fake-bin-")
            .tempdir()
            .expect("tempdir");
        let pids = tempfile::Builder::new()
            .prefix("orch-fake-pids-")
            .tempdir()
            .expect("tempdir");
        for name in Self::BINARIES {
            let script = format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 60\n",
                pids.path().join(name).display()
            );
            let path = bin.path().join(name);
            std::fs::write(&path, script).expect("write fake adapter");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake adapter");
        }
        Self { bin, pids }
    }

    /// A `PATH` with the fakes ahead of the system directories.
    pub fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.bin.path().display())
    }

    /// Pids of the probes that have started so far.
    pub fn started(&self) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir(self.pids.path()) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|text| text.trim().parse().ok())
            .collect()
    }

    /// Whether `pid` is still a live process.
    pub fn alive(pid: u32) -> bool {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid fits i32"));
        nix::sys::signal::kill(pid, None).is_ok()
    }
}

#[cfg(unix)]
impl Drop for StallingAdapters {
    fn drop(&mut self) {
        for pid in self.started() {
            let pid = nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid fits i32"));
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
}
