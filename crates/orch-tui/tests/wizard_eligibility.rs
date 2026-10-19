//! The wizards' final-submit eligibility check and the recovery it offers
//! (R7, D7; decision DQ2).
//!
//! Every agent id a wizard would submit (task and goal assignee, team lead
//! and members, new and edit) is checked against a fresh roster before the
//! first write. A refused or unreadable check writes nothing, keeps the
//! draft and attachments, and reopens the refused choice with fresh rows and
//! nothing chosen in its place. Agents are disabled or removed on the
//! `FakeBackend` after the wizard has taken its snapshot, which is what
//! another process does to the real store.
mod common;

use common::Harness;
use orch_domain::*;
use orch_tui::backend::*;
use orch_tui::hit::ZoneKind;
use orch_tui::wizard::engine::UNTICK_UNAVAILABLE;
use orch_tui::wizard::{DraftKey, configs};
use orch_tui::{App, TestClock};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const ZETA: &str = "agt_zeta";
const ALPHA: &str = "agt_alpha";
const MID: &str = "agt_mid";

fn roster() -> Vec<Agent> {
    vec![
        make_agent(ZETA, "zeta-builder", AgentStatus::Idle),
        make_agent(ALPHA, "alpha-ops", AgentStatus::Idle),
        make_agent(MID, "mid-reviewer", AgentStatus::Running),
    ]
}

fn harness(fake: FakeBackend) -> Harness {
    let mut h = Harness::with(fake);
    h.app.batch_ms = 0;
    h.app.refresh_all(true);
    h
}

fn disable(h: &mut Harness, id: &str) {
    for agent in h.app.backend.agents.iter_mut().filter(|a| a.id == id) {
        agent.status = AgentStatus::Disabled;
    }
}

fn remove(h: &mut Harness, id: &str) {
    h.app.backend.agents.retain(|a| a.id != id);
}

/// What other process changed the agent between the wizard's snapshot and
/// the final submit.
#[derive(Clone, Copy, Debug)]
enum Change {
    Disabled,
    Removed,
}

fn change(h: &mut Harness, id: &str, how: Change) {
    match how {
        Change::Disabled => disable(h, id),
        Change::Removed => remove(h, id),
    }
}

/// Every facade call that writes, in order.
fn writes(calls: &[Call]) -> Vec<Call> {
    calls
        .iter()
        .filter(|c| {
            matches!(
                c,
                Call::CreateTask { .. }
                    | Call::UpdateTask { .. }
                    | Call::AssignTask { .. }
                    | Call::CreateTeam(_)
                    | Call::JoinTeam { .. }
                    | Call::LeaveTeam { .. }
                    | Call::SetTeamLead { .. }
                    | Call::CreateGoal(_)
                    | Call::UpdateGoal { .. }
                    | Call::ToggleAutonomous { .. }
                    | Call::EnableAgent(_)
                    | Call::DisableAgent(_)
            )
        })
        .cloned()
        .collect()
}

fn step(h: &Harness) -> String {
    h.app
        .state
        .wizard
        .as_ref()
        .map_or_else(|| "<closed>".into(), |w| w.step_id().to_string())
}

fn session_error(h: &Harness) -> String {
    h.app
        .state
        .wizard
        .as_ref()
        .and_then(|w| w.session_error.clone())
        .unwrap_or_default()
}

fn answer(h: &Harness, id: &str) -> Option<String> {
    h.app.state.wizard.as_ref()?.values().get(id).cloned()
}

fn listed(h: &Harness) -> Vec<String> {
    h.app
        .state
        .wizard
        .as_ref()
        .map(|w| w.options().into_iter().map(|c| c.value).collect())
        .unwrap_or_default()
}

fn missing(h: &Harness) -> bool {
    h.app
        .state
        .wizard
        .as_ref()
        .is_some_and(|w| w.selection_missing())
}

/// NEW TASK: title, default priority, filter the assignee list by `filter`
/// and pick listed row `digit`, then type a description. Ends on the
/// description step, before the final `Tab`.
fn new_task_to_description(h: &mut Harness, filter: &str, digit: char) {
    h.app.open_task_wizard();
    h.type_str("Ship it")
        .press(KeyCode::Enter)
        .press(KeyCode::Enter);
    assert_eq!(step(h), "assignee");
    h.type_str(filter).key(digit);
    assert_eq!(step(h), "description");
    h.type_str("keep this text");
}

