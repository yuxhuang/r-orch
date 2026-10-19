//! Counted mouse cases, numbered `m0NN` to match `keymap.rs`'s `k0NN`.
//!
//! Three kinds, in order: what every clickable target does, what every
//! deliberately-unclickable surface must *not* do, and the hazards — identity
//! pairing, reflow, modal swallowing, the wheel cap, arming.
//!
//! Clicks are placed with `click_text`, never with hard-coded coordinates, so
//! these read as "click the `R run` hint" and survive a layout change.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, TaskStatus};
use orch_tui::backend::{Call, FakeBackend, make_agent, make_goal, make_task};
use orch_tui::hit::ZoneKind;
use orch_tui::messages::{Message, MsgType};
use orch_tui::mouse::{DOUBLE_CLICK_MS, QUIT_ARM_MS, WHEEL_MAX_STEPS_PER_FRAME};
use orch_tui::state::{DetailPanel, InputMode, ViewId};
use orch_tui::theme::Token;
use ratatui::crossterm::event::KeyCode;

// Resolve the semantic control: I0 uses Enter/Tab, while B2 renders Tab OK.
fn click_wizard_confirm(harness: &mut Harness) {
    let rect = harness
        .hits()
        .zones()
        .iter()
        .find(|z| z.kind == ZoneKind::Verb(orch_tui::hit::VerbId::WizardConfirm))
        .expect("visible wizard confirm control")
        .rect;
    harness.click(rect.x, rect.y);
}

fn tasks(n: usize) -> Vec<orch_domain::Task> {
    (0..n)
        .map(|i| {
            make_task(
                &format!("tsk_{i}"),
                &format!("Task number {i}"),
                TaskStatus::Todo,
            )
        })
        .collect()
}

fn agents(n: usize) -> Vec<orch_domain::Agent> {
    (0..n)
        .map(|i| make_agent(&format!("agt_{i}"), &format!("agent{i}"), AgentStatus::Idle))
        .collect()
}

fn goals(n: usize) -> Vec<orch_domain::Goal> {
    (0..n)
        .map(|i| {
            make_goal(
                &format!("gol_{i}"),
                &format!("Goal number {i}"),
                GoalStatus::Active,
            )
        })
        .collect()
}

fn loaded() -> FakeBackend {
    FakeBackend::new()
        .with_tasks(tasks(4))
        .with_agents(agents(3))
        .with_goals(goals(3))
}

fn view(view: ViewId) -> Harness {
    let mut harness = Harness::with(loaded());
    harness.app.state.view = view;
    harness.draw();
    harness
}

fn logs_with_messages() -> Harness {
    let mut harness = Harness::with(loaded());
    harness.app.state.view = ViewId::Logs;
    harness.app.state.messages = (0..40)
        .map(|i| {
            let (token, msg_type) = if i % 2 == 0 {
                (Token::White, MsgType::Output)
            } else {
                (Token::Red, MsgType::Error)
            };
            Message::new(i as u64 + 1, 0, &format!("msg-{i:02}"), token, msg_type)
        })
        .collect();
    harness.draw();
    harness
}

// ── Tabs ───────────────────────────────────────────────────────────────────

#[test]
fn m001_clicking_each_tab_pill_switches_to_that_view() {
    for (label, expected) in [
        ("agents", ViewId::Agents),
        ("actions", ViewId::Logs),
        ("goals", ViewId::Goals),
        ("tasks", ViewId::Tasks),
    ] {
        let mut harness = view(ViewId::Tasks);
        if expected == ViewId::Tasks {
            harness.app.state.view = ViewId::Goals;
            harness.draw();
        }
        harness.click_text(label);
        assert_eq!(harness.app.state.view, expected, "clicking {label:?}");
    }
}

#[test]
fn m002_a_tab_pill_is_found_whatever_the_project_name_width() {
    for name in ["p", "a-project-with-a-rather-long-name-indeed"] {
        let mut harness = view(ViewId::Tasks);
        harness.app.state.project_name = name.to_string();
        harness.draw();
        harness.click_text("agents");
        assert_eq!(
            harness.app.state.view,
            ViewId::Agents,
            "the pill moved with the brand bar but the zone followed it ({name})"
        );
    }
}

// ── Rows ───────────────────────────────────────────────────────────────────

#[test]
fn m003_clicking_a_task_row_selects_it() {
    let mut harness = view(ViewId::Tasks);
    harness.click_text("Task number 2");
    let selected = harness.app.state.selected_task().expect("a selection");
    assert_eq!(selected.title, "Task number 2");
}

#[test]
fn m004_double_clicking_a_task_row_opens_its_detail() {
    let mut harness = view(ViewId::Tasks);
    harness.double_click_text("Task number 1");
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    assert_eq!(
        harness.app.state.selected_task().unwrap().title,
        "Task number 1"
    );
}

#[test]
fn m005_clicking_an_agent_row_selects_it_and_a_double_click_opens_the_detail() {
    let mut harness = view(ViewId::Agents);
    harness.click_text("agent1");
    assert_eq!(harness.app.state.selected_agent().unwrap().name, "agent1");
    assert_eq!(harness.app.state.detail, DetailPanel::None);

    let mut harness = view(ViewId::Agents);
    harness.double_click_text("agent2");
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
}

#[test]
fn m006_clicking_a_goal_row_selects_it_and_a_double_click_opens_the_detail() {
    let mut harness = view(ViewId::Goals);
    harness.click_text("Goal number 1");
    assert_eq!(harness.app.state.goal_selected, 1);
    assert_eq!(harness.app.state.detail, DetailPanel::None);

    let mut harness = view(ViewId::Goals);
    harness.double_click_text("Goal number 1");
    assert!(harness.app.state.detail_open());
}

#[test]
fn m007_clicking_a_log_row_selects_the_message_drawn_on_it() {
    let mut harness = logs_with_messages();
    harness.click_text("msg-37");
    let index = harness.app.state.log_selected_index;
    assert!(index >= 0, "a row click leaves tail mode");
    let filtered = harness.app.state.filtered_messages();
    let message = &harness.app.state.messages[filtered[index as usize]];
    assert_eq!(message.text, "msg-37");
}

#[test]
fn m008_a_log_row_click_under_a_filter_indexes_the_filtered_set() {
    // The selection indexes `filtered_messages`, never `messages`.
    let mut harness = logs_with_messages();
    harness.app.state.log_type_filter = [MsgType::Error].into_iter().collect();
    harness.draw();
    harness.click_text("msg-29");

    let index = harness.app.state.log_selected_index;
    let filtered = harness.app.state.filtered_messages();
    assert_eq!(
        harness.app.state.messages[filtered[index as usize]].text,
        "msg-29"
    );
}

#[test]
fn m009_double_clicking_a_log_row_toggles_the_log_detail() {
    let mut h = logs_with_messages();
    h.click_text("msg-38");
    h.draw();
    h.click_text("msg-38");
    let snapshot = h.app.state.viewer.as_ref().expect("complete snapshot");
    assert!(snapshot.document().body.contains("msg-38"));
    assert_eq!(h.app.state.detail, DetailPanel::None);
}

#[test]
fn m010_a_scrolled_list_selects_the_scrolled_index() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(40)));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.show_all_tasks = true;
    harness.app.state.task_scroll = 5;
    harness.draw();
    harness.click_text("Task number 5");
    assert_eq!(harness.app.state.task_selected, 5);
}

#[test]
fn m011_in_a_grouped_list_the_row_below_a_header_selects_the_task() {
    let mut tasks = tasks(2);
    tasks[0].goal_id = Some("gol_0".into());
    tasks[1].goal_id = Some("gol_0".into());
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks).with_goals(goals(1)));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.group_by_goal = true;
    harness.draw();

    harness.click_text("Task number 1");
    assert_eq!(
        harness.app.state.selected_task().unwrap().title,
        "Task number 1"
    );
}

// ── Pseudo-rows and nudges ─────────────────────────────────────────────────

#[test]
fn m012_clicking_add_task_opens_the_task_wizard() {
    let mut harness = view(ViewId::Tasks);
    harness.click_text("+ add task...");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
}

#[test]
fn m013_clicking_add_agent_opens_the_agent_wizard() {
    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
}

#[test]
fn m014_clicking_add_goal_opens_the_goal_wizard() {
    let mut harness = view(ViewId::Goals);
    harness.click_text("+ add goal...");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
}

#[test]
fn m015_clicking_show_all_expands_the_task_list() {
    // Tall enough for the column header, ten rows and the Show all row.
    let mut harness = Harness::sized(FakeBackend::new().with_tasks(tasks(40)), 120, 44);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    assert!(!harness.app.state.show_all_tasks);
    harness.click_text("Show all");
    assert!(harness.app.state.show_all_tasks);
    assert_eq!(harness.app.state.task_selected, 0);
    assert_eq!(harness.app.state.task_scroll, 0);
}

// ── Logs status bar and chips ──────────────────────────────────────────────

