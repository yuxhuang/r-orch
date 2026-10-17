//! Foundation render/source and normalized-event seams, not B0 feature tests.
use orch_tui::hit::TextSurfaceId;
use orch_tui::keys::{ExitCause, UiEvent};
use orch_tui::state::InputMode;
use orch_tui::views::{Caps, RenderCtx};
use orch_tui::widgets::text_viewer::ViewerSource;
use orch_tui::{App, AppState, FakeBackend, TestClock};
use ratatui::layout::Rect;

#[test]
fn whole_footer_metrics_match_live_generation_and_fail_closed_when_invalidated() {
    let mut state = AppState::default();
    let origin = state
        .register_viewer_origin(ViewerSource::Message(7))
        .unwrap();
    let caps = Caps {
        can_create_task: false,
        can_add_agent: false,
        can_create_goal: false,
        can_force_stop_agent: false,
        can_send_message: false,
    };
    let ctx = RenderCtx::new(&state, caps, 1, Rect::new(0, 0, 40, 14));
    ctx.publish_text_footer(origin.clone(), Rect::new(1, 12, 38, 1));
    state.mouse.hits = ctx.hits.into_inner();
    let footer = state.mouse.hits.text_footer(&origin).unwrap();
    assert_eq!(footer.rect, Rect::new(1, 12, 38, 1));
    assert!(footer.rect.contains((38, 12).into()));
    assert_eq!(
        state.live_copy_origin(&TextSurfaceId::Viewer(ViewerSource::Message(7))),
        Some(&origin)
    );
    let next = state
        .register_viewer_origin(ViewerSource::Message(7))
        .unwrap();
    assert!(state.mouse.hits.text_footer(&origin).is_none());
    assert!(state.mouse.hits.text_footer(&next).is_none());
}

#[test]
fn dispatcher_ignores_release_and_normalizes_user_and_signal_exit() {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let mut app = App::new(FakeBackend::new(), TestClock::default());
    let mut key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    key.kind = KeyEventKind::Release;
    app.dispatch_ui_event(&UiEvent::Terminal(Event::Key(key)));
    assert!(!app.state.should_exit);
    app.request_exit(ExitCause::User);
    assert!(app.state.should_exit);
    let mut app = App::new(FakeBackend::new(), TestClock::default());
    app.state.input_mode = InputMode::Command;
    app.request_exit(ExitCause::Signal);
    assert!(app.state.should_exit);
}

mod common;
use common::Harness;
use orch_domain::{GoalStatus, TaskStatus};
use orch_tui::backend::{make_goal, make_task};
use orch_tui::state::{DetailPanel, Focus, GoalSection, TaskSection, ViewId};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

#[test]
fn f1_tab_moves_between_panes_without_switching_the_view() {
    let mut h = Harness::new();
    h.press(KeyCode::Tab);
    assert_eq!(
        (h.app.state.view, h.app.state.focus),
        (ViewId::Tasks, Focus::Activity)
    );
    h.press(KeyCode::Tab);
    assert_eq!(h.app.state.focus, Focus::MainList);
    h.press(KeyCode::BackTab);
    assert_eq!(h.app.state.focus, Focus::Activity);
    h.app.state.view = ViewId::Logs;
    h.app.state.focus = Focus::MainList;
    h.press(KeyCode::Tab);
    assert_eq!(
        (h.app.state.view, h.app.state.focus),
        (ViewId::Logs, Focus::MainList)
    );
}

#[test]
fn f2_enter_initially_focuses_overview_and_maximize_restore_recovers_actions() {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![make_task(
        "t",
        "Task",
        TaskStatus::Todo,
    )]));
    h.press(KeyCode::Enter);
    assert_eq!(h.app.state.focus, Focus::TaskDetail(TaskSection::Overview));
    h.key('M').key('M');
    assert_eq!(h.app.state.focus, Focus::MainList);
    assert_eq!(h.app.state.detail, DetailPanel::Entity);
}

