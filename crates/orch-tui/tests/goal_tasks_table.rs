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
//! section drops the GOAL column (every row is the same goal) and gives its
//! width, plus the gap before it, to the TITLE column. So the Tasks view
//! reference is built from the same tasks with their goal link cleared (its
//! GOAL cells render as blanks), and a section row is compared with it
//! *slice by slice*, not as a whole line: the cells before TITLE and from
//! ASSIGNEE on are identical, the title text is identical, and the one thing
//! that moves is the attachment mark, which sits at the right edge of the
//! (now wider) title column. A whole-row equality would pin the old title
//! boundary and reject the approved layout.
//!
//! The RT-D1 cases (design v2 D5a-c, Section 6) exercise the replacement
//! guard: a goal Tasks pane whose picked task vanished must not act on the
//! substitute until a frame has painted that substitute in that same pane.
//! They never draw between the transition and the action: `Harness::draw`,
//! `text`, `assert_text`, `refute_text`, `header_y` and `drawn_titles` all
//! draw, so a case marks the one moment it draws and uses only input, state
//! and backend inspection after it.
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
use orch_tui::theme::{CURSOR, PAPERCLIP};
use orch_tui::widgets::text_viewer::ViewerSource;
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

/// A backend-side edit of task `id`. A refresh only commits tasks whose `id`
/// or `updated_at` differ (`tasks_changed`), so every edit stamps a fresh
/// `updated_at`; an edit that left it alone would never reach the dashboard.
fn edit_task(h: &mut Harness, id: &str, edit: impl FnOnce(&mut Task)) {
    static EDITS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let n = EDITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let task = h
        .app
        .backend
        .tasks
        .iter_mut()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("no backend task {id}"));
    edit(task);
    task.updated_at = iso(BASE - 100_000 + n);
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

/// Where the Q1 layout puts things on one row, read off the two headers so no
/// private seam is named: the section's cells and the Tasks view's cells agree
/// before `title_x` and from `assignee_x` on; the title text is the same; the
/// attachment mark moves right by `shift`, the Goal column plus its gap that
/// the section hands to the title.
struct Q1Layout {
    title_x: usize,
    assignee_x: usize,
    shift: usize,
}

#[track_caller]
fn q1_layout(sec: &Harness, sy: u16, reference: &Harness, ry: u16) -> Q1Layout {
    let (sec_title, sec_assignee) = (
        column_of(sec, sy, "TITLE") as usize,
        column_of(sec, sy, "ASSIGNEE") as usize,
    );
    let (ref_title, ref_goal, ref_assignee) = (
        column_of(reference, ry, "TITLE") as usize,
        column_of(reference, ry, "GOAL") as usize,
        column_of(reference, ry, "ASSIGNEE") as usize,
    );
    assert_eq!(
        sec_title, ref_title,
        "TITLE starts where the Tasks view's does"
    );
    assert_eq!(
        sec_assignee, ref_assignee,
        "the columns after the dropped GOAL keep their coordinates"
    );
    assert!(
        ref_goal < ref_assignee,
        "the fixture width must give the Tasks view a GOAL column"
    );
    Q1Layout {
        title_x: sec_title,
        assignee_x: sec_assignee,
        shift: ref_assignee - ref_goal,
    }
}

/// One section row against its Tasks view reference, as named slices of
/// cells: `(slice name, section cells, reference cells)`.
#[track_caller]
fn q1_slices(
    sec_cells: &[Cell],
    ref_cells: &[Cell],
    title: &str,
    layout: &Q1Layout,
) -> Vec<(&'static str, Vec<Cell>, Vec<Cell>)> {
    let title_w = unicode_width::UnicodeWidthStr::width(title);
    let (t, a) = (layout.title_x, layout.assignee_x);
    vec![
        (
            "cursor, status and priority",
            sec_cells[..t].to_vec(),
            ref_cells[..t].to_vec(),
        ),
        (
            "title text",
            sec_cells[t..t + title_w].to_vec(),
            ref_cells[t..t + title_w].to_vec(),
        ),
        (
            "assignee, started and elapsed",
            sec_cells[a..].to_vec(),
            ref_cells[a..].to_vec(),
        ),
    ]
}

