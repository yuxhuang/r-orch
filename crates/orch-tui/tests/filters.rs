//! Counted filter cases: both logs pickers and every filter predicate
//! (docs/parity/tui.md §1.4, §2.5, §2.6, §3.3, §3.4, §8.6), numbered `f001..`.

mod common;

use common::Harness;
use orch_domain::AgentStatus;
use orch_tui::backend::{FakeBackend, make_agent};
use orch_tui::messages::{Message, MsgType, preset_label, preset_types, type_filter_label};
use orch_tui::state::ViewId;
use orch_tui::theme::Token;
use ratatui::crossterm::event::KeyCode;

fn logs_harness() -> Harness {
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_1", "alpha", AgentStatus::Idle),
        make_agent("agt_2", "beta", AgentStatus::Idle),
        make_agent("agt_3", "gamma", AgentStatus::Idle),
    ]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness
}

fn seed_messages(harness: &mut Harness) {
    harness.app.state.messages = vec![
        Message::new(1, 0, "alpha output", Token::White, MsgType::Output)
            .with_agent(Some("agt_1".into())),
        Message::new(2, 0, "beta tool", Token::Cyan, MsgType::Tool)
            .with_agent(Some("agt_2".into())),
        Message::new(3, 0, "systemic", Token::Dim, MsgType::System),
        Message::new(4, 0, "boom", Token::Red, MsgType::Error).with_agent(Some("agt_1".into())),
    ];
}

/// 40 messages, `msg-00` … `msg-39`, alternating `Output` and `Error` so a
/// type filter leaves a sparse, non-contiguous set of indices behind.
fn seed_numbered_messages(harness: &mut Harness) {
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
}

/// How tall the logs rows area is in this frame: the main region, less the
/// status bar and the agent-chip row (`views/logs.rs`).
fn logs_view_h(harness: &Harness) -> usize {
    harness.app.state.regions(true).main_h as usize - 2
}

/// The `msg-NN` numbers the frame actually drew, top row first.
fn rendered_log_numbers(harness: &mut Harness) -> Vec<usize> {
    harness
        .text()
        .lines()
        .filter_map(|line| line.split_once("msg-"))
        .map(|(_, rest)| rest[..2].parse::<usize>().expect("a two-digit number"))
        .collect()
}

/// The numbers [`orch_tui::state::AppState::visible_log_indices`] says are on
/// screen, read back through the message list the renderer reads.
fn mapped_log_numbers(harness: &Harness) -> Vec<usize> {
    let state = &harness.app.state;
    state
        .visible_log_indices(logs_view_h(harness))
        .into_iter()
        .map(|index| state.messages[index].text["msg-".len()..].parse().unwrap())
        .collect()
}

// ── Agent picker ───────────────────────────────────────────────────────────

#[test]
fn f001_the_agent_picker_opens_with_the_current_filter_as_its_draft() {
    let mut harness = logs_harness();
    harness.app.state.log_agent_filter = ["agt_2".to_string()].into_iter().collect();
    harness.key('a');
    assert_eq!(harness.app.state.picker_draft.len(), 1);
    assert!(harness.app.state.picker_draft.contains("agt_2"));
}

#[test]
fn f002_an_empty_selection_renders_as_all_checked() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.assert_text("3/3 selected");
    harness.assert_text("[✓]");
}

#[test]
fn f003_up_wraps_to_the_last_agent() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.press(KeyCode::Up);
    assert_eq!(harness.app.state.picker_cursor, 2);
}

#[test]
fn f004_down_wraps_to_the_first_agent() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.press(KeyCode::Down);
    harness.press(KeyCode::Down);
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.picker_cursor, 0);
}

#[test]
fn f005_space_from_implicit_all_produces_a_single_explicit_selection() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.press(KeyCode::Down);
    harness.key(' ');
    assert_eq!(harness.app.state.picker_draft.len(), 1);
    assert!(harness.app.state.picker_draft.contains("agt_2"));
}

#[test]
fn f006_space_toggles_an_agent_off_again() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.key(' ');
    harness.key(' ');
    assert!(harness.app.state.picker_draft.is_empty());
}

#[test]
fn f007_a_resets_the_draft_to_all_agents() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.key(' ');
    harness.key('a');
    assert!(harness.app.state.picker_draft.is_empty());
    harness.assert_text("3/3 selected");
}