#[test]
fn f5_goal_task_inspection_and_modified_letters_never_mutate_parent() {
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::ALT,
    ] {
        let mut h = Harness::with(FakeBackend::new().with_goals(vec![make_goal(
            "g",
            "Goal",
            GoalStatus::Active,
        )]));
        h.app.state.view = ViewId::Goals;
        h.app.backend.calls.clear();
        for key in ['c', 'd', 'e'] {
            h.press_mod(KeyCode::Char(key), modifiers);
        }
        assert!(
            h.calls().is_empty(),
            "modified action reached backend: {:?}",
            h.calls()
        );
        assert!(h.app.state.wizard.is_none());
    }
    let mut h = Harness::with(FakeBackend::new().with_goals(vec![make_goal(
        "g",
        "Goal",
        GoalStatus::Active,
    )]));
    h.app.state.view = ViewId::Goals;
    h.app.state.detail = DetailPanel::Entity;
    h.app.state.focus = Focus::GoalDetail(GoalSection::Tasks);
    h.app.backend.calls.clear();
    h.key('c').key('d').key('e');
    assert!(h.calls().is_empty());
    assert!(h.app.state.wizard.is_none());
    assert!(h.app.state.pending_deletions.is_empty());
}

#[test]
fn f6_ctrl_y_copies_the_complete_focused_field_without_a_viewer() {
    let mut task = make_task("t", "Task", TaskStatus::Todo);
    task.description = "first\n  last e\u{301} 中".into();
    let expected = task.description.clone();
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![task]));
    h.app.state.detail = DetailPanel::Entity;
    h.app.state.focus = Focus::TaskDetail(TaskSection::Description);
    h.press_mod(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(
        h.app.backend.copy_requests,
        vec![(orch_tui::backend::CopyTarget::Auto, expected)]
    );
    assert!(h.app.state.viewer.is_none());
    h.press_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)
        .press_mod(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
    assert_eq!(h.app.backend.copy_requests.len(), 1);
}

#[test]
fn n6_picker_end_and_home_reach_the_full_option_list() {
    let mut h = Harness::sized(FakeBackend::new(), 40, 14);
    h.app.state.view = ViewId::Logs;
    h.key('f').draw().press(KeyCode::End).draw();
    assert_eq!(
        h.app.state.picker_cursor,
        orch_tui::messages::MsgType::ALL.len() - 1
    );
    h.press(KeyCode::Home);
    assert_eq!(h.app.state.picker_cursor, 0);
}

#[test]
fn n6_wizard_select_end_home_and_page_use_the_list_without_advancing() {
    use orch_tui::wizard::engine::{Choice, StepKind, Wizard, WizardKind, WizardStep};
    let mut h = Harness::new();
    let options = (0..30)
        .map(|i| Choice::new(&format!("v{i}"), &format!("option{i}"), ""))
        .collect();
    h.app.state.wizard = Some(Wizard::new(
        WizardKind::Task,
        "Pick",
        vec![WizardStep::new("pick", "Pick", StepKind::Select).options(options)],
    ));
    h.app.state.input_mode = InputMode::Wizard;
    h.draw().press(KeyCode::End);
    assert_eq!(h.app.state.wizard.as_ref().unwrap().select_index(), 29);
    h.press(KeyCode::Home);
    assert_eq!(h.app.state.wizard.as_ref().unwrap().select_index(), 0);
    h.draw().press(KeyCode::PageDown);
    assert!(h.app.state.wizard.as_ref().unwrap().select_index() > 0);
    assert_eq!(h.app.state.wizard.as_ref().unwrap().step_id(), "pick");
}

