//! A raw NDJSON client for the daemon integration vectors (T1).
//!
//! Deliberately not `orch-client`: these tests exist to speak the wire the way
//! a hostile or future peer would, byte by byte, so they build frames as plain
//! JSON values and read replies as plain JSON values. Nothing here sleeps;
//! every wait is bounded by a deadline that only fires on a hang.

#![allow(
    dead_code,
    reason = "each test crate uses a subset of the shared client"
)]

use orch_daemon::testing::{ClientStream, DaemonHarness};
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};

/// Upper bound for any single wait. Never a pacing delay.
pub const DEADLINE: Duration = Duration::from_secs(20);

pub struct Raw {
    reader: BufReader<ReadHalf<Box<dyn ClientStream>>>,
    writer: WriteHalf<Box<dyn ClientStream>>,
}

/// `(dev, ino)` of `<root>/.orchestry`, the project identity a hello carries.
pub fn project_id(root: &Path) -> Value {
    let meta = std::fs::metadata(root.join(".orchestry")).unwrap();
    json!({"dev": meta.dev(), "ino": meta.ino()})
}

/// A hello for `root` at the given versions. `extra` fields are merged into
/// the hello object (caller, mode, intent, unknown future fields …).
pub fn hello(root: &Path, protocol: u32, binary: &str, extra: Value) -> Value {
    let mut hello = json!({
        "protocol": protocol,
        "binary": binary,
        "project_root": {"utf8": root.to_str().unwrap()},
        "project_id": project_id(root),
    });
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        hello[key] = value;
    }
    json!({ "hello": hello })
}

/// A session hello at this build's versions.
pub fn session_hello(root: &Path, kind: &str, mode: &str) -> Value {
    hello(
        root,
        orch_proto::LOCAL_PROTOCOL,
        orch_proto::BINARY_VERSION,
        json!({"intent": "session", "caller": {"kind": kind}, "mode": mode}),
    )
}

pub fn line(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    bytes
}

impl Raw {
    /// A fresh connection; nothing sent yet.
    pub async fn open(daemon: &DaemonHarness) -> Raw {
        let stream = daemon.connect().await.expect("connect").into_stream();
        let (read, writer) = tokio::io::split(stream);
        Raw {
            reader: BufReader::new(read),
            writer,
        }
    }

    /// Open, send a session hello, and require a compatible welcome.
    pub async fn session(daemon: &DaemonHarness, kind: &str, mode: &str) -> Raw {
        let mut raw = Raw::open(daemon).await;
        raw.send(&session_hello(daemon.project_root(), kind, mode))
            .await;
        let welcome = raw.recv().await.expect("a welcome");
        assert_eq!(welcome["welcome"]["compatible"], true, "{welcome}");
        raw
    }

    pub async fn write_bytes(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    pub async fn send(&mut self, value: &Value) {
        self.write_bytes(&line(value)).await;
    }

    pub async fn send_text(&mut self, text: &str) {
        self.write_bytes(format!("{text}\n").as_bytes()).await;
    }

    /// The next frame, or `None` when the daemon closed the connection.
    pub async fn recv(&mut self) -> Option<Value> {
        let mut text = String::new();
        let read = tokio::time::timeout(DEADLINE, self.reader.read_line(&mut text))
            .await
            .expect("timed out waiting for the daemon");
        match read {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(text.trim_end()).expect("a JSON frame")),
        }
    }

    /// Read every remaining frame until the daemon closes the connection.
    pub async fn until_closed(&mut self) -> Vec<Value> {
        let mut frames = Vec::new();
        while let Some(frame) = self.recv().await {
            frames.push(frame);
        }
        frames
    }

    /// Send `op` as request `id` and return its terminal frame (`ok` or
    /// `err`), skipping `part` and `ev` frames.
    pub async fn request(&mut self, id: u64, op: &str, args: Value) -> Value {
        self.send(&json!({"id": id, "req": {"op": op, "args": args}}))
            .await;
        self.terminal(id).await
    }

    pub async fn terminal(&mut self, id: u64) -> Value {
        loop {
            let frame = self.recv().await.expect("connection closed before reply");
            if frame["id"] == id && (frame.get("ok").is_some() || frame.get("err").is_some()) {
                return frame;
            }
        }
    }
}

/// The `ok` payload of a terminal frame, panicking on `err`.
pub fn ok(frame: &Value) -> &Value {
    assert!(frame.get("err").is_none(), "unexpected error: {frame}");
    frame
        .get("ok")
        .unwrap_or_else(|| panic!("no ok in {frame}"))
}