/// Assert the refusal contract: no write, the wizard open on `step_id`,
/// the draft and attachments kept, and the message naming the agent.
#[track_caller]
fn assert_refused(h: &Harness, step_id: &str, name: &str, reason: &str) {
    assert_eq!(writes(&h.app.backend.calls), Vec::<Call>::new(), "no write");
    assert_eq!(step(h), step_id);
    let error = session_error(h);
    assert!(error.starts_with("Nothing was saved"), "{error}");
    assert!(error.contains(name) && error.contains(reason), "{error}");
    let key = DraftKey::from_wizard(h.app.state.wizard.as_ref().unwrap());
    assert!(
        h.app.state.wizard_sessions.draft_for(&key).is_some(),
        "draft kept"
    );
}

fn tab(h: &mut Harness) {
    h.press(KeyCode::Tab);
}

// ── New task ───────────────────────────────────────────────────────────────

#[test]
fn new_task_enabled_assignee_after_filter_and_reorder_persists_exactly_that_id() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    h.app.open_task_wizard();
    h.type_str("Ship it")
        .press(KeyCode::Enter)
        .press(KeyCode::Enter);
    // Name order lists alpha-ops first; the filter keeps only it.
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    h.type_str("o").key('2');
    assert_eq!(answer(&h, "assignee").as_deref(), Some(ALPHA));
    h.type_str("details");
    tab(&mut h);
    assert_eq!(step(&h), "<closed>");
    let writes = writes(&h.app.backend.calls);
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(matches!(&writes[0], Call::CreateTask { title, .. } if title == "Ship it"));
    assert!(
        matches!(&writes[1], Call::AssignTask { agent_id, .. } if agent_id == ALPHA),
        "{writes:?}"
    );
}

#[test]
fn new_task_assignee_changed_after_it_was_accepted_is_refused_before_any_write() {
    for how in [Change::Disabled, Change::Removed] {
        let mut h = harness(FakeBackend::new().with_agents(roster()));
        new_task_to_description(&mut h, "ops", '2');
        h.app.state.pending_attachments = vec!["/tmp/shot.png".into()];
        change(&mut h, ALPHA, how);
        tab(&mut h);
        let reason = match how {
            Change::Disabled => "is disabled",
            Change::Removed => "no longer exists",
        };
        assert_refused(&h, "assignee", "alpha-ops", reason);
        assert!(missing(&h), "{how:?}: nothing chosen in its place");
        assert_eq!(answer(&h, "title").as_deref(), Some("Ship it"));
        assert_eq!(answer(&h, "description").as_deref(), Some("keep this text"));
        assert_eq!(h.app.state.pending_attachments, vec!["/tmp/shot.png"]);
        // Fresh rows: the refused agent is no longer offered.
        assert_eq!(listed(&h), vec!["", ZETA, MID], "{how:?}");
    }
}

#[test]
fn new_task_assignee_disabled_before_it_was_accepted_is_refused_too() {
    // The wizard's snapshot still lists alpha-ops when it is picked.
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    h.app.open_task_wizard();
    h.type_str("Ship it")
        .press(KeyCode::Enter)
        .press(KeyCode::Enter);
    disable(&mut h, ALPHA);
    h.type_str("ops").key('2');
    h.type_str("keep this text");
    tab(&mut h);
    assert_refused(&h, "assignee", "alpha-ops", "is disabled");
}

#[test]
fn a_refused_assignee_is_never_confirmed_as_the_sentinel_or_another_row() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    new_task_to_description(&mut h, "ops", '2');
    disable(&mut h, ALPHA);
    tab(&mut h);
    // Confirming without choosing is refused, however often it is tried.
    for _ in 0..3 {
        h.press(KeyCode::Enter);
        tab(&mut h);
    }
    assert_eq!(step(&h), "assignee");
    assert!(missing(&h));
    assert_eq!(
        h.app.state.wizard.as_ref().unwrap().current_value(),
        ALPHA,
        "the refused id is shown as the answer, not the sentinel"
    );
    assert_eq!(writes(&h.app.backend.calls), Vec::<Call>::new());
}

