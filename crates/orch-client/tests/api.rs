//! The typed operation surface against a scripted daemon: generated methods,
//! streamed events and results, cancellation, uploads, and the error mapping
//! of every way a request can end.
use base64::Engine as _;
use orch_client::{Client, ClientError, Progress, Project};
use orch_domain::OrchestratorEvent;
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::frame::{
    ErrFrame, Event, EventBody, EventFrame, MAX_FRAME_BYTES, OkFrame, Part, PartFrame, PartKind,
    RequestFrame, RequestId, ResyncReason, StreamEncoding, StreamedResult, limits,
};
use orch_proto::handshake::{
    Caller, CallerKind, HandshakeReply, HelloFrame, MAX_HANDSHAKE_BYTES, SessionMode, Welcome,
};
use orch_proto::ndjson::{FrameReader, write_frame};
use orch_proto::ops::args::{self, TransferPurpose, TransferToken};
use orch_proto::ops::{Request, results};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::future::Future;
use tokio::io::{BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;

type R = FrameReader<BufReader<ReadHalf<DuplexStream>>>;
type W = WriteHalf<DuplexStream>;

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

/// A compatible session whose daemon side runs `script` after the handshake.
async fn session<F, Fut>(script: F) -> (tempfile::TempDir, Client, JoinHandle<()>)
where
    F: FnOnce(R, W) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".orchestry")).unwrap();
    let project = Project::discover(dir.path(), None).unwrap();
    let (client, server) = tokio::io::duplex(4 * 1024 * 1024);
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
        script(read, write).await;
    });
    let caller = Caller {
        kind: CallerKind::Cli,
        agent_id: None,
        pid: None,
    };
    let client = Client::from_stream(project, client, caller, SessionMode::Normal)
        .await
        .unwrap();
    (dir, client, task)
}

async fn recv(read: &mut R) -> RequestFrame {
    read.read(MAX_FRAME_BYTES).await.unwrap().unwrap()
}

async fn send<T: Serialize>(write: &mut W, frame: &T) {
    write_frame(write, frame, MAX_FRAME_BYTES).await.unwrap();
}

fn ok<T: Serialize>(id: RequestId, value: &T, commit_seq: u64) -> OkFrame {
    OkFrame {
        id,
        ok: serde_json::value::to_raw_value(value).unwrap(),
        commit_seq,
    }
}

fn part<T: Serialize>(id: RequestId, n: u64, kind: PartKind, data: &T) -> PartFrame {
    PartFrame {
        id,
        part: Part {
            n,
            kind,
            data: serde_json::value::to_raw_value(data).unwrap(),
        },
    }
}