#[test]
fn f008_enter_confirms_the_explicit_selection() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.key(' ');
    harness.press(KeyCode::Enter);
    assert!(!harness.app.state.show_agent_picker);
    assert_eq!(harness.app.state.log_agent_filter.len(), 1);
}

#[test]
fn f009_enter_confirms_an_empty_set_as_no_filter() {
    let mut harness = logs_harness();
    harness.app.state.log_agent_filter = ["agt_1".to_string()].into_iter().collect();
    harness.key('a');
    harness.key('a');
    harness.press(KeyCode::Enter);
    assert!(
        harness.app.state.log_agent_filter.is_empty(),
        "an empty set is a legal confirm value"
    );
}

#[test]
fn f010_esc_cancels_the_agent_picker_without_confirming() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.key(' ');
    harness.press(KeyCode::Esc);
    assert!(!harness.app.state.show_agent_picker);
    assert!(harness.app.state.log_agent_filter.is_empty());
}

#[test]
fn f011_the_agent_picker_footer_is_verbatim() {
    let mut harness = logs_harness();
    harness.key('a');
    harness.assert_text("Space toggle · a all · Enter confirm · Esc cancel");
}

#[test]
fn f012_the_agent_picker_shows_per_agent_message_counts() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.key('a');
    harness.assert_text("·2");
}

// ── Type picker ────────────────────────────────────────────────────────────

#[test]
fn f013_the_type_picker_lists_all_eight_labels() {
    let mut harness = logs_harness();
    harness.key('f');
    for label in [
        "System",
        "Lifecycle",
        "Output",
        "Tool calls",
        "Results",
        "Errors",
        "Files",
        "Info",
    ] {
        harness.assert_text(label);
    }
}

#[test]
fn f014_the_type_picker_opens_with_the_current_filter_as_its_draft() {
    let mut harness = logs_harness();
    harness.key('f');
    assert_eq!(harness.app.state.type_picker_draft.len(), 8);
    harness.assert_text("8/8 selected");
}

#[test]
fn f015_type_picker_up_and_down_wrap_over_eight_rows() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.press(KeyCode::Up);
    assert_eq!(harness.app.state.picker_cursor, 7);
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.picker_cursor, 0);
}

#[test]
fn f016_space_toggles_the_type_under_the_cursor() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key(' ');
    assert_eq!(harness.app.state.type_picker_draft.len(), 7);
    harness.assert_text("7/8 selected");
}

#[test]
fn f017_a_deselects_all_when_everything_is_selected() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('a');
    assert!(harness.app.state.type_picker_draft.is_empty());
    harness.assert_text("0/8 selected");
}

#[test]
fn f018_a_selects_all_when_something_is_missing() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key(' ');
    harness.key('a');
    assert_eq!(harness.app.state.type_picker_draft.len(), 8);
}

#[test]
fn f019_preset_1_selects_all_eight_types() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('a');
    harness.key('1');
    assert_eq!(harness.app.state.type_picker_draft.len(), 8);
}

#[test]
fn f020_preset_2_is_text() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('2');
    assert_eq!(
        harness.app.state.type_picker_draft,
        preset_types("text"),
        "exactly {{output}}"
    );
    assert_eq!(harness.app.state.type_picker_draft.len(), 1);
}

#[test]
fn f021_preset_3_is_tools() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('3');
    assert_eq!(harness.app.state.type_picker_draft.len(), 3);
    assert!(harness.app.state.type_picker_draft.contains(&MsgType::Tool));
    assert!(
        harness
            .app
            .state
            .type_picker_draft
            .contains(&MsgType::Result)
    );
    assert!(harness.app.state.type_picker_draft.contains(&MsgType::File));
}

#[test]
fn f022_preset_4_is_errors() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('4');
    assert_eq!(harness.app.state.type_picker_draft.len(), 1);
    assert!(
        harness
            .app
            .state
            .type_picker_draft
            .contains(&MsgType::Error)
    );
}

#[test]
fn f023_preset_5_is_events() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('5');
    assert_eq!(harness.app.state.type_picker_draft.len(), 2);
    assert!(
        harness
            .app
            .state
            .type_picker_draft
            .contains(&MsgType::Lifecycle)
    );
    assert!(
        harness
            .app
            .state
            .type_picker_draft
            .contains(&MsgType::System)
    );
}

