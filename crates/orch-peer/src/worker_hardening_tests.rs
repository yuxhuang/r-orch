// Included inside worker::tests so the socket tests share the hermetic fixture.
struct TestListener {
    _temp: tempfile::TempDir,
    worker: Arc<Worker>,
    agent: String,
    addr: std::net::SocketAddr,
    _server: AbortOnDrop,
}

impl TestListener {
    async fn start() -> Self {
        let (temp, worker, agent) = fixture().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = worker.clone();
        let server = AbortOnDrop(tokio::spawn(async move {
            serving.listen(listener).await.unwrap();
        }));
        Self {
            _temp: temp,
            worker,
            agent,
            addr,
            _server: server,
        }
    }

    async fn hello(&self, id: &str) -> BufReader<TcpStream> {
        let mut socket = TcpStream::connect(self.addr).await.unwrap();
        write_frame(
            &mut socket,
            &Frame::Hello {
                proto: PROTOCOL,
                instance_id: id.into(),
                name: "controller".into(),
                os: "test".into(),
                agents: vec![],
                version: None,
            },
        )
        .await
        .unwrap();
        let mut read = BufReader::new(socket);
        assert!(matches!(
            socket_next(&mut read).await,
            Some(Frame::Welcome { .. })
        ));
        assert!(matches!(
            socket_next(&mut read).await,
            Some(Frame::Agents { .. })
        ));
        read
    }

    async fn sync(&self, read: &mut BufReader<TcpStream>) {
        write_frame(read.get_mut(), &Frame::Sync { active: vec![] })
            .await
            .unwrap();
        assert!(matches!(
            socket_next(read).await,
            Some(Frame::SyncState { .. })
        ));
    }

    async fn healthy(&self) {
        // Different identity: a leaked admission slot must fail this handshake.
        let mut read = self.hello("next-legitimate-controller").await;
        self.sync(&mut read).await;
        write_frame(read.get_mut(), &Frame::Ping).await.unwrap();
        assert!(matches!(socket_next(&mut read).await, Some(Frame::Pong)));
    }
}

async fn socket_next(read: &mut BufReader<TcpStream>) -> Option<Frame> {
    timeout(Duration::from_secs(2), read_frame(read))
        .await
        .expect("peer response deadline")
        .expect("valid peer response")
}

async fn socket_closed(read: &mut BufReader<TcpStream>) {
    timeout(Duration::from_secs(2), async {
        loop {
            match read_frame(read).await {
                Ok(None) => return,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return;
                }
                Ok(Some(Frame::Error { .. })) => {}
                other => panic!("expected rejection and close, got {other:?}"),
            }
        }
    })
    .await
    .expect("hostile socket was not closed");
}

async fn hostile_then_healthy(bytes: &[u8], half_close: bool) {
    use tokio::io::AsyncWriteExt;
    // Exercise both handshake parsing and the admitted session's reader queue.
    for admitted in [false, true] {
        let listener = TestListener::start().await;
        let mut read = if admitted {
            let mut read = listener.hello("hostile-controller").await;
            listener.sync(&mut read).await;
            read
        } else {
            BufReader::new(TcpStream::connect(listener.addr).await.unwrap())
        };
        let sent = timeout(Duration::from_secs(2), read.get_mut().write_all(bytes))
            .await
            .expect("hostile write deadline");
        if let Err(e) = sent {
            assert!(
                matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ),
                "{e}"
            );
        }
        if half_close {
            let _ = read.get_mut().shutdown().await;
        }
        socket_closed(&mut read).await;
        listener.healthy().await;
    }
}

#[tokio::test]
async fn malformed_json_closes_socket_and_listener_accepts_next_controller() {
    hostile_then_healthy(b"{broken json\n", false).await;
}

#[tokio::test]
async fn unknown_frame_type_closes_socket_and_listener_accepts_next_controller() {
    hostile_then_healthy(b"{\"t\":\"not_a_protocol_frame\"}\n", false).await;
}

#[tokio::test]
async fn oversize_frame_closes_socket_and_listener_accepts_next_controller() {
    let mut bytes = b"{\"t\":\"msg\",\"id\":\"msg_oversize\",\"from\":\"cli\",\"to\":\"cli\",\"subject\":\"\",\"body\":\"".to_vec();
    bytes.extend(vec![b'x'; crate::codec::MAX_FRAME_BYTES + 1]);
    bytes.extend_from_slice(b"\"}\n");
    hostile_then_healthy(&bytes, false).await;
}