#[test]
fn choosing_the_offered_sentinel_after_a_refusal_creates_an_unassigned_task() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    new_task_to_description(&mut h, "ops", '2');
    remove(&mut h, ALPHA);
    tab(&mut h);
    h.key('1');
    assert_eq!(step(&h), "description");
    tab(&mut h);
    let writes = writes(&h.app.backend.calls);
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(
        matches!(&writes[0], Call::CreateTask { opts, .. } if opts.description.as_deref() == Some("keep this text"))
    );
    assert_eq!(step(&h), "<closed>");
}

#[test]
fn a_replacement_after_a_refusal_writes_once_with_the_kept_draft_and_attachments() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    new_task_to_description(&mut h, "ops", '2');
    h.app.state.pending_attachments = vec!["/tmp/shot.png".into()];
    disable(&mut h, ALPHA);
    tab(&mut h);
    let key = DraftKey::from_wizard(h.app.state.wizard.as_ref().unwrap());
    h.type_str("mid").key('2');
    assert_eq!(step(&h), "description");
    tab(&mut h);
    // A second submit after the wizard closed must not create again.
    tab(&mut h);
    let writes = writes(&h.app.backend.calls);
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(matches!(
        &writes[0],
        Call::CreateTask { title, opts }
            if title == "Ship it"
                && opts.description.as_deref() == Some("keep this text")
                && opts.attachments.as_deref() == Some(&["/tmp/shot.png".to_string()][..])
    ));
    assert!(matches!(&writes[1], Call::AssignTask { agent_id, .. } if agent_id == MID));
    assert_eq!(step(&h), "<closed>");
    assert!(
        h.app.state.wizard_sessions.draft_for(&key).is_none(),
        "draft cleared after success"
    );
}

#[test]
fn every_candidate_vanishing_keeps_the_refused_step_reachable_with_the_sentinel() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    new_task_to_description(&mut h, "ops", '2');
    h.app.backend.agents.clear();
    tab(&mut h);
    assert_refused(&h, "assignee", "alpha-ops", "no longer exists");
    assert_eq!(listed(&h), vec![""]);
    h.key('1');
    tab(&mut h);
    let writes = writes(&h.app.backend.calls);
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(matches!(&writes[0], Call::CreateTask { .. }));
}

#[test]
fn duplicate_names_are_checked_by_exact_id() {
    let agents = vec![
        make_agent("agt_ops_1", "ops", AgentStatus::Idle),
        make_agent("agt_ops_2", "ops", AgentStatus::Idle),
    ];
    let mut h = harness(FakeBackend::new().with_agents(agents));
    new_task_to_description(&mut h, "ops", '2');
    assert_eq!(answer(&h, "assignee").as_deref(), Some("agt_ops_1"));
    disable(&mut h, "agt_ops_1");
    tab(&mut h);
    // An enabled agent with the same name does not make the choice valid.
    assert_refused(&h, "assignee", "ops", "is disabled");
    assert_eq!(listed(&h), vec!["", "agt_ops_2"]);
}

// ── Mouse and rendering of a refused choice ────────────────────────────────

#[test]
fn a_refused_assignee_draws_no_chosen_row_and_a_click_chooses_one() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    new_task_to_description(&mut h, "ops", '2');
    disable(&mut h, ALPHA);
    tab(&mut h);
    let text = h.text();
    assert!(
        text.contains("Nothing was saved: assignee alpha-ops is disabled"),
        "{text}"
    );
    for row in ["Auto-assign", "zeta-builder", "mid-reviewer"] {
        let line = text.lines().find(|l| l.contains(row)).expect(row);
        assert!(!line.contains('▸'), "no row is drawn as chosen: {line}");
    }
    // A double click on the first row (the sentinel, under the old cursor)
    // chooses it; the parked cursor needs no keyboard first.
    let zone = h
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, ZoneKind::WizardOption { index: 0, .. }))
        .map(|z| z.rect)
        .expect("row 1 is drawn");
    h.double_click(zone.x + 4, zone.y);
    assert_eq!(step(&h), "description");
    assert_eq!(answer(&h, "assignee").as_deref(), Some(""));
}

#[test]
fn down_from_a_refused_choice_enters_the_list_at_the_top() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    new_task_to_description(&mut h, "ops", '2');
    disable(&mut h, ALPHA);
    tab(&mut h);
    h.press(KeyCode::Down);
    let w = h.app.state.wizard.as_ref().unwrap();
    assert!(!w.selection_missing());
    assert_eq!(w.select_index(), 0);
}

