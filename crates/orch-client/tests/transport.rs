use orch_client::{Client, Project};
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::frame::{MAX_FRAME_BYTES, OkFrame, RequestFrame};
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, HelloFrame, MAX_HANDSHAKE_BYTES, SessionMode, Welcome,
};
use orch_proto::ndjson::{FrameReader, write_frame};
use orch_proto::path::WirePath;
use std::fs;
use tokio::io::{AsyncWriteExt, BufReader};

fn project() -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".orchestry")).unwrap();
    let p = Project::discover(dir.path(), None).unwrap();
    (dir, p)
}
fn caller() -> Caller {
    Caller {
        kind: CallerKind::Cli,
        agent_id: None,
        pid: None,
    }
}
fn welcome(hello: &HelloFrame) -> Welcome {
    Welcome {
        protocol: orch_proto::LOCAL_PROTOCOL,
        binary: BoundedString::new(orch_proto::BINARY_VERSION).unwrap(),
        project_id: hello.hello.project_id,
        project_root: hello.hello.project_root.clone(),
        instance: Id::new("test-instance").unwrap(),
        pid: 42,
        compatible: true,
        stop_state: None,
    }
}

#[tokio::test]
async fn sequential_calls_correlate_ids_and_decode_typed_results() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut write,
            &HandshakeReply::welcome(welcome(&hello)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        for expected in 1..=2 {
            let req: RequestFrame = read.read(MAX_FRAME_BYTES).await.unwrap().unwrap();
            assert_eq!(req.id.0.get(), expected);
            let response = OkFrame {
                id: req.id,
                ok: serde_json::value::to_raw_value(&orch_proto::ops::results::DispatchState {
                    dispatch_on: expected == 1,
                    batch_active: false,
                })
                .unwrap(),
                commit_seq: expected,
            };
            write_frame(&mut write, &response, MAX_FRAME_BYTES)
                .await
                .unwrap();
        }
    });
    let mut client = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .unwrap();
    assert!(
        client
            .dispatch(orch_proto::ops::args::DispatchSwitch::On)
            .await
            .unwrap()
            .dispatch_on
    );
    assert!(
        !client
            .dispatch(orch_proto::ops::args::DispatchSwitch::Off)
            .await
            .unwrap()
            .dispatch_on
    );
    task.await.unwrap();
}

#[tokio::test]
async fn strict_versions_reject_both_protocol_directions_and_binary_skew() {
    for (protocol, binary) in [
        (0, orch_proto::BINARY_VERSION),
        (orch_proto::LOCAL_PROTOCOL + 1, orch_proto::BINARY_VERSION),
        (orch_proto::LOCAL_PROTOCOL, "future"),
    ] {
        let (_dir, p) = project();
        let (client, server) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut read = FrameReader::new(BufReader::new(read));
            let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
            let mut reply = welcome(&hello);
            reply.protocol = protocol;
            reply.binary = BoundedString::new(binary).unwrap();
            write_frame(
                &mut write,
                &HandshakeReply::welcome(reply),
                MAX_HANDSHAKE_BYTES,
            )
            .await
            .unwrap();
            assert!(
                read.read_line(MAX_FRAME_BYTES).await.unwrap().is_none(),
                "no operation may follow skew"
            );
        });
        let error = Client::from_stream(p, client, caller(), SessionMode::Normal)
            .await
            .err()
            .unwrap();
        assert_eq!(error.exit_code(), 7);
        assert!(
            error
                .to_string()
                .contains("orch daemon stop, then orch daemon start")
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn wrong_project_is_not_version_skew_or_permission_to_stop() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        let mut reply = welcome(&hello);
        reply.project_id.ino += 1;
        reply.project_root = WirePath::new("/other").unwrap();
        write_frame(
            &mut write,
            &HandshakeReply::welcome(reply),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
    });
    let error = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .err()
        .unwrap();
    assert_eq!(error.exit_code(), 7);
    assert!(error.to_string().contains("wrong_project"));
    assert!(!error.to_string().contains("Start it"));
    task.await.unwrap();
}

#[tokio::test]
async fn wrong_response_id_poisons_lane_and_never_replays() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut write,
            &HandshakeReply::welcome(welcome(&hello)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        let _: RequestFrame = read.read(MAX_FRAME_BYTES).await.unwrap().unwrap();
        write
            .write_all(b"{\"id\":99,\"ok\":null,\"commit_seq\":1}\n")
            .await
            .unwrap();
        assert!(read.read_line(MAX_FRAME_BYTES).await.unwrap().is_none());
    });
    let mut client = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .unwrap();
    assert!(client.daemon_stop().await.is_err());
    assert!(client.daemon_stop().await.is_err());
    drop(client);
    task.await.unwrap();
}

