//! Shared session/footer lifecycle, exercised with injected milliseconds only.
use orch_tui::state::{AppState, CopyOrigin};
use orch_tui::widgets::text_viewer::ViewerSource;
use orch_tui::wizard::{DraftKey, WizardKind};

fn editor(state: &mut AppState, step: &str) -> CopyOrigin {
    state
        .register_editor_origin(
            DraftKey {
                kind: WizardKind::Task,
                target_id: None,
                setting: None,
                template_key: None,
            },
            step.into(),
        )
        .unwrap()
}

#[test]
fn footer_guard_expires_at_the_boundary_and_ignores_foreign_origins() {
    let mut state = AppState::default();
    let origin = editor(&mut state, "description");
    state.suppress_text_footer(&origin, 100);
    assert!(state.text_footer_suppressed(&origin, 499));
    assert!(!state.text_footer_suppressed(&origin, 500));
    let foreign = CopyOrigin::Viewer {
        source: ViewerSource::Message(99),
        generation: 99,
    };
    state.suppress_text_footer(&foreign, 400);
    assert!(!state.text_footer_suppressed(&foreign, 401));
    assert!(state.text_footer_suppressed(&origin, 499));
}

#[test]
fn temporary_viewer_preserves_editor_guard_but_step_replacement_retires_it() {
    let mut state = AppState::default();
    let origin = editor(&mut state, "description");
    state.suppress_text_footer(&origin, 100);
    let viewer = state
        .register_viewer_origin(ViewerSource::Message(1))
        .unwrap();
    state.retire_copy_origin(&viewer);
    assert!(state.text_footer_suppressed(&origin, 101));
    let next = editor(&mut state, "title");
    assert!(!state.text_footer_suppressed(&origin, 101));
    assert!(!state.text_footer_suppressed(&next, 101));
}

#[test]
fn feedback_operations_and_help_roundtrip_do_not_erase_or_extend_live_guard() {
    use orch_tui::backend::{CopyOutcome, CopyResult, CopyTarget, CopyTicket};
    use orch_tui::state::CopyHelpContext;
    let mut state = AppState::default();
    let origin = editor(&mut state, "description");
    state.suppress_text_footer(&origin, 100);
    state.copy_help = Some(CopyHelpContext {
        origin: origin.clone(),
        scroll_row: 3,
    });
    state.show_help = true;
    state.register_copy_request(origin.clone(), CopyTarget::Auto, Ok(CopyTicket(1)), 110);
    state.dismiss_copy_feedback(&origin);
    state.apply_copy_result(
        CopyResult {
            ticket: CopyTicket(1),
            outcome: CopyOutcome::NativeWritten,
            guidance: None,
        },
        120,
    );
    state.clear_copy_feedback(&origin);
    state.show_help = false;
    state.copy_help = None;
    assert!(state.text_footer_suppressed(&origin, 499));
    assert!(!state.text_footer_suppressed(&origin, 500));
}

#[test]
fn injected_clock_guard_is_exact_for_the_live_epoch_and_saturates_deadline() {
    use orch_tui::{Clock, TestClock};
    let clock = TestClock::new(100);
    let mut state = AppState::default();
    let origin = editor(&mut state, "description");
    state.suppress_text_footer(&origin, clock.now_ms());
    clock.advance_ms(399);
    assert!(state.text_footer_suppressed(&origin, clock.now_ms()));
    clock.advance_ms(1);
    assert!(!state.text_footer_suppressed(&origin, clock.now_ms()));
    state.suppress_text_footer(&origin, i64::MAX - 1);
    assert!(state.text_footer_suppressed(&origin, i64::MAX - 1));
    assert!(!state.text_footer_suppressed(&origin, i64::MAX));
}

#[test]
fn field_or_stale_epoch_cannot_replace_a_live_footer_guard() {
    let mut state = AppState::default();
    let stale = editor(&mut state, "description");
    let live = editor(&mut state, "description");
    state.suppress_text_footer(&live, 100);
    state.suppress_text_footer(&stale, 200);
    let field = state
        .register_field_origin(ViewerSource::Message(1))
        .unwrap();
    state.suppress_text_footer(&field, 200);
    assert!(!state.text_footer_suppressed(&stale, 201));
    assert!(!state.text_footer_suppressed(&field, 201));
    assert!(state.text_footer_suppressed(&live, 499));
    assert!(!state.text_footer_suppressed(&live, 500));
}

#[test]
fn active_origin_obeys_modal_priority_without_cloning_editor_text() {
    use orch_tui::state::InputMode;
    use orch_tui::wizard::{LeaveChoice, LeaveIntent, PendingLeave};
    let mut state = AppState::default();
    let field = state
        .register_field_origin(ViewerSource::Message(1))
        .unwrap();
    assert_eq!(state.active_copy_origin(), Some(field));
    let editor = editor(&mut state, "description");
    assert_eq!(state.active_copy_origin(), Some(editor.clone()));
    let viewer = state
        .register_viewer_origin(ViewerSource::Message(2))
        .unwrap();
    assert_eq!(state.active_copy_origin(), Some(viewer.clone()));
    state.retire_copy_origin(&viewer);
    assert_eq!(state.active_copy_origin(), Some(editor));
    state.show_agent_picker = true;
    assert_eq!(state.active_copy_origin(), None);
    state.show_agent_picker = false;
    state.input_mode = InputMode::Command;
    assert_eq!(state.active_copy_origin(), None);
    state.input_mode = InputMode::None;
    state.wizard_sessions.pending_leave = Some(PendingLeave {
        intent: LeaveIntent::Cancel,
        choice: LeaveChoice::Continue,
    });
    assert_eq!(state.active_copy_origin(), None);
}

// B2-specific harness: no shared fixture changes and no runtime filesystem/CLI IO.
use orch_tui::backend::{Call, CopyOutcome, CopyResult, CopyTarget, CopyTicket, FakeBackend};
use orch_tui::hit::{TextAction, TextSurfaceId, ZoneId, ZoneKind};
use orch_tui::keys::{ExitCause, UiEvent};
use orch_tui::views::{Caps, RenderCtx};
use orch_tui::wizard::session::{COPY_ACTIONS, MAX_DRAFT_BYTES, MAX_DRAFTS, MAX_TOTAL_DRAFT_BYTES};
use orch_tui::wizard::{LeaveChoice, StepKind, Values, Wizard, WizardDraft, WizardStep};
use orch_tui::{App, TestClock};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

