//! Foundation copy contracts: no host clipboard or process is involved.
use orch_tui::backend::{CopyOutcome, CopyResult, CopyTarget, FakeBackend, TuiBackend};
use orch_tui::state::AppState;
use orch_tui::widgets::text_viewer::ViewerSource;

#[test]
fn completed_undrained_tickets_stay_busy_until_polled() {
    let mut backend = FakeBackend::new();
    let a = backend.copy_text_via("a\n🦀", CopyTarget::Native).unwrap();
    let b = backend.copy_text_via("b", CopyTarget::Terminal).unwrap();
    assert!(backend.complete_copy(CopyResult {
        ticket: a,
        outcome: CopyOutcome::NativeWritten,
        guidance: None
    }));
    assert!(backend.complete_copy(CopyResult {
        ticket: b,
        outcome: CopyOutcome::TerminalRequested,
        guidance: Some("tmux guidance".into())
    }));
    assert!(backend.copy_text("third").is_err());
    assert_eq!(backend.poll_copy_results().len(), 2);
    assert!(backend.copy_text("third").is_ok());
    assert_eq!(
        backend.copy_requests[0],
        (CopyTarget::Native, "a\n🦀".into())
    );
}

#[test]
fn pending_dismissal_retains_association_and_completion_reappears() {
    let mut state = AppState::default();
    let origin = state
        .register_viewer_origin(ViewerSource::Message(7))
        .unwrap();
    let mut backend = FakeBackend::new();
    let ticket = backend.copy_text("body").unwrap();
    state.register_copy_request(origin.clone(), CopyTarget::Auto, Ok(ticket), 1);
    state.dismiss_copy_feedback(&origin);
    assert!(!state.copy_footer_visible(&origin));
    state.apply_copy_result(
        CopyResult {
            ticket,
            outcome: CopyOutcome::NativeWritten,
            guidance: None,
        },
        2,
    );
    assert!(state.copy_footer_visible(&origin));
    state.dismiss_copy_feedback(&origin);
    assert!(state.feedback_for(&origin).is_some());
    assert!(!state.copy_footer_visible(&origin));
}

fn result(ticket: orch_tui::backend::CopyTicket, outcome: CopyOutcome) -> CopyResult {
    CopyResult {
        ticket,
        outcome,
        guidance: None,
    }
}

#[test]
fn interleaved_viewer_and_editor_results_keep_their_own_destination() {
    use orch_tui::wizard::{DraftKey, WizardKind};
    let mut state = AppState::default();
    let viewer = state
        .register_viewer_origin(ViewerSource::Message(1))
        .unwrap();
    let editor = state
        .register_editor_origin(
            DraftKey {
                kind: WizardKind::EditTask,
                target_id: Some("task-a".into()),
                setting: None,
                template_key: None,
            },
            "description".into(),
        )
        .unwrap();
    let mut backend = FakeBackend::new();
    let v = backend
        .copy_text_via("viewer", CopyTarget::Terminal)
        .unwrap();
    let e = backend.copy_text_via("editor", CopyTarget::Native).unwrap();
    state.register_copy_request(viewer.clone(), CopyTarget::Terminal, Ok(v), 1);
    state.register_copy_request(editor.clone(), CopyTarget::Native, Ok(e), 2);
    state.apply_copy_result(result(e, CopyOutcome::Failed("Host failed".into())), 3);
    state.apply_copy_result(
        CopyResult {
            ticket: v,
            outcome: CopyOutcome::TerminalRequested,
            guidance: Some("TMUX terminal request only".into()),
        },
        4,
    );
    assert_eq!(
        state.feedback_for(&editor).unwrap().outcome,
        CopyOutcome::Failed("Host failed".into())
    );
    assert_eq!(
        state.feedback_for(&viewer).unwrap().guidance.as_deref(),
        Some("TMUX terminal request only")
    );
    assert_eq!(state.copy_feedback.pending_count(), 0);
}

#[test]
fn newer_immediate_failure_wins_over_an_older_accepted_completion() {
    let mut state = AppState::default();
    let origin = state
        .register_field_origin(ViewerSource::Message(1))
        .unwrap();
    let mut backend = FakeBackend::new();
    let ticket = backend.copy_text("old").unwrap();
    state.register_copy_request(origin.clone(), CopyTarget::Auto, Ok(ticket), 1);
    state.register_copy_request(origin.clone(), CopyTarget::Native, Err("Busy".into()), 2);
    state.apply_copy_result(result(ticket, CopyOutcome::NativeWritten), 3);
    let feedback = state.feedback_for(&origin).unwrap();
    assert_eq!(feedback.ticket, None);
    assert_eq!(feedback.outcome, CopyOutcome::Failed("Host: Busy".into()));
    assert_eq!(feedback.request.request_seq, 2);
}