// ── Edit task / goal ───────────────────────────────────────────────────────

fn task_assigned_to(assignee: &str) -> Task {
    let mut task = make_task("tsk_1", "Original", TaskStatus::Todo);
    task.assignee = Some(assignee.into());
    task
}

fn goal_assigned_to(assignee: &str) -> Goal {
    let mut goal = make_goal("goal_1", "Original", GoalStatus::Active);
    goal.assignee = Some(assignee.into());
    goal
}

fn open_edit_task(h: &mut Harness) {
    let task = h.app.state.tasks[0].clone();
    let wizard = configs::edit_task_wizard(&task, &h.app.state.agents);
    h.app.open_wizard(wizard);
}

fn open_edit_goal(h: &mut Harness) {
    let goal = h.app.state.goals[0].clone();
    let wizard = configs::edit_goal_wizard(&goal, &h.app.state.agents);
    h.app.open_wizard(wizard);
}

#[test]
fn edit_task_changed_to_an_agent_disabled_since_is_refused_and_the_task_is_untouched() {
    for how in [Change::Disabled, Change::Removed] {
        let fake = FakeBackend::new()
            .with_agents(roster())
            .with_tasks(vec![task_assigned_to(ZETA)]);
        let mut h = harness(fake);
        open_edit_task(&mut h);
        h.press(KeyCode::End).type_str(" v2").press(KeyCode::Enter);
        h.press(KeyCode::Enter);
        assert_eq!(step(&h), "assignee");
        h.type_str("ops").key('2');
        assert_eq!(step(&h), "description");
        change(&mut h, ALPHA, how);
        tab(&mut h);
        assert_eq!(writes(&h.app.backend.calls), Vec::<Call>::new(), "{how:?}");
        assert_eq!(step(&h), "assignee");
        assert!(missing(&h));
        assert_eq!(answer(&h, "title").as_deref(), Some("Original v2"));
        let task = &h.app.backend.tasks[0];
        assert_eq!(
            (task.title.as_str(), task.assignee.as_deref()),
            ("Original", Some(ZETA))
        );
    }
}

#[test]
fn edit_task_keeping_an_assignee_disabled_since_saves_without_reassigning() {
    // C1: an unchanged assignee is omitted, so a title fix needs no
    // reassignment and does not re-send a disabled agent.
    let fake = FakeBackend::new()
        .with_agents(roster())
        .with_tasks(vec![task_assigned_to(ALPHA)]);
    let mut h = harness(fake);
    open_edit_task(&mut h);
    disable(&mut h, ALPHA);
    h.press(KeyCode::End).type_str(" v2").press(KeyCode::Enter);
    h.press(KeyCode::Enter);
    assert_eq!(h.app.state.wizard.as_ref().unwrap().current_value(), ALPHA);
    h.press(KeyCode::Enter);
    tab(&mut h);
    assert_eq!(step(&h), "<closed>");
    let writes = writes(&h.app.backend.calls);
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(
        matches!(&writes[0], Call::UpdateTask { fields, .. } if fields.title.as_deref() == Some("Original v2"))
    );
    assert_eq!(h.app.backend.tasks[0].assignee.as_deref(), Some(ALPHA));
}

#[test]
fn edit_task_keep_current_sentinel_leaves_the_assignee_and_is_labelled_so() {
    // C2: the sentinel submits nothing and the assignee is unchanged, which
    // is what its label says.
    let fake = FakeBackend::new()
        .with_agents(roster())
        .with_tasks(vec![task_assigned_to(ALPHA)]);
    let mut h = harness(fake);
    open_edit_task(&mut h);
    h.press(KeyCode::Enter).press(KeyCode::Enter);
    let first = h.app.state.wizard.as_ref().unwrap().options()[0].clone();
    assert_eq!(
        (first.label.as_str(), first.hint.as_str()),
        ("Keep current", "leave the assignee unchanged")
    );
    h.key('1');
    tab(&mut h);
    let writes = writes(&h.app.backend.calls);
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(matches!(&writes[0], Call::UpdateTask { .. }));
    assert_eq!(h.app.backend.tasks[0].assignee.as_deref(), Some(ALPHA));
}

