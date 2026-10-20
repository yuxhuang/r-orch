//! Edge cases of stack Y that the per-feature suites leave open: the keyword
//! bound and what may be typed into it, a keyword that matches nothing,
//! selection through filter, sort, grouping and a refresh, header clicks at
//! the edges of the screen, tie order, the `Elapsed*` clamps on screen, and
//! By node grouping at its boundaries (R3, R4, R9, R10, R11, R15).
//!
//! What the other suites already pin is not repeated here: unknown values
//! sorting last in either direction (`qol2_timing.rs`), the post-terminal
//! edit and future-start vectors at the backend (`tui_backend.rs`), the 256
//! emoji bound and a pasted ESC sequence (`filters.rs`), and the only-local
//! and one-offline-node By node cases (`agent_sort.rs`).
//!
//! Every case holds the clock still or moves it by an exact amount, and runs
//! on the fake backend.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, RemoteRef, Run, RunStatus, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_goal, make_task, make_team};
use orch_tui::hit::ZoneKind;
use orch_tui::state::{
    AgentGroupBy, AgentRow, AgentSortField, GoalSortField, InputMode, KEYWORD_MAX_CHARS,
    SortColumn, TaskSortField, ViewId,
};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

/// The instant [`Harness`] starts its clock at.
const BASE: i64 = 1_000_000;

fn ctrl(h: &mut Harness, ch: char) {
    h.press_mod(KeyCode::Char(ch), KeyModifiers::CONTROL);
}

fn iso(ms: i64) -> String {
    assert!((0..86_400_000).contains(&ms), "{ms} is not on day one");
    format!(
        "1970-01-01T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

fn run(task_id: &str, started_ms: i64) -> Run {
    Run {
        id: format!("run_{task_id}_{started_ms}"),
        task_id: task_id.into(),
        agent_id: "agt".into(),
        attempt: 1,
        status: RunStatus::Succeeded,
        started_at: iso(started_ms),
        finished_at: None,
        workspace_path: "/tmp/ws".into(),
        prompt: "p".into(),
        pid: None,
        error: None,
        error_kind: None,
        tokens: None,
    }
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

fn selected_task(h: &Harness) -> String {
    h.app
        .state
        .selected_task()
        .expect("a task is selected")
        .id
        .clone()
}

fn selected_agent(h: &Harness) -> String {
    h.app
        .state
        .selected_agent()
        .expect("an agent is selected")
        .id
        .clone()
}

/// The screen row holding `needle`.
#[track_caller]
fn row<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{screen}"))
}

/// The sort columns that have a header zone on screen row `y`, left to right.
fn header_columns(h: &Harness, y: u16) -> Vec<SortColumn> {
    let mut columns = Vec::new();
    for x in 0..h.app.state.width {
        if let Some(ZoneKind::SortHeader(column)) = h.zone_at(x, y).map(|z| z.kind.clone())
            && columns.last() != Some(&column)
        {
            columns.push(column);
        }
    }
    columns
}

// ── The keyword's bound (D1: 256 scalar values) ────────────────────────────

fn tasks_view() -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![make_task(
        "tsk_a",
        "Alpha",
        TaskStatus::Todo,
    )]));
    h.app.state.view = ViewId::Tasks;
    ctrl(&mut h, 'k');
    h
}

#[test]
fn a_pasted_keyword_of_exactly_256_chars_is_kept_whole() {
    let mut h = tasks_view();
    h.paste(&"a".repeat(KEYWORD_MAX_CHARS));
    assert_eq!(h.app.state.keyword(ViewId::Tasks), "a".repeat(256));
}

#[test]
fn a_pasted_keyword_of_257_chars_is_cut_to_its_first_256() {
    let mut h = tasks_view();
    h.paste(&format!("{}b", "a".repeat(KEYWORD_MAX_CHARS)));
    assert_eq!(h.app.state.keyword(ViewId::Tasks), "a".repeat(256));
}

