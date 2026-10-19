//! R7 / D7 against the real engine: the wizard agent pickers show a snapshot of
//! the roster taken when the wizard opened, so the final submission must
//! re-check the agents it is about to write, before its first write.
//!
//! The dashboard here is the real `App` over a real `EngineBackend` launched
//! with `observe = true`, over a real file-backed project in a temp directory
//! with shell agents, offline model discovery and a throwaway global config.
//! The scheduler is never started (asserted). The unit-level contract, with a
//! recording fake, is in `crates/orch-tui/tests/qol2_picker_eligibility.rs`.
//!
//! Every assertion reads the store through the engine services, not the
//! wizard's own idea of what it wrote: a refused submission must leave the
//! tasks, goals, teams and agents exactly as they were.

mod common;

use std::sync::Arc;

use orch_cli::tui::EngineBackend;
use orch_core::storage::ConfigStore;
use orch_domain::{
    Agent, AgentStatus, CreateAgentInput, CreateGoalInput, CreateTaskInput, CreateTeamInput, Goal,
    GoalStatus, NotificationPreferences, OrchestratorEvent, OrchestratorState, Task, Team,
    TuiPaletteName, WorkspaceMode, default_config,
};
use orch_engine::{EngineSeams, FullEngine, build_full_with};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_tui::App;
use orch_tui::backend::{
    AgentFields, AgentOpts, BackendResult, CreateTaskOpts, GoalFields, LaunchContext,
    MessageTarget, TaskFields, TuiBackend,
};
use orch_tui::clock::TestClock;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::runtime::Handle;

const DESC: &str = "retained draft text";

// ── A project with shell agents ──────────────────────────────────────────────

struct Env {
    temp: tempfile::TempDir,
    engine: Arc<FullEngine>,
    zeta: Agent,
    alpha: Agent,
    mid: Agent,
}

/// A temp project with three shell agents. `existing` also creates one task
/// and one goal for the edit wizards to open on.
async fn env(existing: bool) -> Env {
    let temp = tempfile::tempdir().expect("temp project");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.root()).await.expect("root");
    let mut config = default_config();
    config.defaults.agent.adapter = "shell".into();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    // No watch loop is ever started here; this only documents the intent.
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths), &config)
        .await
        .expect("config");
    let engine = Arc::new(
        build_full_with(
            temp.path(),
            EngineSeams {
                global_config_path: Some(temp.path().join("home/.orchestry/global.yml")),
                ..Default::default()
            },
        )
        .await
        .expect("engine"),
    );
    let mut made = Vec::new();
    for name in ["zeta-builder", "alpha-ops", "mid-reviewer"] {
        made.push(shell_agent(&engine, name).await);
    }
    if existing {
        engine
            .task_service
            .create(CreateTaskInput {
                title: "Existing task".into(),
                ..Default::default()
            })
            .await
            .expect("existing task");
        engine
            .goal_service
            .create(CreateGoalInput {
                title: "Existing goal".into(),
                ..Default::default()
            })
            .await
            .expect("existing goal");
    }
    let mid = made.pop().unwrap();
    let alpha = made.pop().unwrap();
    let zeta = made.pop().unwrap();
    Env {
        temp,
        engine,
        zeta,
        alpha,
        mid,
    }
}

async fn shell_agent(engine: &FullEngine, name: &str) -> Agent {
    engine
        .agent_service
        .create(CreateAgentInput {
            name: name.into(),
            adapter: "shell".into(),
            command: Some("true".into()),
            ..Default::default()
        })
        .await
        .expect("shell agent")
}

async fn backend(env: &Env) -> EngineBackend {
    let backend = EngineBackend::launch_with(Arc::clone(&env.engine), "qol2", true)
        .await
        .with_model_discovery(common::offline_model_discovery());
    assert!(!backend.is_watch_active(), "no scheduler may start");
    backend
}

// ── Driving the real App ────────────────────────────────────────────────────

/// The dashboard under test plus what it takes to change the roster behind its
/// back and read the store.
struct Ui<B: TuiBackend> {
    app: App<B, TestClock>,
    engine: Arc<FullEngine>,
    handle: Handle,
}

impl<B: TuiBackend> Ui<B> {
    fn new(backend: B, env: &Env) -> Self {
        let mut app = App::new(backend, TestClock::new(1_000_000));
        app.batch_ms = 0;
        app.refresh_all(true);
        Ui {
            app,
            engine: Arc::clone(&env.engine),
            handle: Handle::current(),
        }
    }

