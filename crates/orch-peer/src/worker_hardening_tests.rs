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
        matches!(socket_next(&mut read).await, Some(Frame::MsgAck { id }) if id == "msg_wire_duplicate")
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
        matches!(socket_next(&mut read).await, Some(Frame::MsgAck { id }) if id == "msg_wire_duplicate")
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
