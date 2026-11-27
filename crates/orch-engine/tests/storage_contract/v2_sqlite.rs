//! The real SQLite factory for the shared v2 contracts. The restore fixture
//! hook changes only synthetic storage; it is not a product restore endpoint.
#![allow(clippy::unwrap_used, reason = "synthetic conformance fixture setup")]
use crate::{
    v2::Backend,
    v2_operations::{Fixture, Restore},
};
use async_trait::async_trait;
use orch_core::store::{Caller, Lane, ReadSnapshot, Result, Storage, WriteWork};
use orch_core::test_support::assert_hermetic;
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{ConnectionRole, SqliteStorage, StorageOptions, Synchronous, open_connection},
};
use std::sync::Arc;

struct Held {
    storage: SqliteStorage,
    data: Arc<DataDir>,
    _root: tempfile::TempDir,
}
fn held() -> Arc<Held> {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
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
    Arc::new(Held {
        storage,
        data,
        _root: root,
    })
}
pub fn backend() -> Backend {
    Backend {
        storage: held(),
        _guard: Box::new(()),
    }
}
#[async_trait]
impl Storage for Held {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>> {
        self.storage.read(caller).await
    }
    async fn write(&self, caller: Caller, lane: Lane, work: WriteWork) -> Result<()> {
        self.storage.write(caller, lane, work).await
    }
}
#[async_trait]
impl Restore for Held {
    async fn restore_to_lineage(&self, lineage: &str) {
        let lineage = lineage.to_owned();
        let data = self.data.clone();
        // Fixture authority, like FakeStorage::restore_to_lineage. No daemon,
        // live state, arbitrary user query, or runtime activation is involved.
        tokio::task::spawn_blocking(move || {
            let mut conn =
                open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
            let tx = conn.transaction().unwrap();
            tx.execute("DELETE FROM leases", []).unwrap();
            tx.execute(
                "UPDATE daemon_config SET value=?1 WHERE key='lineage'",
                [lineage],
            )
            .unwrap();
            tx.commit().unwrap();
        })
        .await
        .unwrap();
    }
}
pub async fn operations_backend() -> Fixture {
    let held = held();
    held.restore_to_lineage("initial-lineage").await;
    Fixture {
        storage: held.clone(),
        restore: held,
        initial_lineage: "initial-lineage".into(),
    }
}
pub async fn events_backend() -> Arc<dyn Storage> {
    let held = held();
    held.restore_to_lineage("current").await;
    crate::events_v2::seed(held.as_ref()).await;
    held
}