    fn press(&mut self, code: KeyCode) {
        self.app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ctrl(&mut self, c: char) {
        self.app
            .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn type_str(&mut self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c));
        }
    }

    fn step(&self) -> String {
        self.app
            .state
            .wizard
            .as_ref()
            .map_or_else(|| "<closed>".to_string(), |w| w.step_id().to_string())
    }

    fn open(&self) -> bool {
        self.app.state.wizard.is_some()
    }

    fn advance_to(&mut self, target: &str) {
        for _ in 0..12 {
            if self.step() == target {
                return;
            }
            assert!(self.open(), "wizard closed before reaching {target}");
            assert_ne!(self.step(), "description", "passed {target}");
            self.press(KeyCode::Enter);
        }
        assert_eq!(self.step(), target);
    }

    fn advance_to_description_if_needed(&mut self) {
        for _ in 0..8 {
            if self.step() == "description" || !self.open() {
                return;
            }
            self.press(KeyCode::Enter);
        }
    }

    fn go_back_to(&mut self, target: &str) {
        for _ in 0..12 {
            if self.step() == target {
                return;
            }
            assert!(self.open(), "wizard closed while going back to {target}");
            self.press(KeyCode::Esc);
        }
        assert_eq!(self.step(), target);
    }

    /// Type a prefix of the agent's name (the filter), then press the digit of
    /// the agent's row. `None` when the wizard does not offer the agent.
    fn pick(&mut self, agent: &Agent) -> Option<()> {
        let prefix: String = agent.name.chars().take(3).collect();
        self.type_str(&prefix);
        let row = self
            .app
            .state
            .wizard
            .as_ref()?
            .options()
            .iter()
            .position(|c| c.value == agent.id)?;
        assert!(row < 9);
        self.press(KeyCode::Char(char::from(b'1' + row as u8)));
        Some(())
    }

    fn pick_sentinel(&mut self) {
        let first = self.app.state.wizard.as_ref().unwrap().options()[0].clone();
        assert_eq!(first.value, "", "the first row is the sentinel");
        self.press(KeyCode::Char('1'));
    }

    /// Tick `agent` on the members step, then type a filter that hides it.
    fn tick_then_hide(&mut self, agent: &Agent) -> Option<()> {
        assert_eq!(self.step(), "members");
        let prefix: String = agent.name.chars().take(3).collect();
        self.type_str(&prefix);
        let listed = self
            .app
            .state
            .wizard
            .as_ref()?
            .options()
            .iter()
            .any(|c| c.value == agent.id);
        if !listed {
            return None;
        }
        self.press(KeyCode::Char(' '));
        self.type_str("zzzz");
        assert!(self.app.state.wizard.as_ref()?.options().is_empty());
        Some(())
    }

    fn draft_description(&mut self) {
        self.advance_to("description");
        self.type_str(DESC);
    }

    fn submit(&mut self) {
        self.press(KeyCode::Tab);
    }

    /// Submit and require that it was refused: the wizard is still open and
    /// the store is exactly as it was.
    fn submit_refused(&mut self) {
        let before = self.snapshot();
        self.submit();
        assert!(
            self.open(),
            "a refused submission must keep the wizard open"
        );
        assert!(
            self.snapshot() == before,
            "a refused submission must not write anything"
        );
    }

    /// Whether the wizard's current step offers `agent` as a row.
    fn offers(&self, agent: &Agent) -> bool {
        self.app
            .state
            .wizard
            .as_ref()
            .expect("wizard open")
            .options()
            .iter()
            .any(|c| c.value == agent.id)
    }

    fn hammer_confirm_keys(&mut self) {
        for _ in 0..4 {
            self.press(KeyCode::Enter);
            self.press(KeyCode::Tab);
        }
    }

    fn draft_text(&self, id: &str) -> String {
        let wizard = self.app.state.wizard.as_ref().expect("wizard open");
        if wizard.step_id() == id {
            return if id == "description" {
                wizard.textarea_value()
            } else {
                wizard.text_value().to_string()
            };
        }
        wizard.values().get(id).cloned().unwrap_or_default()
    }

    fn error_visible(&self) -> bool {
        self.app
            .state
            .wizard
            .as_ref()
            .is_some_and(|w| w.session_error.is_some())
            || self
                .app
                .state
                .messages
                .iter()
                .any(|m| m.text.starts_with("Failed"))
    }

    // ── the roster, behind the wizard's back ─────────────────────────────

    fn change(&mut self, agent: &Agent, what: Change) {
        match what {
            Change::Disabled => {
                self.handle
                    .block_on(self.engine.agent_service.disable(&agent.id))
                    .expect("disable");
            }
            Change::Removed => {
                self.handle
                    .block_on(self.engine.agent_service.remove(&agent.id))
                    .expect("remove");
            }
        }
        // The app's own cache is current; the wizard's snapshot is not.
        self.app.refresh_all(true);
    }

    // ── the store ────────────────────────────────────────────────────────

    fn tasks(&self) -> Vec<Task> {
        self.handle
            .block_on(self.engine.task_service.list(&Default::default()))
            .expect("tasks")
    }

    fn goals(&self) -> Vec<Goal> {
        self.handle
            .block_on(self.engine.goal_service.list(&Default::default()))
            .expect("goals")
    }

    fn teams(&self) -> Vec<Team> {
        self.handle
            .block_on(self.engine.team_service.list())
            .expect("teams")
    }

    fn agents(&self) -> Vec<Agent> {
        self.handle
            .block_on(self.engine.agent_service.list())
            .expect("agents")
    }

    /// Everything a refused submission must leave alone.
    fn snapshot(&self) -> String {
        format!(
            "{:#?}\n{:#?}\n{:#?}\n{:#?}",
            self.tasks(),
            self.goals(),
            self.teams(),
            self.agents()
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    NewTask,
    EditTask,
    NewGoal,
    EditGoal,
    TeamLead,
    TeamMember,
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Disabled,
    Removed,
}

#[derive(Clone, Copy, Debug)]
enum When {
    AfterOpen,
    AfterSelection,
}

fn start<B: TuiBackend>(ui: &mut Ui<B>, kind: Kind, env: &Env) {
    match kind {
        Kind::NewTask => {
            ui.app.open_task_wizard();
            ui.type_str("Stale picker task");
            ui.advance_to("assignee");
        }
        Kind::EditTask => {
            ui.app.open_edit_task_wizard();
            ui.advance_to("assignee");
        }
        Kind::NewGoal => {
            ui.app.open_goal_wizard();
            ui.type_str("Stale picker goal");
            ui.advance_to("assignee");
        }
        Kind::EditGoal => {
            ui.app.open_edit_goal_wizard();
            ui.advance_to("assignee");
        }
        Kind::TeamLead => {
            ui.app.open_team_wizard();
            ui.type_str("Platform");
            ui.advance_to("lead");
        }
        Kind::TeamMember => {
            ui.app.open_team_wizard();
            ui.type_str("Platform");
            ui.advance_to("lead");
            ui.pick(&env.zeta).expect("zeta is offered as the lead");
            assert_eq!(ui.step(), "members");
        }
    }
    assert!(ui.open());
}

fn choose<B: TuiBackend>(ui: &mut Ui<B>, kind: Kind, agent: &Agent) -> Option<()> {
    match kind {
        Kind::TeamMember => ui.tick_then_hide(agent),
        _ => ui.pick(agent),
    }
}

/// What a scenario ended with: whether the wizard is still open, and whether
/// the store is exactly as it was just before the submission.
struct Outcome {
    open: bool,
    store_unchanged: bool,
}

/// Open, optionally change the roster before choosing, choose `target`, draft
/// the description, optionally change the roster after choosing, and submit.
/// `None` when the wizard did not offer `target` at all.
fn scenario<B: TuiBackend>(
    ui: &mut Ui<B>,
    env: &Env,
    kind: Kind,
    target: &Agent,
    what: Change,
    when: When,
) -> Option<Outcome> {
    start(ui, kind, env);
    if matches!(when, When::AfterOpen) {
        ui.change(target, what);
    }
    choose(ui, kind, target)?;
    ui.draft_description();
    if matches!(when, When::AfterSelection) {
        ui.change(target, what);
    }
    let before = ui.snapshot();
    ui.submit();
    Some(Outcome {
        open: ui.open(),
        store_unchanged: ui.snapshot() == before,
    })
}

async fn run_matrix(kind: Kind, what: Change, when: When) {
    let env = env(matches!(kind, Kind::EditTask | Kind::EditGoal)).await;
    let backend = backend(&env).await;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut ui = Ui::new(backend, &env);
        let target = env.alpha.clone();
        let outcome = scenario(&mut ui, &env, kind, &target, what, when);
        assert!(!env.engine.orchestrator.is_owner(), "scheduler stayed off");
        let _keep = &env.temp;
        outcome
    })
    .await
    .expect("ui thread");
    if let Some(outcome) = outcome {
        assert!(
            outcome.open,
            "{kind:?}/{what:?}/{when:?}: the wizard must stay open on a refused choice"
        );
        assert!(
            outcome.store_unchanged,
            "{kind:?}/{what:?}/{when:?}: the store must be untouched by a refused choice"
        );
    }
}

