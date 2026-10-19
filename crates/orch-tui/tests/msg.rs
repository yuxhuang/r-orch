//! Counted messaging cases: bracketed paste, the `/msg` verb, its grammar at
//! the App level, autocomplete, the `m` binding and the recipient chip
//! (design-v2 §3), numbered `msg001..`.
//!
//! The safety properties live here rather than in `msg.rs`'s unit tests: that
//! module cannot reach a selection or a backend, and these cases are what
//! prove the App does not hand it one.

mod common;

use common::Harness;
use orch_domain::AgentStatus;
use orch_tui::backend::{Call, FakeBackend, make_agent};
use orch_tui::state::{Focus, InputMode, ViewId};
use ratatui::crossterm::event::KeyCode;

fn fleet() -> FakeBackend {
    FakeBackend::new().with_agents(vec![
        make_agent("agt_rev", "Code Reviewer", AgentStatus::Idle),
        make_agent("agt_code", "Code", AgentStatus::Idle),
        make_agent("agt_sq", "say \"hi\"", AgentStatus::Disabled),
        make_agent("agt_dep", "🚀 deploy", AgentStatus::Idle),
    ])
}

fn fleet_harness() -> Harness {
    Harness::with(fleet())
}

fn sent(call: &Call) -> bool {
    matches!(call, Call::SendMessage { .. })
}

// ── Bracketed paste (§3.9) ───────────────────────────────────────────────

#[test]
fn msg001_a_paste_in_command_mode_keeps_its_newlines_and_does_not_submit() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg ").paste("a\nb\nc");

    // One value, two newlines, nothing executed: the pasted `\n` must not
    // become the `Enter` that sends a half a message.
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg a\nb\nc",
        "the newlines belong in the body"
    );
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert!(harness.calls().is_empty());
}

#[test]
fn msg002_pasted_newlines_render_as_a_glyph_rather_than_breaking_the_line() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg Code ").paste("one\ntwo");

    let frame = harness.text();
    assert!(
        frame.contains("one⏎two"),
        "expected the return glyph in the bar:\n{frame}"
    );
    // The value itself is untouched — the glyph is display only.
    assert!(
        harness
            .app
            .state
            .command_bar
            .input
            .value()
            .ends_with("one\ntwo")
    );
}

#[test]
fn msg003_crlf_and_a_lone_cr_normalise_to_a_newline() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg Code ").paste("a\r\nb\rc");
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg Code a\nb\nc"
    );
}

#[test]
fn msg004_a_paste_in_normal_mode_is_dropped_entirely() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    let before = harness.app.state.agents.len();

    // Every destructive normal-mode letter, in one paste: `q` quits, `d`
    // deletes the selected agent, `t`/`g`/`l` switch view. None of them may
    // fire, which is the half of the paste hazard that mattered.
    harness.paste("quit\ndelete\ngo\nthen\nlogs");

    assert_eq!(
        harness.app.state.agents.len(),
        before,
        "an agent was deleted"
    );
    assert!(
        !harness.app.state.should_exit,
        "the paste quit the dashboard"
    );
    assert_eq!(harness.app.state.view, ViewId::Agents, "the view switched");
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert!(harness.app.state.pending_deletions.is_empty());
    assert!(harness.calls().is_empty());
}

#[test]
fn msg005_a_paste_into_a_wizard_text_step_lands_in_that_field() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Tasks;
    harness.key('n');
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);

    harness.paste("Fix the flaky test");

    let wizard = harness.app.state.wizard.as_ref().expect("a wizard is open");
    assert_eq!(wizard.text_value(), "Fix the flaky test");
}

#[test]
fn msg006_a_paste_while_a_picker_is_open_is_dropped() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Logs;
    harness.key('f');
    assert!(harness.app.state.show_type_picker);

    harness.paste("quit\n");

    assert!(harness.app.state.show_type_picker, "the picker closed");
    assert!(!harness.app.state.should_exit);
}

#[test]
fn msg007_an_empty_paste_changes_nothing() {
    let mut harness = fleet_harness();
    harness.key('/');
    harness.paste("");
    assert_eq!(harness.app.state.command_bar.input.value(), "/");
}

// ── Sending (§3.2, §3.4, §3.8) ───────────────────────────────────────────

#[test]
fn msg008_a_quoted_name_sends_to_that_agent_by_id() {
    let mut harness = fleet_harness();
    harness.run_command("/msg \"Code Reviewer\" please look at #12");

    harness.assert_called(|call| {
        *call
            == Call::SendMessage {
                target: orch_tui::backend::MessageTarget::Agent("agt_rev".into()),
                body: "please look at #12".into(),
            }
    });
    harness.assert_message("✓ Queued for Code Reviewer");
    harness.assert_message("delivered on its next run");
}

#[test]
fn msg009_the_body_is_verbatim_and_is_never_reparsed_as_a_command() {
    let mut harness = fleet_harness();
    harness.run_command("/msg Code /quit now   and  keep   spaces");

    harness.assert_called(|call| {
        matches!(call, Call::SendMessage { body, .. } if body == "/quit now   and  keep   spaces")
    });
    assert!(!harness.app.state.should_exit, "the body was executed");
}

#[test]
fn msg010_an_unquoted_name_shadowed_by_a_longer_one_is_refused() {
    let mut harness = fleet_harness();
    harness.run_command("/msg Code Reviewer please look at #12");

    assert!(harness.calls().is_empty(), "nothing may be sent");
    harness.assert_message("\"Code\" is an agent, and so is \"Code Reviewer\"");
    harness.assert_message("/msg \"Code\" Reviewer please look at #12");
    harness.assert_message("/msg \"Code Reviewer\" please look at #12");
}