#[test]
fn f024_enter_normalises_an_empty_draft_to_all_eight() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('a');
    harness.press(KeyCode::Enter);
    assert_eq!(
        harness.app.state.log_type_filter.len(),
        8,
        "unlike the agent picker, empty means all"
    );
}

#[test]
fn f025_enter_confirms_a_partial_draft() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('4');
    harness.press(KeyCode::Enter);
    assert_eq!(
        type_filter_label(&harness.app.state.log_type_filter),
        "errors"
    );
}

#[test]
fn f026_esc_cancels_the_type_picker_without_confirming() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.key('4');
    harness.press(KeyCode::Esc);
    assert_eq!(harness.app.state.log_type_filter.len(), 8);
}

#[test]
fn f027_the_type_picker_shows_the_preset_line() {
    let mut harness = logs_harness();
    harness.key('f');
    harness.assert_text("1=all · 2=text · 3=tools · 4=errors · 5=events");
}

// ── Filter predicates ──────────────────────────────────────────────────────

#[test]
fn f028_the_agent_filter_keeps_messages_without_an_agent_id() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.app.state.log_agent_filter = ["agt_1".to_string()].into_iter().collect();
    let kept: Vec<&str> = harness
        .app
        .state
        .filtered_messages()
        .iter()
        .map(|i| harness.app.state.messages[*i].text.as_str())
        .collect();
    assert_eq!(kept, ["alpha output", "systemic", "boom"]);
}

#[test]
fn f029_the_type_filter_drops_unselected_types() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.app.state.log_type_filter = preset_types("errors");
    let kept: Vec<&str> = harness
        .app
        .state
        .filtered_messages()
        .iter()
        .map(|i| harness.app.state.messages[*i].text.as_str())
        .collect();
    assert_eq!(kept, ["boom"]);
}

#[test]
fn f030_both_filters_compose() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.app.state.log_agent_filter = ["agt_2".to_string()].into_iter().collect();
    harness.app.state.log_type_filter = preset_types("tools");
    let kept: Vec<&str> = harness
        .app
        .state
        .filtered_messages()
        .iter()
        .map(|i| harness.app.state.messages[*i].text.as_str())
        .collect();
    assert_eq!(kept, ["beta tool"]);
}

#[test]
fn f031_the_activity_filter_is_skipped_entirely_when_full_size() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    assert_eq!(harness.app.state.activity_messages().len(), 4);
}

#[test]
fn f032_the_activity_filter_text_preset_hides_lifecycle_events() {
    let mut harness = logs_harness();
    harness.app.state.messages = vec![
        Message::new(1, 0, "[thread.started]", Token::Dim, MsgType::Lifecycle),
        Message::new(2, 0, "real output", Token::White, MsgType::Output),
    ];
    harness.app.state.activity_filter = preset_types("text");
    let kept: Vec<&str> = harness
        .app
        .state
        .activity_messages()
        .iter()
        .map(|i| harness.app.state.messages[*i].text.as_str())
        .collect();
    assert_eq!(kept, ["real output"]);
}

#[test]
fn f033_the_activity_preset_cycles_all_text_tools_errors_events() {
    let mut harness = Harness::new();
    harness.app.state.view = ViewId::Tasks;
    for expected in ["text", "tools", "errors", "events", "all"] {
        harness.key('f');
        assert_eq!(preset_label(&harness.app.state.activity_filter), expected);
    }
}

#[test]
fn f034_the_log_type_preset_cycles_the_same_way() {
    let mut harness = logs_harness();
    for expected in ["text", "tools", "errors", "events", "all"] {
        harness.key('F');
        assert_eq!(
            type_filter_label(&harness.app.state.log_type_filter),
            expected
        );
    }
}

// ── Logs status bar ────────────────────────────────────────────────────────

#[test]
fn f035_with_no_filters_the_bar_shows_neither_agent_nor_type_markers() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    let text = harness.text();
    assert!(!text.contains("agent:"), "{text}");
    assert!(!text.contains("type:"), "{text}");
    assert!(!text.contains(" f:"), "{text}");
}

#[test]
fn f036_cycling_the_type_filter_shows_an_f_prefixed_marker() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.key('F');
    harness.assert_text("f:text");
}