type TestApp = App<FakeBackend, TestClock>;
fn app() -> TestApp {
    let mut app = App::new(FakeBackend::new(), TestClock::new(1000));
    app.batch_ms = 0;
    app
}
fn field(kind: StepKind, initial: &str) -> Wizard {
    Wizard::new(
        WizardKind::Task,
        "Test",
        vec![WizardStep::new("field", "Field", kind).default_value(initial)],
    )
}
fn key(app: &mut TestApp, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}
fn modified(app: &mut TestApp, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}
fn type_text(app: &mut TestApp, text: &str) {
    for ch in text.chars() {
        key(app, KeyCode::Char(ch));
    }
}
fn choose(app: &mut TestApp, choice: LeaveChoice) {
    let current = app
        .state
        .wizard_sessions
        .pending_leave
        .as_ref()
        .expect("leave dialog");
    assert_eq!(current.choice, LeaveChoice::Continue);
    let downs = match choice {
        LeaveChoice::Continue => 0,
        LeaveChoice::KeepDraft => 1,
        LeaveChoice::Discard => 2,
    };
    for _ in 0..downs {
        key(app, KeyCode::Down);
    }
    key(app, KeyCode::Enter);
}
fn draw(app: &mut TestApp, area: Rect) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut hits = None;
    terminal
        .draw(|frame| {
            let ctx = RenderCtx::new(
                &app.state,
                Caps {
                    can_create_task: true,
                    can_add_agent: true,
                    can_create_goal: true,
                    can_force_stop_agent: true,
                    can_send_message: true,
                },
                1000,
                Rect::new(0, 0, 80, 24),
            );
            if let Some(w) = app.state.wizard.as_ref() {
                orch_tui::widgets::wizard::render(frame, area, &ctx, w);
            }
            orch_tui::wizard::render_session_overlay(frame, Rect::new(0, 0, 80, 24), &ctx);
            hits = Some(ctx.hits.into_inner());
        })
        .unwrap();
    app.set_hits(hits.unwrap());
    terminal
}
fn mouse(app: &mut TestApp, kind: MouseEventKind, x: u16, y: u16) {
    app.handle_mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}
fn click(app: &mut TestApp, x: u16, y: u16) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), x, y);
}
fn origin(app: &TestApp) -> CopyOrigin {
    let w = app.state.wizard.as_ref().unwrap();
    app.state
        .live_copy_origin(&TextSurfaceId::Editor(
            DraftKey::from_wizard(w),
            w.step_id().into(),
        ))
        .unwrap()
        .clone()
}
fn body_line(terminal: &Terminal<TestBackend>, y: u16) -> String {
    (0..80)
        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
        .collect()
}

#[test]
fn e1_clean_escape_closes_once_dirty_escape_defaults_to_continue_without_mutation() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, ""));
    key(&mut app, KeyCode::Esc);
    assert!(app.state.wizard.is_none());
    app.open_wizard(field(StepKind::Textarea, ""));
    type_text(&mut app, "hello");
    app.state.pending_attachments.push("fixture.png".into());
    let caret = app.state.wizard.as_ref().unwrap().textarea().cursor();
    key(&mut app, KeyCode::Esc);
    assert!(app.state.wizard_sessions.pending_leave.is_some());
    choose(&mut app, LeaveChoice::Continue);
    let w = app.state.wizard.as_ref().unwrap();
    assert_eq!(
        (w.current_value(), w.textarea().cursor()),
        ("hello".into(), caret)
    );
    assert_eq!(app.state.pending_attachments, ["fixture.png"]);
}

