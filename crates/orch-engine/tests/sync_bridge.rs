//! The synchronous-caller bridge, proved before anything is built on it.
//!
//! The TUI's facade (`orch_tui::backend::TuiBackend`) is **synchronous** —
//! every method returns a value, not a future — while every engine service is
//! `async`, and `orch-cli` already runs its whole `dispatch` inside
//! `Runtime::block_on`. So a ratatui draw loop placed inside `dispatch` would
//! be on a thread that is driving the runtime, and `Handle::block_on` panics
//! there.
//!
//! The way out is to move the engine onto a plain `std::thread` and hand it a
//! `Handle`: short calls become `handle.block_on(…)`, and anything long —
//! dispatching a run, winding the orchestrator down — becomes `handle.spawn(…)`
//! so the frame is never blocked on an agent.
//!
//! This file pins that shape. It is deliberately not a test *of* the TUI: it
//! asserts the engine-side contract the facade will depend on, so a regression
//! shows up here rather than as 56 broken methods.

use std::sync::Arc;
use std::time::Duration;

use orch_core::storage::TaskFilter;
use orch_domain::{CreateTaskInput, TaskStatus};
use orch_engine::{FullEngine, build_full};
use orch_infra::storage::Paths;

/// An initialized project, which is all `require_init` wants.
fn project() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    std::fs::create_dir_all(Paths::new(temp.path()).root()).expect("create .orchestry");
    temp
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_can_be_driven_synchronously_from_a_non_runtime_thread() {
    let temp = project();
    let engine = Arc::new(build_full(temp.path()).await.expect("build_full"));
    let handle = tokio::runtime::Handle::current();

    // The facade's own thread. `std::thread` rather than `spawn_blocking` so
    // there is no doubt the thread is not driving the runtime.
    let worker = {
        let engine: Arc<FullEngine> = Arc::clone(&engine);
        let handle = handle.clone();
        std::thread::spawn(move || {
            // A short call: block the caller, as `create_task` will.
            let task = handle
                .block_on(engine.task_service.create(CreateTaskInput {
                    title: "from the facade".into(),
                    ..Default::default()
                }))
                .expect("create a task");

            // A read-back, as `refresh_tasks` will.
            let tasks = handle
                .block_on(engine.task_service.list(&TaskFilter::default()))
                .expect("list tasks");

            (task.id, tasks.len())
        })
    };

    let (task_id, listed) = worker.join().expect("the facade thread panicked");
    assert_eq!(listed, 1);
    assert_eq!(
        engine
            .task_service
            .get(&task_id)
            .await
            .expect("read back")
            .title,
        "from the facade"
    );
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn long_running_work_is_handed_to_the_runtime_rather_than_blocking_the_caller() {
    // The shape `run_task`, `run_all`, `start_watch` and `stop_watch` need:
    // the facade returns immediately and the outcome arrives later, through
    // the event bus.
    let temp = project();
    let engine = Arc::new(build_full(temp.path()).await.expect("build_full"));
    let handle = tokio::runtime::Handle::current();
    let mut events = engine.events.subscribe();

    let worker = {
        let engine: Arc<FullEngine> = Arc::clone(&engine);
        let handle = handle.clone();
        std::thread::spawn(move || {
            // Fire-and-forget: the join handle is dropped on purpose.
            handle.spawn(async move {
                let _ = engine
                    .task_service
                    .create(CreateTaskInput {
                        title: "spawned".into(),
                        ..Default::default()
                    })
                    .await;
            });
            // Returning without awaiting is the whole point.
        })
    };
    worker.join().expect("the facade thread panicked");

    // The work landed, and the facade learned about it through the bus — which
    // is exactly how `poll_events` will see it.
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("the spawned work never reported")
        .expect("the bus was dropped");
    assert_eq!(
        event.event_type(),
        orch_domain::OrchestratorEventType::TaskCreated
    );
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bus_subscription_survives_being_drained_from_another_thread() {
    // `poll_events` is called once per frame from the facade thread while the
    // engine emits from runtime threads. A `broadcast::Receiver` is `Send`,
    // so it can live on the draw loop's side; this pins that.
    let temp = project();
    let engine = Arc::new(build_full(temp.path()).await.expect("build_full"));
    // The lead's instruction: nothing should warn however many handlers the
    // dashboard attaches.
    engine.events.set_max_listeners(0);
    let handle = tokio::runtime::Handle::current();

    for index in 0..3 {
        engine
            .task_service
            .create(CreateTaskInput {
                title: format!("task {index}"),
                ..Default::default()
            })
            .await
            .expect("create");
    }

    let mut subscription = engine.events.subscribe();
    // Emitted *after* subscribing, since a broadcast receiver sees nothing
    // earlier — the facade therefore has to subscribe before its first frame.
    engine
        .task_service
        .create(CreateTaskInput {
            title: "after subscribing".into(),
            ..Default::default()
        })
        .await
        .expect("create");

    let drained = std::thread::spawn(move || {
        let _ = &handle;
        subscription.try_drain().len()
    })
    .join()
    .expect("the facade thread panicked");

    assert_eq!(drained, 1);
    assert_eq!(
        engine
            .task_service
            .list(&TaskFilter::with_status(TaskStatus::Todo))
            .await
            .expect("list")
            .len(),
        4
    );
    drop(temp);
}
