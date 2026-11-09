//! Startup repairs context indexes without giving reads mutation side effects.

use std::path::Path;
use std::sync::Arc;

use orch_core::storage::ContextEntry;
use orch_core::test_support::FakeProcessManager;
use orch_engine::container::{EngineSeams, FullEngine, build_full_with_processes};

async fn engine(root: &Path) -> FullEngine {
    tokio::fs::create_dir_all(root.join(".orchestry"))
        .await
        .unwrap();
    build_full_with_processes(
        root,
        EngineSeams {
            global_config_path: Some(root.join("global.yml")),
            ..Default::default()
        },
        Default::default(),
        Arc::new(FakeProcessManager::new()),
    )
    .await
    .unwrap()
}

async fn assert_index_matches_entities(full: &FullEngine) {
    let mut entities = Vec::new();
    let mut files = tokio::fs::read_dir(full.paths.context_dir()).await.unwrap();
    while let Some(file) = files.next_entry().await.unwrap() {
        let path = file.path();
        if path.extension().is_some_and(|ext| ext == "json") && file.file_name() != "_index.json" {
            entities.push(
                serde_json::from_slice::<ContextEntry>(&tokio::fs::read(path).await.unwrap())
                    .unwrap(),
            );
        }
    }
    let mut index: Vec<ContextEntry> = serde_json::from_slice(
        &tokio::fs::read(full.paths.context_dir().join("_index.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    entities.sort_by(|a, b| a.key.cmp(&b.key));
    index.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(index, entities);
}

async fn startup_rebuilds_context_index(corrupt: bool) {
    let temp = tempfile::tempdir().unwrap();
    let full = engine(temp.path()).await;
    full.context.set("alpha", "one", None).await.unwrap();
    full.context.set("beta", "two", None).await.unwrap();
    let index = full.paths.context_dir().join("_index.json");
    if corrupt {
        tokio::fs::write(&index, b"broken index").await.unwrap();
    } else {
        tokio::fs::remove_file(&index).await.unwrap();
    }

    assert_eq!(
        full.context.get("alpha").await.unwrap().unwrap().value,
        "one"
    );
    assert_eq!(full.context.list().await.unwrap().len(), 2);
    assert_eq!(full.context.get_all().await.unwrap().len(), 2);
    if corrupt {
        assert_eq!(tokio::fs::read(&index).await.unwrap(), b"broken index");
    } else {
        assert!(!tokio::fs::try_exists(&index).await.unwrap());
    }

    full.daemon_initialize().await.unwrap();
    assert_index_matches_entities(&full).await;
    full.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn startup_rebuilds_missing_context_index() {
    startup_rebuilds_context_index(false).await;
}

#[tokio::test]
async fn startup_rebuilds_corrupt_context_index() {
    startup_rebuilds_context_index(true).await;
}

#[tokio::test]
async fn startup_preserves_a_valid_stale_context_index_for_offline_repair() {
    let temp = tempfile::tempdir().unwrap();
    let full = engine(temp.path()).await;
    full.context.set("alpha", "one", None).await.unwrap();
    let index = full.paths.context_dir().join("_index.json");
    tokio::fs::write(&index, b"[]\n").await.unwrap();

    full.daemon_initialize().await.unwrap();
    assert_eq!(tokio::fs::read(&index).await.unwrap(), b"[]\n");
    assert!(full.context.get("alpha").await.unwrap().is_some());
    full.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn context_index_publish_failure_blocks_startup() {
    let temp = tempfile::tempdir().unwrap();
    let full = engine(temp.path()).await;
    full.context.set("alpha", "one", None).await.unwrap();
    let index = full.paths.context_dir().join("_index.json");
    tokio::fs::remove_file(&index).await.unwrap();
    tokio::fs::create_dir(&index).await.unwrap();

    assert!(full.daemon_initialize().await.is_err());
    tokio::fs::remove_dir(&index).await.unwrap();
    full.daemon_initialize().await.unwrap();
    assert_index_matches_entities(&full).await;
    full.orchestrator.daemon_stop().await.unwrap();
}

#[tokio::test]
async fn malformed_context_entity_is_nonfatal_and_does_not_publish_a_partial_index() {
    use orch_domain::{OrchestratorEvent, OrchestratorEventType};
    use std::sync::atomic::{AtomicBool, Ordering};

    let temp = tempfile::tempdir().unwrap();
    let full = engine(temp.path()).await;
    full.context.set("alpha", "one", None).await.unwrap();
    let index = full.paths.context_dir().join("_index.json");
    let corrupt = full.paths.context_dir().join("bad.json");
    tokio::fs::write(&index, b"broken index").await.unwrap();
    tokio::fs::write(&corrupt, b"not json").await.unwrap();
    let reported = Arc::new(AtomicBool::new(false));
    let seen = reported.clone();
    let _handler = full
        .events
        .on(OrchestratorEventType::OrchestratorError, move |event| {
            if matches!(event, OrchestratorEvent::OrchestratorError { context, fatal: false, .. }
            if context == "context index rebuild")
            {
                seen.store(true, Ordering::SeqCst);
            }
        });

    full.daemon_initialize().await.unwrap();
    assert!(reported.load(Ordering::SeqCst));
    assert_eq!(tokio::fs::read(&index).await.unwrap(), b"broken index");
    assert_eq!(tokio::fs::read(&corrupt).await.unwrap(), b"not json");
    assert_eq!(
        full.context.get("alpha").await.unwrap().unwrap().value,
        "one"
    );
    full.orchestrator.daemon_stop().await.unwrap();
}