#[test]
fn e2_and_e10_selection_navigation_and_undo_are_semantically_clean_redo_is_dirty() {
    for kind in [StepKind::Text, StepKind::Textarea] {
        let mut app = app();
        app.open_wizard(field(kind, "base"));
        modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(
            app.state
                .wizard
                .as_ref()
                .unwrap()
                .selected_text()
                .as_deref(),
            Some("e")
        );
        assert!(!app.state.wizard.as_ref().unwrap().semantic_dirty(&[]));
        key(&mut app, KeyCode::Right);
        type_text(&mut app, "!");
        assert!(app.state.wizard.as_ref().unwrap().semantic_dirty(&[]));
        modified(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(!app.state.wizard.as_ref().unwrap().semantic_dirty(&[]));
        modified(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.state.wizard.as_ref().unwrap().semantic_dirty(&[]));
        modified(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(app.state.wizard.as_ref().unwrap().current_value(), "");
    }
}

#[test]
fn reverted_multiselect_membership_is_semantically_clean() {
    use orch_tui::wizard::Choice;
    let mut wizard = Wizard::new(
        WizardKind::Team,
        "Team",
        vec![
            WizardStep::new("members", "Members", StepKind::Multiselect)
                .options(vec![
                    Choice::new("a", "A", ""),
                    Choice::new("b", "B", ""),
                    Choice::new("c", "C", ""),
                ])
                .default_value("a,b"),
            WizardStep::new("name", "Name", StepKind::Text).default_value("Team"),
        ],
    );
    let press = |wizard: &mut Wizard, code| {
        wizard.handle_key(KeyEvent::new(code, KeyModifiers::NONE), 1000)
    };
    assert!(!wizard.semantic_dirty(&[]));
    press(&mut wizard, KeyCode::Char(' '));
    assert!(wizard.semantic_dirty(&[]), "removed a member");
    press(&mut wizard, KeyCode::Char(' '));
    assert_eq!(wizard.current_value(), "b,a");
    assert!(
        !wizard.semantic_dirty(&[]),
        "restored the original membership"
    );
    press(&mut wizard, KeyCode::Down);
    press(&mut wizard, KeyCode::Down);
    press(&mut wizard, KeyCode::Char(' '));
    assert!(wizard.semantic_dirty(&[]), "added a member");
    press(&mut wizard, KeyCode::Home);
    press(&mut wizard, KeyCode::Char(' '));
    assert!(
        wizard.semantic_dirty(&[]),
        "changed membership at the same count"
    );
    press(&mut wizard, KeyCode::Char(' '));
    press(&mut wizard, KeyCode::End);
    press(&mut wizard, KeyCode::Char(' '));
    assert!(!wizard.semantic_dirty(&[]));
    assert!(wizard.semantic_dirty(&["fixture.png".into()]));

    press(&mut wizard, KeyCode::Tab);
    assert_eq!(wizard.step_id(), "name");
    assert!(
        !wizard.semantic_dirty(&[]),
        "saved multiselect scratch is clean"
    );
    let draft = WizardDraft::capture(&wizard, &[]);
    assert_eq!(draft.baseline["members"], "a,b");
    assert_eq!(draft.values["members"], "b,a");
    press(&mut wizard, KeyCode::Esc);
    assert_eq!(wizard.current_value(), "b,a");
    assert!(!wizard.semantic_dirty(&[]));
    press(&mut wizard, KeyCode::Tab);
    let orch_tui::wizard::WizardOutcome::Complete(values) = press(&mut wizard, KeyCode::Enter)
    else {
        panic!("expected the final text step to submit");
    };
    assert_eq!(
        values["members"], "b,a",
        "submission retains insertion order"
    );
}

#[test]
fn selected_single_line_ctrl_a_moves_without_deleting_text() {
    let mut app = app();
    app.open_wizard(field(StepKind::Text, "a界b"));
    modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    modified(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    let wizard = app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.current_value(), "a界b");
    assert_eq!(wizard.text_input().cursor().pos(), 0);
    assert_eq!(wizard.selected_text(), None);
    assert!(!wizard.semantic_dirty(&[]));
}

#[test]
fn selected_single_line_ctrl_e_moves_without_deleting_text() {
    let mut app = app();
    app.open_wizard(field(StepKind::Text, "a界b"));
    modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    modified(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    let wizard = app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.current_value(), "a界b");
    assert_eq!(wizard.text_input().cursor().pos(), 3);
    assert_eq!(wizard.selected_text(), None);
    assert!(!wizard.semantic_dirty(&[]));
}

#[test]
fn single_line_navigation_after_undo_preserves_redo() {
    for event in [
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('E'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('F'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::META),
    ] {
        let mut app = app();
        app.open_wizard(field(StepKind::Text, "a"));
        type_text(&mut app, "b");
        modified(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        app.handle_key(event);
        modified(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(
            app.state.wizard.as_ref().unwrap().current_value(),
            "ab",
            "{event:?}"
        );
    }
}

#[test]
fn selected_single_line_control_navigation_clears_or_extends_selection_without_editing() {
    for (code, mods, position, selected) in [
        ('B', KeyModifiers::CONTROL, 1, None),
        ('f', KeyModifiers::CONTROL, 3, None),
        ('A', KeyModifiers::ALT, 0, None),
        ('b', KeyModifiers::ALT, 0, None),
        ('F', KeyModifiers::META, 3, None),
        (
            'a',
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            0,
            Some("a界b"),
        ),
        (
            'b',
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            1,
            Some("界b"),
        ),
    ] {
        let mut app = app();
        app.open_wizard(field(StepKind::Text, "a界b"));
        modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        modified(&mut app, KeyCode::Char(code), mods);
        let wizard = app.state.wizard.as_ref().unwrap();
        assert_eq!(wizard.current_value(), "a界b", "{code:?} {mods:?}");
        assert_eq!(
            wizard.text_input().cursor().pos(),
            position,
            "{code:?} {mods:?}"
        );
        assert_eq!(
            wizard.selected_text().as_deref(),
            selected,
            "{code:?} {mods:?}"
        );
        modified(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.state.wizard.as_ref().unwrap().current_value(), "a界b");
    }
}

#[test]
fn single_line_ignored_modified_characters_preserve_selection_and_redo() {
    use orch_tui::text::TextInput;
    for (code, mods) in [
        ('g', KeyModifiers::CONTROL),
        ('G', KeyModifiers::CONTROL),
        ('x', KeyModifiers::ALT),
    ] {
        let mut text = TextInput::new("a界");
        text.handle_editor_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE), 0);
        text.handle_editor_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL), 0);
        text.handle_editor_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT), 0);
        let generation = text.generation();
        assert!(!text.handle_editor_key(KeyEvent::new(KeyCode::Char(code), mods), 0));
        assert_eq!(text.value(), "a界");
        assert_eq!(text.selected_text().as_deref(), Some("界"));
        assert_eq!(text.generation(), generation);
        text.handle_editor_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL), 0);
        assert_eq!(text.value(), "a界b");
    }
}