#[test]
fn msg011_every_refusal_sends_nothing_and_keeps_the_bar_open() {
    // Each of these is typed and submitted, so bar retention is exercised on
    // the real Enter path. The two no-body cases carry a trailing space,
    // which is what `m` prefills and what closes the suggestion panel — with
    // the panel open, Enter fills a row instead of submitting (msg026).
    let cases = [
        ("/msg Code ", "Missing message for \"Code\""),
        (
            "/msg \"Code Reviewer\" ",
            "Missing message for \"Code Reviewer\"",
        ),
        ("/msg \"Code Rev hi", "Unclosed quote in recipient"),
        ("/msg \"Code\"hi", "Expected a space after the quoted name"),
        ("/msg \"\" hi", "Recipient is empty"),
        ("/msg zzz hi", "No agent matches \"zzz\""),
        // Two names start with `Cod`, so the refusal lists both with ids
        // rather than guessing one.
        (
            "/msg Cod hi",
            "No agent named \"Cod\" — 2 agents start with it: agt_rev Code Reviewer, agt_code Code",
        ),
    ];
    for (input, expected) in cases {
        let mut harness = fleet_harness();
        harness.key('/').type_str(&input[1..]).press(KeyCode::Enter);

        assert!(
            harness.calls().is_empty(),
            "{input:?} reached the backend: {:#?}",
            harness.calls()
        );
        harness.assert_message(expected);
        assert_eq!(
            harness.app.state.input_mode,
            InputMode::Command,
            "{input:?} closed the bar"
        );
        // Verbatim, trailing space and all: `/msg Code ` without it reopens
        // the suggestion panel, which covers the refusal (QA repro B).
        assert_eq!(
            harness.app.state.command_bar.input.value(),
            input,
            "{input:?} lost its text"
        );
        // …and `↑` still recovers it, because history is pushed first.
        assert_eq!(
            harness.app.state.command_bar.history.entries(),
            [input.trim_end()]
        );
    }
}

#[test]
fn msg011b_the_bare_verb_reports_its_usage() {
    // Not reachable by typing `/msg` + Enter, which fills the verb row
    // (msg029); this is the history-replay path.
    let mut harness = fleet_harness();
    harness.run_command("/msg");
    harness.assert_message("Usage: /msg <agent|all> <message>");
    assert!(harness.calls().is_empty());
    assert!(harness.app.retain_command_bar, "the bar must be kept open");
}

#[test]
fn msg012_an_ambiguous_name_lists_the_ids_and_sends_nothing() {
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_a1", "Alice", AgentStatus::Idle),
        make_agent("agt_a2", "alice", AgentStatus::Idle),
    ]);
    let mut harness = Harness::with(backend);
    harness.run_command("/msg ALICE hi");

    assert!(harness.calls().is_empty());
    harness.assert_message(
        "Ambiguous recipient \"ALICE\" — 2 agents match: agt_a1 Alice, agt_a2 alice (use the id)",
    );
}

#[test]
fn msg013_a_disabled_recipient_still_receives_with_an_advisory() {
    let mut harness = fleet_harness();
    harness.run_command("/msg \"say \\\"hi\\\"\" wake up");

    harness.assert_called(|call| {
        matches!(call, Call::SendMessage { target, .. }
            if *target == orch_tui::backend::MessageTarget::Agent("agt_sq".into()))
    });
    harness.assert_message("say \"hi\" is disabled — the message waits in its inbox");
}

#[test]
fn msg014_a_body_over_the_cap_warns_but_is_passed_whole_to_the_backend() {
    let mut harness = fleet_harness();
    let body = "b".repeat(4_001);
    harness.run_command(&format!("/msg Code {body}"));

    // The cut belongs to `MessageService::truncate_utf16`; the TUI only makes
    // it visible, and must not pre-truncate.
    harness.assert_called(
        |call| matches!(call, Call::SendMessage { body: sent, .. } if sent.len() == 4_001),
    );
    harness.assert_message("Body truncated to 4000 characters");
}

#[test]
fn msg015_a_backend_failure_is_reported_as_a_failure() {
    let mut harness = fleet_harness();
    harness.app.backend.fail_next = Some("Recipient agent not found: agt_rev".into());
    harness.run_command("/msg Code hi");
    harness.assert_message("Failed: Recipient agent not found: agt_rev");
}

#[test]
fn msg016_the_dispatch_hint_appears_only_when_nothing_is_dispatching() {
    // Neither watch nor observer: the message will sit in the inbox.
    let mut harness = fleet_harness();
    harness.app.state.watch_active = false;
    harness.app.state.observer_mode = false;
    harness.run_command("/msg Code hi");
    harness.assert_message("nothing is dispatching right now; /watch to start");

    // Observer mode *is* dispatching — in another process.
    let mut harness = fleet_harness();
    harness.app.state.watch_active = false;
    harness.app.state.observer_mode = true;
    harness.run_command("/msg Code hi");
    harness.refute_message("nothing is dispatching");

    let mut harness = fleet_harness();
    harness.app.state.watch_active = true;
    harness.run_command("/msg Code hi");
    harness.refute_message("nothing is dispatching");
}

#[test]
fn msg017_resolution_never_falls_back_to_the_selected_agent() {
    // `/agent disable nosuch` substitutes the selection; `/msg` must not, or
    // a body reaches an agent the user never named (dissent S1).
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    assert!(harness.app.state.selected_agent().is_some());

    harness.run_command("/msg nosuch hello");

    assert!(harness.calls().is_empty());
    harness.assert_message("No agent matches \"nosuch\"");
}

// ── Broadcast (§3.4.4, §3.4.6) ───────────────────────────────────────────

#[test]
fn msg018_a_broadcast_needs_a_confirming_second_enter() {
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);

    // First Enter arms: nothing sent, the bar keeps its text.
    assert!(harness.calls().is_empty(), "the first Enter sent something");
    harness.assert_message("Broadcast to 3 agents — press Enter again to send");
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg all ship it"
    );

    // Second Enter on byte-identical text sends.
    harness.press(KeyCode::Enter);
    harness.assert_called(|call| {
        *call
            == Call::SendMessage {
                target: orch_tui::backend::MessageTarget::Broadcast,
                body: "ship it".into(),
            }
    });
    harness.assert_message("✓ Queued for");
    assert_eq!(
        harness.app.state.input_mode,
        InputMode::None,
        "the bar stayed open"
    );
}

#[test]
fn msg019_editing_between_the_two_enters_rearms() {
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    harness.type_str(" now").press(KeyCode::Enter);

    // The arm was keyed on the old text, so this Enter arms again.
    assert!(harness.calls().is_empty());
    harness.assert_message("Broadcast to 3 agents — press Enter again to send");
    harness.press(KeyCode::Enter);
    harness.assert_called(
        |call| matches!(call, Call::SendMessage { body, .. } if body == "ship it now"),
    );
}

#[test]
fn msg020_esc_disarms_a_broadcast() {
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    harness.press(KeyCode::Esc);
    assert_eq!(harness.app.state.input_mode, InputMode::None);

    // Retyping the same command arms again rather than sending.
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    assert!(harness.calls().is_empty(), "a stale arm fired");
    harness.assert_message("press Enter again to send");
}

