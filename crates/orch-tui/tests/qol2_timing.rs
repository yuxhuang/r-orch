//! The Started and `Elapsed*` columns, their disclosure, and the Elapsed sort
//! (R11, R14, R15).
//!
//! `Elapsed*` is a disclosed approximation, not a completion duration: a live
//! row runs to the frame clock, a terminal row stops at its last update, and
//! the order is sampled once per refresh or list-control change. Every case
//! here holds the clock still, so the numbers on screen are exact.

mod common;

use common::Harness;
use orch_domain::{GoalStatus, Run, RunStatus, TaskStatus};
use orch_tui::backend::{FakeBackend, make_goal, make_task};
use orch_tui::state::ViewId;
use orch_tui::widgets::help_overlay::{ELAPSED_DISCLOSURE, ELAPSED_HELP, ELAPSED_ORDER};

/// The instant [`Harness`] starts its clock at.
const BASE: i64 = 1_000_000;

/// Started (8) + gap (1) + `Elapsed*` (9): the last cells of a wide row, in
/// both the Goals and the Tasks table.
const TAIL_W: usize = 18;

/// ISO-8601 for an instant on 1970-01-01, which is all these cases need.
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

/// The one screen row holding `needle`.
#[track_caller]
fn row<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{screen}"))
}

/// A row's Started and `Elapsed*` cells, which are its last [`TAIL_W`] cells.
#[track_caller]
fn tail(line: &str) -> String {
    let cells: Vec<char> = line.chars().collect();
    assert!(
        cells.len() >= TAIL_W,
        "{line:?} is too short to have a tail"
    );
    cells[cells.len() - TAIL_W..].iter().collect()
}

/// Started and `Elapsed*` as the table lays them out: right-aligned in their
/// columns.
fn cells(started: &str, elapsed: &str) -> String {
    format!("{started:>8} {elapsed:>9}")
}

fn sorted_task_ids(h: &Harness) -> Vec<String> {
    h.app
        .state
        .sorted_tasks()
        .into_iter()
        .map(|i| h.app.state.tasks[i].id.clone())
        .collect()
}

fn sorted_goal_ids(h: &Harness) -> Vec<String> {
    h.app
        .state
        .sorted_goals()
        .into_iter()
        .map(|i| h.app.state.goals[i].id.clone())
        .collect()
}

// ── The columns (R11) ──────────────────────────────────────────────────────

#[test]
fn task_table_started_elapsed() {
    // Started 90 s ago and still running, so Started reads `1m` and Elapsed
    // the 1:30 it has been going.
    let mut live = make_task("tsk_live", "Live task", TaskStatus::InProgress);
    live.updated_at = iso(BASE - 90_000);
    let never = make_task("tsk_never", "Never started", TaskStatus::Todo);
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![live, never])
            .with_runs(vec![run("tsk_live", BASE - 90_000)]),
        120,
        40,
    );
    h.app.state.view = ViewId::Tasks;
    let screen = h.text();

    let header = row(&screen, "STATUS");
    assert!(header.contains("STARTED"), "{screen}");
    assert!(header.contains("ELAPSED*"), "{screen}");
    assert_eq!(tail(header), cells("STARTED", "ELAPSED*"), "{screen}");

    assert_eq!(
        tail(row(&screen, "Live task")),
        cells("1m", "1:30"),
        "{screen}"
    );
    // Never started: both cells are blank. Not `0s`, which would assert that
    // it started and took no time.
    assert_eq!(
        tail(row(&screen, "Never started")),
        " ".repeat(TAIL_W),
        "{screen}"
    );
}

#[test]
fn goal_table_started_elapsed() {
    let mut goal = make_goal("gol_live", "Live goal", GoalStatus::Active);
    goal.created_at = iso(BASE - 90_000);
    let mut h = Harness::sized(FakeBackend::new().with_goals(vec![goal]), 120, 40);
    h.app.state.view = ViewId::Goals;
    let screen = h.text();

    let header = row(&screen, "STATUS");
    assert!(header.contains("STARTED"), "{screen}");
    assert!(header.contains("ELAPSED*"), "{screen}");
    assert_eq!(tail(header), cells("STARTED", "ELAPSED*"), "{screen}");

    // A goal starts when it was created, so its Started is never blank.
    assert_eq!(
        tail(row(&screen, "Live goal")),
        cells("1m", "1:30"),
        "{screen}"
    );
}