#[test]
fn single_line_noop_edits_preserve_redo_until_source_changes() {
    for event in [
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
    ] {
        let mut app = app();
        app.open_wizard(field(StepKind::Text, "a"));
        type_text(&mut app, "b");
        modified(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Home);
        app.handle_key(event);
        modified(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(
            app.state.wizard.as_ref().unwrap().current_value(),
            "ab",
            "{event:?}"
        );
        modified(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::End);
        type_text(&mut app, "c");
        modified(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(app.state.wizard.as_ref().unwrap().current_value(), "ac");
    }
}

#[test]
fn e3_back_keeps_unadvanced_multiline_scratch_and_exact_draft() {
    let mut app = app();
    let wizard = Wizard::new(
        WizardKind::Task,
        "T",
        vec![
            WizardStep::new("one", "One", StepKind::Text),
            WizardStep::new("two", "Two", StepKind::Textarea),
        ],
    );
    app.open_wizard(wizard);
    type_text(&mut app, "answer");
    key(&mut app, KeyCode::Tab);
    app.handle_paste("line one\r\nline two");
    key(&mut app, KeyCode::Esc);
    choose(&mut app, LeaveChoice::KeepDraft);
    assert_eq!(app.state.wizard.as_ref().unwrap().step_id(), "one");
    key(&mut app, KeyCode::Tab);
    assert_eq!(
        app.state.wizard.as_ref().unwrap().current_value(),
        "line one\nline two"
    );
    let draft = app
        .state
        .wizard_sessions
        .draft_for(&DraftKey::from_wizard(app.state.wizard.as_ref().unwrap()))
        .unwrap();
    assert_eq!(draft.current_text, "line one\nline two");
}

#[test]
fn e4_replace_and_cancel_guard_before_clearing_attachments() {
    let mut app = app();
    app.open_task_wizard();
    type_text(&mut app, "keep");
    app.state.pending_attachments.push("image.png".into());
    app.open_goal_wizard();
    assert_eq!(app.state.wizard.as_ref().unwrap().kind, WizardKind::Task);
    assert_eq!(app.state.pending_attachments, ["image.png"]);
    key(&mut app, KeyCode::Esc);
    assert!(app.state.wizard_sessions.pending_leave.is_none());
    app.cancel_wizard();
    choose(&mut app, LeaveChoice::KeepDraft);
    assert!(app.state.wizard.is_none());
    app.open_task_wizard();
    assert_eq!(app.state.wizard.as_ref().unwrap().text_value(), "keep");
    assert_eq!(app.state.pending_attachments, ["image.png"]);
    assert!(
        app.state
            .wizard
            .as_ref()
            .unwrap()
            .semantic_dirty(&app.state.pending_attachments)
    );
}

#[test]
fn e5_task_primary_failure_retains_live_form_attachments_and_matching_draft_then_success_clears_it()
{
    let mut app = app();
    app.open_task_wizard();
    type_text(&mut app, "Task");
    key(&mut app, KeyCode::Tab);
    type_text(&mut app, "details");
    key(&mut app, KeyCode::Tab);
    app.state.pending_attachments.push("isolated.png".into());
    let before = app.state.wizard.as_ref().unwrap().step_id().to_string();
    app.backend.fail_next = Some("primary unavailable".into());
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.state.wizard.as_ref().unwrap().step_id(), before);
    assert_eq!(app.state.pending_attachments, ["isolated.png"]);
    let k = DraftKey::from_wizard(app.state.wizard.as_ref().unwrap());
    assert_eq!(
        app.state.wizard_sessions.draft_for(&k).unwrap().attachments,
        ["isolated.png"]
    );
    assert_eq!(app.state.input_mode, orch_tui::state::InputMode::Wizard);
    key(&mut app, KeyCode::Tab);
    assert!(app.state.wizard.is_none());
    assert!(app.state.wizard_sessions.draft_for(&k).is_none());
    assert!(app.state.pending_attachments.is_empty());
    assert_eq!(
        app.backend
            .calls
            .iter()
            .filter(|c| matches!(c, Call::CreateTask { .. }))
            .count(),
        2
    );
}

#[test]
fn e5_edit_goal_failure_and_config_persistence_failure_keep_the_form_and_old_setting() {
    let mut app = app();
    let goal = orch_tui::backend::make_goal("goal_a", "Before", orch_domain::GoalStatus::Active);
    app.backend.goals.push(goal.clone());
    app.state.goals.push(goal.clone());
    app.open_wizard(orch_tui::wizard::configs::edit_goal_wizard(&goal, &[]));
    app.backend.fail_next = Some("update refused".into());
    let values = [
        ("title".into(), "After".into()),
        ("description".into(), "desc".into()),
    ]
    .into_iter()
    .collect();
    app.submit_wizard(values);
    assert!(
        app.state
            .wizard
            .as_ref()
            .unwrap()
            .session_error
            .as_deref()
            .unwrap()
            .contains("refused")
    );
    assert_eq!(app.backend.goals[0].title, "Before");
    app.state.wizard = None;
    app.state.wizard_sessions = Default::default();
    app.open_config_wizard("max-concurrent");
    let before = app.state.max_concurrent;
    let key_id = DraftKey::from_wizard(app.state.wizard.as_ref().unwrap());
    key(&mut app, KeyCode::Down);
    app.backend.fail_next = Some("save refused".into());
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.state.max_concurrent, before);
    assert!(app.state.wizard.is_some());
    assert!(app.state.wizard_sessions.draft_for(&key_id).is_some());
    assert!(
        !app.state
            .messages
            .iter()
            .any(|m| m.text == "Settings saved")
    );
    key(&mut app, KeyCode::Enter);
    assert!(app.state.wizard.is_none());
    assert_ne!(app.state.max_concurrent, before);
}

#[test]
fn e6_draft_keys_separate_new_edit_targets_settings_and_templates() {
    let base = field(StepKind::Text, "draft");
    let mut store = orch_tui::wizard::WizardSessionStore::default();
    for wizard in [
        base.clone(),
        base.clone().with_target("A"),
        base.clone().with_target("B"),
        base.clone().with_setting("one"),
        base.clone().with_setting("two"),
        base.with_template("shop"),
    ] {
        let draft = WizardDraft::capture(&wizard, &["attachment".into()]);
        let key = draft.key.clone();
        store.save_draft(draft).unwrap();
        assert_eq!(store.draft_for(&key).unwrap().attachments, ["attachment"]);
    }
    assert_eq!(store.draft_count(), 6);
}

fn sized_draft(id: usize, bytes: usize) -> WizardDraft {
    let mut draft =
        WizardDraft::capture(&field(StepKind::Text, "").with_target(&id.to_string()), &[]);
    let size = draft.bytes();
    assert!(size <= bytes);
    draft.attachments.push("x".repeat(bytes - size));
    assert_eq!(draft.bytes(), bytes);
    draft
}
#[test]
fn e7_all_count_individual_and_total_boundaries_refuse_without_eviction() {
    let mut store = orch_tui::wizard::WizardSessionStore::default();
    for i in 0..MAX_DRAFTS {
        store.save_draft(sized_draft(i, 128)).unwrap();
    }
    let old = store.total_bytes();
    assert!(store.save_draft(sized_draft(99, 128)).is_err());
    assert_eq!(store.total_bytes(), old);
    store.save_draft(sized_draft(0, MAX_DRAFT_BYTES)).unwrap();
    assert!(
        store
            .save_draft(sized_draft(0, MAX_DRAFT_BYTES + 1))
            .is_err()
    );
    assert_eq!(
        store.draft_for(&sized_draft(0, 128).key).unwrap().bytes(),
        MAX_DRAFT_BYTES
    );
    let mut store = orch_tui::wizard::WizardSessionStore::default();
    for i in 0..8 {
        store.save_draft(sized_draft(i, MAX_DRAFT_BYTES)).unwrap();
    }
    assert_eq!(store.total_bytes(), MAX_TOTAL_DRAFT_BYTES);
    assert!(store.save_draft(sized_draft(9, 128)).is_err());
    assert_eq!(store.draft_count(), 8);
    let key = sized_draft(0, 128).key;
    store.discard_draft(&key);
    store.save_draft(sized_draft(9, MAX_DRAFT_BYTES)).unwrap();
    assert_eq!(store.total_bytes(), MAX_TOTAL_DRAFT_BYTES);
}

