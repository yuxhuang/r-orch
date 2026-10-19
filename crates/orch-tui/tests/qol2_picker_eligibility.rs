//! R7 / D7: the wizard agent pickers show a snapshot of the roster taken when
//! the wizard opened, so the final submission has to re-check the agents it is
//! about to write, before its first write.
//!
//! These tests drive the real key routing of `App` against a recording fake
//! backend. They assert only what a user or the store can observe: whether the
//! wizard is still open, what the backend was asked to write, and what the
//! draft still holds. They name no new production API, so each one compiles
//! against, and can fail on, the pre-fix code.
//!
//! Layout: the refusal matrix (task, goal and team; new and edit; disabled and
//! removed; after open and after selection), draft recovery, correction by
//! replacement or sentinel, a roster that cannot be read, duplicate names, and
//! the unchanged post-write failure policy. The same contract against the real
//! engine is in `crates/orch-cli/tests/qol2_picker_eligibility.rs`.

use orch_domain::{
    Agent, AgentStatus, CreateGoalInput, CreateTeamInput, Goal, GoalStatus,
    NotificationPreferences, OrchestratorEvent, OrchestratorState, Task, TaskStatus, Team,
    TuiPaletteName,
};
use orch_tui::App;
use orch_tui::backend::{
    AgentFields, AgentOpts, BackendResult, Call, ClipboardPaste, CreateTaskOpts, FakeBackend,
    GoalFields, LaunchContext, MessageTarget, TaskFields, TuiBackend, make_agent, make_goal,
    make_task,
};
use orch_tui::clock::TestClock;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ── A fake whose roster can fail and whose assignment can fail ───────────────

/// Forwards every call to a [`FakeBackend`], except where a test needs a seam:
/// the roster read can fail, an assignment can fail, and an agent can be
/// disabled between the primary write and the assignment (the race the
/// preflight read cannot close).
struct Probe {
    inner: FakeBackend,
    roster_error: Option<String>,
    /// `assign_task` refuses an agent that is disabled or no longer listed.
    assign_checks_roster: bool,
    /// Disable this agent as soon as a task has been created or updated.
    disable_after_write: Option<String>,
}

