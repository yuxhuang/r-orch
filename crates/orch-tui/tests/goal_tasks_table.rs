//! The goal detail's Tasks section is the Tasks view's table, filtered by the
//! goal (goal `goal_v2j1-i_`, requirements R1-R4).
//!
//! The Tasks view is the oracle. Where a case says "the same as the Tasks
//! view" it renders a second harness in `ViewId::Tasks` and compares cells,
//! so no private renderer is named here and the tests survive any choice of
//! seams (`TaskColumns::for_width`, `render_task_row`, `render_header_row`
//! being shared, or something else). Only the public API of `orch-tui` is
//! used.
//!
//! One deliberate difference, the approved default for gate question Q1: the
//! section drops the GOAL column (every row is the same goal). So the Tasks
//! view reference is built from the same tasks with their goal link cleared:
//! its GOAL cells render as blanks, and a section row must equal the
//! reference row cell for cell, with `GOAL` blanked in the header.
//!
//! Every case holds the clock still and runs on the fake backend.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, Run, RunStatus, Task, TaskStatus};
use orch_tui::backend::{CopyTarget, FakeBackend, make_agent, make_goal, make_task};
use orch_tui::hit::{ZoneId, ZoneKind};
use orch_tui::layout::MIN_WIDTH;
use orch_tui::messages::TASK_LIST_LIMIT;
use orch_tui::state::{
    EntityType, Focus, GoalSection, SortColumn, TaskSort, TaskSortField, ViewId,
};
use orch_tui::theme::CURSOR;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

/// The instant [`Harness`] starts its clock at.
const BASE: i64 = 1_000_000;

/// The goal under test; its tasks, in source order. `t_c` is running with a
/// run, `t_d` finished with a run, `t_a` carries an attachment and an
/// assignee, so every optional cell of the row format has content.
const LINKED: [&str; 4] = ["Delta", "Alpha", "Charlie", "Bravo"];

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

fn run(task_id: &str, started_ms: i64, finished_ms: Option<i64>) -> Run {
    Run {
        id: format!("run_{task_id}"),
        task_id: task_id.into(),
        agent_id: "agt".into(),
        attempt: 1,
        status: if finished_ms.is_some() {
            RunStatus::Succeeded
        } else {
            RunStatus::Running
        },
        started_at: iso(started_ms),
        finished_at: finished_ms.map(iso),
        workspace_path: "/tmp/ws".into(),
        prompt: "p".into(),
        pid: None,
        error: None,
        error_kind: None,
        tokens: None,
    }
}

fn task(id: &str, title: &str, status: TaskStatus, priority: i64, goal: Option<&str>) -> Task {
    let mut t = make_task(id, title, status);
    t.priority = priority;
    t.goal_id = goal.map(Into::into);
    t.description = format!("DESC-{}", title.to_uppercase());
    t
}

/// The four tasks of goal `g1`, in source order, linked or not.
fn goal_tasks(linked: bool) -> Vec<Task> {
    let g = linked.then_some("g1");
    let mut alpha = task("t_a", "Alpha", TaskStatus::Todo, 1, g);
    alpha.assignee = Some("agt".into());
    alpha.attachments = Some(vec!["notes.txt".into()]);
    let mut charlie = task("t_c", "Charlie", TaskStatus::InProgress, 2, g);
    charlie.assignee = Some("agt".into());
    vec![
        task("t_d", "Delta", TaskStatus::Done, 3, g),
        alpha,
        charlie,
        task("t_b", "Bravo", TaskStatus::Failed, 4, g),
    ]
}

fn backend(linked: bool) -> FakeBackend {
    let mut tasks = goal_tasks(linked);
    let mut goals = Vec::new();
    if linked {
        // Not in the goal: another goal's task and an ungrouped one.
        tasks.push(task("t_f", "Foreign", TaskStatus::Todo, 3, Some("g2")));
        tasks.push(task("t_l", "Loose", TaskStatus::Todo, 3, None));
        goals = vec![
            make_goal("g1", "Goal one", GoalStatus::Active),
            make_goal("g2", "Goal two", GoalStatus::Active),
        ];
    }
    FakeBackend::new()
        .with_tasks(tasks)
        .with_goals(goals)
        .with_agents(vec![make_agent("agt", "worker", AgentStatus::Idle)])
        .with_runs(vec![
            run("t_c", BASE - 5_000, None),
            run("t_d", BASE - 90_000, Some(BASE - 30_000)),
        ])
}

