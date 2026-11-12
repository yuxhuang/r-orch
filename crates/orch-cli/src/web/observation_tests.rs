//! Real framed observation transport; no daemon or child process is started.
use super::*;
use orch_client::{Client, Project, testing::FakeServer};
use orch_proto::{
    bounded::Id,
    frame::OkFrame,
    handshake::{Caller, SessionMode},
    ops::{Request as DaemonRequest, results::SubscribeAck},
};
use tokio::sync::mpsc;

type Peers = mpsc::UnboundedReceiver<FakeServer>;

fn fixture() -> (tempfile::TempDir, Arc<WebState>, Peers, Arc<Mutex<String>>) {
    let root = tempfile::tempdir().expect("valid test fixture");
    std::fs::create_dir(root.path().join(".orchestry")).expect("valid test fixture");
    let path = root.path().to_path_buf();
    let (send, peers) = mpsc::unbounded_channel();
    let instance = Arc::new(Mutex::new(String::from("first-instance")));
    let current = instance.clone();
    let connector = crate::tui::Connector::new(path.clone(), false, move || {
        let project = Project::discover(&path, None).expect("valid test fixture");
        let (transport, mut peer) = orch_client::testing::fake_transport();
        let send = send.clone();
        let instance = current.lock().expect("valid test fixture").clone();
        Box::pin(async move {
            tokio::spawn(async move {
                let hello = peer
                    .accept(Id::new(instance).expect("valid test fixture"))
                    .await
                    .expect("valid test fixture");
                assert_eq!(
                    hello
                        .hello
                        .session_identity()
                        .expect("valid test fixture")
                        .caller
                        .kind,
                    CallerKind::Web
                );
                assert!(send.send(peer).is_ok());
            });
            Client::from_stream(
                project,
                transport,
                Caller {
                    kind: CallerKind::Web,
                    agent_id: None,
                    pid: None,
                },
                SessionMode::Normal,
            )
            .await
        })
    });
    let state = Arc::new(WebState::new(
        &Auth::Token("credential".into()),
        HostPolicy::new(true, vec![]),
        SpawnSpec {
            program: "/must-not-spawn".into(),
            args: vec![],
            cwd: root.path().into(),
        },
        connector,
    ));
    (root, state, peers, instance)
}

async fn acknowledge(peer: &mut FakeServer, instance: &str) {
    let request = tokio::time::timeout(Duration::from_secs(3), peer.recv())
        .await
        .expect("parent must subscribe, not just probe")
        .expect("valid test fixture");
    assert!(
        request.is_some(),
        "parent dropped its preflight instead of holding observation"
    );
    let request = request.expect("valid test fixture");
    let DaemonRequest::Subscribe(args) = request.req else {
        panic!("expected subscribe")
    };
    assert_eq!(&*args.stores, &[orch_proto::frame::Store::State]);
    assert!(!args.bus);
    assert!(args.filter.is_none());
    peer.send(&OkFrame {
        id: request.id,
        commit_seq: 9000,
        ok: serde_json::value::to_raw_value(&SubscribeAck {
            subscription: 1,
            instance: instance.into(),
            seq: 0,
        })
        .expect("valid test fixture"),
    })
    .await
    .expect("valid test fixture");
}

#[tokio::test]
async fn parent_holds_observation_without_http_request() {
    let (_root, state, mut peers, _instance) = fixture();
    let check = {
        let state = state.clone();
        tokio::spawn(async move { state.daemon_check().await })
    };
    let mut peer = peers.recv().await.expect("valid test fixture");
    acknowledge(&mut peer, "first-instance").await;
    check
        .await
        .expect("valid test fixture")
        .expect("valid test fixture");
    // The parent keeps the session while idle, with no periodic requests.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), peer.recv())
            .await
            .is_err()
    );
    state.shutdown();
}

async fn establish(state: &Arc<WebState>, peers: &mut Peers, instance: &str) -> FakeServer {
    let check = {
        let state = state.clone();
        tokio::spawn(async move { state.daemon_check().await })
    };
    let mut peer = peers.recv().await.expect("valid test fixture");
    acknowledge(&mut peer, instance).await;
    check
        .await
        .expect("valid test fixture")
        .expect("valid test fixture");
    peer
}

