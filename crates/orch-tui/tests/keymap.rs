//! Counted key-dispatch cases — one per binding, including every overloaded
//! per-view key (PARITY.md §12.2, V33).

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, TaskStatus};
use orch_tui::backend::{Call, FakeBackend, make_agent, make_goal, make_task};
use orch_tui::state::{DetailPanel, InputMode, ViewId};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

fn tasks_harness() -> Harness {
    let backend = FakeBackend::new().with_tasks(vec![
        make_task("tsk_1", "First task", TaskStatus::Todo),
        make_task("tsk_2", "Second task", TaskStatus::Done),
    ]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness
}

fn review_harness() -> Harness {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Needs review", TaskStatus::Review)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness
}

fn agents_harness() -> Harness {
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_1", "alpha", AgentStatus::Idle),
        make_agent("agt_2", "beta", AgentStatus::Running),
    ]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Agents;
    harness
}

fn goals_harness() -> Harness {
    let backend = FakeBackend::new().with_goals(vec![
        make_goal("goal_1", "Ship v2", GoalStatus::Active),
        make_goal("goal_2", "Paused one", GoalStatus::Paused),
    ]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Goals;
    harness
}

fn logs_harness() -> Harness {
    let backend =
        FakeBackend::new().with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness
}

// ── Global keys ────────────────────────────────────────────────────────────

#[test]
fn k001_lowercase_q_quits() {
    let mut harness = Harness::new();
    harness.key('q');
    assert!(harness.app.state.should_exit);
}

#[test]
fn k002_uppercase_q_quits() {
    let mut harness = Harness::new();
    harness.key('Q');
    assert!(harness.app.state.should_exit);
}

#[test]
fn k003_esc_closes_the_detail_panel() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    harness.press(KeyCode::Esc);
    assert_eq!(harness.app.state.detail, DetailPanel::None);
}

#[test]
fn k004_esc_resets_the_goal_detail_scroll() {
    let mut harness = goals_harness();
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.goal_detail_scroll, 1);
    harness.press(KeyCode::Esc);
    assert_eq!(harness.app.state.goal_detail_scroll, 0);
}

#[test]
fn k005_esc_in_logs_returns_to_tail() {
    let mut harness = logs_harness();
    harness.app.state.log_selected_index = 3;
    harness.app.state.log_scroll_offset = 2;
    harness.press(KeyCode::Esc);
    assert_eq!(harness.app.state.log_selected_index, -1);
    assert_eq!(harness.app.state.log_scroll_offset, 0);
}

#[test]
fn k006_esc_never_quits() {
    let mut harness = Harness::new();
    harness.press(KeyCode::Esc);
    assert!(!harness.app.state.should_exit);
}

#[test]
fn k007_plus_grows_the_detail_panel() {
    let mut harness = tasks_harness();
    harness.key('+');
    assert_eq!(harness.app.state.split_offset, -3);
}

#[test]
fn k008_equals_also_grows_the_detail_panel() {
    let mut harness = tasks_harness();
    harness.key('=');
    assert_eq!(harness.app.state.split_offset, -3);
}

#[test]
fn k009_minus_grows_the_list() {
    let mut harness = tasks_harness();
    harness.key('-');
    assert_eq!(harness.app.state.split_offset, 3);
}

#[test]
fn k010_three_consecutive_grows_stay_renderable() {
    let mut harness = tasks_harness();
    for _ in 0..3 {
        harness.key('+');
    }
    harness.assert_text("TASKS");
}

#[test]
fn k011_two_consecutive_shrinks_stay_renderable() {
    let mut harness = tasks_harness();
    harness.key('-');
    harness.key('-');
    harness.assert_text("TASKS");
}

#[test]
fn k012_the_logs_view_ignores_the_resize_keys() {
    let mut harness = logs_harness();
    harness.key('+');
    harness.key('=');
    harness.key('-');
    assert_eq!(harness.app.state.split_offset, 0);
}