/// Goals view, goal `g1` open, the Tasks section focused.
fn section_with(backend: FakeBackend, width: u16, height: u16) -> Harness {
    let mut h = Harness::sized(backend, width, height);
    h.app.state.view = ViewId::Goals;
    h.press(KeyCode::Enter);
    h.app.state.set_focus(Focus::GoalDetail(GoalSection::Tasks));
    h
}

fn section(width: u16, height: u16) -> Harness {
    let h = section_with(backend(true), width, height);
    assert_eq!(
        h.app.state.selected_goal().map(|g| g.id.as_str()),
        Some("g1"),
        "the fixture opens goal g1"
    );
    h
}

/// The Tasks view over the same tasks with the goal link cleared: the
/// reference a section row must equal (see the module docs).
fn tasks_view(width: u16, height: u16) -> Harness {
    let mut h = Harness::sized(backend(false), width, height);
    h.app.state.view = ViewId::Tasks;
    h
}

fn ctrl(h: &mut Harness, ch: char) {
    h.press_mod(KeyCode::Char(ch), KeyModifiers::CONTROL);
}

/// The screen row of the table header: the last row holding `STATUS`, `PRI`
/// and `TITLE` (the Goals list header above it has no `PRI`).
#[track_caller]
fn header_y(h: &mut Harness) -> u16 {
    let text = h.text();
    let lines: Vec<&str> = text.lines().collect();
    let y = lines
        .iter()
        .rposition(|l| ["STATUS", "PRI", "TITLE"].iter().all(|c| l.contains(c)))
        .unwrap_or_else(|| panic!("no table header on screen:\n{text}"));
    y as u16
}

/// Cells of one drawn row for exact comparison: symbol, foreground and
/// modifiers, and background. A blank cell shows no foreground or modifier,
/// so only its background counts (a column's padding may be styled with the
/// column or left plain).
type Cell = (
    String,
    Option<ratatui::style::Color>,
    ratatui::style::Modifier,
    Option<ratatui::style::Color>,
);

fn cells(h: &Harness, y: u16) -> Vec<Cell> {
    let buffer = h.terminal.backend().buffer();
    (0..buffer.area.width)
        .map(|x| {
            let cell = &buffer[(x, y)];
            let style = cell.style();
            let blank = cell.symbol() == " ";
            (
                cell.symbol().to_string(),
                if blank { None } else { style.fg },
                if blank {
                    ratatui::style::Modifier::empty()
                } else {
                    style.add_modifier
                },
                style.bg,
            )
        })
        .collect()
}

/// The column of the first `needle` on row `y`, as a display column.
#[track_caller]
fn column_of(h: &Harness, y: u16, needle: &str) -> u16 {
    let line = h.line(y);
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is not on row {y}: {line:?}"));
    unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16
}

/// The goal's task titles in the order they are drawn: below the goal's title
/// bar in the goal detail, anywhere in the Tasks view.
fn drawn_titles(h: &mut Harness) -> Vec<String> {
    let text = h.text();
    let lines: Vec<&str> = text.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with("━━━ GOAL:"))
        .map_or(0, |bar| bar + 1);
    lines[from..]
        .iter()
        .filter_map(|l| LINKED.iter().find(|t| l.contains(**t)))
        .map(|t| (*t).to_string())
        .collect()
}

/// The order `AppState` gives the goal's tasks: the Tasks view's sort,
/// restricted to the goal.
fn state_order(h: &Harness) -> Vec<String> {
    let state = &h.app.state;
    state
        .sorted_tasks()
        .into_iter()
        .map(|i| &state.tasks[i])
        .filter(|t| t.goal_id.as_deref() == Some("g1"))
        .map(|t| t.title.clone())
        .collect()
}