#[test]
fn each_section_row_is_the_tasks_view_row_of_that_task_apart_from_the_reclaimed_title_width() {
    for width in [100, 140, 200] {
        let mut sec = section(width, 30);
        let mut reference = tasks_view(width, 30);
        // Move the cursor onto a row in both, so the cursor cell is compared too.
        sec.press(KeyCode::Down);
        reference.press(KeyCode::Down);
        let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
        let layout = q1_layout(&sec, sy, &reference, ry);
        for (i, title) in drawn_titles(&mut reference).iter().enumerate() {
            let (s, r) = (sy + 1 + i as u16, ry + 1 + i as u16);
            for (name, got, want) in
                q1_slices(&cells(&sec, s), &cells(&reference, r), title, &layout)
            {
                let symbols =
                    |cells: &[Cell]| cells.iter().map(|c| c.0.clone()).collect::<Vec<_>>();
                assert_eq!(
                    symbols(&got),
                    symbols(&want),
                    "{name}: {title} at width {width}"
                );
            }
        }
    }
}

#[test]
fn section_rows_carry_the_tasks_view_styles_for_status_priority_title_assignee_and_timing() {
    for width in [100, 140, 200] {
        let mut sec = section(width, 30);
        let mut reference = tasks_view(width, 30);
        sec.press(KeyCode::Down);
        reference.press(KeyCode::Down);
        let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
        let layout = q1_layout(&sec, sy, &reference, ry);
        for (i, title) in drawn_titles(&mut reference).iter().enumerate() {
            let (s, r) = (sy + 1 + i as u16, ry + 1 + i as u16);
            for (name, got, want) in
                q1_slices(&cells(&sec, s), &cells(&reference, r), title, &layout)
            {
                assert_eq!(got, want, "{name}: {title} at width {width}");
            }
        }
    }
}

#[test]
fn the_attachment_mark_moves_right_by_the_reclaimed_goal_width_and_keeps_its_style() {
    // Alpha is the only fixture task with an attachment. Its mark lives at the
    // right edge of the title column, so Q1's wider title moves it: a whole-row
    // oracle would pin it to the old boundary and reject the approved layout.
    for width in [100, 140, 200] {
        let mut sec = section(width, 30);
        let mut reference = tasks_view(width, 30);
        let (sy, ry) = (header_y(&mut sec), header_y(&mut reference));
        let layout = q1_layout(&sec, sy, &reference, ry);
        let row_of = |h: &mut Harness, y0: u16| {
            let i = drawn_titles(h)
                .iter()
                .position(|t| t == "Alpha")
                .expect("Alpha is drawn");
            y0 + 1 + i as u16
        };
        let (s, r) = (row_of(&mut sec, sy), row_of(&mut reference, ry));
        let (sm, rm) = (
            column_of(&sec, s, PAPERCLIP) as usize,
            column_of(&reference, r, PAPERCLIP) as usize,
        );
        assert_eq!(sm, rm + layout.shift, "mark column at width {width}");
        assert_eq!(cells(&sec, s)[sm], cells(&reference, r)[rm], "mark style");
        assert!(
            sec.line(s).contains(&format!("{PAPERCLIP}1")),
            "{:?}",
            sec.line(s)
        );
        // Nothing else sits between the title text and the assignee column.
        let blanks = |h: &Harness, y: u16, from: usize, to: usize| {
            cells(h, y)[from..to].iter().all(|c| c.0 == " ")
        };
        assert!(
            blanks(&sec, s, layout.title_x + "Alpha".len(), sm),
            "before the mark"
        );
        assert!(blanks(&sec, s, sm + 3, layout.assignee_x), "after the mark");
    }
}