macro_rules! refused_before_any_write {
    ($($name:ident: $kind:expr, $what:expr, $when:expr;)*) => {$(
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            run_matrix($kind, $what, $when).await;
        }
    )*};
}

refused_before_any_write! {
    new_task_assignee_disabled_after_open_leaves_the_store_untouched: Kind::NewTask, Change::Disabled, When::AfterOpen;
    new_task_assignee_removed_after_open_leaves_the_store_untouched: Kind::NewTask, Change::Removed, When::AfterOpen;
    new_task_assignee_disabled_after_selection_leaves_the_store_untouched: Kind::NewTask, Change::Disabled, When::AfterSelection;
    new_task_assignee_removed_after_selection_leaves_the_store_untouched: Kind::NewTask, Change::Removed, When::AfterSelection;
    edit_task_assignee_disabled_after_open_leaves_the_store_untouched: Kind::EditTask, Change::Disabled, When::AfterOpen;
    edit_task_assignee_removed_after_open_leaves_the_store_untouched: Kind::EditTask, Change::Removed, When::AfterOpen;
    edit_task_assignee_disabled_after_selection_leaves_the_store_untouched: Kind::EditTask, Change::Disabled, When::AfterSelection;
    edit_task_assignee_removed_after_selection_leaves_the_store_untouched: Kind::EditTask, Change::Removed, When::AfterSelection;
    new_goal_assignee_disabled_after_open_leaves_the_store_untouched: Kind::NewGoal, Change::Disabled, When::AfterOpen;
    new_goal_assignee_removed_after_open_leaves_the_store_untouched: Kind::NewGoal, Change::Removed, When::AfterOpen;
    new_goal_assignee_disabled_after_selection_leaves_the_store_untouched: Kind::NewGoal, Change::Disabled, When::AfterSelection;
    new_goal_assignee_removed_after_selection_leaves_the_store_untouched: Kind::NewGoal, Change::Removed, When::AfterSelection;
    edit_goal_assignee_disabled_after_open_leaves_the_store_untouched: Kind::EditGoal, Change::Disabled, When::AfterOpen;
    edit_goal_assignee_removed_after_open_leaves_the_store_untouched: Kind::EditGoal, Change::Removed, When::AfterOpen;
    edit_goal_assignee_disabled_after_selection_leaves_the_store_untouched: Kind::EditGoal, Change::Disabled, When::AfterSelection;
    edit_goal_assignee_removed_after_selection_leaves_the_store_untouched: Kind::EditGoal, Change::Removed, When::AfterSelection;
    team_lead_disabled_after_open_leaves_the_store_untouched: Kind::TeamLead, Change::Disabled, When::AfterOpen;
    team_lead_disabled_after_selection_leaves_the_store_untouched: Kind::TeamLead, Change::Disabled, When::AfterSelection;
    hidden_team_member_disabled_after_open_leaves_the_store_untouched: Kind::TeamMember, Change::Disabled, When::AfterOpen;
    hidden_team_member_disabled_after_selection_leaves_the_store_untouched: Kind::TeamMember, Change::Disabled, When::AfterSelection;
}