#[test]
fn e7_oversized_keep_draft_leaves_editor_and_discard_available() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, ""));
    app.state
        .pending_attachments
        .push("x".repeat(MAX_DRAFT_BYTES + 1));
    key(&mut app, KeyCode::Esc);
    choose(&mut app, LeaveChoice::KeepDraft);
    assert!(
        app.state
            .wizard_sessions
            .error
            .as_deref()
            .unwrap()
            .contains("256 KiB")
    );
    assert!(app.state.wizard.is_some());
    assert_eq!(app.state.wizard_sessions.draft_count(), 0);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(app.state.wizard.is_none());
}

#[test]
fn e8_stale_edit_draft_is_recovery_only_until_explicit_discard_restart() {
    let mut app = app();
    let task = orch_tui::backend::make_task("A", "Original", orch_domain::TaskStatus::Todo);
    app.state.tasks.push(task.clone());
    app.backend.tasks.push(task.clone());
    app.open_wizard(orch_tui::wizard::configs::edit_task_wizard(&task, &[]));
    type_text(&mut app, " changed");
    app.cancel_wizard();
    choose(&mut app, LeaveChoice::KeepDraft);
    let mut changed = task;
    changed.title = "Remote edit".into();
    app.state.tasks[0] = changed.clone();
    app.open_wizard(orch_tui::wizard::configs::edit_task_wizard(&changed, &[]));
    let before = app.state.wizard.as_ref().unwrap().current_value();
    assert!(
        app.state
            .viewer
            .as_ref()
            .unwrap()
            .document()
            .body
            .contains("Original changed")
    );
    // B1 owns viewer closure; simulate that lifecycle seam on the I0 stub branch.
    let viewer_origin = app.state.active_copy_origin().unwrap();
    app.state.retire_copy_origin(&viewer_origin);
    app.state.viewer = None;
    key(&mut app, KeyCode::Tab);
    app.submit_wizard(Values::new());
    assert_eq!(app.state.wizard.as_ref().unwrap().current_value(), before);
    assert!(
        !app.backend
            .calls
            .iter()
            .any(|c| matches!(c, Call::UpdateTask { .. }))
    );
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.state.wizard.as_ref().unwrap().current_value(),
        "Remote edit"
    );
    assert_eq!(app.state.wizard_sessions.draft_count(), 0);
}

#[test]
fn e9_saved_draft_without_wizard_guards_user_exit_once_and_signal_never_prompts() {
    let mut app = app();
    app.open_wizard(field(StepKind::Text, ""));
    type_text(&mut app, "keep");
    app.cancel_wizard();
    choose(&mut app, LeaveChoice::KeepDraft);
    assert!(app.state.wizard.is_none());
    app.request_exit(ExitCause::User);
    assert!(!app.state.should_exit);
    let terminal = draw(&mut app, Rect::new(1, 16, 78, 7));
    assert!((0..24).any(|y| body_line(&terminal, y).contains("RAM only")));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.state.wizard_sessions.draft_count(), 1);
    app.request_exit(ExitCause::User);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(app.state.should_exit);
    assert_eq!(app.state.wizard_sessions.draft_count(), 0);
    let mut signal = super_app_with_draft();
    signal.request_exit(ExitCause::Signal);
    assert!(signal.state.should_exit);
    assert!(signal.state.wizard_sessions.pending_leave.is_none());
}
fn super_app_with_draft() -> TestApp {
    let mut a = app();
    a.open_wizard(field(StepKind::Text, ""));
    type_text(&mut a, "d");
    a.cancel_wizard();
    choose(&mut a, LeaveChoice::KeepDraft);
    a
}

#[test]
fn e10_f6_end_enter_native_and_selection_auto_copy_preserve_both_editor_kinds() {
    for kind in [StepKind::Text, StepKind::Textarea] {
        let mut app = app();
        app.open_wizard(field(kind, "hello界"));
        modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        let caret = if kind == StepKind::Textarea {
            app.state.wizard.as_ref().unwrap().textarea().cursor().1
        } else {
            app.state
                .wizard
                .as_ref()
                .unwrap()
                .text_input()
                .cursor()
                .pos()
        };
        key(&mut app, KeyCode::F(6));
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.backend.copy_requests,
            [(CopyTarget::Native, "界".into())]
        );
        assert_eq!(
            app.state
                .wizard
                .as_ref()
                .unwrap()
                .selected_text()
                .as_deref(),
            Some("界")
        );
        modified(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(
            app.backend.copy_requests[1],
            (CopyTarget::Auto, "hello界".into())
        );
        let after = if kind == StepKind::Textarea {
            app.state.wizard.as_ref().unwrap().textarea().cursor().1
        } else {
            app.state
                .wizard
                .as_ref()
                .unwrap()
                .text_input()
                .cursor()
                .pos()
        };
        assert_eq!(after, caret);
        assert!(!app.state.wizard.as_ref().unwrap().semantic_dirty(&[]));
    }
}