#[test]
fn typing_after_a_255_char_paste_takes_one_more_char_and_ignores_the_next() {
    let mut h = tasks_view();
    h.paste(&"a".repeat(KEYWORD_MAX_CHARS - 1));
    h.type_str("xy");
    assert_eq!(
        h.app.state.keyword(ViewId::Tasks),
        format!("{}x", "a".repeat(255))
    );
}

#[test]
fn a_second_paste_into_a_full_keyword_changes_nothing() {
    let mut h = tasks_view();
    h.paste(&"a".repeat(KEYWORD_MAX_CHARS));
    h.paste("b");
    assert_eq!(h.app.state.keyword(ViewId::Tasks), "a".repeat(256));
}

#[test]
fn a_256_char_keyword_still_filters_to_the_titles_that_contain_it() {
    let long = "a".repeat(KEYWORD_MAX_CHARS);
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![
        make_task("tsk_long", &format!("{long} tail"), TaskStatus::Todo),
        make_task("tsk_short", "aaa", TaskStatus::Todo),
    ]));
    h.app.state.view = ViewId::Tasks;
    ctrl(&mut h, 'k');
    h.paste(&long);
    assert_eq!(task_ids(&h), ["tsk_long"]);
}

// ── What may be typed into the prompt ──────────────────────────────────────

#[test]
fn typed_control_chars_never_enter_the_keyword() {
    let mut h = tasks_view();
    // ESC, C1 CSI, BEL, NUL, DEL, tab and newline arrive as char events on a
    // terminal that reports them so; none is text.
    for ch in ['\u{1b}', '\u{9b}', '\u{7}', '\0', '\u{7f}', '\t', '\n'] {
        h.press(KeyCode::Char(ch));
    }
    h.type_str("ok");
    assert_eq!(h.app.state.keyword(ViewId::Tasks), "ok");
}

#[test]
fn a_typed_ansi_sequence_leaves_only_its_printable_tail_in_the_keyword() {
    let mut h = tasks_view();
    h.press(KeyCode::Char('\u{1b}'));
    h.type_str("[31mred");
    assert_eq!(h.app.state.keyword(ViewId::Tasks), "[31mred");
}

#[test]
fn a_paste_of_only_control_chars_leaves_the_keyword_empty_and_the_prompt_open() {
    let mut h = tasks_view();
    h.paste("\u{1b}\u{1b}\n\t\r\n");
    assert_eq!(
        (h.app.state.keyword(ViewId::Tasks), h.app.state.input_mode),
        ("", InputMode::Keyword)
    );
}

// ── A keyword that matches nothing ─────────────────────────────────────────

/// Three of each entity, the view open on `view` and `zz` typed.
fn no_match(view: ViewId) -> Harness {
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(
                ["tsk_a", "tsk_b", "tsk_c"]
                    .map(|id| make_task(id, id, TaskStatus::Todo))
                    .to_vec(),
            )
            .with_goals(
                ["g_a", "g_b", "g_c"]
                    .map(|id| make_goal(id, id, GoalStatus::Active))
                    .to_vec(),
            )
            .with_agents(
                ["a_a", "a_b", "a_c"]
                    .map(|id| make_agent(id, id, AgentStatus::Idle))
                    .to_vec(),
            ),
        120,
        30,
    );
    h.app.state.view = view;
    ctrl(&mut h, 'k');
    h.type_str("zz");
    h
}

#[test]
fn a_keyword_matching_no_task_keeps_its_chip_with_a_zero_count() {
    let mut h = no_match(ViewId::Tasks);
    h.assert_text("filter: zz · 0/3");
}

#[test]
fn a_keyword_matching_no_goal_keeps_its_chip_with_a_zero_count() {
    let mut h = no_match(ViewId::Goals);
    h.assert_text("filter: zz · 0/3");
}

#[test]
fn a_keyword_matching_no_agent_keeps_its_chip_with_a_zero_count() {
    let mut h = no_match(ViewId::Agents);
    h.assert_text("filter: zz · 0/3");
}

#[test]
fn a_keyword_matching_no_task_lists_no_task() {
    let h = no_match(ViewId::Tasks);
    assert!(task_ids(&h).is_empty());
}