/// The `err.code` of a terminal frame, panicking on `ok`.
pub fn err_code(frame: &Value) -> &str {
    frame["err"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error, got {frame}"))
}

// ---------------------------------------------------------------------------
// Fixtures shared by the engine/dispatch vectors
// ---------------------------------------------------------------------------

use orch_core::clock::{Clock, iso_from_epoch_ms};
use std::sync::atomic::{AtomicI64, Ordering};

/// A clock that only moves when a test moves it (no sleeping for TTLs).
#[derive(Debug)]
pub struct TestClock(AtomicI64);

impl TestClock {
    pub const START_MS: i64 = 1_760_000_000_000;

    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(TestClock(AtomicI64::new(Self::START_MS)))
    }

    pub fn advance_ms(&self, delta: i64) {
        self.0.fetch_add(delta, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}

/// Create a shell-adapter agent that runs `command` (a fake agent: no real AI
/// CLI is ever involved) with a private `HOME` and a minimal `PATH`.
pub async fn shell_agent(
    raw: &mut Raw,
    daemon: &DaemonHarness,
    name: &str,
    command: &str,
) -> String {
    let reply = raw
        .request(
            1_000_000,
            "agent.create",
            json!({
                "name": name,
                "adapter": "shell",
                "command": command,
                "workspace_mode": "shared",
                "env": [
                    {"name": "HOME", "value": daemon.project_root().to_str().unwrap()},
                    {"name": "PATH", "value": "/usr/bin:/bin"},
                ],
            }),
        )
        .await;
    ok(&reply)["agent"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no agent id in {reply}"))
        .to_owned()
}

/// Create a task through the socket and return its id.
pub async fn task(raw: &mut Raw, title: &str, extra: Value) -> String {
    let mut args = json!({"title": title});
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        args[key] = value;
    }
    let reply = raw.request(1_000_001, "task.create", args).await;
    ok(&reply)["id"].as_str().unwrap().to_owned()
}

pub async fn task_status(raw: &mut Raw, id: &str) -> String {
    let reply = raw.request(1_000_002, "task.get", json!({"id": id})).await;
    ok(&reply)["status"].as_str().unwrap().to_owned()
}

/// Kills the processes it was told about when dropped, so a failing test never
/// leaks a fake agent.
pub struct Reaper {
    processes: std::sync::Arc<dyn orch_core::process::ProcessManager>,
    pids: std::sync::Mutex<Vec<u32>>,
}

impl Reaper {
    pub fn new(daemon: &DaemonHarness) -> Reaper {
        Reaper {
            processes: daemon.engine().processes.clone(),
            pids: Default::default(),
        }
    }
    pub fn watch(&self, pid: u32) {
        self.pids.lock().unwrap().push(pid);
    }
    pub fn alive(&self, pid: u32) -> bool {
        self.processes.is_alive(pid)
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in self.pids.lock().unwrap().iter() {
            self.processes.kill(*pid, orch_core::process::Signal::Kill);
        }
    }
}

/// The pid the daemon recorded for a running task, if it is running.
pub async fn running_pid(daemon: &DaemonHarness, task: &str) -> Option<u32> {
    let state = daemon.engine().state.read().await.unwrap();
    state.running.get(task).map(|run| run.pid)
}

/// Poll a condition (cooperatively, with no delay) until it holds. The
/// deadline is a hang guard only. The condition may `.await`.
#[macro_export]
macro_rules! eventually {
    ($what:expr, $condition:expr) => {{
        let wait = async {
            loop {
                if $condition {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout($crate::support::DEADLINE, wait)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {}", $what));
    }};
}

/// A shell agent that exits at once with `code` and needs no human approval.
pub async fn quick_agent(raw: &mut Raw, daemon: &DaemonHarness, name: &str, code: i32) -> String {
    let reply = raw
        .request(
            1_000_010,
            "agent.create",
            json!({
                "name": name,
                "adapter": "shell",
                "command": format!("exit {code}"),
                "approval_policy": "manual",
                "workspace_mode": "shared",
                "env": [
                    {"name": "HOME", "value": daemon.project_root().to_str().unwrap()},
                    {"name": "PATH", "value": "/usr/bin:/bin"},
                ],
            }),
        )
        .await;
    ok(&reply)["agent"]["id"].as_str().unwrap().to_owned()
}

/// `daemon.status.dispatch` as `(dispatch_on, batch_active)`.
pub async fn dispatch_state(raw: &mut Raw) -> (bool, bool) {
    let status = raw.request(1_000_020, "daemon.status", json!({})).await;
    let state = &ok(&status)["dispatch"];
    (
        state["dispatch_on"].as_bool().unwrap(),
        state["batch_active"].as_bool().unwrap(),
    )
}
