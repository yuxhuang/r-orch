//! `--listen` / `--peer-name` as start options, over the unchanged TCP
//! protocol (R19, D10).

use crate::dispatch::tests::{bring_up, project};
use crate::lifecycle::Running;
use orch_peer::codec::{read_frame, write_frame};
use orch_peer::protocol::{Frame, PROTOCOL};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

async fn listener_addr(root: &std::path::Path) -> String {
    let status = root.join(".orchestry/remotes/_listener.json");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(addr) = std::fs::read(&status)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value["listen_addr"].as_str().map(str::to_owned))
        {
            return addr;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "listener never published"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_is_a_start_option_and_speaks_the_unchanged_peer_protocol() {
    let (_temp, root) = project().await;
    let mut spec = bring_up(&root, None);
    spec.listen = Some("127.0.0.1:0".into());
    spec.peer_name = Some("daemon-worker".into());
    let running = Running::bring_up(spec).await.unwrap();
    // Peer processing does not wait for dispatch (D3).
    assert!(!running.handler.dispatch_state().dispatch_on);
    let addr = listener_addr(&root).await;
    assert!(
        !addr.ends_with(":0"),
        "published the configured port: {addr}"
    );

    // Exactly the hello a 0.5.x controller sends: no `version` field.
    let mut socket = TcpStream::connect(&addr).await.unwrap();
    socket
        .write_all(b"{\"t\":\"hello\",\"proto\":1,\"instance_id\":\"old\",\"name\":\"old\",\"os\":\"linux\",\"agents\":[]}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(socket);
    let welcome = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader))
        .await
        .unwrap()
        .unwrap();
    let Some(Frame::Welcome { proto, name, .. }) = welcome else {
        panic!("expected welcome, got {welcome:?}");
    };
    assert_eq!(proto, PROTOCOL);
    assert_eq!(name, "daemon-worker");
    write_frame(reader.get_mut(), &Frame::Ping).await.unwrap();

    assert!(running.shutdown().await.clean);
    assert!(!root.join(".orchestry/remotes/_listener.json").exists());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(&addr).await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "listener stayed open"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_listen_address_fails_the_start_and_releases_ownership() {
    let (_temp, root) = project().await;
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut spec = bring_up(&root, None);
    spec.listen = Some(occupied.local_addr().unwrap().to_string());
    let error = Running::bring_up(spec).await.err().expect("busy port");
    assert!(error.to_string().contains("Listen on"), "{error}");
    // Guard and PID lock were released: a start without --listen succeeds.
    let running = Running::bring_up(bring_up(&root, None)).await.unwrap();
    assert!(running.shutdown().await.clean);
}