#[test]
fn a_keyword_matching_no_goal_lists_no_goal() {
    let h = no_match(ViewId::Goals);
    assert!(goal_ids(&h).is_empty());
}

#[test]
fn a_keyword_matching_no_agent_lists_no_agent() {
    let h = no_match(ViewId::Agents);
    assert!(agent_ids(&h).is_empty());
}

#[test]
fn a_keyword_matching_no_goal_draws_the_empty_table_with_its_header() {
    let mut h = no_match(ViewId::Goals);
    let screen = h.text();
    assert!(
        screen.contains("No goals match “zz”") && screen.contains("STATUS"),
        "{screen}"
    );
}

#[test]
fn enter_on_a_no_match_list_opens_no_detail() {
    let mut h = no_match(ViewId::Tasks);
    h.press(KeyCode::Enter);
    h.draw().press(KeyCode::Enter);
    assert_eq!(h.app.state.detail, orch_tui::state::DetailPanel::None);
}

#[test]
fn no_match_with_sort_and_grouping_on_draws_every_view_without_panicking() {
    // `g` groups tasks by goal and `b` groups agents by node; in the other
    // views they would switch view, so each list gets its own keys.
    for (view, group) in [
        (ViewId::Tasks, Some('g')),
        (ViewId::Goals, None),
        (ViewId::Agents, Some('b')),
    ] {
        let mut h = no_match(view);
        h.press(KeyCode::Enter);
        h.key('o').key('O');
        if let Some(group) = group {
            h.key(group);
        }
        assert_eq!(h.app.state.view, view);
        let screen = h.text();
        assert!(screen.contains("filter: zz · 0/3"), "{view:?}\n{screen}");
    }
}

// ── Selection through filter, sort, grouping and refresh ───────────────────

fn chain_tasks() -> Harness {
    let mut tasks = vec![
        make_task("tsk_a", "Alpha-x", TaskStatus::Todo),
        make_task("tsk_b", "Bravo-x", TaskStatus::Todo),
        make_task("tsk_c", "Charlie-x", TaskStatus::Todo),
        make_task("tsk_d", "Delta", TaskStatus::Todo),
        make_task("tsk_e", "Echo-x", TaskStatus::Todo),
    ];
    tasks[0].goal_id = Some("g_1".into());
    tasks[1].goal_id = Some("g_2".into());
    tasks[2].goal_id = Some("g_1".into());
    let mut h = Harness::with(FakeBackend::new().with_tasks(tasks).with_goals(vec![
        make_goal("g_1", "One", GoalStatus::Active),
        make_goal("g_2", "Two", GoalStatus::Active),
    ]));
    h.app.state.view = ViewId::Tasks;
    h.app.state.task_selected = 2;
    assert_eq!(selected_task(&h), "tsk_c");
    h
}

#[test]
fn a_selected_task_stays_selected_through_keyword_then_sort_then_grouping() {
    let mut h = chain_tasks();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    assert_eq!(task_ids(&h), ["tsk_a", "tsk_b", "tsk_c", "tsk_e"]);
    assert_eq!(selected_task(&h), "tsk_c", "after the keyword");

    // Status → Priority → Title, then descending.
    h.key('o').key('o').key('O');
    assert_eq!(task_ids(&h), ["tsk_e", "tsk_c", "tsk_b", "tsk_a"]);
    assert_eq!(selected_task(&h), "tsk_c", "after the sort");

    h.key('g');
    assert_eq!(task_ids(&h), ["tsk_c", "tsk_a", "tsk_b", "tsk_e"]);
    assert_eq!(selected_task(&h), "tsk_c", "after grouping by goal");
}

#[test]
fn a_selected_task_stays_selected_when_the_keyword_is_cleared_after_sort_and_grouping() {
    let mut h = chain_tasks();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    h.key('o').key('o').key('O').key('g');
    ctrl(&mut h, 'u');
    assert_eq!(selected_task(&h), "tsk_c");
}