#[test]
fn a_title_the_tasks_view_clips_is_whole_in_the_section_because_the_goal_width_went_to_the_title() {
    let mut plain = tasks_view(100, 30);
    let y = header_y(&mut plain);
    // The Tasks view's title column: from TITLE to the gap before GOAL.
    let title_w = (column_of(&plain, y, "GOAL") - column_of(&plain, y, "TITLE") - 1) as usize;
    let long: String = ('a'..='z').cycle().take(title_w + 2).collect();
    let with_long_title = |linked: bool| {
        let mut tasks = goal_tasks(linked);
        tasks[0].title = long.clone();
        let goals = if linked {
            vec![make_goal("g1", "Goal one", GoalStatus::Active)]
        } else {
            Vec::new()
        };
        FakeBackend::new()
            .with_tasks(tasks)
            .with_goals(goals)
            .with_agents(vec![make_agent("agt", "worker", AgentStatus::Idle)])
    };
    let mut reference = Harness::sized(with_long_title(false), 100, 30);
    reference.app.state.view = ViewId::Tasks;
    reference.refute_text(&long);
    let mut sec = section_with(with_long_title(true), 100, 30);
    let text = sec.text();
    let row = text
        .lines()
        .find(|l| l.contains(&long))
        .unwrap_or_else(|| panic!("the whole title is drawn in the section:\n{text}"));
    assert!(
        row.contains("DONE"),
        "in a table row with the status chip: {row:?}"
    );
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
        TaskSortField::Goal,
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

/// The click identity of the row of `title`, which is drawn second under the
/// ascending Title sort: the zone under its title cell.
fn bravo_zone(h: &mut Harness) -> orch_tui::hit::Zone {
    sort_title(h, false);
    let y = header_y(h) + 2;
    let x = column_of(h, y, "Bravo");
    h.zone_at(x, y)
        .cloned()
        .unwrap_or_else(|| panic!("no zone at ({x}, {y})"))
}

#[test]
fn a_section_row_zone_is_identified_by_its_task_id_in_the_inspector_text_source() {
    // The identity is the task id, not the row number, so a refresh between
    // press and release cancels the click instead of moving it to the row that
    // slid in. It is the inspector's existing ExpandText source for the task's
    // description, not a main-list `Entity` zone: a click here must never
    // select a row of the main Tasks list.
    let mut sec = section(100, 30);
    assert_eq!(
        bravo_zone(&mut sec).id,
        ZoneId::ExpandText(ViewerSource::Entity {
            kind: EntityType::Task,
            id: "t_b".into(),
            field: "description".into(),
        })
    );
}

#[test]
fn a_section_row_zone_is_an_expand_text_zone_not_a_main_list_task_row() {
    let mut sec = section(100, 30);
    assert_eq!(bravo_zone(&mut sec).kind, ZoneKind::ExpandText);
}

#[test]
fn a_retitle_in_state_between_press_and_release_does_not_click_the_row_that_slid_in() {
    // Mutates `state.tasks` directly and redraws: this checks that the click
    // identity follows the task id across a redraw, not the refresh path (the
    // next test drives that through the backend).
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
fn a_backend_refresh_between_press_and_release_does_not_click_the_row_that_slid_in() {
    let mut sec = section(100, 30);
    sort_title(&mut sec, false); // Alpha, Bravo, Charlie, Delta
    let y = header_y(&mut sec) + 3; // Charlie
    let x = column_of(&sec, y, "Charlie");
    sec.press_at(x, y);
    // The backend renames Alpha so it sorts last: Delta slides into the pressed
    // row. The refresh runs with no draw before the release, as in the runner.
    sec.app
        .backend
        .tasks
        .iter_mut()
        .find(|t| t.id == "t_a")
        .unwrap()
        .title = "Zulu".into();
    sec.app.refresh_all(false);
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
        "a click whose rows moved under it, with no frame since, hit the row that slid in"
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
    // Goal `g0` has no task; the tasks in the backend belong to goals that do
    // not exist here, so none of them may show up in its section.
    FakeBackend::new()
        .with_tasks(goal_tasks(true))
        .with_goals(vec![make_goal("g0", "Empty goal", GoalStatus::Active)])
}

/// Goal `g0` opened, then the inspector focus moved with Tab the way a user
/// does (not forced with `set_focus`), until the Tasks section has it.
#[track_caller]
fn empty_goal_at_tasks(width: u16, height: u16) -> Harness {
    let mut h = Harness::sized(empty_goal(), width, height);
    h.app.state.view = ViewId::Goals;
    h.press(KeyCode::Enter);
    for _ in 0..8 {
        if h.app.state.focus == Focus::GoalDetail(GoalSection::Tasks) {
            return h;
        }
        h.press(KeyCode::Tab);
    }
    panic!(
        "Tab never reached the Tasks section of a goal without tasks (R4: it must stay in the \
         focus ring so 'No entries.' is reachable); focus ring {:?}, focus {:?}",
        h.app.state.focus_ring(),
        h.app.state.focus
    );
}

#[test]
fn an_empty_goal_keeps_its_tasks_section_in_the_focus_ring() {
    let mut h = Harness::sized(empty_goal(), 100, 30);
    h.app.state.view = ViewId::Goals;
    h.press(KeyCode::Enter);
    assert!(
        h.app
            .state
            .focus_ring()
            .contains(&Focus::GoalDetail(GoalSection::Tasks)),
        "{:?}",
        h.app.state.focus_ring()
    );
}

#[test]
fn an_empty_goal_with_the_tasks_section_focused_says_no_entries() {
    let mut sec = empty_goal_at_tasks(100, 30);
    sec.assert_text("No entries.");
}

#[test]
fn an_empty_goal_says_no_entries_at_the_narrowest_supported_width() {
    let mut sec = empty_goal_at_tasks(MIN_WIDTH, 30);
    sec.assert_text("No entries.");
}

#[test]
fn an_empty_goal_still_draws_the_table_header_above_the_empty_message() {
    let mut sec = empty_goal_at_tasks(100, 30);
    let header = header_y(&mut sec);
    let message = sec.find_text("No entries.").expect("the empty message").1;
    assert!(
        header < message,
        "header at row {header}, message at row {message}"
    );
}

#[test]
fn an_empty_goal_draws_none_of_the_other_goals_tasks() {
    let mut sec = empty_goal_at_tasks(100, 30);
    let text = sec.text();
    for title in LINKED {
        assert!(!text.contains(title), "{title}:\n{text}");
    }
}

#[test]
fn an_empty_goal_has_no_row_to_expand_or_copy() {
    let mut sec = empty_goal_at_tasks(100, 30);
    sec.draw();
    sec.press(KeyCode::Down).press(KeyCode::Enter);
    ctrl(&mut sec, 'y');
    assert!(sec.app.state.viewer.is_none(), "nothing to expand");
    assert!(sec.app.backend.copy_requests.is_empty(), "nothing to copy");
}

#[test]
fn an_empty_goal_keeps_its_tasks_section_across_a_tick_and_a_refresh() {
    let mut sec = empty_goal_at_tasks(100, 30);
    sec.draw().advance(1_000).draw();
    sec.app.refresh_all(false);
    assert_eq!(sec.app.state.focus, Focus::GoalDetail(GoalSection::Tasks));
    sec.assert_text("No entries.");
}

#[test]
fn a_task_that_arrives_in_an_empty_goal_becomes_a_selectable_row() {
    let mut sec = empty_goal_at_tasks(100, 30);
    sec.draw();
    sec.app
        .backend
        .tasks
        .push(task("t_new", "Newcomer", TaskStatus::Todo, 2, Some("g0")));
    sec.app.refresh_all(false);
    sec.assert_text("Newcomer");
    sec.refute_text("No entries.");
    // The replacement guard (RT-D1) must not mistake an arrival for a loss:
    // there was no picked task to lose, so Enter opens the one row.
    sec.press(KeyCode::Enter);
    let viewer = sec
        .app
        .state
        .viewer
        .as_ref()
        .expect("Enter opens the viewer");
    assert_eq!(&*viewer.document().body, "DESC-NEWCOMER");
}

#[test]
fn a_goal_whose_tasks_vanish_from_state_draws_no_rows_and_expands_nothing() {
    // `state.tasks` is edited directly here: this is the redraw-with-no-rows
    // case. The backend refresh path is the next test.
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

#[test]
fn a_goal_that_loses_its_last_task_on_a_backend_refresh_stays_in_tasks_with_no_source() {
    let mut sec = section(100, 30);
    sec.draw().press(KeyCode::Down).press(KeyCode::Down).draw();
    sec.app
        .backend
        .tasks
        .retain(|t| t.goal_id.as_deref() != Some("g1"));
    sec.app.refresh_all(false);
    let state = &sec.app.state;
    let pane = state.pane_scroll(&state.pane_key(Focus::GoalDetail(GoalSection::Tasks)));
    assert_eq!(
        (
            state.focus,
            state.focused_source(),
            pane.selected,
            pane.offset
        ),
        (Focus::GoalDetail(GoalSection::Tasks), None, None, 0),
        "focus stays, no source, selection and offset cleared"
    );
    sec.assert_text("No entries.");
}

#[test]
fn a_goal_that_loses_its_last_task_on_a_backend_refresh_has_nothing_to_expand_or_copy() {
    let mut sec = section(100, 30);
    sec.draw().press(KeyCode::Down).draw();
    sec.app
        .backend
        .tasks
        .retain(|t| t.goal_id.as_deref() != Some("g1"));
    sec.app.refresh_all(false);
    sec.draw();
    sec.press(KeyCode::Enter);
    ctrl(&mut sec, 'y');
    assert!(sec.app.state.viewer.is_none(), "no viewer");
    assert!(sec.app.backend.copy_requests.is_empty(), "no copy");
}

// ── R2/R3: the keyword filter, and a picked task stays picked ──────────────

#[test]
fn a_no_match_keyword_on_the_tasks_view_does_not_hide_any_row_of_the_section() {
    let mut sec = section(100, 30);
    switch_view(&mut sec, ViewId::Tasks);
    ctrl(&mut sec, 'k');
    sec.type_str("zzz-matches-nothing").press(KeyCode::Enter);
    assert_eq!(sec.app.state.keyword(ViewId::Tasks), "zzz-matches-nothing");
    switch_view(&mut sec, ViewId::Goals);
    if !sec.app.state.detail_open() {
        // A keyword nothing matches leaves the Tasks view without a selected
        // task, which closes its inspector and defers entity actions until a
        // frame: draw, then open goal g1's inspector again.
        sec.draw().press(KeyCode::Enter);
    }
    assert!(
        sec.app.state.detail_open(),
        "setup: goal g1's inspector is open"
    );
    sec.app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Tasks));
    let mut titles = drawn_titles(&mut sec);
    titles.sort();
    assert_eq!(titles, ["Alpha", "Bravo", "Charlie", "Delta"]);
}

#[test]
fn sorting_from_the_section_keeps_the_picked_task_picked_for_enter_and_copy() {
    for act_on in [Act::Enter, Act::Copy] {
        let mut sec = section(100, 30);
        let picked = pick(&mut sec, 2);
        let before = state_order(&sec);
        sec.key('O'); // reverse; no draw before the action
        assert_ne!(state_order(&sec), before, "the sort must move the rows");
        assert_eq!(
            act(&mut sec, act_on),
            Outcome::of(&picked),
            "{act_on:?} after a sort with no frame in between"
        );
    }
}

#[test]
fn a_refresh_that_reorders_the_rows_keeps_the_picked_task_picked_for_enter_and_copy() {
    for act_on in [Act::Enter, Act::Copy] {
        let mut sec = section(100, 30);
        let picked = pick(&mut sec, 2);
        let index = |h: &Harness| state_order(h).iter().position(|t| *t == picked);
        let before = index(&sec);
        // The picked task is cancelled, so the Status sort moves it to the end.
        let status = if picked == "Delta" {
            TaskStatus::Failed
        } else {
            TaskStatus::Done
        };
        edit_task(&mut sec, &tid(&picked), |t| t.status = status);
        sec.app.refresh_all(false);
        assert_ne!(index(&sec), before, "the refresh must move the picked row");
        assert_eq!(
            act(&mut sec, act_on),
            Outcome::of(&picked),
            "{act_on:?} after a reordering refresh with no frame in between"
        );
    }
}

// ── RT-D1: a lost pick's substitute is not actionable until its pane paints it ──
//
// Design v2 D5a-c. A goal Tasks pane keeps its own pick. When refresh takes
// that pick away (deleted, or moved to another goal), the pane falls back to
// the nearest surviving row and Enter / Ctrl+Y must wait until a frame has
// painted *that pane's* selected substitute. A frame of another surface must not
// release them, and an unrelated main-list loss must not block a pick that
// survived. Every case below keeps `Harness::draw` and the helpers that draw
// out of the span between the transition and the action.

#[derive(Debug, Clone, Copy)]
enum Loss {
    Delete,
    Reparent,
}

const LOSSES: [Loss; 2] = [Loss::Delete, Loss::Reparent];

#[derive(Debug, Clone, Copy)]
enum Act {
    Enter,
    Copy,
}

/// What an action reached: the viewer's source and body for Enter, the copied
/// text for Ctrl+Y. Both `None` means the action did nothing.
#[derive(Debug, PartialEq)]
struct Outcome {
    source: Option<ViewerSource>,
    body: Option<String>,
}

impl Outcome {
    const NOTHING: Outcome = Outcome {
        source: None,
        body: None,
    };

    /// The same for either action: the viewer source (Enter only) and body of
    /// the task titled `title`, which `Act::Copy` leaves without a source.
    fn of(title: &str) -> OutcomeOf {
        OutcomeOf(title.to_string())
    }
}

/// An outcome that matches Enter (source and body) or Copy (body) for a title.
#[derive(Debug)]
struct OutcomeOf(String);

impl PartialEq<OutcomeOf> for Outcome {
    fn eq(&self, other: &OutcomeOf) -> bool {
        let body = Some(desc(&other.0));
        self.body == body
            && match &self.source {
                Some(source) => *source == entity_source(&tid(&other.0)),
                None => true,
            }
    }
}

/// The fixture's task id for a title: `Alpha` is `t_a`.
fn tid(title: &str) -> String {
    format!(
        "t_{}",
        title.to_lowercase().chars().next().expect("a title")
    )
}

fn desc(title: &str) -> String {
    format!("DESC-{}", title.to_uppercase())
}

fn entity_source(id: &str) -> ViewerSource {
    ViewerSource::Entity {
        kind: EntityType::Task,
        id: id.into(),
        field: "description".into(),
    }
}

/// `App::switch_view`, which is crate-private: the view, then focus back on
/// the list, then the focus ring normalised.
fn switch_view(h: &mut Harness, view: ViewId) {
    h.app.state.view = view;
    h.app.state.set_focus(Focus::MainList);
    h.app.state.normalize_focus();
}

/// The user's pick: the drawn row at `ordinal` of the goal's Tasks pane,
/// painted. Returns its title. Draws, so it belongs to the setup, never to the
/// span between a transition and an action.
#[track_caller]
fn pick(h: &mut Harness, ordinal: usize) -> String {
    h.draw();
    for _ in 0..ordinal {
        h.press(KeyCode::Down);
    }
    h.draw();
    let picked = state_order(h)[ordinal].clone();
    assert_eq!(
        h.app.state.focused_source(),
        Some(entity_source(&tid(&picked))),
        "setup: the pane's pick is row {ordinal} of the shared sort order"
    );
    picked
}

fn pick_second(h: &mut Harness) -> String {
    pick(h, 1)
}

/// The backend takes the task titled `title` away from goal `g1`, then a
/// refresh runs. No draw.
fn lose(h: &mut Harness, title: &str, loss: Loss) {
    let id = tid(title);
    match loss {
        Loss::Delete => h.app.backend.tasks.retain(|t| t.id != id),
        Loss::Reparent => edit_task(h, &id, |t| t.goal_id = Some("g2".into())),
    }
    h.app.refresh_all(false);
}

/// The task the pane falls back to when its second row vanishes: the old
/// ordinal, clamped, in the new shared order.
#[track_caller]
fn fallback_after(h: &Harness, picked: &str) -> String {
    let order = state_order(h);
    assert!(
        !order.iter().any(|t| t == picked),
        "{picked} is gone: {order:?}"
    );
    let fallback = order[1.min(order.len() - 1)].clone();
    assert_ne!(
        desc(&fallback),
        desc(picked),
        "the substitute must differ from the lost pick"
    );
    fallback
}

/// Press the action's key and report what it reached. Does not draw.
fn act(h: &mut Harness, act: Act) -> Outcome {
    match act {
        Act::Enter => {
            h.press(KeyCode::Enter);
            match h.app.state.viewer.as_ref() {
                Some(v) => Outcome {
                    source: Some(v.document().source.clone()),
                    body: Some(v.document().body.to_string()),
                },
                None => Outcome::NOTHING,
            }
        }
        Act::Copy => {
            ctrl(h, 'y');
            match h.app.backend.copy_requests.last() {
                Some((_, body)) => Outcome {
                    source: None,
                    body: Some(body.clone()),
                },
                None => Outcome::NOTHING,
            }
        }
    }
}

/// Goal `g1`'s Tasks pick is lost while the inspector shows Overview, and
/// only an Overview frame is drawn after the refresh. Focus is back on Tasks;
/// nothing has drawn since. Returns the harness, the lost title and its
/// substitute.
fn lost_behind_overview(loss: Loss) -> (Harness, String, String) {
    let mut h = section(100, 30);
    let picked = pick_second(&mut h);
    h.app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Overview));
    h.draw(); // Overview frame, before the loss
    lose(&mut h, &picked, loss);
    h.draw(); // Overview frame, after the refresh: must not authorise the pane
    h.app.state.set_focus(Focus::GoalDetail(GoalSection::Tasks));
    let fallback = fallback_after(&h, &picked);
    (h, picked, fallback)
}