#[test]
fn msg021_only_the_exact_lowercase_bare_all_broadcasts() {
    for input in [
        "/msg ALL hi",
        "/msg All hi",
        "/msg @all hi",
        "/msg \"all\" hi",
    ] {
        let mut harness = fleet_harness();
        harness.run_command(input);
        harness.refute_called(sent);
        harness.refute_message("press Enter again to send");
    }
}

#[test]
fn msg022_a_broadcast_with_no_enabled_agents_is_refused_before_arming() {
    let backend =
        FakeBackend::new().with_agents(vec![make_agent("agt_off", "off", AgentStatus::Disabled)]);
    let mut harness = Harness::with(backend);
    harness.run_command("/msg all hi");

    harness.refute_called(sent);
    harness.assert_message("No enabled agents to message");
    harness.refute_message("press Enter again to send");
}

// ── Autocomplete (§3.5) ──────────────────────────────────────────────────

#[test]
fn msg023_the_panel_lists_agents_with_the_broadcast_row_last() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg ");

    let frame = harness.text();
    // Panel rows only: the command-bar line also reads `/ /msg …`, and it
    // carries no `·` description.
    let rows: Vec<&str> = frame
        .lines()
        .filter(|line| line.contains("/msg ") && line.contains('·'))
        .collect();
    let all_row = rows
        .iter()
        .position(|line| line.contains("/msg all"))
        .expect("the broadcast row is listed");
    assert_eq!(
        all_row,
        rows.len() - 1,
        "the broadcast row must sort last:\n{frame}"
    );
    assert!(frame.contains("broadcast · 3 enabled agent(s)"), "{frame}");
    // The row shows the *unescaped* name, which is what a human reads.
    assert!(frame.contains("/msg Code Reviewer"), "{frame}");
    assert!(frame.contains("/msg say \"hi\""), "{frame}");
}

#[test]
fn msg024_tab_fills_the_quoted_form_of_the_selected_row() {
    let mut harness = fleet_harness();
    // `/msg a` matches every id (`agt_…`), so index 0 is an agent row, never
    // the broadcast row.
    harness.key('/').type_str("msg a").press(KeyCode::Tab);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"Code Reviewer\" "
    );
    assert!(harness.calls().is_empty(), "Tab must never send");
}

#[test]
fn msg025_tab_on_an_awkward_name_inserts_text_that_parses_back() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg say").press(KeyCode::Tab);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"say \\\"hi\\\"\" "
    );

    // …and sending it reaches that agent, which is the round trip that
    // matters: the inserted text is computed from the record, not scraped
    // from the displayed row.
    harness.type_str("wake up").press(KeyCode::Enter);
    harness.assert_called(|call| {
        matches!(call, Call::SendMessage { target, .. }
            if *target == orch_tui::backend::MessageTarget::Agent("agt_sq".into()))
    });
}

#[test]
fn msg026_enter_on_an_agent_row_fills_instead_of_executing() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg say").press(KeyCode::Enter);

    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"say \\\"hi\\\"\" ",
        "Enter on a row with an insert must fill, not execute"
    );
    assert!(harness.calls().is_empty());
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
}

#[test]
fn msg027_the_panel_closes_once_the_body_starts() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg Code ");
    let frame = harness.text();
    assert!(
        !frame.contains("broadcast · "),
        "the panel must be empty once the body starts:\n{frame}"
    );
    // So Enter cannot be hijacked mid-body.
    harness.type_str("hello").press(KeyCode::Enter);
    harness.assert_called(sent);
}

#[test]
fn msg028_a_partial_quoted_name_still_filters() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg \"Code R");
    let frame = harness.text();
    assert!(frame.contains("/msg Code Reviewer"), "{frame}");
    assert!(
        !frame.contains("/msg Code  "),
        "`Code` must not match:\n{frame}"
    );
}

#[test]
fn msg029_the_verb_row_fills_with_a_trailing_space() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("ms").press(KeyCode::Tab);
    assert_eq!(harness.app.state.command_bar.input.value(), "/msg ");
}

// ── The `m` binding (§3.7) ───────────────────────────────────────────────

#[test]
fn msg030_m_on_the_agents_tab_prefills_the_quoted_name() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.key('m');

    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"Code Reviewer\" "
    );
    assert_eq!(harness.app.state.command_bar.suggestion_index, 0);
}

#[test]
fn msg031_m_does_nothing_where_there_is_nothing_to_message() {
    // Other views.
    for view in [ViewId::Tasks, ViewId::Goals, ViewId::Logs] {
        let mut harness = fleet_harness();
        harness.app.state.view = view;
        harness.key('m');
        assert_eq!(harness.app.state.input_mode, InputMode::None, "{view:?}");
    }
    // No agents at all.
    let mut harness = Harness::new();
    harness.app.state.view = ViewId::Agents;
    harness.key('m');
    assert_eq!(harness.app.state.input_mode, InputMode::None);

    // A backend without messaging.
    let mut backend = fleet();
    backend.can_send_message = false;
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.key('m');
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    // …and the verb is inert too.
    harness.run_command("/msg Code hi");
    harness.refute_called(sent);
    harness.assert_message("Messaging not available");
}

#[test]
fn msg032_uppercase_m_still_maximizes_the_detail_panel() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.key('M');

    assert!(harness.app.state.detail_maximized);
    assert_eq!(harness.app.state.input_mode, InputMode::None);
}

#[test]
fn msg033_m_waits_for_main_list_focus_with_the_detail_panel_open_like_its_neighbours() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.press(KeyCode::Enter);
    assert!(harness.app.state.detail_open());
    assert_ne!(
        harness.app.state.focus,
        Focus::MainList,
        "Enter moves focus into the detail panel"
    );

    // D1: entity actions belong to the main list. With a detail section
    // focused, `m` must not open, prefill or send anything.
    harness.key('m');
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert_eq!(harness.app.state.command_bar.input.value(), "");
    assert!(harness.app.state.detail_open());
    assert!(harness.calls().is_empty());

    // Explicitly returning focus to the main list (the panel stays open)
    // re-enables messaging the selected agent.
    harness.press(KeyCode::BackTab);
    assert_eq!(harness.app.state.focus, Focus::MainList);
    assert!(harness.app.state.detail_open());

    harness.key('m');
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"Code Reviewer\" "
    );
}

#[test]
fn msg034_esc_after_m_clears_the_bar_and_sends_nothing() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.key('m').press(KeyCode::Esc);

    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert_eq!(harness.app.state.command_bar.input.value(), "");
    assert!(harness.calls().is_empty());
}

