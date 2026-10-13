//! Ported from `test/unit/application/orchestrator-mutex.test.ts`.
//!
//! The TS suite extracts the promise-chain mutex and tests the algorithm in
//! isolation, because the algorithm was hand-rolled. Here the mutex **is**
//! `tokio::sync::Mutex`, so testing the library would prove nothing; these
//! cases assert the four properties the TS suite was protecting, against the
//! orchestrator's own state lock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orch_domain::OrchestratorState;

use super::harness::Harness;

#[tokio::test]
async fn critical_sections_serialize_regardless_of_their_duration() {
    let h = Harness::builder().build();
    h.set_state(OrchestratorState::default()).await;
    let order: Arc<std::sync::Mutex<Vec<usize>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    let make = |first: usize, second: usize, delay_ms: u64| {
        let orchestrator = h.orchestrator.clone();
        let order = Arc::clone(&order);
        async move {
            let _guard = orchestrator.inner.state.lock().await;
            order.lock().unwrap().push(first);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            order.lock().unwrap().push(second);
        }
    };

    // Started in order, with decreasing durations: a non-serializing lock
    // would interleave them.
    let first = tokio::spawn(make(1, 2, 30));
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    let second = tokio::spawn(make(3, 4, 10));
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    let third = tokio::spawn(make(5, 6, 0));

    first.await.unwrap();
    second.await.unwrap();
    third.await.unwrap();

    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3, 4, 5, 6]);
}

#[tokio::test]
async fn the_lock_is_released_even_when_the_critical_section_fails() {
    let h = Harness::builder().build();
    h.set_state(OrchestratorState::default()).await;
    h.state.faults.fail_once("boom");

    // `tick_once` takes the state lock and fails inside it.
    assert!(h.tick().await.is_err());

    // A second critical section still runs, and promptly.
    h.state.faults.clear();
    tokio::time::timeout(std::time::Duration::from_millis(500), h.tick())
        .await
        .expect("the lock was not released")
        .unwrap();
}

#[tokio::test]
async fn there_are_no_lost_updates_under_a_read_modify_write_race() {
    let h = Harness::builder().build();
    h.set_state(OrchestratorState::default()).await;

    let mut handles = Vec::new();
    for _ in 0..20 {
        let orchestrator = h.orchestrator.clone();
        handles.push(tokio::spawn(async move {
            let mut slot = orchestrator.inner.state.lock().await;
            let state = slot.as_mut().expect("state");
            let current = state.stats.total_runs;
            // An await inside the section is exactly where a promise-chain
            // mutex that did not hold across awaits would lose the update.
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            state.stats.total_runs = current + 1;
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(h.in_memory_state().await.stats.total_runs, 20);
}

#[tokio::test]
async fn high_contention_does_not_deadlock() {
    let h = Harness::builder().build();
    h.set_state(OrchestratorState::default()).await;
    let count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..100 {
        let orchestrator = h.orchestrator.clone();
        let count = Arc::clone(&count);
        handles.push(tokio::spawn(async move {
            let _guard = orchestrator.inner.state.lock().await;
            count.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let all = futures::future::join_all(handles);
    tokio::time::timeout(std::time::Duration::from_secs(5), all)
        .await
        .expect("deadlock under contention");

    assert_eq!(count.load(Ordering::SeqCst), 100);
}

#[tokio::test]
async fn the_internal_phases_cannot_re_enter_the_state_lock() {
    // The TS deadlock this replaces: `reconcile` runs inside the lock and must
    // call the `_`-prefixed failure handler, not the locking one. Here the
    // phases take `&mut OrchestratorState`, so re-entry is a type error — this
    // case pins the property that makes that true, namely that holding the
    // guard and then calling a phase is what the code actually does.
    let h = Harness::builder().build();
    let mut slot = h.orchestrator.inner.state.lock().await;
    h.orchestrator.load_state(&mut slot).await.unwrap();
    let state = slot.as_mut().expect("state");

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        h.orchestrator.reconcile(state),
    )
    .await
    .expect("a phase re-entered the state lock")
    .unwrap();
}
