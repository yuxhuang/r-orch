//! SQLite-only callback vector; the adapter test supplies its real storage.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orch_core::store::{Caller, Lane, Storage, StorageExt};

/// Call from a Tokio test with a live SQLite adapter and an admitted caller.
/// The memory and file references intentionally do not satisfy this rule.
pub async fn admitted_callback_has_no_tokio_context(storage: &dyn Storage, caller: Caller) {
    assert!(tokio::runtime::Handle::try_current().is_ok());
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&calls);
    let expected_caller = caller.clone();
    let (no_runtime, actual_caller) = storage
        .transaction(caller, Lane::Entities, move |unit| {
            callback_calls.fetch_add(1, Ordering::SeqCst);
            Ok((
                tokio::runtime::Handle::try_current().is_err(),
                unit.caller().clone(),
            ))
        })
        .await
        .expect("SQLite callback must be admitted and commit");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(actual_caller, expected_caller);
    assert!(
        no_runtime,
        "SQLite callback entered a Tokio runtime context"
    );
}