#[tokio::test]
async fn unterminated_frame_at_eof_closes_socket_and_listener_accepts_next_controller() {
    hostile_then_healthy(b"{\"t\":\"ping\"}", true).await;
}

#[tokio::test]
async fn application_frames_before_hello_are_rejected_without_side_effects() {
    use tokio::io::AsyncWriteExt;
    for frame in [
        Frame::Ping,
        Frame::Sync { active: vec![] },
        submit("premature", "missing"),
        Frame::Msg {
            id: "msg_premature".into(),
            from: "cli".into(),
            to: "cli".into(),
            subject: String::new(),
            body: "must not arrive".into(),
            reply_to: None,
        },
    ] {
        let listener = TestListener::start().await;
        let mut socket = TcpStream::connect(listener.addr).await.unwrap();
        socket
            .write_all(&[serde_json::to_vec(&frame).unwrap(), b"\n".to_vec()].concat())
            .await
            .unwrap();
        socket_closed(&mut BufReader::new(socket)).await;
        assert!(
            listener
                .worker
                .ctx
                .engine
                .tasks
                .list(&TaskFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            listener
                .worker
                .ctx
                .engine
                .messages
                .list()
                .await
                .unwrap()
                .is_empty()
        );
        listener.healthy().await;
    }
}

#[tokio::test]
async fn message_before_sync_is_rejected_without_delivery_and_releases_admission() {
    let listener = TestListener::start().await;
    let mut read = listener.hello("premature-controller").await;
    write_frame(
        read.get_mut(),
        &Frame::Msg {
            id: "msg_before_sync".into(),
            from: "cli".into(),
            to: listener.agent.clone(),
            subject: String::new(),
            body: "must not arrive".into(),
            reply_to: None,
        },
    )
    .await
    .unwrap();
    socket_closed(&mut read).await;
    assert!(
        listener
            .worker
            .ctx
            .engine
            .messages
            .list()
            .await
            .unwrap()
            .is_empty()
    );
    listener.healthy().await;
}

#[tokio::test]
async fn wrong_protocol_closes_socket_and_listener_accepts_next_controller() {
    hostile_then_healthy(b"{\"t\":\"hello\",\"proto\":99,\"instance_id\":\"old\",\"name\":\"old\",\"os\":\"test\",\"agents\":[]}\n", false).await;
}

#[tokio::test]
async fn incomplete_handshake_times_out_without_blocking_other_controllers() {
    use tokio::io::AsyncWriteExt;
    let listener = TestListener::start().await;
    let mut hostile = TcpStream::connect(listener.addr).await.unwrap();
    hostile.write_all(b"{\"t\":").await.unwrap();
    // Establish a real controller while the hostile socket remains open.
    listener.healthy().await;
    let mut read = BufReader::new(hostile);
    timeout(Duration::from_secs(17), async {
        assert!(read_frame(&mut read).await.unwrap().is_none());
    })
    .await
    .expect("incomplete hello exceeded handshake deadline");
    listener.healthy().await;
}

#[tokio::test]
async fn duplicate_message_after_reconnect_is_acked_without_reopening_delivered_inbox() {
    let listener = TestListener::start().await;
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    let mut frame = Frame::Msg {
        id: "msg_wire_duplicate".into(),
        from: "lead".into(),
        to: listener.agent.clone(),
        subject: "original subject".into(),
        body: "original body".into(),
        reply_to: None,
    };
    write_frame(read.get_mut(), &frame).await.unwrap();
    assert!(
        matches!(socket_next(&mut read).await, Some(Frame::MsgAck { id, .. }) if id == "msg_wire_duplicate")
    );
    listener
        .worker
        .ctx
        .engine
        .messages
        .mark_delivered("msg_wire_duplicate")
        .await
        .unwrap();
    let original = listener
        .worker
        .ctx
        .engine
        .messages
        .get("msg_wire_duplicate")
        .await
        .unwrap()
        .unwrap();
    drop(read);
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    if let Frame::Msg { body, .. } = &mut frame {
        *body = "duplicate must not overwrite".into();
    }
    write_frame(read.get_mut(), &frame).await.unwrap();
    assert!(
        matches!(socket_next(&mut read).await, Some(Frame::MsgAck { id, .. }) if id == "msg_wire_duplicate")
    );
    let saved = listener
        .worker
        .ctx
        .engine
        .messages
        .get("msg_wire_duplicate")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}

async fn socket_data(read: &mut BufReader<TcpStream>) -> Option<Frame> {
    loop {
        match socket_next(read).await {
            Some(Frame::State { .. } | Frame::Agents { .. }) => {}
            Some(Frame::Ping) => write_frame(read.get_mut(), &Frame::Pong).await.unwrap(),
            frame => return frame,
        }
    }
}

#[tokio::test]
async fn oversized_event_live_and_replay_keep_session_open_for_next_frame() {
    let listener = TestListener::start().await;
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    write_frame(read.get_mut(), &submit("key", &listener.agent))
        .await
        .unwrap();
    assert!(matches!(
        socket_data(&mut read).await,
        Some(Frame::Submitted { .. })
    ));
    let task = mirror(&listener.worker, "key").await;
    listener.worker.record(
        &task.id,
        "run",
        &AgentEvent::new(
            AgentEventKind::Output,
            "now",
            serde_json::json!("x".repeat(crate::codec::MAX_FRAME_BYTES)),
        ),
    );
    assert!(
        matches!(socket_data(&mut read).await, Some(Frame::Event { event, .. }) if event.data["truncated"] == true)
    );
    write_frame(
        read.get_mut(),
        &Frame::Attach {
            exec_key: "key".into(),
            after_seq: 0,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(socket_data(&mut read).await, Some(Frame::Event { event, .. }) if event.data["truncated"] == true)
    );
    assert!(matches!(
        socket_data(&mut read).await,
        Some(Frame::Gap { .. })
    ));
    write_frame(read.get_mut(), &Frame::Ping).await.unwrap();
    assert!(matches!(socket_data(&mut read).await, Some(Frame::Pong)));
    let tap = listener.worker.tap.lock().unwrap();
    assert_eq!(tap.rings["key"].events[0].1.data["truncated"], true);
}

#[tokio::test]
async fn oversized_sync_and_finished_keep_session_open_for_next_frame() {
    let listener = TestListener::start().await;
    let mut keys = Vec::new();
    for n in 0..2 {
        let key = format!("large-{n}");
        listener
            .worker
            .handle("sender", "controller", submit(&key, &listener.agent))
            .await
            .unwrap();
        let (_, mut run) = terminal(&listener.worker, &key, TaskStatus::Failed).await;
        run.status = RunStatus::Failed;
        run.error = Some("error".repeat(crate::codec::MAX_FRAME_BYTES / 4));
        listener.worker.ctx.engine.runs.save(&run).await.unwrap();
        keys.push(key);
    }
    let mut read = listener.hello("sender").await;
    write_frame(
        read.get_mut(),
        &Frame::Sync {
            active: keys.clone(),
        },
    )
    .await
    .unwrap();
    write_frame(read.get_mut(), &Frame::Ping).await.unwrap();
    let mut synced = Vec::new();
    let mut finished = Vec::new();
    loop {
        match socket_data(&mut read).await {
            Some(Frame::SyncState { items }) => {
                for item in items {
                    assert!(matches!(item.outcome, Some(RemoteOutcome::Failed { .. })));
                    synced.push(item.exec_key);
                }
            }
            Some(Frame::Finished { exec_key, outcome }) => {
                assert!(matches!(outcome, RemoteOutcome::Failed { .. }));
                finished.push(exec_key);
            }
            Some(Frame::Pong) => break,
            other => panic!("unexpected sync response: {other:?}"),
        }
    }
    synced.sort();
    finished.sort();
    assert_eq!(synced, keys);
    assert_eq!(finished, keys);
}

/// The live failure this branch fixes: one mirror whose task file could not be
/// read errored the whole tick, so the worker closed the connection right after
/// every handshake and the controller saw the peer as permanently offline. The
/// mirror's index entry survives a corrupt file, so `mirrors()` still lists a
/// task that `tasks.get` cannot read — exactly the production shape.
#[tokio::test]
async fn an_unreadable_mirror_is_reported_unknown_and_keeps_the_session() {
    let listener = TestListener::start().await;
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    write_frame(read.get_mut(), &submit("poison", &listener.agent))
        .await
        .unwrap();
    assert!(matches!(
        socket_data(&mut read).await,
        Some(Frame::Submitted { .. })
    ));
    let task = mirror(&listener.worker, "poison").await;
    let path = listener
        .worker
        .ctx
        .engine
        .paths
        .task_path(&task.id)
        .unwrap();
    tokio::fs::write(&path, b"{not: [valid").await.unwrap();

    write_frame(
        read.get_mut(),
        &Frame::Sync {
            active: vec!["poison".into()],
        },
    )
    .await
    .unwrap();
    let Some(Frame::SyncState { items }) = socket_data(&mut read).await else {
        panic!("sync must be answered even when a mirror is unreadable")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].state, "unknown");
    assert_eq!(
        items[0].reason.as_deref(),
        Some("worker storage error: read failed"),
        "the reason names a class, never the error text: that can carry a local path"
    );
    assert_eq!(items[0].worker_task_id.as_deref(), Some(task.id.as_str()));

    // The session is still usable: the next frame is answered, and a tick's
    // worth of updates reports the same key as unknown rather than ending it.
    write_frame(read.get_mut(), &Frame::Ping).await.unwrap();
    loop {
        match socket_data(&mut read).await {
            // The sync reply's trailing updates repeat the degraded key.
            Some(Frame::SyncState { items }) => assert_eq!(items[0].state, "unknown"),
            Some(Frame::Pong) => break,
            other => panic!("session closed instead of answering: {other:?}"),
        }
    }
    let updates = listener
        .worker
        .updates("sender", &mut HashMap::new())
        .await;
    assert!(
        matches!(&updates[0], Frame::SyncState { items } if items[0].state == "unknown"
            && items[0].reason.is_some()),
        "{updates:?}"
    );

    // And it recovers by itself once the record is readable again.
    listener
        .worker
        .ctx
        .engine
        .tasks
        .save(&task)
        .await
        .unwrap();
    let item = listener.worker.item("poison", Some(&task)).await;
    assert_eq!((item.state.as_str(), item.reason.as_deref()), ("queued", None));
}

/// 2com's `serve.log` had no peer lines at all while every session died, so
/// there was nothing to debug from. A session that ends with an error is now
/// logged, reported, and left behind in `_listener.json` for `remote status`.
#[tokio::test]
async fn a_failed_session_records_why_in_the_listener_record() {
    let listener = TestListener::start().await;
    let mut events = listener.worker.ctx.engine.events.subscribe();
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    // A worker-to-controller frame arriving from the controller: a contract
    // violation, and one of the few things that still ends a session.
    write_frame(
        read.get_mut(),
        &Frame::CancelAck {
            exec_key: "key".into(),
        },
    )
    .await
    .unwrap();
    socket_closed(&mut read).await;

    let record = timeout(Duration::from_secs(5), async {
        loop {
            let status: serde_json::Value = serde_json::from_slice(
                &tokio::fs::read(
                    listener
                        .worker
                        .ctx
                        .engine
                        .paths
                        .remotes_dir()
                        .join("_listener.json"),
                )
                .await
                .unwrap(),
            )
            .unwrap();
            if !status["last_session_error"].is_null() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the ended session was never recorded");
    assert_eq!(record["last_session_error"], "unexpected controller frame");
    assert!(record["last_session_error_at"].is_string());
    assert!(record["controller_name"].is_null(), "{record}");
    assert!(
        matches!(
            timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap().as_ref(),
            orch_domain::OrchestratorEvent::OrchestratorError { error, fatal: false, .. }
                if error.contains("unexpected controller frame")
        ),
        "the session end must reach the event feed"
    );
    listener.healthy().await;
}

fn incoming_message(id: &str, to: &str) -> Frame {
    Frame::Msg {
        id: id.into(),
        from: "cli".into(),
        to: to.into(),
        subject: "subject".into(),
        body: "body".into(),
        reply_to: None,
    }
}

#[tokio::test]
async fn invalid_incoming_message_is_rejected_and_next_message_is_delivered() {
    let listener = TestListener::start().await;
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    write_frame(read.get_mut(), &incoming_message("msg_bad", "agt_missing"))
        .await
        .unwrap();
    assert!(
        matches!(socket_data(&mut read).await, Some(Frame::MsgAck { id, reason: Some(reason) }) if id == "msg_bad" && reason.contains("local agent"))
    );
    write_frame(
        read.get_mut(),
        &incoming_message("msg_good", &listener.agent),
    )
    .await
    .unwrap();
    assert!(
        matches!(socket_data(&mut read).await, Some(Frame::MsgAck { id, reason: None }) if id == "msg_good")
    );
    assert!(
        listener
            .worker
            .ctx
            .engine
            .messages
            .get("msg_good")
            .await
            .unwrap()
            .is_some()
    );
    write_frame(read.get_mut(), &Frame::Ping).await.unwrap();
    assert!(matches!(socket_data(&mut read).await, Some(Frame::Pong)));
}

#[tokio::test]
async fn transient_ingest_failure_is_not_acked_and_session_accepts_retry() {
    let listener = TestListener::start().await;
    let mut read = listener.hello("sender").await;
    listener.sync(&mut read).await;
    // An unreadable stored record represents a recoverable storage fault.
    // No test-specific service/mock is needed, and no real HOME is touched.
    let path = listener
        .worker
        .ctx
        .engine
        .paths
        .message_path("msg_retry")
        .unwrap();
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&path, b"{broken").await.unwrap();
    write_frame(
        read.get_mut(),
        &incoming_message("msg_retry", &listener.agent),
    )
    .await
    .unwrap();
    write_frame(read.get_mut(), &Frame::Ping).await.unwrap();
    assert!(
        matches!(socket_data(&mut read).await, Some(Frame::Pong)),
        "transient ingest must not ack or close"
    );
    tokio::fs::remove_file(path).await.unwrap();
    write_frame(
        read.get_mut(),
        &incoming_message("msg_retry", &listener.agent),
    )
    .await
    .unwrap();
    assert!(
        matches!(socket_data(&mut read).await, Some(Frame::MsgAck { id, reason: None }) if id == "msg_retry")
    );
}

async fn queued_message(worker: &Worker, id: &str) -> orch_domain::Message {
    let message = orch_domain::Message {
        id: id.into(),
        channel: orch_domain::MessageChannel::Direct,
        from_agent_id: "cli".into(),
        to_agent_id: Some("agt_remote@controller".into()),
        subject: "original subject".into(),
        body: "original body".into(),
        reply_to: None,
        created_at: worker.ctx.engine.clock.now_iso(),
        expires_at: None,
        status: orch_domain::MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
    };
    worker.ctx.engine.messages.save(&message).await.unwrap();
    message
}

#[tokio::test]
async fn rejecting_ack_is_alias_checked_bounded_and_idempotent() {
    let (_temp, worker, _agent) = fixture().await;
    queued_message(&worker, "msg_rejected").await;
    worker
        .ctx
        .msg_ack("other-peer", "msg_rejected", Some("wrong peer"))
        .await
        .unwrap();
    assert_eq!(
        worker
            .ctx
            .engine
            .messages
            .get("msg_rejected")
            .await
            .unwrap()
            .unwrap()
            .status,
        orch_domain::MessageStatus::Pending
    );
    let reason = "é".repeat(300);
    worker
        .ctx
        .msg_ack("controller", "msg_rejected", Some(&reason))
        .await
        .unwrap();
    let first = worker
        .ctx
        .engine
        .messages
        .get("msg_rejected")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.status, orch_domain::MessageStatus::Delivered);
    assert_eq!(first.subject, "[rejected] original subject");
    assert_eq!(
        first.body,
        format!(
            "[rejected by controller: {}]\n\noriginal body",
            "é".repeat(128)
        )
    );
    worker
        .ctx
        .msg_ack("controller", "msg_rejected", Some("second reason"))
        .await
        .unwrap();
    worker
        .ctx
        .msg_ack("controller", "msg_rejected", None)
        .await
        .unwrap();
    let second = worker
        .ctx
        .engine
        .messages
        .get("msg_rejected")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.subject, first.subject);
    assert_eq!(second.body, first.body);
    assert_eq!(second.status, first.status);
}

#[tokio::test]
async fn oversized_outbox_message_is_rejected_locally_and_next_message_is_framed() {
    let (_temp, worker, _agent) = fixture().await;
    let mut huge = queued_message(&worker, "msg_huge").await;
    // Service caps bodies, but reply_to and old stored records still need an
    // encoded frame check (JSON escaping counts toward the wire limit).
    huge.reply_to = Some("x".repeat(crate::codec::MAX_FRAME_BYTES));
    worker.ctx.engine.messages.save(&huge).await.unwrap();
    queued_message(&worker, "msg_small").await;
    let frames = worker.ctx.outbox("controller").await.unwrap();
    assert_eq!(frames.len(), 1);
    assert!(matches!(&frames[0], Frame::Msg { id, .. } if id == "msg_small"));
    let huge = worker
        .ctx
        .engine
        .messages
        .get("msg_huge")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(huge.status, orch_domain::MessageStatus::Delivered);
    assert!(huge.body.contains("message too large for remote frame"));
}