#[test]
fn m016_clicking_the_logs_filter_verbs_opens_the_pickers_and_cycles_types() {
    let mut harness = logs_with_messages();
    harness.click_text("a filter");
    assert!(harness.app.state.show_agent_picker);

    let mut harness = logs_with_messages();
    harness.click_text("f type");
    assert!(harness.app.state.show_type_picker);

    let mut harness = logs_with_messages();
    let before = harness.app.state.log_type_filter.clone();
    harness.click_text("F cycle");
    assert_ne!(harness.app.state.log_type_filter, before);
}

#[test]
fn m017_clicking_the_live_chip_returns_to_tail() {
    let mut harness = logs_with_messages();
    harness.click_text("msg-38");
    assert!(harness.app.state.log_selected_index >= 0);
    harness.draw();
    harness.click_text("↑↓");
    assert_eq!(harness.app.state.log_selected_index, -1);
    assert_eq!(harness.app.state.log_scroll_offset, 0);
}

#[test]
fn m018_clicking_an_agent_chip_toggles_it_in_the_logs_filter() {
    let mut harness = logs_with_messages();
    harness.click_text("agent1");
    assert_eq!(
        harness
            .app
            .state
            .log_agent_filter
            .iter()
            .collect::<Vec<_>>(),
        vec!["agt_1"],
        "from implicit-all, one click is a single explicit selection"
    );
    harness.draw();
    harness.click_text("agent1");
    assert!(
        harness.app.state.log_agent_filter.is_empty(),
        "clicking it again returns to implicit-all"
    );
}

// ── Pickers ────────────────────────────────────────────────────────────────

fn agent_picker() -> Harness {
    let mut harness = logs_with_messages();
    harness.app.state.show_agent_picker = true;
    harness.draw();
    harness
}

#[test]
fn m019_a_picker_checkbox_toggles_and_a_label_moves_the_cursor() {
    let mut harness = agent_picker();
    let (x, y) = harness.find_text("[✓] agent1").expect("row 1");
    harness.click(x + 1, y);
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_1"]
    );

    let mut harness = agent_picker();
    // The logs agent-chip row carries the same names, so target the picker's
    // own row by its checkbox.
    let (x, y) = harness.find_text("[✓] agent2").expect("row 2 label");
    harness.click(x + 4, y);
    assert_eq!(harness.app.state.picker_cursor, 2);
    assert!(
        harness.app.state.picker_draft.is_empty(),
        "a label click moves the cursor only"
    );
}

#[test]
fn m020_a_double_click_on_a_picker_label_toggles_it() {
    let mut harness = agent_picker();
    let (x, y) = harness.find_text("[✓] agent2").expect("row 2 label");
    harness.double_click(x + 4, y);
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_2"]
    );
}

#[test]
fn m021_the_picker_footer_verbs_all_work() {
    let mut harness = agent_picker();
    harness.app.state.picker_cursor = 1;
    harness.draw();
    harness.click_text("Space toggle");
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_1"]
    );
    harness.draw();
    harness.click_text("a all");
    assert!(harness.app.state.picker_draft.is_empty(), "`a all` clears");

    let mut harness = agent_picker();
    harness.app.state.picker_draft = ["agt_2".to_string()].into_iter().collect();
    harness.draw();
    harness.click_text("Enter confirm");
    assert!(!harness.app.state.show_agent_picker);
    assert_eq!(
        harness
            .app
            .state
            .log_agent_filter
            .iter()
            .collect::<Vec<_>>(),
        vec!["agt_2"]
    );

    let mut harness = agent_picker();
    harness.app.state.picker_draft = ["agt_2".to_string()].into_iter().collect();
    harness.draw();
    harness.click_text("Esc cancel");
    assert!(!harness.app.state.show_agent_picker);
    assert!(
        harness.app.state.log_agent_filter.is_empty(),
        "cancel discards the draft"
    );
}

#[test]
fn m022_every_type_picker_preset_sets_its_own_set() {
    for (digit, expected_len) in [('2', 1usize), ('3', 3), ('4', 1), ('5', 2), ('1', 8)] {
        let mut harness = logs_with_messages();
        harness.app.state.show_type_picker = true;
        harness.draw();
        harness.click_text(&format!("{digit}="));
        assert_eq!(
            harness.app.state.type_picker_draft.len(),
            expected_len,
            "preset {digit}"
        );
    }
}

#[test]
fn m023_a_type_picker_row_toggles_from_its_checkbox() {
    let mut harness = logs_with_messages();
    harness.app.state.show_type_picker = true;
    harness.app.state.type_picker_draft.clear();
    harness.draw();
    let (x, y) = harness.find_text("[ ]").expect("an unchecked row");
    harness.click(x + 1, y);
    assert_eq!(harness.app.state.type_picker_draft.len(), 1);
}

#[test]
fn m024_a_picker_drawn_inside_the_logs_area_resolves_the_same_zones() {
    // The agent picker renders either in the bottom panel or inside the logs
    // content area with a derived height (`views/mod.rs:143` vs `:239`).
    // Record-during-draw handles both from one call site.
    let mut harness = agent_picker();
    assert!(
        harness
            .hits()
            .zones()
            .iter()
            .any(|z| matches!(z.kind, ZoneKind::PickerRow { .. })),
        "picker rows are registered wherever the picker is drawn:\n{}",
        harness.text()
    );
    harness.click_text("Esc cancel");
    assert!(!harness.app.state.show_agent_picker);
}

// ── Footer hints ───────────────────────────────────────────────────────────

fn wide() -> Harness {
    // Wide enough that every conditional hint fits in the footer.
    let mut harness = Harness::sized(loaded(), 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    harness
}

#[test]
fn m025_clicking_slash_cmd_enters_command_mode() {
    let mut harness = wide();
    harness.click_text("/ cmd");
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(harness.app.state.command_bar.input.value(), "/");
}

#[test]
fn m026_clicking_n_new_opens_this_views_wizard() {
    let mut harness = wide();
    harness.click_text("N new");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
}

#[test]
fn m027_clicking_r_run_runs_the_selected_task() {
    let mut harness = wide();
    harness.click_text("R run");
    harness.assert_called(|c| matches!(c, Call::RunTask(id) if id == "tsk_0"));
}

#[test]
fn m028_clicking_c_cancel_cancels_the_selected_task() {
    let mut harness = wide();
    harness.click_text("C cancel");
    harness.assert_called(|c| matches!(c, Call::CancelTask(id) if id == "tsk_0"));
}

#[test]
fn m029_clicking_a_approve_and_x_reject_act_on_a_review_task() {
    let mut backend = loaded();
    backend.tasks[0].status = TaskStatus::Review;
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    harness.click_text("A approve");
    harness.assert_called(|c| matches!(c, Call::ApproveTask(id) if id == "tsk_0"));

    let mut backend = loaded();
    backend.tasks[0].status = TaskStatus::Review;
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    harness.click_text("X reject");
    harness.assert_called(|c| matches!(c, Call::RejectTask { id, .. } if id == "tsk_0"));
}

#[test]
fn m030_clicking_e_edit_opens_the_edit_wizard_for_the_view() {
    let mut harness = wide();
    harness.click_text("E edit");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
}

#[test]
fn m031_clicking_s_stop_force_stops_the_selected_agent() {
    let mut backend = loaded();
    backend.agents[0].status = AgentStatus::Running;
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Agents;
    harness.draw();
    harness.click_text("S stop");
    harness.assert_called(|c| matches!(c, Call::ForceStopAgent(id) if id == "agt_0"));
}

#[test]
fn m032_clicking_u_toggles_the_selected_agents_autonomous_flag() {
    let mut harness = Harness::sized(loaded(), 140, 30);
    harness.app.state.view = ViewId::Agents;
    harness.draw();
    harness.click_text("auto on");
    harness.assert_called(|c| matches!(c, Call::ToggleAutonomous { id, .. } if id == "agt_0"));
}

#[test]
fn m033_clicking_p_pauses_and_resumes_the_selected_goal() {
    let mut harness = Harness::sized(loaded(), 140, 30);
    harness.app.state.view = ViewId::Goals;
    harness.draw();
    harness.click_text("P pause");
    harness.assert_called(|c| matches!(c, Call::UpdateGoalStatus { id, .. } if id == "gol_0"));
}

#[test]
fn m034_clicking_s_show_all_toggles_the_task_list() {
    let mut harness = wide();
    harness.click_text("S show all");
    assert!(harness.app.state.show_all_tasks);
}

#[test]
fn m035_clicking_d_delete_schedules_a_soft_delete() {
    let mut harness = wide();
    harness.click_text("D delete");
    assert_eq!(harness.app.state.pending_deletions.len(), 1);
    harness.refute_called(|c| matches!(c, Call::DeleteTask(_)));
}

#[test]
fn m036_clicking_z_undo_restores_the_last_deletion() {
    let mut harness = wide();
    harness.click_text("D delete");
    harness.draw();
    harness.click_text("Z undo");
    assert!(harness.app.state.pending_deletions.is_empty());
}

#[test]
fn m037_clicking_the_undo_banner_row_restores_that_row() {
    let mut harness = wide();
    for id in ["tsk_0", "tsk_1", "tsk_2"] {
        harness.app.delete_task_by_id(id);
    }
    harness.draw();
    // Row 0 of the banner names "Task number 0", not the most recent.
    harness.click_text("\"Task number 0\"");
    let left: Vec<&str> = harness
        .app
        .state
        .pending_deletions
        .iter()
        .map(|d| d.entity_name.as_str())
        .collect();
    assert_eq!(left, ["Task number 1", "Task number 2"]);
}

#[test]
fn m038_clicking_enter_detail_and_esc_close_open_and_close_the_panel() {
    let mut harness = wide();
    harness.click_text("Enter detail");
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    // The row it was clicked in is suppressed for a double-click window
    // (§3.3.3), so a second footer click has to wait that out.
    harness.clock.advance_ms(DOUBLE_CLICK_MS + 1);
    harness.draw();
    harness.click_text("Esc to list");
    assert_eq!(harness.app.state.detail, DetailPanel::None);
}

#[test]
fn m039_clicking_question_help_opens_the_overlay() {
    let mut harness = wide();
    harness.click_text("? help");
    assert!(harness.app.state.show_help);
}

// ── Detail resize verbs ────────────────────────────────────────────────────

#[test]
fn m040_the_detail_resize_verbs_grow_shrink_and_maximize() {
    let mut harness = wide();
    harness.app.state.detail = DetailPanel::Entity;
    harness.draw();
    let before = harness.app.state.split_offset;
    harness.click_text("+/-");
    assert_ne!(
        harness.app.state.split_offset, before,
        "`+` grows the detail"
    );

    let mut harness = wide();
    harness.app.state.detail = DetailPanel::Entity;
    harness.draw();
    let (x, y) = harness.find_text("+/- resize").expect("the hint");
    harness.click(x + 2, y);
    assert!(
        harness.app.state.split_offset > 0,
        "`-` grows the list instead"
    );

    let mut harness = wide();
    harness.app.state.detail = DetailPanel::Entity;
    harness.draw();
    let (x, y) = harness.find_text("M max").expect("the max hint");
    harness.click(x, y);
    assert!(harness.app.state.detail_maximized);
}

// ── Activity filter ────────────────────────────────────────────────────────

#[test]
fn m041_clicking_the_activity_filter_suffix_cycles_the_preset() {
    let mut harness = wide();
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    harness.draw();
    let before = harness.app.state.activity_filter.clone();
    harness.click_text("F:ALL");
    assert_ne!(harness.app.state.activity_filter, before);
    harness.assert_called(|c| matches!(c, Call::SaveActivityFilter(_)));
}

// ── Command mode ───────────────────────────────────────────────────────────

fn command_mode() -> Harness {
    let mut harness = wide();
    harness.app.state.input_mode = InputMode::Command;
    harness.app.state.command_bar.input.reset("/t");
    harness.draw();
    harness
}

#[test]
fn m042_clicking_a_suggestion_row_selects_it_and_a_double_click_runs_it() {
    let mut harness = command_mode();
    let rows = harness
        .hits()
        .zones()
        .iter()
        .filter(|z| matches!(z.kind, ZoneKind::SuggestionRow { .. }))
        .count();
    assert!(rows > 1, "the panel lists several suggestions");

    let zone = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match z.kind {
            ZoneKind::SuggestionRow { index: 1 } => Some(z.rect),
            _ => None,
        })
        .next()
        .expect("a second suggestion row");
    harness.click(zone.x + 6, zone.y);
    assert_eq!(harness.app.state.command_bar.suggestion_index, 1);
}

