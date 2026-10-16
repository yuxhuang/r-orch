//! One engine backs every TUI watch/pause cycle.

use super::harness::Harness;
use orch_domain::OrchestratorEventType;
use std::time::Duration;

#[tokio::test]
async fn repeated_watch_pause_cycles_tick_and_release_the_lock_every_time() {
    let h = Harness::builder().build();
    for cycle in 1..=4 {
        h.start_watch().await.unwrap();
        assert!(h.orchestrator.is_owner(), "cycle {cycle}");
        assert!(!h.orchestrator.is_shutting_down(), "cycle {cycle}");
        assert_eq!(
            h.events_of(OrchestratorEventType::OrchestratorTick).len(),
            cycle
        );
        let mut waiting = Box::pin(h.orchestrator.wait_for_stop());
        assert!(futures::poll!(&mut waiting).is_pending(), "cycle {cycle}");
        h.orchestrator.stop().await.unwrap();
        waiting.await;
        h.orchestrator.stop().await.unwrap();
        assert!(!h.lock.is_held(), "cycle {cycle}");
        assert_eq!(h.state.peek().pid, None, "cycle {cycle}");
        assert_eq!(h.lock.release_count(), cycle);
    }
}

#[tokio::test]
async fn a_previous_cycles_waiter_completes_even_after_the_next_watch_starts() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    let mut waiting = Box::pin(h.orchestrator.wait_for_stop());
    assert!(futures::poll!(&mut waiting).is_pending());
    h.orchestrator.stop().await.unwrap();
    h.start_watch().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap();
    let mut current = Box::pin(h.orchestrator.wait_for_stop());
    assert!(futures::poll!(&mut current).is_pending());
    h.orchestrator.stop().await.unwrap();
    current.await;
}

#[tokio::test]
async fn watch_restart_waits_for_in_progress_stop_before_acquiring_the_lock() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    let state = h.orchestrator.inner.state.lock().await;
    let mut stopping = Box::pin(h.orchestrator.stop());
    assert!(futures::poll!(&mut stopping).is_pending());
    let mut restarting = Box::pin(h.start_watch());
    assert!(futures::poll!(&mut restarting).is_pending());
    drop(state);
    tokio::time::timeout(Duration::from_secs(2), async {
        let (stopped, started) = tokio::join!(stopping, restarting);
        stopped.unwrap();
        started.unwrap();
    })
    .await
    .unwrap();
    assert!(h.orchestrator.is_owner());
    h.orchestrator.stop().await.unwrap();
    assert_eq!(h.lock.release_count(), 2);
}

#[tokio::test]
async fn watch_restart_refuses_incomplete_shutdown_instead_of_resetting_its_owner() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    let state = h.orchestrator.inner.state.lock().await;
    let mut stopping = Box::pin(h.orchestrator.stop());
    assert!(futures::poll!(&mut stopping).is_pending());
    drop(stopping);
    drop(state);
    let error = h.start_watch().await.unwrap_err();
    assert!(
        error.to_string().contains("shutdown did not complete"),
        "{error}"
    );
    assert!(h.orchestrator.is_owner());
}

#[tokio::test]
async fn resumed_watch_keeps_polling_after_its_initial_tick() {
    let mut config = super::harness::test_config();
    config.scheduling.poll_interval_ms = 1;
    let h = Harness::builder().config(config).build();
    for _ in 0..3 {
        h.start_watch().await.unwrap();
        // Subscribe after the synchronous first tick: only the background
        // loop can produce this event. No sleeps or real child processes.
        let mut events = h.bus.subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = events.recv().await.unwrap();
                if matches!(
                    *event,
                    orch_domain::OrchestratorEvent::OrchestratorTick { .. }
                ) {
                    break;
                }
            }
        })
        .await
        .expect("resumed watch loop stopped polling");
        h.orchestrator.stop().await.unwrap();
    }
}

