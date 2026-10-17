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