#[test]
fn k013_uppercase_m_toggles_maximize() {
    let mut harness = tasks_harness();
    harness.key('M');
    assert!(harness.app.state.detail_maximized);
    harness.key('M');
    assert!(!harness.app.state.detail_maximized);
}

#[test]
fn k014_lowercase_m_has_no_binding() {
    let mut harness = tasks_harness();
    harness.key('m');
    assert!(!harness.app.state.detail_maximized);
}

#[test]
fn k015_plus_exits_maximize_to_half_the_content() {
    let mut harness = tasks_harness();
    harness.key('M');
    harness.key('+');
    assert!(!harness.app.state.detail_maximized);
    assert!(harness.app.state.split_offset < 0);
}

#[test]
fn k016_minus_exits_maximize_to_half_the_content() {
    let mut harness = tasks_harness();
    harness.key('M');
    harness.key('-');
    assert!(!harness.app.state.detail_maximized);
    assert!(harness.app.state.split_offset > 0);
}

#[test]
fn k017_maximize_survives_a_tab_switch() {
    let mut harness = tasks_harness();
    harness.key('M');
    harness.key('a');
    harness.key('t');
    assert!(harness.app.state.detail_maximized);
}

#[test]
fn k018_question_mark_opens_the_help_overlay() {
    let mut harness = Harness::new();
    harness.key('?');
    assert!(harness.app.state.show_help);
    harness.assert_text("KEYBOARD SHORTCUTS");
}

#[test]
fn k019_f1_opens_the_help_overlay() {
    // Un-skipped: `ink-testing-library` could not deliver `\x1bOP`.
    let mut harness = Harness::new();
    harness.press(KeyCode::F(1));
    assert!(harness.app.state.show_help);
    harness.assert_text("KEYBOARD SHORTCUTS");
}

#[test]
fn k020_f1_also_dismisses_the_help_overlay_and_is_swallowed() {
    // The second of the two previously unskippable F1 tests.
    let mut harness = tasks_harness();
    harness.press(KeyCode::F(1));
    harness.press(KeyCode::F(1));
    assert!(!harness.app.state.show_help);
    assert_eq!(harness.app.state.view, ViewId::Tasks);
}

#[test]
fn k021_question_mark_dismisses_the_overlay_and_is_swallowed() {
    let mut harness = tasks_harness();
    harness.key('?');
    harness.key('?');
    assert!(!harness.app.state.show_help);
    assert_eq!(harness.app.state.input_mode, InputMode::None);
}

#[test]
fn k022_esc_dismisses_the_overlay_and_is_swallowed() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Enter);
    harness.key('?');
    harness.press(KeyCode::Esc);
    assert!(!harness.app.state.show_help);
    assert_eq!(
        harness.app.state.detail,
        DetailPanel::Entity,
        "Esc was swallowed by the overlay"
    );
}

#[test]
fn k023_any_other_key_dismisses_the_overlay_and_performs_its_action() {
    let mut harness = tasks_harness();
    harness.key('?');
    harness.key('l');
    assert!(!harness.app.state.show_help);
    assert_eq!(harness.app.state.view, ViewId::Logs);
}

#[test]
fn k024_slash_opens_command_mode_seeded_with_a_slash() {
    let mut harness = Harness::new();
    harness.key('/');
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(harness.app.state.command_bar.input.value(), "/");
}

#[test]
fn k025_slash_is_blocked_while_the_detail_panel_is_open() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Enter);
    harness.key('/');
    assert_eq!(harness.app.state.input_mode, InputMode::None);
}

#[test]
fn k026_tab_cycles_forward_with_wrap() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Tab);
    assert_eq!(harness.app.state.view, ViewId::Agents);
    harness.press(KeyCode::Tab);
    assert_eq!(harness.app.state.view, ViewId::Logs);
    harness.press(KeyCode::Tab);
    assert_eq!(harness.app.state.view, ViewId::Goals);
    harness.press(KeyCode::Tab);
    assert_eq!(harness.app.state.view, ViewId::Tasks);
}