/// The same, with the main Tasks view (not the goal inspector) as the surface
/// that draws after the refresh.
fn lost_behind_main_tasks(loss: Loss) -> (Harness, String, String) {
    let mut h = section(100, 30);
    let picked = pick_second(&mut h);
    switch_view(&mut h, ViewId::Tasks);
    h.draw(); // main Tasks frame, before the loss
    lose(&mut h, &picked, loss);
    h.draw(); // main Tasks frame, after the refresh: must not authorise the pane
    switch_view(&mut h, ViewId::Goals);
    h.app.state.set_focus(Focus::GoalDetail(GoalSection::Tasks));
    assert_eq!(
        h.app.state.selected_goal().map(|g| g.id.as_str()),
        Some("g1"),
        "setup: back on goal g1"
    );
    let fallback = fallback_after(&h, &picked);
    (h, picked, fallback)
}

#[test]
fn a_lost_hidden_pick_is_not_actionable_after_only_an_overview_frame() {
    for loss in LOSSES {
        for act_on in [Act::Enter, Act::Copy] {
            let (mut h, picked, fallback) = lost_behind_overview(loss);
            assert_eq!(
                act(&mut h, act_on),
                Outcome::NOTHING,
                "{act_on:?} on the unseen substitute {fallback} of {picked} ({loss:?})"
            );
        }
    }
}