#[test]
fn edit_goal_changed_to_a_removed_agent_is_refused_and_the_goal_is_untouched() {
    let fake = FakeBackend::new()
        .with_agents(roster())
        .with_goals(vec![goal_assigned_to(ZETA)]);
    let mut h = harness(fake);
    open_edit_goal(&mut h);
    h.press(KeyCode::Enter);
    assert_eq!(step(&h), "assignee");
    h.type_str("ops").key('2');
    remove(&mut h, ALPHA);
    tab(&mut h);
    assert_refused(&h, "assignee", "alpha-ops", "no longer exists");
    assert_eq!(h.app.backend.goals[0].assignee.as_deref(), Some(ZETA));
}

#[test]
fn edit_goal_keeping_its_assignee_sends_no_assignee_change() {
    let fake = FakeBackend::new()
        .with_agents(roster())
        .with_goals(vec![goal_assigned_to(ALPHA)]);
    let mut h = harness(fake);
    open_edit_goal(&mut h);
    remove(&mut h, ALPHA);
    h.press(KeyCode::Enter).press(KeyCode::Enter);
    tab(&mut h);
    assert_eq!(step(&h), "<closed>");
    let writes = writes(&h.app.backend.calls);
    assert!(
        matches!(&writes[..], [Call::UpdateGoal { fields, .. }] if fields.assignee.is_none()),
        "{writes:?}"
    );
}

// ── New goal ───────────────────────────────────────────────────────────────

#[test]
fn new_goal_assignee_changed_after_it_was_accepted_is_refused_before_create() {
    for how in [Change::Disabled, Change::Removed] {
        let mut h = harness(FakeBackend::new().with_agents(roster()));
        h.app.open_goal_wizard();
        h.type_str("Goal").press(KeyCode::Enter);
        h.type_str("ops").key('2');
        assert_eq!(step(&h), "description");
        h.type_str("why");
        change(&mut h, ALPHA, how);
        tab(&mut h);
        assert_eq!(writes(&h.app.backend.calls), Vec::<Call>::new(), "{how:?}");
        assert_eq!(step(&h), "assignee");
        assert!(missing(&h));
        // Recover with the explicit sentinel.
        h.key('1');
        tab(&mut h);
        let writes = writes(&h.app.backend.calls);
        assert!(
            matches!(&writes[..], [Call::CreateGoal(input)] if input.assignee.is_none() && input.description.as_deref() == Some("why")),
            "{how:?}: {writes:?}"
        );
    }
}

// ── Team ───────────────────────────────────────────────────────────────────

fn team_to_members(h: &mut Harness, lead_filter: &str) {
    h.app.open_team_wizard();
    h.type_str("Core").press(KeyCode::Enter);
    assert_eq!(step(h), "lead");
    h.type_str(lead_filter).key('1');
    assert_eq!(step(h), "members");
}

#[test]
fn a_refused_team_lead_reopens_the_lead_step_with_nothing_chosen() {
    for how in [Change::Disabled, Change::Removed] {
        let mut h = harness(FakeBackend::new().with_agents(roster()));
        team_to_members(&mut h, "ops");
        h.press(KeyCode::Enter);
        h.type_str("purpose");
        change(&mut h, ALPHA, how);
        tab(&mut h);
        assert_eq!(writes(&h.app.backend.calls), Vec::<Call>::new(), "{how:?}");
        assert_eq!(step(&h), "lead");
        assert!(missing(&h));
        // A lead has no sentinel: confirming without a choice stays put.
        h.press(KeyCode::Enter);
        assert_eq!(step(&h), "lead");
        h.type_str("zeta").key('1');
        h.press(KeyCode::Enter);
        assert_eq!(step(&h), "description");
        tab(&mut h);
        let writes = writes(&h.app.backend.calls);
        assert!(
            matches!(&writes[..], [Call::CreateTeam(input)] if input.lead_agent_id == ZETA && input.description.as_deref() == Some("purpose")),
            "{how:?}: {writes:?}"
        );
    }
}

