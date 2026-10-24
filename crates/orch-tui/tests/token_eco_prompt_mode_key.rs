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

/// Whether the header shows exactly `mode` (`normal`, `eco…`, ...) as the
/// prompt mode: in the brand bar's right group (`… · {mode} · {chip}`), or
/// leading row 2 in the narrow layout.
fn shows_mode(h: &mut Harness, mode: &str) -> bool {
    let text = h.text();
    let mut rows = text.lines().skip(1);
    let brand = rows.next().unwrap_or_default();
    let second = rows.next().unwrap_or_default().trim_start();
    let inline = brand.contains(&format!(" {mode} · "));
    let narrow = second.trim_end() == mode || second.starts_with(&format!("{mode} · "));
    inline || narrow
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
    assert!(shows_mode(&mut h, "normal…"), "{}", header(&mut h));
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
    assert!(shows_mode(&mut h, "eco"), "{}", header(&mut h));
    let text = h.text();
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
    assert!(
        shows_mode(&mut h, "normal") && !shows_mode(&mut h, "eco"),
        "{}",
        header(&mut h)
    );
    let text = h.text();
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

// ── review-b4 F2 / gate C7: the settings wizard and the refresh race ────────

#[test]
fn a_refresh_while_a_write_is_pending_keeps_the_stored_header_value_until_the_outcome() {
    let mut h = harness();
    h.press(KeyCode::F(3));
    // The file already says eco (the write landed, or another process wrote)
    // but the outcome has not been polled: a refresh must not move the
    // header ahead of, or behind, the outcome it is waiting for.
    h.app.backend.prompt_mode = PromptMode::Eco;
    h.app.refresh_all(false);
    assert_eq!(h.app.state.prompt_mode, PromptMode::Normal);
    assert!(
        shows_mode(&mut h, "normal…") && !shows_mode(&mut h, "eco"),
        "{}",
        header(&mut h)
    );
    // The outcome decides.
    h.app.backend.prompt_mode_request = Some(Some(PromptMode::Eco));
    assert!(h.app.backend.complete_prompt_mode());
    h.app.tick();
    assert_eq!(h.app.state.prompt_mode, PromptMode::Eco);
    assert!(
        shows_mode(&mut h, "eco") && !shows_mode(&mut h, "eco…"),
        "{}",
        header(&mut h)
    );
}

#[test]
fn the_prompt_mode_settings_wizard_submitted_while_a_write_is_pending_is_refused_and_requests_nothing()
 {
    let mut h = harness();
    h.press(KeyCode::F(3));
    assert_eq!(mode_requests(&h), 1);
    h.run_command("/config prompt-mode");
    assert_eq!(h.app.state.input_mode, InputMode::Wizard);
    // Pick eco and submit the one-step wizard.
    h.press(KeyCode::Down);
    h.press(KeyCode::Enter);
    assert_eq!(
        mode_requests(&h),
        1,
        "the wizard must not issue a second write while one is pending"
    );
    assert!(
        h.messages()
            .iter()
            .any(|m| m.contains("Prompt mode change already in progress")),
        "the refusal must be visible: {:?}",
        h.messages()
    );
    assert_eq!(h.app.backend.prompt_mode, PromptMode::Normal);
    // The first write still lands on its own.
    assert!(h.app.backend.complete_prompt_mode());
    h.app.tick();
    assert_eq!(h.app.backend.prompt_mode, PromptMode::Eco);
}

// ── QA-4: the prompt mode stays readable on narrow frames ───────────────────

/// Rows 1 and 2 of the header (brand bar, then the peer-status line) at the
/// given width, in QA's observing session on the given project name.
fn header_rows_for(project: &str, width: u16, height: u16) -> (String, String) {
    let mut h = Harness::sized(
        FakeBackend::new().with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]),
        width,
        height,
    );
    h.app.state.project_name = project.into();
    h.app.state.observer_mode = true;
    h.app.state.prompt_mode = PromptMode::Normal;
    let text = h.text();
    let mut lines = text.lines().skip(1);
    (
        lines.next().unwrap_or_default().to_owned(),
        lines.next().unwrap_or_default().to_owned(),
    )
}

fn header_rows(width: u16, height: u16) -> (String, String) {
    header_rows_for("main", width, height)
}

#[test]
fn at_120_columns_the_prompt_mode_stays_on_the_brand_bar() {
    let (brand, second) = header_rows(120, 36);
    assert!(
        brand.ends_with("0 running │ 0 todo · normal · ● OBSERVING"),
        "{brand}"
    );
    assert!(!brand.contains("Prompt"), "no prefix any more: {brand}");
    assert!(!second.contains("normal"), "{second}");
}

#[test]
fn at_80_columns_the_prompt_mode_stays_on_the_brand_bar_when_it_fits() {
    // The counts give way first, so a short project name keeps the mode and
    // the chip on row 1.
    let (brand, second) = header_rows(80, 24);
    assert!(brand.ends_with(" normal · ● OBSERVING"), "{brand}");
    assert!(!second.contains("normal"), "{second}");
}

#[test]
fn at_80_columns_the_prompt_mode_leads_the_second_header_line_in_full() {
    // A longer name leaves room for the chip only: the mode moves to row 2.
    let (brand, second) = header_rows_for("main-project", 80, 24);
    assert!(
        second.trim_start().starts_with("normal · "),
        "row 2: {second}"
    );
    assert!(!brand.contains("normal"), "no clipped fragment: {brand}");
    assert!(brand.ends_with(" ● OBSERVING"), "{brand}");
}

#[test]
fn at_40_columns_the_header_renders_and_the_prompt_mode_is_still_readable() {
    let (_brand, second) = header_rows(40, 24);
    assert!(second.trim_start().starts_with("normal · "), "{second}");
}

#[test]
fn the_narrow_layout_shows_the_pending_ellipsis_and_eco() {
    let mut h = Harness::sized(
        FakeBackend::new().with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]),
        80,
        24,
    );
    h.app.state.project_name = "main-project".into();
    h.app.state.observer_mode = true;
    h.press(KeyCode::F(3));
    let second = |h: &mut Harness| h.text().lines().nth(2).unwrap_or_default().to_owned();
    assert!(second(&mut h).starts_with("normal… · "), "{}", h.text());
    h.app.backend.complete_prompt_mode();
    h.app.tick();
    assert!(second(&mut h).starts_with("eco · "), "{}", h.text());
}