#[test]
fn m043_clicking_esc_in_command_mode_leaves_it() {
    let mut harness = command_mode();
    harness.click_text("Esc ✕");
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert_eq!(harness.app.state.command_bar.input.value(), "");
}

// ── Help overlay ───────────────────────────────────────────────────────────

#[test]
fn m044_a_click_inside_the_help_overlay_leaves_it_open() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.show_help = true;
    harness.draw();
    let selected = harness.app.state.task_selected;
    harness.click_text("NAVIGATION");
    assert!(
        harness.app.state.show_help,
        "a click on the box itself does nothing"
    );
    assert_eq!(harness.app.state.task_selected, selected);
}

#[test]
fn m045_a_click_outside_the_help_overlay_dismisses_it_and_nothing_else() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.show_help = true;
    harness.draw();
    let selected = harness.app.state.task_selected;
    let view_before = harness.app.state.view;

    let y = harness.app.state.height / 2;
    harness.click(0, y);
    assert!(!harness.app.state.show_help, "the margin dismisses");
    assert_eq!(
        harness.app.state.task_selected, selected,
        "and performs no action of its own"
    );
    assert_eq!(harness.app.state.view, view_before);
}

// ── Negative cases: deliberately not clickable ─────────────────────────────

#[test]
fn m046_the_header_chrome_is_not_clickable() {
    for needle in ["ORCH", "IDLE"] {
        let mut harness = view(ViewId::Tasks);
        let before = (
            harness.app.state.view,
            harness.app.state.task_selected,
            harness.app.state.detail,
        );
        harness.click_text(needle);
        assert_eq!(
            (
                harness.app.state.view,
                harness.app.state.task_selected,
                harness.app.state.detail
            ),
            before,
            "{needle} is a status display, not a control"
        );
    }
}

#[test]
fn m047_a_section_header_row_is_not_clickable() {
    let mut tasks = tasks(2);
    tasks[0].goal_id = Some("gol_0".into());
    tasks[1].goal_id = Some("gol_0".into());
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks).with_goals(goals(1)));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.group_by_goal = true;
    harness.app.state.task_selected = 1;
    harness.draw();

    harness.click_text("GOAL NUMBER 0");
    assert_eq!(
        harness.app.state.task_selected, 1,
        "a header consumes no selection index"
    );
}

#[test]
fn m048_the_arrow_hints_are_not_clickable() {
    let mut harness = wide();
    let before = harness.app.state.task_selected;
    harness.click_text("Tab panes; ←→ views");
    assert_eq!(harness.app.state.view, ViewId::Tasks, "no tab switch");
    assert_eq!(harness.app.state.task_selected, before);
}

#[test]
fn m049_the_activity_feed_rows_are_not_clickable() {
    let mut harness = wide();
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    harness.draw();
    let before = harness.app.state.task_selected;
    let (_, y) = harness.find_text("ACTIVITY").expect("the feed");
    harness.click(20, y + 2);
    assert_eq!(harness.app.state.task_selected, before);
    assert_eq!(harness.app.state.detail, DetailPanel::None);
}

#[test]
fn m050_blank_space_below_the_last_row_does_nothing() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(2)));
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    let before = harness.app.state.task_selected;
    // Three rows below the last task is inside the list region but on no row.
    let (_, y) = harness.find_text("Task number 1").expect("the last task");
    harness.click(20, y + 3);
    assert_eq!(harness.app.state.task_selected, before);
    assert_eq!(harness.app.state.detail, DetailPanel::None);
}

#[test]
fn m051_the_detail_body_is_not_clickable() {
    let mut harness = wide();
    harness.app.state.detail = DetailPanel::Entity;
    harness.draw();
    let before = harness.app.state.detail;
    let (_, y) = harness.find_text("DETAIL").expect("the detail header");
    harness.click(30, y + 3);
    assert_eq!(harness.app.state.detail, before, "read-only content");
}

#[test]
fn m052_a_toast_is_not_clickable() {
    let mut harness = wide();
    harness
        .app
        .state
        .push_toast(orch_tui::state::ToastKind::Done, "Title", "Body", 0);
    harness.draw();
    let before = harness.app.state.toasts.len();
    harness.click_text("Title");
    assert_eq!(
        harness.app.state.toasts.len(),
        before,
        "toasts expire on their own clock; dismissing would be a new mutation"
    );
}

#[test]
fn m053_the_wizard_progress_dots_are_not_clickable() {
    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    harness.draw();
    let step = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();
    let (x, y) = harness.find_text("step 1/").expect("the step counter");
    harness.click(x, y);
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        step,
        "the engine exposes no safe jump-to-step"
    );
}

#[test]
fn m054_right_and_middle_clicks_do_nothing() {
    let mut harness = view(ViewId::Tasks);
    let (x, y) = harness.find_text("Task number 2").expect("a row");
    let before = harness.app.state.task_selected;
    harness.right_click(x, y);
    harness.middle_click(x, y);
    assert_eq!(harness.app.state.task_selected, before);
}

#[test]
fn m055_horizontal_scrolling_changes_nothing() {
    let mut harness = view(ViewId::Tasks);
    let before = (
        harness.app.state.task_selected,
        harness.app.state.task_scroll,
    );
    let (x, y) = harness.find_text("Task number 1").expect("a row");
    harness.scroll_left(x, y).scroll_right(x, y);
    assert_eq!(
        (
            harness.app.state.task_selected,
            harness.app.state.task_scroll
        ),
        before,
        "no horizontal scroll state exists in AppState"
    );
}

#[test]
fn m056_a_drag_event_does_nothing() {
    let mut harness = view(ViewId::Tasks);
    let before = harness.app.state.task_selected;
    let (x, y) = harness.find_text("Task number 1").expect("a row");
    harness.press_at(x, y);
    harness.drag_to(x, y + 1);
    assert_eq!(harness.app.state.task_selected, before, "1002 is never on");
}

// ── Hazards ────────────────────────────────────────────────────────────────

