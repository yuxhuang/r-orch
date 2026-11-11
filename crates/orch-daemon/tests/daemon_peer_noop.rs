#![cfg(feature = "testing")]
//! Write-free peer frames stay silent through the daemon publication path.
mod support;

use orch_daemon::testing::DaemonHarness;
use orch_domain::{CreateTaskInput, Message, MessageChannel, MessageStatus, TaskStatus};
use orch_peer::codec::{read_frame, write_frame};
use orch_peer::protocol::{Frame, PROTOCOL};
use serde_json::json;
use std::time::Duration;
use support::{DEADLINE, Raw, ok};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// One TCP peer session. A reader task owns the partial line, so a bounded
/// wait on [`Peer::next`] never loses a frame.
struct Peer {
    frames: mpsc::Receiver<Frame>,
    writer: OwnedWriteHalf,
    _reader: JoinHandle<()>,
    fence_id: u64,
}

impl Peer {
    fn new(socket: TcpStream) -> Peer {
        let (read, writer) = socket.into_split();
        let (tx, frames) = mpsc::channel(256);
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
            fence_id: 0,
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

impl Drop for Peer {
    fn drop(&mut self) {
        self._reader.abort();
    }
}

fn message(id: &str, to: &str) -> Frame {
    Frame::Msg {
        id: id.into(),
        from: "sender".into(),
        to: to.into(),
        subject: "test".into(),
        body: "body".into(),
        reply_to: None,
    }
}

fn outgoing(id: &str) -> Message {
    Message {
        id: id.into(),
        channel: MessageChannel::Direct,
        from_agent_id: "cli".into(),
        to_agent_id: Some("agent@C1".into()),
        subject: "test".into(),
        body: "body".into(),
        reply_to: None,
        created_at: "2026-11-11T00:00:00Z".into(),
        expires_at: None,
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
    }
}

async fn fence(peer: &mut Peer) {
    peer.fence_id += 1;
    let key = format!("fence-{}", peer.fence_id);
    // Unlike Pong (or an empty SyncState), this reply cannot be confused
    // with an unread response from the burst that precedes it.
    peer.send(&Frame::Sync {
        active: vec!["existing".into(), key.clone()],
    })
    .await;
    peer.next(DEADLINE, |f| matches!(f, Frame::SyncState { items } if items.iter().any(|item| item.exec_key == key))).await.expect("peer processing fence");
}

async fn fixture() -> (DaemonHarness, Peer) {
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .listen("127.0.0.1:0")
        .peer_name("worker")
        .start()
        .await
        .unwrap();
    let mut peer = Peer::new(
        TcpStream::connect(listener_addr(&daemon).await)
            .await
            .unwrap(),
    );
    peer.send(&Frame::Hello {
        proto: PROTOCOL,
        instance_id: "controller".into(),
        name: "C1".into(),
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
        .expect("sync fence");
    (daemon, peer)
}

async fn subscribe(daemon: &DaemonHarness) -> Raw {
    let mut raw = Raw::session(daemon, "cli", "normal").await;
    ok(&raw.request(1, "subscribe", json!({"stores": ["tasks", "goals", "agents", "messages", "context", "teams", "runs", "config", "state", "remote"]})).await);
    raw
}

async fn silent_burst(
    daemon: &DaemonHarness,
    peer: &mut Peer,
    observer: &mut Raw,
    control: &mut Raw,
    frame: &Frame,
) {
    let boundary = daemon.engine().operation_boundary();
    let seq = boundary.current_commit_seq();
    let traffic = async {
        for _ in 0..128 {
            peer.send(frame).await;
        }
        fence(peer).await;
    };
    let request = control.request(2, "status.get", json!({}));
    let (_, reply) = tokio::time::timeout(DEADLINE, async { tokio::join!(traffic, request) })
        .await
        .expect("control answered alongside peer traffic");
    ok(&reply);
    assert_eq!(
        boundary.current_commit_seq(),
        seq,
        "write-free burst {frame:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), observer.recv())
            .await
            .is_err(),
        "write-free burst published a subscriber frame: {frame:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_message_ack_bursts_do_not_publish() {
    let (daemon, mut peer) = fixture().await;
    let mut observer = subscribe(&daemon).await;
    let mut control = Raw::session(&daemon, "cli", "normal").await;
    for reason in [None, Some("rejected".into())] {
        silent_burst(
            &daemon,
            &mut peer,
            &mut observer,
            &mut control,
            &Frame::MsgAck {
                id: "absent".into(),
                reason,
            },
        )
        .await;
    }
    drop((peer, observer, control));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_and_conditionally_write_free_bursts_stay_silent_then_real_and_partial_writes_publish()
 {
    let (daemon, mut peer) = fixture().await;
    let task = daemon
        .engine()
        .task_service
        .create(CreateTaskInput {
            title: "mirror".into(),
            labels: Some(vec![
                "remote-ctl:controller".into(),
                "exec:existing".into(),
                "remote-finished-ack".into(),
            ]),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut task = task;
    task.status = TaskStatus::Cancelled;
    daemon.engine().tasks.save(&task).await.unwrap();
    peer.send(&message("duplicate", "cli")).await;
    peer.next(
        DEADLINE,
        |f| matches!(f, Frame::MsgAck { id, .. } if id == "duplicate"),
    )
    .await
    .expect("seed message");
    daemon
        .engine()
        .messages
        .save(&outgoing("rejected"))
        .await
        .unwrap();
    peer.send(&Frame::MsgAck {
        id: "rejected".into(),
        reason: Some("no recipient".into()),
    })
    .await;
    fence(&mut peer).await;
    let mut observer = subscribe(&daemon).await;
    let mut control = Raw::session(&daemon, "cli", "normal").await;
    let frames = [
        Frame::Attach {
            exec_key: "absent".into(),
            after_seq: 0,
        },
        Frame::Attach {
            exec_key: "existing".into(),
            after_seq: 0,
        },
        Frame::FinishedAck {
            exec_key: "absent".into(),
        },
        Frame::FinishedAck {
            exec_key: "existing".into(),
        },
        Frame::Submit {
            exec_key: "invalid".into(),
            agent_id: "absent".into(),
            from: "lead".into(),
            prompt: "prompt".into(),
            title: "title".into(),
            priority: 2,
            review_criteria: vec![],
            goal_id: None,
        },
        Frame::Submit {
            exec_key: "existing".into(),
            agent_id: "absent".into(),
            from: "lead".into(),
            prompt: "prompt".into(),
            title: "title".into(),
            priority: 2,
            review_criteria: vec![],
            goal_id: None,
        },
        message("bad_recipient", "absent"),
        message("duplicate", "cli"),
        Frame::MsgAck {
            id: "duplicate".into(),
            reason: None,
        }, // Local recipient, wrong alias.
        Frame::MsgAck {
            id: "duplicate".into(),
            reason: Some("wrong alias".into()),
        },
        Frame::MsgAck {
            id: "rejected".into(),
            reason: Some("no recipient".into()),
        },
        Frame::Cancel {
            exec_key: "absent".into(),
        },
        Frame::Cancel {
            exec_key: "existing".into(),
        },
        Frame::Cancel {
            exec_key: String::new(),
        },
        Frame::Sync {
            active: vec!["existing".into()],
        },
        Frame::Ping,
        Frame::Pong,
    ];
    for frame in frames {
        silent_burst(&daemon, &mut peer, &mut observer, &mut control, &frame).await;
    }
    let seq = daemon.engine().operation_boundary().current_commit_seq();
    peer.send(&message("real_mutation", "cli")).await;
    peer.next(
        DEADLINE,
        |f| matches!(f, Frame::MsgAck { id, .. } if id == "real_mutation"),
    )
    .await
    .expect("real write acknowledged");
    let event = observer.recv().await.unwrap();
    assert_eq!(event["ev"]["store"], "messages", "{event}");
    assert_eq!(event["ev"]["id"], "real_mutation", "{event}");
    assert!(event["ev"].get("resync").is_none(), "{event}");
    assert_eq!(event["ev"]["commit_seq"], seq + 1);
    // The entity write succeeds; replacing a directory with the index fails.
    // This exercises actual storage failure rather than manufacturing a note.
    let index = daemon.engine().paths.messages_dir().join("_index.json");
    std::fs::remove_file(&index).unwrap();
    std::fs::create_dir(&index).unwrap();
    let seq = daemon.engine().operation_boundary().current_commit_seq();
    peer.send(&message("partial_mutation", "cli")).await;
    fence(&mut peer).await;
    std::fs::remove_dir(&index).unwrap();
    assert!(
        daemon
            .engine()
            .messages
            .get("partial_mutation")
            .await
            .unwrap()
            .is_some(),
        "entity persisted before index failure"
    );
    let event = observer.recv().await.unwrap();
    assert_eq!(event["ev"]["store"], "messages", "{event}");
    assert_eq!(event["ev"]["id"], "partial_mutation", "{event}");
    assert!(event["ev"].get("resync").is_none(), "{event}");
    assert_eq!(event["ev"]["commit_seq"], seq + 1);
    drop((peer, observer, control));
    daemon.shutdown().await.unwrap();
}