#[test]
fn a_lost_hidden_pick_is_not_actionable_after_only_a_main_tasks_frame() {
    for loss in LOSSES {
        for act_on in [Act::Enter, Act::Copy] {
            let (mut h, picked, fallback) = lost_behind_main_tasks(loss);
            assert_eq!(
                act(&mut h, act_on),
                Outcome::NOTHING,
                "{act_on:?} on the unseen substitute {fallback} of {picked} ({loss:?})"
            );
        }
    }
}

#[test]
fn once_the_tasks_pane_paints_the_substitute_enter_and_copy_act_on_it_after_an_overview_frame() {
    for loss in LOSSES {
        for act_on in [Act::Enter, Act::Copy] {
            let (mut h, _, fallback) = lost_behind_overview(loss);
            h.draw(); // the pane paints its selected substitute
            assert_eq!(
                act(&mut h, act_on),
                Outcome::of(&fallback),
                "{act_on:?} ({loss:?})"
            );
        }
    }
}

#[test]
fn once_the_tasks_pane_paints_the_substitute_enter_and_copy_act_on_it_after_a_main_tasks_frame() {
    for loss in LOSSES {
        for act_on in [Act::Enter, Act::Copy] {
            let (mut h, _, fallback) = lost_behind_main_tasks(loss);
            h.draw();
            assert_eq!(
                act(&mut h, act_on),
                Outcome::of(&fallback),
                "{act_on:?} ({loss:?})"
            );
        }
    }
}