#[test]
fn m057_a_press_and_a_release_on_different_targets_do_nothing() {
    let mut harness = view(ViewId::Tasks);
    let before = harness.app.state.task_selected;
    let (ax, ay) = harness.find_text("Task number 1").expect("row a");
    let (bx, by) = harness.find_text("Task number 2").expect("row b");
    harness.press_at(ax, ay);
    harness.release_at(bx, by);
    assert_eq!(
        harness.app.state.task_selected, before,
        "dragging off a target cancels the click"
    );
}

#[test]
fn m058_a_right_press_then_a_left_release_does_nothing() {
    // In X10 encoding a release carries no button identity and crossterm
    // reports `Left` when it does not know. Requiring an armed `Down(Left)`
    // closes that hole.
    let mut harness = view(ViewId::Tasks);
    let before = harness.app.state.task_selected;
    let (x, y) = harness.find_text("Task number 2").expect("a row");
    harness.press_right_at(x, y);
    harness.release_at(x, y);
    assert_eq!(harness.app.state.task_selected, before);
}

#[test]
fn m059_a_row_that_changes_identity_between_press_and_release_cancels() {
    let mut harness = view(ViewId::Tasks);
    let before = harness.app.state.task_selected;
    let (x, y) = harness.find_text("Task number 2").expect("a row");
    harness.press_at(x, y);
    // A refresh reorders the list under the pointer.
    harness.app.state.tasks.reverse();
    harness.draw();
    harness.release_at(x, y);
    assert_eq!(
        harness.app.state.task_selected, before,
        "a refresh mid-click must cancel, not fire on whatever moved in"
    );
}

#[test]
fn m060_a_third_click_is_a_fresh_primary_not_a_toggle_closed() {
    let mut harness = view(ViewId::Tasks);
    harness.double_click_text("Task number 1");
    assert_eq!(harness.app.state.detail, DetailPanel::Entity);
    harness.draw();
    let (x, y) = harness.find_text("Task number 1").expect("the row");
    harness.click(x, y);
    assert_eq!(
        harness.app.state.detail,
        DetailPanel::Entity,
        "the third click re-selects the row; it does not close the detail"
    );
}

#[test]
fn m061_two_clicks_outside_the_double_click_window_are_two_primaries() {
    let mut harness = view(ViewId::Tasks);
    let (x, y) = harness.find_text("Task number 1").expect("a row");
    harness.click(x, y);
    harness.clock.advance_ms(DOUBLE_CLICK_MS + 1);
    harness.click(x, y);
    assert_eq!(
        harness.app.state.detail,
        DetailPanel::None,
        "{DOUBLE_CLICK_MS}ms apart is not a double click"
    );
}

#[test]
fn m062_a_footer_action_suppresses_the_whole_row_for_a_double_click_window() {
    let mut backend = loaded();
    backend.tasks[0].status = TaskStatus::Review;
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();

    // Approving makes `A`/`X` vanish and everything right of them shift left.
    let (x, y) = harness.find_text("A approve").expect("the approve hint");
    harness.click(x, y);
    harness.assert_called(|c| matches!(c, Call::ApproveTask(_)));

    // A habitual second click on the same cell must hit nothing at all.
    harness.draw();
    harness.click(x, y);
    harness.refute_called(|c| matches!(c, Call::RejectTask { .. }));
    harness.refute_called(|c| matches!(c, Call::DeleteTask(_)));
    assert!(harness.app.state.pending_deletions.is_empty());

    // Once the window passes, the row is live again.
    harness.clock.advance_ms(DOUBLE_CLICK_MS + 1);
    harness.draw();
    harness.click_text("? help");
    assert!(harness.app.state.show_help);
}

#[test]
fn m063_a_tab_pill_click_with_a_detail_open_closes_it_and_switches() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.detail = DetailPanel::Entity;
    harness.draw();
    harness.click_text("agents");
    assert_eq!(harness.app.state.detail, DetailPanel::None);
    assert_eq!(harness.app.state.view, ViewId::Agents);
}

#[test]
fn m064_quit_arms_on_the_first_click_and_exits_on_the_second() {
    let mut harness = wide();
    harness.click_text("Q quit");
    assert!(!harness.app.state.should_exit, "one click must not exit");
    assert!(harness.app.quit_armed());

    harness.draw();
    harness.click_text("Q quit?");
    assert!(harness.app.state.should_exit);
}

#[test]
fn m065_a_quit_click_after_the_window_re_arms_instead_of_exiting() {
    let mut harness = wide();
    harness.click_text("Q quit");
    harness.clock.advance_ms(QUIT_ARM_MS + 1);
    assert!(!harness.app.quit_armed());
    harness.draw();
    harness.click_text("Q quit");
    assert!(
        !harness.app.state.should_exit,
        "the arming expired, so this is a fresh first click"
    );
    assert!(harness.app.quit_armed());
}

#[test]
fn m066_quit_arming_survives_two_queued_toasts() {
    // The arming is rendered in the footer precisely because at most two
    // toasts are ever drawn, so a toast could be invisible.
    let mut harness = wide();
    for i in 0..2 {
        harness
            .app
            .state
            .push_toast(orch_tui::state::ToastKind::Done, &format!("T{i}"), "b", 0);
    }
    harness.draw();
    harness.click_text("Q quit");
    assert!(!harness.app.state.should_exit);
    harness.draw();
    harness.assert_text("Q quit?");
    harness.click_text("Q quit?");
    assert!(harness.app.state.should_exit);
}

// ── Modal swallowing ───────────────────────────────────────────────────────

#[test]
fn m067_an_open_picker_swallows_a_tab_pill_click() {
    let mut harness = agent_picker();
    harness.app.state.picker_draft = ["agt_1".to_string()].into_iter().collect();
    harness.draw();
    harness.click_text("goals");
    assert!(harness.app.state.show_agent_picker, "the picker stays open");
    assert_eq!(
        harness.app.state.view,
        ViewId::Logs,
        "the view is unchanged"
    );
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_1"],
        "and the draft is untouched"
    );
}

#[test]
fn m068_an_open_wizard_swallows_a_task_row_click() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.task_selected = 2;
    harness.click_text("+ add task...");
    harness.draw();
    harness.click_text("Task number 0");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    assert_eq!(harness.app.state.task_selected, 2);
}

#[test]
fn m069_command_mode_swallows_a_task_row_click() {
    let mut harness = command_mode();
    let selected = harness.app.state.task_selected;
    harness.click_text("Task number 1");
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(harness.app.state.command_bar.input.value(), "/t");
    assert_eq!(harness.app.state.task_selected, selected);
}

#[test]
fn m070_the_help_overlay_swallows_everything_but_its_own_outside() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.show_help = true;
    harness.draw();
    // A tab pill sits in the header, which the overlay does not cover, so it
    // is "outside": the overlay closes and the pill does **not** fire.
    harness.click_text("agents");
    assert!(!harness.app.state.show_help);
    assert_eq!(
        harness.app.state.view,
        ViewId::Tasks,
        "the dismissing click performs no action of its own"
    );
}

// ── Map validity ───────────────────────────────────────────────────────────

#[test]
fn m071_a_keystroke_in_the_same_batch_drops_the_click() {
    let mut harness = view(ViewId::Tasks);
    let (x, y) = harness.find_text("Task number 2").expect("a row");
    harness.press(KeyCode::Char('j'));
    let after_key = harness.app.state.task_selected;
    harness.click(x, y);
    assert_eq!(
        harness.app.state.task_selected, after_key,
        "the map is stale until the next draw"
    );
}

#[test]
fn m072_a_click_outside_the_frame_is_dropped() {
    let mut harness = view(ViewId::Tasks);
    let before = harness.app.state.task_selected;
    let (w, h) = (harness.app.state.width, harness.app.state.height);
    harness.click(w, h);
    harness.click(w + 50, 0);
    assert_eq!(harness.app.state.task_selected, before);
}

#[test]
fn m073_every_click_is_a_no_op_below_the_minimum_terminal_size() {
    let mut harness = Harness::sized(loaded(), 30, 10);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    let before = (harness.app.state.view, harness.app.state.task_selected);
    for y in 0..10 {
        for x in 0..30 {
            harness.click(x, y);
        }
    }
    assert_eq!(
        (harness.app.state.view, harness.app.state.task_selected),
        before
    );
}

#[test]
fn m074_with_capture_off_every_mouse_event_is_a_no_op() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.mouse.enabled = false;
    let before = harness.app.state.task_selected;
    let (x, y) = harness.find_text("Task number 2").expect("a row");
    harness.click(x, y);
    harness.wheel_down(x, y);
    harness.double_click(x, y);
    assert_eq!(harness.app.state.task_selected, before);
}

// ── Wheel ──────────────────────────────────────────────────────────────────

#[test]
fn m075_the_wheel_moves_the_selection_and_is_capped_per_frame() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(40)));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.show_all_tasks = true;
    harness.draw();
    let (x, y) = harness.find_text("Task number 1").expect("a row");

    for _ in 0..10 {
        harness.wheel_down(x, y);
    }
    assert_eq!(
        harness.app.state.task_selected, WHEEL_MAX_STEPS_PER_FRAME as usize,
        "a 10-notch flick spends only the frame's budget"
    );

    // The budget resets with the next frame.
    harness.draw();
    harness.wheel_down(x, y);
    assert_eq!(
        harness.app.state.task_selected,
        WHEEL_MAX_STEPS_PER_FRAME as usize + 1
    );
    harness.draw();
    harness.wheel_up(x, y);
    assert_eq!(
        harness.app.state.task_selected,
        WHEEL_MAX_STEPS_PER_FRAME as usize
    );
}