#[test]
fn k027_right_arrow_cycles_forward() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Right);
    assert_eq!(harness.app.state.view, ViewId::Agents);
}

#[test]
fn k028_left_arrow_cycles_backward() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Left);
    assert_eq!(harness.app.state.view, ViewId::Goals);
}

#[test]
fn k029_view_switching_is_blocked_while_the_detail_panel_is_open() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Enter);
    harness.press(KeyCode::Tab);
    harness.key('l');
    harness.key('g');
    assert_eq!(harness.app.state.view, ViewId::Tasks);
}

#[test]
fn k030_t_switches_to_tasks() {
    let mut harness = goals_harness();
    harness.key('t');
    assert_eq!(harness.app.state.view, ViewId::Tasks);
}

#[test]
fn k031_l_switches_to_logs() {
    let mut harness = tasks_harness();
    harness.key('l');
    assert_eq!(harness.app.state.view, ViewId::Logs);
    harness.assert_text("ACTIONS");
}

#[test]
fn k032_g_switches_to_goals_from_other_views() {
    let mut harness = agents_harness();
    harness.key('g');
    assert_eq!(harness.app.state.view, ViewId::Goals);
}

#[test]
fn k033_a_switches_to_agents_when_no_review_task_is_selected() {
    let mut harness = tasks_harness();
    harness.key('a');
    assert_eq!(harness.app.state.view, ViewId::Agents);
}

#[test]
fn k034_a_is_not_a_view_switch_on_the_logs_view() {
    let mut harness = logs_harness();
    harness.key('a');
    assert_eq!(harness.app.state.view, ViewId::Logs);
    assert!(harness.app.state.show_agent_picker);
}

#[test]
fn k035_key_release_events_are_ignored() {
    let mut harness = Harness::new();
    harness.release(KeyCode::Char('q'));
    assert!(!harness.app.state.should_exit);
}

// ── Tasks view ─────────────────────────────────────────────────────────────

#[test]
fn k036_n_opens_the_new_task_wizard() {
    let mut harness = tasks_harness();
    harness.key('n');
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    harness.assert_text("NEW TASK");
    harness.assert_text("Task title");
}

#[test]
fn k037_uppercase_n_also_opens_the_task_wizard() {
    let mut harness = tasks_harness();
    harness.key('N');
    harness.assert_text("NEW TASK");
}

#[test]
fn k038_a_approves_a_selected_review_task() {
    let mut harness = review_harness();
    harness.key('a');
    harness.assert_called(|c| matches!(c, Call::ApproveTask(id) if id == "tsk_1"));
    assert_eq!(
        harness.app.state.view,
        ViewId::Tasks,
        "approve overrides the agents-tab binding"
    );
}

#[test]
fn k039_x_rejects_a_selected_review_task() {
    let mut harness = review_harness();
    harness.key('x');
    harness.assert_called(|c| matches!(c, Call::RejectTask { id, .. } if id == "tsk_1"));
}

#[test]
fn k040_x_does_nothing_for_a_non_review_task() {
    let mut harness = tasks_harness();
    harness.key('x');
    harness.refute_called(|c| matches!(c, Call::RejectTask { .. }));
}

#[test]
fn k041_c_cancels_the_selected_task() {
    let mut harness = tasks_harness();
    harness.key('c');
    harness.assert_called(|c| matches!(c, Call::CancelTask(id) if id == "tsk_1"));
}

#[test]
fn k042_c_refuses_a_terminal_task_with_its_status() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Done one", TaskStatus::Done)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.key('c');
    harness.refute_called(|c| matches!(c, Call::CancelTask(_)));
    harness.assert_message("Cannot cancel — status is done");
}

#[test]
fn k043_e_opens_the_edit_task_wizard() {
    let mut harness = tasks_harness();
    harness.key('e');
    harness.assert_text("EDIT TASK");
}