#[test]
fn the_opened_substitute_names_its_task_id_as_the_viewer_source() {
    // Source identity, not just body text: the viewer document names the
    // substitute's id and the `description` field (not the lost pick's).
    let (mut h, picked, fallback) = lost_behind_overview(Loss::Delete);
    h.draw();
    let outcome = act(&mut h, Act::Enter);
    assert_eq!(
        outcome.source,
        Some(entity_source(&tid(&fallback))),
        "opened the substitute of {picked}"
    );
}

#[test]
fn the_pane_paints_the_cursor_on_the_substitute_row() {
    let (mut h, _, fallback) = lost_behind_overview(Loss::Delete);
    let (_, y) = {
        h.draw();
        h.find_text(&fallback).expect("the substitute row is drawn")
    };
    assert!(
        h.line(y).starts_with(CURSOR),
        "the substitute carries the cursor: {:?}",
        h.line(y)
    );
}

/// Twelve-plus tasks of goal `g1`, all Todo and equal in priority, so the
/// default Status sort keeps source order. `Aa 00` is the pane's pick; the
/// main list's pick is `Zz Main`, the last row inside the ten-row cap, which
/// the Title sort pushes past the cap while `Aa 00` survives it.
fn capped_backend() -> FakeBackend {
    let count = TASK_LIST_LIMIT + 2;
    let tasks: Vec<Task> = (0..count)
        .map(|i| {
            let title = if i == TASK_LIST_LIMIT - 1 {
                "Zz Main".to_string()
            } else {
                format!("Aa {i:02}")
            };
            let mut t = task(
                &format!("t_{i:02}"),
                &title,
                TaskStatus::Todo,
                3,
                Some("g1"),
            );
            t.description = format!("DESC-{i:02}");
            t
        })
        .collect();
    FakeBackend::new()
        .with_tasks(tasks)
        .with_goals(vec![make_goal("g1", "Goal one", GoalStatus::Active)])
}