#[test]
fn m076_the_wheel_does_nothing_over_a_task_or_agent_detail() {
    for view_id in [ViewId::Tasks, ViewId::Agents] {
        let mut harness = view(view_id);
        harness.app.state.detail = DetailPanel::Entity;
        harness.draw();
        let before = (
            harness.app.state.task_selected,
            harness.app.state.agent_selected,
            harness.app.state.detail,
        );
        let y = harness.app.state.height / 3;
        harness.wheel_down(20, y);
        harness.wheel_up(20, y);
        assert_eq!(
            (
                harness.app.state.task_selected,
                harness.app.state.agent_selected,
                harness.app.state.detail
            ),
            before,
            "{view_id:?} detail has no scroll state, and changing the entity \
             behind an open panel is the surprise this avoids"
        );
    }
}

#[test]
fn m077_the_wheel_scrolls_an_open_goal_detail_without_moving_the_selection() {
    use orch_tui::state::{Focus, GoalSection};
    let mut h = view(ViewId::Goals);
    h.app.state.goal_selected = 1;
    let id = h.app.state.selected_goal().unwrap().id.clone();
    h.app
        .state
        .goals
        .iter_mut()
        .find(|g| g.id == id)
        .unwrap()
        .description = "line\n".repeat(100);
    h.app.state.detail = DetailPanel::Entity;
    h.app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Description));
    h.draw();
    let key = h.app.state.pane_key(h.app.state.focus);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    h.wheel_down(body.x, body.y);
    assert_eq!(h.app.state.goal_selected, 1);
    assert_eq!(h.app.state.pane_scroll(&key).offset, 3);
}

#[test]
fn m078_the_wheel_does_nothing_over_a_log_detail() {
    use orch_tui::state::{Focus, LogSection};
    let mut h = logs_with_messages();
    h.click_text("msg-38");
    h.key('i');
    let index = h.app.state.filtered_messages()[h.app.state.log_selected_index as usize];
    h.app.state.messages[index].detail = Some("line\n".repeat(100).into());
    h.app.state.set_focus(Focus::LogDetail(LogSection::Body));
    h.draw();
    let before = h.app.state.log_selected_index;
    let key = h.app.state.pane_key(h.app.state.focus);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    h.wheel_down(body.x, body.y);
    assert_eq!(h.app.state.log_selected_index, before);
    assert_eq!(h.app.state.pane_scroll(&key).offset, 3);
}

#[test]
fn m079_the_wheel_clamps_in_a_picker_while_the_keyboard_still_wraps() {
    let mut harness = agent_picker();
    harness.app.state.picker_cursor = 2;
    harness.draw();
    let (x, y) = harness.find_text("agent0").expect("a picker row");

    harness.wheel_down(x, y);
    assert_eq!(
        harness.app.state.picker_cursor, 2,
        "the wheel does not wrap bottom to top"
    );
    // The key path is unchanged and still wraps.
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.picker_cursor, 0);
}

#[test]
fn m080_the_wheel_does_not_walk_command_history_without_suggestions() {
    let mut harness = wide();
    harness.app.state.command_bar.history.push("/status");
    harness.app.state.input_mode = InputMode::Command;
    // A value with no suggestions, so `Up`/`Down` would fall back to history.
    harness.app.state.command_bar.input.reset("/zzzz");
    harness.draw();
    harness.wheel_up(10, harness.app.state.height - 1);
    harness.wheel_down(10, harness.app.state.height - 1);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/zzzz",
        "a wheel must never walk history"
    );
}

#[test]
fn m081_the_wheel_moves_the_suggestion_index_when_there_are_suggestions() {
    let mut harness = command_mode();
    let (_, y) = harness.find_text("COMMANDS").expect("the panel label");
    harness.wheel_down(10, y + 1);
    assert_eq!(harness.app.state.command_bar.suggestion_index, 1);
    harness.draw();
    harness.wheel_up(10, y + 1);
    assert_eq!(harness.app.state.command_bar.suggestion_index, 0);
}

#[test]
fn m082_the_wheel_does_nothing_over_the_header_command_bar_or_overlay() {
    let mut harness = view(ViewId::Tasks);
    let before = harness.app.state.task_selected;
    harness.wheel_down(20, 1);
    harness.wheel_down(20, harness.app.state.height - 1);
    assert_eq!(harness.app.state.task_selected, before);

    harness.app.state.show_help = true;
    harness.draw();
    harness.wheel_down(20, harness.app.state.height / 2);
    assert_eq!(harness.app.state.task_selected, before);
    assert!(harness.app.state.show_help);
}

// ── Wizard ─────────────────────────────────────────────────────────────────

#[test]
fn m083_clicking_a_wizard_select_option_moves_the_cursor_and_a_double_confirms() {
    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    harness.type_str("alpha");
    harness.press(KeyCode::Enter);
    harness.draw();
    let step = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();

    let option = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match z.kind {
            ZoneKind::WizardOption { index: 2, .. } => Some(z.rect),
            _ => None,
        })
        .next();
    let Some(rect) = option else {
        panic!("no third option on {step}:\n{}", harness.text());
    };
    harness.click(rect.x + 6, rect.y);
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().select_index(),
        2,
        "a single click moves the cursor"
    );
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        step,
        "and does not confirm"
    );
}

#[test]
fn m084_three_fast_clicks_across_a_step_boundary_do_not_confirm_the_next_step() {
    // The reviewer's walk: with an index-only identity, click → click
    // (confirm, step advances) → third click inside 400 ms counted as a
    // double and confirmed option 0 of the *new* step, so three fast clicks
    // could walk a wizard to "create" with defaults.
    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    harness.type_str("alpha");
    harness.press(KeyCode::Enter);
    harness.draw();
    let first_step = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();

    let rect = harness
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, ZoneKind::WizardOption { index: 0, .. }))
        .expect("an option")
        .rect;
    // Three clicks on the same cell, no clock movement at all.
    harness.click(rect.x + 6, rect.y);
    harness.click(rect.x + 6, rect.y);
    let after_two = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();
    harness.click(rect.x + 6, rect.y);
    let after_three = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();

    assert_ne!(after_two, first_step, "the double click confirmed step 1");
    assert_eq!(
        after_three, after_two,
        "the third click must not confirm the next step as well"
    );
    assert!(
        harness.app.state.mouse.last_click.is_none(),
        "the double-click window is retired when the secondary fires"
    );
}

#[test]
fn m085_clicking_a_wizard_hint_verb_confirms_and_goes_back() {
    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    harness.type_str("alpha");
    harness.draw();
    click_wizard_confirm(&mut harness);
    let step = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();
    assert_ne!(step, "name", "confirm advanced the wizard");

    harness.draw();
    harness.click_text("Esc back");
    if harness.app.state.wizard_sessions.pending_leave.is_some() {
        harness.draw().click_text("Continue editing");
        assert_eq!(harness.app.state.wizard.as_ref().unwrap().step_id(), step);
        harness.draw().click_text("Esc back");
        harness.draw().click_text("Discard and leave");
    }
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        "name",
        "`Esc back` here means back one step, not cancel"
    );
}

#[test]
fn m086_clicking_a_wizard_suggestion_applies_it() {
    let mut harness = view(ViewId::Tasks);
    harness.click_text("+ add task...");
    harness.draw();
    let suggestion = harness
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, ZoneKind::WizardSuggestion { .. }))
        .map(|z| z.rect);
    if let Some(rect) = suggestion {
        harness.click(rect.x + 6, rect.y);
        assert!(
            !harness
                .app
                .state
                .wizard
                .as_ref()
                .unwrap()
                .text_value()
                .is_empty(),
            "the suggestion filled the field"
        );
    }
}

// ── Onboarding ─────────────────────────────────────────────────────────────

#[test]
fn m087_clicking_the_onboarding_nudge_opens_the_new_entity_wizard() {
    let mut harness = Harness::with(FakeBackend::new().with_tasks(tasks(1)));
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.onboarding = orch_tui::onboarding::OnboardingStep::TaskCreated;
    harness.draw();
    if harness.find_text("R run task").is_some() {
        harness.click_text("R run task");
        harness.assert_called(|c| matches!(c, Call::RunTask(_)));
    }
}

#[test]
fn m088_quit_is_exempt_from_setting_suppression_but_not_from_anothers() {
    // Arming must not suppress the confirmation it just asked for.
    let mut harness = wide();
    harness.click_text("Q quit");
    assert!(
        harness.app.state.mouse.footer_suppressed_until.is_none(),
        "quit does not arm the row suppression"
    );

    // But a reflow caused by another action must still protect the row,
    // including the quit hint — otherwise a stray second click after
    // approving could land on it.
    let mut backend = loaded();
    backend.tasks[0].status = TaskStatus::Review;
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();
    harness.click_text("A approve");
    harness.draw();
    harness.click_text("Q quit");
    assert!(
        !harness.app.quit_armed(),
        "the row was suppressed by the approve, so the quit click is dropped"
    );
    assert!(!harness.app.state.should_exit);
}