// Guards, not regressions: `create_team` already rejects a removed lead or
// member ("Lead agent not found"), so these four pass on 98130a4 too. They stay
// so that a change to the preflight cannot reopen that protection.
refused_before_any_write! {
    team_lead_removed_after_open_is_still_refused_by_the_engine: Kind::TeamLead, Change::Removed, When::AfterOpen;
    team_lead_removed_after_selection_is_still_refused_by_the_engine: Kind::TeamLead, Change::Removed, When::AfterSelection;
    hidden_team_member_removed_after_open_is_still_refused_by_the_engine: Kind::TeamMember, Change::Removed, When::AfterOpen;
    hidden_team_member_removed_after_selection_is_still_refused_by_the_engine: Kind::TeamMember, Change::Removed, When::AfterSelection;
}

// ── Draft recovery and correction, against the persisted store ───────────────

/// Run `body` on the dashboard thread over a fresh project.
async fn with_ui<T, F>(existing: bool, body: F) -> T
where
    T: Send + 'static,
    F: FnOnce(Ui<EngineBackend>, Env) -> T + Send + 'static,
{
    let env = env(existing).await;
    let backend = backend(&env).await;
    tokio::task::spawn_blocking(move || {
        let ui = Ui::new(backend, &env);
        body(ui, env)
    })
    .await
    .expect("ui thread")
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_task_is_explained_and_keeps_the_draft_text_and_attachment() {
    let kept = with_ui(false, |mut ui, env| {
        let file = env.temp.path().join("shot.png");
        std::fs::write(&file, b"png").expect("attachment file");
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.app
            .state
            .pending_attachments
            .push(file.to_string_lossy().into_owned());
        ui.change(&env.alpha, Change::Removed);
        ui.submit();
        (
            ui.open(),
            ui.error_visible(),
            ui.draft_text("title"),
            ui.draft_text("description"),
            ui.app.state.pending_attachments.len(),
            ui.tasks().len(),
        )
    })
    .await;
    assert_eq!(
        kept,
        (
            true,
            true,
            "Stale picker task".to_string(),
            DESC.to_string(),
            1,
            0
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_task_corrected_with_a_replacement_commits_one_task_with_the_draft() {
    let (tasks, open) = with_ui(false, |mut ui, env| {
        let file = env.temp.path().join("shot.png");
        std::fs::write(&file, b"png").expect("attachment file");
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.app
            .state
            .pending_attachments
            .push(file.to_string_lossy().into_owned());
        ui.change(&env.alpha, Change::Disabled);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick(&env.mid)
            .expect("mid is offered for the correction");
        ui.advance_to_description_if_needed();
        ui.submit();
        let open = ui.open();
        ui.hammer_confirm_keys();
        (
            ui.tasks()
                .into_iter()
                .map(|t| {
                    (
                        t.title,
                        t.description,
                        t.assignee,
                        t.attachments.map_or(0, |a| a.len()),
                    )
                })
                .collect::<Vec<_>>(),
            open,
        )
    })
    .await;
    assert!(!open, "the corrected submission closes the wizard");
    assert_eq!(tasks.len(), 1, "exactly one task: {tasks:?}");
    assert_eq!(tasks[0].0, "Stale picker task");
    assert_eq!(tasks[0].1, DESC);
    assert_eq!(tasks[0].3, 1, "the attachment survives the refusal");
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_task_corrected_with_a_replacement_assigns_the_replacement() {
    let (assignee, mid) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Removed);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick(&env.mid).unwrap();
        ui.advance_to_description_if_needed();
        ui.submit();
        (
            ui.tasks().first().and_then(|t| t.assignee.clone()),
            env.mid.id.clone(),
        )
    })
    .await;
    assert_eq!(assignee, Some(mid));
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_task_corrected_with_the_sentinel_commits_one_unassigned_task() {
    let tasks = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Removed);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick_sentinel();
        ui.advance_to_description_if_needed();
        ui.submit();
        ui.hammer_confirm_keys();
        ui.tasks()
            .into_iter()
            .map(|t| (t.title, t.description, t.assignee))
            .collect::<Vec<_>>()
    })
    .await;
    assert_eq!(
        tasks,
        vec![("Stale picker task".to_string(), DESC.to_string(), None)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_goal_corrected_with_a_replacement_persists_one_goal_for_the_replacement() {
    let (goals, mid) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewGoal, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Disabled);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick(&env.mid).unwrap();
        ui.advance_to_description_if_needed();
        ui.submit();
        ui.hammer_confirm_keys();
        (
            ui.goals()
                .into_iter()
                .map(|g| (g.title, g.description, g.assignee))
                .collect::<Vec<_>>(),
            env.mid.id.clone(),
        )
    })
    .await;
    assert_eq!(
        goals,
        vec![("Stale picker goal".to_string(), DESC.to_string(), Some(mid))]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_goal_corrected_with_the_sentinel_never_persists_the_removed_agent() {
    let (goals, alpha) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewGoal, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Removed);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick_sentinel();
        ui.advance_to_description_if_needed();
        ui.submit();
        (ui.goals(), env.alpha.id.clone())
    })
    .await;
    assert_eq!(goals.len(), 1, "exactly one goal");
    assert_ne!(goals[0].assignee.as_deref(), Some(alpha.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_edit_task_corrected_with_a_replacement_updates_the_task_once() {
    let (tasks, zeta) = with_ui(true, |mut ui, env| {
        start(&mut ui, Kind::EditTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Removed);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick(&env.zeta).unwrap();
        ui.advance_to_description_if_needed();
        ui.submit();
        ui.hammer_confirm_keys();
        (ui.tasks(), env.zeta.id.clone())
    })
    .await;
    assert_eq!(tasks.len(), 1, "the edit never duplicates the task");
    assert_eq!(
        (
            tasks[0].assignee.as_deref(),
            tasks[0].description.contains(DESC)
        ),
        (Some(zeta.as_str()), true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_edit_goal_corrected_with_a_replacement_updates_the_goal_once() {
    let (goals, zeta) = with_ui(true, |mut ui, env| {
        start(&mut ui, Kind::EditGoal, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Disabled);
        ui.submit_refused();
        ui.go_back_to("assignee");
        ui.pick(&env.zeta).unwrap();
        ui.advance_to_description_if_needed();
        ui.submit();
        (ui.goals(), env.zeta.id.clone())
    })
    .await;
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].assignee.as_deref(), Some(zeta.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_team_lead_corrected_with_a_replacement_persists_one_team_with_that_lead() {
    let (teams, mid) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::TeamLead, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Removed);
        ui.submit_refused();
        ui.go_back_to("lead");
        ui.pick(&env.mid).unwrap();
        ui.advance_to_description_if_needed();
        ui.submit();
        ui.hammer_confirm_keys();
        (ui.teams(), env.mid.id.clone())
    })
    .await;
    assert_eq!(teams.len(), 1, "exactly one team");
    assert_eq!(teams[0].lead_agent_id, mid);
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_hidden_member_leaves_no_team_until_it_is_unticked() {
    let (before, after, members) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::TeamMember, &env);
        ui.tick_then_hide(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Disabled);
        ui.submit_refused();
        let before = ui.teams().len();
        ui.hammer_confirm_keys();
        let after = ui.teams().len();
        ui.go_back_to("members");
        ui.ctrl('u');
        let row = ui
            .app
            .state
            .wizard
            .as_ref()
            .unwrap()
            .options()
            .iter()
            .position(|c| c.value == env.alpha.id)
            .expect("the refused member stays listed so that it can be unticked");
        ui.press(KeyCode::Home);
        for _ in 0..row {
            ui.press(KeyCode::Down);
        }
        ui.press(KeyCode::Char(' '));
        ui.advance_to_description_if_needed();
        ui.submit();
        let members = ui
            .teams()
            .first()
            .map(|t| t.members.len())
            .unwrap_or_default();
        (before, after, members)
    })
    .await;
    assert_eq!((before, after), (0, 0), "no team while a member is refused");
    assert_eq!(members, 1, "once unticked, the lead alone forms the team");
}

// ── A refused field reopens on fresh choices ─────────────────────────────────

macro_rules! reopens_on_fresh_choices {
    ($($name:ident: $kind:expr, $what:expr, $field:expr;)*) => {$(
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let offered = with_ui(false, |mut ui, env| {
                start(&mut ui, $kind, &env);
                choose(&mut ui, $kind, &env.alpha).unwrap();
                ui.draft_description();
                ui.change(&env.alpha, $what);
                ui.submit_refused();
                ui.go_back_to($field);
                ui.ctrl('u');
                ui.offers(&env.alpha)
            })
            .await;
            assert!(!offered, "the agent that was refused must not be offered again");
        }
    )*};
}

