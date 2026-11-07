//! A hand-rolled, black-box MCP client for `orch mcp`.
//!
//! It speaks newline-delimited JSON-RPC 2.0 over the child's stdio with
//! `serde_json` and `std::process` only, deliberately *not* the `rmcp` crate the
//! server is built on: a test that shares the server's protocol library would
//! agree with its bugs. Every read has a deadline, so a hung server fails the
//! test instead of blocking CI.

#![allow(dead_code, reason = "each `mcp_*` test crate uses a different subset")]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Deadline for every single read from, and wait on, the server.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// The MCP revision the harness announces in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

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
    let dir = empty_project();
    let output = orch_command(dir.path())
        .args(["init", "--name", "p", "--adapter", "shell"])
        .output()
        .expect("run `orch init`");
    assert!(
        output.status.success(),
        "`orch init` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    dir
}

/// The `orch` binary, isolated from the developer's machine: a private `HOME`
/// and a `PATH` with no agent CLIs on it (so nothing real is ever exec'd).
pub fn orch_command(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
    command
        .current_dir(cwd)
        .env("HOME", cwd.join(".test-home"))
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
    stdin: Option<ChildStdin>,
    frames: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
    seen: Vec<String>,
    next_id: u64,
}

impl McpServer {
    /// Spawn `orch mcp` with `cwd` as the working directory.
    pub fn spawn(cwd: &Path) -> Self {
        Self::spawn_with_args(cwd, &[])
    }

    pub fn spawn_with_args(cwd: &Path, extra_args: &[&str]) -> Self {
        let mut child = orch_command(cwd)
            .arg("mcp")
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `orch mcp`");

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");

        // Reads happen on threads so the test thread can put a deadline on them.
        let (sender, frames) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let stderr_reader = thread::spawn(move || {
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

        Self {
            child,
            stdin,
            frames,
            stderr,
            readers: vec![stdout_reader, stderr_reader],
            seen: Vec::new(),
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
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        let written = stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush());
        if let Err(err) = written {
            panic!(
                "could not write to the server ({err}); it has probably exited. stderr: {}",
                self.stderr()
            );
        }
    }

    /// Send a request and return its whole response object (`result` or
    /// `error`), skipping any notification the server interleaves.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.read_response(id)
    }

    /// Send a request and return its `result`, failing if it carried an `error`.
    pub fn expect_result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        match response.get("result") {
            Some(result) => result.clone(),
            None => panic!("`{method}` returned no result: {response}"),
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// The handshake: `initialize`, then `notifications/initialized`. Returns
    /// the `initialize` result.
    pub fn initialize(&mut self) -> Value {
        let result = self.expect_result(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "orch-mcp-test-harness", "version": "0.0.0"},
            }),
        );
        self.notify("notifications/initialized", json!({}));
        result
    }

    /// Read lines until the response with `id` arrives, within [`TIMEOUT`].
    fn read_response(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let line = self.read_line_before(deadline, &format!("the response to request {id}"));
            // A non-JSON line is kept in `seen` for the stdout-discipline
            // assertion; it can never be a response, so keep looking.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message;
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
                "timed out after {TIMEOUT:?} waiting for {waiting_for}. stdout so far: {:?}; stderr: {}",
                self.seen,
                self.stderr()
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

    /// Wait for the process to exit within [`TIMEOUT`], then collect all of its
    /// output. A process that does not exit is killed and the test fails.
    pub fn wait_for_exit(mut self) -> Exit {
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll the child") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "the server did not exit within {TIMEOUT:?}. stdout: {:?}; stderr: {}",
                    self.seen,
                    self.stderr()
                );
            }
            thread::sleep(Duration::from_millis(10));
        };

        // The pipes close with the process, so the readers finish promptly.
        for reader in self.readers.drain(..) {
            let _ = reader.join();
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
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
