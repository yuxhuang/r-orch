//! Gate C2 / QA-2 (UI side): a successful wizard add or edit shows the
//! authored agent's effort diagnostics as yellow system lines, the way the
//! launch warnings are shown, and a failed write shows none.
//!
//! The diagnostics themselves are computed by the engine backend (tested in
//! `orch-cli`'s `tui::backend::authoring_tests`); here the fake hands out
//! fixed lines through `take_agent_diagnostics`.

mod common;

use common::Harness;
use orch_domain::AgentStatus;
use orch_tui::backend::{FakeBackend, make_agent};
use orch_tui::theme::Token;
use orch_tui::wizard::engine::{StepKind, Values};
use orch_tui::wizard::{Wizard, WizardKind, WizardStep};

const CODEX_WARNING: &str = "Wizard 🧪 (agt_1): capability-unknown: codex effort with no model: \
forwarding effort can bypass a managed new-thread model default";

fn yellow_lines(h: &mut Harness) -> Vec<String> {
    let now = h.now();
    h.app.state.flush_messages(now);
    h.app
        .state
        .messages
        .iter()
        .filter(|m| m.color == Token::Yellow && m.text.starts_with('⚠'))
        .map(|m| m.text.clone())
        .collect()
}

fn wizard(kind: WizardKind, target: Option<&str>) -> Wizard {
    let mut wizard = Wizard::new(
        kind,
        "Test",
        vec![WizardStep::new("name", "Name", StepKind::Text)],
    );
    wizard.target_id = target.map(str::to_owned);
    wizard
}

fn values(pairs: &[(&str, &str)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn a_wizard_add_shows_each_authored_diagnostic_as_a_yellow_line_after_the_success_line() {
    let mut h = Harness::new();
    h.app.backend.agent_diagnostics = vec![CODEX_WARNING.into(), "second line".into()];
    h.app.open_wizard(wizard(WizardKind::Agent, None));
    h.app.submit_wizard(values(&[
        ("name", "Wizard 🧪"),
        ("adapter", "codex"),
        ("effort", "high"),
    ]));
    let messages = h.messages();
    let created = messages
        .iter()
        .position(|m| m.starts_with("✓ Created agent \"Wizard 🧪\""))
        .unwrap_or_else(|| panic!("{messages:?}"));
    let warning = messages
        .iter()
        .position(|m| m == &format!("⚠ {CODEX_WARNING}"))
        .unwrap_or_else(|| panic!("{messages:?}"));
    assert!(created < warning, "{messages:?}");
    assert_eq!(
        yellow_lines(&mut h),
        vec![format!("⚠ {CODEX_WARNING}"), "⚠ second line".to_owned()]
    );
    assert!(h.app.backend.agent_diagnostics.is_empty(), "taken once");
}

#[test]
fn a_wizard_edit_shows_the_authored_diagnostics_too() {
    let mut h = Harness::with(FakeBackend::new().with_agents(vec![make_agent(
        "agt_1",
        "Wizard 🧪",
        AgentStatus::Idle,
    )]));
    h.app.backend.agent_diagnostics = vec![CODEX_WARNING.into()];
    h.app
        .open_wizard(wizard(WizardKind::EditAgent, Some("agt_1")));
    h.app
        .submit_wizard(values(&[("name", "Wizard 🧪"), ("effort", "high")]));
    h.assert_message("✓ Updated agent \"Wizard 🧪\"");
    assert_eq!(yellow_lines(&mut h), vec![format!("⚠ {CODEX_WARNING}")]);
}

#[test]
fn a_failed_write_shows_no_authoring_diagnostics() {
    let mut h = Harness::new();
    h.app.backend.agent_diagnostics = vec![CODEX_WARNING.into()];
    h.app.backend.fail_next = Some("eco prompt mode is incompatible with custom templates".into());
    h.app.open_wizard(wizard(WizardKind::Agent, None));
    h.app
        .submit_wizard(values(&[("name", "Wizard 🧪"), ("adapter", "codex")]));
    assert!(yellow_lines(&mut h).is_empty());
}

#[test]
fn the_agent_add_command_shows_the_authored_diagnostics() {
    let mut h = Harness::new();
    h.app.backend.agent_diagnostics = vec![CODEX_WARNING.into()];
    h.run_command("/agent add helper codex");
    h.assert_message("✓ Created agent \"helper\"");
    assert_eq!(yellow_lines(&mut h), vec![format!("⚠ {CODEX_WARNING}")]);
}