#[test]
fn a_timing_failure_is_visible_and_leaves_the_spans_unknown() {
    let mut backend = FakeBackend::new()
        .with_tasks(vec![make_task("tsk_a", "Alpha", TaskStatus::InProgress)])
        .with_runs(vec![run("tsk_a", BASE - 90_000)]);
    backend.timings_error = Some("runs/broken.json: invalid JSON".into());
    let mut h = Harness::sized(backend, 120, 40);
    h.app.state.view = ViewId::Tasks;
    h.assert_message("Task timings unavailable: runs/broken.json: invalid JSON");
    // `?`, not a blank: the history could not be read, which is not the same
    // claim as "never started".
    let screen = h.text();
    assert_eq!(tail(row(&screen, "Alpha")), cells("?", "?"), "{screen}");
}

// ── The disclosure (R15) ───────────────────────────────────────────────────

/// The screen rows of the help overlay's [`ELAPSED_HELP`] block, which must
/// sit on consecutive rows, at `width`×`height`.
#[track_caller]
fn elapsed_help_rows(width: u16, height: u16) -> (String, Vec<String>) {
    let mut h = Harness::sized(FakeBackend::new(), width, height);
    h.key('?');
    let screen = h.text();
    let rows: Vec<&str> = screen.lines().collect();
    let first = rows
        .iter()
        .position(|line| line.contains(ELAPSED_HELP[0]))
        .unwrap_or_else(|| {
            panic!("at {width}x{height} the first Elapsed* help line is not on screen:\n{screen}")
        });
    let block: Vec<String> = rows[first..]
        .iter()
        .take(ELAPSED_HELP.len())
        .map(|line| line.trim().to_owned())
        .collect();
    (screen, block)
}

#[test]
fn help_overlay_carries_the_elapsed_disclosure() {
    let (screen, block) = elapsed_help_rows(100, 30);
    assert_eq!(block, ELAPSED_HELP, "{screen}");
}

#[test]
fn help_overlay_words_the_disclosure_and_the_order_rule_verbatim() {
    // Wrapped over three rows, the sentences R15 and D1 word must each still
    // read exactly once, word for word.
    let wrapped = ELAPSED_HELP.join(" ");
    assert_eq!(
        (
            wrapped.matches(ELAPSED_DISCLOSURE).count(),
            wrapped.matches(ELAPSED_ORDER).count()
        ),
        (1, 1),
        "{wrapped:?}"
    );
}

// R15 makes the help line part of the minimum discoverability of `Elapsed*`,
// so it must be on screen at the default terminal size, not only on a tall one.
#[test]
fn help_overlay_shows_the_elapsed_disclosure_at_80x24() {
    let (screen, block) = elapsed_help_rows(80, 24);
    assert_eq!(block, ELAPSED_HELP, "{screen}");
}

#[test]
fn help_overlay_keeps_every_shortcut_and_the_dismiss_hint_at_80x24() {
    let mut h = Harness::sized(FakeBackend::new(), 80, 24);
    h.key('?');
    let screen = h.text();
    let missing: Vec<&str> = [
        "KEYBOARD SHORTCUTS",
        "NAVIGATION",
        "Inspector",
        "Undo delete",
        "/quit",
        "Press any key to dismiss",
        "Goal/agent Activity, newest first",
        "Lists: o sort",
    ]
    .into_iter()
    .filter(|needle| !screen.contains(needle))
    .collect();
    assert!(
        missing.is_empty(),
        "missing {missing:?} at 80x24:\n{screen}"
    );
}

#[test]
fn help_overlay_shows_the_elapsed_disclosure_at_120x40() {
    let (screen, block) = elapsed_help_rows(120, 40);
    assert_eq!(block, ELAPSED_HELP, "{screen}");
}