#[test]
fn n5_actual_tasks_and_agents_share_display_row_paging_and_refresh_units() {
    use orch_domain::AgentStatus;
    use orch_tui::backend::{make_agent, make_team};
    use orch_tui::hit::PaneMetrics;
    for agents in [false, true] {
        let mut h = Harness::new();
        h.app.state.onboarding = orch_tui::onboarding::OnboardingStep::Dismissed;
        h.app.state.goals = vec![
            make_goal("ga", "A", GoalStatus::Active),
            make_goal("gb", "B", GoalStatus::Active),
            make_goal("gc", "C", GoalStatus::Active),
        ];
        h.app.state.tasks = (0..6)
            .map(|i| {
                let mut t = make_task(&format!("t{i}"), &format!("task{i}"), TaskStatus::Todo);
                t.goal_id = Some(["ga", "gb", "gc"][i / 2].into());
                t
            })
            .collect();
        h.app.state.agents = (0..6)
            .map(|i| make_agent(&format!("a{i}"), &format!("agent{i}"), AgentStatus::Idle))
            .collect();
        h.app.state.teams = vec![
            make_team("a", "A", "a0", &["a0", "a1"]),
            make_team("b", "B", "a2", &["a2", "a3"]),
        ];
        h.app.state.view = if agents {
            ViewId::Agents
        } else {
            ViewId::Tasks
        };
        h.app.state.group_by_goal = true;
        h.draw();
        let key = h.app.state.pane_key(Focus::MainList);
        let metrics = PaneMetrics {
            key: key.clone(),
            body: Rect::new(0, 8, 80, 5),
            items: 7,
            display_rows: 10,
            selection_rows: vec![1, 2, 4, 5, 7, 8, 9],
        };
        h.app
            .state
            .mouse
            .hits
            .metrics
            .panes
            .retain(|m| m.key != key);
        h.app.state.mouse.hits.metrics.panes.push(metrics);
        h.press(KeyCode::Down);
        assert_eq!(
            if agents {
                (h.app.state.agent_selected, h.app.state.agent_scroll)
            } else {
                (h.app.state.task_selected, h.app.state.task_scroll)
            },
            (1, 0)
        );
        h.press(KeyCode::Home).press(KeyCode::PageDown);
        assert_eq!(
            if agents {
                (h.app.state.agent_selected, h.app.state.agent_scroll)
            } else {
                (h.app.state.task_selected, h.app.state.task_scroll)
            },
            (4, 5)
        );
        h.press(KeyCode::End).press(KeyCode::PageUp);
        assert_eq!(
            if agents {
                (h.app.state.agent_selected, h.app.state.agent_scroll)
            } else {
                (h.app.state.task_selected, h.app.state.task_scroll)
            },
            (2, 0)
        );
        h.press(KeyCode::End);
        h.app.state.clamp_all(true);
        assert_eq!(
            if agents {
                h.app.state.agent_scroll
            } else {
                h.app.state.task_scroll
            },
            5,
            "refresh must retain display-row offset"
        );
    }
}

#[test]
fn n8_readonly_wheel_has_three_rows_per_notch_and_three_notches_per_frame() {
    let mut t = make_task("t", "Task", TaskStatus::Todo);
    t.description = (0..100)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![t]));
    h.app.state.detail = DetailPanel::Entity;
    h.app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Description));
    h.draw();
    let key = h.app.state.pane_key(h.app.state.focus);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    for _ in 0..4 {
        h.wheel_down(body.x, body.y);
    }
    assert_eq!(h.app.state.pane_scroll(&key).offset, 9);
    assert_eq!(h.app.state.mouse.wheel_steps, 3);
}

#[test]
fn c7_cancelled_footer_down_cannot_activate_after_guard_expiry() {
    use orch_tui::hit::{HitMap, TextAction, TextFooterMetrics, TextSurfaceId, ZoneId, ZoneKind};
    use orch_tui::widgets::text_viewer::{
        Completeness, Representation, TextViewer, ViewerDocument,
    };
    use std::sync::Arc;
    let mut h = Harness::sized(FakeBackend::new(), 40, 14);
    let source = ViewerSource::Message(1);
    let origin = h.app.state.register_viewer_origin(source.clone()).unwrap();
    h.app.state.viewer = Some(TextViewer::new(ViewerDocument {
        source: source.clone(),
        title: "snapshot".into(),
        body: Arc::from("body"),
        representation: Representation::Field,
        raw_body: None,
        completeness: Completeness::Complete,
        original_bytes: 4,
        captured_at_ms: 0,
        initial_anchor: 0,
    }));
    let mut hits = HitMap::new(Rect::new(0, 0, 40, 14));
    let footer = Rect::new(1, 12, 38, 1);
    hits.metrics.viewer_footer = Some(TextFooterMetrics {
        rect: footer,
        origin: origin.clone(),
    });
    hits.push(
        footer,
        footer,
        ZoneKind::TextAction(TextAction::CopyAll),
        ZoneId::TextAction(TextSurfaceId::Viewer(source), TextAction::CopyAll),
    );
    h.app.set_hits(hits);
    h.app.state.suppress_text_footer(&origin, h.now());
    h.clock.advance_ms(399);
    h.press_at(5, 12);
    assert!(h.app.state.mouse.armed.is_none());
    h.clock.advance_ms(2);
    h.release_at(5, 12);
    assert!(h.app.state.mouse.armed.is_none());
    assert!(h.app.backend.copy_requests.is_empty());
}