#[test]
fn f037_an_agent_filter_shows_the_selected_over_total_count() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.key('a');
    harness.key(' ');
    harness.press(KeyCode::Enter);
    harness.assert_text("1/3 agents");
}

#[test]
fn f038_the_live_chip_shows_in_tail_mode_and_the_browse_chip_when_selecting() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.assert_text("LIVE");
    harness.press(KeyCode::Up);
    harness.assert_text("3/4");
}

#[test]
fn f039_the_event_count_reflects_the_filtered_set() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.assert_text("4 events");
    harness.app.state.log_type_filter = preset_types("errors");
    harness.assert_text("1 events");
}

#[test]
fn f040_the_logs_status_bar_advertises_its_three_filter_keys() {
    let mut harness = logs_harness();
    harness.assert_text("a filter");
    harness.assert_text("f type");
    harness.assert_text("F cycle");
}

#[test]
fn f041_an_empty_feed_renders_the_waiting_box() {
    let mut harness = logs_harness();
    harness.assert_text("Waiting for activity");
}

#[test]
fn f042_filters_that_hide_everything_render_the_filtered_empty_state() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.app.state.log_type_filter = [MsgType::Info].into_iter().collect();
    harness.assert_text("No events for current filter.");
}

#[test]
fn f043_the_agent_chip_row_dims_filtered_out_agents() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.key('a');
    harness.key(' ');
    harness.press(KeyCode::Enter);
    // Every agent still appears; only the styling differs.
    harness.assert_text("alpha");
    harness.assert_text("beta");
    harness.assert_text("gamma");
}

#[test]
fn f044_opening_a_picker_shrinks_the_logs_list() {
    let mut harness = logs_harness();
    seed_messages(&mut harness);
    harness.key('a');
    harness.assert_text("Agent Filter");
    harness.assert_text("LIVE");
}

// ── Row → message-index mapping (PARITY.md §14.20) ─────────────────────────

#[test]
fn f045_the_visible_window_matches_the_rendered_rows_in_tail_mode() {
    let mut harness = logs_harness();
    seed_numbered_messages(&mut harness);
    let view_h = logs_view_h(&harness);
    let drawn = rendered_log_numbers(&mut harness);

    assert_eq!(drawn, mapped_log_numbers(&harness));
    // Tail mode is pinned to the newest message, which is the last row.
    assert_eq!(drawn.len(), view_h);
    assert_eq!(drawn.last(), Some(&39));
    assert_eq!(drawn.first(), Some(&(40 - view_h)));
}

#[test]
fn f046_the_visible_window_matches_the_rendered_rows_while_browsing() {
    let mut harness = logs_harness();
    seed_numbered_messages(&mut harness);
    // The first `Up` leaves tail mode on the last message; the rest walk back
    // through the list, moving `log_scroll_offset` with them.
    for _ in 0..12 {
        harness.press(KeyCode::Up);
    }
    assert!(harness.app.state.log_selected_index >= 0);
    assert_eq!(
        rendered_log_numbers(&mut harness),
        mapped_log_numbers(&harness)
    );

    // And from a window that is neither at the head nor at the tail.
    harness.app.state.log_scroll_offset = 7;
    assert_eq!(
        rendered_log_numbers(&mut harness),
        mapped_log_numbers(&harness)
    );
    assert_eq!(rendered_log_numbers(&mut harness).first(), Some(&7));
}

#[test]
fn f047_the_window_indexes_the_filtered_set_and_not_the_message_list() {
    let mut harness = logs_harness();
    seed_numbered_messages(&mut harness);
    // Only the odd `msg-NN` are errors, so a correct window is odd-numbered
    // throughout; indexing `messages` instead of `filtered_messages` would
    // draw a contiguous run.
    harness.app.state.log_type_filter = preset_types("errors");

    let tail = rendered_log_numbers(&mut harness);
    assert_eq!(tail, mapped_log_numbers(&harness));
    assert!(tail.iter().all(|n| n % 2 == 1), "{tail:?}");
    assert_eq!(tail.last(), Some(&39));

    harness.app.state.log_selected_index = 4;
    harness.app.state.log_scroll_offset = 3;
    let browsed = rendered_log_numbers(&mut harness);
    assert_eq!(browsed, mapped_log_numbers(&harness));
    // Filtered index 3 is `msg-07`, not `msg-03`.
    assert_eq!(browsed.first(), Some(&7));
}
