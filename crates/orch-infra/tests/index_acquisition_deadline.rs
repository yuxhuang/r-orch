//! Acquisition includes filesystem work queued behind a saturated blocking pool.
use std::time::{Duration, Instant};

use orch_core::Error;
use orch_infra::storage::index::{IndexManager, ItemFormat};
use serde_json::{Value, json};

fn assert_blocking_pool_cannot_extend_deadline(warm: bool) {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let index = IndexManager::<Value>::new(temp.path().join("items"), ItemFormat::Json);
        if warm {
            index.write_index(&[]).await.unwrap();
        }
        let before = std::fs::read(index.index_path()).ok();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(12)).unwrap();
        });
        ready_rx.await.unwrap();
        let started = Instant::now();
        let items = [json!({"id": "late"})];
        let mut write = Box::pin(index.write_index(&items));
        let first = tokio::time::timeout(Duration::from_millis(6200), &mut write).await;
        let elapsed = started.elapsed();
        let returned_while_held = first.is_ok() && !blocker.is_finished();
        // Release and reap even on the broken path, before asserting the result.
        release_tx.send(()).unwrap();
        let result = match first {
            Ok(result) => result,
            Err(_) => tokio::time::timeout(Duration::from_secs(3), write)
                .await
                .unwrap(),
        };
        blocker.await.unwrap();
        // Drain queued filesystem work before checking for late publication.
        tokio::task::spawn_blocking(|| {}).await.unwrap();
        println!("warm={warm} acquisition_ms={} returned_while_held={returned_while_held} result={result:?}", elapsed.as_millis());
        assert!(returned_while_held, "acquisition outlived its deadline");
        assert!(elapsed >= Duration::from_secs(5));
        assert!(matches!(result, Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::TimedOut));
        assert_eq!(std::fs::read(index.index_path()).ok(), before, "timed-out writer published after release");
        index.write_index(&[json!({"id": "after"})]).await.unwrap();
        assert_eq!(index.read_index().await.unwrap(), vec![json!({"id": "after"})]);
    });
}

#[test]
fn cold_directory_acquisition_times_out_while_blocking_worker_is_held() {
    assert_blocking_pool_cannot_extend_deadline(false);
}

#[test]
fn warm_index_acquisition_times_out_while_blocking_worker_is_held() {
    assert_blocking_pool_cannot_extend_deadline(true);
}