// ── Capture toggle (stage 0) ───────────────────────────────────────────────

#[test]
fn m089_f2_toggles_capture_from_inside_every_modal_without_disturbing_it() {
    // Stage 0 is before the help overlay, the pickers, the wizard and command
    // mode — the places where selecting an error message matters most, and
    // where every other key is swallowed.
    let mut harness = view(ViewId::Tasks);
    harness.app.state.show_help = true;
    harness.press(KeyCode::F(2));
    assert!(!harness.app.state.mouse.enabled);
    assert!(harness.app.state.show_help, "the overlay is not dismissed");

    let mut harness = agent_picker();
    harness.app.state.picker_draft = ["agt_1".to_string()].into_iter().collect();
    harness.press(KeyCode::F(2));
    assert!(!harness.app.state.mouse.enabled);
    assert!(harness.app.state.show_agent_picker);
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_1"],
        "the draft is untouched"
    );

    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    harness.type_str("alpha");
    harness.press(KeyCode::F(2));
    assert!(!harness.app.state.mouse.enabled);
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().text_value(),
        "alpha",
        "F2 is not typed into the field"
    );

    let mut harness = command_mode();
    harness.press(KeyCode::F(2));
    assert!(!harness.app.state.mouse.enabled);
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(harness.app.state.command_bar.input.value(), "/t");
}

#[test]
fn m090_f2_toggles_back_and_changes_nothing_else() {
    let mut harness = view(ViewId::Tasks);
    let before = (harness.app.state.view, harness.app.state.task_selected);
    harness.press(KeyCode::F(2));
    assert!(!harness.app.state.mouse.enabled);
    harness.press(KeyCode::F(2));
    assert!(harness.app.state.mouse.enabled);
    assert_eq!(
        (harness.app.state.view, harness.app.state.task_selected),
        before
    );
}

#[test]
fn m091_the_footer_marker_renders_only_while_capture_is_off() {
    let mut harness = view(ViewId::Tasks);
    harness.refute_text("mouse off");
    harness.press(KeyCode::F(2));
    harness.assert_text("mouse off");
    harness.press(KeyCode::F(2));
    harness.refute_text("mouse off");
}

#[test]
fn m092_the_mouse_verb_sets_and_reports_the_flag() {
    let mut harness = view(ViewId::Tasks);
    harness.app.execute_command("mouse off");
    assert!(!harness.app.state.mouse.enabled);
    harness.assert_message("Mouse capture off");

    harness.app.execute_command("mouse on");
    assert!(harness.app.state.mouse.enabled);

    harness.app.execute_command("mouse status");
    harness.assert_message("Mouse capture is on");

    harness.app.execute_command("mouse sideways");
    harness.assert_message("Usage: mouse on|off|status");
}

#[test]
fn m093_the_mouse_verb_appears_in_the_slash_suggestions() {
    // `/` is the TUI's own discovery surface, which is the point of adding a
    // verb next to `F2`.
    let suggestions = orch_tui::command_bar::resolve_suggestions("/mou");
    assert!(
        suggestions.iter().any(|s| s.cmd.starts_with("/mouse")),
        "got {suggestions:#?}"
    );
}

#[test]
fn m094_turning_capture_off_clears_any_armed_press_or_arming() {
    let mut harness = wide();
    harness.click_text("Q quit");
    assert!(harness.app.quit_armed());
    let (x, y) = harness.find_text("Task number 1").expect("a row");
    harness.press_at(x, y);
    assert!(harness.app.state.mouse.armed.is_some());

    harness.app.set_mouse_enabled(false);
    assert!(harness.app.state.mouse.armed.is_none());
    assert!(!harness.app.quit_armed());
    // And a release that arrives after the toggle does nothing.
    harness.release_at(x, y);
    assert!(!harness.app.state.should_exit);
}

#[test]
fn m095_the_help_overlay_advertises_f2() {
    let mut harness = view(ViewId::Tasks);
    harness.app.state.show_help = true;
    harness.assert_text("Mouse on/off");
}

#[test]
fn m096_clicking_a_log_row_never_scrolls_the_view_under_the_pointer() {
    // Lead ruling: a click selects the row and leaves `log_scroll_offset`
    // alone unless the row would be off-window — which, for a row the user
    // just clicked, it cannot be. The subtlety is tail mode: there the offset
    // is 0 while the window shows the *last* `view_h` rows, so leaving the
    // offset at 0 would jump the view to the top of the list. Keeping the
    // same rows on screen is what the assertion below pins.
    let mut harness = logs_with_messages();
    let before: Vec<String> = (0..harness.app.state.height)
        .map(|y| harness.line(y))
        .collect();
    assert_eq!(
        harness.app.state.log_selected_index, -1,
        "the harness starts in tail mode"
    );

    harness.click_text("msg-35");
    harness.draw();

    // Same message rows, in the same order, after leaving tail mode.
    let after: Vec<String> = (0..harness.app.state.height)
        .map(|y| harness.line(y))
        .collect();
    let msgs = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .filter(|l| l.contains("msg-"))
            .map(|l| l[l.find("msg-").unwrap()..][..6].to_string())
            .collect()
    };
    assert_eq!(
        msgs(&after),
        msgs(&before),
        "the clicked row was already visible, so the window must not move"
    );

    // And a second click on another visible row does not move it either.
    let offset = harness.app.state.log_scroll_offset;
    harness.click_text("msg-32");
    harness.draw();
    assert_eq!(harness.app.state.log_scroll_offset, offset);
    assert_eq!(
        msgs(
            &(0..harness.app.state.height)
                .map(|y| harness.line(y))
                .collect::<Vec<_>>()
        ),
        msgs(&before)
    );
}

// ── Multiselect rows behave like the picker rows they look like ────────────

/// The team wizard: name (text) → lead (select) → members (**multiselect**).
fn team_wizard_on_members() -> Harness {
    let mut harness = Harness::with(loaded());
    harness.app.open_team_wizard();
    harness.app.state.input_mode = InputMode::Wizard;
    harness.type_str("frontend");
    harness.press(KeyCode::Enter); // name → lead
    harness.press(KeyCode::Enter); // lead → members
    harness.draw();
    let step = harness
        .app
        .state
        .wizard
        .as_ref()
        .map(|w| w.step_id().to_string());
    assert_eq!(
        step.as_deref(),
        Some("members"),
        "expected the multiselect step:\n{}",
        harness.text()
    );
    harness
}

#[track_caller]
fn option_cell(harness: &Harness, index: usize, part: orch_tui::hit::RowPart) -> (u16, u16) {
    let rect = harness
        .hits()
        .zones()
        .iter()
        .find(|z| z.kind == ZoneKind::WizardOption { index, part })
        .unwrap_or_else(|| panic!("no wizard {part:?} zone for option {index}"))
        .rect;
    (rect.x, rect.y)
}

#[test]
fn m097_a_double_click_on_a_multiselect_label_toggles_and_never_confirms() {
    // Before the fix the secondary action sent Enter, which on a multiselect
    // step **submits the whole step** — so a double click on a label advanced
    // the wizard instead of ticking a box.
    let mut harness = team_wizard_on_members();
    let (x, y) = option_cell(&harness, 1, orch_tui::hit::RowPart::Label);

    harness.double_click(x + 8, y);
    let wizard = harness.app.state.wizard.as_ref().expect("still open");
    assert_eq!(
        wizard.step_id(),
        "members",
        "a double click on a multiselect label must not advance the wizard"
    );
    assert_eq!(
        wizard.multi_selected().len(),
        1,
        "it toggles that option instead"
    );
}

#[test]
fn m098_every_click_on_a_multiselect_checkbox_toggles_it() {
    // A wizard checkbox and a picker checkbox look identical, so the same
    // gesture must do the same thing. Before the fix the second click was a
    // no-op here while it toggled back in a picker.
    let mut harness = team_wizard_on_members();
    let (x, y) = option_cell(&harness, 0, orch_tui::hit::RowPart::Checkbox);

    harness.click(x + 1, y);
    assert_eq!(
        harness
            .app
            .state
            .wizard
            .as_ref()
            .unwrap()
            .multi_selected()
            .len(),
        1
    );
    harness.draw();
    // Second click, inside the double-click window, on the same cell.
    harness.click(x + 1, y);
    assert_eq!(
        harness
            .app
            .state
            .wizard
            .as_ref()
            .unwrap()
            .multi_selected()
            .len(),
        0,
        "the second click toggles back, as a picker checkbox does"
    );
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        "members",
        "and neither click confirms the step"
    );
}

#[test]
fn m099_a_double_click_on_a_select_label_still_confirms() {
    // The multiselect fix must not take the Select step's confirm with it:
    // there is one value, so confirming is the obvious second click.
    let mut harness = view(ViewId::Agents);
    harness.click_text("+ add agent...");
    harness.type_str("alpha");
    harness.press(KeyCode::Enter);
    harness.draw();
    let before = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .step_id()
        .to_string();
    let (x, y) = option_cell(&harness, 0, orch_tui::hit::RowPart::Label);

    harness.double_click(x + 6, y);
    assert_ne!(
        harness
            .app
            .state
            .wizard
            .as_ref()
            .map(|w| w.step_id().to_string()),
        Some(before),
        "a Select label's double click confirms"
    );
}