// ── The Elapsed sort (R14/R15) ─────────────────────────────────────────────

/// A terminal task whose span froze at 5 000 ms, and a live one 4 999 ms old
/// when the sort samples: one millisecond either side of the crossing, so the
/// order can only come from the displayed span.
fn crossing_tasks() -> Harness {
    let mut done = make_task("tsk_done", "Ended", TaskStatus::Done);
    done.updated_at = iso(BASE - 5_000);
    let mut live = make_task("tsk_live", "Running", TaskStatus::InProgress);
    live.updated_at = iso(BASE - 4_999);
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![done, live])
            .with_runs(vec![
                run("tsk_done", BASE - 10_000),
                run("tsk_live", BASE - 4_999),
            ]),
        120,
        40,
    );
    h.app.state.view = ViewId::Tasks;
    // Status → Priority → Title → Assignee → Goal → Started → Elapsed.
    for _ in 0..6 {
        h.key('o');
    }
    assert_eq!(
        h.app.state.task_sort.field,
        orch_tui::state::TaskSortField::Elapsed
    );
    h
}

#[test]
fn elapsed_sort_orders_tasks_by_the_displayed_span() {
    let mut h = crossing_tasks();
    let at = h.app.state.sort_clock_ms;
    // The terminal row's span is the one on screen: start to its last update,
    // frozen. The live row's runs to the sample.
    assert_eq!(h.app.state.task_elapsed_ms(0, at), Some(5_000));
    assert_eq!(h.app.state.task_elapsed_ms(1, at), Some(4_999));

    assert_eq!(sorted_task_ids(&h), ["tsk_live", "tsk_done"], "ascending");
    h.assert_text("ELAPSED*↑");
    h.key('O');
    assert_eq!(sorted_task_ids(&h), ["tsk_done", "tsk_live"], "descending");
    h.assert_text("ELAPSED*↓");

    // Two milliseconds on, the live span has passed the terminal one. The
    // terminal row did not move — it has no completion duration of its own,
    // only the frozen span — so re-sampling the sort flips the pair.
    h.advance(2);
    assert_eq!(h.app.state.task_elapsed_ms(0, h.now()), Some(5_000));
    assert_eq!(h.app.state.task_elapsed_ms(1, h.now()), Some(5_001));
    h.key('O');
    assert_eq!(h.app.state.sort_clock_ms, BASE + 2);
    assert_eq!(sorted_task_ids(&h), ["tsk_done", "tsk_live"], "ascending");
    h.key('O');
    assert_eq!(sorted_task_ids(&h), ["tsk_live", "tsk_done"], "descending");
}

#[test]
fn elapsed_sort_orders_goals_by_the_displayed_span() {
    let mut ended = make_goal("gol_done", "Ended", GoalStatus::Achieved);
    ended.created_at = iso(BASE - 10_000);
    ended.updated_at = Some(iso(BASE - 5_000));
    let mut live = make_goal("gol_live", "Running", GoalStatus::Active);
    live.created_at = iso(BASE - 4_999);
    let mut h = Harness::sized(FakeBackend::new().with_goals(vec![ended, live]), 120, 40);
    h.app.state.view = ViewId::Goals;
    // Status → Title → Progress → Assignee → Started → Elapsed.
    for _ in 0..5 {
        h.key('o');
    }
    assert_eq!(
        h.app.state.goal_sort.field,
        orch_tui::state::GoalSortField::Elapsed
    );

    assert_eq!(sorted_goal_ids(&h), ["gol_live", "gol_done"], "ascending");
    h.key('O');
    assert_eq!(sorted_goal_ids(&h), ["gol_done", "gol_live"], "descending");

    h.advance(2);
    h.key('O');
    assert_eq!(sorted_goal_ids(&h), ["gol_done", "gol_live"], "ascending");
}

// ── Unknown values sort last in either direction (D2) ──────────────────────
//
// The never-started (or unreadable) row sits in the middle of the source
// order, so neither "unknown first" nor "reverse the ascending list" can pass
// by accident.

