//! Board-2 code review F1 (tsk_idtODWr): a connection that was accepted and
//! then failed is not "no daemon".
//!
//! A live listener on the project's socket that accepts and closes (before
//! or after reading the client's hello) is a daemon at capacity or shutting
//! down, not an absent one. The client must say `daemon_busy` (exit 7) and
//! must never tell the owner to start a daemon or an agent to stop and
//! report that none is running. The dial-time checks (no socket, a socket
//! with group/other bits) stay "no daemon" and are pinned as controls.
//!
//! These cases failed at f70fc707 (`No orch daemon is running for ...
//! (handshake failed)` / `(permission denied)`) and pass with the fix
//! (83bc9be9, `fix/connect-after-accept-classification`).
//!
//! Hermetic: the listener is a plain `std` `UnixListener` on the sandbox's
//! short socket path (0700 dir, 0600 socket, same uid); no daemon, no real
//! project, no AI CLI. Every cell runs with the process-table watch used by
//! the no-daemon matrix: no client may start anything.

mod daemon_support;

use daemon_support::frontends::*;
use daemon_support::*;
use serde_json::json;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// What the stand-in daemon does with an accepted connection.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Close at once, reading nothing.
    CloseImmediately,
    /// Read the client's hello, then close without a reply.
    ReadHelloThenClose,
}