fn chain_agents() -> Harness {
    let remote = |alias: &str, online: bool| {
        Some(RemoteRef {
            alias: alias.into(),
            online,
        })
    };
    let mut agents = vec![
        make_agent("b", "Bo-x", AgentStatus::Idle),
        make_agent("al", "Al-x", AgentStatus::Idle),
        make_agent("a1", "Ann-x", AgentStatus::Idle),
        make_agent("a2", "Abe-x", AgentStatus::Idle),
        make_agent("z", "Zed", AgentStatus::Idle),
    ];
    agents[2].remote = remote("remote-a", true);
    agents[3].remote = remote("remote-a", true);
    agents[4].remote = remote("remote-z", false);
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    h.app.state.agent_selected = agent_ids(&h)
        .iter()
        .position(|id| id == "a1")
        .expect("a1 is listed");
    assert_eq!(selected_agent(&h), "a1");
    h
}

#[test]
fn a_selected_agent_stays_selected_through_keyword_then_sort_then_node_grouping() {
    let mut h = chain_agents();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    assert!(!agent_ids(&h).contains(&"z".to_string()));
    assert_eq!(selected_agent(&h), "a1", "after the keyword");

    // Original → Name, descending.
    h.key('o').key('O');
    assert_eq!(h.app.state.agent_sort.field, AgentSortField::Name);
    assert_eq!(selected_agent(&h), "a1", "after the sort");

    h.key('b');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Node);
    assert_eq!(agent_ids(&h), ["b", "al", "a1", "a2"]);
    assert_eq!(selected_agent(&h), "a1", "after grouping by node");
}

#[test]
fn a_selected_goal_stays_selected_through_keyword_and_sort() {
    let mut h = Harness::with(FakeBackend::new().with_goals(vec![
        make_goal("g_a", "Alpha-x", GoalStatus::Active),
        make_goal("g_b", "Bravo", GoalStatus::Active),
        make_goal("g_c", "Charlie-x", GoalStatus::Active),
    ]));
    h.app.state.view = ViewId::Goals;
    h.app.state.goal_selected = 2;
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    // Status → Title, descending: Charlie leads.
    h.key('o').key('O');
    assert_eq!(h.app.state.goal_sort.field, GoalSortField::Title);
    assert_eq!(goal_ids(&h), ["g_c", "g_a"]);
    assert_eq!(h.app.state.selected_goal().unwrap().id, "g_c");
}

#[test]
fn a_refresh_that_removes_the_selected_filtered_task_selects_a_remaining_match() {
    let mut h = chain_tasks();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    h.app.backend.tasks.retain(|t| t.id != "tsk_c");
    h.app.refresh_all(false);
    assert_eq!(
        selected_task(&h),
        "tsk_e",
        "the nearest row of the filtered list"
    );
}

#[test]
fn a_refresh_that_removes_the_selected_task_defers_entity_actions_until_it_is_drawn() {
    let mut h = chain_tasks();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    h.draw();
    assert!(!h.app.state.entity_actions_deferred);
    h.app.backend.tasks.retain(|t| t.id != "tsk_c");
    h.app.refresh_all(false);
    assert!(h.app.state.entity_actions_deferred);
}

#[test]
fn a_refresh_that_removes_the_last_matching_task_leaves_the_keyword_and_an_empty_list() {
    let mut h = chain_tasks();
    ctrl(&mut h, 'k');
    h.type_str("delta");
    h.press(KeyCode::Enter);
    h.app.backend.tasks.retain(|t| t.id != "tsk_d");
    h.app.refresh_all(false);
    assert_eq!(
        (h.app.state.keyword(ViewId::Tasks), task_ids(&h).len()),
        ("delta", 0)
    );
}

#[test]
fn a_refresh_that_removes_the_selected_agent_under_node_grouping_selects_a_neighbour() {
    let mut h = chain_agents();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    h.key('b');
    assert_eq!(
        agent_ids(&h),
        ["b", "al", "a1", "a2"],
        "Local, then remote-a"
    );
    h.app.backend.agents.retain(|a| a.id != "a1");
    h.app.refresh_all(true);
    assert_eq!(selected_agent(&h), "a2");
}