macro_rules! forward {
    ($(fn $name:ident(&mut self $(, $arg:ident : $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) -> $ret {
            self.inner.$name($($arg),*)
        })*
    };
}

impl Probe {
    fn disable_pending_agent(&mut self) {
        if let Some(id) = self.disable_after_write.take()
            && let Some(agent) = self.inner.agents.iter_mut().find(|a| a.id == id)
        {
            agent.status = AgentStatus::Disabled;
        }
    }
}

impl TuiBackend for Probe {
    fn context(&self) -> LaunchContext {
        self.inner.context()
    }
    fn can_create_task(&self) -> bool {
        self.inner.can_create_task()
    }
    fn can_add_agent(&self) -> bool {
        self.inner.can_add_agent()
    }
    fn can_create_goal(&self) -> bool {
        self.inner.can_create_goal()
    }
    fn can_force_stop_agent(&self) -> bool {
        self.inner.can_force_stop_agent()
    }
    fn can_paste_image(&self) -> bool {
        self.inner.can_paste_image()
    }
    fn can_send_message(&self) -> bool {
        self.inner.can_send_message()
    }
    fn paste_image(&mut self) -> BackendResult<ClipboardPaste> {
        self.inner.paste_image()
    }
    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
        match &self.roster_error {
            Some(error) => Err(error.clone()),
            None => self.inner.refresh_agents(),
        }
    }
    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task> {
        let task = self.inner.create_task(title, opts)?;
        self.disable_pending_agent();
        Ok(task)
    }
    fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task> {
        let task = self.inner.update_task(id, fields)?;
        self.disable_pending_agent();
        Ok(task)
    }
    fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()> {
        if self.assign_checks_roster
            && !self
                .inner
                .agents
                .iter()
                .any(|a| a.id == agent_id && a.status != AgentStatus::Disabled)
        {
            self.inner.calls.push(Call::AssignTask {
                task_id: task_id.into(),
                agent_id: agent_id.into(),
            });
            return Err(format!("Agent {agent_id} is not available"));
        }
        self.inner.assign_task(task_id, agent_id)
    }
    forward! {
        fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>>;
        fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>>;
        fn refresh_state(&mut self) -> BackendResult<OrchestratorState>;
        fn list_teams(&mut self) -> BackendResult<Vec<Team>>;
        fn delete_task(&mut self, id: &str) -> BackendResult<()>;
        fn run_task(&mut self, id: &str) -> BackendResult<()>;
        fn run_all(&mut self) -> BackendResult<()>;
        fn cancel_task(&mut self, id: &str) -> BackendResult<()>;
        fn retry_task(&mut self, id: &str) -> BackendResult<()>;
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
        fn create_goal(&mut self, input: CreateGoalInput) -> BackendResult<Goal>;
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

// ── Fixture ──────────────────────────────────────────────────────────────────

type TestApp = App<Probe, TestClock>;

const ZETA: &str = "agent_zeta";
const ALPHA: &str = "agent_alpha";
const MID: &str = "agent_mid";
const DESC: &str = "retained draft text";
const ATTACHMENT: &str = "/tmp/qol2-picker-attachment.png";

fn roster() -> Vec<Agent> {
    vec![
        make_agent(ZETA, "zeta-builder", AgentStatus::Idle),
        make_agent(ALPHA, "alpha-ops", AgentStatus::Idle),
        make_agent(MID, "mid-reviewer", AgentStatus::Idle),
    ]
}

fn app_with(agents: Vec<Agent>) -> TestApp {
    let mut inner = FakeBackend::new()
        .with_agents(agents)
        .with_tasks(vec![make_task(
            "tsk_old",
            "Existing task",
            TaskStatus::Todo,
        )])
        .with_goals(vec![make_goal(
            "goal_old",
            "Existing goal",
            GoalStatus::Active,
        )]);
    inner.clipboard = ClipboardPaste::Image {
        path: ATTACHMENT.into(),
        bytes: 2048,
    };
    let mut app = App::new(
        Probe {
            inner,
            roster_error: None,
            assign_checks_roster: false,
            disable_after_write: None,
        },
        TestClock::new(1_000_000),
    );
    app.batch_ms = 0;
    app.refresh_all(true);
    app
}

fn app() -> TestApp {
    app_with(roster())
}

fn press(app: &mut TestApp, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_str(app: &mut TestApp, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn step(app: &TestApp) -> String {
    app.state
        .wizard
        .as_ref()
        .map_or_else(|| "<closed>".to_string(), |w| w.step_id().to_string())
}

fn wizard_open(app: &TestApp) -> bool {
    app.state.wizard.is_some()
}

/// Every recorded call that changes something. Reading the clipboard and
/// ringing the bell are not writes.
fn writes(app: &TestApp) -> Vec<Call> {
    app.backend
        .inner
        .calls
        .iter()
        .filter(|c| !matches!(c, Call::PasteImage | Call::Bell))
        .cloned()
        .collect()
}

fn count(app: &TestApp, pred: impl Fn(&Call) -> bool) -> usize {
    writes(app).iter().filter(|c| pred(c)).count()
}

fn name_of(id: &str) -> String {
    roster()
        .into_iter()
        .chain([make_agent("agent_alpha2", "alpha-ops", AgentStatus::Idle)])
        .find(|a| a.id == id)
        .map(|a| a.name)
        .unwrap_or_else(|| id.to_string())
}

/// Press Enter on every step before `target`, with whatever the step already
/// holds, until `target` is reached.
fn advance_to(app: &mut TestApp, target: &str) {
    for _ in 0..12 {
        if step(app) == target {
            return;
        }
        assert!(wizard_open(app), "wizard closed before reaching {target}");
        if step(app) == "description" {
            panic!("passed {target}: now on description");
        }
        press(app, KeyCode::Enter);
    }
    assert_eq!(step(app), target, "could not reach {target}");
}

/// Choose `id` on the agent step the wizard is on: type a prefix of its name
/// (the filter), find its numbered row and press that digit. `None` when the
/// wizard does not offer the agent at all.
fn pick(app: &mut TestApp, id: &str) -> Option<()> {
    let name = name_of(id);
    let prefix: String = name.chars().take(3).collect();
    type_str(app, &prefix);
    let row = app
        .state
        .wizard
        .as_ref()?
        .options()
        .iter()
        .position(|c| c.value == id)?;
    assert!(row < 9, "row {row} is beyond the digit keys");
    press(app, KeyCode::Char(char::from(b'1' + row as u8)));
    Some(())
}

/// Press the digit of the sentinel row (the first row, value empty).
fn pick_sentinel(app: &mut TestApp) {
    let first = app
        .state
        .wizard
        .as_ref()
        .expect("wizard")
        .options()
        .first()
        .cloned()
        .expect("an option");
    assert_eq!(first.value, "", "the first row is the sentinel");
    press(app, KeyCode::Char('1'));
}

/// Tick `id` on the members step, then type a filter that hides every row, so
/// the tick is held but not shown.
fn tick_then_hide(app: &mut TestApp, id: &str) -> Option<()> {
    assert_eq!(step(app), "members");
    let name = name_of(id);
    let prefix: String = name.chars().take(3).collect();
    type_str(app, &prefix);
    let listed = app
        .state
        .wizard
        .as_ref()?
        .options()
        .iter()
        .any(|c| c.value == id);
    if !listed {
        return None;
    }
    press(app, KeyCode::Char(' '));
    type_str(app, "zzzz");
    assert!(
        app.state.wizard.as_ref()?.options().is_empty(),
        "the filter must hide the ticked member"
    );
    Some(())
}

fn change(app: &mut TestApp, id: &str, what: Change) {
    match what {
        Change::Disabled => {
            for agent in app.backend.inner.agents.iter_mut().filter(|a| a.id == id) {
                agent.status = AgentStatus::Disabled;
            }
        }
        Change::Removed => app.backend.inner.agents.retain(|a| a.id != id),
    }
    // The app's own cache is current; the wizard's snapshot is not.
    app.refresh_all(true);
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

/// Open the wizard and fill every earlier step, stopping on the agent step.
fn start(app: &mut TestApp, kind: Kind) {
    match kind {
        Kind::NewTask => {
            app.open_task_wizard();
            type_str(app, "Stale picker task");
            advance_to(app, "assignee");
        }
        Kind::EditTask => {
            app.open_edit_task_wizard();
            advance_to(app, "assignee");
        }
        Kind::NewGoal => {
            app.open_goal_wizard();
            type_str(app, "Stale picker goal");
            advance_to(app, "assignee");
        }
        Kind::EditGoal => {
            app.open_edit_goal_wizard();
            advance_to(app, "assignee");
        }
        Kind::TeamLead => {
            app.open_team_wizard();
            type_str(app, "Platform");
            advance_to(app, "lead");
        }
        Kind::TeamMember => {
            app.open_team_wizard();
            type_str(app, "Platform");
            advance_to(app, "lead");
            pick(app, ZETA).expect("zeta is offered as the lead");
            assert_eq!(step(app), "members");
        }
    }
    assert!(wizard_open(app), "{kind:?}: wizard open at the agent step");
}

/// Choose `id` on the agent step.
fn choose(app: &mut TestApp, kind: Kind, id: &str) -> Option<()> {
    match kind {
        Kind::TeamMember => tick_then_hide(app, id),
        _ => pick(app, id),
    }
}

/// Reach the description step and leave the draft text and, if asked, an
/// attachment on it. The wizard is not submitted.
fn draft_description(app: &mut TestApp, attach: bool) {
    advance_to(app, "description");
    if attach {
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    }
    type_str(app, DESC);
    assert_eq!(step(app), "description");
}

fn submit(app: &mut TestApp) {
    press(app, KeyCode::Tab);
}

/// Drive the whole flow: open, optionally change the roster before choosing,
/// choose `id`, draft the description, optionally change the roster after
/// choosing, and submit. `None` when the wizard did not offer the agent.
fn run(app: &mut TestApp, kind: Kind, id: &str, what: Change, when: When) -> Option<()> {
    start(app, kind);
    if matches!(when, When::AfterOpen) {
        change(app, id, what);
    }
    choose(app, kind, id)?;
    draft_description(app, false);
    if matches!(when, When::AfterSelection) {
        change(app, id, what);
    }
    submit(app);
    Some(())
}

fn assert_refused(app: &TestApp, context: &str) {
    assert!(
        wizard_open(app),
        "{context}: the wizard must stay open on a refused choice"
    );
    assert_eq!(
        writes(app),
        Vec::<Call>::new(),
        "{context}: nothing may be written before the choice is refused"
    );
}

// ── The refusal matrix ───────────────────────────────────────────────────────

macro_rules! refused_before_any_write {
    ($($name:ident: $kind:expr, $what:expr, $when:expr;)*) => {$(
        #[test]
        fn $name() {
            let mut app = app();
            if run(&mut app, $kind, ALPHA, $what, $when).is_some() {
                assert_refused(&app, stringify!($name));
            }
        }
    )*};
}

refused_before_any_write! {
    new_task_assignee_disabled_after_open_is_refused_before_any_write: Kind::NewTask, Change::Disabled, When::AfterOpen;
    new_task_assignee_removed_after_open_is_refused_before_any_write: Kind::NewTask, Change::Removed, When::AfterOpen;
    new_task_assignee_disabled_after_selection_is_refused_before_any_write: Kind::NewTask, Change::Disabled, When::AfterSelection;
    new_task_assignee_removed_after_selection_is_refused_before_any_write: Kind::NewTask, Change::Removed, When::AfterSelection;
    edit_task_assignee_disabled_after_open_is_refused_before_any_write: Kind::EditTask, Change::Disabled, When::AfterOpen;
    edit_task_assignee_removed_after_open_is_refused_before_any_write: Kind::EditTask, Change::Removed, When::AfterOpen;
    edit_task_assignee_disabled_after_selection_is_refused_before_any_write: Kind::EditTask, Change::Disabled, When::AfterSelection;
    edit_task_assignee_removed_after_selection_is_refused_before_any_write: Kind::EditTask, Change::Removed, When::AfterSelection;
    new_goal_assignee_disabled_after_open_is_refused_before_any_write: Kind::NewGoal, Change::Disabled, When::AfterOpen;
    new_goal_assignee_removed_after_open_is_refused_before_any_write: Kind::NewGoal, Change::Removed, When::AfterOpen;
    new_goal_assignee_disabled_after_selection_is_refused_before_any_write: Kind::NewGoal, Change::Disabled, When::AfterSelection;
    new_goal_assignee_removed_after_selection_is_refused_before_any_write: Kind::NewGoal, Change::Removed, When::AfterSelection;
    edit_goal_assignee_disabled_after_open_is_refused_before_any_write: Kind::EditGoal, Change::Disabled, When::AfterOpen;
    edit_goal_assignee_removed_after_open_is_refused_before_any_write: Kind::EditGoal, Change::Removed, When::AfterOpen;
    edit_goal_assignee_disabled_after_selection_is_refused_before_any_write: Kind::EditGoal, Change::Disabled, When::AfterSelection;
    edit_goal_assignee_removed_after_selection_is_refused_before_any_write: Kind::EditGoal, Change::Removed, When::AfterSelection;
    team_lead_disabled_after_open_is_refused_before_any_write: Kind::TeamLead, Change::Disabled, When::AfterOpen;
    team_lead_removed_after_open_is_refused_before_any_write: Kind::TeamLead, Change::Removed, When::AfterOpen;
    team_lead_disabled_after_selection_is_refused_before_any_write: Kind::TeamLead, Change::Disabled, When::AfterSelection;
    team_lead_removed_after_selection_is_refused_before_any_write: Kind::TeamLead, Change::Removed, When::AfterSelection;
    hidden_team_member_disabled_after_open_is_refused_before_any_write: Kind::TeamMember, Change::Disabled, When::AfterOpen;
    hidden_team_member_removed_after_open_is_refused_before_any_write: Kind::TeamMember, Change::Removed, When::AfterOpen;
    hidden_team_member_disabled_after_selection_is_refused_before_any_write: Kind::TeamMember, Change::Disabled, When::AfterSelection;
    hidden_team_member_removed_after_selection_is_refused_before_any_write: Kind::TeamMember, Change::Removed, When::AfterSelection;
}

#[test]
fn new_task_assignee_disabled_after_selection_is_refused_without_a_cache_refresh() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).expect("alpha offered");
    draft_description(&mut app, false);
    // Only the backend changes: `state.agents` still lists alpha as enabled.
    for agent in &mut app.backend.inner.agents {
        if agent.id == ALPHA {
            agent.status = AgentStatus::Disabled;
        }
    }
    assert!(
        app.state
            .agents
            .iter()
            .any(|a| a.id == ALPHA && a.status != AgentStatus::Disabled),
        "precondition: the app cache is stale"
    );
    submit(&mut app);
    assert_refused(&app, "stale cache");
}

#[test]
fn team_with_a_valid_lead_and_a_removed_hidden_member_is_refused_and_no_member_is_dropped() {
    let mut app = app();
    start(&mut app, Kind::TeamMember);
    tick_then_hide(&mut app, ALPHA).expect("alpha offered as member");
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Removed);
    submit(&mut app);
    // Nothing is created: neither with the member silently left out, nor
    // with the lead alone.
    assert_eq!(count(&app, |c| matches!(c, Call::CreateTeam(_))), 0);
}

#[test]
fn team_member_disabled_while_another_valid_member_is_ticked_is_refused_whole() {
    let mut app = app();
    start(&mut app, Kind::TeamMember);
    tick_then_hide(&mut app, MID).expect("mid offered");
    // Clear the hiding filter, then tick alpha too.
    press(&mut app, KeyCode::Esc);
    assert_eq!(step(&app), "members", "Esc clears the filter first");
    tick_then_hide(&mut app, ALPHA).expect("alpha offered");
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Disabled);
    submit(&mut app);
    assert_refused(&app, "one of two members disabled");
}

// ── The error is visible and the draft survives ──────────────────────────────

/// True when the user is told something went wrong: an error on the wizard or
/// a failure message in the log.
fn error_is_visible(app: &TestApp) -> bool {
    app.state
        .wizard
        .as_ref()
        .is_some_and(|w| w.session_error.is_some())
        || app
            .state
            .messages
            .iter()
            .any(|m| m.text.starts_with("Failed"))
}

/// The text the draft holds for a step, whether or not it is the current one.
fn draft_text(app: &TestApp, id: &str) -> String {
    let wizard = app.state.wizard.as_ref().expect("wizard open");
    if wizard.step_id() == id {
        return if id == "description" {
            wizard.textarea_value()
        } else {
            wizard.text_value().to_string()
        };
    }
    wizard.values().get(id).cloned().unwrap_or_default()
}

#[test]
fn refused_new_task_tells_the_user_why() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewTask,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    assert!(error_is_visible(&app));
}