#[test]
fn c7_same_cell_drag_jitter_preserves_the_entity_double_click() {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![make_task(
        "t",
        "Task",
        TaskStatus::Todo,
    )]));
    h.draw();
    let row = h
        .hits()
        .zones()
        .iter()
        .find(|z| matches!(z.kind, orch_tui::hit::ZoneKind::TaskRow { selection: 0 }))
        .unwrap()
        .rect;
    h.press_at(row.x, row.y)
        .drag_to(row.x, row.y)
        .release_at(row.x, row.y);
    assert!(h.app.state.mouse.last_click.is_some());
    h.press_at(row.x, row.y)
        .drag_to(row.x, row.y)
        .release_at(row.x, row.y);
    assert_eq!(h.app.state.detail, DetailPanel::Entity);
}

#[test]
fn n10_resize_refresh_at_minimum_and_below_never_panics() {
    for height in [13, 14, 15, 16, 24] {
        let mut h = Harness::sized(FakeBackend::new(), 40, 24);
        h.app.state.split_offset = i32::MAX;
        h.app.resize(40, height);
        h.app.refresh_all(false);
        if height <= 16 {
            assert_eq!(h.app.state.split_offset, 0);
        }
    }
}

#[test]
fn c7_whole_editor_footer_guard_blocks_legacy_confirm_and_cancelled_release() {
    use orch_tui::hit::{HitMap, TextFooterMetrics, VerbId, ZoneId, ZoneKind};
    use orch_tui::wizard::DraftKey;
    use orch_tui::wizard::engine::{StepKind, Wizard, WizardKind, WizardStep};
    let mut h = Harness::new();
    h.app.state.wizard = Some(Wizard::new(
        WizardKind::Task,
        "Task",
        vec![WizardStep::new("title", "Title", StepKind::Text)],
    ));
    h.app.state.input_mode = InputMode::Wizard;
    h.type_str("new title");
    let draft = DraftKey::from_wizard(h.app.state.wizard.as_ref().unwrap());
    let origin = h
        .app
        .state
        .register_editor_origin(draft, "title".into())
        .unwrap();
    let mut hits = HitMap::new(Rect::new(0, 0, 100, 30));
    let footer = Rect::new(0, 28, 100, 1);
    hits.metrics.editor_footer = Some(TextFooterMetrics {
        origin: origin.clone(),
        rect: footer,
    });
    hits.push(
        footer,
        footer,
        ZoneKind::Verb(VerbId::WizardConfirm),
        ZoneId::Verb(VerbId::WizardConfirm),
    );
    h.app.set_hits(hits);
    h.app.state.suppress_text_footer(&origin, h.now());
    h.clock.advance_ms(399);
    h.press_at(22, 28);
    h.clock.advance_ms(2);
    h.release_at(22, 28);
    assert!(
        h.app.state.wizard.is_some(),
        "cancelled Down must not submit after expiry"
    );
    h.click(22, 28);
    assert!(
        h.app.state.wizard.is_none(),
        "fresh post-deadline click follows legacy confirm once"
    );
}

#[test]
fn c8_contextual_f1_keeps_exact_editor_origin_and_no_closing_key_replay() {
    use orch_tui::wizard::DraftKey;
    let mut h = Harness::sized(FakeBackend::new(), 40, 14);
    h.key('n').type_str("keep input");
    let w = h.app.state.wizard.as_ref().unwrap();
    let before = (w.text_input().value().to_string(), w.step_id().to_string());
    let origin = h
        .app
        .state
        .register_editor_origin(DraftKey::from_wizard(w), w.step_id().into())
        .unwrap();
    h.app.state.register_copy_request(
        origin.clone(),
        orch_tui::backend::CopyTarget::Native,
        Err("safe host failure".into()),
        h.now(),
    );
    h.press(KeyCode::F(1));
    assert!(h.app.state.show_help);
    assert_eq!(h.app.state.copy_help.as_ref().unwrap().origin, origin);
    h.press(KeyCode::End).press(KeyCode::Esc);
    assert!(!h.app.state.show_help);
    assert!(h.app.state.copy_help.is_none());
    let w = h.app.state.wizard.as_ref().unwrap();
    assert_eq!(
        (w.text_input().value().to_string(), w.step_id().to_string()),
        before
    );
    assert!(h.app.state.copy_origin_is_live(&origin));
    assert!(h.app.state.feedback_for(&origin).is_some());
    h.press(KeyCode::F(1)).press(KeyCode::F(1));
    assert!(h.app.state.wizard.is_some());
    assert!(!h.app.state.show_help);
}

