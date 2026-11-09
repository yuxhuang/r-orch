use orch_client::ClientConfig;
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, HelloFrame, Intent, MAX_HANDSHAKE_BYTES, SessionMode,
    StopState, Welcome,
};
use orch_proto::ndjson::{FrameReader, write_frame};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;

fn fixture() -> (tempfile::TempDir, UnixListener, ClientConfig) {
    let dir = tempfile::Builder::new()
        .prefix("cs-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir(dir.path().join(".orchestry")).unwrap();
    let socket = dir.path().join("s");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let config = ClientConfig {
        cwd: dir.path().into(),
        project_root: Some(dir.path().into()),
        socket: Some(socket),
        test_socket_root: Some(dir.path().into()),
        agent_context: false,
        caller: Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::Normal,
    };
    (dir, listener, config)
}
fn welcome(hello: &HelloFrame, state: StopState) -> Welcome {
    Welcome {
        protocol: orch_proto::LOCAL_PROTOCOL + 1,
        binary: BoundedString::new("future").unwrap(),
        project_root: hello.hello.project_root.clone(),
        project_id: hello.hello.project_id,
        instance: Id::new("instance").unwrap(),
        pid: 42,
        compatible: false,
        stop_state: Some(state),
    }
}

#[tokio::test]
async fn frozen_stop_completes_across_binary_and_protocol_skew() {
    let (_dir, listener, cfg) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(reader));
        let hello: HelloFrame = reader.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        assert_eq!(hello.hello.intent, Intent::Stop);
        for state in [StopState::Accepted, StopState::Completed] {
            write_frame(
                &mut writer,
                &HandshakeReply::welcome(welcome(&hello, state)),
                MAX_HANDSHAKE_BYTES,
            )
            .await
            .unwrap();
        }
        assert!(
            reader
                .read_line(MAX_HANDSHAKE_BYTES)
                .await
                .unwrap()
                .is_none()
        );
    });
    assert!(orch_client::stop(&cfg).await.unwrap().clean);
    server.await.unwrap();
}

#[tokio::test]
async fn stop_acceptance_without_cleanup_completion_is_failure() {
    let (_dir, listener, cfg) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(reader));
        let hello: HelloFrame = reader.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut writer,
            &HandshakeReply::welcome(welcome(&hello, StopState::Accepted)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
    });
    let error = orch_client::stop(&cfg).await.unwrap_err();
    assert_eq!(error.exit_code(), 7);
    let text = error.render(false);
    assert!(text.contains("daemon accepted stop"), "{text}");
    assert!(text.contains("inspect state before retrying"), "{text}");
    assert!(!text.contains("Start it with"), "{text}");
    server.await.unwrap();
}

#[tokio::test]
async fn failed_cleanup_reject_preserves_failure_instead_of_claiming_success() {
    let (_dir, listener, cfg) = fixture();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(reader));
        let hello: HelloFrame = reader.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut writer,
            &HandshakeReply::welcome(welcome(&hello, StopState::Accepted)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        let reject = orch_proto::handshake::Reject {
            code: orch_proto::handshake::RejectCode::StopFailed,
            message: BoundedString::new("secret-cleanup-text").unwrap(),
            sessions: None,
            cap: None,
            project_root: None,
        };
        write_frame(
            &mut writer,
            &HandshakeReply::reject(reject),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
    });
    let error = orch_client::stop(&cfg).await.unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(error.to_string().contains("cleanup failed"));
    assert!(!error.to_string().contains("secret-cleanup-text"));
    server.await.unwrap();
}

#[tokio::test]
async fn accepted_stop_deadline_has_its_own_diagnosis() {
    let (_dir, listener, cfg) = fixture();
    let (accepted, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(reader));
        let hello: HelloFrame = reader.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut writer,
            &HandshakeReply::welcome(welcome(&hello, StopState::Accepted)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        accepted.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let stopping = tokio::spawn(async move { orch_client::stop(&cfg).await });
    observed.await.unwrap();
    // Let the client read acceptance before advancing only Tokio's clock.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let error = stopping.await.unwrap().unwrap_err();
    assert_eq!(error.exit_code(), 7);
    let text = error.render(false);
    assert!(
        text.contains("daemon accepted stop but PID 42 is still running after 30 s"),
        "{text}"
    );
    assert!(text.contains("inspect state before retrying"), "{text}");
    assert!(!text.contains("Start it with"), "{text}");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn completed_reply_with_owner_guard_still_held_times_out() {
    use nix::fcntl::{Flock, FlockArg};
    use std::os::unix::fs::OpenOptionsExt;
    let (dir, listener, cfg) = fixture();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.path().join(".orchestry/.daemon-owner.lock"))
        .unwrap();
    let _guard = Flock::lock(file, FlockArg::LockExclusiveNonblock).unwrap();
    let (closed, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(reader));
        let hello: HelloFrame = reader.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        for state in [StopState::Accepted, StopState::Completed] {
            let mut reply = welcome(&hello, state);
            reply.pid = std::process::id();
            write_frame(
                &mut writer,
                &HandshakeReply::welcome(reply),
                MAX_HANDSHAKE_BYTES,
            )
            .await
            .unwrap();
        }
        // Stop must close its session before checking release, so drain can finish.
        assert!(
            reader
                .read_line(MAX_HANDSHAKE_BYTES)
                .await
                .unwrap()
                .is_none()
        );
        closed.send(()).unwrap();
    });
    let stopping = tokio::spawn(async move { orch_client::stop(&cfg).await });
    observed.await.unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let error = stopping.await.unwrap().unwrap_err();
    assert_eq!(error.exit_code(), 7);
    assert!(error.render(false).contains(&format!(
        "daemon accepted stop but PID {} is still running after 30 s",
        std::process::id()
    )));
    server.await.unwrap();
}