#[test]
fn a_refused_member_hidden_by_the_filter_is_listed_and_must_be_unticked() {
    for how in [Change::Disabled, Change::Removed] {
        let mut h = harness(FakeBackend::new().with_agents(roster()));
        team_to_members(&mut h, "zeta");
        // Tick alpha-ops and mid-reviewer, then filter alpha-ops out of view.
        h.type_str("ops").key(' ');
        h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
        h.type_str("mid").key(' ');
        h.press(KeyCode::Enter);
        assert_eq!(answer(&h, "members").as_deref(), Some("agt_alpha,agt_mid"));
        h.type_str("purpose");
        change(&mut h, ALPHA, how);
        tab(&mut h);
        assert_refused(
            &h,
            "members",
            "alpha-ops",
            match how {
                Change::Disabled => "is disabled",
                Change::Removed => "no longer exists",
            },
        );
        let w = h.app.state.wizard.as_ref().unwrap();
        // Still ticked, both of them: nothing was dropped.
        assert_eq!(
            w.multi_selected().iter().cloned().collect::<Vec<_>>(),
            vec![ALPHA, MID]
        );
        let first = w.options()[0].clone();
        assert_eq!(
            first.value, ALPHA,
            "{how:?}: the refused member is listed first"
        );
        assert!(first.hint.contains("untick to remove"), "{}", first.hint);
        // A filter cannot hide it.
        h.type_str("zzz");
        assert_eq!(listed(&h).first().map(String::as_str), Some(ALPHA));
        h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
        // Confirming while it is ticked is refused, with the reason.
        h.press(KeyCode::Enter);
        assert_eq!(step(&h), "members");
        let w = h.app.state.wizard.as_ref().unwrap();
        assert_eq!(w.visible_error(), Some(UNTICK_UNAVAILABLE));
        // Untick it explicitly: it leaves the list and cannot come back.
        h.press(KeyCode::Home).key(' ');
        assert!(!listed(&h).contains(&ALPHA.to_string()), "{how:?}");
        h.press(KeyCode::Enter);
        assert_eq!(step(&h), "description");
        tab(&mut h);
        let writes = writes(&h.app.backend.calls);
        assert!(
            matches!(&writes[..], [Call::CreateTeam(input)]
                if input.lead_agent_id == ZETA
                    && input.member_agent_ids.as_deref() == Some(&[MID.to_string()][..])),
            "{how:?}: {writes:?}"
        );
    }
}

#[test]
fn a_refused_member_is_drawn_with_its_reason() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    team_to_members(&mut h, "zeta");
    h.type_str("ops").key(' ');
    h.press(KeyCode::Enter);
    disable(&mut h, ALPHA);
    tab(&mut h);
    let text = h.text();
    let row = text
        .lines()
        .find(|l| l.contains("alpha-ops") && l.contains("[✓]"))
        .unwrap_or_else(|| panic!("refused member row:\n{text}"));
    assert!(row.contains("disabled · untick to remove"), "{row}");
}

#[test]
fn a_members_step_holding_a_refused_member_is_not_skipped_when_no_one_else_is_left() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    team_to_members(&mut h, "zeta");
    h.type_str("ops").key(' ');
    h.press(KeyCode::Enter);
    // Only the lead is left eligible; the members step must stay reachable.
    remove(&mut h, ALPHA);
    remove(&mut h, MID);
    tab(&mut h);
    assert_refused(&h, "members", "alpha-ops", "no longer exists");
    h.key(' ');
    h.press(KeyCode::Enter);
    tab(&mut h);
    let writes = writes(&h.app.backend.calls);
    assert!(
        matches!(&writes[..], [Call::CreateTeam(input)] if input.member_agent_ids.is_none()),
        "{writes:?}"
    );
}

// ── C5: a rebuild never swaps a lost agent for the sentinel ────────────────

#[test]
fn replacing_steps_never_turns_an_untouched_lost_agent_into_the_sentinel() {
    let agents = roster();
    let task = task_assigned_to(ALPHA);
    let mut wizard = configs::edit_task_wizard(&task, &agents);
    wizard.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
    wizard.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
    assert_eq!(wizard.step_id(), "assignee");
    assert_eq!(wizard.current_value(), ALPHA, "untouched default");
    let without_alpha: Vec<Agent> = agents.into_iter().filter(|a| a.id != ALPHA).collect();
    wizard.replace_steps(configs::edit_task_wizard_steps(&task, &without_alpha));
    assert!(wizard.selection_missing());
    assert_eq!(wizard.current_value(), ALPHA);
    wizard.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
    assert_eq!(wizard.step_id(), "assignee", "confirming is refused");
}

// ── The roster read itself ─────────────────────────────────────────────────