#[test]
fn f6_body_click_focuses_without_opening_and_enter_carries_source_anchor() {
    let mut t = make_task("t", "Task", TaskStatus::Todo);
    t.description = (0..60)
        .map(|i| format!("source line{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let expected = t.description.clone();
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![t]));
    h.app.state.detail = DetailPanel::Entity;
    h.app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Description));
    h.draw().press(KeyCode::PageDown).draw();
    let key = h.app.state.pane_key(h.app.state.focus);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    let byte = h.hits().source_anchor(body.x, body.y + 1).unwrap().1;
    h.click(body.x, body.y + 1);
    assert!(h.app.state.viewer.is_none());
    h.press(KeyCode::Enter);
    let doc = h.app.state.viewer.as_ref().unwrap().document();
    assert_eq!(doc.initial_anchor, byte);
    assert_eq!(&*doc.body, expected);
}

#[test]
fn v6_changed_source_discards_an_old_clicked_byte_anchor() {
    let mut t = make_task("t", "Task", TaskStatus::Todo);
    t.description = "old line\nold body\nold end".into();
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![t]));
    h.app.state.detail = DetailPanel::Entity;
    h.app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Description));
    h.draw();
    let key = h.app.state.pane_key(h.app.state.focus);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    h.click(body.x, body.y + 1);
    h.app.state.tasks[0].description = "new".into();
    h.press(KeyCode::Enter);
    let doc = h.app.state.viewer.as_ref().unwrap().document();
    assert_eq!(&*doc.body, "new");
    assert_eq!(doc.initial_anchor, 0);
}

#[test]
fn e9_all_four_exit_sources_reach_the_single_normalized_request_path() {
    for source in 0..4 {
        let mut h = Harness::sized(FakeBackend::new(), 160, 30);
        match source {
            0 => {
                h.key('q');
            }
            1 => {
                h.draw();
                h.click_text("Q quit");
                h.draw();
                h.click_text("Q quit?");
            }
            2 => {
                h.run_command("quit");
            }
            _ => h.app.request_exit(ExitCause::Signal),
        }
        assert!(h.app.state.should_exit, "exit source{source}");
    }
}

#[test]
fn n6_text_suggestions_page_while_home_and_end_keep_editor_cursor_semantics() {
    use orch_tui::wizard::engine::{Choice, StepKind, Wizard, WizardKind, WizardStep};
    let mut h = Harness::new();
    let choices = (0..30)
        .map(|i| Choice::new(&format!("v{i}"), &format!("option{i}"), ""))
        .collect();
    h.app.state.wizard = Some(Wizard::new(
        WizardKind::Task,
        "Pick",
        vec![WizardStep::new("title", "Title", StepKind::Text).suggestions(choices)],
    ));
    h.app.state.input_mode = InputMode::Wizard;
    h.draw();
    h.press(KeyCode::PageDown);
    assert!(h.app.state.wizard.as_ref().unwrap().suggestion_index() > 0);
    h.type_str("opt").press(KeyCode::Home);
    assert_eq!(
        h.app
            .state
            .wizard
            .as_ref()
            .unwrap()
            .text_input()
            .cursor()
            .pos(),
        0
    );
}

