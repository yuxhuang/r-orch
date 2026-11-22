//! The file reference's factories: a fresh directory per scenario, kept until the
//! backend is dropped. Everything else comes from the shared v2 scenarios.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::store::{Caller, Lane, ReadSnapshot, Result, Storage, WriteWork};
use orch_core::test_support::assert_hermetic;
use orch_infra::storage::v2::FileStorage;

use crate::v2::Backend;
use crate::v2_operations::{Fixture, Restore};

/// A file reference that owns its directory, so a scenario that holds the
/// storage keeps the files too.
struct Held {
    storage: FileStorage,
    _root: tempfile::TempDir,
}

pub fn backend() -> Backend {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    Backend {
        storage: Arc::new(FileStorage::new(root.path())),
        _guard: Box::new(root),
    }
}

fn held() -> Arc<Held> {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    Arc::new(Held {
        storage: FileStorage::new(root.path()),
        _root: root,
    })
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
    #[allow(
        clippy::unwrap_used,
        reason = "fixture hook outside #[test] functions; clippy.toml allows unwrap only inside them"
    )]
    async fn restore_to_lineage(&self, lineage: &str) {
        self.storage.restore_to_lineage(lineage).await.unwrap();
    }
}

/// The lease and combined scenarios' fixture, with the lineage the memory
/// fixture starts on.
pub async fn operations_backend() -> Fixture {
    let held = held();
    held.restore_to_lineage("initial-lineage").await;
    Fixture {
        storage: held.clone(),
        restore: held,
        initial_lineage: "initial-lineage".into(),
    }
}

/// The event scenarios' fixture: the same seeded leases as the memory one.
pub async fn events_backend() -> Arc<dyn Storage> {
    let held = held();
    held.restore_to_lineage("current").await;
    crate::events_v2::seed(held.as_ref()).await;
    held
}