#[test]
fn k044_s_toggles_show_all_and_resets_the_selection() {
    let mut harness = tasks_harness();
    harness.app.state.task_selected = 1;
    harness.key('s');
    assert!(harness.app.state.show_all_tasks);
    assert_eq!(harness.app.state.task_selected, 0);
    harness.key('S');
    assert!(!harness.app.state.show_all_tasks);
}

#[test]
fn k045_g_toggles_group_by_goal_on_the_tasks_view() {
    let mut harness = tasks_harness();
    harness.key('g');
    assert!(harness.app.state.group_by_goal);
    assert_eq!(
        harness.app.state.view,
        ViewId::Tasks,
        "grouping overrides the goals-tab binding"
    );
    harness.key('G');
    assert!(!harness.app.state.group_by_goal);
}

#[test]
fn k046_d_soft_deletes_the_selected_task() {
    let mut harness = tasks_harness();
    harness.key('d');
    assert_eq!(harness.app.state.pending_deletions.len(), 1);
    harness.refute_called(|c| matches!(c, Call::DeleteTask(_)));
    harness.assert_message("will be deleted in 5s");
}

#[test]
fn k047_d_never_deletes_a_running_task() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Busy", TaskStatus::InProgress)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.key('d');
    assert!(harness.app.state.pending_deletions.is_empty());
}

#[test]
fn k048_z_undoes_the_most_recent_deletion() {
    let mut harness = tasks_harness();
    harness.key('d');
    harness.key('z');
    assert!(harness.app.state.pending_deletions.is_empty());
    harness.assert_message("restored");
}

#[test]
fn k049_uppercase_z_also_undoes() {
    let mut harness = tasks_harness();
    harness.key('d');
    harness.key('Z');
    assert!(harness.app.state.pending_deletions.is_empty());
}

#[test]
fn k050_r_runs_a_runnable_task() {
    let mut harness = tasks_harness();
    harness.key('r');
    harness.assert_called(|c| matches!(c, Call::RunTask(id) if id == "tsk_1"));
}

#[test]
fn k051_r_refuses_a_non_runnable_task() {
    let mut harness = review_harness();
    harness.key('r');
    harness.refute_called(|c| matches!(c, Call::RunTask(_)));
    harness.assert_message("Cannot run \"Needs review\" — status is review");
}

#[test]
fn k052_enter_opens_and_closes_the_task_detail_panel() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    harness.assert_text("DETAIL");
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::None);
}

#[test]
fn k053_enter_on_the_show_all_row_toggles_show_all() {
    let tasks: Vec<_> = (0..11)
        .map(|i| make_task(&format!("tsk_{i}"), &format!("Task {i}"), TaskStatus::Todo))
        .collect();
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.task_selected = 10;
    harness.press(KeyCode::Enter);
    assert!(harness.app.state.show_all_tasks);
}

#[test]
fn k054_enter_on_the_add_task_row_opens_the_wizard() {
    let mut harness = tasks_harness();
    harness.app.state.task_selected = 2;
    harness.press(KeyCode::Enter);
    harness.assert_text("NEW TASK");
}

#[test]
fn k055_j_and_k_navigate_the_task_list_with_clamping() {
    let mut harness = tasks_harness();
    harness.key('j');
    assert_eq!(harness.app.state.task_selected, 1);
    harness.key('j');
    harness.key('j');
    harness.key('j');
    assert_eq!(
        harness.app.state.task_selected, 2,
        "clamped to the add-task row"
    );
    harness.key('k');
    assert_eq!(harness.app.state.task_selected, 1);
    harness.key('k');
    harness.key('k');
    assert_eq!(harness.app.state.task_selected, 0);
}

#[test]
fn k056_arrow_keys_navigate_like_j_and_k() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.task_selected, 1);
    harness.press(KeyCode::Up);
    assert_eq!(harness.app.state.task_selected, 0);
}

// ── Agents view ────────────────────────────────────────────────────────────

#[test]
fn k057_n_opens_the_new_agent_wizard() {
    let mut harness = agents_harness();
    harness.key('n');
    harness.assert_text("NEW AGENT");
    harness.assert_text("Agent name");
}