#[test]
fn refused_new_task_keeps_title_description_and_attachment() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, true);
    assert_eq!(app.state.pending_attachments, [ATTACHMENT]);
    change(&mut app, ALPHA, Change::Removed);
    submit(&mut app);
    assert!(wizard_open(&app), "the wizard stays open");
    assert_eq!(
        (
            draft_text(&app, "title"),
            draft_text(&app, "description"),
            app.state.pending_attachments.clone()
        ),
        (
            "Stale picker task".to_string(),
            DESC.to_string(),
            vec![ATTACHMENT.to_string()]
        )
    );
}

#[test]
fn refused_new_goal_keeps_title_and_description() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewGoal,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    assert!(wizard_open(&app));
    assert_eq!(
        (draft_text(&app, "title"), draft_text(&app, "description")),
        ("Stale picker goal".to_string(), DESC.to_string())
    );
}

#[test]
fn refused_team_keeps_name_and_description() {
    let mut app = app();
    run(
        &mut app,
        Kind::TeamLead,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    assert!(wizard_open(&app));
    assert_eq!(
        (draft_text(&app, "name"), draft_text(&app, "description")),
        ("Platform".to_string(), DESC.to_string())
    );
}

#[test]
fn refused_edit_task_keeps_the_edited_description_and_attachment() {
    let mut app = app();
    start(&mut app, Kind::EditTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, true);
    change(&mut app, ALPHA, Change::Disabled);
    submit(&mut app);
    assert!(wizard_open(&app));
    assert_eq!(
        (
            draft_text(&app, "description"),
            app.state.pending_attachments.clone()
        ),
        (DESC.to_string(), vec![ATTACHMENT.to_string()])
    );
}

// ── A refused choice cannot be re-submitted as it was ────────────────────────

/// Press every key a user could use to confirm or advance, repeatedly.
fn hammer_confirm_keys(app: &mut TestApp) {
    for _ in 0..4 {
        press(app, KeyCode::Enter);
        press(app, KeyCode::Tab);
    }
}

#[test]
fn repeated_submit_of_a_refused_task_choice_still_writes_nothing() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewTask,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    hammer_confirm_keys(&mut app);
    assert_refused(&app, "repeat submit");
}

