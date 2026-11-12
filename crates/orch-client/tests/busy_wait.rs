//! Board-2 resilience R1: `Client::connect` waits up to `BUSY_WAIT` for a
//! daemon that answers busy at the handshake, and redials nothing else. A
//! fake daemon on a real Unix socket scripts each accepted connection.
use orch_client::{BUSY_WAIT, Client, ClientConfig, ClientError};
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, HelloFrame, MAX_HANDSHAKE_BYTES, Reject, RejectCode,
    SessionMode, Welcome,
};
use orch_proto::ndjson::{FrameReader, write_frame};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use tokio::net::UnixListener;

/// What the fake daemon does with one accepted connection.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// Reject the hello with `busy` (session cap or handshake capacity full).
    Busy,
    /// Drop the connection at once, before reading anything (the daemon's
    /// overflow budget is also full, SE-6).
    Close,
    /// Read the hello, then drop the connection without a reply.
    CloseAfterHello,
    Welcome,
    /// A welcome from a daemon of another protocol.
    Skew,
    /// A definite reject that is not capacity.
    HandshakeFailed,
    /// Accept and never answer.
    Silent,
}

struct FakeDaemon {
    socket: PathBuf,
    accepted: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl FakeDaemon {
    /// Connection `n` gets `script(n)` (0-based).
    fn start(script: impl Fn(usize) -> Answer + Send + 'static) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("cb-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::create_dir(dir.path().join(".orchestry")).unwrap();
        let socket = dir.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let answer = script(count.fetch_add(1, Ordering::SeqCst));
                tokio::spawn(async move {
                    if matches!(answer, Answer::Close) {
                        return;
                    }
                    let (read, mut write) = stream.into_split();
                    let mut read = FrameReader::new(BufReader::new(read));
                    let Ok(Some(hello)) = read.read::<HelloFrame>(MAX_HANDSHAKE_BYTES).await else {
                        return;
                    };
                    let reply = match answer {
                        Answer::Close => unreachable!(),
                        Answer::CloseAfterHello => return,
                        Answer::Silent => {
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                            return;
                        }
                        Answer::Busy => reject(RejectCode::Busy),
                        Answer::HandshakeFailed => reject(RejectCode::HandshakeFailed),
                        Answer::Welcome => {
                            HandshakeReply::welcome(welcome(&hello, orch_proto::LOCAL_PROTOCOL))
                        }
                        Answer::Skew => HandshakeReply::welcome(Welcome {
                            compatible: false,
                            ..welcome(&hello, orch_proto::LOCAL_PROTOCOL + 1)
                        }),
                    };
                    let _ = write_frame(&mut write, &reply, MAX_HANDSHAKE_BYTES).await;
                });
            }
        });
        Self {
            socket,
            accepted,
            server,
            _dir: dir,
        }
    }

    fn root(&self) -> &Path {
        self.socket.parent().unwrap()
    }

    fn config(&self) -> ClientConfig {
        config(self.root(), Some(self.socket.clone()))
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.server.abort();
    }
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

fn reject(code: RejectCode) -> HandshakeReply {
    HandshakeReply::reject(Reject {
        code,
        message: BoundedString::new("handshake capacity is full").unwrap(),
        sessions: Some(1),
        cap: Some(64),
        project_root: None,
    })
}

fn welcome(hello: &HelloFrame, protocol: u32) -> Welcome {
    Welcome {
        protocol,
        binary: BoundedString::new(orch_proto::BINARY_VERSION).unwrap(),
        project_id: hello.hello.project_id,
        project_root: hello.hello.project_root.clone(),
        instance: Id::new("instance").unwrap(),
        pid: 42,
        compatible: true,
        stop_state: None,
    }
}

async fn timed_connect(config: &ClientConfig) -> (Result<Client, ClientError>, Duration) {
    let started = Instant::now();
    let result = Client::connect(config).await;
    (result, started.elapsed())
}

fn assert_busy(error: &ClientError) {
    assert_eq!(error.exit_code(), 7, "{error}");
    for agent_context in [false, true] {
        let text = error.render(agent_context);
        assert!(text.contains("daemon_busy"), "{text}");
        assert!(!text.contains("No orch daemon is running"), "{text}");
        assert!(!text.contains("not running"), "{text}");
        assert!(!text.contains("Start it"), "{text}");
    }
}

#[test]
fn the_budget_is_the_admission_deadline() {
    assert_eq!(BUSY_WAIT, orch_proto::frame::limits::ADMISSION_TIMEOUT);
    assert_eq!(BUSY_WAIT, Duration::from_secs(10));
}

/// Every form of busy is redialled: a busy reject, and a connection the live
/// listener dropped before any reply, with or without reading the hello.
#[tokio::test]
async fn busy_k_times_then_welcome_connects_after_k_retries() {
    for (k, busy) in [
        (1, Answer::Busy),
        (4, Answer::Busy),
        (3, Answer::Close),
        (3, Answer::CloseAfterHello),
    ] {
        let daemon = FakeDaemon::start(move |n| if n < k { busy } else { Answer::Welcome });
        let (result, elapsed) = timed_connect(&daemon.config()).await;
        let client = result.unwrap_or_else(|e| panic!("{busy:?} x{k}: {e}"));
        assert_eq!(client.welcome.pid, 42);
        assert_eq!(daemon.accepted(), k + 1, "{busy:?} x{k}");
        // Backoff 25 ms doubling (equal jitter): four retries sleep < 375 ms.
        assert!(
            elapsed < Duration::from_secs(2),
            "{busy:?} x{k}: {elapsed:?}"
        );
    }
}

