//! The D5 connect state machine over real Unix sockets: every way of not
//! reaching a compatible daemon maps to its frozen text and exit code, and the
//! session carries the resolved caller identity (R8).
use orch_client::{Client, ClientConfig, ClientError};
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, HelloFrame, MAX_HANDSHAKE_BYTES, SessionMode, Welcome,
};
use orch_proto::ndjson::{FrameReader, write_frame};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A short project root under /tmp (macOS socket paths are at most 104 bytes).
fn project_dir() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("cc-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir(dir.path().join(".orchestry")).unwrap();
    dir
}

fn config(root: &Path, socket: Option<PathBuf>) -> ClientConfig {
    ClientConfig {
        cwd: root.into(),
        project_root: Some(root.into()),
        socket,
        test_socket_root: Some(root.into()),
        caller: Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::Normal,
        agent_context: false,
    }
}

fn bind(path: &Path) -> UnixListener {
    let listener = UnixListener::bind(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    listener
}

async fn connect_err(config: &ClientConfig) -> ClientError {
    Client::connect(config)
        .await
        .err()
        .expect("connect must fail")
}

/// The uniform NoDaemon form: exit 7, the frozen headline with `detail`, the
/// start hint for the owner and the stop-and-report variant for an agent.
fn assert_no_daemon(error: &ClientError, root: &Path, detail: &str) {
    assert_eq!(error.exit_code(), 7, "{error}");
    let root = root.canonicalize().unwrap();
    let owner = error.render(false);
    assert_eq!(
        owner,
        format!(
            "  ✕ No orch daemon is running for {} ({detail})\n    Start it with: orch daemon start",
            root.display()
        )
    );
    let agent = error.render(true);
    assert_eq!(
        agent,
        format!(
            "  ✕ The orch daemon for {} is not running. Do not start it; report this to the owner and stop.\n    Detail: {detail}",
            root.display()
        )
    );
}

#[tokio::test]
async fn no_project_is_not_initialized_exit_3() {
    let dir = tempfile::Builder::new()
        .prefix("cc-")
        .tempdir_in("/tmp")
        .unwrap();
    let error = connect_err(&config(dir.path(), None)).await;
    assert_eq!(error.exit_code(), 3);
    assert!(error.render(false).contains("Run: orch init"), "{error}");
}

#[tokio::test]
async fn absent_daemon_without_discovery_record() {
    let dir = project_dir();
    let error = connect_err(&config(dir.path(), None)).await;
    assert_no_daemon(&error, dir.path(), "no daemon.json");
}

#[tokio::test]
async fn unsafe_discovery_record() {
    let dir = project_dir();
    let record = dir.path().join(".orchestry/daemon.json");
    fs::write(&record, "{}").unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
    let error = connect_err(&config(dir.path(), None)).await;
    assert_no_daemon(&error, dir.path(), "daemon.json failed its safety checks");
}

#[tokio::test]
async fn explicit_socket_that_does_not_exist() {
    let dir = project_dir();
    let error = connect_err(&config(dir.path(), Some(dir.path().join("s")))).await;
    assert_no_daemon(&error, dir.path(), "no socket");
}

#[tokio::test]
async fn stale_socket_left_by_a_dead_daemon_is_connection_refused() {
    let dir = project_dir();
    let socket = dir.path().join("s");
    drop(bind(&socket));
    assert!(socket.exists(), "the stale socket file stays behind");
    let error = connect_err(&config(dir.path(), Some(socket))).await;
    assert_no_daemon(&error, dir.path(), "connection refused");
}

#[tokio::test]
async fn unsafe_endpoints_are_permission_denied() {
    let dir = project_dir();
    let socket = dir.path().join("s");
    let _listener = bind(&socket);
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    let error = connect_err(&config(dir.path(), Some(socket))).await;
    assert_no_daemon(&error, dir.path(), "permission denied");

    let file = dir.path().join("f");
    fs::write(&file, "").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let error = connect_err(&config(dir.path(), Some(file))).await;
    assert_no_daemon(&error, dir.path(), "permission denied");
}

#[tokio::test]
async fn overlong_or_relative_socket_path_is_invalid_arguments() {
    let dir = project_dir();
    for socket in [
        dir.path().join("x".repeat(120)),
        PathBuf::from("relative.sock"),
    ] {
        let error = connect_err(&config(dir.path(), Some(socket))).await;
        assert!(matches!(error, ClientError::InvalidArguments(_)), "{error}");
        assert_eq!(error.exit_code(), 2);
    }
}

#[tokio::test(start_paused = true)]
async fn silent_daemon_times_out_the_handshake() {
    let dir = project_dir();
    let socket = dir.path().join("s");
    let listener = bind(&socket);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // Hold the connection open and never answer the hello.
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        drop(stream);
    });
    let error = connect_err(&config(dir.path(), Some(socket))).await;
    assert_no_daemon(&error, dir.path(), "timed out");
    server.abort();
}