#[test]
fn repeated_submit_of_a_refused_goal_choice_still_writes_nothing() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewGoal,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    hammer_confirm_keys(&mut app);
    assert_refused(&app, "repeat submit");
}

#[test]
fn repeated_submit_of_a_refused_team_member_still_writes_nothing() {
    let mut app = app();
    run(
        &mut app,
        Kind::TeamMember,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    hammer_confirm_keys(&mut app);
    assert_refused(&app, "repeat submit");
}

#[test]
fn a_refused_task_assignee_is_never_swapped_for_auto_assign() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewTask,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    hammer_confirm_keys(&mut app);
    assert_eq!(
        count(&app, |c| matches!(c, Call::CreateTask { .. })),
        0,
        "no unassigned task may appear in place of the refused assignment"
    );
}

#[test]
fn a_refused_goal_assignee_is_never_swapped_for_any_agent() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewGoal,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    hammer_confirm_keys(&mut app);
    assert_eq!(count(&app, |c| matches!(c, Call::CreateGoal(_))), 0);
}

// ── Correction: a replacement agent, or the offered sentinel ─────────────────

/// Walk back to `target`, then clear any filter.
fn go_back_to(app: &mut TestApp, target: &str) {
    for _ in 0..12 {
        if step(app) == target {
            return;
        }
        assert!(
            wizard_open(app),
            "wizard closed while going back to {target}"
        );
        press(app, KeyCode::Esc);
    }
    assert_eq!(step(app), target);
}