/// Started oldest → newest is `tsk_old`, `tsk_new`; `tsk_never` has no run.
fn tasks_with_a_never_started_one() -> Harness {
    let mut h = Harness::sized(
        FakeBackend::new()
            .with_tasks(vec![
                make_task("tsk_old", "Old", TaskStatus::InProgress),
                make_task("tsk_never", "Never", TaskStatus::Todo),
                make_task("tsk_new", "New", TaskStatus::InProgress),
            ])
            .with_runs(vec![
                run("tsk_old", BASE - 9_000),
                run("tsk_new", BASE - 1_000),
            ]),
        120,
        40,
    );
    h.app.state.view = ViewId::Tasks;
    h.draw();
    assert_eq!(h.app.state.task_started_ms(1), None, "tsk_never never ran");
    h
}

fn sort_tasks(h: &mut Harness, field: orch_tui::state::TaskSortField, descending: bool) {
    h.app.state.task_sort = orch_tui::state::TaskSort { field, descending };
}

fn sort_goals(h: &mut Harness, field: orch_tui::state::GoalSortField, descending: bool) {
    h.app.state.goal_sort = orch_tui::state::GoalSort { field, descending };
}

#[test]
fn never_started_task_sorts_last_by_started_in_both_directions() {
    use orch_tui::state::TaskSortField::Started;
    let mut h = tasks_with_a_never_started_one();
    sort_tasks(&mut h, Started, false);
    assert_eq!(sorted_task_ids(&h), ["tsk_old", "tsk_new", "tsk_never"]);
    sort_tasks(&mut h, Started, true);
    assert_eq!(sorted_task_ids(&h), ["tsk_new", "tsk_old", "tsk_never"]);
}

#[test]
fn never_started_task_sorts_last_by_elapsed_in_both_directions() {
    use orch_tui::state::TaskSortField::Elapsed;
    let mut h = tasks_with_a_never_started_one();
    sort_tasks(&mut h, Elapsed, false);
    assert_eq!(sorted_task_ids(&h), ["tsk_new", "tsk_old", "tsk_never"]);
    sort_tasks(&mut h, Elapsed, true);
    assert_eq!(sorted_task_ids(&h), ["tsk_old", "tsk_new", "tsk_never"]);
}

/// Goals start at creation; `gol_bad`'s creation time does not parse, so its
/// Started and Elapsed are unknown.
fn goals_with_an_unknown_start() -> Harness {
    let mut old = make_goal("gol_old", "Old", GoalStatus::Active);
    old.created_at = iso(BASE - 9_000);
    let mut bad = make_goal("gol_bad", "Bad", GoalStatus::Active);
    bad.created_at = "not a timestamp".into();
    let mut new = make_goal("gol_new", "New", GoalStatus::Active);
    new.created_at = iso(BASE - 1_000);
    let mut h = Harness::sized(FakeBackend::new().with_goals(vec![old, bad, new]), 120, 40);
    h.app.state.view = ViewId::Goals;
    h.draw();
    assert_eq!(h.app.state.goal_started_ms(1), None, "gol_bad is unknown");
    h
}

#[test]
fn unknown_goal_start_sorts_last_by_started_in_both_directions() {
    use orch_tui::state::GoalSortField::Started;
    let mut h = goals_with_an_unknown_start();
    sort_goals(&mut h, Started, false);
    assert_eq!(sorted_goal_ids(&h), ["gol_old", "gol_new", "gol_bad"]);
    sort_goals(&mut h, Started, true);
    assert_eq!(sorted_goal_ids(&h), ["gol_new", "gol_old", "gol_bad"]);
}

#[test]
fn unknown_goal_start_sorts_last_by_elapsed_in_both_directions() {
    use orch_tui::state::GoalSortField::Elapsed;
    let mut h = goals_with_an_unknown_start();
    sort_goals(&mut h, Elapsed, false);
    assert_eq!(sorted_goal_ids(&h), ["gol_new", "gol_old", "gol_bad"]);
    sort_goals(&mut h, Elapsed, true);
    assert_eq!(sorted_goal_ids(&h), ["gol_old", "gol_new", "gol_bad"]);
}