fn assigned(task: &str) -> OrchestratorEvent {
    OrchestratorEvent::TaskAssigned {
        task_id: task.into(),
        agent_id: "agt_1".into(),
    }
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn assert_unknown(error: &ClientError) {
    assert_eq!(error.exit_code(), 7, "{error}");
    let text = error.render(false);
    assert!(text.contains("request outcome unknown"), "{text}");
    assert!(text.contains("inspect state before retrying"), "{text}");
    assert!(!text.contains("Start it with"), "{text}");
}

fn context_get(key: &str) -> args::ContextGetArgs {
    args::ContextGetArgs {
        key: Id::new(key).unwrap(),
    }
}

#[tokio::test]
async fn generated_methods_send_their_catalogue_op_and_decode_the_typed_result() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        assert_eq!(req.req, Request::ContextGet(context_get("k")));
        send(&mut write, &ok(req.id, &serde_json::Value::Null, 7)).await;
        let req = recv(&mut read).await;
        assert_eq!(req.req.name(), "goal.progress");
        send(&mut write, &ok(req.id, &Some("half way"), 8)).await;
    })
    .await;
    assert_eq!(client.last_commit_seq(), None);
    assert_eq!(client.context_get(context_get("k")).await.unwrap(), None);
    assert_eq!(client.last_commit_seq(), Some(7));
    let progress = client
        .request(args::GoalProgressArgs {
            id: Id::new("goal_1").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(progress.as_deref(), Some("half way"));
    assert_eq!(client.last_commit_seq(), Some(8));
    task.await.unwrap();
}

#[tokio::test]
async fn streamed_events_arrive_in_order_before_the_terminal_result() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        assert_eq!(req.req.name(), "dispatch.run_task");
        send(
            &mut write,
            &part(req.id, 0, PartKind::Event, &assigned("tsk_1")),
        )
        .await;
        // Pushes carry no request id; nothing consumes them before PR 3.
        let push = EventFrame {
            ev: Event {
                subscription: 1,
                instance: Id::new("i").unwrap(),
                seq: 1,
                body: EventBody::Resync {
                    reason: ResyncReason::Lagged,
                },
            },
        };
        send(&mut write, &push).await;
        send(
            &mut write,
            &part(req.id, 1, PartKind::Event, &assigned("tsk_2")),
        )
        .await;
        send(
            &mut write,
            &ErrFrame {
                id: req.id,
                err: ErrorBody::new(ErrorCode::Conflict, "task failed", Execution::Completed),
            },
        )
        .await;
    })
    .await;
    let mut call = client
        .start(args::DispatchRunTaskArgs {
            task_id: Id::new("tsk_1").unwrap(),
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    let error = loop {
        match call.next().await {
            Ok(Progress::Event(event)) => seen.push(*event),
            Ok(Progress::Done(_)) => panic!("expected the terminal error"),
            Err(error) => break error,
        }
    };
    assert_eq!(seen, vec![assigned("tsk_1"), assigned("tsk_2")]);
    assert!(matches!(error, ClientError::Remote(_)), "{error}");
    assert_eq!(error.exit_code(), 1);
    // A finished call cannot be read again; it says so instead of hanging.
    assert_eq!(call.next().await.unwrap_err().exit_code(), 2);
    task.await.unwrap();
}

#[tokio::test]
async fn request_discards_events_and_returns_the_result() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        send(
            &mut write,
            &part(req.id, 0, PartKind::Event, &assigned("tsk_1")),
        )
        .await;
        send(&mut write, &part(req.id, 1, PartKind::Log, &"ignored")).await;
        send(&mut write, &ok(req.id, &serde_json::Value::Null, 3)).await;
    })
    .await;
    client
        .request(args::StateCompleteOnboardingArgs {})
        .await
        .unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn cancel_sends_request_cancel_and_waits_for_both_terminals_in_either_order() {
    for cancel_ack_first in [true, false] {
        let (_dir, mut client, task) = session(move |mut read, mut write| async move {
            let follow = recv(&mut read).await;
            assert_eq!(
                follow.req,
                Request::LogsFollow(args::LogsFollowArgs {
                    run_id: None,
                    task_id: Some(Id::new("tsk_1").unwrap()),
                    agent_id: None,
                })
            );
            send(
                &mut write,
                &part(follow.id, 0, PartKind::Event, &assigned("tsk_1")),
            )
            .await;
            let cancel = recv(&mut read).await;
            assert_eq!(
                cancel.req,
                Request::RequestCancel(args::RequestCancelArgs { id: follow.id })
            );
            assert_ne!(cancel.id, follow.id);
            // An event already in flight when cancel lands is discarded.
            send(
                &mut write,
                &part(follow.id, 1, PartKind::Event, &assigned("tsk_1")),
            )
            .await;
            if cancel_ack_first {
                send(&mut write, &ok(cancel.id, &(), 1)).await;
                send(&mut write, &ok(follow.id, &(), 1)).await;
            } else {
                send(&mut write, &ok(follow.id, &(), 1)).await;
                send(&mut write, &ok(cancel.id, &(), 1)).await;
            }
            let next = recv(&mut read).await;
            send(&mut write, &ok(next.id, &serde_json::Value::Null, 2)).await;
        })
        .await;
        let mut logs = client
            .follow_logs(args::LogsFollowArgs {
                run_id: None,
                task_id: Some(Id::new("tsk_1").unwrap()),
                agent_id: None,
            })
            .await
            .unwrap();
        assert!(matches!(logs.next().await.unwrap(), Progress::Event(_)));
        logs.cancel().await.unwrap();
        // The lane is free again after a confirmed cancel.
        assert_eq!(client.context_get(context_get("k")).await.unwrap(), None);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn refused_cancel_reports_the_refusal_and_keeps_the_lane_closed() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let _follow = recv(&mut read).await;
        let cancel = recv(&mut read).await;
        send(
            &mut write,
            &ErrFrame {
                id: cancel.id,
                err: ErrorBody::new(ErrorCode::Overloaded, "busy", Execution::NotExecuted),
            },
        )
        .await;
        // The original never ends; the client must not read anything else.
        assert!(read.read_line(MAX_FRAME_BYTES).await.unwrap().is_none());
    })
    .await;
    let logs = client
        .follow_logs(args::LogsFollowArgs {
            run_id: None,
            task_id: None,
            agent_id: None,
        })
        .await
        .unwrap();
    let error = logs.cancel().await.unwrap_err();
    assert!(matches!(error, ClientError::Remote(_)), "{error}");
    assert_eq!(error.exit_code(), 7);
    assert_unknown(&client.status().await.unwrap_err());
    drop(client);
    task.await.unwrap();
}

