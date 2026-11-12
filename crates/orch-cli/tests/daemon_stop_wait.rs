//! A successful stop must let the owner start its successor immediately.
mod daemon_support;
use daemon_support::{READY, Sandbox, start_ready, text};
use orch_proto::ops::results::{DaemonStatus, OwnershipStatus};
use std::process::Output;

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
    // A request already accepted when ownership is lost can be refused once with
    // `request cancelled before execution`; the next reply must say lost.
    let mut tolerated_cancellation = false;
    daemon_support::wait_for(READY, "ownership loss", || {
        ownership_loss_probe(&mut tolerated_cancellation, sandbox.status())
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

fn ownership_loss_probe(
    tolerated_cancellation: &mut bool,
    result: Result<DaemonStatus, Output>,
) -> Option<()> {
    match result {
        Ok(status) if status.ownership == OwnershipStatus::Lost => Some(()),
        Ok(status) => {
            assert!(
                !*tolerated_cancellation,
                "expected lost after transition-time cancellation: {status:?}"
            );
            None
        }
        Err(output)
            if !*tolerated_cancellation
                && output.status.code() == Some(1)
                && output.stdout.is_empty()
                && output.stderr == "  ✕ request cancelled before execution\n".as_bytes() =>
        {
            *tolerated_cancellation = true;
            eprintln!("tolerated transition-time cancellation while polling ownership loss");
            None
        }
        Err(output) => panic!("unexpected ownership-loss probe: {output:?}"),
    }
}

mod ownership_loss_poll_tests {
    use super::*;
    use orch_proto::ops::results::DispatchState;
    use orch_proto::path::WirePath;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn status(ownership: OwnershipStatus) -> Result<DaemonStatus, Output> {
        Ok(DaemonStatus {
            pid: 1,
            project_root: WirePath::new("project").unwrap(),
            socket: WirePath::new("socket").unwrap(),
            protocol: 1,
            binary: "test".into(),
            instance: "instance".into(),
            executable: WirePath::new("orch").unwrap(),
            started_at: "2026-01-01T00:00:00Z".into(),
            dispatch: DispatchState {
                dispatch_on: false,
                batch_active: false,
            },
            running_tasks: Vec::new(),
            sessions: 1,
            session_cap: 1,
            busy_rejections: 0,
            ownership,
            removed_env: Vec::new(),
        })
    }

    fn refusal() -> Output {
        Output {
            status: ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: "  ✕ request cancelled before execution\n"
                .as_bytes()
                .to_vec(),
        }
    }

    #[test]
    fn refusal_then_lost_completes() {
        let mut tolerated = false;
        let replies = [Err(refusal()), status(OwnershipStatus::Lost)];
        let decisions: Vec<_> = replies
            .into_iter()
            .map(|reply| ownership_loss_probe(&mut tolerated, reply))
            .collect();
        assert_eq!((decisions, tolerated), (vec![None, Some(())], true));
    }

    #[test]
    fn held_then_refusal_then_lost_completes() {
        let mut tolerated = false;
        let replies = [
            status(OwnershipStatus::Held),
            Err(refusal()),
            status(OwnershipStatus::Lost),
        ];
        let decisions: Vec<_> = replies
            .into_iter()
            .map(|reply| ownership_loss_probe(&mut tolerated, reply))
            .collect();
        assert_eq!((decisions, tolerated), (vec![None, None, Some(())], true));
    }

    #[test]
    fn held_then_held_then_lost_completes_without_tolerance() {
        let mut tolerated = false;
        let replies = [
            status(OwnershipStatus::Held),
            status(OwnershipStatus::Held),
            status(OwnershipStatus::Lost),
        ];
        let decisions: Vec<_> = replies
            .into_iter()
            .map(|reply| ownership_loss_probe(&mut tolerated, reply))
            .collect();
        assert_eq!((decisions, tolerated), (vec![None, None, Some(())], false));
    }

    #[test]
    #[should_panic(expected = "expected lost after transition-time cancellation:")]
    fn refusal_then_held_fails_immediately() {
        let mut tolerated = false;
        ownership_loss_probe(&mut tolerated, Err(refusal()));
        ownership_loss_probe(&mut tolerated, status(OwnershipStatus::Held));
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe: Output")]
    fn refusal_then_refusal_fails_immediately() {
        let mut tolerated = false;
        ownership_loss_probe(&mut tolerated, Err(refusal()));
        ownership_loss_probe(&mut tolerated, Err(refusal()));
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe: Output")]
    fn exit_seven_fails_immediately() {
        let mut output = refusal();
        output.status = ExitStatus::from_raw(7 << 8);
        ownership_loss_probe(&mut false, Err(output));
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe: Output")]
    fn exit_one_with_other_stderr_fails_immediately() {
        let mut output = refusal();
        output.stderr = b"another error\n".to_vec();
        ownership_loss_probe(&mut false, Err(output));
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe: Output")]
    fn refusal_with_nonempty_stdout_fails_immediately() {
        let mut output = refusal();
        output.stdout = b"unexpected output".to_vec();
        ownership_loss_probe(&mut false, Err(output));
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe: Output")]
    fn signal_status_fails_immediately() {
        let mut output = refusal();
        output.status = ExitStatus::from_raw(15);
        ownership_loss_probe(&mut false, Err(output));
    }
}