/// Correct a refused single-agent field with `id`, then finish and submit.
fn correct_with_agent(app: &mut TestApp, field: &str, id: &str) {
    go_back_to(app, field);
    pick(app, id).unwrap_or_else(|| panic!("{id} is offered for the correction"));
    advance_to_description_if_needed(app);
    submit(app);
}

fn advance_to_description_if_needed(app: &mut TestApp) {
    for _ in 0..8 {
        if step(app) == "description" || !wizard_open(app) {
            return;
        }
        press(app, KeyCode::Enter);
    }
}

fn correct_with_sentinel(app: &mut TestApp, field: &str) {
    go_back_to(app, field);
    pick_sentinel(app);
    advance_to_description_if_needed(app);
    submit(app);
}

fn create_task_calls(app: &TestApp) -> Vec<(String, CreateTaskOpts)> {
    writes(app)
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateTask { title, opts } => Some((title, opts)),
            _ => None,
        })
        .collect()
}

#[test]
fn new_task_corrected_with_a_replacement_agent_commits_once_with_the_draft_and_attachment() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, true);
    change(&mut app, ALPHA, Change::Disabled);
    submit(&mut app);
    assert_refused(&app, "before correction");
    correct_with_agent(&mut app, "assignee", MID);
    let created = create_task_calls(&app);
    assert_eq!(created.len(), 1, "exactly one task is created");
    assert_eq!(
        (
            created[0].0.as_str(),
            created[0].1.description.as_deref(),
            created[0].1.attachments.clone()
        ),
        (
            "Stale picker task",
            Some(DESC),
            Some(vec![ATTACHMENT.to_string()])
        )
    );
    assert!(
        writes(&app).contains(&Call::AssignTask {
            task_id: "tsk_001".into(),
            agent_id: MID.into()
        }),
        "the replacement is the one assigned: {:?}",
        writes(&app)
    );
    assert!(!wizard_open(&app));
    hammer_confirm_keys(&mut app);
    assert_eq!(create_task_calls(&app).len(), 1, "no second create");
}

#[test]
fn new_task_corrected_with_the_auto_assign_sentinel_commits_once_unassigned() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewTask,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_sentinel(&mut app, "assignee");
    assert_eq!(create_task_calls(&app).len(), 1);
    assert_eq!(
        count(&app, |c| matches!(c, Call::AssignTask { .. })),
        0,
        "the explicit sentinel assigns nobody"
    );
}

#[test]
fn new_task_corrected_with_the_sentinel_keeps_the_description() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewTask,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_sentinel(&mut app, "assignee");
    assert_eq!(
        create_task_calls(&app)[0].1.description.as_deref(),
        Some(DESC)
    );
}