#[tokio::test]
async fn dropping_a_call_mid_stream_poisons_the_lane_without_replay() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        send(
            &mut write,
            &part(req.id, 0, PartKind::Event, &assigned("tsk_1")),
        )
        .await;
        assert!(
            read.read_line(MAX_FRAME_BYTES).await.unwrap().is_none(),
            "nothing may be sent on a poisoned lane"
        );
    })
    .await;
    {
        let mut logs = client
            .follow_logs(args::LogsFollowArgs {
                run_id: None,
                task_id: None,
                agent_id: None,
            })
            .await
            .unwrap();
        assert!(matches!(logs.next().await.unwrap(), Progress::Event(_)));
    }
    assert_unknown(&client.context_get(context_get("k")).await.unwrap_err());
    drop(client);
    task.await.unwrap();
}

/// Split `value`'s JSON into base64 `result_chunk` parts and a summary,
/// as the daemon does for results above the frame bound.
async fn stream_result(write: &mut W, id: RequestId, raw: &[u8], digest: String) {
    for (n, chunk) in (0u64..).zip(raw.chunks(limits::MAX_CHUNK_BYTES)) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        send(write, &part(id, n, PartKind::ResultChunk, &encoded)).await;
    }
    let summary = StreamedResult {
        streamed: true,
        encoding: StreamEncoding::JsonUtf8,
        bytes: raw.len() as u64,
        sha256: Id::new(digest).unwrap(),
    };
    send(write, &ok(id, &summary, 9)).await;
}

fn big_entry() -> results::ContextEntry {
    results::ContextEntry {
        key: "big".into(),
        value: "x".repeat(MAX_FRAME_BYTES + 300 * 1024),
        created_at: "2026-11-09T00:00:00Z".into(),
        updated_at: "2026-11-09T00:00:00Z".into(),
        ttl_ms: None,
        expires_at: None,
    }
}

#[tokio::test]
async fn streamed_large_results_are_reassembled_and_checked() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        let raw = serde_json::to_vec(&Some(big_entry())).unwrap();
        stream_result(&mut write, req.id, &raw, hex(&raw)).await;
    })
    .await;
    let entry = client.context_get(context_get("big")).await.unwrap();
    assert_eq!(entry, Some(big_entry()));
    assert_eq!(client.last_commit_seq(), Some(9));
    task.await.unwrap();
}

