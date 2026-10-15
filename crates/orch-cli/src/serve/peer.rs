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
            runtime = Some(PeerRuntime::start(Arc::clone(&engine)).await?);
            Ok(())
        })
        .await?;
    Ok(runtime.expect("successful watch startup prepared the peer runtime"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::storage::ConfigStore;
    use orch_infra::storage::{FileConfigStore, Paths};

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
}