#[test]
fn edit_task_corrected_with_a_replacement_agent_updates_once_and_assigns_the_replacement() {
    let mut app = app();
    start(&mut app, Kind::EditTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Removed);
    submit(&mut app);
    assert_refused(&app, "before correction");
    correct_with_agent(&mut app, "assignee", ZETA);
    let all = writes(&app);
    assert_eq!(
        all.iter()
            .filter(|c| matches!(c, Call::UpdateTask { .. }))
            .count(),
        1
    );
    assert!(all.contains(&Call::AssignTask {
        task_id: "tsk_old".into(),
        agent_id: ZETA.into()
    }));
}

#[test]
fn edit_task_corrected_with_the_sentinel_updates_once_and_assigns_nobody() {
    let mut app = app();
    run(
        &mut app,
        Kind::EditTask,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_sentinel(&mut app, "assignee");
    let all = writes(&app);
    assert_eq!(
        (
            all.iter()
                .filter(|c| matches!(c, Call::UpdateTask { .. }))
                .count(),
            all.iter()
                .filter(|c| matches!(c, Call::AssignTask { .. }))
                .count()
        ),
        (1, 0)
    );
}

#[test]
fn new_goal_corrected_with_a_replacement_agent_creates_once_with_the_description() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewGoal,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_agent(&mut app, "assignee", ZETA);
    let goals: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateGoal(input) => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(goals.len(), 1, "exactly one goal is created");
    assert_eq!(
        (
            goals[0].assignee.as_deref(),
            goals[0].description.as_deref()
        ),
        (Some(ZETA), Some(DESC))
    );
}

#[test]
fn new_goal_corrected_with_the_sentinel_creates_once_with_no_assignee() {
    let mut app = app();
    run(
        &mut app,
        Kind::NewGoal,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_sentinel(&mut app, "assignee");
    let goals: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateGoal(input) => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(goals.len(), 1);
    assert_ne!(
        goals[0].assignee.as_deref(),
        Some(ALPHA),
        "the removed agent must not reach the goal"
    );
}

#[test]
fn edit_goal_corrected_with_a_replacement_agent_updates_once_with_it() {
    let mut app = app();
    run(
        &mut app,
        Kind::EditGoal,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_agent(&mut app, "assignee", MID);
    let updates: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::UpdateGoal { id, fields } => Some((id, fields)),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].1.assignee.as_deref(), Some(MID));
}

#[test]
fn edit_goal_corrected_with_the_sentinel_never_writes_the_stale_agent() {
    let mut app = app();
    run(
        &mut app,
        Kind::EditGoal,
        ALPHA,
        Change::Disabled,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_sentinel(&mut app, "assignee");
    let stale = writes(&app).into_iter().any(|c| match c {
        Call::UpdateGoal { fields, .. } => fields.assignee.as_deref() == Some(ALPHA),
        _ => false,
    });
    assert!(!stale);
}

#[test]
fn team_corrected_with_a_replacement_lead_creates_once_with_that_lead() {
    let mut app = app();
    run(
        &mut app,
        Kind::TeamLead,
        ALPHA,
        Change::Removed,
        When::AfterSelection,
    )
    .unwrap();
    assert_refused(&app, "before correction");
    correct_with_agent(&mut app, "lead", MID);
    let teams: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateTeam(input) => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(teams.len(), 1, "exactly one team is created");
    assert_eq!(teams[0].lead_agent_id, MID);
}

#[test]
fn team_with_the_refused_member_unticked_creates_once_without_it() {
    let mut app = app();
    start(&mut app, Kind::TeamMember);
    tick_then_hide(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Disabled);
    submit(&mut app);
    assert_refused(&app, "before correction");
    // Back to the members step, clear the filter and untick the refused one.
    go_back_to(&mut app, "members");
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    let row = app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .options()
        .iter()
        .position(|c| c.value == ALPHA)
        .expect("the refused member stays listed so that it can be unticked");
    press(&mut app, KeyCode::Home);
    for _ in 0..row {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    assert!(
        !app.state
            .wizard
            .as_ref()
            .unwrap()
            .multi_selected()
            .contains(ALPHA),
        "the refused member can be unticked"
    );
    advance_to_description_if_needed(&mut app);
    submit(&mut app);
    let teams: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateTeam(input) => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(teams.len(), 1);
    assert_eq!(
        teams[0].member_agent_ids.clone().unwrap_or_default(),
        Vec::<String>::new(),
        "the lead alone, nobody else"
    );
}

// ── A refused field reopens on fresh choices ─────────────────────────────────

macro_rules! reopens_on_fresh_choices {
    ($($name:ident: $kind:expr, $what:expr, $field:expr;)*) => {$(
        #[test]
        fn $name() {
            let mut app = app();
            run(&mut app, $kind, ALPHA, $what, When::AfterSelection).unwrap();
            assert_refused(&app, "before looking at the choices");
            go_back_to(&mut app, $field);
            app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            let offered = app
                .state
                .wizard
                .as_ref()
                .unwrap()
                .options()
                .iter()
                .any(|c| c.value == ALPHA);
            assert!(!offered, "the agent that was refused must not be offered again");
        }
    )*};
}

reopens_on_fresh_choices! {
    refused_disabled_task_assignee_is_not_offered_again: Kind::NewTask, Change::Disabled, "assignee";
    refused_removed_task_assignee_is_not_offered_again: Kind::NewTask, Change::Removed, "assignee";
    refused_disabled_edit_task_assignee_is_not_offered_again: Kind::EditTask, Change::Disabled, "assignee";
    refused_disabled_goal_assignee_is_not_offered_again: Kind::NewGoal, Change::Disabled, "assignee";
    refused_removed_edit_goal_assignee_is_not_offered_again: Kind::EditGoal, Change::Removed, "assignee";
    refused_disabled_team_lead_is_not_offered_again: Kind::TeamLead, Change::Disabled, "lead";
    refused_removed_team_lead_is_not_offered_again: Kind::TeamLead, Change::Removed, "lead";
}

// ── The roster cannot be read ────────────────────────────────────────────────

fn fail_roster(app: &mut TestApp) {
    app.backend.roster_error = Some("roster unavailable".into());
}

#[test]
fn unreadable_roster_refuses_a_new_task_assignment_and_writes_nothing() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    fail_roster(&mut app);
    submit(&mut app);
    assert_refused(&app, "roster read failure");
}

#[test]
fn unreadable_roster_refuses_a_new_goal_assignment_and_writes_nothing() {
    let mut app = app();
    start(&mut app, Kind::NewGoal);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    fail_roster(&mut app);
    submit(&mut app);
    assert_refused(&app, "roster read failure");
}

#[test]
fn unreadable_roster_refuses_an_edit_task_assignment_and_writes_nothing() {
    let mut app = app();
    start(&mut app, Kind::EditTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    fail_roster(&mut app);
    submit(&mut app);
    assert_refused(&app, "roster read failure");
}

#[test]
fn unreadable_roster_refuses_a_team_and_writes_nothing() {
    let mut app = app();
    start(&mut app, Kind::TeamLead);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    fail_roster(&mut app);
    submit(&mut app);
    assert_refused(&app, "roster read failure");
}

#[test]
fn unreadable_roster_is_reported_to_the_user_and_keeps_the_draft() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, true);
    fail_roster(&mut app);
    submit(&mut app);
    assert!(error_is_visible(&app), "the failure must be visible");
    assert_eq!(
        (
            draft_text(&app, "title"),
            draft_text(&app, "description"),
            app.state.pending_attachments.len()
        ),
        ("Stale picker task".to_string(), DESC.to_string(), 1)
    );
}