#[test]
fn c7_compact_inner_geometry_and_fixed_rail_slots_never_overlap() {
    for height in [2, 3, 4, 5, 6, 7, 8, 9, 10] {
        let mut app = app();
        app.open_wizard(field(StepKind::Textarea, "one\ntwo\nthree"));
        let area = Rect::new(1, 10, 38, height);
        let expected =
            orch_tui::widgets::wizard::field_rect(area, app.state.wizard.as_ref().unwrap());
        assert!(expected.height >= 1);
        assert!(expected.bottom() < area.bottom());
        let terminal = draw(&mut app, area);
        assert!(body_line(&terminal, area.bottom() - 1).contains("Copy… (F6)"));
        let body = app
            .state
            .mouse
            .hits
            .zones()
            .iter()
            .find(|z| matches!(z.id, ZoneId::EditorText(..)))
            .unwrap();
        assert_eq!(body.rect, expected);
        for (index, action) in COPY_ACTIONS.iter().enumerate() {
            app.state.wizard.as_mut().unwrap().copy_rail = Some(index);
            let terminal = draw(&mut app, area);
            let row = body_line(&terminal, area.bottom() - 1);
            assert_eq!(&row[2..3], "<");
            for (offset, action) in [
                (1, TextAction::PreviousCopyAction),
                (33, TextAction::NextCopyAction),
                (35, TextAction::CloseCopyRail),
            ] {
                assert!(
                    matches!(app.state.mouse.hits.resolve(area.x+offset,area.bottom()-1).unwrap().kind,ZoneKind::TextAction(a) if a==action)
                );
            }
            let label = app
                .state
                .mouse
                .hits
                .zones()
                .iter()
                .find(|z| z.kind == ZoneKind::TextAction(*action))
                .unwrap();
            assert_eq!(label.rect.x, area.x + 3);
            assert!(label.rect.width <= 29);
        }
    }
}

#[test]
fn v4_editor_drag_uses_library_mapping_and_copies_exact_unicode_bytes() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, "a界b\nnext"));
    let area = Rect::new(1, 16, 38, 7);
    draw(&mut app, area);
    let body = orch_tui::widgets::wizard::field_rect(area, app.state.wizard.as_ref().unwrap());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 3,
        body.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        body.x + 3,
        body.y,
    );
    assert_eq!(
        app.state
            .wizard
            .as_ref()
            .unwrap()
            .selected_text()
            .as_deref(),
        Some("界")
    );
    modified(
        &mut app,
        KeyCode::Char('y'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(app.backend.copy_requests, [(CopyTarget::Auto, "界".into())]);
    assert!(!app.state.wizard.as_ref().unwrap().semantic_dirty(&[]));
}

#[test]
fn c8_pending_dismissal_preserves_association_matching_completion_reappears_and_close_retires() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, "text"));
    let origin = origin(&app);
    modified(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::F(6));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.state.copy_feedback.pending_count(), 1);
    assert!(!app.state.copy_footer_visible(&origin));
    app.state.apply_copy_result(
        CopyResult {
            ticket: CopyTicket(1),
            outcome: CopyOutcome::NativeWritten,
            guidance: None,
        },
        1001,
    );
    app.dispatch_ui_event(&UiEvent::Tick);
    let terminal = draw(&mut app, Rect::new(1, 16, 38, 7));
    assert!(body_line(&terminal, 22).contains("Copied on this host"));
    app.cancel_wizard();
    assert!(!app.state.copy_origin_is_live(&origin));
    app.open_wizard(field(StepKind::Textarea, "text"));
    assert_ne!(origin, crate_origin(&app));
}
fn crate_origin(app: &TestApp) -> CopyOrigin {
    origin(app)
}

#[test]
fn c7_footer_click_arms_suppression_before_mode_change_and_invalidates_hits() {
    let mut app = app();
    app.open_wizard(field(StepKind::Text, "text"));
    let origin = origin(&app);
    draw(&mut app, Rect::new(1, 16, 38, 7));
    click(&mut app, 2, 22);
    assert_eq!(app.state.wizard.as_ref().unwrap().copy_rail, Some(0));
    assert!(!app.state.mouse.hits.is_valid());
    assert!(app.state.text_footer_suppressed(&origin, 1399));
    assert!(!app.state.text_footer_suppressed(&origin, 1400));
    assert!(app.backend.copy_requests.is_empty());
}

// Inject failure after a successful primary write, entirely at the backend seam.
use orch_domain::*;
use orch_tui::backend::*;
struct AssignmentFailure {
    inner: FakeBackend,
}
impl TuiBackend for AssignmentFailure {
    fn context(&self) -> LaunchContext {
        self.inner.context()
    }
    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        self.inner.refresh_tasks()
    }
    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
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
    fn assign_task(&mut self, _task_id: &str, _agent_id: &str) -> BackendResult<()> {
        Err("assignment unavailable".into())
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

#[test]
fn e5_partial_assignment_failure_reports_committed_id_and_never_retains_retry_create() {
    let mut app = App::new(
        AssignmentFailure {
            inner: FakeBackend::new(),
        },
        TestClock::new(0),
    );
    app.batch_ms = 0;
    app.open_task_wizard();
    let key_id = DraftKey::from_wizard(app.state.wizard.as_ref().unwrap());
    let values = [
        ("title".into(), "Primary".into()),
        ("assignee".into(), "agent_a".into()),
    ]
    .into_iter()
    .collect();
    app.submit_wizard(values);
    assert!(app.state.wizard.is_none());
    assert!(app.state.wizard_sessions.draft_for(&key_id).is_none());
    assert_eq!(app.backend.inner.tasks.len(), 1);
    let id = &app.backend.inner.tasks[0].id;
    assert!(
        app.state
            .messages
            .iter()
            .any(|m| m.text.contains(id) && m.text.contains("assignment failed"))
    );
    app.submit_wizard(Values::new());
    assert_eq!(app.backend.inner.tasks.len(), 1);
}

#[test]
fn e5_all_config_saves_fail_without_local_mutation_or_false_success() {
    for (setting, field, value) in [
        ("palette", "palette", "ocean"),
        ("activity-filter", "activity_filter", "errors"),
        ("notifications-toast", "notifications_toast", "false"),
        ("notifications-bell", "notifications_bell", "true"),
    ] {
        let mut app = app();
        app.open_config_wizard(setting);
        let palette = app.state.palette.name;
        let notifications = app.state.notifications;
        let filter = app.state.activity_filter.clone();
        app.backend.fail_next = Some("persistence failed".into());
        app.submit_wizard(
            [(field.to_string(), value.to_string())]
                .into_iter()
                .collect(),
        );
        assert!(app.state.wizard.is_some(), "{setting}");
        assert_eq!(app.state.palette.name, palette);
        assert_eq!(app.state.notifications, notifications);
        assert_eq!(app.state.activity_filter, filter);
        assert!(
            !app.state
                .messages
                .iter()
                .any(|m| m.text == "Settings saved")
        );
    }
}

#[test]
fn e5_shop_invalid_keeps_form_valid_advances_to_next_wizard() {
    let mut app = app();
    app.open_shop_wizard();
    app.submit_wizard(
        [("shop_template".into(), "missing-template".into())]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        app.state.wizard.as_ref().unwrap().kind,
        WizardKind::AgentShop
    );
    let template = &orch_domain::AGENT_SHOP_TEMPLATES[0];
    app.submit_wizard(
        [("shop_template".into(), template.key.to_string())]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        app.state.wizard.as_ref().unwrap().kind,
        WizardKind::AgentFromShop
    );
    assert_eq!(
        app.state.wizard.as_ref().unwrap().template_key.as_deref(),
        Some(template.key)
    );
    assert_eq!(app.state.input_mode, orch_tui::state::InputMode::Wizard);
}

#[test]
fn e6_only_matching_successful_draft_is_cleared() {
    let mut app = super_app_with_draft();
    let other = field(StepKind::Text, "").with_setting("other");
    let other_key = DraftKey::from_wizard(&other);
    app.state
        .wizard_sessions
        .save_draft(WizardDraft::capture(&other, &[]))
        .unwrap();
    app.open_wizard(field(StepKind::Text, ""));
    app.submit_wizard([("title".into(), "Committed".into())].into_iter().collect());
    assert_eq!(app.state.wizard_sessions.draft_count(), 1);
    assert!(app.state.wizard_sessions.draft_for(&other_key).is_some());
}

#[test]
fn e2_initial_effective_defaults_and_crlf_outer_whitespace_are_clean_including_skips() {
    let mut w = Wizard::new(
        WizardKind::Task,
        "T",
        vec![
            WizardStep::new("one", "One", StepKind::Textarea).default_value("a\r\nb"),
            WizardStep::new("skip", "Skipped", StepKind::Text)
                .default_value("ignored")
                .always_skip(),
            WizardStep::new("last", "Last", StepKind::Text).default_value("default"),
        ],
    );
    assert!(!w.semantic_dirty(&[]));
    w.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 0);
    assert!(!w.semantic_dirty(&[]));
    assert_eq!(w.step_id(), "last");
    w.insert_paste(" ", 0);
    assert!(!w.semantic_dirty(&[]));
}