#[tokio::test]
async fn garbage_handshake_is_handshake_failed() {
    let dir = project_dir();
    let socket = dir.path().join("s");
    let listener = bind(&socket);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"not json\n").await.unwrap();
    });
    let error = connect_err(&config(dir.path(), Some(socket))).await;
    assert_no_daemon(&error, dir.path(), "handshake failed");
    server.await.unwrap();
}

/// `connect` succeeded, so a listener is alive: a connection dropped before
/// any reply (the daemon's pre-handshake budget is full, SE-6) is capacity.
/// Depending on timing the client sees a lost peer credential, a failed hello
/// write or EOF; every one of them is busy, which `connect` redials, never
/// absence (which it would report at once). `busy_wait.rs` covers the text.
#[tokio::test]
async fn connection_closed_after_accept_before_any_reply_is_busy_not_absence() {
    for read_hello_first in [false, true] {
        let dir = project_dir();
        let socket = dir.path().join("s");
        let listener = bind(&socket);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            if read_hello_first {
                let mut read = FrameReader::new(BufReader::new(stream));
                let _: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
            } else {
                drop(stream);
            }
            answer(listener, welcome).await.unwrap()
        });
        let client = Client::connect(&config(dir.path(), Some(socket)))
            .await
            .unwrap_or_else(|e| panic!("read_hello_first={read_hello_first}: {e}"));
        assert_eq!(client.welcome.pid, 42);
        server.await.unwrap();
    }
}

/// Serve one hello on `socket` and answer it with `reply(hello)`.
fn answer(
    listener: UnixListener,
    reply: impl FnOnce(&HelloFrame) -> Welcome + Send + 'static,
) -> tokio::task::JoinHandle<HelloFrame> {
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        let welcome = reply(&hello);
        write_frame(
            &mut write,
            &HandshakeReply::welcome(welcome),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        hello
    })
}

fn welcome(hello: &HelloFrame) -> Welcome {
    Welcome {
        protocol: orch_proto::LOCAL_PROTOCOL,
        binary: BoundedString::new(orch_proto::BINARY_VERSION).unwrap(),
        project_id: hello.hello.project_id,
        project_root: hello.hello.project_root.clone(),
        instance: Id::new("instance").unwrap(),
        pid: 42,
        compatible: true,
        stop_state: None,
    }
}

