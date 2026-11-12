#![cfg(debug_assertions)]

use orch_client::{
    Client, ClientError, Project, SubscriptionError, SubscriptionEvent, SubscriptionResyncReason,
    testing::{FakeServer, fake_transport},
};
use orch_proto::{
    bounded::Id,
    error::NoDaemonDetail,
    frame::{
        ChangeKind, Event, EventBody, EventFrame, MAX_FRAME_BYTES, OkFrame, ResyncReason, Store,
        limits,
    },
    handshake::{Caller, CallerKind, SessionMode},
    ops::{Request, args::SubscribeArgs, results::SubscribeAck},
};
use std::{future::Future, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle};

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn args() -> SubscribeArgs {
    SubscribeArgs {
        stores: orch_proto::bounded::BoundedVec::new(vec![Store::Tasks, Store::State]).unwrap(),
        filter: None,
        bus: true,
    }
}
fn push(seq: u64) -> EventFrame {
    EventFrame {
        ev: Event {
            subscription: 3,
            instance: id("fake"),
            seq,
            body: EventBody::Change {
                store: Store::State,
                id: None,
                kind: ChangeKind::Updated,
                commit_seq: 9000 + seq,
            },
        },
    }
}
async fn ack(peer: &mut FakeServer, cursor: u64, instance: &str, subscription: u64) {
    let req = peer.recv().await.unwrap().unwrap();
    assert!(matches!(req.req, Request::Subscribe(_)));
    peer.send(&OkFrame {
        id: req.id,
        commit_seq: 9000,
        ok: serde_json::value::to_raw_value(&SubscribeAck {
            subscription,
            instance: instance.into(),
            seq: cursor,
        })
        .unwrap(),
    })
    .await
    .unwrap();
}
async fn session<F, Fut>(script: F) -> (tempfile::TempDir, Client, JoinHandle<()>)
where
    F: FnOnce(FakeServer) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".orchestry")).unwrap();
    let project = Project::discover(dir.path(), None).unwrap();
    let (stream, mut peer) = fake_transport();
    let server = tokio::spawn(async move {
        peer.accept(id("fake")).await.unwrap();
        script(peer).await;
    });
    let client = Client::from_stream(
        project,
        stream,
        Caller {
            kind: CallerKind::Tui,
            agent_id: None,
            pid: None,
        },
        SessionMode::Normal,
    )
    .await
    .unwrap();
    (dir, client, server)
}
async fn next(s: &mut orch_client::Subscription) -> Result<SubscriptionEvent, SubscriptionError> {
    tokio::time::timeout(Duration::from_secs(3), s.next())
        .await
        .expect("observation must progress while idle")
}