#[test]
fn c6_help_owned_keys_and_f1_are_declined_without_changing_editor_rail_selection_or_history() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, "abc"));
    modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::F(6));
    key(&mut app, KeyCode::End);
    let before = origin(&app);
    let text = app.state.wizard.as_ref().unwrap().current_value();
    // B0 owns help routing; B2 must decline the event and preserve state.
    key(&mut app, KeyCode::F(1));
    assert_eq!(app.state.wizard.as_ref().unwrap().copy_rail, Some(3));
    app.state.show_help = true;
    app.state.copy_help = Some(orch_tui::state::CopyHelpContext {
        origin: before.clone(),
        scroll_row: 0,
    });
    app.dispatch_ui_event(&UiEvent::Tick);
    assert_eq!(origin(&app), before);
    assert_eq!(app.state.wizard.as_ref().unwrap().current_value(), text);
    assert_eq!(
        app.state
            .wizard
            .as_ref()
            .unwrap()
            .selected_text()
            .as_deref(),
        Some("c")
    );
}

#[test]
fn e4_empty_field_backspace_and_mouse_back_use_the_same_dirty_guard() {
    let mut app = app();
    app.open_wizard(Wizard::new(
        WizardKind::Task,
        "T",
        vec![
            WizardStep::new("one", "One", StepKind::Text),
            WizardStep::new("two", "Two", StepKind::Text),
        ],
    ));
    type_text(&mut app, "answer");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.state.wizard.as_ref().unwrap().step_id(), "two");
    key(&mut app, KeyCode::Esc);
    draw(&mut app, Rect::new(1, 16, 38, 7));
    let back = app
        .state
        .mouse
        .hits
        .zones()
        .iter()
        .find(|z| z.kind == ZoneKind::Verb(orch_tui::hit::VerbId::WizardBack))
        .unwrap()
        .rect;
    click(&mut app, back.x, back.y);
    assert!(app.state.wizard_sessions.pending_leave.is_some());
    assert_eq!(app.state.wizard.as_ref().unwrap().step_id(), "two");
}

#[test]
fn e5_each_primary_helper_failure_and_missing_target_is_explicit_and_retains_input() {
    for kind in [
        WizardKind::Task,
        WizardKind::EditTask,
        WizardKind::Agent,
        WizardKind::EditAgent,
        WizardKind::Team,
        WizardKind::Goal,
        WizardKind::EditGoal,
    ] {
        let mut app = app();
        let mut wizard = field(StepKind::Textarea, "unchanged");
        wizard.kind = kind.clone();
        wizard.target_id = Some("target".into());
        app.open_wizard(wizard);
        app.backend.fail_next = Some("primary fail".into());
        app.submit_wizard(Values::new());
        assert_eq!(
            app.state.wizard.as_ref().unwrap().current_value(),
            "unchanged",
            "{kind:?}"
        );
        assert_eq!(
            app.state.wizard.as_ref().unwrap().session_error.as_deref(),
            Some("primary fail")
        );
        assert_eq!(app.state.wizard_sessions.draft_count(), 1);
    }
    for kind in [
        WizardKind::EditTask,
        WizardKind::EditAgent,
        WizardKind::EditGoal,
        WizardKind::Config,
    ] {
        let mut app = app();
        let mut wizard = field(StepKind::Text, "keep");
        wizard.kind = kind;
        app.open_wizard(wizard);
        app.submit_wizard(Values::new());
        assert!(
            app.state
                .wizard
                .as_ref()
                .unwrap()
                .session_error
                .as_deref()
                .unwrap()
                .contains("Missing")
        );
    }
}

#[test]
fn e8_deleted_target_draft_opens_a_frozen_recovery_document_and_blocks_submission() {
    let mut app = app();
    let goal = make_goal("gone", "Goal", GoalStatus::Active);
    app.state.goals.push(goal.clone());
    app.open_wizard(orch_tui::wizard::configs::edit_goal_wizard(&goal, &[]));
    type_text(&mut app, " edited");
    app.cancel_wizard();
    choose(&mut app, LeaveChoice::KeepDraft);
    app.state.goals.clear();
    app.open_wizard(orch_tui::wizard::configs::edit_goal_wizard(&goal, &[]));
    assert!(
        app.state
            .viewer
            .as_ref()
            .unwrap()
            .document()
            .title
            .contains("recovery only")
    );
    let before = app.backend.calls.len();
    app.submit_wizard(Values::new());
    assert_eq!(app.backend.calls.len(), before);
    assert!(app.state.wizard.is_some());
}

