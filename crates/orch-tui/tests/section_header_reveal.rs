//! A section header stays in view when the selection climbs back to the first
//! row of its section (goal `goal_vQ7IkUT`, plan.md R1-R3).
//!
//! The selection is driven past the bottom of a short list, so the headers
//! scroll out above, then back up one row at a time. Every time the selection
//! lands on the first row of a section, the screen line directly above it
//! must be that section's header, not the table's column header.
//!
//! Only the public API and the rendered frame are read. Every case runs on
//! the fake backend with a held clock and touches no home directory, network
//! or AI CLI.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_goal, make_task};
use orch_tui::state::{AgentGroupBy, ViewId};
use ratatui::crossterm::event::KeyCode;

/// The size the bug shows at: a list pane a few rows high.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 15;

/// The screen line holding `needle`, top first.
#[track_caller]
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{text}"))
}

/// Drives the selection down `steps` rows, then back up as many, and at every
/// upward step where the selection is the first of a section checks that the
/// line above it on screen names that section.
///
/// `first_of` maps a selected row's label to the header text expected above
/// it, `None` for rows that are not first in their section.
#[track_caller]
fn assert_headers_on_the_way_up(
    h: &mut Harness,
    steps: usize,
    selected: impl Fn(&Harness) -> String,
    first_of: impl Fn(&str) -> Option<&'static str>,
) {
    h.draw();
    for _ in 0..steps {
        h.press(KeyCode::Down).draw();
    }
    let bottom = selected(h);
    assert!(
        first_of(&bottom).is_none(),
        "the walk must end mid-section, below a scrolled-out header"
    );
    let mut checked = 0;
    for _ in 0..steps {
        h.press(KeyCode::Up).draw();
        let label = selected(h);
        let Some(header) = first_of(&label) else {
            continue;
        };
        let text = h.text();
        let at = line_of(&text, &label);
        let above = text.lines().nth(at.saturating_sub(1)).unwrap_or_default();
        assert!(
            at > 0 && above.contains(header),
            "{label} is first under {header:?}, but the line above it is {above:?}:\n{text}"
        );
        checked += 1;
    }
    assert!(checked >= 3, "only {checked} section starts were checked");
}

/// Three sections of three agents under Status grouping: ACTIVE `Agx-00..02`,
/// IDLE `Agx-03..05`, ERROR `Agx-06..08`.
fn agents_harness() -> Harness {
    let statuses = [AgentStatus::Running, AgentStatus::Idle, AgentStatus::Error];
    let agents = (0..9)
        .map(|i| make_agent(&format!("a{i}"), &format!("Agx-{i:02}"), statuses[i / 3]))
        .collect();
    let mut h = Harness::sized(FakeBackend::new().with_agents(agents), WIDTH, HEIGHT);
    h.app.state.view = ViewId::Agents;
    h.app.state.set_agent_group_by(AgentGroupBy::Status);
    h
}

#[test]
fn scrolling_back_up_shows_each_agent_section_header_above_its_first_agent() {
    let mut h = agents_harness();
    assert_headers_on_the_way_up(
        &mut h,
        8,
        |h| h.app.state.selected_agent().expect("an agent").name.clone(),
        |name| match name {
            "Agx-00" => Some("ACTIVE"),
            "Agx-03" => Some("IDLE"),
            "Agx-06" => Some("ERROR"),
            _ => None,
        },
    );
}

#[test]
fn a_refresh_keeps_the_section_header_above_a_selected_first_agent() {
    let mut h = agents_harness();
    h.draw();
    for _ in 0..8 {
        h.press(KeyCode::Down).draw();
    }
    for _ in 0..2 {
        h.press(KeyCode::Up).draw();
    }
    assert_eq!(h.app.state.selected_agent().unwrap().name, "Agx-06");
    // Whatever left the ERROR header cut off above the viewport (rows 0-7 are
    // the three sections up to Agx-05, the header is row 8, Agx-06 row 9), a
    // refresh brings it back.
    h.app.state.agent_scroll = 9;
    h.app.refresh_all(true);
    h.draw();
    let text = h.text();
    let at = line_of(&text, "Agx-06");
    assert!(
        text.lines().nth(at - 1).unwrap().contains("ERROR"),
        "the ERROR header must sit above Agx-06:\n{text}"
    );
}

#[test]
fn scrolling_back_up_shows_each_goal_header_above_its_first_task() {
    // Goals `Alpha` and `Beta` with three tasks each, then three ungrouped.
    let goals = vec![
        make_goal("goal_a", "Alpha", GoalStatus::Active),
        make_goal("goal_b", "Beta", GoalStatus::Active),
    ];
    let tasks = (0..9)
        .map(|i| {
            let mut t = make_task(
                &format!("tsk_{i}"),
                &format!("Tsk-{i:02}"),
                TaskStatus::Todo,
            );
            t.goal_id = match i / 3 {
                0 => Some("goal_a".into()),
                1 => Some("goal_b".into()),
                _ => None,
            };
            t
        })
        .collect();
    let backend = FakeBackend::new().with_goals(goals).with_tasks(tasks);
    let mut h = Harness::sized(backend, WIDTH, HEIGHT);
    h.app.state.view = ViewId::Tasks;
    h.app.state.group_by_goal = true;
    assert_headers_on_the_way_up(
        &mut h,
        8,
        |h| h.app.state.selected_task().expect("a task").title.clone(),
        |title| match title {
            "Tsk-00" => Some("ALPHA"),
            "Tsk-03" => Some("BETA"),
            "Tsk-06" => Some("UNGROUPED"),
            _ => None,
        },
    );
}