#[tokio::test]
async fn idle_push_uses_cursor_one_and_separate_large_baseline() {
    let (_dir, client, task) = session(|mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        peer.send(&push(1)).await.unwrap();
    })
    .await;
    let mut s = client.into_subscription(args()).await.unwrap();
    assert_eq!(s.subscription(), 3);
    assert_eq!(s.instance(), &id("fake"));
    assert_eq!(s.baseline_commit_seq(), 9000);
    assert!(matches!(
        next(&mut s).await.unwrap(),
        SubscriptionEvent::Change {
            seq: 1,
            id: None,
            commit_seq: 9001,
            ..
        }
    ));
    assert!(matches!(
        next(&mut s).await,
        Err(SubscriptionError::TransportClosed)
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn ack_rejects_nonzero_cursor_wrong_instance_and_zero_subscription() {
    for (cursor, instance, sub) in [
        (9000, "fake", 3),
        (0, "other", 3),
        (0, "", 3),
        (0, "fake", 0),
    ] {
        let (_dir, client, task) = session(move |mut peer| async move {
            ack(&mut peer, cursor, instance, sub).await;
        })
        .await;
        assert!(matches!(
            client.into_subscription(args()).await,
            Err(ClientError::Connection(orch_client::NoDaemon::NoDaemon {
                detail: NoDaemonDetail::HandshakeFailed,
                ..
            }))
        ));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn gap_duplicate_and_instance_change_require_resync_and_end_the_handle() {
    for (seq, instance, reason) in [
        (
            2,
            "fake",
            SubscriptionResyncReason::SequenceGap {
                expected: 1,
                received: 2,
            },
        ),
        (
            0,
            "fake",
            SubscriptionResyncReason::SequenceGap {
                expected: 1,
                received: 0,
            },
        ),
        (
            1,
            "new",
            SubscriptionResyncReason::InstanceChanged {
                expected: id("fake"),
                received: id("new"),
            },
        ),
    ] {
        let (_dir, client, task) = session(move |mut peer| async move {
            ack(&mut peer, 0, "fake", 3).await;
            let mut p = push(seq);
            p.ev.instance = id(instance);
            peer.send(&p).await.unwrap();
        })
        .await;
        let mut s = client.into_subscription(args()).await.unwrap();
        assert_eq!(
            next(&mut s).await.unwrap(),
            SubscriptionEvent::ResyncRequired { reason }
        );
        assert!(matches!(s.next().await, Err(SubscriptionError::Ended)));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn bus_and_explicit_resync_are_typed() {
    for reason in [ResyncReason::Lagged, ResyncReason::StoreSetUncertain] {
        let (_dir, client, task) = session(move |mut peer| async move {
            ack(&mut peer, 0, "fake", 3).await;
            let mut p = push(1);
            p.ev.body = EventBody::Bus(Box::new(orch_domain::OrchestratorEvent::TaskAssigned {
                task_id: "t".into(),
                agent_id: "a".into(),
            }));
            peer.send(&p).await.unwrap();
            let mut p = push(7);
            p.ev.body = EventBody::Resync { reason };
            peer.send(&p).await.unwrap();
        })
        .await;
        let mut s = client.into_subscription(args()).await.unwrap();
        assert!(matches!(
            next(&mut s).await.unwrap(),
            SubscriptionEvent::Bus { seq: 1, .. }
        ));
        assert_eq!(
            next(&mut s).await.unwrap(),
            SubscriptionEvent::ResyncRequired {
                reason: SubscriptionResyncReason::Server(reason)
            }
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn unsubscribe_correlates_ack_among_inflight_events_and_closes_session() {
    let (_dir, client, task) = session(|mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        peer.send(&push(1)).await.unwrap();
        let req = peer.recv().await.unwrap().unwrap();
        assert!(matches!(req.req, Request::Unsubscribe(a) if a.subscription==3));
        peer.send(&push(2)).await.unwrap();
        peer.send(&OkFrame {
            id: req.id,
            ok: serde_json::value::to_raw_value(&()).unwrap(),
            commit_seq: 9002,
        })
        .await
        .unwrap();
        assert!(peer.recv().await.unwrap().is_none());
    })
    .await;
    client
        .into_subscription(args())
        .await
        .unwrap()
        .unsubscribe()
        .await
        .unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn cancellation_mid_frame_preserves_the_reader_and_cursor() {
    let (started, start) = oneshot::channel();
    let (release, resume) = oneshot::channel();
    let (_dir, client, task) = session(move |mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        let bytes = orch_proto::ndjson::encode(&push(1), MAX_FRAME_BYTES).unwrap();
        peer.send_raw(&bytes[..30]).await.unwrap();
        started.send(()).unwrap();
        resume.await.unwrap();
        peer.send_raw(&bytes[30..]).await.unwrap();
    })
    .await;
    let mut s = client.into_subscription(args()).await.unwrap();
    start.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), s.next())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    assert!(matches!(
        next(&mut s).await.unwrap(),
        SubscriptionEvent::Change { seq: 1, .. }
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn malformed_oversize_and_misaddressed_frames_are_typed_failures() {
    for bytes in [b"{broken}\n".to_vec(), vec![b'x'; MAX_FRAME_BYTES+2], b"\n".to_vec(), b"{\"ev\":{\"subscription\":4,\"instance\":\"fake\",\"seq\":1,\"store\":\"state\",\"kind\":\"updated\",\"commit_seq\":9001}}\n".to_vec()] {
        let (_dir, client, task)=session(move |mut peer| async move { ack(&mut peer, 0, "fake", 3).await; let _=peer.send_raw(&bytes).await; }).await;
        let mut s=client.into_subscription(args()).await.unwrap();
        assert!(matches!(next(&mut s).await, Err(SubscriptionError::InvalidFrame))); task.await.unwrap();
    }
}

#[tokio::test]
async fn count_overflow_is_explicit_resync_instead_of_silent_drop() {
    let (done, sent) = oneshot::channel();
    let (_dir, client, task) = session(move |mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        for seq in 1..=limits::MAX_QUEUED_EVENTS_PER_SUBSCRIPTION as u64 + 1 {
            peer.send(&push(seq)).await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(3), peer.recv())
                .await
                .expect("overflow must close promptly")
                .unwrap()
                .is_none(),
            "overflow closes both transport halves"
        );
        done.send(()).unwrap();
    })
    .await;
    let mut s = client.into_subscription(args()).await.unwrap();
    sent.await.unwrap();
    task.await.unwrap();
    assert_eq!(
        next(&mut s).await.unwrap(),
        SubscriptionEvent::ResyncRequired {
            reason: SubscriptionResyncReason::BufferOverflow
        }
    );
}

#[tokio::test]
async fn mutation_disconnect_still_has_unknown_outcome() {
    let (_dir, mut client, task) = session(|mut peer| async move {
        let r = peer.recv().await.unwrap().unwrap();
        assert!(matches!(r.req, Request::StateCompleteOnboarding(_)));
    })
    .await;
    let error = client
        .request(orch_proto::ops::args::StateCompleteOnboardingArgs {})
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Connection(orch_client::NoDaemon::NoDaemon {
            detail: NoDaemonDetail::RequestOutcomeUnknown,
            ..
        })
    ));
    assert!(client.into_subscription(args()).await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn byte_overflow_is_bounded_before_the_event_count_limit() {
    let (_dir, client, task) = session(|mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        for seq in 1..=5 {
            let mut p = push(seq);
            p.ev.body = EventBody::Bus(Box::new(orch_domain::OrchestratorEvent::TaskAssigned {
                task_id: "x".repeat(900_000),
                agent_id: "a".into(),
            }));
            peer.send(&p).await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(3), peer.recv())
                .await
                .expect("overflow must close promptly")
                .unwrap()
                .is_none(),
            "overflow closes both transport halves"
        );
    })
    .await;
    let mut s = client.into_subscription(args()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        next(&mut s).await.unwrap(),
        SubscriptionEvent::ResyncRequired {
            reason: SubscriptionResyncReason::BufferOverflow
        }
    );
}

#[tokio::test]
async fn exactly_the_count_bound_preserves_every_event_in_order() {
    let (_dir, client, task) = session(|mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        for seq in 1..=limits::MAX_QUEUED_EVENTS_PER_SUBSCRIPTION as u64 {
            peer.send(&push(seq)).await.unwrap();
        }
    })
    .await;
    let mut s = client.into_subscription(args()).await.unwrap();
    task.await.unwrap();
    for expected in 1..=limits::MAX_QUEUED_EVENTS_PER_SUBSCRIPTION as u64 {
        assert!(
            matches!(next(&mut s).await.unwrap(), SubscriptionEvent::Change { seq, .. } if seq==expected)
        );
    }
    assert!(matches!(
        next(&mut s).await,
        Err(SubscriptionError::TransportClosed)
    ));
}

#[tokio::test]
async fn unsubscribe_eof_refusal_and_wrong_correlation_are_errors_not_panics() {
    for case in 0..3 {
        let (_dir, client, task) = session(move |mut peer| async move {
            ack(&mut peer, 0, "fake", 3).await;
            let req = peer.recv().await.unwrap().unwrap();
            match case {
                0 => {} // EOF without a terminal reply.
                1 => peer
                    .send(&orch_proto::frame::ErrFrame {
                        id: req.id,
                        err: orch_proto::error::ErrorBody::new(
                            orch_proto::error::ErrorCode::Overloaded,
                            "busy",
                            orch_proto::error::Execution::NotExecuted,
                        ),
                    })
                    .await
                    .unwrap(),
                _ => peer
                    .send(&OkFrame {
                        id: orch_proto::frame::RequestId(req.id.0.checked_add(1).unwrap()),
                        ok: serde_json::value::to_raw_value(&()).unwrap(),
                        commit_seq: 9000,
                    })
                    .await
                    .unwrap(),
            }
        })
        .await;
        let error = client
            .into_subscription(args())
            .await
            .unwrap()
            .unsubscribe()
            .await
            .unwrap_err();
        match case {
            0 => assert_eq!(error, SubscriptionError::TransportClosed),
            1 => assert!(
                matches!(error, SubscriptionError::Remote(body) if body.code == orch_proto::error::ErrorCode::Overloaded)
            ),
            _ => assert_eq!(error, SubscriptionError::InvalidFrame),
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn dropping_handle_closes_observation_without_sending_a_mutation() {
    let (_dir, client, task) = session(|mut peer| async move {
        ack(&mut peer, 0, "fake", 3).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(3), peer.recv())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
    })
    .await;
    drop(client.into_subscription(args()).await.unwrap());
    task.await.unwrap();
}

#[tokio::test]
async fn ack_is_correlated_closed_and_precedes_events() {
    for raw in [
        r#"{"id":2,"ok":{"subscription":3,"instance":"fake","seq":0},"commit_seq":9000}"#,
        r#"{"id":1,"ok":{"subscription":3,"instance":"fake","seq":0,"extra":true},"commit_seq":9000}"#,
        r#"{"ev":{"subscription":3,"instance":"fake","seq":1,"store":"state","kind":"updated","commit_seq":9001}}"#,
    ] {
        let (_dir, client, task) = session(move |mut peer| async move {
            let _ = peer.recv().await.unwrap().unwrap();
            peer.send_raw(format!("{raw}\n").as_bytes()).await.unwrap();
        })
        .await;
        assert!(matches!(
            client.into_subscription(args()).await,
            Err(ClientError::Connection(orch_client::NoDaemon::NoDaemon {
                detail: NoDaemonDetail::HandshakeFailed,
                ..
            }))
        ));
        task.await.unwrap();
    }
}
