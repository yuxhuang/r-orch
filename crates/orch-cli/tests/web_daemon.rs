//! `orch web` as a daemon client that holds a liveness session (T3; spec R16,
//! brief D7 web row, §4 S3b).
//!
//! The real `orch web` binary runs against a real test daemon through a
//! counting proxy (`push_support::CountingProxy`), which shows from the
//! daemon's side what the web parent does: whether it keeps a connection open
//! while idle, and whether it closes that connection when the daemon exits.
//! The pty children it starts (`orch tui`) inherit the same proxy.
//!
//! Authentication, Host/Origin and PTY limits keep their own suite
//! (`web_bridge.rs`); here they appear only where the outage behaviour
//! interacts with them.

#![cfg(unix)]

mod push_support;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use futures::StreamExt;
use push_support::{ConnInfo, Guarded, Project, WAIT, stdout_lines, wait_until};
use tokio_tungstenite::tungstenite::Message;

/// A running `orch web` with a generated token, reached through the proxy.
struct Web {
    guard: Guarded,
    addr: String,
    token: String,
    // Keeps the stdout reader's channel alive.
    _lines: Receiver<String>,
}

fn spawn_web(project: &Project) -> Web {
    let mut command = project.frontend(&["web", "--bind", "127.0.0.1:0"]);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut guard = Guarded::spawn(&mut command);
    let lines = stdout_lines(&mut guard.child);
    let listening = loop {
        let line = lines
            .recv_timeout(WAIT)
            .expect("orch web printed no listening line");
        if line.starts_with("orch web listening on http://") {
            break line;
        }
    };
    let rest = listening
        .strip_prefix("orch web listening on http://")
        .unwrap();
    let addr = rest.split('/').next().unwrap().to_owned();
    let token = rest
        .split("?token=")
        .nth(1)
        .expect("a token in the URL")
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    Web {
        guard,
        addr,
        token,
        _lines: lines,
    }
}

impl Web {
    /// A bare HTTP/1.1 GET: `(status, body)`.
    fn get(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.addr).expect("connect to orch web");
        stream.set_read_timeout(Some(WAIT)).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.addr
        )
        .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
        (status, body)
    }

    fn authed(&self, path: &str) -> String {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!("{path}{separator}token={}", self.token)
    }
}

/// The parent's long-lived connection to the daemon: caller `web`, still open,
/// and used to subscribe.
fn held_session(project: &Project) -> Option<ConnInfo> {
    let conns = project.proxy.conns();
    let ops = project.proxy.all_ops();
    conns.into_iter().find(|conn| {
        conn.caller.as_deref() == Some("web")
            && !conn.client_closed
            && ops
                .iter()
                .any(|seen| seen.conn == conn.id && seen.op == "subscribe")
    })
}

/// While idle the parent keeps one session to the daemon and subscribes on it
/// (it does not merely dial and drop a connection per request).
#[test]
fn an_idle_web_parent_holds_a_daemon_observation_session_before_any_request() {
    let project = Project::start();
    let _web = spawn_web(&project);
    let held = wait_until("the web parent's held, subscribed session", WAIT, || {
        held_session(&project)
    });
    assert_eq!(held.caller.as_deref(), Some("web"));
    assert!(!held.upstream_closed, "{held:?}");
}

/// The parent notices the daemon going away by itself: the daemon closes the
/// session, the parent reads EOF and closes its end — with no HTTP request
/// made in between. (A parent that only checks at request time never reads
/// the socket, so never closes it.)
#[test]
fn the_parent_sees_eof_and_closes_its_session_when_the_daemon_exits() {
    let mut project = Project::start();
    let web = spawn_web(&project);
    let held = wait_until("the web parent's held, subscribed session", WAIT, || {
        held_session(&project)
    });
    project.stop_daemon();
    wait_until("the parent to close its session after EOF", WAIT, || {
        project
            .proxy
            .conns()
            .into_iter()
            .find(|conn| conn.id == held.id)
            .filter(|conn| conn.upstream_closed && conn.client_closed)
    });
    // It noticed; it did not crash, and it started nothing.
    let mut guard = web.guard;
    assert!(
        guard.child.try_wait().unwrap().is_none(),
        "orch web exited when the daemon did"
    );
}

/// After the outage an authenticated request gets 503 with the frozen
/// no-daemon message; a request without a credential gets 401 (the project is
/// never named to it).
#[test]
fn after_the_daemon_exits_an_authenticated_request_gets_503_and_an_anonymous_one_401() {
    let mut project = Project::start();
    let web = spawn_web(&project);
    wait_until("the web parent's held, subscribed session", WAIT, || {
        held_session(&project)
    });
    project.stop_daemon();

    let (status, body) = wait_until("an authenticated 503", WAIT, || {
        let (status, body) = web.get(&web.authed("/"));
        (status == 503).then_some((status, body))
    });
    assert_eq!(status, 503);
    assert!(
        body.contains("No orch daemon is running"),
        "the 503 body does not carry the frozen message: {body}"
    );
    let (anonymous, body) = web.get("/");
    assert_eq!(anonymous, 401, "{body}");
    assert!(
        !body.contains(&project.root().display().to_string()),
        "the project path leaked to an anonymous request: {body}"
    );
}

/// After an explicit owner restart the next page load works again and a fresh
/// pty child (`orch tui`) talks to the new daemon, not the old one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_pty_after_an_explicit_restart_reaches_the_new_daemon() {
    let mut project = Project::start();
    let web = spawn_web(&project);
    wait_until("the web parent's held, subscribed session", WAIT, || {
        held_session(&project)
    });
    project.stop_daemon();
    project.start_daemon();
    let before = project.proxy.conns().len();

    // The owner restarted the daemon; the user reloads the page.
    wait_until("the page to load again", WAIT, || {
        (web.get(&web.authed("/")).0 == 200).then_some(())
    });
    let url = format!("ws://{}{}", web.addr, web.authed("/ws?cols=100&rows=30"));
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("the websocket upgrade after the restart");
    let mut seen = String::new();
    let draw = async {
        while let Some(message) = socket.next().await {
            if let Message::Binary(bytes) = message.expect("frame") {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains("ORCH") {
                    return;
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), draw)
        .await
        .unwrap_or_else(|_| panic!("the pty child drew nothing; saw {seen:?}"));

    let tui_after_restart = project
        .proxy
        .conns()
        .into_iter()
        .any(|conn| conn.id >= before && conn.caller.as_deref() == Some("tui"));
    assert!(
        tui_after_restart,
        "no dashboard session reached the restarted daemon: {:?}",
        project.proxy.conns()
    );
    // And the parent re-established its liveness session on the new daemon.
    wait_until(
        "the parent to hold a session on the new daemon",
        WAIT,
        || held_session(&project).filter(|conn| conn.id >= before),
    );
}