#[tokio::test]
async fn a_streamed_result_that_fails_its_digest_is_never_success() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        let raw = serde_json::to_vec(&Some(big_entry())).unwrap();
        stream_result(&mut write, req.id, &raw, hex(b"something else")).await;
    })
    .await;
    assert_unknown(&client.context_get(context_get("big")).await.unwrap_err());
    // The lane stays closed: the reply was not a usable terminal.
    assert_unknown(&client.status().await.unwrap_err());
    task.await.unwrap();
}

#[tokio::test]
async fn malformed_out_of_order_or_misaddressed_parts_are_unknown_outcomes() {
    let cases: Vec<Box<dyn Fn(RequestId) -> PartFrame + Send>> = vec![
        Box::new(|id| part(id, 0, PartKind::Event, &"not an event")),
        Box::new(|id| part(id, 0, PartKind::ResultChunk, &"***not base64***")),
        Box::new(|id| {
            part(
                RequestId(id.0.checked_add(5).unwrap()),
                0,
                PartKind::Event,
                &assigned("t"),
            )
        }),
    ];
    for case in cases {
        let (_dir, mut client, task) = session(move |mut read, mut write| async move {
            let req = recv(&mut read).await;
            send(&mut write, &case(req.id)).await;
        })
        .await;
        let mut call = client.start(args::DispatchRunAllArgs {}).await.unwrap();
        assert_unknown(&call.next().await.unwrap_err());
        drop(call);
        task.await.unwrap();
    }
    // A part number going backwards is a protocol violation too.
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let req = recv(&mut read).await;
        send(
            &mut write,
            &part(req.id, 3, PartKind::Event, &assigned("t")),
        )
        .await;
        send(
            &mut write,
            &part(req.id, 3, PartKind::Event, &assigned("t")),
        )
        .await;
    })
    .await;
    let mut call = client.start(args::DispatchRunAllArgs {}).await.unwrap();
    assert!(matches!(call.next().await.unwrap(), Progress::Event(_)));
    assert_unknown(&call.next().await.unwrap_err());
    drop(call);
    task.await.unwrap();
}

#[tokio::test]
async fn remote_errors_keep_their_exit_code_and_hint_and_free_the_lane() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        for (code, exit, hint) in [
            (ErrorCode::LockConflict, 4, Some("Use: orch status")),
            (ErrorCode::NotPermitted, 2, None),
            (ErrorCode::Overloaded, 7, None),
            (ErrorCode::General, 99, None),
        ] {
            let req = recv(&mut read).await;
            let mut err = ErrorBody::new(code, "refused \u{1b}[31m", Execution::NotExecuted);
            err.exit_code = exit;
            if let Some(hint) = hint {
                err = err.with_hint(hint);
            }
            send(&mut write, &ErrFrame { id: req.id, err }).await;
        }
    })
    .await;
    for expected in [4, 2, 7, 1] {
        let error = client.context_get(context_get("k")).await.unwrap_err();
        assert!(matches!(error, ClientError::Remote(_)), "{error}");
        assert_eq!(
            error.exit_code(),
            expected,
            "an out-of-range code is general"
        );
        let text = error.render(false);
        assert!(!text.contains('\u{1b}'), "peer text is escaped: {text:?}");
        if expected == 4 {
            assert!(text.ends_with("\n    Use: orch status"), "{text}");
        }
    }
    task.await.unwrap();
}