#[test]
fn k058_e_opens_the_edit_agent_wizard() {
    let mut harness = agents_harness();
    harness.key('e');
    harness.assert_text("EDIT AGENT");
}

#[test]
fn k059_s_force_stops_a_running_agent() {
    let mut harness = agents_harness();
    // beta is running and sorts first by status.
    harness.app.state.agent_selected = 0;
    harness.key('s');
    harness.assert_called(|c| matches!(c, Call::ForceStopAgent(id) if id == "agt_2"));
}

#[test]
fn k060_s_refuses_an_agent_that_is_not_running() {
    let mut harness = agents_harness();
    harness.app.state.agent_selected = 1;
    harness.key('s');
    harness.refute_called(|c| matches!(c, Call::ForceStopAgent(_)));
    harness.assert_message("Agent \"alpha\" is not running");
}

#[test]
fn k061_d_soft_deletes_the_selected_agent() {
    let mut harness = agents_harness();
    harness.app.state.agent_selected = 1;
    harness.key('d');
    assert_eq!(harness.app.state.pending_deletions.len(), 1);
    assert!(!harness.app.state.pending_deletions[0].needs_force_stop);
}

#[test]
fn k062_d_on_a_running_agent_carries_needs_force_stop() {
    let mut harness = agents_harness();
    harness.app.state.agent_selected = 0;
    harness.key('d');
    assert!(harness.app.state.pending_deletions[0].needs_force_stop);
}

#[test]
fn k063_u_toggles_autonomous_mode() {
    let mut harness = agents_harness();
    harness.app.state.agent_selected = 1;
    harness.key('u');
    harness.assert_called(
        |c| matches!(c, Call::ToggleAutonomous { id, enabled } if id == "agt_1" && *enabled),
    );
    harness.assert_message("⟳ alpha autonomous ON");
}

#[test]
fn k064_enter_opens_the_agent_detail_panel() {
    let mut harness = agents_harness();
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    harness.assert_text("AGENT");
}

#[test]
fn k065_enter_on_the_add_agent_row_opens_the_wizard() {
    let mut harness = agents_harness();
    harness.app.state.agent_selected = 2;
    harness.press(KeyCode::Enter);
    harness.assert_text("NEW AGENT");
}

// ── Goals view ─────────────────────────────────────────────────────────────

#[test]
fn k066_n_opens_the_new_goal_wizard() {
    let mut harness = goals_harness();
    harness.key('n');
    harness.assert_text("New Goal");
    harness.assert_text("Goal title");
}

#[test]
fn k067_e_opens_the_edit_goal_wizard() {
    let mut harness = goals_harness();
    harness.key('e');
    harness.assert_text("Edit Goal: Ship v2");
}

#[test]
fn k068_d_soft_deletes_the_selected_goal() {
    let mut harness = goals_harness();
    harness.key('d');
    assert_eq!(harness.app.state.pending_deletions.len(), 1);
    harness.refute_called(|c| matches!(c, Call::DeleteGoal(_)));
}

#[test]
fn k069_c_achieves_the_goal_with_force() {
    let mut harness = goals_harness();
    harness.key('c');
    harness.assert_called(|c| {
        matches!(
            c,
            Call::UpdateGoalStatus { id, status, force }
                if id == "goal_1" && *status == GoalStatus::Achieved && *force
        )
    });
}

#[test]
fn k070_x_abandons_the_goal_without_force() {
    let mut harness = goals_harness();
    harness.key('x');
    harness.assert_called(|c| {
        matches!(
            c,
            Call::UpdateGoalStatus { id, status, force }
                if id == "goal_1" && *status == GoalStatus::Abandoned && !*force
        )
    });
}

#[test]
fn k071_p_pauses_an_active_goal() {
    let mut harness = goals_harness();
    harness.key('p');
    harness.assert_called(
        |c| matches!(c, Call::UpdateGoalStatus { status, .. } if *status == GoalStatus::Paused),
    );
}