// ── The recipient chip (§3.6.1) ──────────────────────────────────────────

#[test]
fn msg035_the_chip_keeps_the_recipient_visible_behind_a_long_body() {
    let mut harness = Harness::sized(fleet(), 80, 30);
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.key('m');
    harness.type_str("please take a look at the flaky test in the runner, it fails on CI");

    let frame = harness.text();
    assert!(
        frame.contains("→ Code Reviewer"),
        "the chip must survive a long body at 80 columns:\n{frame}"
    );
}

#[test]
fn msg036_the_chip_reports_what_enter_would_do() {
    for (input, expected) in [
        ("/msg \"Code Reviewer\" hi", "→ Code Reviewer"),
        ("/msg \"say \\\"hi\\\"\" hi", "→ say \"hi\" (disabled)"),
        ("/msg all hi", "→ all agents (3)"),
        ("/msg zzz hi", "⚠ no match: \"zzz\""),
        // `Cod` is a prefix of two names here, so the chip says how many.
        ("/msg Cod hi", "⚠ no match — 2 agents start with it"),
    ] {
        let mut harness = fleet_harness();
        harness.key('/').type_str(&input[1..]);
        let frame = harness.text();
        assert!(
            frame.contains(expected),
            "expected chip {expected:?} for {input:?}:\n{frame}"
        );
    }
}

#[test]
fn msg037_a_wide_name_does_not_overflow_the_bar() {
    let wide = || {
        FakeBackend::new().with_agents(vec![
            make_agent("agt_cjk", "名前 です", AgentStatus::Idle),
            make_agent("agt_emo", "🚀 deploy", AgentStatus::Idle),
        ])
    };
    for name in ["名前 です", "🚀 deploy"] {
        let mut harness = Harness::sized(wide(), 80, 30);
        harness.key('/').type_str(&format!(
            "msg \"{name}\" a body long enough to force truncation here"
        ));
        let frame = harness.text();
        let bar = frame
            .lines()
            .find(|line| line.contains('→'))
            .unwrap_or_else(|| panic!("no chip in:\n{frame}"));
        // The row is 80 cells wide whatever happens — `put_line` clips — so
        // the assertion is on what overflow *costs*: the cursor and the
        // cancel hint are the last two things the bar draws, so they are the
        // first things a too-wide line loses. (Not `UnicodeWidthStr::width`
        // over the flattened frame: a wide glyph holds two cells and the
        // second renders as a space, so that measure counts every CJK cluster
        // twice.)
        assert!(
            bar.contains('█'),
            "the bar overflowed and lost its cursor: {bar:?}"
        );
        assert!(
            bar.trim_end().ends_with('✕'),
            "the bar overflowed and lost its cancel: {bar:?}"
        );
        assert_eq!(bar.chars().count(), 80, "the row is one cell per column");
    }
}

// ── The cursor and its window (§3.6.2) ───────────────────────────────────

#[test]
fn msg038_the_cursor_moves_and_text_is_inserted_where_it_sits() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 1; // `Code`
    harness.key('m').type_str("hello");

    for _ in 0..5 {
        harness.press(KeyCode::Left);
    }
    harness.key('X');

    // Inserted at the insertion point, not appended.
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg Code Xhello"
    );
    let cursor = harness.app.state.command_bar.input.cursor();
    assert_eq!(cursor.pos(), cursor.len() - 5, "the cursor stayed put");
}

#[test]
fn msg039_the_window_anchors_on_the_cursor_not_the_end() {
    // Long enough to truncate at 60 columns.
    let body = "a".repeat(120);
    let mut harness = Harness::sized(fleet(), 60, 30);
    harness.key('/').type_str(&format!("msg Code {body}"));

    // Cursor at the end: the tail is on screen, as it always was.
    let frame = harness.text();
    assert!(frame.contains("aaa"), "{frame}");
    let bar = |harness: &mut Harness| {
        harness
            .text()
            .lines()
            .find(|line| line.starts_with("/ "))
            .map(str::to_owned)
            .expect("the command bar renders")
    };
    assert!(bar(&mut harness).contains('…'), "the left is clipped");

    // Ctrl+A to the start: the window follows, so the recipient is visible
    // and the `…` moves to the other end of the hidden text.
    harness.press_mod(
        KeyCode::Char('a'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    );
    let line = bar(&mut harness);
    assert!(
        line.contains("msg Code"),
        "the start of the value must be shown: {line:?}"
    );
}

#[test]
fn msg040_the_block_cursor_sits_at_the_insertion_point() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg Code hi");
    // At the end, the block trails the value exactly as it always has.
    let line = harness
        .text()
        .lines()
        .find(|line| line.starts_with("/ "))
        .map(str::to_owned)
        .expect("the bar renders");
    assert!(line.contains("hi█"), "{line:?}");

    harness.press(KeyCode::Left).press(KeyCode::Left);
    let line = harness
        .text()
        .lines()
        .find(|line| line.starts_with("/ "))
        .map(str::to_owned)
        .expect("the bar renders");
    assert!(
        line.contains("█hi"),
        "the block must move with the cursor: {line:?}"
    );
}

// ── Mouse (§3.5.4) ───────────────────────────────────────────────────────

#[test]
fn msg041_a_double_click_on_an_agent_row_fills_the_bar() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg ");
    harness.draw();
    harness.double_click_text("/msg say \"hi\"");

    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"say \\\"hi\\\"\" "
    );
    assert!(harness.calls().is_empty(), "a double click must not send");
}

#[test]
fn msg042_a_press_and_release_on_different_rows_does_nothing() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg ");
    harness.draw();
    let (x1, y1) = harness.find_text("/msg Code Reviewer").expect("row 1");
    let (x2, y2) = harness.find_text("/msg all").expect("the broadcast row");

    harness.press_at(x1, y1).release_at(x2, y2);

    // Selection follows the press, but nothing is accepted.
    assert_eq!(harness.app.state.command_bar.input.value(), "/msg ");
    assert!(harness.calls().is_empty());
}

#[test]
fn msg043_tab_and_enter_on_a_separator_row_keep_the_input() {
    // The pre-existing bug the category headers already hit: `Tab` on a
    // separator called `reset("")` and wiped what was typed.
    for key in [KeyCode::Tab, KeyCode::Enter] {
        let mut harness = fleet_harness();
        harness.key('/');
        // The bare `/` list starts with the MANAGEMENT separator.
        assert!(orch_tui::command_bar::resolve_suggestions("/")[0].is_separator());
        harness.press(key);

        assert_eq!(
            harness.app.state.command_bar.input.value(),
            "/",
            "{key:?} cleared the input"
        );
        assert_eq!(harness.app.state.input_mode, InputMode::Command);
    }
}