/// The capped fixture with the pane on `Aa 00` and the main list on `Zz Main`,
/// then the shared sort moved to Title from inside the Tasks section with no
/// frame since. Returns the harness; the main list has lost `Zz Main` to the
/// cap while the pane's `Aa 00` is still where it was picked.
#[track_caller]
fn retained_pick_with_capped_out_main_pick() -> Harness {
    let mut h = section_with(capped_backend(), 100, 30);
    h.draw().press(KeyCode::Down).press(KeyCode::Up).draw(); // pane pick: Aa 00
    assert_eq!(
        h.app.state.focused_source(),
        Some(entity_source("t_00")),
        "setup: the pane's pick is Aa 00"
    );
    switch_view(&mut h, ViewId::Tasks);
    h.draw();
    for _ in 0..TASK_LIST_LIMIT - 1 {
        h.press(KeyCode::Down);
    }
    h.draw();
    assert_eq!(
        h.app.state.selected_task().map(|t| t.title.as_str()),
        Some("Zz Main"),
        "setup: the main list's pick is the last row inside the cap"
    );
    switch_view(&mut h, ViewId::Goals);
    h.app.state.set_focus(Focus::GoalDetail(GoalSection::Tasks));
    h.draw();
    for _ in 0..7 {
        if h.app.state.task_sort.field == TaskSortField::Title {
            break;
        }
        h.key('o'); // the Tasks section's `o` drives the shared task sort
    }
    assert_eq!(
        h.app.state.task_sort.field,
        TaskSortField::Title,
        "setup: `o` in the Tasks section reaches the Title sort"
    );
    assert_ne!(
        h.app.state.selected_task().map(|t| t.title.as_str()),
        Some("Zz Main"),
        "setup: the Title sort pushed the main pick past the cap"
    );
    h
}

