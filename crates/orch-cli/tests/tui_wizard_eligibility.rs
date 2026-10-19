//! The dashboard wizards' final-submit eligibility check against the real
//! engine and its files (R7, D7; decision DQ2).
//!
//! Each scenario has its own temp project with three shell agents. The
//! dashboard runs on an observer `EngineBackend` (no orchestrator, nothing
//! dispatched), and the chosen agent is disabled or removed by a second,
//! independent engine over the same files, standing in for another process.
//! The outcome is read back from storage, not from the dashboard.

mod common;

use std::path::Path;
use std::sync::Arc;

use orch_cli::tui::EngineBackend;
use orch_core::storage::ConfigStore;
use orch_domain::{CreateAgentInput, WorkspaceMode, default_config};
use orch_engine::{EngineSeams, FullEngine, build_full_with};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_tui::App;
use orch_tui::clock::TestClock;
use orch_tui::wizard::configs;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

type Dashboard = App<EngineBackend, TestClock>;

fn key(app: &mut Dashboard, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn text(app: &mut Dashboard, value: &str) {
    for ch in value.chars() {
        key(app, KeyCode::Char(ch));
    }
}

fn step(app: &Dashboard) -> String {
    app.state
        .wizard
        .as_ref()
        .map_or_else(|| "<closed>".into(), |w| w.step_id().to_string())
}

fn session_error(app: &Dashboard) -> String {
    app.state
        .wizard
        .as_ref()
        .and_then(|w| w.session_error.clone())
        .unwrap_or_default()
}

async fn engine_at(dir: &Path) -> Arc<FullEngine> {
    Arc::new(
        build_full_with(
            dir,
            EngineSeams {
                global_config_path: Some(dir.join("home/global.yml")),
                ..Default::default()
            },
        )
        .await
        .expect("engine"),
    )
}

struct Project {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    /// The dashboard's engine.
    engine: Arc<FullEngine>,
    /// Another engine over the same files: "another process".
    other: Arc<FullEngine>,
    zeta: String,
    alpha: String,
    mid: String,
}

async fn project() -> Project {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::new(dir.path());
    tokio::fs::create_dir_all(paths.root()).await.expect("root");
    let mut config = default_config();
    config.defaults.agent.adapter = "shell".into();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    ConfigStore::write(&FileConfigStore::new(paths), &config)
        .await
        .expect("config");
    let engine = engine_at(dir.path()).await;
    let mut ids = Vec::new();
    for name in ["zeta-builder", "alpha-ops", "mid-reviewer"] {
        let agent = engine
            .agent_service
            .create(CreateAgentInput {
                name: name.into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .expect("agent");
        ids.push(agent.id);
    }
    let other = engine_at(dir.path()).await;
    Project {
        root: dir.path().to_path_buf(),
        _dir: dir,
        engine,
        other,
        zeta: ids[0].clone(),
        alpha: ids[1].clone(),
        mid: ids[2].clone(),
    }
}

async fn backend(engine: &Arc<FullEngine>) -> EngineBackend {
    let backend = EngineBackend::launch_with(Arc::clone(engine), "r7", true)
        .await
        .with_model_discovery(common::offline_model_discovery());
    assert!(!backend.is_watch_active(), "observer: nothing dispatches");
    backend
}

fn dashboard(backend: EngineBackend) -> Dashboard {
    let mut app = App::new(backend, TestClock::new(1_000_000));
    app.state.width = 120;
    app.state.height = 40;
    app.batch_ms = 0;
    app.refresh_all(true);
    app
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Disabled,
    Removed,
}

/// Change `id` through the other engine, synchronously from the dashboard's
/// blocking thread.
fn change(handle: &tokio::runtime::Handle, other: &Arc<FullEngine>, id: &str, how: Change) {
    match how {
        Change::Disabled => {
            handle
                .block_on(other.agent_service.disable(id))
                .expect("disable");
        }
        Change::Removed => handle
            .block_on(other.agent_service.remove(id))
            .expect("remove"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_task_with_an_assignee_changed_elsewhere_writes_nothing_until_corrected() {
    for how in [Change::Disabled, Change::Removed] {
        let p = project().await;
        let backend = backend(&p.engine).await;
        let handle = tokio::runtime::Handle::current();
        let (other, alpha, mid) = (Arc::clone(&p.other), p.alpha.clone(), p.mid.clone());
        let after_refusal = tokio::task::spawn_blocking(move || {
            let mut app = dashboard(backend);
            app.open_task_wizard();
            text(&mut app, "Ship it");
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Enter);
            text(&mut app, "ops");
            key(&mut app, KeyCode::Char('2'));
            assert_eq!(step(&app), "description");
            text(&mut app, "keep this");
            change(&handle, &other, &alpha, how);
            key(&mut app, KeyCode::Tab);
            let refused = (step(&app), session_error(&app));
            let persisted = handle
                .block_on(other.task_service.list(&Default::default()))
                .expect("tasks")
                .len();
            // Correct it explicitly and submit again.
            text(&mut app, "mid");
            key(&mut app, KeyCode::Char('2'));
            assert_eq!(step(&app), "description");
            key(&mut app, KeyCode::Tab);
            assert_eq!(step(&app), "<closed>");
            (refused, persisted, mid)
        })
        .await
        .expect("dashboard thread");
        let ((refused_step, error), persisted_after_refusal, mid) = after_refusal;
        assert_eq!(refused_step, "assignee", "{how:?}");
        assert!(error.starts_with("Nothing was saved"), "{how:?}: {error}");
        assert_eq!(persisted_after_refusal, 0, "{how:?}: nothing written");
        let tasks = p
            .engine
            .task_service
            .list(&Default::default())
            .await
            .expect("tasks");
        assert_eq!(tasks.len(), 1, "{how:?}: one write after the correction");
        assert_eq!(tasks[0].assignee.as_deref(), Some(mid.as_str()));
        assert_eq!(tasks[0].description, "keep this");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_goal_with_a_removed_assignee_is_not_created() {
    let p = project().await;
    let backend = backend(&p.engine).await;
    let handle = tokio::runtime::Handle::current();
    let (other, alpha) = (Arc::clone(&p.other), p.alpha.clone());
    let (step_after, error) = tokio::task::spawn_blocking(move || {
        let mut app = dashboard(backend);
        app.open_goal_wizard();
        text(&mut app, "Launch");
        key(&mut app, KeyCode::Enter);
        text(&mut app, "ops");
        key(&mut app, KeyCode::Char('2'));
        change(&handle, &other, &alpha, Change::Removed);
        key(&mut app, KeyCode::Tab);
        (step(&app), session_error(&app))
    })
    .await
    .expect("dashboard thread");
    assert_eq!(step_after, "assignee");
    assert!(error.contains("alpha-ops no longer exists"), "{error}");
    let goals = p
        .engine
        .goal_service
        .list(&Default::default())
        .await
        .expect("goals");
    assert!(goals.is_empty(), "{goals:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hidden_team_member_disabled_elsewhere_blocks_the_team_until_unticked() {
    let p = project().await;
    let backend = backend(&p.engine).await;
    let handle = tokio::runtime::Handle::current();
    let (other, alpha) = (Arc::clone(&p.other), p.alpha.clone());
    let (refused_step, teams_after_refusal) = tokio::task::spawn_blocking(move || {
        let mut app = dashboard(backend);
        app.open_team_wizard();
        text(&mut app, "Core");
        key(&mut app, KeyCode::Enter);
        text(&mut app, "zeta");
        key(&mut app, KeyCode::Char('1'));
        text(&mut app, "ops");
        key(&mut app, KeyCode::Char(' '));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        text(&mut app, "mid");
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Enter);
        assert_eq!(step(&app), "description");
        change(&handle, &other, &alpha, Change::Disabled);
        key(&mut app, KeyCode::Tab);
        let refused = step(&app);
        let teams = handle
            .block_on(other.team_service.list())
            .expect("teams")
            .len();
        // The refused member is listed first; untick it and finish.
        key(&mut app, KeyCode::Home);
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Tab);
        assert_eq!(step(&app), "<closed>");
        (refused, teams)
    })
    .await
    .expect("dashboard thread");
    assert_eq!(refused_step, "members");
    assert_eq!(teams_after_refusal, 0);
    let teams = p.engine.team_service.list().await.expect("teams");
    assert_eq!(teams.len(), 1);
    let members: Vec<&str> = teams[0]
        .members
        .iter()
        .map(|m| m.agent_id.as_str())
        .collect();
    assert_eq!(
        members,
        [p.zeta.as_str(), p.mid.as_str()],
        "lead, then the kept member"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_a_task_whose_assignee_changed_elsewhere_saves_and_leaves_the_assignee() {
    // C1: the assignee the wizard opened with is not submitted again, so a
    // title fix neither re-sends a disabled agent nor fails on a removed one.
    for how in [Change::Disabled, Change::Removed] {
        let p = project().await;
        let backend = backend(&p.engine).await;
        let alpha = p.alpha.clone();
        let task = p
            .engine
            .task_service
            .create(orch_domain::CreateTaskInput {
                title: "Original".into(),
                ..Default::default()
            })
            .await
            .expect("task");
        p.engine
            .task_service
            .assign(&task.id, &alpha)
            .await
            .expect("assign");
        let handle = tokio::runtime::Handle::current();
        let (other, task_id) = (Arc::clone(&p.other), task.id.clone());
        let (closed, failures) = tokio::task::spawn_blocking(move || {
            let mut app = dashboard(backend);
            let task = app
                .state
                .tasks
                .iter()
                .find(|t| t.id == task_id)
                .expect("listed")
                .clone();
            app.open_wizard(configs::edit_task_wizard(&task, &app.state.agents));
            change(&handle, &other, &alpha, how);
            key(&mut app, KeyCode::End);
            text(&mut app, " v2");
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Enter);
            key(&mut app, KeyCode::Tab);
            let failures: Vec<String> = app
                .state
                .messages
                .iter()
                .filter(|m| m.text.contains("failed"))
                .map(|m| m.text.clone())
                .collect();
            (step(&app), failures)
        })
        .await
        .expect("dashboard thread");
        assert_eq!(closed, "<closed>", "{how:?}");
        assert_eq!(failures, Vec::<String>::new(), "{how:?}");
        let saved = p.engine.task_service.get(&task.id).await.expect("task");
        assert_eq!(saved.title, "Original v2", "{how:?}");
        assert_eq!(saved.assignee.as_deref(), Some(p.alpha.as_str()), "{how:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_remote_registry_fails_closed_only_when_an_agent_is_chosen() {
    let p = project().await;
    let backend = backend(&p.engine).await;
    let registry = p.root.join(".orchestry").join("remotes.yml");
    let (assigned, unassigned) = tokio::task::spawn_blocking(move || {
        let mut app = dashboard(backend);
        let submit = |app: &mut Dashboard, digit: char| {
            app.open_task_wizard();
            text(app, "Ship it");
            key(app, KeyCode::Enter);
            key(app, KeyCode::Enter);
            key(app, KeyCode::Char(digit));
            assert_eq!(step(app), "description");
            key(app, KeyCode::Tab);
            (step(app), session_error(app))
        };
        std::fs::write(&registry, "{ not: [yaml").expect("corrupt the registry");
        // No agent chosen: the roster is not needed, so it is not read.
        let unassigned = submit(&mut app, '1');
        let assigned = submit(&mut app, '2');
        (assigned, unassigned)
    })
    .await
    .expect("dashboard thread");
    assert_eq!(assigned.0, "description", "{assigned:?}");
    assert!(
        assigned.1.contains("could not read the agent list"),
        "{assigned:?}"
    );
    assert_eq!(unassigned.0, "<closed>", "{unassigned:?}");
    let tasks = p
        .engine
        .task_service
        .list(&Default::default())
        .await
        .expect("tasks");
    assert_eq!(tasks.len(), 1, "only the unassigned task was written");
    assert_eq!(tasks[0].assignee, None);
}
