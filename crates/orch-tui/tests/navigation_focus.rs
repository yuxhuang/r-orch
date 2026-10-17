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
