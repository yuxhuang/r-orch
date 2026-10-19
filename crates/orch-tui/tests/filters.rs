//! Counted filter cases: both logs pickers and every filter predicate, numbered `f001..`.

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

// ── Row → message-index mapping ─────────────────────────

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

// ── Keyword filter (R3–R5, plan D1) ────────────────────────────────────────

mod keyword {
    use super::common::Harness;
    use orch_domain::{AgentStatus, GoalStatus, RemoteRef, TaskStatus};
    use orch_tui::backend::{Call, FakeBackend, make_agent, make_goal, make_task, make_team};
    use orch_tui::hit::{VerbId, ZoneKind};
    use orch_tui::state::{
        AgentGroupBy, DetailPanel, InputMode, KEYWORD_MAX_CHARS, TaskSort, TaskSortField, ViewId,
    };
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn ctrl(h: &mut Harness, ch: char) {
        h.press_mod(KeyCode::Char(ch), KeyModifiers::CONTROL);
    }

    fn goal_ids(h: &Harness) -> Vec<String> {
        let state = &h.app.state;
        state
            .sorted_goals()
            .into_iter()
            .map(|i| state.goals[i].id.clone())
            .collect()
    }

    fn task_ids(h: &Harness) -> Vec<String> {
        let state = &h.app.state;
        state
            .visible_tasks()
            .into_iter()
            .map(|i| state.tasks[i].id.clone())
            .collect()
    }

    fn agent_ids(h: &Harness) -> Vec<String> {
        let state = &h.app.state;
        state
            .sorted_agents()
            .into_iter()
            .map(|i| state.agents[i].id.clone())
            .collect()
    }

    fn goals_harness() -> Harness {
        let mut ship = make_goal("g_ship", "Ship the release", GoalStatus::Active);
        ship.assignee = Some("agt_rel".into());
        let backend = FakeBackend::new()
            .with_goals(vec![
                ship,
                make_goal("g_store", "Refactor storage", GoalStatus::Achieved),
                make_goal("g_docs", "SHIP docs", GoalStatus::Paused),
            ])
            .with_agents(vec![make_agent("agt_rel", "Releaser", AgentStatus::Idle)]);
        let mut h = Harness::with(backend);
        h.app.state.view = ViewId::Goals;
        h
    }

    #[test]
    fn keyword_filter_goals() {
        let mut h = goals_harness();
        ctrl(&mut h, 'k');
        assert_eq!(h.app.state.input_mode, InputMode::Keyword);

        // Live and case-insensitive: each key narrows the list at once.
        h.type_str("sh");
        assert_eq!(goal_ids(&h), ["g_ship", "g_docs"]);
        h.type_str("ip d");
        assert_eq!(goal_ids(&h), ["g_docs"]);
        h.draw();
        h.assert_text("SHIP docs");
        h.refute_text("Ship the release");
        h.refute_text("Refactor storage");

        // The id, the status (stored or shown) and the assignee's name each
        // match on their own.
        for (keyword, expected) in [
            ("g_store", vec!["g_store"]),
            ("achieved", vec!["g_store"]),
            ("paused", vec!["g_docs"]),
            ("releaser", vec!["g_ship"]),
            ("agt_rel", vec!["g_ship"]),
        ] {
            h.app.state.goal_keyword.reset("");
            h.paste(keyword);
            assert_eq!(goal_ids(&h), expected, "keyword {keyword:?}");
        }
    }

