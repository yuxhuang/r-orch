#![cfg(feature = "testing")]
//! Peer state work enters the project's operation boundary (design D2, spec
//! R12, R19; board 1 finding C1).
//!
//! Each vector holds the daemon engine's own operation permit, drives one peer
//! state phase over the unchanged TCP protocol, and requires that the phase
//! waits for the release and then commits with an advanced commit sequence. A
//! phase that bypasses the boundary would change state under the held
//! operation and leave the sequence where it was.
//!
//! Both directions are covered: the outbound link (this daemon dials a fake
//! worker) and the inbound worker (a fake controller dials this daemon's
//! `--listen` address).

mod support;

use orch_daemon::testing::DaemonHarness;
use orch_domain::{Message, MessageChannel, MessageStatus};
use orch_engine::container::OperationBoundary;
use orch_peer::codec::{read_frame, write_frame};
use orch_peer::protocol::{Frame, PROTOCOL};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use support::{DEADLINE, Raw, ok};
use tokio::io::BufReader;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

/// Long enough for every peer timer involved (link poll 250 ms / 1 s, worker
/// tick 1 s) to have run at least twice while the permit is held.
const HELD_FOR: Duration = Duration::from_millis(2_500);

/// The project permit, taken by a test task the way any other operation
/// takes it, and held until [`Held::release`].
struct Held {
    boundary: Arc<OperationBoundary>,
    seq: u64,
    release: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Held {
    async fn take(daemon: &DaemonHarness) -> Held {
        let boundary = daemon.engine().operation_boundary();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let task = {
            let boundary = boundary.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                boundary
                    .run(false, async {
                        entered.notify_one();
                        release.notified().await;
                    })
                    .await;
            })
        };
        entered.notified().await;
        let seq = boundary.current_commit_seq();
        Held {
            boundary,
            seq,
            release,
            task,
        }
    }

    /// The commit sequence must not move while the permit is held: nothing
    /// may commit under another operation.
    fn assert_nothing_committed(&self) {
        assert_eq!(
            self.boundary.current_commit_seq(),
            self.seq,
            "a state operation committed while another held the project permit"
        );
    }

    async fn release(self) -> (Arc<OperationBoundary>, u64) {
        self.assert_nothing_committed();
        self.release.notify_one();
        self.task.await.unwrap();
        (self.boundary, self.seq)
    }
}

/// One TCP peer session. A reader task owns the partial line, so a bounded
/// wait on [`Peer::next`] never loses a frame.
struct Peer {
    frames: mpsc::Receiver<Frame>,
    writer: OwnedWriteHalf,
    _reader: JoinHandle<()>,
}

impl Peer {
    fn new(socket: TcpStream) -> Peer {
        let (read, writer) = socket.into_split();
        let (tx, frames) = mpsc::channel(64);
        let reader = tokio::spawn(async move {
            let mut read = BufReader::new(read);
            while let Ok(Some(frame)) = read_frame(&mut read).await {
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
        });
        Peer {
            frames,
            writer,
            _reader: reader,
        }
    }

    async fn send(&mut self, frame: &Frame) {
        write_frame(&mut self.writer, frame).await.unwrap();
    }

    /// The next frame accepted by `want` within `wait`, answering pings and
    /// skipping everything else; `None` when `wait` elapses.
    async fn next(&mut self, wait: Duration, want: impl Fn(&Frame) -> bool) -> Option<Frame> {
        let until = tokio::time::Instant::now() + wait;
        loop {
            let frame = tokio::time::timeout_at(until, self.frames.recv())
                .await
                .ok()?
                .expect("peer session closed");
            if matches!(frame, Frame::Ping) {
                self.send(&Frame::Pong).await;
            } else if want(&frame) {
                return Some(frame);
            }
        }
    }
}

fn peer_msg(id: &str, to: &str) -> Frame {
    Frame::Msg {
        id: id.into(),
        from: "sender".into(),
        to: to.into(),
        subject: "boundary".into(),
        body: "ordered with every other operation".into(),
        reply_to: None,
    }
}

/// A pending message queued for `to` (`agent@peer`), as `msg send` leaves it.
async fn queue(daemon: &DaemonHarness, id: &str, to: &str) {
    daemon
        .engine()
        .messages
        .save(&Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: "cli".into(),
            to_agent_id: Some(to.into()),
            subject: "outbound".into(),
            body: "for the peer".into(),
            created_at: "2026-11-09T00:00:00.000Z".into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        })
        .await
        .unwrap();
}

async fn status(daemon: &DaemonHarness, id: &str) -> Option<MessageStatus> {
    daemon
        .engine()
        .messages
        .get(id)
        .await
        .unwrap()
        .map(|m| m.status)
}