#[test]
fn k072_p_resumes_a_paused_goal() {
    let mut harness = goals_harness();
    harness.app.state.goal_selected = 1;
    harness.key('p');
    harness.assert_called(
        |c| matches!(c, Call::UpdateGoalStatus { status, .. } if *status == GoalStatus::Active),
    );
}

#[test]
fn k073_the_goal_transitions_are_refused_for_terminal_goals() {
    let backend =
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "Done", GoalStatus::Achieved)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Goals;
    harness.key('c');
    harness.key('x');
    harness.key('p');
    harness.refute_called(|c| matches!(c, Call::UpdateGoalStatus { .. }));
}

#[test]
fn k074_enter_opens_the_goal_detail_panel() {
    let mut harness = goals_harness();
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    harness.assert_text("GOAL: Ship v2");
}

#[test]
fn k075_enter_on_the_add_goal_row_opens_the_wizard() {
    let mut harness = goals_harness();
    harness.app.state.goal_selected = 2;
    harness.press(KeyCode::Enter);
    harness.assert_text("New Goal");
}

#[test]
fn k076_up_and_down_scroll_the_open_goal_detail_panel() {
    let mut harness = goals_harness();
    harness.press(KeyCode::Enter);
    harness.key('j');
    assert_eq!(harness.app.state.goal_detail_scroll, 1);
    harness.key('k');
    assert_eq!(harness.app.state.goal_detail_scroll, 0);
    assert_eq!(
        harness.app.state.goal_selected, 0,
        "the list selection does not move while the panel is open"
    );
}

// ── Logs view ──────────────────────────────────────────────────────────────

#[test]
fn k077_a_opens_the_agent_filter_picker() {
    let mut harness = logs_harness();
    harness.key('a');
    assert!(harness.app.state.show_agent_picker);
    harness.assert_text("Agent Filter");
}

#[test]
fn k078_f_opens_the_type_filter_picker() {
    let mut harness = logs_harness();
    harness.key('f');
    assert!(harness.app.state.show_type_picker);
    harness.assert_text("Type Filter");
}

#[test]
fn k079_uppercase_f_cycles_the_log_type_preset() {
    let mut harness = logs_harness();
    harness.key('F');
    assert_eq!(
        orch_tui::messages::type_filter_label(&harness.app.state.log_type_filter),
        "text"
    );
    harness.key('F');
    assert_eq!(
        orch_tui::messages::type_filter_label(&harness.app.state.log_type_filter),
        "tools"
    );
}

#[test]
fn k080_the_agent_picker_swallows_global_keys() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.key('q');
    assert!(!harness.app.state.should_exit);
}

#[test]
fn k081_the_type_picker_also_swallows_global_keys() {
    // PARITY.md §14.8 — fixed: in the original `q` quit through this picker.
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('q');
    assert!(!harness.app.state.should_exit);
    assert!(harness.app.state.show_type_picker);
}

#[test]
fn k082_up_from_tail_jumps_to_the_last_message() {
    let mut harness = logs_harness();
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    let count = harness.app.state.messages.len();
    harness.press(KeyCode::Up);
    assert_eq!(harness.app.state.log_selected_index, count as i64 - 1);
}

#[test]
fn k083_down_at_tail_stays_at_tail() {
    let mut harness = logs_harness();
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.log_selected_index, -1);
}

#[test]
fn k084_down_past_the_last_index_returns_to_tail() {
    let mut harness = logs_harness();
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    harness.press(KeyCode::Up);
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.log_selected_index, -1);
    assert_eq!(harness.app.state.log_scroll_offset, 0);
}

#[test]
fn k085_enter_toggles_the_log_detail_panel_only_with_a_selection() {
    let mut harness = logs_harness();
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::None);
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    harness.press(KeyCode::Up);
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::Log);
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.detail, DetailPanel::None);
}

// ── Activity filter ────────────────────────────────────────────────────────