#[tokio::test]
async fn held_prepare_bounds_stop_and_concurrent_start_without_resetting_the_cycle() {
    let h = Harness::builder().build();
    let prepare = tokio::sync::Semaphore::new(0);
    let mut starting = Box::pin(h.orchestrator.start_watch_with(
        super::super::WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        },
        || async {
            prepare.acquire().await.unwrap().forget();
            Ok(())
        },
    ));
    assert!(futures::poll!(&mut starting).is_pending());
    assert!(h.orchestrator.is_owner());
    let cycle = h.orchestrator.inner.stopped.lock().unwrap().subscribe();
    let (stop, start) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(h.orchestrator.stop(), h.start_watch())
    })
    .await
    .expect("new lifecycle waits must be bounded");
    assert!(stop.unwrap_err().to_string().contains("Timed out"));
    assert!(start.unwrap_err().to_string().contains("Timed out"));
    assert!(h.orchestrator.is_owner());
    assert!(!h.orchestrator.is_shutting_down());
    assert!(!*cycle.borrow());
    prepare.add_permits(1);
    starting.await.unwrap();
    h.orchestrator.stop().await.unwrap();
    assert!(*cycle.borrow());
    assert_eq!(h.lock.release_count(), 1);
}

#[tokio::test]
async fn restart_timeout_retains_old_loop_and_cycle_until_the_loop_finishes() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    h.orchestrator.stop().await.unwrap();
    // Replace the already cancelled loop with a gated old-loop tail, such as
    // a filesystem touch after tick has released the state lock.
    let previous = h.orchestrator.inner.watch_task.lock().await.take().unwrap();
    previous.await.unwrap();
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let task_gate = gate.clone();
    *h.orchestrator.inner.watch_task.lock().await = Some(tokio::spawn(async move {
        task_gate.acquire().await.unwrap().forget();
    }));
    let cycle = h.orchestrator.inner.stopped.lock().unwrap().subscribe();
    let error = tokio::time::timeout(Duration::from_secs(5), h.start_watch())
        .await
        .expect("old watch join must be bounded")
        .unwrap_err();
    assert!(error.to_string().contains("Timed out"), "{error}");
    assert!(
        h.orchestrator
            .inner
            .watch_task
            .lock()
            .await
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    );
    assert!(!h.orchestrator.is_owner());
    assert!(h.orchestrator.is_shutting_down());
    assert!(*cycle.borrow());
    assert_eq!(
        h.events_of(OrchestratorEventType::OrchestratorTick).len(),
        1
    );
    // Cancelling a retry must also retain the handle, not detach it.
    let mut retry = Box::pin(h.start_watch());
    assert!(futures::poll!(&mut retry).is_pending());
    drop(retry);
    assert!(h.orchestrator.inner.watch_task.lock().await.is_some());
    gate.add_permits(1);
    h.start_watch().await.unwrap();
    assert!(h.orchestrator.is_owner());
    assert!(!h.orchestrator.is_shutting_down());
    assert!(*cycle.borrow());
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn restart_times_out_behind_in_progress_stop_without_acquiring_ownership() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();
    let state = h.orchestrator.inner.state.lock().await;
    let mut stopping = Box::pin(h.orchestrator.stop());
    assert!(futures::poll!(&mut stopping).is_pending());
    let error = tokio::time::timeout(Duration::from_secs(5), h.start_watch())
        .await
        .expect("restart shutdown wait must be bounded")
        .unwrap_err();
    assert!(error.to_string().contains("Timed out"), "{error}");
    assert!(h.orchestrator.is_owner());
    assert!(h.orchestrator.is_shutting_down());
    assert_eq!(h.lock.release_count(), 0);
    drop(state);
    stopping.await.unwrap();
    h.start_watch().await.unwrap();
    h.orchestrator.stop().await.unwrap();
    assert_eq!(h.lock.release_count(), 2);
}

#[tokio::test]
async fn loop_installation_timeout_releases_ownership_and_unsubscribes() {
    let h = Harness::builder().build();
    let slot = h.orchestrator.inner.watch_task.lock().await;
    let error = tokio::time::timeout(Duration::from_secs(5), h.start_watch())
        .await
        .expect("loop installation must be bounded")
        .unwrap_err();
    assert!(error.to_string().contains("Timed out"), "{error}");
    assert!(!h.orchestrator.is_owner());
    assert!(!h.lock.is_held());
    assert_eq!(h.bus.listener_count(OrchestratorEventType::TaskCreated), 0);
    assert!(slot.is_none());
    drop(slot);
    h.start_watch().await.unwrap();
    h.orchestrator.stop().await.unwrap();
}
