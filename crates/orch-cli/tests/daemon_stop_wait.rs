//! A successful stop must let the owner start its successor immediately.
mod daemon_support;
use daemon_support::{READY, Sandbox, start_ready, text};

#[test]
fn stop_then_immediate_start_succeeds_five_rounds() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut current = start_ready(&sandbox, &[], "initial");
    for round in 0..5 {
        let output = sandbox.run(&["daemon", "stop"]);
        assert!(
            output.status.success(),
            "round {round}: {}",
            text(&output.stderr)
        );
        // Deliberately do not wait/reap the old daemon before starting again.
        let next = start_ready(&sandbox, &[], &format!("successor-{round}"));
        assert!(current.wait_exit(READY).success());
        current = next;
    }
    assert!(sandbox.run(&["daemon", "stop"]).status.success());
    assert!(current.wait_exit(READY).success());
}

#[test]
fn plain_status_names_ownership_socket_instance_and_running_count() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut daemon = start_ready(&sandbox, &[], "status");
    let status = sandbox.status().unwrap();
    let output = sandbox.run(&["daemon", "status"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let plain = text(&output.stdout);
    for line in [
        "  ownership: held".to_owned(),
        format!("  socket: {}", status.socket.display_escaped()),
        format!("  instance: {}", status.instance),
        "  running tasks: 0".to_owned(),
    ] {
        assert!(
            plain.lines().any(|actual| actual == line),
            "missing {line:?}: {plain}"
        );
    }
    // Removing this owner's PID lock makes the live RPC report LOST.
    std::fs::remove_file(sandbox.state_dir().join("orchestry.lock")).unwrap();
    daemon_support::wait_for(READY, "ownership loss", || {
        (sandbox.status().unwrap().ownership == orch_proto::ops::results::OwnershipStatus::Lost)
            .then_some(())
    });
    let output = sandbox.run(&["daemon", "status"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout)
            .lines()
            .any(|line| line == "  ownership: LOST — stop and restart the daemon")
    );
    assert!(sandbox.run(&["daemon", "stop"]).status.success());
    assert!(daemon.wait_exit(READY).success());
}

#[test]
fn committed_context_set_with_discarded_reply_is_reported_as_unknown() {
    use orch_client::{Client, ClientConfig, Project};
    use orch_proto::ops::{Request, args, results};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut daemon = start_ready(&sandbox, &[], "replylost");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let upstream = tokio::net::UnixStream::connect(sandbox.socket())
            .await
            .unwrap();
        let (stream, proxy) = tokio::io::duplex(1024 * 1024);
        let relay = tokio::spawn(async move {
            let (up_read, mut up_write) = upstream.into_split();
            let (proxy_read, mut proxy_write) = tokio::io::split(proxy);
            let mut up_read = BufReader::new(up_read);
            let mut proxy_read = BufReader::new(proxy_read);
            let mut line = String::new();
            // Relay the handshake and one mutation, then discard its committed reply.
            for handshake in [true, false] {
                line.clear();
                assert!(proxy_read.read_line(&mut line).await.unwrap() > 0);
                up_write.write_all(line.as_bytes()).await.unwrap();
                line.clear();
                assert!(up_read.read_line(&mut line).await.unwrap() > 0);
                if handshake {
                    proxy_write.write_all(line.as_bytes()).await.unwrap();
                } else {
                    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert!(response.get("ok").is_some(), "{line}");
                }
            }
        });
        let config = ClientConfig {
            cwd: sandbox.root(),
            project_root: Some(sandbox.root()),
            socket: Some(sandbox.socket()),
            test_socket_root: Some(sandbox.short_dir().into()),
            caller: orch_proto::handshake::Caller {
                kind: orch_proto::handshake::CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            mode: orch_proto::handshake::SessionMode::Normal,
            agent_context: false,
        };
        let mut client = Client::from_stream(
            Project::discover(&sandbox.root(), None).unwrap(),
            stream,
            config.caller.clone(),
            config.mode,
        )
        .await
        .unwrap();
        let error = client
            .call::<Option<results::ContextEntry>>(Request::ContextSet(args::ContextSetArgs {
                key: "replylost".try_into().unwrap(),
                value: "committed".try_into().unwrap(),
                ttl_ms: None,
            }))
            .await
            .unwrap_err();
        relay.await.unwrap();
        assert_eq!(error.exit_code(), 7);
        let text = error.render(false);
        assert!(
            text.contains("the request may have been applied; inspect state before retrying"),
            "{text}"
        );
        assert!(!text.contains("Start it with"), "{text}");
        let mut inspect = Client::connect(&config).await.unwrap();
        let stored: Option<results::ContextEntry> = inspect
            .call(Request::ContextGet(args::ContextGetArgs {
                key: "replylost".try_into().unwrap(),
            }))
            .await
            .unwrap();
        assert_eq!(stored.unwrap().value, "committed");
    });
    assert!(sandbox.run(&["daemon", "stop"]).status.success());
    assert!(daemon.wait_exit(READY).success());
}