#[test]
fn unreadable_roster_does_not_report_a_created_task() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    fail_roster(&mut app);
    submit(&mut app);
    assert!(
        !app.state
            .messages
            .iter()
            .any(|m| m.text.contains("Created")),
        "no success may be fabricated: {:?}",
        app.state
            .messages
            .iter()
            .map(|m| &m.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn once_the_roster_is_readable_again_the_same_draft_commits_exactly_once() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, true);
    fail_roster(&mut app);
    submit(&mut app);
    assert_refused(&app, "while unreadable");
    app.backend.roster_error = None;
    for _ in 0..3 {
        if !wizard_open(&app) {
            break;
        }
        submit(&mut app);
    }
    hammer_confirm_keys(&mut app);
    assert_eq!(create_task_calls(&app).len(), 1, "one task, no duplicate");
}

// ── Duplicate names, distinct ids; every candidate gone ──────────────────────

fn roster_with_two_alphas() -> Vec<Agent> {
    let mut agents = roster();
    agents.push(make_agent("agent_alpha2", "alpha-ops", AgentStatus::Idle));
    agents
}

#[test]
fn disabled_agent_is_not_swapped_for_a_same_named_sibling() {
    let mut app = app_with(roster_with_two_alphas());
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Disabled);
    submit(&mut app);
    assert_refused(&app, "duplicate names");
    hammer_confirm_keys(&mut app);
    assert!(
        !writes(&app).contains(&Call::AssignTask {
            task_id: "tsk_001".into(),
            agent_id: "agent_alpha2".into()
        }),
        "the sibling with the same name must not be assigned"
    );
}

#[test]
fn removed_agent_is_not_swapped_for_a_same_named_sibling() {
    let mut app = app_with(roster_with_two_alphas());
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Removed);
    submit(&mut app);
    assert_refused(&app, "duplicate names, one removed");
    hammer_confirm_keys(&mut app);
    assert!(
        !writes(&app).contains(&Call::AssignTask {
            task_id: "tsk_001".into(),
            agent_id: "agent_alpha2".into()
        }),
        "the sibling with the same name must not be assigned"
    );
}

#[test]
fn the_chosen_id_stays_valid_when_a_same_named_sibling_is_disabled() {
    let mut app = app_with(roster_with_two_alphas());
    start(&mut app, Kind::NewTask);
    pick(&mut app, "agent_alpha2").unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Disabled);
    submit(&mut app);
    assert!(
        writes(&app).contains(&Call::AssignTask {
            task_id: "tsk_001".into(),
            agent_id: "agent_alpha2".into()
        }),
        "exact-id membership: {:?}",
        writes(&app)
    );
}

#[test]
fn team_whose_every_candidate_vanished_writes_nothing_and_has_no_sentinel_fallback() {
    let mut app = app_with(vec![make_agent(ALPHA, "alpha-ops", AgentStatus::Idle)]);
    start(&mut app, Kind::TeamLead);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Removed);
    submit(&mut app);
    hammer_confirm_keys(&mut app);
    assert_refused(&app, "no candidate left");
}