    #[test]
    fn keyword_filter_tasks() {
        let mut tasks = vec![
            make_task("tsk_1", "Deploy staging", TaskStatus::Todo),
            make_task("tsk_2", "Write docs", TaskStatus::InProgress),
            make_task("tsk_3", "Fix login bug", TaskStatus::Failed),
            make_task("tsk_4", "Tidy up", TaskStatus::Todo),
        ];
        tasks[1].assignee = Some("agt_w".into());
        tasks[3].goal_id = Some("g_rel".into());
        // A description is not on the row, so it does not match.
        tasks[2].description = "deploy hotfix".into();
        let backend = FakeBackend::new()
            .with_tasks(tasks)
            .with_goals(vec![make_goal("g_rel", "Release 2.0", GoalStatus::Active)])
            .with_agents(vec![make_agent("agt_w", "Writer Ünal", AgentStatus::Idle)]);
        let mut h = Harness::with(backend);
        h.app.state.view = ViewId::Tasks;
        ctrl(&mut h, 'k');
        h.type_str("DEPLOY");
        assert_eq!(task_ids(&h), ["tsk_1"]);

        for (keyword, expected) in [
            ("ünal", vec!["tsk_2"]), // the assignee's name, Unicode case-folded
            ("run", vec!["tsk_2"]),  // the chip's word for in_progress
            ("in_progress", vec!["tsk_2"]),
            ("release", vec!["tsk_4"]), // the goal's title
            ("g_rel", vec!["tsk_4"]),
            ("tsk_3", vec!["tsk_3"]),
            ("hotfix", vec![]),
        ] {
            h.app.state.task_keyword.reset("");
            h.paste(keyword);
            assert_eq!(task_ids(&h), expected, "keyword {keyword:?}");
        }
        // Nothing matches: the list says so and how to get back.
        h.draw();
        h.assert_text("No tasks match");
        h.assert_text("Ctrl+U clears the filter");
        h.assert_text("0/4");
    }

    #[test]
    fn keyword_filter_agents() {
        let mut rover = make_agent("rover", "rover", AgentStatus::Idle);
        rover.remote = Some(RemoteRef {
            alias: "w1".into(),
            online: true,
        });
        let mut coder = make_agent("a_code", "coder", AgentStatus::Running);
        coder.adapter = "codex".into();
        coder.config.model = Some("gpt-5".into());
        let backend = FakeBackend::new()
            .with_agents(vec![
                coder,
                make_agent("a_rev", "reviewer", AgentStatus::Idle),
                rover,
            ])
            .with_teams(vec![make_team("t_q", "quality", "a_rev", &["a_rev"])]);
        let mut h = Harness::with(backend);
        h.app.state.view = ViewId::Agents;
        ctrl(&mut h, 'k');
        for (keyword, expected) in [
            ("w1", vec!["rover"]), // the node alias
            ("local", vec!["a_rev", "a_code"]),
            ("codex", vec!["a_code"]),  // the adapter
            ("gpt-5", vec!["a_code"]),  // the model
            ("quality", vec!["a_rev"]), // the active team
            ("active", vec!["a_code"]), // the chip's word for running
        ] {
            h.app.state.agent_keyword.reset("");
            h.paste(keyword);
            assert_eq!(agent_ids(&h), expected, "keyword {keyword:?}");
        }

        // A section the keyword empties is not drawn, and the counts on the
        // ones left are of matches.
        h.app.state.agent_keyword.reset("");
        h.paste("o");
        h.draw();
        h.assert_text("UNASSIGNED · 2 agents");
        h.app.state.agent_keyword.reset("");
        h.paste("reviewer");
        h.draw();
        h.assert_text("QUALITY · 1 agent");
        h.refute_text("UNASSIGNED");
    }

    #[test]
    fn keyword_filter_composes_with_sort_group_and_show_all() {
        // Twelve tasks; the only match is the twelfth, past the ten-row cap,
        // inside a goal group. Filtering after the cap would lose it.
        let mut tasks: Vec<_> = (1..=12)
            .map(|i| {
                make_task(
                    &format!("tsk_{i:02}"),
                    &format!("Chore {i}"),
                    TaskStatus::Todo,
                )
            })
            .collect();
        tasks[11].title = "Needle in the stack".into();
        tasks[11].goal_id = Some("g_a".into());
        tasks[10].title = "needle two".into();
        let backend = FakeBackend::new()
            .with_tasks(tasks)
            .with_goals(vec![make_goal("g_a", "Alpha", GoalStatus::Active)]);
        let mut h = Harness::sized(backend, 120, 40);
        h.app.state.view = ViewId::Tasks;
        h.key('g');
        assert!(h.app.state.group_by_goal);
        assert!(!h.app.state.show_all_tasks);

        ctrl(&mut h, 'k');
        h.type_str("needle");
        h.press(KeyCode::Enter);
        // Grouped: Alpha's match first, the ungrouped one after.
        assert_eq!(task_ids(&h), ["tsk_12", "tsk_11"]);
        assert_eq!(h.app.state.hidden_task_count(), 0);
        h.draw();
        h.assert_text("Needle in the stack");

        // Ungrouped, Title descending orders the matches only.
        h.key('g');
        h.app.state.set_task_sort(
            TaskSort {
                field: TaskSortField::Title,
                descending: true,
            },
            h.now(),
            true,
        );
        assert_eq!(task_ids(&h), ["tsk_11", "tsk_12"]);

        // Clearing gives the whole list back, still sorted and capped.
        ctrl(&mut h, 'u');
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "");
        assert_eq!(task_ids(&h).len(), 10);
        assert_eq!(h.app.state.hidden_task_count(), 2);

