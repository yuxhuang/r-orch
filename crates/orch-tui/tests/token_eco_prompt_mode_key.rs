//! D1-1 / D1-2 (UI side) — `F3` switches the **project** prompt mode.
//!
//! Driven through `App` + `FakeBackend`; the fake's "config file" is its
//! `prompt_mode` field, and a write stays *pending* until the test completes
//! it, so every intermediate state is observable without a sleep.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, PromptMode};
use orch_tui::backend::{Call, FakeBackend, make_agent};
use orch_tui::state::InputMode;
use orch_tui::wizard::engine::StepKind;
use orch_tui::wizard::{Wizard, WizardKind, WizardStep};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

fn mode_requests(h: &Harness) -> usize {
    h.app
        .backend
        .calls
        .iter()
        .filter(|c| matches!(c, Call::RequestPromptMode(_)))
        .count()
}

fn harness() -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_agents(vec![make_agent(
        "agt_1",
        "alpha",
        AgentStatus::Idle,
    )]));
    h.app.state.prompt_mode = PromptMode::Normal;
    h
}

fn header(h: &mut Harness) -> String {
    h.text().lines().take(4).collect::<Vec<_>>().join("\n")
}

fn modal_wizard() -> Wizard {
    Wizard::new(
        WizardKind::Task,
        "Test",
        vec![WizardStep::new("field", "Field", StepKind::Text)],
    )
}

#[test]
fn f3_in_ordinary_navigation_requests_one_toggle_and_the_header_keeps_the_stored_value_until_it_lands()
 {
    let mut h = harness();
    h.press(KeyCode::F(3));
    assert_eq!(
        h.app.backend.calls.last(),
        Some(&Call::RequestPromptMode(None))
    );
    assert_eq!(mode_requests(&h), 1);
    // Pending: the header still shows the stored value, with an ellipsis.
    assert!(
        header(&mut h).contains("Prompt: normal…"),
        "{}",
        header(&mut h)
    );
    assert_eq!(
        h.app.backend.prompt_mode,
        PromptMode::Normal,
        "nothing written yet"
    );
}

#[test]
fn a_completed_f3_shows_the_new_mode_and_the_truthful_next_dispatch_toast() {
    let mut h = harness();
    h.press(KeyCode::F(3));
    assert!(h.app.backend.complete_prompt_mode());
    h.app.tick();
    assert_eq!(h.app.backend.prompt_mode, PromptMode::Eco);
    let text = h.text();
    assert!(text.contains("Prompt: eco"), "{text}");
    assert!(
        text.contains("Project prompt mode: eco")
            && text.contains("next dispatch; agent overrides retained"),
        "{text}"
    );
}

#[test]
fn f3_twice_with_a_write_pending_refuses_the_second_press_visibly_and_writes_once() {
    let mut h = harness();
    h.press(KeyCode::F(3));
    h.press(KeyCode::F(3));
    assert_eq!(
        mode_requests(&h),
        1,
        "key repeat must not queue a second write"
    );
    h.app.backend.complete_prompt_mode();
    h.app.tick();
    assert_eq!(
        h.app.backend.prompt_mode,
        PromptMode::Eco,
        "one toggle, not two"
    );
}

#[test]
fn f3_toggles_back_to_normal_on_a_second_separate_press() {
    let mut h = harness();
    for expected in [PromptMode::Eco, PromptMode::Normal] {
        h.press(KeyCode::F(3));
        h.app.backend.complete_prompt_mode();
        h.app.tick();
        assert_eq!(h.app.backend.prompt_mode, expected);
    }
    assert_eq!(mode_requests(&h), 2);
}

#[test]
fn a_failed_write_leaves_the_header_and_stored_mode_unchanged_and_shows_the_reason() {
    let mut h = harness();
    h.app.backend.prompt_mode_fail =
        Some("eco prompt mode is incompatible with custom templates".into());
    h.press(KeyCode::F(3));
    h.app.backend.complete_prompt_mode();
    h.app.tick();
    assert_eq!(
        h.app.backend.prompt_mode,
        PromptMode::Normal,
        "stored mode must not change"
    );
    assert_eq!(h.app.state.prompt_mode, PromptMode::Normal);
    let text = h.text();
    assert!(
        text.contains("Prompt: normal") && !text.contains("Prompt: eco"),
        "{text}"
    );
    assert!(
        text.contains("incompatible with custom templates"),
        "the reason must be visible: {text}"
    );
    // The pending state is released: a retry is possible.
    h.press(KeyCode::F(3));
    assert_eq!(mode_requests(&h), 2);
}

#[test]
fn a_request_the_backend_refuses_up_front_changes_nothing() {
    let mut h = harness();
    h.app.backend.fail_next = Some("config is read-only".into());
    h.press(KeyCode::F(3));
    assert_eq!(h.app.backend.prompt_mode, PromptMode::Normal);
    assert!(
        !h.app.state.prompt_mode_pending,
        "a refused request must not leave the UI pending"
    );
    assert!(h.text().contains("config is read-only"));
}

#[test]
fn f3_is_swallowed_by_every_modal_surface_and_writes_nothing() {
    type Open = fn(&mut Harness);
    let surfaces: [(&str, Open); 6] = [
        ("help overlay", |h| h.app.state.show_help = true),
        ("agent picker", |h| h.app.state.show_agent_picker = true),
        ("type picker", |h| h.app.state.show_type_picker = true),
        ("command editor", |h| {
            h.app.state.input_mode = InputMode::Command
        }),
        ("keyword editor", |h| {
            h.app.state.input_mode = InputMode::Keyword
        }),
        ("wizard", |h| h.app.open_wizard(modal_wizard())),
    ];
    for (name, open) in surfaces {
        let mut h = harness();
        open(&mut h);
        // One press: help swallows F3 by closing, so a second press would be
        // an ordinary-navigation F3 and legitimately write.
        h.press(KeyCode::F(3));
        assert_eq!(mode_requests(&h), 0, "{name}: F3 must not request a write");
        assert_eq!(h.app.backend.prompt_mode, PromptMode::Normal, "{name}");
        assert!(!h.app.state.prompt_mode_pending, "{name}");
    }
}

#[test]
fn f3_with_a_modifier_or_as_a_key_release_is_ignored() {
    let mut h = harness();
    h.press_mod(KeyCode::F(3), KeyModifiers::SHIFT);
    h.press_mod(KeyCode::F(3), KeyModifiers::CONTROL);
    h.release(KeyCode::F(3));
    assert_eq!(mode_requests(&h), 0);
}

#[test]
fn f1_and_f2_keep_their_bindings_and_never_touch_the_prompt_mode() {
    let mut h = harness();
    h.press(KeyCode::F(1));
    assert!(h.app.state.show_help, "F1 still opens help");
    h.press(KeyCode::F(1));
    h.press(KeyCode::F(2));
    assert_eq!(mode_requests(&h), 0);
    assert_eq!(h.app.backend.prompt_mode, PromptMode::Normal);
}

#[test]
fn help_lists_the_f3_prompt_mode_toggle() {
    let mut h = harness();
    h.press(KeyCode::F(1));
    let text = h.text();
    assert!(text.contains("F3") && text.contains("Prompt"), "{text}");
}