fn tokens(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

fn sort_title(h: &mut Harness, descending: bool) {
    h.app.state.task_sort = TaskSort {
        field: TaskSortField::Title,
        descending,
    };
}

// ── R1: the same header and the same rows as the Tasks view ────────────────

#[test]
fn the_section_header_is_the_tasks_view_header_without_the_goal_column() {
    for width in [100, 140, 200] {
        let mut sec = section(width, 30);
        let mut reference = tasks_view(width, 30);
        let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
        assert_eq!(
            sec.line(sy).trim_end(),
            reference.line(ry).replacen("GOAL", "    ", 1).trim_end(),
            "width {width}"
        );
    }
}

#[test]
fn the_section_header_names_the_sortable_columns_of_the_tasks_view() {
    let mut sec = section(100, 30);
    let y = header_y(&mut sec);
    for label in ["STATUS", "PRI", "TITLE", "ASSIGNEE", "STARTED", "ELAPSED*"] {
        assert!(sec.line(y).contains(label), "{label}: {:?}", sec.line(y));
    }
    assert!(!sec.line(y).contains("GOAL"), "{:?}", sec.line(y));
}

#[test]
fn each_section_row_is_the_tasks_view_row_of_that_task() {
    for width in [100, 140, 200] {
        let mut sec = section(width, 30);
        let mut reference = tasks_view(width, 30);
        // Move the cursor onto a row in both, so the cursor cell is compared too.
        sec.press(KeyCode::Down);
        reference.press(KeyCode::Down);
        let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
        for i in 1..=LINKED.len() as u16 {
            assert_eq!(
                sec.line(sy + i).trim_end(),
                reference.line(ry + i).trim_end(),
                "row {i} at width {width}"
            );
        }
    }
}

#[test]
fn section_rows_carry_the_tasks_view_styles_for_status_priority_and_assignee() {
    let mut sec = section(100, 30);
    let mut reference = tasks_view(100, 30);
    sec.press(KeyCode::Down);
    reference.press(KeyCode::Down);
    let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
    for i in 1..=LINKED.len() as u16 {
        assert_eq!(cells(&sec, sy + i), cells(&reference, ry + i), "row {i}");
    }
}

#[test]
fn the_section_lists_exactly_the_tasks_of_its_goal() {
    let mut sec = section(100, 30);
    let mut titles = drawn_titles(&mut sec);
    titles.sort();
    assert_eq!(titles, ["Alpha", "Bravo", "Charlie", "Delta"]);
}

#[test]
fn a_task_of_another_goal_or_of_none_is_not_in_the_section() {
    let mut sec = section(100, 30);
    sec.refute_text("Foreign");
    sec.refute_text("Loose");
}

#[test]
fn the_section_rows_have_a_status_chip_and_priority_pips_not_the_old_plain_text() {
    let mut sec = section(100, 30);
    // `in_progress` is the old `status.as_str()` column, the chip says RUN.
    sec.refute_text("in_progress");
    let y = header_y(&mut sec);
    let rows: Vec<String> = (1..=4).map(|i| sec.line(y + i)).collect();
    assert!(rows.iter().any(|r| r.contains("RUN")), "{rows:#?}");
    assert!(rows.iter().any(|r| r.contains("DONE")), "{rows:#?}");
    assert!(rows.iter().any(|r| r.contains("!!!")), "{rows:#?}");
}

#[test]
fn the_section_drops_the_goal_column_the_tasks_view_has() {
    let mut sec = section(100, 30);
    sec.refute_text("⊕");
    let mut tasks = Harness::sized(backend(true), 100, 30);
    tasks.app.state.view = ViewId::Tasks;
    tasks.assert_text("⊕");
}

// ── R2: one sort for both surfaces ─────────────────────────────────────────

#[test]
fn o_in_the_section_cycles_the_task_sort_and_leaves_the_goal_sort() {
    let mut sec = section(100, 30);
    let (task_before, goal_before) = (sec.app.state.task_sort, sec.app.state.goal_sort);
    sec.key('o');
    assert_ne!(sec.app.state.task_sort, task_before, "task sort moved");
    assert_eq!(sec.app.state.goal_sort, goal_before, "goal sort untouched");
}

#[test]
fn o_in_the_section_draws_the_goals_tasks_in_the_tasks_view_order() {
    let mut sec = section(100, 30);
    sec.key('o');
    assert_eq!(state_order(&sec).len(), 4);
    assert_ne!(
        state_order(&sec),
        ["Delta", "Alpha", "Charlie", "Bravo"],
        "the fixture's next sort field must differ from source order"
    );
    assert_eq!(drawn_titles(&mut sec), state_order(&sec));
}

#[test]
fn capital_o_in_the_section_reverses_the_task_sort_and_the_drawn_order() {
    let mut sec = section(100, 30);
    let forward = state_order(&sec);
    sec.key('O');
    assert!(sec.app.state.task_sort.descending);
    let mut reversed = forward.clone();
    reversed.reverse();
    assert_eq!(drawn_titles(&mut sec), reversed);
}

#[test]
fn zero_in_the_section_restores_the_default_task_sort_and_order() {
    let mut sec = section(100, 30);
    let default_order = state_order(&sec);
    sort_title(&mut sec, true);
    sec.key('0');
    assert_eq!(sec.app.state.task_sort, TaskSort::default());
    assert_eq!(drawn_titles(&mut sec), default_order);
}

#[test]
fn a_click_on_the_title_header_sorts_the_tasks_by_title_then_reverses() {
    let mut sec = section(100, 30);
    let y = header_y(&mut sec);
    let x = column_of(&sec, y, "TITLE");
    assert_eq!(
        sec.zone_at(x, y).map(|z| z.kind.clone()),
        Some(ZoneKind::SortHeader(SortColumn::Task(TaskSortField::Title))),
        "the section header is clickable like the Tasks view's"
    );
    sec.click(x, y);
    assert_eq!(sec.app.state.task_sort.field, TaskSortField::Title);
    assert!(!sec.app.state.task_sort.descending);
    assert_eq!(
        drawn_titles(&mut sec),
        ["Alpha", "Bravo", "Charlie", "Delta"]
    );
    sec.click(x, y);
    assert!(sec.app.state.task_sort.descending);
    assert_eq!(
        drawn_titles(&mut sec),
        ["Delta", "Charlie", "Bravo", "Alpha"]
    );
}

#[test]
fn a_header_click_in_the_section_leaves_the_goal_sort_alone() {
    let mut sec = section(100, 30);
    let goal_before = sec.app.state.goal_sort;
    let y = header_y(&mut sec);
    let x = column_of(&sec, y, "TITLE");
    sec.click(x, y);
    assert_eq!(sec.app.state.goal_sort, goal_before);
}

#[test]
fn the_section_and_the_tasks_view_draw_the_goals_tasks_in_the_same_order_for_every_sort() {
    for field in [
        TaskSortField::Status,
        TaskSortField::Priority,
        TaskSortField::Title,
        TaskSortField::Assignee,
        TaskSortField::Started,
        TaskSortField::Elapsed,
    ] {
        for descending in [false, true] {
            let sort = TaskSort { field, descending };
            let mut sec = section(100, 30);
            sec.app.state.task_sort = sort;
            let mut reference = tasks_view(100, 30);
            reference.app.state.task_sort = sort;
            assert_eq!(
                drawn_titles(&mut sec),
                drawn_titles(&mut reference),
                "{field:?} descending={descending}"
            );
        }
    }
}

#[test]
fn a_sort_chosen_in_the_tasks_view_orders_the_section() {
    let mut tasks = Harness::sized(backend(true), 100, 30);
    tasks.app.state.view = ViewId::Tasks;
    tasks.key('o');
    let chosen = tasks.app.state.task_sort;
    tasks.app.state.view = ViewId::Goals;
    tasks.press(KeyCode::Enter);
    tasks
        .app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Tasks));
    assert_eq!(tasks.app.state.task_sort, chosen);
    assert_eq!(drawn_titles(&mut tasks), state_order(&tasks));
}