#[test]
fn f2_every_optional_detail_section_cycles_once_and_maximize_skips_main_at_all_sizes() {
    use orch_domain::{AgentLastError, AgentStatus, TaskProof};
    use orch_tui::backend::make_agent;
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    for (width, height) in [(40, 14), (80, 24), (120, 40)] {
        let mut t = make_task("t", "Task", TaskStatus::Todo);
        t.attachments = Some(vec!["image.png".into()]);
        t.proof = Some(TaskProof {
            agent_summary: Some("Result".into()),
            files_changed: vec!["src/lib.rs".into()],
            ..TaskProof::default()
        });
        t.goal_id = Some("g".into());
        let mut a = make_agent("a", "Agent", AgentStatus::Error);
        a.last_error = Some(AgentLastError {
            message: "error".into(),
            kind: "other".into(),
            timestamp: "now".into(),
        });
        let mut h = Harness::sized(
            FakeBackend::new()
                .with_tasks(vec![t])
                .with_goals(vec![make_goal("g", "Goal", GoalStatus::Active)])
                .with_agents(vec![a]),
            width,
            height,
        );
        h.app.state.messages = vec![
            Message::new(1, 0, "activity", Token::White, MsgType::Info).with_task(Some("t".into())),
        ];
        for (view, sections) in [
            (ViewId::Tasks, 6),
            (ViewId::Goals, 4),
            (ViewId::Agents, 3),
            (ViewId::Logs, 2),
        ] {
            h.app.state.view = view;
            h.app.state.detail = if view == ViewId::Logs {
                DetailPanel::Log
            } else {
                DetailPanel::Entity
            };
            h.app.state.log_selected_index = 0;
            h.app.state.goal_progress_report = Some("progress".into());
            h.app.state.detail_maximized = false;
            h.app.state.set_focus(Focus::MainList);
            let ring = h.app.state.focus_ring();
            assert_eq!(ring.len(), sections + 1);
            for focus in ring.iter().cycle().skip(1).take(ring.len()) {
                h.press(KeyCode::Tab);
                assert_eq!(&h.app.state.focus, focus);
            }
            h.press(KeyCode::BackTab);
            assert_eq!(h.app.state.focus, *ring.last().unwrap());
            if view != ViewId::Logs {
                h.key('M');
                assert!(!h.app.state.focus_ring().contains(&Focus::MainList));
                h.key('M');
                assert_eq!(h.app.state.focus, Focus::MainList);
            }
        }
    }
}

#[test]
fn v5_source_resolution_keeps_complete_human_and_exact_raw_for_known_envelopes() {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    use orch_tui::widgets::text_viewer::Representation;
    let expected = "First paragraph\n\nprintf 'hello world'\nlast line";
    let raw=serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":expected}}).to_string();
    let mut state = AppState::default();
    state.messages.push(
        Message::new(1, 0, "short preview", Token::White, MsgType::Output)
            .with_detail(Some(raw.clone())),
    );
    let doc = state
        .text_for_source(&ViewerSource::Message(1), 3, 42)
        .unwrap();
    assert_eq!(&*doc.body, expected);
    assert_eq!(doc.representation, Representation::Human);
    assert_eq!(doc.raw_body.as_deref(), Some(raw.as_str()));
    assert_eq!(doc.original_bytes, raw.len());
    let blocks=serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"first\nquote \"one\""},{"type":"text","text":"second"}]}}).to_string();
    state.messages.push(
        Message::new(2, 0, "preview", Token::White, MsgType::Output).with_detail(Some(blocks)),
    );
    let doc = state
        .text_for_source(&ViewerSource::Message(2), 0, 0)
        .unwrap();
    assert_eq!(&*doc.body, "first\nquote \"one\"\nsecond");
    assert!(doc.title.contains("text blocks joined"));
    let unknown = "{\"type\":\"unknown\",\"text\":\"do not infer\"}";
    state.messages.push(
        Message::new(3, 0, "preview", Token::White, MsgType::Output)
            .with_detail(Some(unknown.into())),
    );
    let doc = state
        .text_for_source(&ViewerSource::Message(3), 0, 0)
        .unwrap();
    assert_eq!(&*doc.body, unknown);
    assert_eq!(doc.representation, Representation::Raw);
    assert!(doc.raw_body.is_none());
}