/// A `FakeBackend` whose roster read can fail, as an unreadable remote
/// registry makes the real one fail.
struct Roster {
    inner: FakeBackend,
    error: Option<String>,
}
impl TuiBackend for Roster {
    fn context(&self) -> LaunchContext {
        self.inner.context()
    }
    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        self.inner.refresh_tasks()
    }
    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.inner.refresh_agents()
    }
    fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>> {
        self.inner.refresh_goals()
    }
    fn refresh_state(&mut self) -> BackendResult<OrchestratorState> {
        self.inner.refresh_state()
    }
    fn list_teams(&mut self) -> BackendResult<Vec<Team>> {
        self.inner.list_teams()
    }
    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task> {
        self.inner.create_task(title, opts)
    }
    fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task> {
        self.inner.update_task(id, fields)
    }
    fn delete_task(&mut self, id: &str) -> BackendResult<()> {
        self.inner.delete_task(id)
    }
    fn run_task(&mut self, id: &str) -> BackendResult<()> {
        self.inner.run_task(id)
    }
    fn run_all(&mut self) -> BackendResult<()> {
        self.inner.run_all()
    }
    fn cancel_task(&mut self, id: &str) -> BackendResult<()> {
        self.inner.cancel_task(id)
    }
    fn retry_task(&mut self, id: &str) -> BackendResult<()> {
        self.inner.retry_task(id)
    }
    fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()> {
        self.inner.assign_task(task_id, agent_id)
    }
    fn approve_task(&mut self, id: &str) -> BackendResult<()> {
        self.inner.approve_task(id)
    }
    fn reject_task(&mut self, id: &str, feedback: Option<&str>) -> BackendResult<()> {
        self.inner.reject_task(id, feedback)
    }
    fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent> {
        self.inner.add_agent(name, adapter, opts)
    }
    fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent> {
        self.inner.update_agent(id, fields)
    }
    fn delete_agent(&mut self, id: &str) -> BackendResult<()> {
        self.inner.delete_agent(id)
    }
    fn force_stop_agent(&mut self, id: &str) -> BackendResult<()> {
        self.inner.force_stop_agent(id)
    }
    fn toggle_autonomous(&mut self, id: &str, enabled: bool) -> BackendResult<Agent> {
        self.inner.toggle_autonomous(id, enabled)
    }
    fn disable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.inner.disable_agent(id)
    }
    fn enable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.inner.enable_agent(id)
    }
    fn create_team(&mut self, input: CreateTeamInput) -> BackendResult<Team> {
        self.inner.create_team(input)
    }
    fn join_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.inner.join_team(team_id, agent_id)
    }
    fn leave_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.inner.leave_team(team_id, agent_id)
    }
    fn disband_team(&mut self, team_id: &str) -> BackendResult<()> {
        self.inner.disband_team(team_id)
    }
    fn set_team_lead(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.inner.set_team_lead(team_id, agent_id)
    }
    fn create_goal(&mut self, input: CreateGoalInput) -> BackendResult<Goal> {
        self.inner.create_goal(input)
    }
    fn update_goal(&mut self, id: &str, fields: GoalFields) -> BackendResult<Goal> {
        self.inner.update_goal(id, fields)
    }
    fn update_goal_status(
        &mut self,
        id: &str,
        status: GoalStatus,
        force: bool,
    ) -> BackendResult<Goal> {
        self.inner.update_goal_status(id, status, force)
    }
    fn delete_goal(&mut self, id: &str) -> BackendResult<()> {
        self.inner.delete_goal(id)
    }
    fn goal_progress(&mut self, id: &str) -> BackendResult<String> {
        self.inner.goal_progress(id)
    }
    fn send_message(&mut self, target: MessageTarget, body: &str) -> BackendResult<Vec<String>> {
        self.inner.send_message(target, body)
    }
    fn start_watch(&mut self) -> BackendResult<()> {
        self.inner.start_watch()
    }
    fn stop_watch(&mut self) -> BackendResult<()> {
        self.inner.stop_watch()
    }
    fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()> {
        self.inner.save_palette(palette)
    }
    fn save_activity_filter(&mut self, preset: &str) -> BackendResult<()> {
        self.inner.save_activity_filter(preset)
    }
    fn save_notifications(&mut self, notifications: NotificationPreferences) -> BackendResult<()> {
        self.inner.save_notifications(notifications)
    }
    fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()> {
        self.inner.save_max_concurrent(value)
    }
    fn complete_onboarding(&mut self) -> BackendResult<()> {
        self.inner.complete_onboarding()
    }
    fn poll_events(&mut self) -> Vec<OrchestratorEvent> {
        self.inner.poll_events()
    }
}