#[test]
fn o_with_another_section_focused_still_sorts_the_goals_not_the_tasks() {
    let mut sec = section(100, 30);
    sec.app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Description));
    let (task_before, goal_before) = (sec.app.state.task_sort, sec.app.state.goal_sort);
    sec.key('o');
    assert_eq!(sec.app.state.task_sort, task_before);
    assert_ne!(sec.app.state.goal_sort, goal_before);
}

#[test]
fn the_section_has_no_goal_rules_no_show_all_row_and_no_add_task_row() {
    let mut sec = section(100, 30);
    sec.app.state.group_by_goal = true;
    sec.refute_text("% done");
    sec.refute_text("UNGROUPED");
    sec.refute_text("Show all");
    sec.refute_text("add task");
}

#[test]
fn the_section_lists_every_linked_task_past_the_tasks_view_limit() {
    let count = TASK_LIST_LIMIT + 4;
    let tasks: Vec<Task> = (0..count)
        .map(|i| {
            task(
                &format!("t_{i:02}"),
                &format!("Many {i:02}"),
                TaskStatus::Todo,
                3,
                Some("g1"),
            )
        })
        .collect();
    let backend = FakeBackend::new()
        .with_tasks(tasks)
        .with_goals(vec![make_goal("g1", "Goal one", GoalStatus::Active)]);
    let mut sec = section_with(backend, 100, 70);
    let text = sec.text();
    for i in 0..count {
        assert!(text.contains(&format!("Many {i:02}")), "task {i}:\n{text}");
    }
    sec.refute_text("Show all");
}