#[test]
fn a_refresh_that_keeps_the_selected_task_leaves_it_selected_when_others_vanish() {
    let mut h = chain_tasks();
    ctrl(&mut h, 'k');
    h.type_str("-x");
    h.press(KeyCode::Enter);
    h.app
        .backend
        .tasks
        .retain(|t| t.id != "tsk_a" && t.id != "tsk_b");
    h.app.refresh_all(false);
    assert_eq!(selected_task(&h), "tsk_c");
}

// ── Header clicks at the edges (R10) ───────────────────────────────────────

fn tasks_at(width: u16) -> Harness {
    let mut h = Harness::sized(
        FakeBackend::new().with_tasks(vec![
            make_task("tsk_a", "Alpha", TaskStatus::Todo),
            make_task("tsk_b", "Bravo", TaskStatus::InProgress),
        ]),
        width,
        14,
    );
    h.app.state.view = ViewId::Tasks;
    h
}

#[test]
fn a_click_on_the_last_cell_of_the_last_header_at_40_cols_sorts_that_column() {
    let mut h = tasks_at(40);
    let (x, y) = h.find_text("ELAPSED*").expect("the Elapsed* header");
    h.click(39, y);
    assert_eq!(
        (x + 8, h.app.state.task_sort.field),
        (40, TaskSortField::Elapsed),
        "the label ends on the terminal's last cell"
    );
}

#[test]
fn a_click_on_the_last_cell_of_the_last_header_at_120_cols_sorts_that_column() {
    let mut h = tasks_at(120);
    let (_, y) = h.find_text("ELAPSED*").expect("the Elapsed* header");
    h.click(119, y);
    assert_eq!(h.app.state.task_sort.field, TaskSortField::Elapsed);
}

#[test]
fn a_click_on_the_last_goals_header_at_40_cols_sorts_goals_by_elapsed() {
    let mut h = Harness::sized(
        FakeBackend::new().with_goals(vec![make_goal("g_a", "Alpha", GoalStatus::Active)]),
        40,
        14,
    );
    h.app.state.view = ViewId::Goals;
    let (_, y) = h.find_text("ELAPSED*").expect("the Elapsed* header");
    h.click(39, y);
    assert_eq!(h.app.state.goal_sort.field, GoalSortField::Elapsed);
}

fn agents_at_40_cols() -> Harness {
    let mut h = Harness::sized(
        FakeBackend::new().with_agents(vec![
            make_agent("z", "Zed", AgentStatus::Idle),
            make_agent("a", "Al", AgentStatus::Idle),
        ]),
        40,
        14,
    );
    h.app.state.view = ViewId::Agents;
    h
}

#[test]
fn a_click_on_the_name_header_at_40_cols_sorts_agents_by_name() {
    let mut h = agents_at_40_cols();
    h.click_text("NAME");
    assert_eq!(h.app.state.agent_sort.field, AgentSortField::Name);
}

#[test]
fn a_click_on_the_last_agents_header_at_40_cols_sorts_by_runs() {
    // RUNS is the last cell of the row; every Agents column sorts now, and
    // its zone reaches the last cell.
    let mut h = agents_at_40_cols();
    let (_, y) = h.find_text("RUNS").expect("the RUNS header");
    h.click(39, y);
    assert_eq!(h.app.state.agent_sort.field, AgentSortField::Runs);
}

#[test]
fn the_40_col_tasks_header_offers_only_the_columns_that_fit() {
    let mut h = tasks_at(40);
    let (_, y) = h.find_text("ELAPSED*").expect("the Elapsed* header");
    assert_eq!(
        header_columns(&h, y),
        [
            SortColumn::Task(TaskSortField::Status),
            SortColumn::Task(TaskSortField::Priority),
            SortColumn::Task(TaskSortField::Title),
            SortColumn::Task(TaskSortField::Elapsed),
        ],
        "no zone for a column that is not drawn"
    );
}