#[test]
fn task_whose_every_candidate_vanished_never_falls_back_to_unassigned() {
    let mut app = app_with(vec![make_agent(ALPHA, "alpha-ops", AgentStatus::Idle)]);
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    change(&mut app, ALPHA, Change::Removed);
    submit(&mut app);
    hammer_confirm_keys(&mut app);
    assert_refused(&app, "no candidate left");
}

// ── Controls: an eligible choice still commits exactly what was chosen ──────

#[test]
fn enabled_agent_chosen_through_a_filter_is_the_one_assigned_to_a_new_task() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    submit(&mut app);
    let all = writes(&app);
    assert_eq!(
        (create_task_calls(&app).len(), wizard_open(&app)),
        (1, false)
    );
    assert!(all.contains(&Call::AssignTask {
        task_id: "tsk_001".into(),
        agent_id: ALPHA.into()
    }));
}

#[test]
fn running_agent_is_still_an_eligible_assignee() {
    let mut agents = roster();
    agents[1].status = AgentStatus::Running;
    let mut app = app_with(agents);
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    submit(&mut app);
    assert!(
        writes(&app).contains(&Call::AssignTask {
            task_id: "tsk_001".into(),
            agent_id: ALPHA.into()
        }),
        "a busy agent is a legitimate choice: {:?}",
        writes(&app)
    );
}

#[test]
fn enabled_team_lead_and_member_commit_one_team_with_exactly_those_ids() {
    let mut app = app();
    start(&mut app, Kind::TeamMember);
    tick_then_hide(&mut app, MID).unwrap();
    draft_description(&mut app, false);
    submit(&mut app);
    let teams: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::CreateTeam(input) => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(teams.len(), 1);
    assert_eq!(
        (
            teams[0].lead_agent_id.as_str(),
            teams[0].member_agent_ids.clone()
        ),
        (ZETA, Some(vec![MID.to_string()]))
    );
}

#[test]
fn enabled_edit_goal_assignee_updates_the_goal_once() {
    let mut app = app();
    start(&mut app, Kind::EditGoal);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    submit(&mut app);
    let updates: Vec<_> = writes(&app)
        .into_iter()
        .filter_map(|c| match c {
            Call::UpdateGoal { fields, .. } => Some(fields),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].assignee.as_deref(), Some(ALPHA));
}

#[test]
fn explicit_sentinel_with_a_healthy_roster_creates_one_unassigned_task() {
    let mut app = app();
    start(&mut app, Kind::NewTask);
    pick_sentinel(&mut app);
    draft_description(&mut app, false);
    submit(&mut app);
    assert_eq!(create_task_calls(&app).len(), 1);
    assert_eq!(count(&app, |c| matches!(c, Call::AssignTask { .. })), 0);
}

// ── Failure after the primary write keeps the committed-id, no-retry policy ──

#[test]
fn assignment_failure_after_a_valid_preflight_reports_the_committed_task_id() {
    let mut app = app();
    app.backend.assign_checks_roster = true;
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    app.backend.disable_after_write = Some(ALPHA.into());
    submit(&mut app);
    let id = app
        .backend
        .inner
        .tasks
        .last()
        .map(|t| t.id.clone())
        .unwrap();
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.text.contains(&id) && m.text.contains("assignment failed")),
        "{:?}",
        app.state
            .messages
            .iter()
            .map(|m| &m.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn assignment_failure_after_a_valid_preflight_closes_the_wizard_and_creates_one_task() {
    let mut app = app();
    app.backend.assign_checks_roster = true;
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    app.backend.disable_after_write = Some(ALPHA.into());
    submit(&mut app);
    assert_eq!(
        (wizard_open(&app), create_task_calls(&app).len()),
        (false, 1),
        "an agent disabled after the validating read is not a refusal; the primary write landed"
    );
}

#[test]
fn assignment_failure_after_the_primary_write_never_retains_a_retry_create() {
    let mut app = app();
    app.backend.assign_checks_roster = true;
    start(&mut app, Kind::NewTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    app.backend.disable_after_write = Some(ALPHA.into());
    submit(&mut app);
    hammer_confirm_keys(&mut app);
    submit(&mut app);
    assert_eq!(create_task_calls(&app).len(), 1, "no duplicate create");
}

#[test]
fn edit_task_assignment_failure_after_the_update_reports_the_task_and_closes_the_wizard() {
    let mut app = app();
    app.backend.assign_checks_roster = true;
    start(&mut app, Kind::EditTask);
    pick(&mut app, ALPHA).unwrap();
    draft_description(&mut app, false);
    app.backend.disable_after_write = Some(ALPHA.into());
    submit(&mut app);
    assert_eq!(
        (
            wizard_open(&app),
            count(&app, |c| matches!(c, Call::UpdateTask { .. })),
            app.state
                .messages
                .iter()
                .any(|m| m.text.contains("tsk_old") && m.text.contains("assignment failed")),
        ),
        (false, 1, true),
        "{:?}",
        app.state
            .messages
            .iter()
            .map(|m| &m.text)
            .collect::<Vec<_>>()
    );
}