#[test]
fn msg044_the_wheel_still_walks_the_suggestion_list() {
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg ");
    harness.draw();
    let (x, y) = harness.find_text("/msg Code Reviewer").expect("a row");

    harness.wheel_down(x, y);
    assert_eq!(harness.app.state.command_bar.suggestion_index, 1);
    harness.wheel_up(x, y);
    assert_eq!(harness.app.state.command_bar.suggestion_index, 0);
}

// ── Discoverability (§6) ─────────────────────────────────────────────────

#[test]
fn msg045_the_agents_footer_offers_m_msg_and_the_click_opens_the_bar() {
    let mut harness = fleet_harness();
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.assert_text("m msg");

    harness.draw();
    harness.click_text("m msg");
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg \"Code Reviewer\" "
    );
}

#[test]
fn msg046_the_footer_hides_m_msg_on_a_backend_that_cannot_send() {
    let mut backend = fleet();
    backend.can_send_message = false;
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.refute_text("m msg");
}

#[test]
fn msg047_the_help_overlay_lists_the_verb_without_changing_shape() {
    let mut harness = fleet_harness();
    harness.press(KeyCode::F(1));
    harness.assert_text("/msg");
    harness.assert_text("Message agent");
    // ACTIONS is at `MAX_ROWS`, so the overlay's geometry must not move.
    harness.assert_text("NAVIGATION");
    harness.assert_text("ACTIONS");
    harness.assert_text("COMMANDS");
}

#[test]
fn msg048_help_lists_the_verb_after_mouse() {
    let mut harness = fleet_harness();
    harness.run_command("/help");
    let messages = harness.messages();
    let position = |needle: &str| {
        messages
            .iter()
            .position(|m| m.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing from {messages:#?}"))
    };
    assert!(position("/msg") > position("/mouse"));
    assert!(position("/msg") < position("/quit"));
    assert!(
        messages
            .iter()
            .any(|m| m.contains("/msg [agent] [message]  — Message an agent")),
        "{messages:#?}"
    );
}

#[test]
fn msg049_an_arm_does_not_survive_another_command() {
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    harness.assert_message("press Enter again to send");

    // The bar is **still open** with the armed text, so this has to clear the
    // line first — `key('/')` would only type a slash into it. (The earlier
    // version of this case did exactly that and never left command mode, so
    // it passed with the disarm mutated out.)
    harness.press_mod(
        KeyCode::Char('u'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    );
    harness.type_str("/status").press(KeyCode::Enter);
    assert_eq!(
        harness.app.state.input_mode,
        InputMode::None,
        "the other verb must have closed the bar"
    );
    harness.assert_message("tasks");

    // Retyping the same text asks again rather than sending on the first
    // Enter.
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);

    harness.refute_called(sent);
    harness.assert_message("press Enter again to send");
}

#[test]
fn msg049b_clicking_the_bars_close_glyph_disarms_a_broadcast() {
    // The mouse half of msg020: the `✕` in the hint row is a live zone, and
    // it used to clear the bar while leaving the arm behind — so the UI said
    // cancelled and the next Enter broadcast with no prompt (gate S2).
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    assert!(
        harness.app.armed_broadcast.is_some(),
        "the arm is the setup"
    );

    harness.draw();
    harness.click_text("Esc ✕");
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert!(harness.app.armed_broadcast.is_none(), "the arm survived ✕");
    assert!(!harness.app.retain_command_bar, "retention survived ✕");

    // The whole point: one Enter on the recalled command must not send.
    // (Ctrl-U first: with `/` in the bar the suggestion list owns `↑`.)
    harness.key('/').press_mod(
        KeyCode::Char('u'),
        ratatui::crossterm::event::KeyModifiers::CONTROL,
    );
    harness.press(KeyCode::Up).press(KeyCode::Enter);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg all ship it",
        "history must recall the command"
    );
    harness.refute_called(sent);
    harness.assert_message("press Enter again to send");
}

#[test]
fn msg049c_history_recall_and_a_retype_cannot_confirm_a_broadcast() {
    // Both reuse the *same text*, which is all the arm used to be keyed on.
    // `↑` recall:
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    harness.press(KeyCode::Up).press(KeyCode::Enter);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg all ship it"
    );
    harness.refute_called(sent);

    // Backspace and retype the same character (QA info 8): the text is
    // identical again, the edit state is not.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    harness.press(KeyCode::Backspace).key('t');
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg all ship it"
    );
    harness.press(KeyCode::Enter);
    harness.refute_called(sent);
    harness.assert_message("press Enter again to send");

    // …and the Enter after *that* one does send, because nothing touched the
    // bar in between.
    harness.press(KeyCode::Enter);
    harness.assert_called(sent);
}

#[test]
fn msg049d_a_cursor_move_or_a_paste_between_the_two_enters_disarms() {
    // Found live: `Left` cleared the visible confirm prompt but not the arm,
    // so the bar said nothing and Enter sent (gate frontend F2). The prompt
    // and the arm now go together, for **any** non-Enter key — `PageUp`
    // included, which the bar does not even consume, so nothing else about
    // the input changes.
    for nudge in [
        KeyCode::Left,
        KeyCode::Home,
        KeyCode::Right,
        KeyCode::PageUp,
    ] {
        let mut harness = fleet_harness();
        harness
            .key('/')
            .type_str("msg all ship it")
            .press(KeyCode::Enter);
        harness.press(nudge).press(KeyCode::Enter);
        harness.refute_called(sent);
        harness.assert_message("press Enter again to send");
    }

    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    harness.paste(" now").press(KeyCode::Enter);
    harness.refute_called(sent);
}

#[test]
fn msg049e_a_rewrite_of_the_bar_that_is_not_a_keystroke_also_disarms() {
    // The generation half of the arm, on its own. Every path that rewrites
    // the bar today goes through a key or through `leave_command_mode`, which
    // disarm explicitly — this pins the backstop for the next one that does
    // not: putting the identical text back is still a new edit state, so it
    // cannot inherit a confirmation.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all ship it")
        .press(KeyCode::Enter);
    assert!(harness.app.armed_broadcast.is_some());

    harness
        .app
        .state
        .command_bar
        .input
        .reset("/msg all ship it");
    harness.press(KeyCode::Enter);

    harness.refute_called(sent);
    harness.assert_message("press Enter again to send");
}

