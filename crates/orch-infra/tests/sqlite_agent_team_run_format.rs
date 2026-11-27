//! R142 named public-port cases on the real SQLite adapter.
//! The vectors are shared with the reference backends in storage_conformance.
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
};
use std::sync::Arc;

#[path = "../../orch-engine/tests/storage_contract/v2_r142_public.rs"]
mod r142_public;

// Retain the directory lock and its synthetic files until the storage workers close.
struct Fixture {
    storage: SqliteStorage,
    _data: Arc<DataDir>,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(root.path());
        let data = Arc::new(
            DataDir::acquire(
                &root.path().join("data"),
                &Holder {
                    kind: LockKind::Daemon,
                    pid: std::process::id(),
                    started_at: "synthetic".into(),
                    binary_version: "test".into(),
                    executable: "/synthetic/orch".into(),
                },
            )
            .unwrap(),
        );
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        Self {
            storage,
            _data: data,
            _root: root,
        }
    }
}

#[tokio::test]
async fn runs_of_one_task_with_attempts_2_10_10_list_numerically_descending_with_binary_id_ties() {
    let fixture = Fixture::new();
    r142_public::runs_of_one_task_with_attempts_2_10_10_list_numerically_descending_with_binary_id_ties(
        &fixture.storage,
    ).await;
    fixture.storage.close().await.unwrap();
}

#[tokio::test]
async fn filters_pages_and_cursors_agree_with_the_reference_for_all_three_families() {
    let fixture = Fixture::new();
    r142_public::filters_pages_and_cursors_agree_with_the_reference_for_all_three_families(
        &fixture.storage,
    )
    .await;
    fixture.storage.close().await.unwrap();
}