#[tokio::test]
async fn parent_notices_eof_and_typed_error_without_request_and_retry_uses_new_instance() {
    for malformed in [false, true] {
        let (_root, state, mut peers, instance) = fixture();
        let mut peer = establish(&state, &mut peers, "first-instance").await;
        let mut status = state
            .observation
            .lock()
            .await
            .as_ref()
            .expect("valid test fixture")
            .status
            .clone();
        if malformed {
            peer.send_raw(b"not a frame\n")
                .await
                .expect("valid test fixture");
        } else {
            drop(peer);
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while status.borrow().is_ok() {
                status.changed().await.expect("valid test fixture");
            }
        })
        .await
        .expect("idle EOF/typed failure must mark the parent unavailable");
        assert_eq!(status.borrow().as_ref().unwrap_err().exit_code, 7);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), peers.recv())
                .await
                .is_err(),
            "transport loss must not run an automatic reconnect loop"
        );
        *instance.lock().expect("valid test fixture") = "restarted-instance".into();
        let _fresh = establish(&state, &mut peers, "restarted-instance").await;
        let held = state.observation.lock().await;
        assert_eq!(
            held.as_ref()
                .expect("valid test fixture")
                .status
                .borrow()
                .as_ref()
                .expect("valid test fixture")
                .as_str(),
            "restarted-instance"
        );
        state.shutdown();
    }
}

#[tokio::test]
async fn gap_resync_and_changed_instance_replace_the_observation() {
    use orch_proto::frame::{ChangeKind, Event, EventBody, EventFrame, ResyncReason, Store};
    let bodies = [
        (
            2,
            "first-instance",
            EventBody::Change {
                store: Store::State,
                id: None,
                kind: ChangeKind::Updated,
                commit_seq: 9001,
            },
        ),
        (
            1,
            "first-instance",
            EventBody::Resync {
                reason: ResyncReason::Lagged,
            },
        ),
        (
            1,
            "restarted-instance",
            EventBody::Change {
                store: Store::State,
                id: None,
                kind: ChangeKind::Updated,
                commit_seq: 1,
            },
        ),
    ];
    for (seq, event_instance, body) in bodies {
        let (_root, state, mut peers, instance) = fixture();
        let mut old = establish(&state, &mut peers, "first-instance").await;
        let mut status = state
            .observation
            .lock()
            .await
            .as_ref()
            .expect("valid test fixture")
            .status
            .clone();
        *instance.lock().expect("valid test fixture") = "restarted-instance".into();
        old.send(&EventFrame {
            ev: Event {
                subscription: 1,
                instance: Id::new(event_instance).expect("valid test fixture"),
                seq,
                body,
            },
        })
        .await
        .expect("valid test fixture");
        let mut fresh = tokio::time::timeout(Duration::from_secs(3), peers.recv())
            .await
            .expect("resync must re-establish observation")
            .expect("valid test fixture");
        assert!(
            status.borrow().is_err(),
            "parent is unavailable until new ack"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(3), old.recv())
                .await
                .expect("valid test fixture")
                .expect("valid test fixture")
                .is_none(),
            "old observation must be closed before replacement"
        );
        acknowledge(&mut fresh, "restarted-instance").await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if status
                    .borrow()
                    .as_ref()
                    .is_ok_and(|id| id.as_str() == "restarted-instance")
                {
                    break;
                }
                status.changed().await.expect("valid test fixture");
            }
        })
        .await
        .expect("valid test fixture");
        state.shutdown();
    }
}

