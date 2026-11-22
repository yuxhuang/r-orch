//! The file reference's factory: a fresh directory per scenario, kept until the
//! backend is dropped. Everything else comes from the shared v2 scenarios.

use std::sync::Arc;

use orch_core::test_support::assert_hermetic;
use orch_infra::storage::v2::FileStorage;

use crate::v2::Backend;

pub fn backend() -> Backend {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    Backend {
        storage: Arc::new(FileStorage::new(root.path())),
        _guard: Box::new(root),
    }
}