// ── A chip click retires the rows it re-filters ────────────────────────────

#[test]
fn m100_a_chip_click_invalidates_the_rows_in_the_same_drain() {
    // Toggling an agent chip changes the length of `filtered_messages`, so
    // every LogRow zone now names a different message. A row click later in
    // the same drain — no redraw in between — must be dropped rather than
    // select whatever used to be drawn there.
    let mut harness = logs_with_messages();
    for (i, message) in harness.app.state.messages.iter_mut().enumerate() {
        if i % 2 == 0 {
            message.agent_id = Some("agt_1".into());
        }
    }
    harness.draw();

    let (rx, ry) = harness.find_text("msg-37").expect("a log row");
    let before = harness.app.state.log_selected_index;
    harness.click_text("agent1");
    assert!(
        !harness.app.state.mouse.hits.is_valid(),
        "the chip click must retire the map it re-filtered"
    );
    harness.click(rx, ry);
    assert_eq!(
        harness.app.state.log_selected_index, before,
        "the row click resolved against a map that no longer describes the screen"
    );
}

// ── A row click reflows the footer under the pointer ──────────────────────

#[test]
fn m101_selecting_a_review_row_cannot_fire_the_footer_delete_it_replaced() {
    // Code gate finding 1. A `Todo` selection puts `D delete` in the footer;
    // selecting a `Review` task replaces it with `A approve  X reject`, and
    // everything right of that shifts. A second click in the same drain — a
    // stalled websocket flushing, a resumed tmux pane, a replayed burst — at
    // the old `D delete` cells would otherwise delete a task whose footer
    // never offered deletion.
    let mut backend = loaded();
    backend.tasks[1].status = TaskStatus::Review;
    let mut harness = Harness::sized(backend, 140, 30);
    harness.app.state.view = ViewId::Tasks;
    harness.draw();

    // A Review task sorts to the top, so select a Todo one first to get the
    // `D delete` footer, then let its own suppression window lapse — this
    // test is about the *selection* reflow, not the footer-click one.
    harness.click_text("Task number 0");
    harness.clock.advance_ms(DOUBLE_CLICK_MS + 1);
    harness.draw();
    let (dx, dy) = harness
        .find_text("D delete")
        .expect("a Todo selection offers delete");
    let (rx, ry) = harness.find_text("Task number 1").expect("the review row");

    // One drain: select the Review row, then click where `D delete` used to be.
    harness.click(rx, ry);
    harness.click(dx, dy);

    assert!(
        harness.app.state.pending_deletions.is_empty(),
        "a stale footer cell must not delete: {:?}",
        harness.app.state.pending_deletions
    );
    harness.refute_called(|c| matches!(c, Call::DeleteTask(_)));
    // The row selection itself still happened.
    assert_eq!(
        harness.app.state.selected_task().unwrap().title,
        "Task number 1"
    );
}

#[test]
fn m102_a_row_double_click_still_works_in_a_single_drain() {
    // The guard for m101's fix: invalidating the map would have served m101
    // and silently broken this, which is how a real double-click arrives —
    // both reports in one write.
    let mut harness = view(ViewId::Tasks);
    let (x, y) = harness.find_text("Task number 1").expect("a row");
    harness.click(x, y);
    harness.click(x, y);
    assert_eq!(
        harness.app.state.detail,
        DetailPanel::Entity,
        "two clicks in one drain must still open the detail"
    );
}

// ── Picker checkbox: the clicked row, and the cursor follows ──────────────

#[test]
fn m103_a_picker_checkbox_toggles_its_own_row_and_takes_the_cursor_there() {
    // QA B1. The cursor starts on row 0; clicking row 2's checkbox must
    // toggle row 2 — not the cursor row — and leave the cursor on row 2 so
    // that `Space` next acts on the row the user just touched.
    let mut harness = agent_picker();
    harness.app.state.picker_cursor = 0;
    harness.draw();

    let (x, y) = harness
        .find_text("[✓] agent2")
        .expect("the third picker row");
    harness.click(x + 1, y);

    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_2"],
        "the clicked row toggled, not the cursor row"
    );
    assert_eq!(
        harness.app.state.picker_cursor, 2,
        "and the cursor followed the click"
    );
}

#[test]
fn m104_a_type_picker_checkbox_toggles_its_own_row_and_takes_the_cursor_there() {
    let mut harness = logs_with_messages();
    harness.app.state.show_type_picker = true;
    harness.app.state.type_picker_draft.clear();
    harness.app.state.picker_cursor = 0;
    harness.draw();

    let rows: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|z| match z.kind {
            ZoneKind::TypePickerRow {
                index: 3,
                part: orch_tui::hit::RowPart::Checkbox,
            } => Some(z.rect),
            _ => None,
        })
        .collect();
    let rect = rows.first().copied().expect("row 3's checkbox");
    harness.click(rect.x + 1, rect.y);

    assert_eq!(harness.app.state.type_picker_draft.len(), 1);
    assert_eq!(harness.app.state.picker_cursor, 3);
}

// ── The last wizard step must be finishable with the mouse ───────────────

#[test]
fn m105_the_textarea_confirm_hint_finishes_the_wizard() {
    // QA B2. On a `Textarea` step the hint reads `Ctrl+Enter/Tab confirm`,
    // and the action used to send plain `Enter` — which inserts a newline
    // there, so nothing happened and a mouse-only user could not finish
    // creating a task or a goal.
    let mut harness = view(ViewId::Goals);
    harness.click_text("+ add goal...");
    harness.type_str("Ship it");
    harness.draw();

    // Walk to the description step, which is the textarea.
    for _ in 0..4 {
        let kind = harness.app.state.wizard.as_ref().map(|w| w.step().kind);
        if kind == Some(orch_tui::wizard::engine::StepKind::Textarea) {
            break;
        }
        harness.draw();
        click_wizard_confirm(&mut harness);
    }
    let kind = harness.app.state.wizard.as_ref().map(|w| w.step().kind);
    assert_eq!(
        kind,
        Some(orch_tui::wizard::engine::StepKind::Textarea),
        "expected to reach the textarea step:\n{}",
        harness.text()
    );
    harness.draw();
    click_wizard_confirm(&mut harness);
    assert_eq!(
        harness.app.state.input_mode,
        InputMode::None,
        "clicking the confirm hint on the last step must finish the wizard"
    );
    harness.assert_called(|c| matches!(c, Call::CreateGoal { .. }));
}

// ── Help overlay: the footer dismisses too ───────────────────────────────

#[test]
fn m106_a_footer_click_with_the_help_overlay_open_dismisses_and_does_nothing() {
    // QA B5. The command bar is drawn *after* the overlay, so a `FooterHint`
    // zone sat on top of `HelpOverlayOutside` and the stage swallowed the
    // click — the overlay stayed up and the hint did nothing. Per the policy,
    // anything outside the box dismisses.
    let mut harness = wide();
    harness.app.state.show_help = true;
    harness.draw();

    let before = harness.app.state.task_selected;
    harness.click_text("? help");
    assert!(
        !harness.app.state.show_help,
        "a footer click is outside the box, so it dismisses"
    );
    assert_eq!(harness.app.state.task_selected, before);

    // And the dismissing click performs no action of its own: the overlay did
    // not reopen, and quit was not armed.
    harness.app.state.show_help = true;
    harness.draw();
    harness.click_text("Q quit");
    assert!(!harness.app.state.show_help);
    assert!(!harness.app.quit_armed(), "dismiss only, no action");
    assert!(!harness.app.state.should_exit);
}

#[test]
fn m107_a_late_release_still_fires_and_a_resize_between_cancels_it() {
    // QA B3's mechanism. tmux splits a click's press and release across two
    // writes after a resize, so the release arrives in a later batch. The arm
    // has to survive that — otherwise the click is simply lost.
    let mut harness = view(ViewId::Tasks);
    let (x, y) = harness.find_text("agents").expect("the agents pill");
    harness.press_at(x, y);
    // Frames pass with no release in sight.
    harness.draw();
    harness.draw();
    harness.release_at(x, y);
    assert_eq!(
        harness.app.state.view,
        ViewId::Agents,
        "a release that arrives frames later still completes the click"
    );

    // But if the geometry it was pressed against is retired in between, the
    // click is cancelled rather than fired against a rebuilt frame.
    let mut harness = view(ViewId::Tasks);
    let (x, y) = harness.find_text("agents").expect("the agents pill");
    harness.press_at(x, y);
    harness.app.resize(80, 24);
    assert!(
        harness.app.state.mouse.armed.is_none(),
        "a real resize cancels the press it invalidated"
    );
    harness.app.state.width = 80;
    harness.app.state.height = 24;
    harness.draw();
    harness.release_at(x, y);
    assert_eq!(
        harness.app.state.view,
        ViewId::Tasks,
        "and the stale release does nothing"
    );
}