#[test]
fn a_click_on_the_last_header_moves_the_rows() {
    // tsk_a ran for 5 s, tsk_b for 1 s: source order is the reverse of the
    // ascending Elapsed order.
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![
                make_task("tsk_a", "Alpha", TaskStatus::InProgress),
                make_task("tsk_b", "Bravo", TaskStatus::InProgress),
            ])
            .with_runs(vec![run("tsk_a", BASE - 5_000), run("tsk_b", BASE - 1_000)]),
        40,
        14,
    );
    h.app.state.view = ViewId::Tasks;
    h.key('o'); // Make sure the click below is the first thing to reorder.
    h.key('0');
    assert_eq!(task_ids(&h), ["tsk_a", "tsk_b"]);
    let (_, y) = h.find_text("ELAPSED*").expect("the Elapsed* header");
    h.click(39, y);
    assert_eq!(task_ids(&h), ["tsk_b", "tsk_a"]);
}

#[test]
fn header_zones_follow_their_columns_after_a_resize() {
    let mut h = tasks_at(100);
    let (wide_x, y) = h.find_text("ASSIGNEE").expect("ASSIGNEE at 100 cols");
    h.app.state.width = 60;
    h.terminal.backend_mut().resize(60, 14);
    h.draw();
    let (narrow_x, narrow_y) = h.find_text("ASSIGNEE").expect("ASSIGNEE at 60 cols");
    assert_ne!(wide_x, narrow_x, "the column moved");
    assert_eq!(
        h.zone_at(narrow_x, narrow_y).map(|z| z.kind.clone()),
        Some(ZoneKind::SortHeader(SortColumn::Task(
            TaskSortField::Assignee
        ))),
        "its zone moved with it (header row {y} before)"
    );
}

#[test]
fn a_click_where_a_header_was_before_a_resize_sorts_the_column_drawn_there_now() {
    // At 100 cols ASSIGNEE starts at (67, 7); at 70 cols that cell is under
    // ELAPSED*. A zone left over from the old layout would sort Assignee.
    let mut h = tasks_at(100);
    let (old_x, y) = h.find_text("ASSIGNEE").expect("ASSIGNEE at 100 cols");
    assert_eq!((old_x, y), (67, 7));
    h.app.state.width = 70;
    h.terminal.backend_mut().resize(70, 14);
    h.draw();
    h.click(old_x, y);
    assert_eq!(h.app.state.task_sort.field, TaskSortField::Elapsed);
}

// ── Ties keep source order (D2) ────────────────────────────────────────────

fn tied_tasks() -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![
        make_task("tsk_3", "Charlie", TaskStatus::Todo),
        make_task("tsk_1", "Alpha", TaskStatus::Todo),
        make_task("tsk_2", "Bravo", TaskStatus::Todo),
    ]));
    h.app.state.view = ViewId::Tasks;
    h.key('o'); // Status → Priority: every task is priority 3.
    assert_eq!(h.app.state.task_sort.field, TaskSortField::Priority);
    h
}

#[test]
fn tasks_tied_on_the_sort_field_keep_source_order_ascending() {
    let h = tied_tasks();
    assert_eq!(task_ids(&h), ["tsk_3", "tsk_1", "tsk_2"]);
}

#[test]
fn tasks_tied_on_the_sort_field_keep_source_order_descending_too() {
    let mut h = tied_tasks();
    h.key('O');
    assert_eq!(task_ids(&h), ["tsk_3", "tsk_1", "tsk_2"]);
}

#[test]
fn goals_tied_on_the_sort_field_keep_source_order_descending() {
    let mut h = Harness::with(FakeBackend::new().with_goals(vec![
        make_goal("g_3", "Charlie", GoalStatus::Active),
        make_goal("g_1", "Alpha", GoalStatus::Active),
        make_goal("g_2", "Bravo", GoalStatus::Active),
    ]));
    h.app.state.view = ViewId::Goals;
    h.key('O');
    assert_eq!(goal_ids(&h), ["g_3", "g_1", "g_2"]);
}

#[test]
fn tasks_with_the_same_title_in_different_case_keep_source_order() {
    let mut h = Harness::with(FakeBackend::new().with_tasks(vec![
        make_task("tsk_2", "deploy", TaskStatus::Todo),
        make_task("tsk_1", "DEPLOY", TaskStatus::Todo),
    ]));
    h.app.state.view = ViewId::Tasks;
    h.key('o').key('o');
    assert_eq!(h.app.state.task_sort.field, TaskSortField::Title);
    assert_eq!(task_ids(&h), ["tsk_2", "tsk_1"]);
}