#[tokio::test]
async fn single_reply_entry_points_refuse_multi_frame_ops_before_any_byte() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        // The first frame the daemon sees is the ordinary call after the refusals.
        let req = recv(&mut read).await;
        assert_eq!(req.id.0.get(), 1, "refusals consume no request id");
        assert_eq!(req.req.name(), "context.get");
        send(&mut write, &ok(req.id, &serde_json::Value::Null, 1)).await;
    })
    .await;
    let follow = args::LogsFollowArgs {
        run_id: None,
        task_id: None,
        agent_id: None,
    };
    let refusals = [
        client.request(follow.clone()).await.unwrap_err(),
        client
            .call::<()>(Request::LogsFollow(follow))
            .await
            .unwrap_err(),
        client
            .request(args::TransferBeginArgs {
                purpose: TransferPurpose::Text,
                total_bytes: 1,
            })
            .await
            .unwrap_err(),
        client
            .start(args::UnsubscribeArgs { subscription: 1 })
            .await
            .err()
            .unwrap(),
    ];
    for error in refusals {
        assert!(matches!(error, ClientError::InvalidArguments(_)), "{error}");
        assert_eq!(error.exit_code(), 2);
    }
    assert_eq!(client.context_get(context_get("k")).await.unwrap(), None);
    task.await.unwrap();
}

#[tokio::test]
async fn upload_chunks_in_order_and_seals_with_the_digest() {
    let payload: Vec<u8> = (0..limits::MAX_CHUNK_BYTES * 2 + 17)
        .map(|i| (i % 251) as u8)
        .collect();
    let expected = payload.clone();
    let (_dir, mut client, task) = session(move |mut read, mut write| async move {
        let token = TransferToken(Id::new("transfer_1_1").unwrap());
        let begin = recv(&mut read).await;
        assert_eq!(
            begin.req,
            Request::TransferBegin(args::TransferBeginArgs {
                purpose: TransferPurpose::Document,
                total_bytes: expected.len() as u64,
            })
        );
        let handle = results::TransferHandle {
            token: token.clone(),
            sealed: false,
        };
        send(&mut write, &ok(begin.id, &handle, 1)).await;
        let mut assembled = Vec::new();
        for n in 0..3 {
            let chunk = recv(&mut read).await;
            let Request::TransferChunk(chunk_args) = chunk.req else {
                panic!("expected transfer.chunk")
            };
            assert_eq!(chunk_args.n, n);
            assert_eq!(chunk_args.token, token);
            assembled.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(chunk_args.data_base64.as_bytes())
                    .unwrap(),
            );
            send(&mut write, &ok(chunk.id, &(), 1)).await;
        }
        assert_eq!(assembled, expected);
        let finish = recv(&mut read).await;
        assert_eq!(
            finish.req,
            Request::TransferFinish(args::TransferFinishArgs {
                token: token.clone(),
                sha256: Id::new(hex(&expected)).unwrap(),
            })
        );
        let sealed = results::TransferHandle {
            token,
            sealed: true,
        };
        send(&mut write, &ok(finish.id, &sealed, 1)).await;
    })
    .await;
    let token = client
        .upload(TransferPurpose::Document, &payload)
        .await
        .unwrap();
    assert_eq!(token.0.as_str(), "transfer_1_1");
    task.await.unwrap();
}

#[tokio::test]
async fn upload_bounds_are_checked_before_any_byte_and_a_failed_step_is_reported() {
    let (_dir, mut client, task) = session(|mut read, mut write| async move {
        let begin = recv(&mut read).await;
        assert_eq!(begin.req.name(), "transfer.begin");
        let err = ErrorBody::new(
            ErrorCode::Overloaded,
            "staged transfer budget is full",
            Execution::NotExecuted,
        );
        send(&mut write, &ErrFrame { id: begin.id, err }).await;
    })
    .await;
    for bytes in [Vec::new(), vec![0; limits::MAX_TRANSFER_BYTES + 1]] {
        let error = client
            .upload(TransferPurpose::Text, &bytes)
            .await
            .unwrap_err();
        assert_eq!(error.exit_code(), 2, "{error}");
    }
    let error = client
        .upload(TransferPurpose::Text, b"hello")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("staged transfer budget is full"));
    assert_eq!(error.exit_code(), 7);
    task.await.unwrap();
}