#[test]
fn msg050_a_five_thousand_character_paste_lands_whole_and_warns() {
    // The App side of a large paste: the body is handed over whole, well
    // under the 64 KiB paste cap (msg063), and the engine's own 4000-unit cut
    // is only *reported*. (What a *terminal* delivers is a separate limit — a
    // bracketed paste is all-or-nothing, so one bigger than the pty queue is
    // dropped entirely rather than partly inserted. Measured at ~4 KB under
    // tmux.)
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code ")
        .paste(&"x".repeat(5_000));
    harness.press(KeyCode::Enter);

    harness.assert_called(
        |call| matches!(call, Call::SendMessage { body, .. } if body.len() == 5_000),
    );
    harness.assert_message("Body truncated to 4000 characters");
}

#[test]
fn msg051_a_narrow_terminal_drops_the_hint_before_the_chip() {
    // Degradation order: the hint goes first, then the chip shortens, and the
    // chip never vanishes while a recipient is resolved — it is the only
    // thing on screen that says who is about to be messaged.
    // 40 columns is the minimum the dashboard renders at all (`MIN_WIDTH`).
    let mut harness = Harness::sized(fleet(), 40, 24);
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    harness.key('m').type_str("ship it");

    let frame = harness.text();
    let bar = frame
        .lines()
        .find(|line| line.starts_with("/ "))
        .unwrap_or_else(|| panic!("no bar in:\n{frame}"));
    assert!(bar.contains('→'), "the chip must survive: {bar:?}");
    assert!(
        !bar.contains("Enter exec"),
        "the hint must go before the chip: {bar:?}"
    );
    assert!(
        unicode_width::UnicodeWidthStr::width(bar.trim_end()) <= 40,
        "the bar overflowed: {bar:?}"
    );
}

// ── The retained refusal, on screen (§3.6.3 × §3.5) ──────────────────────

#[test]
fn msg052_a_shadow_refusal_is_drawn_above_the_bar_it_is_about() {
    // The feed is not rendered in command mode — the bottom panel belongs to
    // the suggestion list — so a refusal that only reached `state.messages`
    // would be the one thing off screen. Asserted on the **drawn frame**.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code Reviewer please look at #12")
        .press(KeyCode::Enter);

    let frame = harness.text();
    for needle in [
        "\"Code\" is an agent, and so is \"Code Reviewer\" — quote the one you mean:",
        "/msg \"Code\" Reviewer please look at #12",
        "/msg \"Code Reviewer\" please look at #12",
    ] {
        assert!(frame.contains(needle), "missing {needle:?} in:\n{frame}");
    }
    // Drawn directly above the bar, not a panel's height away from it.
    let rows: Vec<usize> = frame
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("quote the one you mean") || line.starts_with("/ "))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(rows.len(), 2, "expected the refusal and the bar:\n{frame}");
    assert_eq!(rows[1] - rows[0], 3, "three refusal lines then the bar");
    assert!(harness.calls().is_empty());
}

#[test]
fn msg053_an_ambiguity_refusal_is_drawn_with_its_ids() {
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_a1", "Alice", AgentStatus::Idle),
        make_agent("agt_a2", "alice", AgentStatus::Idle),
    ]);
    let mut harness = Harness::with(backend);
    harness
        .key('/')
        .type_str("msg ALICE hi")
        .press(KeyCode::Enter);

    let frame = harness.text();
    assert!(
        frame.contains("Ambiguous recipient \"ALICE\""),
        "the ids to copy must be on screen:\n{frame}"
    );
    assert!(frame.contains("agt_a1 Alice"), "{frame}");
    assert!(frame.contains("agt_a2 alice"), "{frame}");
}

#[test]
fn msg054_the_drawn_refusal_truncates_rather_than_overflowing() {
    // Long names make the shadow refusal wider than the terminal.
    let long = "Reviewer of the Signup Flow and Everything Else";
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_s", "Code", AgentStatus::Idle),
        make_agent("agt_l", &format!("Code {long}"), AgentStatus::Idle),
    ]);
    let mut harness = Harness::sized(backend, 80, 24);
    harness
        .key('/')
        .type_str(&format!("msg Code {long} please look"))
        .press(KeyCode::Enter);

    let frame = harness.text();
    assert!(frame.contains("is an agent, and so is"), "{frame}");
    for line in frame.lines() {
        assert!(
            unicode_width::UnicodeWidthStr::width(line.trim_end()) <= 80,
            "line overflowed 80 columns: {line:?}"
        );
    }
    // The ellipsis must be on a *refusal* line — the bar left-truncates with
    // one of its own, which would satisfy a frame-wide check for nothing.
    let capped = frame
        .lines()
        .filter(|line| !line.starts_with("/ "))
        .any(|line| line.contains("is an agent") && line.contains('…'));
    assert!(
        capped,
        "a too-long refusal must be capped, not wrapped:\n{frame}"
    );
    // …and capped at the *columns* it has: `"  ⚠ "` is 6 bytes and 4 columns,
    // and measuring the marker in bytes cost the line two of them.
    let capped = frame
        .lines()
        .find(|line| line.contains("is an agent"))
        .expect("the refusal line");
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(capped.trim_end()),
        80,
        "the refusal must use the whole row: {capped:?}"
    );
}

#[test]
fn msg055_the_drawn_refusal_clears_on_the_next_edit_send_or_esc() {
    let refused = |harness: &mut Harness| harness.text().contains("quote the one you mean");

    // The next keystroke clears it.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code Reviewer please")
        .press(KeyCode::Enter);
    assert!(refused(&mut harness));
    harness.press(KeyCode::Backspace);
    assert!(!refused(&mut harness), "an edit must clear it");

    // Esc clears it, and so does the next command.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code Reviewer please")
        .press(KeyCode::Enter);
    harness.press(KeyCode::Esc);
    assert!(!refused(&mut harness));
    assert!(harness.app.state.command_bar.retained_refusal.is_empty());

    // A successful send leaves nothing behind.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code Reviewer please")
        .press(KeyCode::Enter);
    harness.press(KeyCode::Esc);
    harness
        .key('/')
        .type_str("msg Code hello")
        .press(KeyCode::Enter);
    harness.assert_called(sent);
    assert!(!refused(&mut harness), "a send must clear it");
    // …and the feed still carries every line, as it always did.
    harness.assert_message("quote the one you mean");
}