#[test]
fn e6_exact_edit_key_restores_caret_range_attachments_and_original_baseline() {
    let mut app = app();
    let task = make_task("A", "Text界", TaskStatus::Todo);
    app.state.tasks.push(task.clone());
    let mut wizard = field(StepKind::Textarea, "base");
    wizard.kind = WizardKind::EditTask;
    wizard.target_id = Some(task.id.clone());
    app.open_wizard(wizard.clone());
    type_text(&mut app, " changed");
    modified(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    let caret = app.state.wizard.as_ref().unwrap().textarea().cursor();
    let selected = app.state.wizard.as_ref().unwrap().selected_text();
    app.state
        .pending_attachments
        .push("fixture/path.png".into());
    app.cancel_wizard();
    choose(&mut app, LeaveChoice::KeepDraft);
    app.open_wizard(wizard);
    assert_eq!(
        app.state.wizard.as_ref().unwrap().textarea().cursor(),
        caret
    );
    assert_eq!(app.state.wizard.as_ref().unwrap().selected_text(), selected);
    assert_eq!(app.state.pending_attachments, ["fixture/path.png"]);
    assert!(
        app.state
            .wizard
            .as_ref()
            .unwrap()
            .semantic_dirty(&app.state.pending_attachments)
    );
}

#[test]
fn e10_no_selection_makes_no_transport_attempt_and_rail_cancel_never_leaves_editor() {
    let mut app = app();
    app.open_wizard(field(StepKind::Text, "text"));
    modified(
        &mut app,
        KeyCode::Char('y'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(app.backend.copy_requests.is_empty());
    key(&mut app, KeyCode::F(6));
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Esc);
    assert!(app.state.wizard.is_some());
    assert!(app.state.wizard_sessions.pending_leave.is_none());
    assert_eq!(app.state.wizard.as_ref().unwrap().current_value(), "text");
}

#[test]
fn v4_single_line_drag_maps_graphemes_and_capture_or_geometry_loss_cancels_only_the_drag() {
    let mut app = app();
    app.open_wizard(field(StepKind::Text, "a界bc"));
    key(&mut app, KeyCode::Home); // Cursor block precedes the displayed text.
    let area = Rect::new(1, 16, 38, 7);
    draw(&mut app, area);
    let body = orch_tui::widgets::wizard::field_rect(area, app.state.wizard.as_ref().unwrap());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 4,
        body.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 7,
        body.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        body.x + 7,
        body.y,
    );
    assert_eq!(
        app.state
            .wizard
            .as_ref()
            .unwrap()
            .selected_text()
            .as_deref(),
        Some("界b")
    );
    draw(&mut app, area);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 2,
        body.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 4,
        body.y,
    );
    let before = app.state.wizard.as_ref().unwrap().selected_text();
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::F(2));
    draw(&mut app, area);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 8,
        body.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        body.x + 8,
        body.y,
    );
    assert_eq!(app.state.wizard.as_ref().unwrap().selected_text(), before);
}

#[test]
fn c7_rail_mouse_wrap_navigation_has_fixed_identity_and_no_mode_deadline() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, "text"));
    let origin = origin(&app);
    key(&mut app, KeyCode::F(6));
    let area = Rect::new(1, 16, 38, 7);
    for expected in [1, 2, 3, 0] {
        draw(&mut app, area);
        click(&mut app, area.x + 33, area.bottom() - 1);
        assert_eq!(app.state.wizard.as_ref().unwrap().copy_rail, Some(expected));
        assert!(!app.state.text_footer_suppressed(&origin, 1000));
        assert!(app.backend.copy_requests.is_empty());
    }
    draw(&mut app, area);
    click(&mut app, area.x + 35, area.bottom() - 1);
    assert_eq!(app.state.wizard.as_ref().unwrap().copy_rail, None);
    assert!(app.state.text_footer_suppressed(&origin, 1000));
}

#[test]
fn c8_immediate_failure_and_foreign_completion_never_claim_editor_success() {
    let mut app = app();
    app.open_wizard(field(StepKind::Textarea, "text"));
    let origin = origin(&app);
    app.backend.copy_fail_next = Some("denied".into());
    key(&mut app, KeyCode::F(6));
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    let terminal = draw(&mut app, Rect::new(1, 16, 38, 7));
    assert!(body_line(&terminal, 22).contains("Host failed"));
    let viewer = app
        .state
        .register_viewer_origin(ViewerSource::Message(9))
        .unwrap();
    app.state
        .register_copy_request(viewer, CopyTarget::Native, Ok(CopyTicket(77)), 1001);
    app.state.apply_copy_result(
        CopyResult {
            ticket: CopyTicket(77),
            outcome: CopyOutcome::NativeWritten,
            guidance: None,
        },
        1002,
    );
    assert!(matches!(
        app.state.feedback_for(&origin).unwrap().outcome,
        CopyOutcome::Failed(_)
    ));
    let terminal = draw(&mut app, Rect::new(1, 16, 38, 7));
    assert!(body_line(&terminal, 22).contains("Host failed"));
}

#[test]
fn n8_catalog_reordering_preserves_semantic_choice_and_untouched_removed_defaults_are_clean() {
    use orch_tui::wizard::Choice;
    let step = |options: Vec<Choice>| {
        WizardStep::new("model", "Model", StepKind::Select)
            .options(options)
            .default_value("a")
    };
    let a = Choice::new("a", "A", "");
    let b = Choice::new("b", "B", "");
    let mut wizard = Wizard::new(
        WizardKind::Agent,
        "A",
        vec![step(vec![a.clone(), b.clone()])],
    );
    wizard.replace_steps(vec![step(vec![b.clone(), a])]);
    assert_eq!(wizard.current_value(), "a");
    assert!(!wizard.semantic_dirty(&[]));
    wizard.replace_steps(vec![step(vec![b])]);
    assert_eq!(wizard.current_value(), "b");
    assert!(!wizard.semantic_dirty(&[]));
}
