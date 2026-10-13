//! The whole stack: the real dashboard, over the real engine, over a real
//! project tree — driven from a thread that is not driving the runtime.
//!
//! `tui_backend.rs` tests the facade method by method against a `FullEngine`;
//! `orch-tui`'s own suites test the dashboard against a `FakeBackend`. Only
//! here do the two halves meet, which is where the mistakes that neither suite
//! can see live: a `Handle::block_on` on the wrong thread, a method the `App`
//! calls on its first frame that the facade gets wrong, and the `App::new`
//! → `context()` → render path that `orch tui` runs before anything is
//! interactive.

use std::sync::Arc;

use orch_core::storage::ConfigStore;
use orch_domain::{CreateAgentInput, CreateTaskInput, TaskStatus, WorkspaceMode, default_config};
use orch_engine::{FullEngine, build_full};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_tui::clock::TestClock;
use orch_tui::state::ViewId;
use orch_tui::{App, views};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use orch_cli::tui::EngineBackend;

async fn project(name: &str) -> (tempfile::TempDir, Arc<FullEngine>) {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.root())
        .await
        .expect("create .orchestry");

    let mut config = default_config();
    config.project.name = name.to_owned();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    // Long enough that the watch loop `launch` starts never fires a tick: the
    // assertions are about what the dashboard shows, not what the orchestrator
    // would do next.
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    let engine = Arc::new(build_full(temp.path()).await.expect("build_full"));
    (temp, engine)
}

/// Build the `App` over the facade and run `body` on a thread that is not
/// driving the runtime — `orch_tui::run`'s thread, in other words.
///
/// Everything the dashboard does to the engine happens inside `body`, so a
/// `Handle::block_on` from a runtime context would panic here exactly as it
/// would in production.
async fn on_dashboard<T, F>(backend: EngineBackend, body: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&mut App<EngineBackend, TestClock>, &mut Terminal<TestBackend>) -> T + Send + 'static,
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

fn frame(app: &App<EngineBackend, TestClock>, terminal: &mut Terminal<TestBackend>) -> String {
    terminal.draw(|f| views::draw(f, app)).expect("draw");
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
            adapter: "claude".into(),
            ..Default::default()
        })
        .await
        .expect("create agent");

    let backend = EngineBackend::launch(Arc::clone(&engine), "9.9.9").await;
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

    engine.orchestrator.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_task_through_the_dashboard_reaches_the_store() {
    let (_temp, engine) = project("round-trip").await;
    let backend = EngineBackend::launch(Arc::clone(&engine), "9.9.9").await;

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

    engine.orchestrator.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn quitting_sets_the_exit_flag_the_runner_loops_on() {
    let (_temp, engine) = project("quit").await;
    let backend = EngineBackend::launch(Arc::clone(&engine), "9.9.9").await;

    let should_exit = on_dashboard(backend, |app, terminal| {
        frame(app, terminal);
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.state.should_exit
    })
    .await;

    assert!(should_exit, "`q` must end `orch_tui::run`'s loop");
    engine.orchestrator.stop().await.expect("stop");
}
