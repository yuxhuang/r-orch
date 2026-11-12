//! The whole stack: the real dashboard, over the real daemon client, over a
//! test daemon on a real project tree — driven from a thread that is not
//! driving the runtime.
//!
//! `tui_backend.rs` tests the facade method by method against a test daemon;
//! `orch-tui`'s own suites test the dashboard against a `FakeBackend`. Only
//! here do the two halves meet, which is where the mistakes that neither suite
//! can see live: a `Handle::block_on` on the wrong thread, a method the `App`
//! calls on its first frame that the facade gets wrong, and the `App::new`
//! → `context()` → render path that `orch tui` runs before anything is
//! interactive.

mod tui_support;

use std::sync::Arc;

use orch_core::storage::ConfigStore;
use orch_domain::{CreateAgentInput, CreateTaskInput, TaskStatus, WorkspaceMode, default_config};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_tui::clock::TestClock;
use orch_tui::state::ViewId;
use orch_tui::{App, views};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use orch_cli::tui::DaemonBackend;
use tui_support::TestDaemon;

async fn project(name: &str) -> (tempfile::TempDir, Arc<TestDaemon>) {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tui_support::create_state_dir(temp.path());

    let mut config = default_config();
    config.project.name = name.to_owned();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    // The daemon's dispatch is OFF; this only keeps a future ON test quiet.
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    let engine = Arc::new(TestDaemon::start_default(temp.path()).await);
    (temp, engine)
}

/// Build the `App` over the facade and run `body` on a thread that is not
/// driving the runtime — `orch_tui::run`'s thread, in other words.
///
/// Everything the dashboard does to the engine happens inside `body`, so a
/// `Handle::block_on` from a runtime context would panic here exactly as it
/// would in production.
async fn on_dashboard<T, F>(backend: DaemonBackend, body: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&mut App<DaemonBackend, TestClock>, &mut Terminal<TestBackend>) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut app = App::new(backend, TestClock::new(1_000_000));
                    app.state.width = 100;
                    app.state.height = 30;
                    let mut terminal =
                        Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
                    body(&mut app, &mut terminal)
                })
                .join()
                .expect("the dashboard thread panicked")
        })
    })
    .await
    .expect("the blocking task panicked")
}

fn frame(app: &App<DaemonBackend, TestClock>, terminal: &mut Terminal<TestBackend>) -> String {
    terminal.draw(|f| _ = views::draw(f, app)).expect("draw");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dashboard_renders_what_the_engine_has_on_disk() {
    let (_temp, engine) = project("live-project").await;
    engine
        .task_service
        .create(CreateTaskInput {
            title: "Port the collector".into(),
            ..Default::default()
        })
        .await
        .expect("create task");
    engine
        .agent_service
        .create(CreateAgentInput {
            name: "builder".into(),
            // A **shell** agent, not `claude`: this test dispatches, and with
            // a stream adapter the real `claude` CLI gets exec'd — with
            // `--dangerously-skip-permissions` and a rendered prompt — on any
            // machine that has it installed. Nothing here asserts the adapter;
            // it only needs an agent to exist and be dispatchable. Caught by
            // the gate's `no-real-cli` step.
            adapter: "shell".into(),
            command: Some("echo dashboard-fixture".into()),
            ..Default::default()
        })
        .await
        .expect("create agent");

    let backend = engine.launch().await;
    let (header, tasks, agents) = on_dashboard(backend, |app, terminal| {
        // `App::new` has already called `context()` and the three refreshes,
        // which is what the first frame of `orch tui` does.
        let header = frame(app, terminal);
        app.state.view = ViewId::Tasks;
        let tasks = frame(app, terminal);
        app.state.view = ViewId::Agents;
        let agents = frame(app, terminal);
        (header, tasks, agents)
    })
    .await;

    assert!(header.contains("live-project"), "{header}");
    assert!(header.contains("9.9.9"), "{header}");
    assert!(tasks.contains("Port the collector"), "{tasks}");
    assert!(agents.contains("builder"), "{agents}");
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_task_through_the_dashboard_reaches_the_store() {
    let (_temp, engine) = project("round-trip").await;
    let backend = engine.launch().await;

    // `/task add <title>` is the shortest real mutation path: it goes through
    // the same `create_task` the wizard calls, and proves a write survives the
    // facade, the service and the store while the draw loop owns the thread.
    let frame_after = on_dashboard(backend, |app, terminal| {
        app.state.view = ViewId::Tasks;
        frame(app, terminal);
        app.execute_command("/task add Wired end to end");
        frame(app, terminal)
    })
    .await;

    let tasks = engine
        .task_service
        .list(&Default::default())
        .await
        .expect("list");
    let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["Wired end to end"]);
    assert_eq!(tasks[0].status, TaskStatus::Todo);
    // The status bar counted it, which means the post-mutation `refresh_tasks`
    // went back through the facade and saw the write — the dashboard is not
    // merely rendering its own optimistic copy.
    assert!(frame_after.contains("TODO 1"), "{frame_after}");
}

#[tokio::test(flavor = "multi_thread")]
async fn quitting_sets_the_exit_flag_the_runner_loops_on() {
    let (_temp, engine) = project("quit").await;
    let backend = engine.launch().await;

    let should_exit = on_dashboard(backend, |app, terminal| {
        frame(app, terminal);
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.state.should_exit
    })
    .await;

    assert!(should_exit, "`q` must end `orch_tui::run`'s loop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_is_rendered_to_the_user_not_swallowed() {
    let (_temp, engine) = project("contested").await;
    let backend = engine.launch().await;

    let rendered = on_dashboard(backend, |app, terminal| {
        // `0` flushes the message batch synchronously.
        app.batch_ms = 0;
        app.execute_command("/run tsk_missing");
        frame(app, terminal)
    })
    .await;

    // Returned synchronously: the dashboard has no renderer for
    // `orchestrator:error`, so this text is the only way the user learns why
    // (BACKEND.md §3).
    assert!(rendered.contains("Failed:"), "{rendered}");
    assert!(rendered.contains("tsk_missing"), "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_in_the_dashboard_turns_the_daemons_dispatch_on_and_the_header_follows() {
    let (_temp, engine) = project("switch").await;
    let backend = engine.launch().await;

    let (idle, watching) = on_dashboard(backend, |app, terminal| {
        app.batch_ms = 0;
        let idle = frame(app, terminal);
        app.execute_command("/watch");
        (idle, frame(app, terminal))
    })
    .await;
    assert!(idle.contains("IDLE"), "{idle}");
    assert!(watching.contains("WATCHING"), "{watching}");
    let mut probe = orch_client::Client::connect(&engine.client_config())
        .await
        .expect("probe");
    assert!(probe.status().await.expect("status").dispatch.dispatch_on);
}