// ── Started / Elapsed* on screen (R11, R15) ────────────────────────────────

#[test]
fn a_live_task_that_started_after_the_sample_has_zero_elapsed_not_a_negative_span() {
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![make_task("tsk_a", "Alpha", TaskStatus::InProgress)])
            .with_runs(vec![run("tsk_a", BASE + 5_000)]),
        120,
        30,
    );
    h.app.state.view = ViewId::Tasks;
    assert_eq!(h.app.state.task_elapsed_ms(0, BASE), Some(0));
}

#[test]
fn a_goal_created_after_now_shows_a_zero_elapsed_cell() {
    let mut goal = make_goal("g_a", "Alpha", GoalStatus::Active);
    goal.created_at = iso(BASE + 5_000);
    let mut h = Harness::sized(FakeBackend::new().with_goals(vec![goal]), 120, 30);
    h.app.state.view = ViewId::Goals;
    let screen = h.text();
    assert!(
        row(&screen, "Alpha").trim_end().ends_with(" 0s"),
        "{screen}"
    );
}

#[test]
fn a_future_start_ranks_as_zero_when_sorting_by_elapsed() {
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![
                make_task("tsk_old", "Old", TaskStatus::InProgress),
                make_task("tsk_future", "Future", TaskStatus::InProgress),
            ])
            .with_runs(vec![
                run("tsk_old", BASE - 9_000),
                run("tsk_future", BASE + 5_000),
            ]),
        120,
        30,
    );
    h.app.state.view = ViewId::Tasks;
    for _ in 0..6 {
        h.key('o');
    }
    assert_eq!(task_ids(&h), ["tsk_future", "tsk_old"]);
}

/// A terminal task that ran from `BASE-10s` to `BASE-4s` beside a live one
/// started 90 s ago.
fn frozen_beside_live() -> Harness {
    let mut done = make_task("tsk_done", "Frozen", TaskStatus::Done);
    done.updated_at = iso(BASE - 4_000);
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![
                done,
                make_task("tsk_live", "Ticking", TaskStatus::InProgress),
            ])
            .with_runs(vec![
                run("tsk_done", BASE - 10_000),
                run("tsk_live", BASE - 90_000),
            ]),
        120,
        30,
    );
    h.app.state.view = ViewId::Tasks;
    h
}

#[test]
fn a_terminal_task_elapsed_cell_does_not_move_when_the_clock_advances() {
    let mut h = frozen_beside_live();
    let before = h.text();
    h.advance(3_600_000);
    let after = h.text();
    assert_eq!(
        (
            row(&before, "Frozen").trim_end().rsplit(' ').next(),
            row(&after, "Frozen").trim_end().rsplit(' ').next()
        ),
        (Some("6s"), Some("6s")),
        "{after}"
    );
}

#[test]
fn a_live_task_elapsed_cell_does_move_when_the_clock_advances() {
    // The control for the case above: the same advance does move a live row.
    let mut h = frozen_beside_live();
    h.advance(3_600_000);
    let after = h.text();
    assert!(
        row(&after, "Ticking").trim_end().ends_with("1h01m"),
        "{after}"
    );
}

// ── By node grouping at its boundaries (R9) ────────────────────────────────

fn on_node(id: &str, alias: &str, online: bool) -> orch_domain::Agent {
    let mut agent = make_agent(id, id, AgentStatus::Idle);
    agent.remote = Some(RemoteRef {
        alias: alias.into(),
        online,
    });
    agent
}

fn node_view(agents: Vec<orch_domain::Agent>) -> Harness {
    let mut h = Harness::with(FakeBackend::new().with_agents(agents));
    h.app.state.view = ViewId::Agents;
    h.key('b');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Node);
    h
}