#[test]
fn msg056_the_broadcast_confirm_is_drawn_where_the_user_is_looking() {
    // Not a refusal, but it keeps the bar open for the same reason and the
    // prompt is the whole point of the step.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg all standup in five")
        .press(KeyCode::Enter);

    let frame = harness.text();
    assert!(
        frame.contains("Broadcast to 3 agents — press Enter again to send"),
        "{frame}"
    );
    assert!(harness.calls().is_empty());
    // The second Enter still sends, and the prompt goes with it.
    harness.press(KeyCode::Enter);
    harness.assert_called(sent);
    assert!(!harness.text().contains("press Enter again"));
}

#[test]
fn msg057_other_verbs_draw_no_refusal_panel() {
    // Only `/msg` retains, so no other verb's refusal changes the panel.
    let mut harness = fleet_harness();
    harness.key('/').type_str("task nope").press(KeyCode::Enter);
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert!(harness.app.state.command_bar.retained_refusal.is_empty());
    // The bar closed, so the feed is back and carries the usage line.
    harness.assert_message("Usage: /task add|list");
}

#[test]
fn msg058_a_refusal_with_no_body_is_drawn_over_the_reopened_panel() {
    // `/msg Code ` + Enter is refused, and the bar it is retained with lists
    // every agent again — which used to cover the refusal, leaving the screen
    // saying nothing at all about why nothing was sent (QA repro B).
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg Code ").press(KeyCode::Enter);

    harness.assert_text("Missing message for \"Code\"");
    assert!(harness.calls().is_empty());
    // The bar keeps the text **as typed**, trailing space included, so the
    // agent list stays closed and the refusal has those rows to itself.
    assert_eq!(harness.app.state.command_bar.input.value(), "/msg Code ");
    harness.refute_text("· agt_rev");
    // Backspace reopens the list — and clears the refusal with it.
    harness.press(KeyCode::Backspace);
    harness.assert_text("· agt_rev");
    harness.refute_text("Missing message for");
}

#[test]
fn msg059_the_bare_verb_plus_enter_reports_its_usage() {
    // The verb row's fill would only add the space the user is already past,
    // so Enter runs the verb and its Usage line explains what is missing
    // (§3.2) — it used to fill and say nothing.
    let mut harness = fleet_harness();
    harness.key('/').type_str("msg").press(KeyCode::Enter);

    harness.assert_text("Usage: /msg <agent|all> <message>");
    assert_eq!(harness.app.state.command_bar.input.value(), "/msg");
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert!(harness.calls().is_empty());
    // Tab still fills, which is what Tab is for.
    harness.press(KeyCode::Tab);
    assert_eq!(harness.app.state.command_bar.input.value(), "/msg ");

    // Only the **bare verb** runs: a finished recipient still fills, because
    // the space it adds is the one the body goes after — refusing there would
    // leave the user typing `/msg "Code Reviewer"hi`.
    for (typed, filled) in [
        ("msg \"Code Reviewer\"", "/msg \"Code Reviewer\" "),
        ("msg Code", "/msg Code "),
        ("msg all", "/msg all "),
    ] {
        let mut harness = fleet_harness();
        harness.key('/').type_str(typed).press(KeyCode::Enter);
        assert_eq!(
            harness.app.state.command_bar.input.value(),
            filled,
            "{typed:?} must fill"
        );
        assert!(harness.calls().is_empty());
    }
}

#[test]
fn msg060_an_exactly_typed_name_is_the_row_that_tab_fills() {
    // Gate QA repro A: with `alice` and `Alice`, `/msg Alice` showed
    // `→ Alice` and Tab rewrote it to `/msg alice `, so the body went to the
    // other agent. Both creation orders, because the bug was a sort order.
    for (first, second) in [("agt_lo", "alice", "agt_up", "Alice")]
        .into_iter()
        .flat_map(|(id_a, a, id_b, b)| [((id_a, a), (id_b, b)), ((id_b, b), (id_a, a))])
    {
        for typed in ["Alice", "\"Alice\""] {
            let backend = FakeBackend::new().with_agents(vec![
                make_agent(first.0, first.1, AgentStatus::Idle),
                make_agent(second.0, second.1, AgentStatus::Idle),
            ]);
            let mut harness = Harness::with(backend);
            harness.key('/').type_str(&format!("msg {typed}"));
            harness.press(KeyCode::Tab);
            assert_eq!(
                harness.app.state.command_bar.input.value(),
                "/msg Alice ",
                "{typed:?} with {first:?} created first"
            );
            // …and the chip agrees with what the fill chose.
            harness.assert_text("→ Alice");
            harness.type_str("ship it").press(KeyCode::Enter);
            harness.assert_called(|call| {
                matches!(call, Call::SendMessage { target, .. }
                    if *target == orch_tui::backend::MessageTarget::Agent("agt_up".into()))
            });
        }
    }
}

#[test]
fn msg061_a_body_that_starts_a_longer_name_is_refused_too() {
    // `/msg Code Rev hi` committed to `Code` and sent "Rev hi" — the body's
    // first word was eaten as part of a name it only started (gate code (a)).
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code Rev hi")
        .press(KeyCode::Enter);

    assert!(harness.calls().is_empty(), "nothing may be sent");
    harness.assert_message("\"Code\" is an agent, and so is \"Code Reviewer\"");
    harness.assert_message("/msg \"Code\" Rev hi");
    harness.assert_message("/msg \"Code Reviewer\" hi");
    // An ordinary body still commits: `hello` starts no agent's name.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code hello")
        .press(KeyCode::Enter);
    harness.assert_called(sent);
}

// ── The window, its budget and its cost (§3.6.2) ─────────────────────────