#[tokio::test]
async fn readiness_checks_share_one_observation_and_shutdown_releases_it() {
    let (_root, state, mut peers, _instance) = fixture();
    let mut observation = establish(&state, &mut peers, "first-instance").await;
    let checks = (0..4)
        .map(|_| {
            let state = state.clone();
            tokio::spawn(async move { state.daemon_check().await })
        })
        .collect::<Vec<_>>();
    for check in checks {
        check
            .await
            .expect("valid test fixture")
            .expect("valid test fixture");
        let mut preflight = peers.recv().await.expect("valid test fixture");
        assert!(
            preflight
                .recv()
                .await
                .expect("valid test fixture")
                .is_none(),
            "readiness probe must not create another subscription"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observation.recv())
            .await
            .is_err()
    );
    state.shutdown();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), observation.recv())
            .await
            .expect("valid test fixture")
            .expect("valid test fixture")
            .is_none()
    );
}

async fn http(addr: SocketAddr, path: &str, host: &str, upgrade: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("valid test fixture");
    let websocket = if upgrade {
        "Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
    } else {
        ""
    };
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{websocket}\r\n")
                .as_bytes(),
        )
        .await
        .expect("valid test fixture");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("valid test fixture");
    response
}

#[tokio::test]
async fn unavailable_page_and_ws_preserve_auth_host_order_and_never_spawn_pty() {
    for auth in [Auth::Token("credential".into()), Auth::Open] {
        let root = tempfile::tempdir().expect("valid test fixture");
        let marker = root.path().join("pty-started");
        let calls = Arc::new(AtomicUsize::new(0));
        let attempts = calls.clone();
        let path = root.path().to_path_buf();
        let connector = crate::tui::Connector::new(path.clone(), false, move || {
            attempts.fetch_add(1, Ordering::SeqCst);
            let error = orch_client::NoDaemon::no_daemon(
                &path.to_string_lossy(),
                orch_proto::error::NoDaemonDetail::ConnectionLost,
            );
            Box::pin(async move { Err(error.into()) })
        });
        let state = Arc::new(WebState::new(
            &auth,
            HostPolicy::new(true, vec![]),
            SpawnSpec {
                program: "sh".into(),
                args: vec!["-c".into(), "touch pty-started".into()],
                cwd: root.path().into(),
            },
            connector,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("valid test fixture");
        let addr = listener.local_addr().expect("valid test fixture");
        let service = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
        let server = tokio::spawn(async move {
            axum::serve(listener, service)
                .await
                .expect("valid test fixture");
        });
        let denied = http(addr, "/?token=credential", "evil.example", false).await;
        assert!(denied.starts_with("HTTP/1.1 403"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if auth != Auth::Open {
            let unauthenticated = http(addr, "/", &addr.to_string(), false).await;
            assert!(unauthenticated.starts_with("HTTP/1.1 401"));
            assert!(unauthenticated.contains("authentication required"));
            assert!(!unauthenticated.contains(&root.path().to_string_lossy().to_string()));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        for (path, upgrade) in [
            ("/?token=credential", false),
            ("/ws?token=credential", true),
        ] {
            let response = http(addr, path, &addr.to_string(), upgrade).await;
            assert!(response.starts_with("HTTP/1.1 503"), "{response}");
            if auth == Auth::Open {
                assert!(response.ends_with(DAEMON_UNAVAILABLE));
                assert!(!response.contains(&root.path().to_string_lossy().to_string()));
            } else {
                assert!(response.contains(&root.path().to_string_lossy().to_string()));
                assert!(response.contains("orch daemon start"));
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.live_sessions(), 0);
        assert!(
            !marker.exists(),
            "a failed preflight must never start a PTY"
        );
        state.shutdown();
        server.abort();
    }
}

#[tokio::test]
async fn request_welcome_with_new_instance_replaces_still_open_observation() {
    let (_root, state, mut peers, instance) = fixture();
    let mut old = establish(&state, &mut peers, "first-instance").await;
    *instance.lock().expect("valid test fixture") = "restarted-instance".into();
    let _fresh = establish(&state, &mut peers, "restarted-instance").await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), old.recv())
            .await
            .expect("valid test fixture")
            .expect("valid test fixture")
            .is_none()
    );
    assert_eq!(
        state
            .observation
            .lock()
            .await
            .as_ref()
            .expect("valid test fixture")
            .status
            .borrow()
            .as_ref()
            .expect("valid test fixture")
            .as_str(),
        "restarted-instance"
    );
    state.shutdown();
}