fn roster_app(error: Option<&str>) -> App<Roster, TestClock> {
    let mut app = App::new(
        Roster {
            inner: FakeBackend::new().with_agents(roster()),
            error: None,
        },
        TestClock::new(1_000_000),
    );
    app.batch_ms = 0;
    app.backend.error = error.map(str::to_string);
    app
}

fn press(app: &mut App<Roster, TestClock>, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_into(app: &mut App<Roster, TestClock>, text: &str) {
    for ch in text.chars() {
        press(app, KeyCode::Char(ch));
    }
}

#[test]
fn an_unreadable_roster_fails_closed_with_the_draft_kept_and_retries() {
    let mut app = roster_app(None);
    app.open_task_wizard();
    app.state.pending_attachments = vec!["/tmp/shot.png".into()];
    type_into(&mut app, "Ship it");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    type_into(&mut app, "ops");
    press(&mut app, KeyCode::Char('2'));
    type_into(&mut app, "keep this text");
    // The cached list still shows alpha-ops as eligible.
    app.backend.error = Some("remotes.yml: invalid YAML".into());
    press(&mut app, KeyCode::Tab);
    assert_eq!(writes(&app.backend.inner.calls), Vec::<Call>::new());
    let w = app.state.wizard.as_ref().expect("wizard stays open");
    assert_eq!(w.step_id(), "description");
    let error = w.session_error.clone().unwrap_or_default();
    assert!(
        error.starts_with("Nothing was saved: could not read the agent list")
            && error.contains("remotes.yml: invalid YAML"),
        "{error}"
    );
    assert_eq!(w.values().get("assignee").map(String::as_str), Some(ALPHA));
    assert_eq!(app.state.pending_attachments, vec!["/tmp/shot.png"]);
    let key = DraftKey::from_wizard(w);
    assert!(app.state.wizard_sessions.draft_for(&key).is_some());
    // Once the roster reads again, the same submission goes through once.
    app.backend.error = None;
    press(&mut app, KeyCode::Tab);
    let writes = writes(&app.backend.inner.calls);
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(matches!(&writes[1], Call::AssignTask { agent_id, .. } if agent_id == ALPHA));
    assert!(app.state.wizard.is_none());
}

#[test]
fn an_unreadable_roster_does_not_block_a_wizard_that_submits_no_agent() {
    // C4: an unassigned task does not depend on the roster.
    let mut app = roster_app(Some("remotes.yml: invalid YAML"));
    app.open_task_wizard();
    type_into(&mut app, "Ship it");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Tab);
    let writes = writes(&app.backend.inner.calls);
    assert!(
        matches!(&writes[..], [Call::CreateTask { .. }]),
        "{writes:?}"
    );
    assert!(app.state.wizard.is_none());
}

#[test]
fn a_refused_lead_and_member_are_both_reopened_in_turn() {
    let mut h = harness(FakeBackend::new().with_agents(roster()));
    team_to_members(&mut h, "ops");
    h.type_str("mid").key(' ');
    h.press(KeyCode::Enter);
    h.type_str("purpose");
    // Only zeta-builder is left; it can lead, but then nobody else is
    // eligible to be a member.
    remove(&mut h, ALPHA);
    disable(&mut h, MID);
    tab(&mut h);
    assert_refused(&h, "lead", "alpha-ops", "no longer exists");
    let error = session_error(&h);
    assert!(error.contains("member mid-reviewer is disabled"), "{error}");
    h.key('1');
    // The members step still holds the refused member, so it is not skipped.
    assert_eq!(step(&h), "members");
    assert_eq!(listed(&h), vec![MID]);
    assert_eq!(
        h.app.state.wizard.as_ref().unwrap().visible_error(),
        Some(UNTICK_UNAVAILABLE)
    );
    h.key(' ').press(KeyCode::Enter);
    assert_eq!(step(&h), "description");
    tab(&mut h);
    let writes = writes(&h.app.backend.calls);
    assert!(
        matches!(&writes[..], [Call::CreateTeam(input)]
            if input.lead_agent_id == ZETA && input.member_agent_ids.is_none()),
        "{writes:?}"
    );
}