#[tokio::test]
async fn version_skew_in_either_direction_names_both_versions_and_the_restart() {
    for (protocol, binary) in [
        (orch_proto::LOCAL_PROTOCOL + 1, orch_proto::BINARY_VERSION),
        (orch_proto::LOCAL_PROTOCOL - 1, orch_proto::BINARY_VERSION),
        (orch_proto::LOCAL_PROTOCOL, "0.0.1"),
    ] {
        let dir = project_dir();
        let socket = dir.path().join("s");
        let server = answer(bind(&socket), move |hello| Welcome {
            protocol,
            binary: BoundedString::new(binary).unwrap(),
            compatible: false,
            ..welcome(hello)
        });
        let error = connect_err(&config(dir.path(), Some(socket))).await;
        server.await.unwrap();
        assert_eq!(error.exit_code(), 7);
        let root = dir.path().canonicalize().unwrap();
        let detail = format!(
            "daemon is orch {binary} protocol {protocol}, this client is orch {} protocol {}",
            orch_proto::BINARY_VERSION,
            orch_proto::LOCAL_PROTOCOL
        );
        assert_eq!(
            error.render(false),
            format!(
                "  ✕ No orch daemon is running for {} ({detail})\n    Start it with: orch daemon stop, then orch daemon start",
                root.display()
            )
        );
        let agent = error.render(true);
        assert!(agent.contains("Do not start it"), "{agent}");
        assert!(agent.contains(&detail), "{agent}");
    }
}

#[tokio::test]
async fn incompatible_welcome_with_equal_versions_is_handshake_failed() {
    let dir = project_dir();
    let socket = dir.path().join("s");
    let server = answer(bind(&socket), |hello| Welcome {
        compatible: false,
        ..welcome(hello)
    });
    let error = connect_err(&config(dir.path(), Some(socket))).await;
    server.await.unwrap();
    assert_no_daemon(&error, dir.path(), "handshake failed");
}

#[tokio::test]
async fn the_hello_carries_the_resolved_identity_and_mode() {
    let dir = project_dir();
    let socket = dir.path().join("s");
    let server = answer(bind(&socket), welcome);
    let mut env = config(dir.path(), Some(socket));
    env.caller.agent_id = Some(Id::new("agt_env").unwrap());
    let cfg = env
        .with_agent(Some("agt_flag"))
        .unwrap()
        .with_frontend(CallerKind::Mcp)
        .with_mode(SessionMode::ReadOnly);
    let client = Client::connect(&cfg).await.unwrap();
    let hello = server.await.unwrap();
    let session = hello.hello.session_identity().unwrap();
    assert_eq!(session.caller.kind, CallerKind::Mcp);
    assert_eq!(
        session.caller.agent_id.as_deref(),
        Some("agt_flag"),
        "--agent wins over ORCH_AGENT_ID"
    );
    assert_eq!(session.mode, SessionMode::ReadOnly);
    assert_eq!(client.project().root, dir.path().canonicalize().unwrap());
}

#[test]
fn identity_resolution_and_agent_context() {
    let dir = project_dir();
    let base = config(dir.path(), None);

    // No flag keeps the environment's identity; an over-long flag is refused.
    let mut env = base.clone();
    env.caller.agent_id = Some(Id::new("agt_env").unwrap());
    let kept = env.clone().with_agent(None).unwrap();
    assert_eq!(kept.caller.agent_id.as_deref(), Some("agt_env"));
    let error = env.with_agent(Some(&"a".repeat(4096))).unwrap_err();
    assert_eq!(error.exit_code(), 2);

    // An owner's --agent on the CLI does not make it an agent context.
    let owner = base.clone().with_agent(Some("agt_1")).unwrap();
    assert!(!owner.agent_context);
    assert_eq!(owner.caller.kind, CallerKind::Cli);

    // An MCP bridge with a resolved identity renders the agent variant,
    // whichever order the builders run in; without one it does not.
    let mcp_first = base
        .clone()
        .with_frontend(CallerKind::Mcp)
        .with_agent(Some("agt_1"))
        .unwrap();
    let agent_first = base
        .clone()
        .with_agent(Some("agt_1"))
        .unwrap()
        .with_frontend(CallerKind::Mcp);
    assert!(mcp_first.agent_context && agent_first.agent_context);
    assert!(!base.clone().with_frontend(CallerKind::Mcp).agent_context);
    assert!(!base.clone().with_frontend(CallerKind::Tui).agent_context);

    // An agent context from the environment is never cleared.
    let mut task_env = base;
    task_env.agent_context = true;
    assert!(task_env.with_frontend(CallerKind::Web).agent_context);
}