#[test]
fn a_retained_pane_pick_stays_actionable_when_the_main_list_pick_falls_past_the_cap() {
    for act_on in [Act::Enter, Act::Copy] {
        let mut h = retained_pick_with_capped_out_main_pick();
        let outcome = act(&mut h, act_on);
        assert_eq!(
            (outcome.body.as_deref(), outcome.source),
            (
                Some("DESC-00"),
                match act_on {
                    Act::Enter => Some(entity_source("t_00")),
                    Act::Copy => None,
                }
            ),
            "{act_on:?}: the pane's Aa 00 survived, the main list's loss must not block it"
        );
    }
}

#[test]
fn a_lost_hidden_pick_does_not_block_a_main_list_pick_that_survived() {
    let mut h = section(100, 30);
    let picked = pick_second(&mut h);
    switch_view(&mut h, ViewId::Tasks);
    h.draw();
    // The main list's pick is its first row: not the pane's, and still present.
    let main_pick = h.app.state.selected_task().map(|t| t.id.clone());
    assert_ne!(main_pick, Some(tid(&picked)), "setup: distinct picks");
    lose(&mut h, &picked, Loss::Delete); // no draw
    assert_eq!(
        h.app.state.selected_task().map(|t| t.id.clone()),
        main_pick,
        "setup: the main pick survived the refresh"
    );
    let open = h.app.state.detail_open();
    h.press(KeyCode::Enter);
    assert_ne!(
        h.app.state.detail_open(),
        open,
        "Enter on the surviving main-list pick toggles its detail"
    );
}

#[test]
fn a_second_refresh_that_keeps_the_unpainted_substitute_does_not_authorise_it() {
    for act_on in [Act::Enter, Act::Copy] {
        let (mut h, _, fallback) = lost_behind_overview(Loss::Delete);
        // Another refresh: something unrelated changes, the substitute stays.
        edit_task(&mut h, "t_f", |t| t.title = "Foreign, renamed".into());
        h.app.refresh_all(false);
        assert!(
            state_order(&h).contains(&fallback),
            "setup: the substitute survived the second refresh"
        );
        h.app
            .state
            .set_focus(Focus::GoalDetail(GoalSection::Overview));
        h.draw(); // yet another frame of another surface
        h.app.state.set_focus(Focus::GoalDetail(GoalSection::Tasks));
        assert_eq!(
            act(&mut h, act_on),
            Outcome::NOTHING,
            "{act_on:?}: retaining {fallback} proves nothing about it having been shown"
        );
    }
}

#[test]
fn losing_the_pending_substitute_keeps_the_guard_and_only_the_painted_new_substitute_acts() {
    for act_on in [Act::Enter, Act::Copy] {
        let (mut h, _, first) = lost_behind_overview(Loss::Delete);
        // The substitute itself vanishes before it was ever painted.
        h.app
            .state
            .set_focus(Focus::GoalDetail(GoalSection::Overview));
        lose(&mut h, &first, Loss::Delete);
        h.draw(); // Overview again
        h.app.state.set_focus(Focus::GoalDetail(GoalSection::Tasks));
        let second = state_order(&h)[1.min(state_order(&h).len() - 1)].clone();
        assert_ne!(second, first, "setup: a new substitute");
        assert_eq!(
            act(&mut h, act_on),
            Outcome::NOTHING,
            "{act_on:?} before {second} was painted"
        );
        h.draw(); // the pane paints the new substitute
        assert_eq!(
            act(&mut h, act_on),
            Outcome::of(&second),
            "{act_on:?} after {second} was painted"
        );
    }
}