#[test]
fn msg062_a_sixty_four_kilobyte_value_still_renders_a_frame_promptly() {
    // The quadratic left-trim took 5 s per *frame* at 20k characters and 8 s
    // at 64k, on every keystroke and every animation tick (gate S1/F1). The
    // bound is deliberately generous — this is an order-of-magnitude assert,
    // not a benchmark.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code ")
        .paste(&"x".repeat(64_000));

    let start = std::time::Instant::now();
    for _ in 0..10 {
        harness.draw();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "10 frames with a 64k value took {elapsed:?}"
    );

    // …and typing into it still works, which is what the user does next.
    let start = std::time::Instant::now();
    harness.type_str("tail").draw();
    assert!(
        harness
            .app
            .state
            .command_bar
            .input
            .value()
            .ends_with("xtail"),
        "the keystrokes landed at the cursor"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn msg063_a_paste_over_the_cap_is_cut_and_says_so() {
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code ")
        .paste(&"x".repeat(200_000));

    let value = harness.app.state.command_bar.input.value();
    assert_eq!(
        value.len(),
        "/msg Code ".len() + 64 * 1024,
        "the paste must be cut at 64 KiB"
    );
    harness.assert_message("Pasted text cut at 64 KiB");
    // …and it is *drawn*, because the feed it also lands in is not rendered
    // while the bar is open — the cut is about the text now in the bar.
    harness.assert_text("Pasted text cut at 64 KiB");
    assert!(
        !harness.app.retain_command_bar,
        "an advisory must not make the next command retain the bar"
    );
    // Under the cap nothing is cut and nothing is said.
    let mut harness = fleet_harness();
    harness
        .key('/')
        .type_str("msg Code ")
        .paste(&"x".repeat(5_000));
    harness.refute_message("Pasted text cut");
}

#[test]
fn msg064_the_chip_leaves_the_body_a_readable_window_at_eighty_columns() {
    // With the chip *and* the 46-column hint, 80 columns left 8 for the body
    // — about one word of what the user was typing (gate frontend F1).
    let mut harness = Harness::sized(fleet(), 80, 24);
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0; // `Code Reviewer`
    harness.key('m');
    harness.type_str("the signup test is flaky on CI and has been for a week");

    let frame = harness.text();
    let bar = frame
        .lines()
        .find(|line| line.starts_with("/ "))
        .unwrap_or_else(|| panic!("no bar in:\n{frame}"));
    assert!(bar.contains("→ Code Reviewer"), "{bar:?}");
    // The window is what sits between the chip's divider and the cursor.
    let window = bar
        .split_once("│ ")
        .expect("the chip divider")
        .1
        .split('█')
        .next()
        .expect("the cursor");
    let columns = unicode_width::UnicodeWidthStr::width(window);
    assert!(
        columns >= 25,
        "the body window is {columns} columns: {bar:?}"
    );
    // The long hint is what paid for it, and `Esc ✕` — the only clickable way
    // out of the bar — is what survives.
    assert!(!bar.contains("Enter exec"), "{bar:?}");
    assert!(bar.contains("Esc ✕"), "{bar:?}");
}

#[test]
fn msg065_a_long_command_shows_the_window_it_always_did() {
    // The cursor cell is counted once, by the caller: counting it again here
    // cost every verb a column against the pre-chip bar (gate code F4). The
    // window is master's `maxLen = width - 6 - hint - completion - 1` — at
    // 100 columns with the 45-character command hint and no completion,
    // `100 - 6 - 45 - 0 - 1` = 48 columns, the leading `…` included.
    let mut harness = Harness::sized(FakeBackend::new(), 100, 30);
    harness
        .key('/')
        .type_str(&format!("task add {}", "x".repeat(200)));

    let frame = harness.text();
    let bar = frame
        .lines()
        .find(|line| line.starts_with("/ "))
        .unwrap_or_else(|| panic!("no bar in:\n{frame}"));
    let window: String = bar.chars().skip(2).take_while(|ch| *ch != '█').collect();
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(window.as_str()),
        48,
        "window {window:?} in {bar:?}"
    );
    assert!(window.starts_with('…'), "{window:?}");
    assert!(bar.contains("Enter exec"), "the hint stays: {bar:?}");
}

#[test]
fn msg066_a_failed_send_keeps_the_body_on_screen() {
    // A transient store error is no reason to make the user find a pasted
    // stack trace again (gate code F5).
    let mut harness = fleet_harness();
    harness.app.backend.fail_next = Some("Disk is on fire".into());
    harness
        .key('/')
        .type_str("msg Code the build is broken")
        .press(KeyCode::Enter);

    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(
        harness.app.state.command_bar.input.value(),
        "/msg Code the build is broken"
    );
    harness.assert_text("Failed: Disk is on fire");

    // So does a backend that cannot message at all.
    let mut harness = fleet_harness();
    harness.app.backend.can_send_message = false;
    harness
        .key('/')
        .type_str("msg Code hi")
        .press(KeyCode::Enter);
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    harness.assert_text("Messaging not available");
}

#[test]
fn msg067_the_help_overlay_offers_the_m_binding() {
    // Discovery was footer-only, and the footer is cut off at 60 columns
    // (gate frontend F3).
    let mut harness = fleet_harness();
    harness.press(KeyCode::F(1));
    let frame = harness.text();
    assert!(frame.contains("Message agent"), "{frame}");
    let row = frame
        .lines()
        .find(|line| line.contains("m         Message agent"))
        .unwrap_or_else(|| panic!("no `m` row in:\n{frame}"));
    // In the NAVIGATION column — left of the first divider — so ACTIONS stays
    // at `MAX_ROWS` and the box does not move.
    assert!(
        row.find("m         Message agent") < row.find('│'),
        "{row:?}"
    );
    harness.assert_text("NAVIGATION");
    harness.assert_text("ACTIONS");
    harness.assert_text("COMMANDS");
}

#[test]
fn msg068_an_agent_name_carrying_escapes_renders_no_control_bytes() {
    // The protection is ratatui's (`Buffer::set_stringn` drops any grapheme
    // containing a control character), and nothing pinned it — an upgrade or
    // a path that wrote to stdout directly could take it away silently
    // (gate S4).
    let backend = FakeBackend::new().with_agents(vec![make_agent(
        "agt_esc",
        "ev\u{1b}[2J\u{1b}[Hil\u{7}\u{9b}31m",
        AgentStatus::Idle,
    )]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = 0;
    // Through the prefill, the chip, the suggestion row and the refusal.
    harness.key('m');
    let frame = harness.text();
    harness.press(KeyCode::Enter);
    let refusal = harness.text();
    harness.press(KeyCode::Esc).key('/').type_str("msg ev");
    let rows = harness.text();

    for frame in [frame, refusal, rows] {
        assert!(
            !frame.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control byte reached the buffer: {:?}",
            frame.escape_debug().to_string()
        );
        // The escapes are defanged, not hidden: the cluster carrying each
        // control character is dropped and its payload stays as plain text,
        // so the operator can see the name is a hostile one.
        assert!(frame.contains("[2J"), "the name still renders: {frame}");
    }
}