reopens_on_fresh_choices! {
    refused_disabled_task_assignee_is_not_offered_again: Kind::NewTask, Change::Disabled, "assignee";
    refused_removed_task_assignee_is_not_offered_again: Kind::NewTask, Change::Removed, "assignee";
    refused_disabled_goal_assignee_is_not_offered_again: Kind::NewGoal, Change::Disabled, "assignee";
    refused_removed_goal_assignee_is_not_offered_again: Kind::NewGoal, Change::Removed, "assignee";
    refused_disabled_team_lead_is_not_offered_again: Kind::TeamLead, Change::Disabled, "lead";
    refused_removed_team_lead_is_not_offered_again: Kind::TeamLead, Change::Removed, "lead";
}

// ── The roster cannot be read ────────────────────────────────────────────────

/// A registry the remote directory cannot parse: `refresh_agents` reads it
/// when it merges remote agents into the local roster, so it fails.
fn break_roster(env: &Env) {
    let path = Paths::new(env.temp.path()).remotes_path();
    std::fs::write(&path, ": : [ not a yaml registry\n\t- {").expect("corrupt the registry");
}

fn repair_roster(env: &Env) {
    std::fs::remove_file(Paths::new(env.temp.path()).remotes_path()).expect("remove registry");
}

#[tokio::test(flavor = "multi_thread")]
async fn unreadable_roster_leaves_the_store_untouched_and_keeps_the_task_draft() {
    let (store_unchanged, open, error, title, description) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        break_roster(&env);
        let before = ui.snapshot();
        ui.submit();
        (
            ui.snapshot() == before,
            ui.open(),
            ui.error_visible(),
            ui.draft_text("title"),
            ui.draft_text("description"),
        )
    })
    .await;
    assert_eq!(
        (store_unchanged, open, error),
        (true, true, true),
        "fail closed, visibly"
    );
    assert_eq!(
        (title.as_str(), description.as_str()),
        ("Stale picker task", DESC)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unreadable_roster_leaves_the_store_untouched_for_a_goal() {
    let (store_unchanged, open) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewGoal, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        break_roster(&env);
        let before = ui.snapshot();
        ui.submit();
        (ui.snapshot() == before, ui.open())
    })
    .await;
    assert_eq!((store_unchanged, open), (true, true));
}