// ── R3: selection, Enter, Ctrl+Y and the mouse apply to the table rows ─────

#[test]
fn enter_on_a_row_expands_that_tasks_description_in_the_drawn_order() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false);
    sec.draw().press(KeyCode::Down).press(KeyCode::Enter);
    let viewer = sec
        .app
        .state
        .viewer
        .as_ref()
        .expect("Enter opens the viewer");
    assert_eq!(&*viewer.document().body, "DESC-BRAVO");
}

#[test]
fn enter_on_the_first_row_expands_the_first_drawn_task() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false);
    sec.draw().press(KeyCode::Enter);
    let viewer = sec
        .app
        .state
        .viewer
        .as_ref()
        .expect("Enter opens the viewer");
    assert_eq!(&*viewer.document().body, "DESC-ALPHA");
}

#[test]
fn ctrl_y_copies_the_description_of_the_selected_row_in_the_drawn_order() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, true); // Delta, Charlie, Bravo, Alpha
    sec.draw().press(KeyCode::Down);
    ctrl(&mut sec, 'y');
    assert_eq!(
        sec.app.backend.copy_requests,
        vec![(CopyTarget::Auto, "DESC-CHARLIE".to_string())]
    );
    assert!(sec.app.state.viewer.is_none(), "copying opens no viewer");
}

#[test]
fn a_click_on_a_row_selects_it_without_opening_the_viewer() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false);
    let y = header_y(&mut sec) + 2; // the second drawn row: Bravo
    let x = column_of(&sec, y, "Bravo");
    sec.click(x, y);
    assert!(sec.app.state.viewer.is_none(), "one click only selects");
    sec.draw();
    assert!(
        sec.line(y).starts_with(CURSOR),
        "the cursor is on the clicked row: {:?}",
        sec.line(y)
    );
}

#[test]
fn enter_after_a_click_on_a_row_opens_the_clicked_task() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false);
    let y = header_y(&mut sec) + 2;
    let x = column_of(&sec, y, "Bravo");
    sec.click(x, y).press(KeyCode::Enter);
    let viewer = sec
        .app
        .state
        .viewer
        .as_ref()
        .expect("Enter opens the viewer");
    assert_eq!(&*viewer.document().body, "DESC-BRAVO");
}

#[test]
fn a_click_on_the_status_cell_of_a_row_selects_that_row_too() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false);
    let y = header_y(&mut sec) + 3; // Charlie
    sec.click(3, y).press(KeyCode::Enter);
    let viewer = sec
        .app
        .state
        .viewer
        .as_ref()
        .expect("Enter opens the viewer");
    assert_eq!(&*viewer.document().body, "DESC-CHARLIE");
}

