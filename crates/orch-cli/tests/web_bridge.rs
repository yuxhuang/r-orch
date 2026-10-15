//! `orch web`'s bridge end to end: a real websocket client against the real
//! router, with `sh -c ...` standing in for `orch tui`.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use orch_cli::web::{Auth, HostPolicy, SpawnSpec, WebState, router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

const ECHO: &str = "echo hello; cat";

async fn start(auth: Auth, script: &str) -> (SocketAddr, Arc<WebState>) {
    let state = Arc::new(WebState::new(
        &auth,
        HostPolicy::new(true, Vec::new()),
        SpawnSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let service = router(Arc::clone(&state)).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, service).await.expect("serve");
    });
    (addr, state)
}

async fn connect_to(
    addr: SocketAddr,
    path: &str,
    headers: &[(&'static str, String)],
) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
    let mut request = format!("ws://{addr}{path}")
        .into_client_request()
        .expect("request");
    for (name, value) in headers {
        request
            .headers_mut()
            .insert(*name, HeaderValue::from_str(value).expect("header value"));
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(client, _)| client)
}

async fn connect(
    addr: SocketAddr,
    headers: &[(&'static str, String)],
) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
    connect_to(addr, "/ws?cols=100&rows=30", headers).await
}

async fn read_until(client: &mut Client, needle: &str) -> String {
    let mut seen = String::new();
    let wait = async {
        while let Some(message) = client.next().await {
            if let Message::Binary(bytes) = message.expect("frame") {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains(needle) {
                    return;
                }
            }
        }
        panic!("socket closed before {needle:?}; saw {seen:?}");
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; saw {seen:?}"));
    seen
}

fn status_of(err: tokio_tungstenite::tungstenite::Error) -> u16 {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
        other => panic!("expected an HTTP rejection, got {other:?}"),
    }
}

/// A bare HTTP/1.1 GET, returning the raw response head and body.
async fn http_get(addr: SocketAddr, path: &str, extra: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{extra}\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    response
}

async fn eventually(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn pid_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

#[tokio::test]
async fn output_arrives_and_input_is_echoed_and_malformed_controls_are_ignored() {
    let (addr, _) = start(Auth::Open, ECHO).await;
    let mut client = connect(addr, &[("origin", format!("http://{addr}"))])
        .await
        .expect("same-origin upgrade");

    read_until(&mut client, "hello").await;

    client
        .send(Message::Text("{not json".into()))
        .await
        .expect("send junk");
    client
        .send(Message::Text(
            r#"{"type":"resize","cols":90,"rows":20}"#.into(),
        ))
        .await
        .expect("send resize");
    client
        .send(Message::Binary(b"ping-pong\n".to_vec().into()))
        .await
        .expect("send input");
    read_until(&mut client, "ping-pong").await;
}

#[tokio::test]
async fn an_oversized_message_closes_the_connection() {
    let (addr, _) = start(Auth::Open, ECHO).await;
    let mut client = connect(addr, &[]).await.expect("upgrade");
    read_until(&mut client, "hello").await;

    let big = vec![b'x'; orch_cli::web::MAX_MESSAGE_BYTES + 1];
    // The server may reset the connection before the send completes.
    let _ = client.send(Message::Binary(big.into())).await;
    let closed = async {
        while let Some(message) = client.next().await {
            if message.is_err() || matches!(message, Ok(Message::Close(_))) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), closed)
        .await
        .expect("connection should close after an oversized message");
}

#[tokio::test]
async fn cross_origin_upgrades_and_foreign_hosts_are_rejected() {
    let (addr, _) = start(Auth::Open, ECHO).await;
    let err = connect(addr, &[("origin", "http://evil.example".to_owned())])
        .await
        .expect_err("cross-origin upgrade must fail");
    assert_eq!(status_of(err), 403);

    let err = connect(
        addr,
        &[
            ("host", "evil.example:7681".to_owned()),
            ("origin", "http://evil.example:7681".to_owned()),
        ],
    )
    .await
    .expect_err("a rebinding Host must fail even when Origin agrees with it");
    assert_eq!(status_of(err), 403);
}

#[tokio::test]
async fn a_token_is_required_and_failures_back_off() {
    let token = "0123456789abcdef0123456789abcdef";
    let (addr, _) = start(Auth::Token(token.into()), ECHO).await;

    let err = connect(addr, &[]).await.expect_err("no credential");
    assert_eq!(status_of(err), 401);

    let mut client = connect_to(addr, &format!("/ws?token={token}&cols=100&rows=30"), &[])
        .await
        .expect("the query token");
    read_until(&mut client, "hello").await;

    // The token also works as the Basic password: dXNlcjo...  is `user:<token>`.
    let basic = format!(
        "Basic {}",
        base64_encode(format!("user:{token}").as_bytes())
    );
    let mut client = connect(addr, &[("authorization", basic)])
        .await
        .expect("token as Basic password");
    read_until(&mut client, "hello").await;

    for _ in 0..5 {
        let err = connect_to(addr, "/ws?token=wrong", &[])
            .await
            .expect_err("wrong token");
        assert_eq!(status_of(err), 401);
    }
    // Blocked: a wrong or missing credential is answered 429 ...
    let err = connect_to(addr, "/ws?token=wrong", &[])
        .await
        .expect_err("blocked");
    assert_eq!(status_of(err), 429);
    let err = connect(addr, &[])
        .await
        .expect_err("blocked, no credential");
    assert_eq!(status_of(err), 429);
    // ... but a correct one always gets in, and clears the block.
    let mut client = connect_to(addr, &format!("/ws?token={token}"), &[])
        .await
        .expect("a correct token must pass while blocked");
    read_until(&mut client, "hello").await;
    let err = connect_to(addr, "/ws?token=wrong", &[])
        .await
        .expect_err("wrong again");
    assert_eq!(status_of(err), 401, "success reset the counter");
}

#[tokio::test]
async fn a_password_is_required_on_the_page_and_the_upgrade() {
    let (addr, _) = start(Auth::Password("s3cret".into()), ECHO).await;

    assert!(http_get(addr, "/", "").await.starts_with("HTTP/1.1 401"));
    let good = format!("Authorization: Basic {}\r\n", base64_encode(b"user:s3cret"));
    assert!(http_get(addr, "/", &good).await.starts_with("HTTP/1.1 200"));
    // A token in the query is not a credential in password mode.
    assert!(
        http_get(addr, "/?token=s3cret", "")
            .await
            .starts_with("HTTP/1.1 401")
    );

    let origin = ("origin", format!("http://{addr}"));
    let err = connect(addr, std::slice::from_ref(&origin))
        .await
        .expect_err("no credentials");
    assert_eq!(status_of(err), 401);

    let wrong = format!("Basic {}", base64_encode(b"user:wrong"));
    let err = connect(addr, &[origin.clone(), ("authorization", wrong)])
        .await
        .expect_err("wrong password");
    assert_eq!(status_of(err), 401);

    let right = format!("Basic {}", base64_encode(b"user:s3cret"));
    let mut client = connect(addr, &[origin, ("authorization", right)])
        .await
        .expect("right password");
    read_until(&mut client, "hello").await;
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let token = "0123456789abcdef0123456789abcdef";
    let (addr, _) = start(Auth::Token(token.into()), ECHO).await;
    for (path, status) in [
        (format!("/?token={token}"), "200"),
        ("/".to_owned(), "401"),
        ("/xterm.js".to_owned(), "200"),
        ("/nope".to_owned(), "404"),
    ] {
        let response = http_get(addr, &path, "").await.to_ascii_lowercase();
        assert!(
            response.starts_with(&format!("http/1.1 {status}")),
            "{path}: {response}"
        );
        for header in [
            "x-frame-options: deny",
            "x-content-type-options: nosniff",
            "referrer-policy: no-referrer",
            "content-security-policy: default-src 'none'",
        ] {
            assert!(
                response.contains(header),
                "{path} lacks {header}: {response}"
            );
        }
        assert!(response.contains("frame-ancestors 'none'"), "{path}");
        let cache = if path == "/xterm.js" {
            "cache-control: public, max-age=31536000, immutable"
        } else {
            "cache-control: no-store"
        };
        assert!(response.contains(cache), "{path} lacks {cache}: {response}");
    }
}

#[tokio::test]
async fn assets_need_no_credential_but_the_page_does() {
    let (addr, _) = start(Auth::Token("0123456789abcdef0123456789abcdef".into()), ECHO).await;
    assert!(
        http_get(addr, "/xterm.js", "")
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert!(http_get(addr, "/", "").await.starts_with("HTTP/1.1 401"));
}

#[tokio::test]
async fn at_most_eight_sessions_run_at_once() {
    let (addr, state) = start(Auth::Open, ECHO).await;
    let mut clients = Vec::new();
    for _ in 0..8 {
        let mut client = connect(addr, &[]).await.expect("within the cap");
        read_until(&mut client, "hello").await;
        clients.push(client);
    }
    let err = connect(addr, &[]).await.expect_err("ninth session");
    assert_eq!(status_of(err), 503);

    drop(clients);
    eventually("sessions to wind down", || state.live_sessions() == 0).await;
}

#[tokio::test]
async fn the_client_closing_ends_the_session_and_kills_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let script = format!("echo $$ > {}; echo hello; exec cat", pid_file.display());
    let (addr, state) = start(Auth::Open, &script).await;

    let mut client = connect(addr, &[]).await.expect("upgrade");
    read_until(&mut client, "hello").await;
    let pid = read_trimmed(&pid_file).expect("pid file");
    client.close(None).await.expect("close");
    let drained = async { while client.next().await.is_some() {} };
    tokio::time::timeout(Duration::from_secs(10), drained)
        .await
        .expect("server should finish the close handshake");

    eventually("the child to exit", || !pid_alive(&pid)).await;
    eventually("the slot to free", || state.live_sessions() == 0).await;
}

/// A child that never reads its input must not wedge the bridge: output keeps
/// draining, the backlog limit closes the session, and the child goes away.
#[tokio::test]
async fn a_child_that_ignores_stdin_cannot_wedge_the_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("pid");
    let script = format!("echo $$ > {}; exec sleep 100", pid_file.display());
    let (addr, state) = start(Auth::Open, &script).await;

    let mut client = connect(addr, &[]).await.expect("upgrade");
    eventually("the child to start", || read_trimmed(&pid_file).is_some()).await;
    let pid = read_trimmed(&pid_file).expect("pid file");

    let chunk = vec![b'p'; 16 * 1024];
    for _ in 0..(4 * 1024 * 1024 / chunk.len()) {
        if client
            .send(Message::Binary(chunk.clone().into()))
            .await
            .is_err()
        {
            break; // the server already closed the session
        }
    }
    let _ = client.close(None).await;

    eventually("the child to be killed", || !pid_alive(&pid)).await;
    eventually("the slot to free", || state.live_sessions() == 0).await;
}

/// Teardown starts with SIGHUP and waits, so a child that cleans up on HUP
/// (as `orch tui` does to stop its agents) gets to finish.
#[tokio::test]
async fn teardown_gives_the_child_time_to_clean_up_on_sighup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("graceful");
    let script = format!(
        "trap 'sleep 1; echo cleaned > {}; exit 0' HUP; echo ready; while :; do sleep 0.1; done",
        marker.display()
    );
    let (addr, state) = start(Auth::Open, &script).await;

    let mut client = connect(addr, &[]).await.expect("upgrade");
    read_until(&mut client, "ready").await;
    drop(client);

    eventually("the cleanup marker", || read_trimmed(&marker).is_some()).await;
    eventually("the slot to free", || state.live_sessions() == 0).await;
}

#[tokio::test]
async fn shutdown_closes_open_sessions() {
    let (addr, state) = start(Auth::Open, ECHO).await;
    let mut client = connect(addr, &[]).await.expect("upgrade");
    read_until(&mut client, "hello").await;

    state.shutdown();
    let closed = async {
        while let Some(message) = client.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), closed)
        .await
        .expect("the session should close on shutdown");
    eventually("the slot to free", || state.live_sessions() == 0).await;
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