#[test]
fn m108_a_late_resize_notice_matching_the_drawn_frame_keeps_the_map() {
    // crossterm delivers the SIGWINCH notice on the first read after the
    // signal, by which time the loop has usually redrawn at the new size. If
    // that late notice invalidated the map, it would take the click it
    // arrived with (QA B3's first layer).
    let mut harness = view(ViewId::Tasks);
    let area = harness.hits().area();
    harness.app.resize(area.width, area.height);
    assert!(
        harness.hits().is_valid(),
        "a notice for the size we already drew is stale news, not a change"
    );

    harness.app.resize(area.width + 20, area.height);
    assert!(
        !harness.hits().is_valid(),
        "a size we have not drawn yet does retire the map"
    );
}

// ── Which entity a picker click acts on (QA B1) ───────────────────────────

/// Three agents, so "selected one row" and "toggled the other row" are
/// distinguishable. With two they are not, which is what made QA's two-agent
/// repro read as an index mismatch.
fn three_agent_picker() -> Harness {
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_a", "Alpha", AgentStatus::Idle),
        make_agent("agt_b", "Beta", AgentStatus::Idle),
        make_agent("agt_c", "Gamma", AgentStatus::Idle),
    ]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness.app.state.show_agent_picker = true;
    harness.app.state.picker_cursor = 0;
    harness.draw();
    harness
}

/// The ids whose checkbox is rendered ticked, in row order — read off the
/// frame, not computed, so this asserts what the user actually sees.
fn checked_rows(harness: &mut Harness) -> Vec<String> {
    let text = harness.text();
    let draft = &harness.app.state.picker_draft;
    [("Alpha", "agt_a"), ("Beta", "agt_b"), ("Gamma", "agt_c")]
        .iter()
        .filter(|(name, _)| text.contains(&format!("[✓] {name}")))
        .map(|(_, id)| {
            // Cross-check the rendering against the state it came from.
            assert!(
                draft.is_empty() || draft.contains(*id),
                "a row renders ticked only when implicit-all or explicitly drafted"
            );
            (*id).to_string()
        })
        .collect()
}

#[test]
fn m109_a_picker_checkbox_acts_on_the_entity_on_that_row() {
    // Asserted by **entity id**, with the cursor deliberately parked on a
    // different row than the one clicked.
    let mut harness = three_agent_picker();
    assert_eq!(
        checked_rows(&mut harness),
        vec!["agt_a", "agt_b", "agt_c"],
        "an empty draft renders every row ticked (implicit-all)"
    );

    // Click Beta — the middle row — with the cursor still on Alpha.
    let (x, y) = harness.find_text("[✓] Beta").expect("Beta's row");
    harness.click(x + 1, y);

    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_b"],
        "the clicked entity is the one selected — not the cursor's, and not a neighbour's"
    );
    assert_eq!(harness.app.state.picker_cursor, 1, "the cursor followed");
    // What the user sees: Beta ticked, the other two cleared. That is
    // implicit-all collapsing to one explicit selection, not the wrong row
    // toggling — and with only two agents it is indistinguishable from it.
    assert_eq!(checked_rows(&mut harness), vec!["agt_b"]);

    // With a non-empty draft the mapping is unambiguous: clicking Gamma adds
    // Gamma and leaves Beta exactly as it was.
    harness.draw();
    let (x, y) = harness.find_text("[ ] Gamma").expect("Gamma's row");
    harness.click(x + 1, y);
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_b", "agt_c"],
        "the second click added only the row it was on"
    );
    assert_eq!(checked_rows(&mut harness), vec!["agt_b", "agt_c"]);

    // And clicking a ticked row's box clears just that row.
    harness.draw();
    let (x, y) = harness.find_text("[✓] Beta").expect("Beta's row again");
    harness.click(x + 1, y);
    assert_eq!(
        harness.app.state.picker_draft.iter().collect::<Vec<_>>(),
        vec!["agt_c"]
    );
}

#[test]
fn m110_the_mouse_and_the_keyboard_select_the_same_entity() {
    // The semantics above are the keyboard's, not the mouse's: `Space` on the
    // cursor row and a click on that row's checkbox must land identically.
    let mut clicked = three_agent_picker();
    let (x, y) = clicked.find_text("[✓] Beta").expect("Beta's row");
    clicked.click(x + 1, y);

    let mut typed = three_agent_picker();
    typed.app.state.picker_cursor = 1;
    typed.press(KeyCode::Char(' '));

    assert_eq!(
        clicked.app.state.picker_draft.iter().collect::<Vec<_>>(),
        typed.app.state.picker_draft.iter().collect::<Vec<_>>(),
        "a checkbox click is `Space` on that row, nothing else"
    );
}

#[test]
fn m111_the_whole_confirm_label_is_clickable_from_its_first_cell() {
    // The first cell of the rendered confirmation label must be actionable.
    let mut harness = view(ViewId::Goals);
    harness.click_text("+ add goal...");
    harness.type_str("Ship it");
    harness.draw();
    for _ in 0..4 {
        let kind = harness.app.state.wizard.as_ref().map(|w| w.step().kind);
        if kind == Some(orch_tui::wizard::engine::StepKind::Textarea) {
            break;
        }
        harness.draw();
        click_wizard_confirm(&mut harness);
    }
    harness.draw();
    // Locate the visible text independently of its hit rectangle, so a clipped
    // zone cannot make this check pass by moving its own first cell.
    let label = if harness.find_text("Tab OK").is_some() {
        "Tab OK".into()
    } else {
        format!("{}+Enter/Tab confirm", orch_tui::widgets::wizard::CMD_KEY)
    };
    let (x, y) = harness.find_text(&label).expect("visible confirm label");
    harness.click(x, y);
    assert_eq!(
        harness.app.state.input_mode,
        InputMode::None,
        "clicking the first cell of the visible label confirms too"
    );
    harness.assert_called(|c| matches!(c, Call::CreateGoal { .. }));
}

/// The NEW TASK wizard on its assignee step with a filter (`zz`) that lists
/// no agent: only the Auto-assign sentinel is offered.
fn assignee_step_matching_no_agent() -> Harness {
    let mut harness = Harness::with(loaded());
    harness.key('t');
    harness.key('n');
    harness.type_str("Ship it");
    harness.press(KeyCode::Enter); // title
    harness.press(KeyCode::Enter); // priority
    harness.type_str("zz");
    harness.draw();
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.step_id(), "assignee");
    assert!(wizard.picker_blocked(), "the filter lists no agent");
    assert_eq!(
        wizard
            .options()
            .iter()
            .map(|c| c.label.as_str())
            .collect::<Vec<_>>(),
        ["Auto-assign"]
    );
    harness
}

#[test]
fn m112_a_double_click_on_the_offered_sentinel_picks_it_when_no_agent_matches() {
    // Review N1 / DQ1 D3: the sentinel is a real row even when the filter
    // matches nobody. Pointing at it twice is as explicit as its digit, so it
    // is accepted; only a bare Enter on typed text is refused.
    let mut harness = assignee_step_matching_no_agent();
    let (x, y) = option_cell(&harness, 0, orch_tui::hit::RowPart::Label);

    harness.double_click(x + 6, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(
        wizard.step_id(),
        "description",
        "the double click accepts the sentinel and advances"
    );
    assert_eq!(
        wizard.values().get("assignee").map(String::as_str),
        Some(""),
        "the accepted value is Auto-assign's own id, the empty assignee"
    );
    harness.refute_called(|c| matches!(c, Call::CreateTask { .. }));
}

#[test]
fn m113_with_no_agent_matching_a_single_click_or_a_bare_enter_still_does_not_pick() {
    // The other half of N1: the fix is the pointer's explicit pick, not a
    // weaker free-text guard.
    let mut harness = assignee_step_matching_no_agent();
    let (x, y) = option_cell(&harness, 0, orch_tui::hit::RowPart::Label);

    harness.click(x + 6, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.step_id(), "assignee", "a single click only selects");

    harness.press(KeyCode::Enter);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(
        wizard.step_id(),
        "assignee",
        "a bare Enter is still refused"
    );
    assert_eq!(
        wizard.visible_error(),
        Some(orch_tui::wizard::engine::NO_AGENT_MATCHES)
    );
}

#[test]
fn m114_a_double_click_on_a_filtered_agent_row_accepts_that_agent_id() {
    // The sentinel fix keeps the matching case on its own path: with a filter
    // that lists agents, a double click on a displayed row accepts that row's
    // exact id through the ordinary confirm, not the sentinel.
    let mut harness = Harness::with(loaded());
    harness.key('t');
    harness.key('n');
    harness.type_str("Ship it");
    harness.press(KeyCode::Enter); // title
    harness.press(KeyCode::Enter); // priority
    // Letters only: a typed digit is a pick, not filter text.
    harness.type_str("agent");
    harness.draw();
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert!(!wizard.picker_blocked());
    assert_eq!(
        wizard.options()[3].value,
        "agt_2",
        "agent2 is the fourth row"
    );
    let (x, y) = option_cell(&harness, 3, orch_tui::hit::RowPart::Label);

    harness.double_click(x + 6, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.step_id(), "description");
    assert_eq!(
        wizard.values().get("assignee").map(String::as_str),
        Some("agt_2")
    );
}