/// A std listener that accepts every connection and applies a [`Mode`].
struct Listener {
    socket: std::path::PathBuf,
    accepted: Arc<AtomicUsize>,
    hellos: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    fn start(socket: std::path::PathBuf, mode: Mode, socket_mode: u32) -> Listener {
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(socket_mode)).unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let hellos = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (accepted, hellos, stop) = (accepted.clone(), hellos.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    if let Mode::ReadHelloThenClose = mode {
                        let mut buf = [0_u8; 4096];
                        if matches!(stream.read(&mut buf), Ok(n) if n > 0) {
                            hellos.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    drop(stream);
                }
            })
        };
        Listener {
            socket,
            accepted,
            hellos,
            stop,
            thread: Some(thread),
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    fn hellos(&self) -> usize {
        self.hellos.load(Ordering::SeqCst)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the thread sees the flag.
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An initialised project plus a listener in `mode` on its socket.
fn busy_scene(mode: Mode) -> (Sandbox, Listener) {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let listener = Listener::start(sandbox.socket(), mode, 0o600);
    (sandbox, listener)
}

const READ: &[&str] = &["task", "list"];
const WRITE: &[&str] = &["task", "add", "must not exist"];

/// The text a busy daemon must produce and the texts it must not.
fn assert_busy(who: &str, text: &str, root: &std::path::Path) {
    let seen = normalize(text);
    let wanted = format!("daemon_busy for {}", root.display());
    assert!(
        seen.contains(&wanted),
        "{who}: missing {wanted:?} in:\n{text}"
    );
    for forbidden in [
        "No orch daemon is running",
        "is not running",
        "Start it with",
        "Do not start it",
        "permission denied",
        "handshake failed",
    ] {
        assert!(
            !seen.contains(forbidden),
            "{who}: a live daemon was reported as absent ({forbidden:?}):\n{text}"
        );
    }
}

fn check_cli_busy(mode: Mode, args: &[&str], env: &[(&str, &str)]) {
    let _serial = serial();
    let (sandbox, listener) = busy_scene(mode);
    let root = sandbox.root();
    let before = tree(&root);
    let watch = Watch::begin();

    let mut cmd = sandbox.orch();
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let (out, pid) = output_and_pid(cmd, CALL);

    let who = format!("{mode:?} cli {args:?}");
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(7), "{who}: {stderr}");
    assert!(
        out.stdout.is_empty(),
        "{who} wrote stdout: {}",
        text(&out.stdout)
    );
    assert_busy(&who, &stderr, &root);
    // The listener really was reached (otherwise the text proves nothing).
    assert!(
        listener.accepted() >= 1,
        "{who}: the client never connected"
    );
    if let Mode::ReadHelloThenClose = mode {
        assert!(listener.hellos() >= 1, "{who}: the client sent no hello");
    }
    watch.assert_only_clients(&who, &[]);
    assert!(
        process_group_empty(pid),
        "{who} left a process in its group"
    );
    // Nothing repaired, nothing written (a direct-store fallback would show).
    assert_eq!(tree(&root), before, "{who} changed the project tree");
}

const AGENT_TASK: &[(&str, &str)] = &[("ORCH_TASK_ID", "tsk_agent")];

#[test]
fn cli_read_against_a_listener_that_closes_at_once_exits_7_daemon_busy() {
    check_cli_busy(Mode::CloseImmediately, READ, &[]);
}

#[test]
fn cli_write_against_a_listener_that_closes_at_once_exits_7_daemon_busy_and_writes_nothing() {
    check_cli_busy(Mode::CloseImmediately, WRITE, &[]);
}

#[test]
fn cli_read_in_an_agent_context_against_a_listener_that_closes_at_once_is_busy_not_stop_and_report()
{
    check_cli_busy(Mode::CloseImmediately, READ, AGENT_TASK);
}

#[test]
fn cli_write_in_an_agent_context_against_a_listener_that_closes_at_once_is_busy_not_stop_and_report()
 {
    check_cli_busy(Mode::CloseImmediately, WRITE, AGENT_TASK);
}

#[test]
fn cli_read_against_a_listener_that_reads_the_hello_then_closes_exits_7_daemon_busy() {
    check_cli_busy(Mode::ReadHelloThenClose, READ, &[]);
}

#[test]
fn cli_write_against_a_listener_that_reads_the_hello_then_closes_exits_7_daemon_busy_and_writes_nothing()
 {
    check_cli_busy(Mode::ReadHelloThenClose, WRITE, &[]);
}

#[test]
fn cli_read_in_an_agent_context_after_the_hello_is_closed_is_busy_not_stop_and_report() {
    check_cli_busy(Mode::ReadHelloThenClose, READ, AGENT_TASK);
}

#[test]
fn cli_write_in_an_agent_context_after_the_hello_is_closed_is_busy_not_stop_and_report() {
    check_cli_busy(Mode::ReadHelloThenClose, WRITE, AGENT_TASK);
}

// ── MCP ────────────────────────────────────────────────────────────────────

fn check_mcp_busy(mode: Mode) {
    let _serial = serial();
    let (sandbox, listener) = busy_scene(mode);
    let root = sandbox.root();
    let before = tree(&root);
    let watch = Watch::begin();
    let mut mcp = Mcp::spawn(sandbox.orch(), &[]);
    let pid = mcp.pid;

    let response = mcp.initialize();

    let who = format!("{mode:?} mcp initialize");
    let failure = mcp_failure(&who, &response);
    assert_busy(&who, &failure, &root);
    assert_mcp_exit_code_7(&who, &response);
    assert!(
        listener.accepted() >= 1,
        "{who}: the bridge never connected"
    );
    assert!(
        mcp.is_running(),
        "{who}: the bridge died instead of surfacing the error: {}",
        mcp.stderr()
    );
    // A client that ignores the failed initialize gets the same answer.
    mcp.notify("notifications/initialized", json!({}));
    let response = mcp.call_tool("status", json!({}));
    let failure = mcp_failure("mcp tool call", &response);
    assert_busy("mcp tool call", &failure, &root);
    watch.assert_only_clients(&who, &[pid]);
    assert_eq!(mcp.finish(), Some(0), "stderr: {}", mcp.stderr());
    assert!(process_group_empty(pid));
    assert_eq!(tree(&root), before, "{who} changed the project tree");
}

#[test]
fn mcp_initialize_against_a_listener_that_closes_at_once_returns_daemon_busy() {
    check_mcp_busy(Mode::CloseImmediately);
}

#[test]
fn mcp_initialize_against_a_listener_that_reads_the_hello_then_closes_returns_daemon_busy() {
    check_mcp_busy(Mode::ReadHelloThenClose);
}

// ── controls: dial-time failures stay "no daemon" ──────────────────────────

fn check_control(prepare: impl FnOnce(&Sandbox) -> Option<Listener>, detail: &str) {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let listener = prepare(&sandbox);
    let root = sandbox.root();
    let before = tree(&root);
    let watch = Watch::begin();

    let mut cmd = sandbox.orch();
    cmd.args(READ);
    let (out, pid) = output_and_pid(cmd, CALL);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(7), "{stderr}");
    let seen = normalize(&stderr);
    let wanted = format!(
        "No orch daemon is running for {} ({detail})",
        root.display()
    );
    assert!(seen.contains(&wanted), "missing {wanted:?} in:\n{stderr}");
    assert!(
        !seen.contains("daemon_busy"),
        "an absent/untrusted socket was reported busy:\n{stderr}"
    );
    if let Some(listener) = listener {
        // The mode check refuses before connecting: the listener saw nothing.
        assert_eq!(
            listener.accepted(),
            0,
            "the client dialled an unsafe socket"
        );
    }
    watch.assert_only_clients("control", &[]);
    assert!(process_group_empty(pid));
    assert_eq!(tree(&root), before);
}

#[test]
fn control_no_socket_is_still_no_daemon_no_socket() {
    check_control(|_| None, "no socket");
}

#[test]
fn control_stale_socket_with_group_bits_is_still_no_daemon_permission_denied() {
    check_control(
        |sandbox| {
            drop(UnixListener::bind(sandbox.socket()).unwrap());
            fs::set_permissions(sandbox.socket(), fs::Permissions::from_mode(0o660)).unwrap();
            None
        },
        "permission denied",
    );
}

#[test]
fn control_own_uid_socket_with_mode_0666_is_still_no_daemon_permission_denied_and_is_never_dialled()
{
    check_control(
        |sandbox| {
            Some(Listener::start(
                sandbox.socket(),
                Mode::CloseImmediately,
                0o666,
            ))
        },
        "permission denied",
    );
}