#[test]
fn c8_repainted_field_feedback_does_not_leave_invisible_footer_actions() {
    let mut task = make_task("t", "Task", TaskStatus::Todo);
    task.description = "body".into();
    let mut h = Harness::sized(FakeBackend::new().with_tasks(vec![task]), 40, 14);
    h.app.state.detail = DetailPanel::Entity;
    h.app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Description));
    h.press_mod(KeyCode::Char('y'), KeyModifiers::CONTROL)
        .draw();
    assert!(h.line(13).contains("Copy pending"));
    h.click(5, 13);
    assert_eq!(
        h.app.state.detail,
        DetailPanel::Entity,
        "pending text must not hide an active Esc close hit"
    );
}

#[test]
fn f3_logs_inspector_mouse_maximize_and_keyboard_restore_have_no_hidden_main_focus() {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let mut h = Harness::new();
    h.app.state.view = ViewId::Logs;
    h.app
        .state
        .messages
        .push(Message::new(1, 0, "body", Token::White, MsgType::Info));
    h.press(KeyCode::Up).key('i').draw();
    h.click_text("M max");
    assert!(h.app.state.detail_maximized);
    assert!(!h.app.state.focus_ring().contains(&Focus::MainList));
    h.key('M');
    assert!(!h.app.state.detail_maximized);
    assert_eq!(h.app.state.focus, Focus::MainList);
}

