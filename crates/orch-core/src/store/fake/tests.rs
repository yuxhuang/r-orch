use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU8, Ordering};

use futures::FutureExt;

use super::*;
use crate::store::blob::BlobStore;
use crate::store::retries::{MAX_RETRY_ERROR_BYTES, Retry};

// Reversible names isolate the async boundary from physical hashing (covered
// by the disk-backed reference tests). Faults start only after publication.
#[derive(Clone, Default)]
struct FaultBlobs(Arc<AtomicU8>);

impl BlobStore for FaultBlobs {
    type Hash = String;
    type Error = std::io::Error;

    fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
        String::from_utf8(bytes.to_vec()).map_err(std::io::Error::other)
    }

    fn get(&self, hash: &String) -> std::io::Result<Vec<u8>> {
        match self.0.load(Ordering::SeqCst) {
            1 => std::panic::panic_any(216_u32),
            2 => Err(std::io::Error::other("injected missing retry blob")),
            _ => Ok(hash.as_bytes().to_vec()),
        }
    }

    fn remove(&self, _: &String) -> std::io::Result<bool> {
        Ok(false)
    }
}

fn name(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn caller() -> Caller {
    Caller {
        project: "retry-boundary".into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

async fn fixture() -> (FakeStorage, FaultBlobs, Retry) {
    let blobs = FaultBlobs::default();
    let store = FakeStorage::with_retry_blob_store(blobs.clone(), name, MAX_RETRY_ERROR_BYTES);
    let row = Retry {
        task: "retry-task".into(),
        attempt: 1,
        due_at: 10,
        error: "x".repeat(MAX_RETRY_ERROR_BYTES + 1),
    };
    let saved = row.clone();
    store
        .write(
            caller(),
            Lane::Entities,
            Box::new(move |unit| unit.put_retry(&saved)),
        )
        .await
        .unwrap();
    (store, blobs, row)
}

#[derive(Clone, Copy)]
enum Read {
    Point,
    Due,
    Page,
}

async fn read(snapshot: &dyn ReadSnapshot, mode: Read) -> Result<Vec<Retry>> {
    match mode {
        Read::Point => Ok(snapshot
            .read_retry("retry-task")
            .await?
            .into_iter()
            .collect()),
        Read::Due => snapshot.read_due_retries(10, 1).await,
        Read::Page => Ok(snapshot.read_retries(None, 1).await?.rows),
    }
}

async fn resolver_panic(mode: Read) {
    let (store, blobs, original) = fixture().await;
    let snapshot = store.read(caller()).await.unwrap();
    // A later commit must not replace the snapshot's row, even after a panic.
    let mut replacement = original.clone();
    replacement.error = "replacement".into();
    store
        .write(
            caller(),
            Lane::Entities,
            Box::new(move |unit| unit.put_retry(&replacement)),
        )
        .await
        .unwrap();
    blobs.0.store(1, Ordering::SeqCst);
    let outcome = AssertUnwindSafe(read(snapshot.as_ref(), mode))
        .catch_unwind()
        .await;
    let payload = outcome.expect_err("resolver panic must reach the snapshot caller");
    assert_eq!(payload.downcast_ref::<u32>(), Some(&216));
    blobs.0.store(0, Ordering::SeqCst);
    assert_eq!(read(snapshot.as_ref(), mode).await.unwrap(), vec![original]);
    assert_eq!(
        store
            .read(caller())
            .await
            .unwrap()
            .read_retry("retry-task")
            .await
            .unwrap()
            .unwrap()
            .error,
        "replacement"
    );
}

#[tokio::test]
async fn point_snapshot_resumes_resolver_panic_and_retains_original_state() {
    resolver_panic(Read::Point).await;
}

#[tokio::test]
async fn due_snapshot_resumes_resolver_panic_and_retains_original_state() {
    resolver_panic(Read::Due).await;
}

#[tokio::test]
async fn paged_snapshot_resumes_resolver_panic_and_retains_original_state() {
    resolver_panic(Read::Page).await;
}

#[tokio::test]
async fn ordinary_resolver_damage_remains_a_backend_error_on_every_snapshot_read() {
    let (store, blobs, row) = fixture().await;
    let snapshot = store.read(caller()).await.unwrap();
    blobs.0.store(2, Ordering::SeqCst);
    for mode in [Read::Point, Read::Due, Read::Page] {
        let error = read(snapshot.as_ref(), mode).await.unwrap_err();
        assert!(
            matches!(error, StoreError::Backend(crate::Error::Domain(_))),
            "{error:?}"
        );
        assert!(
            error.to_string().contains("injected missing retry blob"),
            "{error}"
        );
    }
    blobs.0.store(0, Ordering::SeqCst);
    assert_eq!(snapshot.read_retry("retry-task").await.unwrap(), Some(row));
}

#[tokio::test]
async fn invalid_retry_read_bounds_keep_their_typed_errors() {
    let (store, _, _) = fixture().await;
    let snapshot = store.read(caller()).await.unwrap();
    assert!(matches!(
        snapshot.read_due_retries(10, 0).await,
        Err(StoreError::InvalidRetry(_))
    ));
    assert!(matches!(
        snapshot.read_retries(None, 0).await,
        Err(StoreError::InvalidRetry(_))
    ));
}

#[tokio::test]
async fn cancelled_retry_join_retains_the_typed_io_error_and_join_source() {
    // Cancellation cannot occur once spawn_blocking work has begun. Supply a
    // real cancelled join at the same completion boundary without timing races.
    let task = tokio::spawn(std::future::pending::<Result<()>>());
    task.abort();
    let error = finish_retry_read(task.await).unwrap_err();
    let StoreError::Backend(crate::Error::Io {
        context,
        path,
        source,
    }) = error
    else {
        panic!("cancelled join must remain a backend I/O error: {error:?}");
    };
    assert_eq!(context, "cannot join memory reference retry read");
    assert_eq!(path, None);
    let join = source
        .get_ref()
        .unwrap()
        .downcast_ref::<tokio::task::JoinError>()
        .unwrap();
    assert!(join.is_cancelled());
}