/// Register remote `probe` at a fake worker and complete the link's
/// handshake through its sync fence.
async fn linked_worker(daemon: &DaemonHarness) -> Peer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut raw = Raw::session(daemon, "cli", "normal").await;
    let added = raw
        .request(
            1,
            "remote.add",
            json!({"alias": "probe", "addr": listener.local_addr().unwrap().to_string()}),
        )
        .await;
    ok(&added);
    let (socket, _) = tokio::time::timeout(DEADLINE, listener.accept())
        .await
        .expect("the daemon dials its remote")
        .unwrap();
    let mut peer = Peer::new(socket);
    peer.next(DEADLINE, |f| matches!(f, Frame::Hello { .. }))
        .await
        .expect("hello");
    peer.send(&Frame::Welcome {
        proto: PROTOCOL,
        instance_id: "probe_peer".into(),
        name: "probe".into(),
        os: "test".into(),
        agents: vec![],
        version: None,
    })
    .await;
    peer.next(DEADLINE, |f| matches!(f, Frame::Sync { .. }))
        .await
        .expect("sync");
    peer.send(&Frame::SyncState { items: vec![] }).await;
    peer
}

async fn link_daemon() -> DaemonHarness {
    DaemonHarness::builder()
        .maintenance_timer(false)
        .start()
        .await
        .expect("start harness")
}

/// The board's probe (C1): a worker's `Msg` reached the link while another
/// operation held the permit and was stored and acknowledged under it, with
/// the commit sequence unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inbound_peer_message_on_a_link_waits_for_the_held_operation() {
    let daemon = link_daemon().await;
    let mut peer = linked_worker(&daemon).await;

    let held = Held::take(&daemon).await;
    peer.send(&peer_msg("msg_link_ingest", "cli")).await;
    let early = peer
        .next(HELD_FOR, |f| matches!(f, Frame::MsgAck { .. }))
        .await;
    assert!(
        early.is_none(),
        "acknowledged under a held permit: {early:?}"
    );
    assert_eq!(status(&daemon, "msg_link_ingest").await, None);
    let (boundary, seq) = held.release().await;

    let ack = peer
        .next(DEADLINE, |f| matches!(f, Frame::MsgAck { .. }))
        .await
        .expect("acknowledged once the permit is free");
    let Frame::MsgAck { id, reason } = ack else {
        unreachable!()
    };
    assert_eq!((id.as_str(), reason), ("msg_link_ingest", None));
    assert_eq!(
        status(&daemon, "msg_link_ingest").await,
        Some(MessageStatus::Pending)
    );
    assert!(
        boundary.current_commit_seq() > seq,
        "ingest never committed"
    );
    drop(peer);
    daemon.shutdown().await.unwrap();
}

/// The acknowledgement mutation (`msg_ack`) on the link: a worker's
/// `MsgAck` for an outbound message marks it delivered only once the permit
/// is free, as its own commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_acknowledgement_on_a_link_waits_for_the_held_operation() {
    let daemon = link_daemon().await;
    let mut peer = linked_worker(&daemon).await;
    queue(&daemon, "msg_link_out", "agent@probe").await;
    let sent = peer
        .next(DEADLINE, |f| matches!(f, Frame::Msg { .. }))
        .await
        .expect("the link sends its outbox");
    assert!(matches!(sent, Frame::Msg { ref id, .. } if id == "msg_link_out"));

    let held = Held::take(&daemon).await;
    peer.send(&Frame::MsgAck {
        id: "msg_link_out".into(),
        reason: None,
    })
    .await;
    tokio::time::sleep(HELD_FOR).await;
    assert_eq!(
        status(&daemon, "msg_link_out").await,
        Some(MessageStatus::Pending),
        "delivered under a held permit"
    );
    let (boundary, seq) = held.release().await;

    // Observed as an operation, so a write seen here has also been sequenced:
    // a direct file read can see the write before its operation finishes.
    let until = tokio::time::Instant::now() + DEADLINE;
    while boundary
        .run(false, status(&daemon, "msg_link_out"))
        .await
        .value
        != Some(MessageStatus::Delivered)
    {
        assert!(tokio::time::Instant::now() < until, "never delivered");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(boundary.current_commit_seq() > seq, "ack never committed");
    drop(peer);
    daemon.shutdown().await.unwrap();
}