#[test]
fn f4_filter_then_copy_in_one_key_drain_preserves_the_selected_message_id() {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let mut h = Harness::new();
    h.app.state.view = ViewId::Logs;
    h.app.state.messages = vec![
        Message::new(1, 0, "tool", Token::White, MsgType::Tool),
        Message::new(2, 0, "selected B", Token::White, MsgType::Output),
        Message::new(3, 0, "other C", Token::White, MsgType::Output),
    ];
    h.press(KeyCode::Home).press(KeyCode::Down);
    assert_eq!(h.app.state.log_selected_id, Some(2));
    h.key('F')
        .press_mod(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(h.app.backend.copy_requests.last().unwrap().1, "selected B");
}

#[test]
fn v5_existing_subtype_and_generic_content_shapes_decode_but_unknown_errors_stay_raw() {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    use orch_tui::widgets::text_viewer::Representation;
    let mut state = AppState::default();
    for (index,raw) in [r#"{"type":"system","subtype":"task_progress","message":{"content":[{"type":"text","text":"first\nsecond"}]}}"#,r#"{"content":[{"type":"text","text":"first\nsecond"}]}"#].iter().enumerate() {
        let id=index as u64+1;
        state.messages.push(Message::new(id,0,"preview",Token::White,MsgType::Output).with_detail(Some((*raw).into())));
        let doc=state.text_for_source(&ViewerSource::Message(id),0,0).unwrap();assert_eq!(&*doc.body,"first\nsecond");assert_eq!(doc.representation,Representation::Human);assert_eq!(doc.raw_body.as_deref(),Some(*raw));
    }
    for (index, raw) in [
        r#"{ "type": "unknown", "text": "preserve me" }"#,
        r#"{ "error":broken"#,
    ]
    .iter()
    .enumerate()
    {
        let id = index as u64 + 10;
        state.messages.push(
            Message::new(id, 0, "error", Token::Red, MsgType::Error)
                .with_detail(Some((*raw).into())),
        );
        let doc = state
            .text_for_source(&ViewerSource::Message(id), 0, 0)
            .unwrap();
        assert_eq!(&*doc.body, *raw);
        assert_eq!(doc.representation, Representation::Raw);
    }
}

#[test]
fn f4_activity_filter_removal_chooses_nearest_surviving_position() {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let mut h = Harness::new();
    h.app.state.messages = vec![
        Message::new(1, 0, "A", Token::White, MsgType::Tool),
        Message::new(2, 0, "B", Token::White, MsgType::Output),
        Message::new(3, 0, "C", Token::White, MsgType::Tool),
        Message::new(4, 0, "D", Token::White, MsgType::Output),
    ];
    h.app.state.set_focus(Focus::Activity);
    h.draw()
        .press(KeyCode::Home)
        .press(KeyCode::Down)
        .press(KeyCode::Down);
    assert_eq!(h.app.state.activity_selected, Some(3));
    h.key('F');
    h.press_mod(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(h.app.backend.copy_requests.last().unwrap().1, "D");
    assert_eq!(h.app.state.activity_selected, Some(4));
    h.press(KeyCode::Enter);
    assert_eq!(
        h.app.state.viewer.as_ref().unwrap().document().source,
        ViewerSource::Message(4)
    );
}

#[test]
fn f5_activity_selects_then_expands_exact_message_after_runner_redraw() {
    use orch_tui::hit::ZoneId;
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let mut h = Harness::new();
    h.app.state.messages = (1..=3)
        .map(|id| Message::new(id, 0, &format!("message {id}"), Token::White, MsgType::Info))
        .collect();
    h.draw();
    let row = h
        .hits()
        .zones()
        .iter()
        .find(|z| z.id == ZoneId::ActivityMessage(2))
        .unwrap()
        .rect;
    h.click(row.x, row.y);
    assert_eq!(h.app.state.focus, Focus::Activity);
    assert_eq!(h.app.state.activity_selected, Some(2));
    assert!(h.app.state.viewer.is_none());
    h.draw().click(row.x, row.y);
    assert_eq!(
        h.app.state.viewer.as_ref().unwrap().document().source,
        ViewerSource::Message(2)
    );
    assert_eq!(
        &*h.app.state.viewer.as_ref().unwrap().document().body,
        "message 2"
    );
}

#[test]
fn n7_activity_wheel_at_final_never_reenters_tail_but_keyboard_down_does() {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let mut h = Harness::new();
    h.app.state.messages = (1..=3)
        .map(|id| Message::new(id, 0, "body", Token::White, MsgType::Info))
        .collect();
    h.app.state.set_focus(Focus::Activity);
    h.draw();
    h.press(KeyCode::End).press(KeyCode::Up);
    assert!(!h.app.state.activity_follow_tail);
    h.draw();
    let key = h.app.state.pane_key(Focus::Activity);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    h.wheel_down(body.x, body.y);
    assert!(!h.app.state.activity_follow_tail);
    h.press(KeyCode::Down);
    assert!(h.app.state.activity_follow_tail);
}

#[test]
fn n8_textarea_adapter_forwards_three_rows_per_accepted_notch() {
    use orch_tui::hit::{ScrollRegion, ZoneKind};
    use orch_tui::wizard::engine::{StepKind, Wizard, WizardKind, WizardStep};
    let value = (0..80)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut h = Harness::new();
    h.app.state.input_mode = InputMode::Wizard;
    h.app.state.wizard = Some(Wizard::new(
        WizardKind::Task,
        "Task",
        vec![
            WizardStep::new("description", "Description", StepKind::Textarea).default_value(&value),
        ],
    ));
    h.draw();
    for _ in 0..80 {
        h.press(KeyCode::Up);
    }
    h.draw();
    let field = h
        .hits()
        .zones()
        .iter()
        .find(|z| z.kind == ZoneKind::Scroll(ScrollRegion::WizardTextarea))
        .unwrap()
        .rect;
    assert!(h.line(field.y).contains("row00"));
    for _ in 0..4 {
        h.wheel_down(field.x, field.y);
    }
    h.draw();
    assert!(h.line(field.y).contains("row09"), "{}", h.line(field.y));
}

#[test]
fn v6_changed_source_uses_current_scrolled_top_instead_of_stale_click() {
    let mut task = make_task("t", "Task", TaskStatus::Todo);
    task.description = (0..100).map(|i| format!("old line{i:02}\n")).collect();
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![task]));
    h.app.state.detail = DetailPanel::Entity;
    h.app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Description));
    h.draw().press(KeyCode::PageDown).draw();
    let key = h.app.state.pane_key(h.app.state.focus);
    let body = h.hits().pane_metrics(&key).unwrap().body;
    h.click(body.x, body.y + 1);
    let offset = h.app.state.pane_scroll(&key).offset;
    assert!(offset > 0);
    let updated: String = (0..100)
        .map(|i| format!("new longer line{i:02}\n"))
        .collect();
    let expected = updated
        .split_inclusive('\n')
        .take(offset)
        .map(str::len)
        .sum::<usize>();
    h.app.state.tasks[0].description = updated;
    h.press(KeyCode::Enter);
    assert_eq!(
        h.app
            .state
            .viewer
            .as_ref()
            .unwrap()
            .document()
            .initial_anchor,
        expected
    );
}