#[test]
fn cleared_or_retired_requests_drain_without_resurrecting_feedback() {
    for retire in [false, true] {
        let mut state = AppState::default();
        let origin = state
            .register_viewer_origin(ViewerSource::Message(1))
            .unwrap();
        let ticket = orch_tui::backend::CopyTicket(1);
        state.register_copy_request(origin.clone(), CopyTarget::Auto, Ok(ticket), 1);
        if retire {
            state.retire_copy_origin(&origin);
        } else {
            state.clear_copy_feedback(&origin);
        }
        state.apply_copy_result(result(ticket, CopyOutcome::NativeWritten), 2);
        assert!(state.feedback_for(&origin).is_none());
        assert_eq!(state.copy_feedback.pending_count(), 0);
    }
}

#[test]
fn reopened_same_source_never_inherits_the_previous_generation_result() {
    let mut state = AppState::default();
    let old = state
        .register_viewer_origin(ViewerSource::Message(1))
        .unwrap();
    let ticket = orch_tui::backend::CopyTicket(1);
    state.register_copy_request(old.clone(), CopyTarget::Auto, Ok(ticket), 1);
    let new = state
        .register_viewer_origin(ViewerSource::Message(1))
        .unwrap();
    assert_ne!(old, new);
    state.apply_copy_result(result(ticket, CopyOutcome::NativeWritten), 2);
    assert!(state.feedback_for(&new).is_none());
    assert!(!state.copy_footer_visible(&old));
}

#[test]
fn contract_defects_and_unknown_completions_are_bounded_and_observable() {
    let mut state = AppState::default();
    let origin = state
        .register_viewer_origin(ViewerSource::Message(1))
        .unwrap();
    for id in [1, 2, 3, 1] {
        state.register_copy_request(
            origin.clone(),
            CopyTarget::Auto,
            Ok(orch_tui::backend::CopyTicket(id)),
            1,
        );
    }
    assert_eq!(state.copy_feedback.pending_count(), 2);
    assert_eq!(state.copy_feedback.contract_errors, 2);
    let unknown = orch_tui::backend::CopyTicket(99);
    state.apply_copy_result(result(unknown, CopyOutcome::NativeWritten), 2);
    state.apply_copy_result(result(unknown, CopyOutcome::NativeWritten), 3);
    assert_eq!(state.copy_feedback.unknown_results, 2);
    assert_eq!(state.copy_feedback.pending_count(), 2);
}

#[test]
fn feedback_bounds_utf8_and_removes_terminal_controls() {
    let mut state = AppState::default();
    let origin = state
        .register_viewer_origin(ViewerSource::Message(1))
        .unwrap();
    let ticket = orch_tui::backend::CopyTicket(1);
    state.register_copy_request(origin.clone(), CopyTarget::Auto, Ok(ticket), 1);
    state.apply_copy_result(
        CopyResult {
            ticket,
            outcome: CopyOutcome::Failed(format!("\x1b\n{}", "🦀".repeat(200))),
            guidance: Some("🦀".repeat(300)),
        },
        2,
    );
    let feedback = state.feedback_for(&origin).unwrap();
    let CopyOutcome::Failed(error) = &feedback.outcome else {
        panic!("expected failure")
    };
    assert!(error.len() <= 256 && error.ends_with('…'));
    assert!(!error.chars().any(char::is_control));
    let guidance = feedback.guidance.as_ref().unwrap();
    assert!(guidance.len() <= 512 && guidance.ends_with('…'));
}

#[test]
fn fake_failure_is_immediate_and_unknown_or_duplicate_completion_is_rejected() {
    let mut backend = FakeBackend::new();
    backend.copy_fail_next = Some("denied".into());
    assert_eq!(
        backend.copy_text_via("exact", CopyTarget::Native),
        Err("denied".into())
    );
    let ticket = backend.copy_text("exact").unwrap();
    assert!(backend.complete_copy(result(ticket, CopyOutcome::NativeWritten)));
    assert!(!backend.complete_copy(result(ticket, CopyOutcome::NativeWritten)));
    backend.poll_copy_results();
    assert!(!backend.complete_copy(result(ticket, CopyOutcome::NativeWritten)));
}