#[tokio::test]
async fn busy_is_capacity_not_absence_and_reject_text_is_not_echoed() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let _: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        let reject = orch_proto::handshake::Reject {
            code: orch_proto::handshake::RejectCode::Busy,
            message: BoundedString::new("secret-canary").unwrap(),
            sessions: Some(64),
            cap: Some(64),
            project_root: None,
        };
        write_frame(
            &mut write,
            &HandshakeReply::reject(reject),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
    });
    let error = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .err()
        .unwrap();
    assert_eq!(error.exit_code(), 7);
    let text = error.render(false);
    assert!(text.contains("daemon_busy"));
    assert!(text.contains("64/64 sessions"));
    assert!(!text.contains("Start it"));
    assert!(!text.contains("secret-canary"));
    task.await.unwrap();
}

#[tokio::test]
async fn malformed_handshake_uses_uniform_diagnosis_without_peer_text() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let _: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write.write_all(b"{\"secret-canary\":}\n").await.unwrap();
    });
    let error = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .err()
        .unwrap();
    assert_eq!(error.exit_code(), 7);
    let text = error.render(false);
    assert!(text.contains("No orch daemon is running"));
    assert!(text.contains("handshake failed"));
    assert!(!text.contains("secret-canary"));
    task.await.unwrap();
}

#[tokio::test]
async fn session_frames_enforce_depth_before_decoding_payloads() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut write,
            &HandshakeReply::welcome(welcome(&hello)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        let _: RequestFrame = read.read(MAX_FRAME_BYTES).await.unwrap().unwrap();
        let reply = format!(
            "{{\"id\":1,\"ok\":{}null{},\"commit_seq\":1}}\n",
            "[".repeat(64),
            "]".repeat(64)
        );
        write.write_all(reply.as_bytes()).await.unwrap();
    });
    let mut client = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .unwrap();
    let reply: Result<serde_json::Value, _> = client
        .call(orch_proto::ops::Request::DaemonStatus(
            orch_proto::ops::args::DaemonStatusArgs {},
        ))
        .await;
    assert!(reply.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn committed_mutation_with_lost_reply_requires_inspection_before_retry() {
    let (dir, p) = project();
    let committed = dir.path().join("committed");
    let observed = committed.clone();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut write,
            &HandshakeReply::welcome(welcome(&hello)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        let request: RequestFrame = read.read(MAX_FRAME_BYTES).await.unwrap().unwrap();
        let orch_proto::ops::Request::ContextSet(args) = request.req else {
            panic!("expected context.set")
        };
        fs::write(observed, args.value.as_str()).unwrap();
        // The mutation committed, but its terminal response never arrived.
    });
    let mut client = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .unwrap();
    let error = client
        .call::<()>(orch_proto::ops::Request::ContextSet(
            orch_proto::ops::args::ContextSetArgs {
                key: "replylost".try_into().unwrap(),
                value: "committed".try_into().unwrap(),
                ttl_ms: None,
            },
        ))
        .await
        .unwrap_err();
    task.await.unwrap();
    assert_eq!(fs::read_to_string(committed).unwrap(), "committed");
    assert_eq!(error.exit_code(), 7);
    for agent in [false, true] {
        let text = error.render(agent);
        assert!(
            text.contains("the request may have been applied; inspect state before retrying"),
            "{text}"
        );
        assert!(!text.contains("Start it with"), "{text}");
        assert!(!text.contains("not_executed"), "{text}");
    }
    assert!(
        client
            .status()
            .await
            .unwrap_err()
            .render(false)
            .contains("inspect state before retrying")
    );
}

#[tokio::test(start_paused = true)]
async fn control_deadline_poisons_lane_without_replay() {
    let (_dir, p) = project();
    let (client, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut read = FrameReader::new(BufReader::new(read));
        let hello: HelloFrame = read.read(MAX_HANDSHAKE_BYTES).await.unwrap().unwrap();
        write_frame(
            &mut write,
            &HandshakeReply::welcome(welcome(&hello)),
            MAX_HANDSHAKE_BYTES,
        )
        .await
        .unwrap();
        let _: RequestFrame = read.read(MAX_FRAME_BYTES).await.unwrap().unwrap();
        // No reply: the client deadline expires, and a second call cannot write.
        assert!(read.read_line(MAX_FRAME_BYTES).await.unwrap().is_none());
    });
    let mut client = Client::from_stream(p, client, caller(), SessionMode::Normal)
        .await
        .unwrap();
    let error = client
        .dispatch(orch_proto::ops::args::DispatchSwitch::On)
        .await
        .unwrap_err();
    assert_eq!(error.exit_code(), 7);
    assert!(
        error
            .render(false)
            .contains("inspect state before retrying")
    );
    assert!(
        client
            .status()
            .await
            .unwrap_err()
            .render(false)
            .contains("inspect state before retrying")
    );
    drop(client);
    task.await.unwrap();
}
