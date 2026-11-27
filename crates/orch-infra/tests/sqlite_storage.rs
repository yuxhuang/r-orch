//! Public factory smoke proof and the SQLite-only Amendment 3 callback vector.
#[path = "sqlite_callback_context/vector.rs"]
mod callback_context;

#[tokio::test]
async fn admitted_callback_has_no_tokio_context() {
    use orch_infra::{
        data_dir::{DataDir, Holder, LockKind},
        sqlite::{SqliteStorage, StorageOptions},
    };
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(
        &root.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    callback_context::admitted_callback_has_no_tokio_context(
        &storage,
        orch_core::store::Caller {
            project: "p".into(),
            principal: orch_core::store::Principal::Owner {
                identity: "synthetic".into(),
            },
        },
    )
    .await;
    storage.close().await.unwrap();
}