#[tokio::test(flavor = "multi_thread")]
async fn unreadable_roster_leaves_the_store_untouched_for_a_team() {
    let (store_unchanged, open) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::TeamLead, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        break_roster(&env);
        let before = ui.snapshot();
        ui.submit();
        (ui.snapshot() == before, ui.open())
    })
    .await;
    assert_eq!((store_unchanged, open), (true, true));
}

#[tokio::test(flavor = "multi_thread")]
async fn unreadable_roster_leaves_the_store_untouched_for_an_edit_task() {
    let (store_unchanged, open) = with_ui(true, |mut ui, env| {
        start(&mut ui, Kind::EditTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        break_roster(&env);
        let before = ui.snapshot();
        ui.submit();
        (ui.snapshot() == before, ui.open())
    })
    .await;
    assert_eq!((store_unchanged, open), (true, true));
}

#[tokio::test(flavor = "multi_thread")]
async fn once_the_roster_is_readable_again_the_same_draft_commits_one_task() {
    let tasks = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        break_roster(&env);
        ui.submit_refused();
        repair_roster(&env);
        for _ in 0..3 {
            if !ui.open() {
                break;
            }
            ui.submit();
        }
        ui.hammer_confirm_keys();
        (ui.tasks(), env.alpha.id.clone())
    })
    .await;
    assert_eq!(tasks.0.len(), 1, "one task, no duplicate: {:?}", tasks.0);
    assert_eq!(tasks.0[0].assignee.as_deref(), Some(tasks.1.as_str()));
}