#[tokio::test]
async fn busy_forever_is_daemon_busy_after_the_whole_budget() {
    let daemon = FakeDaemon::start(|_| Answer::Busy);
    let (result, elapsed) = timed_connect(&daemon.config()).await;
    let error = result.err().expect("busy forever must fail");
    assert_busy(&error);
    assert!(
        error.render(false).contains("1/64 sessions"),
        "the daemon's own counts are kept: {error}"
    );
    assert!(elapsed >= BUSY_WAIT, "{elapsed:?}");
    assert!(elapsed < BUSY_WAIT + Duration::from_secs(1), "{elapsed:?}");
    // Capped at 500 ms, the backoff redials at least a dozen times in 10 s.
    assert!(daemon.accepted() >= 12, "{} attempts", daemon.accepted());
}

#[tokio::test]
async fn dropped_before_reply_forever_is_daemon_busy_after_the_whole_budget() {
    let daemon = FakeDaemon::start(|_| Answer::Close);
    let (result, elapsed) = timed_connect(&daemon.config()).await;
    let error = result.err().expect("closing forever must fail");
    assert_busy(&error);
    assert!(elapsed >= BUSY_WAIT, "{elapsed:?}");
    assert!(elapsed < BUSY_WAIT + Duration::from_secs(1), "{elapsed:?}");
}

/// A stop may already have been acted on, so even a busy answer is final.
#[tokio::test]
async fn a_stop_intent_never_retries() {
    for busy in [Answer::Busy, Answer::Close, Answer::CloseAfterHello] {
        let daemon = FakeDaemon::start(move |_| busy);
        let started = Instant::now();
        let error = orch_client::stop(&daemon.config()).await.unwrap_err();
        assert_eq!(error.exit_code(), 7, "{busy:?}: {error}");
        assert!(started.elapsed() < Duration::from_secs(1), "{busy:?}");
        // Give a redial, if there were one, time to reach the listener.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(daemon.accepted(), 1, "{busy:?}");
    }
}

#[tokio::test]
async fn definite_answers_are_never_retried() {
    for (answer, detail) in [
        (Answer::Skew, "protocol"),
        (Answer::HandshakeFailed, "handshake failed"),
    ] {
        let daemon = FakeDaemon::start(move |_| answer);
        let (result, elapsed) = timed_connect(&daemon.config()).await;
        let error = result.err().expect("must fail");
        assert!(error.render(false).contains(detail), "{error}");
        assert!(elapsed < Duration::from_secs(1), "{answer:?}: {elapsed:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(daemon.accepted(), 1, "{answer:?}");
    }
}

#[tokio::test]
async fn a_hung_daemon_times_out_once_within_the_handshake_deadline() {
    let daemon = FakeDaemon::start(|_| Answer::Silent);
    let (result, elapsed) = timed_connect(&daemon.config()).await;
    let error = result.err().expect("a silent daemon must fail");
    assert!(error.render(false).contains("(timed out)"), "{error}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(daemon.accepted(), 1);
}

#[tokio::test]
async fn an_absent_daemon_still_fails_at_once() {
    let dir = tempfile::Builder::new()
        .prefix("cb-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir(dir.path().join(".orchestry")).unwrap();
    let missing = dir.path().join("s");
    let (result, elapsed) = timed_connect(&config(dir.path(), Some(missing.clone()))).await;
    let error = result.err().expect("no socket must fail");
    assert!(error.render(false).contains("(no socket)"), "{error}");
    assert!(elapsed < Duration::from_millis(200), "{elapsed:?}");

    // A stale socket file left by a dead daemon: connection refused.
    drop(UnixListener::bind(&missing).unwrap());
    fs::set_permissions(&missing, fs::Permissions::from_mode(0o600)).unwrap();
    let (result, elapsed) = timed_connect(&config(dir.path(), Some(missing))).await;
    let error = result.err().expect("refused must fail");
    assert!(
        error.render(false).contains("(connection refused)"),
        "{error}"
    );
    assert!(elapsed < Duration::from_millis(200), "{elapsed:?}");
}

#[tokio::test]
async fn an_unsafe_socket_mode_is_permission_denied_at_once() {
    let daemon = FakeDaemon::start(|_| Answer::Welcome);
    fs::set_permissions(&daemon.socket, fs::Permissions::from_mode(0o666)).unwrap();
    let (result, elapsed) = timed_connect(&daemon.config()).await;
    let error = result.err().expect("unsafe mode must fail");
    assert!(
        error.render(false).contains("(permission denied)"),
        "{error}"
    );
    assert!(elapsed < Duration::from_millis(200), "{elapsed:?}");
    assert_eq!(daemon.accepted(), 0);
}