async fn listener_addr(daemon: &DaemonHarness) -> String {
    let status = daemon
        .project_root()
        .join(".orchestry/remotes/_listener.json");
    let until = tokio::time::Instant::now() + DEADLINE;
    loop {
        if let Some(addr) = std::fs::read(&status)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value["listen_addr"].as_str().map(str::to_owned))
        {
            return addr;
        }
        assert!(
            tokio::time::Instant::now() < until,
            "listener never published"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The worker's per-tick outbox phase (expiry, read, oversize rejection):
/// a message queued for the connected controller is not read while another
/// operation holds the permit, and is sent once it is free.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_outbox_tick_waits_for_the_held_operation() {
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .listen("127.0.0.1:0")
        .peer_name("boundary-worker")
        .start()
        .await
        .expect("start harness");
    let addr = listener_addr(&daemon).await;
    let mut peer = Peer::new(TcpStream::connect(&addr).await.unwrap());
    peer.send(&Frame::Hello {
        proto: PROTOCOL,
        instance_id: "ctl_boundary".into(),
        name: "ctl".into(),
        os: "test".into(),
        agents: vec![],
        version: None,
    })
    .await;
    peer.next(DEADLINE, |f| matches!(f, Frame::Welcome { .. }))
        .await
        .expect("welcome");
    peer.send(&Frame::Sync { active: vec![] }).await;
    peer.next(DEADLINE, |f| matches!(f, Frame::SyncState { .. }))
        .await
        .expect("sync state");

    let held = Held::take(&daemon).await;
    queue(&daemon, "msg_worker_out", "agent@ctl").await;
    let early = peer
        .next(HELD_FOR, |f| matches!(f, Frame::Msg { .. }))
        .await;
    assert!(
        early.is_none(),
        "outbox read under a held permit: {early:?}"
    );
    held.release().await;

    let sent = peer
        .next(DEADLINE, |f| matches!(f, Frame::Msg { .. }))
        .await
        .expect("sent once the permit is free");
    assert!(matches!(sent, Frame::Msg { ref id, .. } if id == "msg_worker_out"));
    drop(peer);
    daemon.shutdown().await.unwrap();
}

/// Stop drains admitted sessions and stalled handshakes alike. In particular,
/// the invalid pre-Sync Ping from the original 8.19 failure may reach the
/// session-end listener rewrite while shutdown is starting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_drains_peer_sessions_before_releasing_publication() {
    for round in 0..20 {
        let daemon = DaemonHarness::builder()
            .maintenance_timer(false)
            .listen("127.0.0.1:0")
            .peer_name("shutdown-worker")
            .start()
            .await
            .unwrap();
        let addr = listener_addr(&daemon).await;
        let mut peer = Peer::new(TcpStream::connect(&addr).await.unwrap());
        peer.send(&Frame::Hello {
            proto: PROTOCOL,
            instance_id: "old".into(),
            name: "old".into(),
            os: "test".into(),
            agents: vec![],
            version: None,
        })
        .await;
        peer.next(DEADLINE, |f| matches!(f, Frame::Welcome { .. }))
            .await
            .expect("welcome");
        // This socket never supplies a hello; shutdown must cancel that
        // network wait rather than wait out the handshake deadline.
        let stalled = TcpStream::connect(&addr).await.unwrap();
        peer.send(&Frame::Ping).await;
        let root = daemon.project_root().to_owned();
        let quiet = tokio::time::timeout(DEADLINE, daemon.quiesce())
            .await
            .expect("peer network waits prevented shutdown")
            .unwrap();
        let path = root.join(".orchestry/remotes/_listener.json");
        assert!(
            !path.exists(),
            "listener record survived stop in round {round}"
        );
        assert!(
            TcpStream::connect(&addr).await.is_err(),
            "listener stayed open"
        );
        // Keep the old engine and sockets alive after clean stop. No peer
        // task may publish again, even while a successor could own the root.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            !path.exists(),
            "listener record reappeared in round {round}"
        );
        drop((quiet, stalled, peer));
    }
}

/// An outbound link awaiting Welcome must also join without waiting on the
/// remote process. Keep its accepted socket open throughout the local stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_joins_an_outbound_link_waiting_for_its_peer() {
    let daemon = link_daemon().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    ok(&raw
        .request(
            1,
            "remote.add",
            json!({
                "alias": "stalled", "addr": listener.local_addr().unwrap().to_string()
            }),
        )
        .await);
    let (socket, _) = tokio::time::timeout(DEADLINE, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut peer = Peer::new(socket);
    peer.next(DEADLINE, |f| matches!(f, Frame::Hello { .. }))
        .await
        .expect("hello");
    drop(raw);
    let quiet = tokio::time::timeout(Duration::from_secs(2), daemon.quiesce())
        .await
        .expect("shutdown waited for a remote Welcome")
        .unwrap();
    assert!(
        tokio::time::timeout(DEADLINE, peer.frames.recv())
            .await
            .unwrap()
            .is_none(),
        "outbound session survived clean stop"
    );
    drop(quiet);
}