// ── Duplicate names, distinct ids ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn removed_agent_is_not_replaced_by_a_look_alike_sibling_in_the_store() {
    let (tasks, open, sibling) = with_ui(false, |mut ui, env| {
        // A second agent the three-letter filter cannot tell apart from alpha.
        let sibling = ui
            .handle
            .block_on(shell_agent(&env.engine, "alpha-ops-two"));
        ui.app.refresh_all(true);
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.change(&env.alpha, Change::Removed);
        ui.submit();
        ui.hammer_confirm_keys();
        (ui.tasks(), ui.open(), sibling.id)
    })
    .await;
    assert_eq!(
        (
            tasks.len(),
            open,
            tasks
                .iter()
                .any(|t| t.assignee.as_deref() == Some(sibling.as_str()))
        ),
        (0, true, false),
        "no task, wizard open, and the near-duplicate is not assigned"
    );
}

// ── Controls ─────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn enabled_agent_chosen_through_a_filter_is_persisted_as_the_assignee_once() {
    let (tasks, alpha, open) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.submit();
        let open = ui.open();
        ui.hammer_confirm_keys();
        (ui.tasks(), env.alpha.id.clone(), open)
    })
    .await;
    assert_eq!((tasks.len(), open), (1, false));
    assert_eq!(tasks[0].assignee.as_deref(), Some(alpha.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_agent_is_still_an_eligible_assignee() {
    let assignee = with_ui(false, |mut ui, env| {
        ui.handle
            .block_on(
                env.engine
                    .agent_service
                    .set_status(&env.alpha.id, AgentStatus::Running),
            )
            .expect("mark running");
        ui.app.refresh_all(true);
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.submit();
        (
            ui.tasks().first().and_then(|t| t.assignee.clone()),
            env.alpha.id.clone(),
        )
    })
    .await;
    assert_eq!(assignee.0, Some(assignee.1));
}

#[tokio::test(flavor = "multi_thread")]
async fn enabled_team_lead_and_member_persist_one_team_with_exactly_those_ids() {
    let (teams, zeta, mid) = with_ui(false, |mut ui, env| {
        start(&mut ui, Kind::TeamMember, &env);
        ui.tick_then_hide(&env.mid).unwrap();
        ui.draft_description();
        ui.submit();
        (ui.teams(), env.zeta.id.clone(), env.mid.id.clone())
    })
    .await;
    assert_eq!(teams.len(), 1);
    assert_eq!(teams[0].lead_agent_id, zeta);
    assert!(teams[0].members.iter().any(|m| m.agent_id == mid));
}

// ── A failure after the primary write keeps the committed-id, no-retry policy ─

/// Forwards to the real backend, but removes an agent from the engine right
/// after a task is created: the validating read has already passed, so the
/// primary write lands and the assignment that follows then fails for real.
struct RemoveAfterCreate {
    inner: EngineBackend,
    engine: Arc<FullEngine>,
    handle: Handle,
    remove: Option<String>,
}

macro_rules! forward {
    ($(fn $name:ident(&mut self $(, $arg:ident : $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) -> $ret {
            self.inner.$name($($arg),*)
        })*
    };
}

impl TuiBackend for RemoveAfterCreate {
    fn context(&self) -> LaunchContext {
        self.inner.context()
    }
    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task> {
        let task = self.inner.create_task(title, opts)?;
        if let Some(id) = self.remove.take() {
            self.handle
                .block_on(self.engine.agent_service.remove(&id))
                .expect("remove after create");
        }
        Ok(task)
    }
    forward! {
        fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>>;
        fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>>;
        fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>>;
        fn refresh_state(&mut self) -> BackendResult<OrchestratorState>;
        fn list_teams(&mut self) -> BackendResult<Vec<Team>>;
        fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task>;
        fn delete_task(&mut self, id: &str) -> BackendResult<()>;
        fn run_task(&mut self, id: &str) -> BackendResult<()>;
        fn run_all(&mut self) -> BackendResult<()>;
        fn cancel_task(&mut self, id: &str) -> BackendResult<()>;
        fn retry_task(&mut self, id: &str) -> BackendResult<()>;
        fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()>;
        fn approve_task(&mut self, id: &str) -> BackendResult<()>;
        fn reject_task(&mut self, id: &str, feedback: Option<&str>) -> BackendResult<()>;
        fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent>;
        fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent>;
        fn delete_agent(&mut self, id: &str) -> BackendResult<()>;
        fn force_stop_agent(&mut self, id: &str) -> BackendResult<()>;
        fn toggle_autonomous(&mut self, id: &str, enabled: bool) -> BackendResult<Agent>;
        fn disable_agent(&mut self, id: &str) -> BackendResult<()>;
        fn enable_agent(&mut self, id: &str) -> BackendResult<()>;
        fn create_team(&mut self, input: CreateTeamInput) -> BackendResult<Team>;
        fn join_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team>;
        fn leave_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team>;
        fn disband_team(&mut self, team_id: &str) -> BackendResult<()>;
        fn set_team_lead(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team>;
        fn create_goal(&mut self, input: orch_domain::CreateGoalInput) -> BackendResult<Goal>;
        fn update_goal(&mut self, id: &str, fields: GoalFields) -> BackendResult<Goal>;
        fn update_goal_status(&mut self, id: &str, status: GoalStatus, force: bool) -> BackendResult<Goal>;
        fn delete_goal(&mut self, id: &str) -> BackendResult<()>;
        fn goal_progress(&mut self, id: &str) -> BackendResult<String>;
        fn send_message(&mut self, target: MessageTarget, body: &str) -> BackendResult<Vec<String>>;
        fn start_watch(&mut self) -> BackendResult<()>;
        fn stop_watch(&mut self) -> BackendResult<()>;
        fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()>;
        fn save_activity_filter(&mut self, preset: &str) -> BackendResult<()>;
        fn save_notifications(&mut self, notifications: NotificationPreferences) -> BackendResult<()>;
        fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()>;
        fn complete_onboarding(&mut self) -> BackendResult<()>;
        fn poll_events(&mut self) -> Vec<OrchestratorEvent>;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_lost_after_the_validating_read_commits_one_task_and_reports_its_id() {
    let env = env(false).await;
    let inner = backend(&env).await;
    let wrapper = RemoveAfterCreate {
        inner,
        engine: Arc::clone(&env.engine),
        handle: Handle::current(),
        remove: Some(env.alpha.id.clone()),
    };
    let (tasks, open, reported, again) = tokio::task::spawn_blocking(move || {
        let mut ui = Ui::new(wrapper, &env);
        start(&mut ui, Kind::NewTask, &env);
        ui.pick(&env.alpha).unwrap();
        ui.draft_description();
        ui.submit();
        let open = ui.open();
        let tasks = ui.tasks();
        let reported = tasks.first().is_some_and(|t| {
            ui.app
                .state
                .messages
                .iter()
                .any(|m| m.text.contains(&t.id) && m.text.contains("assignment failed"))
        });
        ui.hammer_confirm_keys();
        let again = ui.tasks().len();
        (tasks, open, reported, again)
    })
    .await
    .expect("ui thread");
    assert_eq!(
        (
            tasks.len(),
            tasks[0].assignee.clone(),
            open,
            reported,
            again
        ),
        (1, None, false, true, 1),
        "the primary write landed once, unassigned, with its id reported and no retry create"
    );
}
