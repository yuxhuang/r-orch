//! Start the peer transport inside the engine's owner-only startup window.

use std::sync::Arc;

use orch_core::Result;
use orch_engine::{FullEngine, WatchOptions};
use orch_peer::PeerRuntime;

/// Retain the returned guard until the owner has stopped, so shutdown can
/// still forward remote cancellations before closing the links.
pub(crate) async fn start_watch(
    engine: Arc<FullEngine>,
    options: WatchOptions,
) -> Result<PeerRuntime> {
    let mut runtime = None;
    engine
        .orchestrator
        .start_watch_with(options, || async {
            runtime = Some(PeerRuntime::prepare(Arc::clone(&engine)).await?);
            Ok(())
        })
        .await?;
    let mut runtime = runtime.expect("successful watch startup prepared the peer runtime");
    runtime.start_listener();
    Ok(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::storage::ConfigStore;
    use orch_infra::storage::{FileConfigStore, Paths};

    // A listener that accepts during the first tick can exhaust descriptors
    // before startup's fallible filesystem work has completed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn inbound_handshakes_wait_until_the_first_watch_tick_finishes() {
        use std::io::{Read, Write};
        use std::sync::Mutex;
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = port.local_addr().unwrap();
        drop(port);
        let mut config = orch_domain::default_config();
        config.peer.listen = Some(addr.to_string());
        config.peer.name = Some("test-worker".into());
        FileConfigStore::new(paths.clone())
            .write(&config)
            .await
            .unwrap();
        let engine = Arc::new(
            orch_engine::build_full_with(
                temp.path(),
                orch_engine::EngineSeams {
                    global_config_path: Some(paths.root().join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let observed = Arc::new(Mutex::new(None));
        let recorded = observed.clone();
        let _subscription = engine.events.once(
            orch_domain::OrchestratorEventType::OrchestratorTick,
            move |_| {
                // Keep the first tick on the stack while another runtime
                // thread handles the real socket. The bound port may queue
                // TCP connections, but must not read a Hello yet.
                let mut client =
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .unwrap();
                client
                    .write_all(b"{\"t\":\"hello\",\"proto\":1,\"instance_id\":\"early\",\"name\":\"early\",\"os\":\"linux\",\"agents\":[]}\n")
                    .unwrap();
                *recorded.lock().unwrap() = Some(client.read(&mut [0u8; 1]));
            },
        );
        let runtime = start_watch(
            engine.clone(),
            WatchOptions {
                skip_autonomous_seeding: true,
                handle_signals: false,
            },
        )
        .await
        .unwrap();
        engine.orchestrator.stop().await.unwrap();
        drop(runtime);
        let response = observed
            .lock()
            .unwrap()
            .take()
            .expect("first tick observed");
        assert!(
            matches!(response, Err(ref error) if matches!(error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "listener read a Hello before startup completed: {response:?}"
        );
    }

    #[tokio::test]
    async fn a_listener_bind_failure_releases_the_engine_owner_lock() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = orch_domain::default_config();
        config.peer.listen = Some(occupied.local_addr().unwrap().to_string());
        config.peer.name = Some("test-worker".into());
        FileConfigStore::new(paths.clone())
            .write(&config)
            .await
            .unwrap();
        let engine = Arc::new(
            orch_engine::build_full_with(
                temp.path(),
                orch_engine::EngineSeams {
                    global_config_path: Some(paths.root().join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let result = start_watch(
            Arc::clone(&engine),
            WatchOptions {
                skip_autonomous_seeding: true,
                handle_signals: false,
            },
        )
        .await;
        let error = result.err().expect("the occupied port must reject startup");
        assert!(error.to_string().contains("Listen on"), "{error}");
        assert!(!engine.orchestrator.is_owner());
        assert!(!paths.lock_path().exists());
    }

    #[tokio::test]
    async fn pausing_closes_the_listener_and_status_reports_it_inactive() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        let mut config = orch_domain::default_config();
        config.peer.listen = Some("127.0.0.1:0".into());
        config.peer.name = Some("test-worker".into());
        FileConfigStore::new(paths.clone())
            .write(&config)
            .await
            .unwrap();
        let engine = Arc::new(
            orch_engine::build_full_with(
                temp.path(),
                orch_engine::EngineSeams {
                    global_config_path: Some(paths.root().join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let options = || WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        };
        let status = || crate::commands::remote::listener_status(&paths, true);
        let runtime = start_watch(Arc::clone(&engine), options()).await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let addr = loop {
            if let (Some(addr), _) = status().await.unwrap() {
                break addr;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "listener never published"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert!(
            !addr.ends_with(":0"),
            "published the configured port: {addr}"
        );
        tokio::net::TcpStream::connect(&addr).await.unwrap();

        // The TUI's `/pause`: stop the engine, then drop its peer runtime.
        engine.orchestrator.stop().await.unwrap();
        drop(runtime);
        assert_eq!(status().await.unwrap(), (None, None));
        assert!(!paths.remotes_dir().join("_listener.json").exists());
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while tokio::net::TcpStream::connect(&addr).await.is_ok() {
            assert!(tokio::time::Instant::now() < deadline, "socket stayed open");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