#[test]
fn by_node_with_no_agents_has_no_section_and_only_the_add_row() {
    let h = node_view(vec![]);
    assert_eq!(
        h.app.state.agent_rows(true),
        [AgentRow::AddAgent { selection: 0 }]
    );
}

#[test]
fn by_node_with_no_agents_counts_no_section_rows() {
    let h = node_view(vec![]);
    assert_eq!(h.app.state.agent_section_rows(), 0);
}

#[test]
fn by_node_with_no_agents_draws_without_panicking() {
    let mut h = node_view(vec![]);
    h.assert_text("+ add agent...");
}

#[test]
fn by_node_with_only_remote_agents_has_no_local_section() {
    let h = node_view(vec![on_node("r1", "w1", true), on_node("r2", "w2", true)]);
    let first = h.app.state.agent_rows(false)[0].clone();
    assert_eq!(
        first,
        AgentRow::NodeSection {
            alias: Some("w1".into()),
            online: true,
            count: 1
        }
    );
}

#[test]
fn a_node_whose_agents_are_all_offline_is_one_offline_section() {
    let h = node_view(vec![on_node("d1", "w1", false), on_node("d2", "w1", false)]);
    assert_eq!(
        h.app.state.agent_rows(false)[0],
        AgentRow::NodeSection {
            alias: Some("w1".into()),
            online: false,
            count: 2
        }
    );
}

#[test]
fn an_all_offline_node_does_not_sort_after_an_online_one() {
    // Sections follow alias order, never link state.
    let h = node_view(vec![on_node("u", "w9", true), on_node("d", "w1", false)]);
    assert_eq!(agent_ids(&h), ["d", "u"]);
}

#[test]
fn an_all_offline_node_draws_its_header_marked_offline() {
    let mut h = node_view(vec![on_node("d1", "w1", false), on_node("d2", "w1", false)]);
    h.assert_text("◈ w1 ○ offline · 2 agents");
}

#[test]
fn an_agent_of_an_all_offline_node_stays_selected_across_the_group_toggle() {
    let mut local = vec![
        make_agent("l1", "Local one", AgentStatus::Idle),
        on_node("d1", "w1", false),
    ];
    local.push(on_node("d2", "w1", false));
    let mut h = Harness::with(FakeBackend::new().with_agents(local));
    h.app.state.view = ViewId::Agents;
    h.app.state.agent_selected = agent_ids(&h).iter().position(|id| id == "d2").unwrap();
    h.key('b');
    assert_eq!(selected_agent(&h), "d2");
}

#[test]
fn by_node_counts_only_the_sections_a_keyword_leaves() {
    let mut h = node_view(vec![
        make_agent("l1", "Local one", AgentStatus::Idle),
        on_node("r1", "w1", true),
        on_node("r2", "w2", false),
    ]);
    ctrl(&mut h, 'k');
    h.type_str("w2");
    assert_eq!(h.app.state.agent_section_rows(), 1);
}

#[test]
fn by_node_with_a_keyword_draws_only_the_matching_node_header() {
    let mut h = node_view(vec![
        make_agent("l1", "Local one", AgentStatus::Idle),
        on_node("r2", "w2", false),
    ]);
    ctrl(&mut h, 'k');
    h.type_str("w2");
    let screen = h.text();
    assert!(
        screen.contains("◈ w2 ○ offline · 1 agent") && !screen.contains("LOCAL"),
        "{screen}"
    );
}

#[test]
fn by_team_still_groups_by_team_after_a_node_round_trip() {
    let mut h = Harness::with(
        FakeBackend::new()
            .with_agents(vec![
                make_agent("a", "Ann", AgentStatus::Idle),
                on_node("r", "w1", true),
            ])
            .with_teams(vec![make_team("t1", "Crew", "a", &["a"])]),
    );
    h.app.state.view = ViewId::Agents;
    h.key('b').key('B');
    assert_eq!(h.app.state.agent_group_by, AgentGroupBy::Team);
    let rows = h.app.state.agent_rows(false);
    assert!(
        matches!(rows[0], AgentRow::TeamSection { ref name, .. } if name == "Crew"),
        "{rows:?}"
    );
}