#[test]
fn a_section_row_zone_is_identified_by_its_task_id() {
    // Plan R3: identity is the task id, so a refresh between press and
    // release cancels the click instead of moving it to the row that slid in.
    let mut sec = section(100, 30);
    sort_title(&mut sec, false);
    let y = header_y(&mut sec) + 2;
    let x = column_of(&sec, y, "Bravo");
    assert_eq!(
        sec.zone_at(x, y).map(|z| z.id.clone()),
        Some(ZoneId::Entity(EntityType::Task, "t_b".into()))
    );
}

#[test]
fn a_refresh_between_press_and_release_does_not_click_the_row_that_slid_in() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false); // Alpha, Bravo, Charlie, Delta
    let y = header_y(&mut sec) + 3; // Charlie
    let x = column_of(&sec, y, "Charlie");
    sec.press_at(x, y);
    // Alpha is renamed to sort last, so Delta slides into the pressed row.
    sec.app.state.tasks[1].title = "Zulu".into();
    sec.draw();
    sec.release_at(x, y);
    sec.press(KeyCode::Enter);
    let opened = sec
        .app
        .state
        .viewer
        .as_ref()
        .map(|v| v.document().body.to_string());
    assert_ne!(
        opened.as_deref(),
        Some("DESC-DELTA"),
        "the release landed on a different task than the press"
    );
}

#[test]
fn the_pane_wheel_still_scrolls_the_table_rows() {
    let tasks: Vec<Task> = (0..40)
        .map(|i| {
            task(
                &format!("t_{i:02}"),
                &format!("Many {i:02}"),
                TaskStatus::Todo,
                3,
                Some("g1"),
            )
        })
        .collect();
    let backend = FakeBackend::new()
        .with_tasks(tasks)
        .with_goals(vec![make_goal("g1", "Goal one", GoalStatus::Active)]);
    let mut sec = section_with(backend, 100, 30);
    sec.refute_text("Many 39");
    let y = header_y(&mut sec) + 2;
    for _ in 0..30 {
        sec.wheel_down(10, y);
    }
    sec.assert_text("Many 39");
}

// ── R4: narrow panes, the rail and the footer, the empty goal ──────────────

/// Six tasks of goal `g1` whose titles are `title` and whose assignee name
/// is long, with attachments: the widest content the row format can carry.
fn sweep_backend(title: &str) -> FakeBackend {
    let tasks: Vec<Task> = (0..6)
        .map(|i| {
            let mut t = task(
                &format!("t_{i}"),
                title,
                TaskStatus::InProgress,
                1,
                Some("g1"),
            );
            t.assignee = Some("agt".into());
            t.attachments = Some(vec!["a".into(), "b".into(), "c".into()]);
            t
        })
        .collect();
    FakeBackend::new()
        .with_tasks(tasks)
        .with_goals(vec![make_goal("g1", "Goal one", GoalStatus::Active)])
        .with_agents(vec![make_agent(
            "agt",
            &"wide-名前-agent".repeat(8),
            AgentStatus::Idle,
        )])
        .with_runs(vec![run("t_0", BASE - 5_000, None)])
}

#[test]
fn a_width_sweep_from_20_to_200_cells_draws_without_panic_and_keeps_the_rail_and_footer() {
    // A frame with enormous double-width titles and one with one-letter
    // titles differ only in the table body: the rail, the title bar and the
    // footer rows must be equal, so no row can have spilled into them.
    let huge = "宽字符😀 title ".repeat(40);
    for width in 20..=200u16 {
        let mut wide = section_with(sweep_backend(&huge), width, 30);
        let wide_text = wide.text();
        if width < MIN_WIDTH {
            assert!(wide_text.contains("terminal too small"), "width {width}");
            continue;
        }
        let mut tiny = section_with(sweep_backend("x"), width, 30);
        let tiny_text = tiny.text();
        let wide_lines: Vec<&str> = wide_text.lines().collect();
        let tiny_lines: Vec<&str> = tiny_text.lines().collect();
        assert_eq!(wide_lines.len(), tiny_lines.len(), "width {width}");
        let bar = tiny_lines
            .iter()
            .position(|l| l.starts_with("━━━ GOAL:"))
            .unwrap_or_else(|| panic!("no goal title bar at width {width}:\n{tiny_text}"));
        let last = tiny_lines.len();
        for i in [bar - 1, bar, last - 3, last - 2, last - 1] {
            assert_eq!(
                wide_lines[i], tiny_lines[i],
                "row {i} at width {width}:\n{wide_text}"
            );
        }
        // Every table row stays inside the pane: the title bar is intact and
        // the table starts below it.
        assert!(
            wide_lines[bar + 1..last - 3]
                .iter()
                .any(|l| l.contains("STATUS")),
            "no table header at width {width}:\n{wide_text}"
        );
    }
}