#[test]
fn k086_f_cycles_the_activity_filter_on_the_entity_views() {
    let mut harness = tasks_harness();
    harness.key('f');
    assert_eq!(
        orch_tui::messages::preset_label(&harness.app.state.activity_filter),
        "text"
    );
    harness.assert_called(|c| matches!(c, Call::SaveActivityFilter(p) if p == "text"));
}

#[test]
fn k087_uppercase_f_also_cycles_the_activity_filter() {
    let mut harness = agents_harness();
    harness.key('F');
    assert_eq!(
        orch_tui::messages::preset_label(&harness.app.state.activity_filter),
        "text"
    );
}

#[test]
fn k088_the_activity_filter_is_blocked_while_the_detail_panel_is_open() {
    let mut harness = tasks_harness();
    harness.press(KeyCode::Enter);
    harness.key('f');
    assert_eq!(
        orch_tui::messages::preset_label(&harness.app.state.activity_filter),
        "all"
    );
}

// ── Wizard mode ────────────────────────────────────────────────────────────

#[test]
fn k089_a_wizard_swallows_global_hotkeys_including_q() {
    let mut harness = tasks_harness();
    harness.key('n');
    harness.key('q');
    assert!(!harness.app.state.should_exit);
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().text_value(),
        "q",
        "the key was typed into the field"
    );
}

#[test]
fn k090_ctrl_s_in_the_agent_wizard_opens_the_agent_shop() {
    let mut harness = agents_harness();
    harness.key('n');
    harness.press_mod(KeyCode::Char('s'), KeyModifiers::CONTROL);
    harness.assert_text("AGENT SHOP");
}

#[test]
fn k091_ctrl_s_outside_the_agent_wizard_does_not_open_the_shop() {
    let mut harness = tasks_harness();
    harness.key('n');
    harness.press_mod(KeyCode::Char('s'), KeyModifiers::CONTROL);
    harness.refute_text("AGENT SHOP");
}

#[test]
fn k092_the_command_bar_consumes_navigation_keys() {
    let mut harness = tasks_harness();
    harness.key('/');
    harness.type_str("task");
    assert_eq!(harness.app.state.command_bar.input.value(), "/task");
    assert_eq!(harness.app.state.view, ViewId::Tasks);
}

#[test]
fn k093_ctrl_s_is_refused_when_the_default_adapter_is_shell() {
    // The Agent Shop needs an AI provider; the shortcut must refuse rather
    // than open an unusable wizard (docs/parity/tui.md §5.2).
    let mut backend = FakeBackend::new();
    backend.context.default_adapter = "shell".into();
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Agents;
    harness.key('n');
    harness.press_mod(KeyCode::Char('s'), KeyModifiers::CONTROL);
    harness.refute_text("AGENT SHOP");
    harness.assert_message("Agent Shop requires an AI provider. Press N to create a Shell agent.");
}

#[test]
fn k094_a_name_step_template_suggestion_is_refused_for_a_shell_default() {
    // `handleSuggestionSelected` guards before resolving the template, with a
    // different string from the `/agent shop` refusal (App.tsx:1320-1323).
    let mut backend = FakeBackend::new();
    backend.context.default_adapter = "shell".into();
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Agents;
    harness.key('n');
    // Browse the shop templates offered on the name step and pick the first.
    harness.press(KeyCode::Down);
    harness.press(KeyCode::Enter);
    harness.assert_message(
        "Agent Shop requires an AI provider. Choose Shell and enter a command instead.",
    );
    harness.refute_text("NEW AGENT — Backend Developer");
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().kind,
        orch_tui::wizard::WizardKind::Agent,
        "the open wizard is untouched"
    );
}

#[test]
fn k095_a_name_step_template_suggestion_opens_the_prefilled_wizard_otherwise() {
    let mut harness = Harness::with(FakeBackend::new());
    harness.app.state.view = ViewId::Agents;
    harness.key('n');
    harness.press(KeyCode::Down);
    harness.press(KeyCode::Enter);
    harness.assert_text("NEW AGENT — Backend Developer");
    harness.refute_message("Agent Shop requires an AI provider");
}