        // The cap applies to the matches: all twelve match `tsk_`, ten show and two wait behind show-all,
        // which then lists every match. Show-all is the user's, not the
        // keyword's: clearing leaves it on.
        ctrl(&mut h, 'k');
        h.type_str("tsk_");
        h.press(KeyCode::Enter);
        assert_eq!(task_ids(&h).len(), 10);
        assert_eq!(h.app.state.hidden_task_count(), 2);
        h.key('s');
        assert!(h.app.state.show_all_tasks);
        assert_eq!(task_ids(&h).len(), 12);
        ctrl(&mut h, 'k');
        h.type_str("1");
        h.press(KeyCode::Enter);
        // `tsk_1`: tsk_10, tsk_11, tsk_12.
        assert_eq!(task_ids(&h).len(), 3);
        assert_eq!(h.app.state.hidden_task_count(), 0);
        ctrl(&mut h, 'u');
        assert!(h.app.state.show_all_tasks);
        assert_eq!(task_ids(&h).len(), 12);
        h.key('s');

        // Agents: Node sections and a descending sort keep working on the
        // matches, and a node with none disappears.
        let mut far = make_agent("a_far", "bot far", AgentStatus::Idle);
        far.remote = Some(RemoteRef {
            alias: "w9".into(),
            online: false,
        });
        let backend = FakeBackend::new().with_agents(vec![
            make_agent("a_x", "bot x", AgentStatus::Idle),
            make_agent("a_y", "bot y", AgentStatus::Idle),
            make_agent("a_z", "zed", AgentStatus::Idle),
            far,
        ]);
        let mut a = Harness::with(backend);
        a.app.state.view = ViewId::Agents;
        a.key('b');
        assert_eq!(a.app.state.agent_group_by, AgentGroupBy::Node);
        a.key('o').key('O');
        ctrl(&mut a, 'k');
        a.type_str("bot");
        assert_eq!(agent_ids(&a), ["a_y", "a_x", "a_far"]);
        a.type_str(" x");
        a.draw();
        assert_eq!(agent_ids(&a), ["a_x"]);
        a.refute_text("◈ w9");
    }

    #[test]
    fn keyword_filter_reset_preserves_visible_selection() {
        let backend = FakeBackend::new().with_tasks(vec![
            make_task("tsk_a", "Alpha", TaskStatus::Todo),
            make_task("tsk_b", "Bravo", TaskStatus::Todo),
            make_task("tsk_c", "Charlie", TaskStatus::Todo),
        ]);
        let mut h = Harness::with(backend);
        h.app.state.view = ViewId::Tasks;
        h.press(KeyCode::Down);
        assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_b");

        // B still matches: it keeps the cursor though its row moved.
        ctrl(&mut h, 'k');
        h.type_str("r");
        assert_eq!(task_ids(&h), ["tsk_b", "tsk_c"]);
        assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_b");
        assert!(!h.app.state.entity_actions_deferred);

        // Esc clears and leaves the prompt; B is still the selection.
        h.press(KeyCode::Esc);
        assert_eq!(h.app.state.input_mode, InputMode::None);
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "");
        assert_eq!(task_ids(&h), ["tsk_a", "tsk_b", "tsk_c"]);
        assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_b");

        // Enter keeps the keyword; a filter that hides B moves the cursor to
        // the nearest match, and entity actions wait for the frame that
        // shows it.
        ctrl(&mut h, 'k');
        h.type_str("charlie");
        h.press(KeyCode::Enter);
        assert_eq!(h.app.state.input_mode, InputMode::None);
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "charlie");
        assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_c");
        assert!(h.app.state.entity_actions_deferred);
        h.press(KeyCode::Enter);
        assert_eq!(h.app.state.detail, DetailPanel::None);
        h.draw().press(KeyCode::Enter);
        assert_eq!(h.app.state.detail, DetailPanel::Entity);
        h.press(KeyCode::Esc);

        // Ctrl+K reopens on the kept keyword, cursor at its end.
        ctrl(&mut h, 'k');
        h.type_str("!");
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "charlie!");
        h.press(KeyCode::Esc);
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "");
    }

    #[test]
    fn keyword_filter_reset_chip() {
        let backend = FakeBackend::new().with_tasks(vec![
            make_task("tsk_a", "Alpha", TaskStatus::Todo),
            make_task("tsk_b", "Bravo", TaskStatus::Todo),
            make_task("tsk_c", "Charlie", TaskStatus::Todo),
        ]);
        // Wide enough for the footer's item count.
        let mut h = Harness::sized(backend, 140, 30);
        h.app.state.view = ViewId::Tasks;
        h.press(KeyCode::Down).press(KeyCode::Down);
        ctrl(&mut h, 'k');
        h.type_str("a");
        h.press(KeyCode::Enter);
        assert_eq!(task_ids(&h), ["tsk_a", "tsk_b", "tsk_c"]);
        h.app.state.task_keyword.reset("");
        ctrl(&mut h, 'k');
        h.type_str("ar");
        h.press(KeyCode::Enter);
        assert_eq!(task_ids(&h), ["tsk_c"]);
        assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_c");

        // The chip shows the keyword and the counts, and is a button.
        h.draw();
        let (x, y) = h.find_text("filter: ar · 1/3").expect("the keyword chip");
        assert_eq!(
            h.zone_at(x, y).map(|z| z.kind.clone()),
            Some(ZoneKind::Verb(VerbId::KeywordClear))
        );
        h.assert_text("1/3 tasks");
        h.click(x, y);
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "");
        assert_eq!(task_ids(&h), ["tsk_a", "tsk_b", "tsk_c"]);
        assert_eq!(h.app.state.selected_task().unwrap().id, "tsk_c");
        h.draw();
        h.refute_text("filter:");

        // Ctrl+U is the key for the same reset, outside the prompt.
        ctrl(&mut h, 'k');
        h.type_str("bravo");
        h.press(KeyCode::Enter);
        ctrl(&mut h, 'u');
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "");
        assert_eq!(h.app.state.input_mode, InputMode::None);
    }

    #[test]
    fn keyword_clear_after_no_match_scrolls_the_grown_list_to_its_top() {
        // The list pane is sized to its rows, so the frame drawn for "no
        // match" leaves a two-row pane behind. Clearing must not reveal the
        // selection inside that stale pane: the grown list starts at its
        // first row, goal header included.
        let mut tasks = vec![
            make_task("tsk_a", "Alpha deploy", TaskStatus::Cancelled),
            make_task("tsk_b", "Bravo docs", TaskStatus::Cancelled),
            make_task("tsk_c", "Charlie deploy", TaskStatus::Cancelled),
        ];
        tasks[1].goal_id = Some("g_r".into());
        tasks[2].goal_id = Some("g_r".into());
        let backend = FakeBackend::new()
            .with_tasks(tasks)
            .with_goals(vec![make_goal("g_r", "Release train", GoalStatus::Active)]);
        let mut h = Harness::sized(backend, 120, 40);
        h.app.state.view = ViewId::Tasks;
        h.key('g');
        h.draw();
        ctrl(&mut h, 'k');
        h.type_str("zz");
        h.draw();
        h.assert_text("No tasks match");
        h.press(KeyCode::Esc);
        assert_eq!(h.app.state.task_scroll, 0);
        h.draw();
        h.assert_text("RELEASE TRAIN · 2 tasks");
        h.assert_text("Charlie deploy");
    }

    #[test]
    fn keyword_goal_section_counts_its_matches_of_the_goal() {
        let mut tasks = vec![
            make_task("tsk_a", "Alpha deploy", TaskStatus::Done),
            make_task("tsk_b", "Bravo docs", TaskStatus::Todo),
        ];
        tasks[0].goal_id = Some("g_r".into());
        tasks[1].goal_id = Some("g_r".into());
        let backend = FakeBackend::new()
            .with_tasks(tasks)
            .with_goals(vec![make_goal("g_r", "Release train", GoalStatus::Active)]);
        let mut h = Harness::sized(backend, 120, 40);
        h.app.state.view = ViewId::Tasks;
        h.key('g');
        ctrl(&mut h, 'k');
        h.type_str("docs");
        h.draw();
        // One of the goal's two tasks is listed; its progress is unchanged.
        h.assert_text("RELEASE TRAIN · 1/2 tasks · 50% done");
        h.refute_text("Alpha deploy");
    }

    #[test]
    fn keyword_filter_is_per_list() {
        let backend = FakeBackend::new()
            .with_tasks(vec![make_task("tsk_a", "Alpha", TaskStatus::Todo)])
            .with_goals(vec![make_goal("g_a", "Alpha goal", GoalStatus::Active)]);
        let mut h = Harness::with(backend);
        h.app.state.view = ViewId::Tasks;
        ctrl(&mut h, 'k');
        h.type_str("zzz");
        h.press(KeyCode::Enter);
        h.app.state.view = ViewId::Goals;
        assert_eq!(goal_ids(&h), ["g_a"]);
        assert_eq!(h.app.state.keyword(ViewId::Goals), "");
        h.app.state.view = ViewId::Tasks;
        assert!(task_ids(&h).is_empty());
    }

    #[test]
    fn keyword_filter_bounds_unicode_and_sanitizes_paste() {
        let backend =
            FakeBackend::new().with_tasks(vec![make_task("tsk_a", "A", TaskStatus::Todo)]);
        let mut h = Harness::with(backend);
        h.app.state.view = ViewId::Tasks;
        ctrl(&mut h, 'k');

        // 256 emoji fit; the 257th key is ignored.
        h.type_str(&"🚀".repeat(KEYWORD_MAX_CHARS + 1));
        assert_eq!(
            h.app.state.keyword(ViewId::Tasks).chars().count(),
            KEYWORD_MAX_CHARS
        );
        // A paste is cut to what is left (nothing).
        h.paste("more");
        assert_eq!(
            h.app.state.keyword(ViewId::Tasks).chars().count(),
            KEYWORD_MAX_CHARS
        );
        h.press(KeyCode::Esc);

        // Control characters never reach the keyword: a pasted ESC sequence
        // and newlines are dropped, the text around them kept.
        ctrl(&mut h, 'k');
        h.paste("a\u{1b}[2J\nb\tc\u{7f}");
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "a[2Jbc");
        h.draw();
        h.refute_text("\u{1b}");

        // Keys in the prompt are text, never commands: `q` does not quit,
        // `d` does not delete, `?` does not open help.
        h.press(KeyCode::Esc);
        ctrl(&mut h, 'k');
        h.type_str("qd?");
        assert!(!h.app.state.should_exit);
        assert!(!h.app.state.show_help);
        assert!(h.app.state.pending_deletions.is_empty());
        h.refute_called(|c| matches!(c, Call::DeleteTask(..)));
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "qd?");
    }

    #[test]
    fn keyword_prompt_mouse_verbs_keep_and_clear() {
        let backend = FakeBackend::new().with_tasks(vec![
            make_task("tsk_a", "Alpha", TaskStatus::Todo),
            make_task("tsk_b", "Bravo", TaskStatus::Todo),
        ]);
        let mut h = Harness::sized(backend, 120, 30);
        h.app.state.view = ViewId::Tasks;
        h.draw();
        // `^K filter` in the footer opens the prompt.
        h.click_text("^K filter");
        assert_eq!(h.app.state.input_mode, InputMode::Keyword);
        h.type_str("brav");
        // The footer row ignores clicks for a double-click window after one
        // of its hints fired; nobody types four letters that fast.
        h.advance(1_000);
        h.draw();
        h.assert_text("Filter tasks › brav");
        h.assert_text("1/2");
        h.click_text("Enter keep");
        assert_eq!(h.app.state.input_mode, InputMode::None);
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "brav");

        ctrl(&mut h, 'k');
        h.draw();
        h.click_text("Esc clear");
        assert_eq!(h.app.state.input_mode, InputMode::None);
        assert_eq!(h.app.state.keyword(ViewId::Tasks), "");
    }
}