#[test]
fn a_width_sweep_keeps_the_section_columns_to_the_tasks_views_minus_goal() {
    for width in MIN_WIDTH..=200u16 {
        let mut sec = section(width, 30);
        let mut reference = tasks_view(width, 30);
        let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
        let expected: Vec<String> = tokens(&reference.line(ry))
            .into_iter()
            .filter(|t| t != "GOAL")
            .collect();
        assert_eq!(tokens(&sec.line(sy)), expected, "width {width}");
    }
}

#[test]
fn a_40_cell_pane_keeps_only_the_columns_the_tasks_view_keeps_there() {
    // At 40 cells the Tasks view keeps only ELAPSED* of the optional columns.
    let mut sec = section(40, 30);
    let y = header_y(&mut sec);
    let header = sec.line(y);
    for gone in ["GOAL", "ASSIGNEE", "STARTED"] {
        assert!(!header.contains(gone), "{gone} at 40 cells: {header:?}");
    }
    assert!(header.contains("ELAPSED*"), "{header:?}");
}

#[test]
fn a_narrow_pane_draws_one_row_per_task_without_wrapping() {
    let mut sec = section(40, 30);
    let y = header_y(&mut sec);
    let rows: Vec<String> = (1..=4).map(|i| sec.line(y + i)).collect();
    for title in LINKED {
        assert_eq!(
            rows.iter().filter(|r| r.contains(title)).count(),
            1,
            "{title}: {rows:#?}"
        );
    }
}

fn empty_goal() -> FakeBackend {
    FakeBackend::new()
        .with_tasks(goal_tasks(true))
        .with_goals(vec![make_goal("g0", "Empty goal", GoalStatus::Active)])
}

#[test]
fn an_empty_goal_draws_no_table_and_none_of_the_other_goals_tasks() {
    // Today a goal without tasks has no Tasks tab at all; this holds either way.
    let mut sec = section_with(empty_goal(), 100, 30);
    let text = sec.text();
    assert!(!text.contains("PRI"), "no table header:\n{text}");
    for title in LINKED {
        assert!(!text.contains(title), "{title}:\n{text}");
    }
}

#[test]
#[ignore = "R4 says an empty goal shows 'No entries.', but on master a goal with no tasks has no \
Tasks tab, so the section cannot be focused (focus falls back to Overview). The design must rule: \
keep the tab hidden (then drop this test and R4's sentence) or show the tab and this text. \
The builder removes the ignore if the tab is shown."]
fn an_empty_goal_with_the_tasks_section_focused_says_no_entries() {
    let mut sec = section_with(empty_goal(), 100, 30);
    sec.app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Tasks));
    sec.assert_text("No entries.");
}

#[test]
#[ignore = "same open question as an_empty_goal_with_the_tasks_section_focused_says_no_entries, \
at the narrowest supported width"]
fn an_empty_goal_says_no_entries_at_the_narrowest_supported_width() {
    let mut sec = section_with(empty_goal(), MIN_WIDTH, 30);
    sec.app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Tasks));
    sec.assert_text("No entries.");
}

#[test]
fn a_goal_that_loses_its_last_task_on_refresh_draws_no_rows_and_expands_nothing() {
    let mut sec = section(100, 30);
    sec.press(KeyCode::Down).press(KeyCode::Down);
    sec.app
        .state
        .tasks
        .retain(|t| t.goal_id.as_deref() != Some("g1"));
    let text = sec.text();
    for title in LINKED {
        assert!(!text.contains(title), "{title}:\n{text}");
    }
    sec.press(KeyCode::Enter);
    assert!(sec.app.state.viewer.is_none(), "no row, nothing to expand");
}
