//! Edge-case gap tests for stack X of the TUI QoL round 2: the detail rail
//! and Activity sections (R1, R2, R12), the Overview block (R6) and the
//! wizard agent picker (R7, R8).
//!
//! These cover what the stack's own tests leave open: sections that are
//! empty or conditional, the 500-message cap under a paused selection, wide
//! and very long text at the smallest and a very large size, and the picker
//! at its limits (no agent, one agent, twins, unicode, digits past the end,
//! every filter under every order, pointer identity). Everything runs
//! in-process on the fake backend and the test clock: no tmux, no home
//! directory, no AI CLI, no sleeps.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, TaskProof, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_goal, make_task};
use orch_tui::hit::{RowPart, ZoneKind};
use orch_tui::messages::{Message, MsgType, PendingMessage};
use orch_tui::state::{AgentSection, DetailPanel, Focus, GoalSection, TaskSection, ViewId};
use orch_tui::theme::Token;
use orch_tui::wizard::configs;
use orch_tui::wizard::{Choice, Wizard, WizardKind, WizardOutcome};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ── Shared helpers ──────────────────────────────────────────────────────────

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

fn type_into(wizard: &mut Wizard, text: &str) {
    for ch in text.chars() {
        wizard.handle_key(key(KeyCode::Char(ch)), 0);
    }
}

fn labels(choices: &[Choice]) -> Vec<String> {
    choices.iter().map(|c| c.label.clone()).collect()
}

fn values(choices: &[Choice]) -> Vec<String> {
    choices.iter().map(|c| c.value.clone()).collect()
}

/// The detail sections in focus-ring order (the list and feed panes removed).
fn detail_ring(harness: &mut Harness) -> Vec<Focus> {
    harness.draw();
    harness
        .app
        .state
        .focus_ring()
        .into_iter()
        .filter(|f| !matches!(f, Focus::MainList | Focus::Activity | Focus::TextViewer))
        .collect()
}

/// The rail as drawn, left to right.
fn rail_on_screen(harness: &mut Harness) -> Vec<Focus> {
    harness.draw();
    let mut rail: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|zone| match zone.kind {
            ZoneKind::DetailSection(section) if zone.rect.height == 1 => {
                Some((zone.rect.x, section))
            }
            _ => None,
        })
        .collect();
    rail.sort_by_key(|(x, _)| *x);
    rail.into_iter().map(|(_, section)| section).collect()
}

fn message(id: u64, ts: i64, task: Option<&str>, agent: Option<&str>) -> Message {
    Message::new(id, ts, &format!("m{id}"), Token::White, MsgType::Info)
        .with_task(task.map(Into::into))
        .with_agent(agent.map(Into::into))
}

fn pending(ts: i64, task: Option<&str>, agent: Option<&str>) -> PendingMessage {
    PendingMessage {
        text: format!("p{ts}"),
        color: Token::White,
        msg_type: MsgType::Info,
        agent_id: agent.map(Into::into),
        task_id: task.map(Into::into),
        detail: None,
        ts_ms: Some(ts),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Rail order with every conditional section present or absent (R2)
// ═══════════════════════════════════════════════════════════════════════════

/// A task with every optional part: attachments, a result, changed files.
fn full_task() -> orch_domain::Task {
    let mut task = make_task("tsk_1", "Everything", TaskStatus::Done);
    task.attachments = Some(vec!["/tmp/shot.png".into()]);
    task.proof = Some(TaskProof {
        agent_summary: Some("Did it".into()),
        files_changed: vec!["src/lib.rs".into()],
        ..TaskProof::default()
    });
    task
}

fn task_harness(task: orch_domain::Task, width: u16, height: u16, with_message: bool) -> Harness {
    let mut harness = Harness::sized(FakeBackend::new().with_tasks(vec![task]), width, height);
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.detail = DetailPanel::Entity;
    if with_message {
        harness.app.state.messages = vec![message(1, 900_000, Some("tsk_1"), None)];
    }
    harness
}

#[test]
fn task_rail_with_every_conditional_section_follows_the_fixed_order() {
    let mut harness = task_harness(full_task(), 200, 50, true);
    assert_eq!(
        detail_ring(&mut harness),
        [
            Focus::TaskDetail(TaskSection::Overview),
            Focus::TaskDetail(TaskSection::Attachments),
            Focus::TaskDetail(TaskSection::Description),
            Focus::TaskDetail(TaskSection::Result),
            Focus::TaskDetail(TaskSection::Files),
            Focus::TaskDetail(TaskSection::Activity),
        ]
    );
}

#[test]
fn task_rail_with_no_conditional_section_is_overview_then_description() {
    let mut harness = task_harness(
        make_task("tsk_1", "Plain", TaskStatus::Todo),
        200,
        50,
        false,
    );
    assert_eq!(
        detail_ring(&mut harness),
        [
            Focus::TaskDetail(TaskSection::Overview),
            Focus::TaskDetail(TaskSection::Description),
        ]
    );
}

/// Each conditional section on its own lands in its fixed slot; none moves
/// another.
#[test]
fn task_rail_places_each_conditional_section_alone_in_its_slot() {
    let attachments = |t: &mut orch_domain::Task| t.attachments = Some(vec!["/tmp/a.png".into()]);
    let result = |t: &mut orch_domain::Task| {
        t.proof = Some(TaskProof {
            agent_summary: Some("ok".into()),
            ..TaskProof::default()
        })
    };
    let files = |t: &mut orch_domain::Task| {
        t.proof = Some(TaskProof {
            files_changed: vec!["a.rs".into()],
            ..TaskProof::default()
        })
    };
    type Mutate<'a> = &'a dyn Fn(&mut orch_domain::Task);
    let overview = Focus::TaskDetail(TaskSection::Overview);
    let description = Focus::TaskDetail(TaskSection::Description);
    let cases: [(Mutate, bool, Vec<Focus>); 4] = [
        (
            &attachments,
            false,
            vec![
                overview,
                Focus::TaskDetail(TaskSection::Attachments),
                description,
            ],
        ),
        (
            &result,
            false,
            vec![
                overview,
                description,
                Focus::TaskDetail(TaskSection::Result),
            ],
        ),
        (
            &files,
            false,
            vec![overview, description, Focus::TaskDetail(TaskSection::Files)],
        ),
        (
            &|_| {},
            true,
            vec![
                overview,
                description,
                Focus::TaskDetail(TaskSection::Activity),
            ],
        ),
    ];
    for (n, (mutate, message, expected)) in cases.into_iter().enumerate() {
        let mut task = make_task("tsk_1", "One extra", TaskStatus::Done);
        mutate(&mut task);
        let mut harness = task_harness(task, 200, 50, message);
        assert_eq!(detail_ring(&mut harness), expected, "case {n}");
    }
}

#[test]
fn task_rail_on_screen_matches_the_focus_ring_with_every_section_present() {
    let mut harness = task_harness(full_task(), 200, 50, true);
    let ring = detail_ring(&mut harness);
    harness.app.state.set_focus(ring[0]);
    assert_eq!(rail_on_screen(&mut harness), ring);
}

#[test]
fn task_activity_joins_the_rail_last_when_its_first_message_arrives() {
    let mut harness = task_harness(full_task(), 200, 50, false);
    let before = detail_ring(&mut harness);
    harness
        .app
        .state
        .add_message(pending(5, Some("tsk_1"), None), 5, 0);
    let after = detail_ring(&mut harness);
    let mut expected = before;
    expected.push(Focus::TaskDetail(TaskSection::Activity));
    assert_eq!(after, expected);
}

#[test]
fn goal_rail_places_progress_before_tasks_and_activity_last_in_every_combination() {
    for (progress, linked) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut member = make_task("tsk_1", "Member", TaskStatus::Todo);
        member.goal_id = Some("g1".into());
        let backend = FakeBackend::new()
            .with_goals(vec![make_goal("g1", "Goal", GoalStatus::Active)])
            .with_tasks(if linked { vec![member] } else { vec![] });
        let mut harness = Harness::sized(backend, 200, 50);
        harness.app.state.view = ViewId::Goals;
        harness.app.state.detail = DetailPanel::Entity;
        harness.app.state.goal_progress_report = progress.then(|| "report".to_string());
        let mut expected = vec![
            Focus::GoalDetail(GoalSection::Overview),
            Focus::GoalDetail(GoalSection::Description),
        ];
        if progress {
            expected.push(Focus::GoalDetail(GoalSection::Progress));
        }
        if linked {
            expected.push(Focus::GoalDetail(GoalSection::Tasks));
        }
        expected.push(Focus::GoalDetail(GoalSection::Activity));
        assert_eq!(
            detail_ring(&mut harness),
            expected,
            "progress={progress} linked={linked}"
        );
    }
}

#[test]
fn agent_rail_puts_error_between_overview_and_role_only_when_the_agent_failed() {
    for failed in [false, true] {
        let mut agent = make_agent("a1", "alpha", AgentStatus::Idle);
        if failed {
            agent.last_error = Some(orch_domain::AgentLastError {
                message: "boom".into(),
                kind: "crash".into(),
                timestamp: "2026-10-19T00:00:00.000Z".into(),
            });
        }
        let mut harness = Harness::sized(FakeBackend::new().with_agents(vec![agent]), 200, 50);
        harness.app.state.view = ViewId::Agents;
        harness.app.state.detail = DetailPanel::Entity;
        let mut expected = vec![Focus::AgentDetail(AgentSection::Overview)];
        if failed {
            expected.push(Focus::AgentDetail(AgentSection::Error));
        }
        expected.extend([
            Focus::AgentDetail(AgentSection::Role),
            Focus::AgentDetail(AgentSection::Activity),
        ]);
        assert_eq!(detail_ring(&mut harness), expected, "failed={failed}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Empty Activity sections: reachable, and nothing to navigate (R12)
// ═══════════════════════════════════════════════════════════════════════════

/// An empty goal and an agent with no messages, details open on Activity.
fn empty_activity_harness(view: ViewId, width: u16, height: u16) -> (Harness, Focus) {
    let backend = FakeBackend::new()
        .with_goals(vec![make_goal("g1", "Quiet goal", GoalStatus::Active)])
        .with_agents(vec![make_agent("a1", "quiet", AgentStatus::Idle)]);
    let mut harness = Harness::sized(backend, width, height);
    harness.app.state.messages.clear();
    harness.app.state.view = view;
    harness.app.state.detail = DetailPanel::Entity;
    let focus = match view {
        ViewId::Goals => Focus::GoalDetail(GoalSection::Activity),
        _ => Focus::AgentDetail(AgentSection::Activity),
    };
    harness.app.state.set_focus(focus);
    (harness, focus)
}

#[test]
fn empty_activity_says_no_activity_at_the_minimum_and_a_very_large_size() {
    for (width, height, maximized) in [(40, 14, true), (200, 50, false)] {
        for view in [ViewId::Goals, ViewId::Agents] {
            let (mut harness, _) = empty_activity_harness(view, width, height);
            harness.app.state.detail_maximized = maximized;
            harness.assert_text("No activity.");
        }
    }
}

#[test]
fn empty_activity_keeps_following_whatever_navigation_key_is_pressed() {
    for view in [ViewId::Goals, ViewId::Agents] {
        let (mut harness, focus) = empty_activity_harness(view, 120, 40);
        harness.draw();
        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::End,
            KeyCode::Home,
        ] {
            harness.press(code);
            let pane = harness.app.state.pane_key(focus);
            assert!(
                harness.app.state.detail_activity_following(&pane),
                "{view:?} after {code:?}"
            );
        }
    }
}

#[test]
fn empty_activity_enter_opens_no_viewer() {
    for view in [ViewId::Goals, ViewId::Agents] {
        let (mut harness, focus) = empty_activity_harness(view, 120, 40);
        harness.draw().press(KeyCode::Enter);
        assert!(harness.app.state.viewer.is_none(), "{view:?}");
        assert_eq!(harness.app.state.focus, focus, "{view:?}");
    }
}

#[test]
fn empty_activity_wheel_scrolling_neither_pauses_nor_panics() {
    for view in [ViewId::Goals, ViewId::Agents] {
        let (mut harness, focus) = empty_activity_harness(view, 120, 40);
        harness.draw();
        let (x, y) = harness.find_text("No activity.").expect("empty state");
        harness.wheel_down(x, y).wheel_up(x, y).wheel_down(x, y);
        let pane = harness.app.state.pane_key(focus);
        assert!(
            harness.app.state.detail_activity_following(&pane),
            "{view:?}"
        );
    }
}

#[test]
fn tab_walks_the_empty_activity_section_like_any_other_in_the_ring() {
    let (mut harness, focus) = empty_activity_harness(ViewId::Goals, 120, 40);
    harness.draw();
    harness.press(KeyCode::Tab);
    let after_tab = harness.app.state.focus;
    harness.press(KeyCode::BackTab);
    assert_eq!((after_tab != focus, harness.app.state.focus), (true, focus));
}

// ═══════════════════════════════════════════════════════════════════════════
// The 500-message cap under a paused selection (R12)
// ═══════════════════════════════════════════════════════════════════════════

/// 500 messages for the agent and its task, oldest first (ts = id).
fn full_feed_harness(view: ViewId) -> (Harness, Focus) {
    let mut task = make_task("tsk_1", "Busy", TaskStatus::InProgress);
    task.goal_id = Some("g1".into());
    let backend = FakeBackend::new()
        .with_tasks(vec![task])
        .with_goals(vec![make_goal("g1", "Busy goal", GoalStatus::Active)])
        .with_agents(vec![make_agent("a1", "busy", AgentStatus::Running)]);
    let mut harness = Harness::sized(backend, 120, 40);
    harness.app.state.messages.clear();
    for ts in 1..=orch_tui::messages::MAX_MESSAGES as i64 {
        harness
            .app
            .state
            .add_message(pending(ts * 10, Some("tsk_1"), Some("a1")), ts * 10, 0);
    }
    assert_eq!(
        harness.app.state.messages.len(),
        orch_tui::messages::MAX_MESSAGES
    );
    harness.app.state.view = view;
    harness.app.state.detail = DetailPanel::Entity;
    let focus = match view {
        ViewId::Goals => Focus::GoalDetail(GoalSection::Activity),
        ViewId::Agents => Focus::AgentDetail(AgentSection::Activity),
        _ => Focus::TaskDetail(TaskSection::Activity),
    };
    harness.app.state.set_focus(focus);
    harness.draw();
    (harness, focus)
}

/// The message id under the pane's selection, and the pane's row count.
fn selected_message(harness: &Harness, focus: Focus) -> (Option<u64>, usize) {
    let pane = harness.app.state.pane_key(focus);
    let rows = harness.app.state.detail_activity_rows(&pane);
    let selected = harness.app.state.detail_activity_scroll(&pane, 20).selected;
    (
        selected.map(|at| harness.app.state.messages[rows[at]].id),
        rows.len(),
    )
}

#[test]
fn paused_goal_and_agent_activity_keep_their_message_while_the_cap_evicts_older_rows() {
    for view in [ViewId::Goals, ViewId::Agents] {
        let (mut harness, focus) = full_feed_harness(view);
        for _ in 0..4 {
            harness.press(KeyCode::Down);
        }
        let (before, _) = selected_message(&harness, focus);
        for ts in 1..=7 {
            harness.app.state.add_message(
                pending(10_000 + ts, Some("tsk_1"), Some("a1")),
                10_000 + ts,
                0,
            );
        }
        let (after, rows) = selected_message(&harness, focus);
        assert_eq!(
            (after, rows),
            (before, orch_tui::messages::MAX_MESSAGES),
            "{view:?}"
        );
    }
}

#[test]
fn paused_newest_first_activity_on_the_oldest_row_stays_paused_on_the_new_oldest_when_evicted() {
    for view in [ViewId::Goals, ViewId::Agents] {
        let (mut harness, focus) = full_feed_harness(view);
        harness.press(KeyCode::End);
        let pane = harness.app.state.pane_key(focus);
        assert!(
            !harness.app.state.detail_activity_following(&pane),
            "{view:?}"
        );
        for ts in 1..=3 {
            harness.app.state.add_message(
                pending(10_000 + ts, Some("tsk_1"), Some("a1")),
                10_000 + ts,
                0,
            );
        }
        let rows = harness.app.state.detail_activity_rows(&pane).len();
        let scroll = harness.app.state.detail_activity_scroll(&pane, 20);
        assert_eq!(
            (
                scroll.selected,
                harness.app.state.detail_activity_following(&pane)
            ),
            (Some(rows - 1), false),
            "{view:?}: the evicted row's selection clamps to the oldest survivor"
        );
    }
}

#[test]
fn paused_task_activity_on_the_oldest_row_moves_to_the_new_top_when_evicted() {
    let (mut harness, focus) = full_feed_harness(ViewId::Tasks);
    harness.app.state.task_selected = 0;
    // Task Activity follows the bottom; Home pauses at the oldest row.
    harness.press(KeyCode::Home);
    let (oldest, _) = selected_message(&harness, focus);
    assert!(oldest.is_some());
    for ts in 1..=3 {
        harness.app.state.add_message(
            pending(10_000 + ts, Some("tsk_1"), Some("a1")),
            10_000 + ts,
            0,
        );
    }
    let pane = harness.app.state.pane_key(focus);
    let scroll = harness.app.state.detail_activity_scroll(&pane, 20);
    assert_eq!(
        (
            scroll.selected,
            harness.app.state.detail_activity_following(&pane)
        ),
        (Some(0), false)
    );
}

#[test]
fn feed_cap_holds_at_500_and_drops_the_oldest_first_for_a_goal_activity_pane() {
    let (mut harness, focus) = full_feed_harness(ViewId::Goals);
    harness
        .app
        .state
        .add_message(pending(99_999, Some("tsk_1"), Some("a1")), 99_999, 0);
    let pane = harness.app.state.pane_key(focus);
    let rows = harness.app.state.detail_activity_rows(&pane);
    let (newest, oldest) = (
        harness.app.state.messages[rows[0]].ts_ms,
        harness.app.state.messages[*rows.last().unwrap()].ts_ms,
    );
    assert_eq!((rows.len(), newest, oldest), (500, 99_999, 20));
}

// ═══════════════════════════════════════════════════════════════════════════
// Overview block: long, wide and emoji text; the pin (R6)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, Debug)]
enum Kind {
    Goal,
    Task,
    Agent,
}

const KINDS: [Kind; 3] = [Kind::Goal, Kind::Task, Kind::Agent];

fn overview_harness(kind: Kind, title: &str, width: u16, height: u16) -> (Harness, Focus) {
    let (backend, view, focus) = match kind {
        Kind::Goal => (
            FakeBackend::new().with_goals(vec![make_goal("g1", title, GoalStatus::Active)]),
            ViewId::Goals,
            Focus::GoalDetail(GoalSection::Overview),
        ),
        Kind::Task => (
            FakeBackend::new().with_tasks(vec![make_task("t1", title, TaskStatus::InProgress)]),
            ViewId::Tasks,
            Focus::TaskDetail(TaskSection::Overview),
        ),
        Kind::Agent => (
            FakeBackend::new().with_agents(vec![make_agent("a1", title, AgentStatus::Idle)]),
            ViewId::Agents,
            Focus::AgentDetail(AgentSection::Overview),
        ),
    };
    let mut harness = Harness::sized(backend, width, height);
    harness.app.state.view = view;
    harness.app.state.detail = DetailPanel::Entity;
    // At the minimum size only a maximized detail has rows to spare.
    harness.app.state.detail_maximized = width < 80;
    harness.app.state.set_focus(focus);
    (harness, focus)
}

fn tag(kind: Kind) -> &'static str {
    match kind {
        Kind::Goal => "GOAL",
        Kind::Task => "DETAIL",
        Kind::Agent => "AGENT",
    }
}

/// Every row of the focused Overview, top to bottom, scrolled into view.
fn overview_rows(harness: &mut Harness, focus: Focus) -> Vec<String> {
    let key = harness.app.state.pane_key(focus);
    let mut rows: Vec<Option<String>> = Vec::new();
    loop {
        harness.draw();
        let metrics = harness
            .hits()
            .pane_metrics(&key)
            .expect("overview geometry")
            .clone();
        rows.resize(metrics.display_rows, None);
        let offset = harness.app.state.pane_scroll(&key).offset;
        let buffer = harness.terminal.backend().buffer();
        for screen in 0..metrics.body.height {
            let Some(slot) = rows.get_mut(offset + screen as usize) else {
                break;
            };
            let y = metrics.body.y + screen;
            let mut row = String::new();
            let mut covered = 0;
            for x in metrics.body.x..metrics.body.right() {
                if covered > 0 {
                    covered -= 1;
                    row.push(' ');
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                covered = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
                row.push_str(symbol);
            }
            *slot = Some(row);
        }
        if offset + metrics.body.height as usize >= metrics.display_rows {
            break;
        }
        harness.press(KeyCode::Down);
        assert!(
            harness.app.state.pane_scroll(&key).offset > offset,
            "Down scrolls the Overview towards its last row"
        );
    }
    rows.into_iter()
        .map(|row| row.expect("every published row is reachable by scrolling"))
        .collect()
}

/// The text with whitespace and rule glyphs removed, so a wrapped value
/// reads as one run however its rows were broken.
fn squeezed(rows: &[String]) -> String {
    rows.concat()
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '━')
        .collect()
}

fn squeeze(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// A 300-character title made of words, and one with no break opportunity.
fn long_titles() -> [String; 2] {
    let words = "word ".repeat(60);
    [words.trim_end().to_owned(), "x".repeat(300)]
}

#[test]
fn overview_title_of_300_chars_is_fully_reachable_for_every_kind_at_200x50() {
    for title in long_titles() {
        assert_eq!(
            title.chars().count(),
            if title.contains(' ') { 299 } else { 300 }
        );
        for kind in KINDS {
            let (mut harness, focus) = overview_harness(kind, &title, 200, 50);
            let rows = overview_rows(&mut harness, focus);
            assert!(
                squeezed(&rows).contains(&squeeze(&format!("{}: {title}", tag(kind)))),
                "{kind:?}: full title in:\n{}",
                rows.join("\n")
            );
        }
    }
}

#[test]
fn overview_title_of_300_chars_is_fully_reachable_for_every_kind_at_40x14() {
    for title in long_titles() {
        for kind in KINDS {
            let (mut harness, focus) = overview_harness(kind, &title, 40, 14);
            let rows = overview_rows(&mut harness, focus);
            assert!(
                squeezed(&rows).contains(&squeeze(&format!("{}: {title}", tag(kind)))),
                "{kind:?}: full title in:\n{}",
                rows.join("\n")
            );
        }
    }
}

#[test]
fn overview_title_of_300_chars_is_never_clipped_with_an_ellipsis() {
    for (width, height) in [(200, 50), (40, 14)] {
        for kind in KINDS {
            let (mut harness, focus) = overview_harness(kind, &"y".repeat(300), width, height);
            let rows = overview_rows(&mut harness, focus);
            assert!(
                !rows.concat().contains('…'),
                "{kind:?} at {width}x{height}:\n{}",
                rows.join("\n")
            );
        }
    }
}

#[test]
fn overview_wide_glyph_title_is_fully_reachable_for_every_kind_at_200x50_and_40x14() {
    let title = "发布 🚀 版本二の界面改进 Ünïcödé مرحبا 🧪".repeat(8);
    for (width, height) in [(200, 50), (40, 14)] {
        for kind in KINDS {
            let (mut harness, focus) = overview_harness(kind, &title, width, height);
            let rows = overview_rows(&mut harness, focus);
            assert!(
                squeezed(&rows).contains(&squeeze(&format!("{}: {title}", tag(kind)))),
                "{kind:?} at {width}x{height}:\n{}",
                rows.join("\n")
            );
        }
    }
}

/// A wrapped emoji cluster is never torn across two rows: every family and
/// flag the title holds is drawn whole, in one row's cell.
#[test]
fn overview_wrapping_never_splits_a_zwj_emoji_cluster_across_rows() {
    let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
    let flag = "🏳\u{fe0f}\u{200d}🌈";
    let title = format!("{family}{flag}").repeat(30);
    for (width, height) in [(200, 50), (80, 24), (40, 14)] {
        let (mut harness, focus) = overview_harness(Kind::Task, &title, width, height);
        let rows = overview_rows(&mut harness, focus);
        let whole = |cluster: &str| {
            rows.iter()
                .map(|row| row.matches(cluster).count())
                .sum::<usize>()
        };
        assert_eq!(
            (whole(family), whole(flag)),
            (30, 30),
            "{width}x{height}:\n{}",
            rows.join("\n")
        );
    }
}

#[test]
fn overview_pin_is_two_rows_for_every_kind_with_a_300_char_title() {
    for (width, height) in [(200, 50), (80, 24)] {
        for kind in KINDS {
            let (mut harness, focus) = overview_harness(kind, &"p".repeat(300), width, height);
            // Focus another section so the Overview is the pin.
            let other = match focus {
                Focus::GoalDetail(_) => Focus::GoalDetail(GoalSection::Description),
                Focus::TaskDetail(_) => Focus::TaskDetail(TaskSection::Description),
                _ => Focus::AgentDetail(AgentSection::Role),
            };
            harness.app.state.set_focus(other);
            harness.draw();
            let pin = harness
                .hits()
                .zones()
                .iter()
                .find(|z| z.kind == ZoneKind::DetailSection(focus) && z.rect.height > 1)
                .map(|z| z.rect.height);
            assert_eq!(pin, Some(2), "{kind:?} at {width}x{height}");
        }
    }
}

#[test]
fn overview_pin_second_row_is_the_first_field_row_even_with_a_300_char_title() {
    for kind in KINDS {
        let (mut harness, focus) = overview_harness(kind, &"p".repeat(300), 200, 50);
        let other = match focus {
            Focus::GoalDetail(_) => Focus::GoalDetail(GoalSection::Description),
            Focus::TaskDetail(_) => Focus::TaskDetail(TaskSection::Description),
            _ => Focus::AgentDetail(AgentSection::Role),
        };
        harness.app.state.set_focus(other);
        harness.draw();
        let pin = harness
            .hits()
            .zones()
            .iter()
            .find(|z| z.kind == ZoneKind::DetailSection(focus) && z.rect.height > 1)
            .map(|z| z.rect)
            .expect("pin zone");
        let second = harness.line(pin.y + 1);
        let second = second[second
            .char_indices()
            .nth(pin.x as usize)
            .map_or(0, |(at, _)| at)..]
            .trim_start()
            .to_owned();
        assert!(
            second.starts_with("status "),
            "{kind:?}: the pin's second row is `status`, not a title continuation: {second:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Agent picker (R7, R8): limits
// ═══════════════════════════════════════════════════════════════════════════

fn agent(
    id: &str,
    name: &str,
    adapter: &str,
    role: &str,
    status: AgentStatus,
) -> orch_domain::Agent {
    let mut a = make_agent(id, name, status);
    a.adapter = adapter.into();
    a.role = (!role.is_empty()).then(|| role.to_string());
    a
}

fn roster() -> Vec<orch_domain::Agent> {
    vec![
        agent(
            "agt_a",
            "Alpha",
            "claude",
            "Frontend reviewer",
            AgentStatus::Running,
        ),
        agent(
            "agt_b",
            "beta",
            "codex",
            "Backend builder",
            AgentStatus::Idle,
        ),
        agent("agt_c", "Gamma", "shell", "", AgentStatus::Error),
        agent(
            "agt_d",
            "Delta",
            "claude",
            "Never listed",
            AgentStatus::Disabled,
        ),
        agent("agt_e", "alpha-2", "opencode", "Docs", AgentStatus::Idle),
    ]
}

fn step_wizard(kind: WizardKind, steps: Vec<orch_tui::wizard::WizardStep>, id: &str) -> Wizard {
    let step = steps.into_iter().find(|s| s.id == id).unwrap();
    Wizard::new(kind, "T", vec![step])
}

fn assignee_wizard(agents: &[orch_domain::Agent]) -> Wizard {
    step_wizard(
        WizardKind::Task,
        configs::task_wizard_steps(agents),
        "assignee",
    )
}

fn lead_wizard(agents: &[orch_domain::Agent]) -> Wizard {
    step_wizard(
        WizardKind::Team,
        configs::team_wizard_steps(agents, &[]),
        "lead",
    )
}

fn members_wizard(agents: &[orch_domain::Agent]) -> Wizard {
    step_wizard(
        WizardKind::Team,
        configs::team_wizard_steps(agents, &[]),
        "members",
    )
}

fn completes_with(wizard: &mut Wizard, code: KeyCode, step: &str) -> String {
    match wizard.handle_key(key(code), 0) {
        WizardOutcome::Complete(values) => values[step].clone(),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn new_task_wizard_skips_the_assignee_step_when_there_is_no_agent() {
    let wizard = configs::new_task_wizard(&[]);
    let active: Vec<&str> = wizard
        .active_steps()
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(active, ["title", "priority", "description"]);
}

#[test]
fn team_lead_picker_with_no_agent_lists_nothing_and_refuses_enter_and_tab() {
    let mut wizard = lead_wizard(&[]);
    assert!(wizard.options().is_empty());
    for confirm in [KeyCode::Enter, KeyCode::Tab] {
        let outcome = wizard.handle_key(key(confirm), 0);
        assert_eq!(
            (outcome, wizard.step_id().to_owned()),
            (WizardOutcome::Consumed, "lead".to_owned()),
            "{confirm:?}"
        );
    }
}

#[test]
fn team_lead_picker_with_no_agent_ignores_digits_ctrl_o_and_ctrl_u() {
    let mut wizard = lead_wizard(&[]);
    for event in [
        key(KeyCode::Char('1')),
        key(KeyCode::Char('9')),
        ctrl('o'),
        ctrl('u'),
        key(KeyCode::Down),
        key(KeyCode::End),
    ] {
        let outcome = wizard.handle_key(event, 0);
        assert!(
            !matches!(outcome, WizardOutcome::Complete(_)),
            "{event:?} completed an empty picker"
        );
    }
}

#[test]
fn team_lead_picker_with_no_agent_draws_at_the_minimum_size() {
    let mut harness = Harness::sized(FakeBackend::new(), 40, 14);
    harness.app.open_wizard(lead_wizard(&[]));
    harness.assert_text("Team lead");
}

/// Known defect, found by this suite: with no agent at all and nothing typed,
/// Enter on the team lead step reports "No agent matches the filter", though
/// no filter exists. The step stays open (correct); the wording misleads.
#[test]
fn team_lead_picker_with_no_agent_does_not_blame_a_filter() {
    let mut wizard = lead_wizard(&[]);
    wizard.handle_key(key(KeyCode::Enter), 0);
    let error = wizard.visible_error().unwrap_or_default().to_lowercase();
    assert!(!error.contains("filter"), "{error:?}");
}

#[test]
fn one_agent_picker_lists_the_sentinel_then_the_agent_and_two_picks_it() {
    let one = [agent("agt_x", "Solo", "codex", "", AgentStatus::Idle)];
    let mut wizard = assignee_wizard(&one);
    assert_eq!(labels(&wizard.options()), ["Auto-assign", "Solo"]);
    assert_eq!(
        completes_with(&mut wizard, KeyCode::Char('2'), "assignee"),
        "agt_x"
    );
}

#[test]
fn one_agent_picker_keeps_the_list_through_a_full_order_cycle() {
    let one = [agent("agt_x", "Solo", "codex", "", AgentStatus::Idle)];
    let mut wizard = assignee_wizard(&one);
    for _ in 0..4 {
        wizard.handle_key(ctrl('o'), 0);
        assert_eq!(labels(&wizard.options()), ["Auto-assign", "Solo"]);
    }
}

fn twins() -> Vec<orch_domain::Agent> {
    vec![
        agent("agt_1", "Twin", "claude", "", AgentStatus::Idle),
        agent("agt_2", "Twin", "codex", "", AgentStatus::Idle),
        agent("agt_3", "Other", "shell", "", AgentStatus::Idle),
    ]
}

#[test]
fn duplicate_names_are_both_listed_in_catalogue_order_when_filtered() {
    let mut wizard = assignee_wizard(&twins());
    type_into(&mut wizard, "twin");
    assert_eq!(values(&wizard.options()), ["", "agt_1", "agt_2"]);
}

#[test]
fn duplicate_names_pick_by_row_number_the_agent_shown_on_that_row() {
    let mut wizard = assignee_wizard(&twins());
    type_into(&mut wizard, "twin");
    assert_eq!(
        completes_with(&mut wizard, KeyCode::Char('3'), "assignee"),
        "agt_2"
    );
}

#[test]
fn duplicate_names_keep_catalogue_order_under_the_name_order() {
    let mut wizard = assignee_wizard(&twins());
    wizard.handle_key(ctrl('o'), 0);
    assert_eq!(values(&wizard.options()), ["", "agt_3", "agt_1", "agt_2"]);
}

#[test]
fn duplicate_names_tell_twins_apart_by_adapter_filter() {
    let mut wizard = assignee_wizard(&twins());
    type_into(&mut wizard, "codex");
    assert_eq!(values(&wizard.options()), ["", "agt_2"]);
}

#[test]
fn duplicate_names_keep_the_cursor_on_the_same_twin_through_every_order() {
    let mut wizard = assignee_wizard(&twins());
    wizard.handle_key(key(KeyCode::Down), 0);
    wizard.handle_key(key(KeyCode::Down), 0);
    let on = |w: &Wizard| w.options()[w.select_index()].value.clone();
    assert_eq!(on(&wizard), "agt_2");
    for _ in 0..4 {
        wizard.handle_key(ctrl('o'), 0);
        assert_eq!(on(&wizard), "agt_2");
    }
}

fn international() -> Vec<orch_domain::Agent> {
    vec![
        agent("agt_1", "Émile", "claude", "", AgentStatus::Idle),
        agent("agt_2", "ÉCOLE bot", "codex", "", AgentStatus::Idle),
        agent("agt_3", "研究员", "shell", "文档审阅", AgentStatus::Idle),
        agent("agt_4", "🤖 builder", "opencode", "", AgentStatus::Idle),
        agent("agt_5", "ДМИТРИЙ", "claude", "", AgentStatus::Idle),
    ]
}

#[test]
fn unicode_filter_is_case_insensitive_across_scripts() {
    for (typed, expected) in [
        ("émile", "agt_1"),
        ("école", "agt_2"),
        ("дмитрий", "agt_5"),
        ("ÉCOLE", "agt_2"),
    ] {
        let mut wizard = assignee_wizard(&international());
        type_into(&mut wizard, typed);
        assert_eq!(values(&wizard.options()), ["", expected], "{typed}");
    }
}

/// Known defect, found by this suite: `İ` lowercases to `i` plus a combining
/// dot (U+0307), so a plain `istanbul` query misses an agent named `İstanbul`.
#[test]
fn unicode_filter_matches_a_dotted_capital_i_name_with_a_plain_i_query() {
    let turkish = [agent("agt_1", "İstanbul", "codex", "", AgentStatus::Idle)];
    let mut wizard = assignee_wizard(&turkish);
    type_into(&mut wizard, "istanbul");
    assert_eq!(values(&wizard.options()), ["", "agt_1"]);
}

#[test]
fn unicode_filter_matches_cjk_emoji_and_role_text() {
    for (typed, expected) in [("研", "agt_3"), ("审阅", "agt_3"), ("🤖", "agt_4")] {
        let mut wizard = assignee_wizard(&international());
        type_into(&mut wizard, typed);
        assert_eq!(values(&wizard.options()), ["", expected], "{typed}");
    }
}

#[test]
fn picker_rows_with_wide_names_draw_at_the_minimum_size() {
    let mut harness = Harness::sized(FakeBackend::new(), 40, 14);
    harness.app.open_wizard(assignee_wizard(&international()));
    harness.assert_text("Auto-assign");
}

#[test]
fn picker_row_with_a_300_char_name_is_cut_with_an_ellipsis_inside_the_pane() {
    let long = [agent(
        "agt_1",
        &"N".repeat(300),
        "codex",
        "",
        AgentStatus::Idle,
    )];
    let mut harness = Harness::sized(FakeBackend::new(), 60, 20);
    harness.app.open_wizard(assignee_wizard(&long));
    let text = harness.text();
    let row = text
        .lines()
        .find(|line| line.contains("NNNN"))
        .expect("the agent's row");
    assert!(
        row.trim_end()
            .trim_end_matches('│')
            .trim_end()
            .ends_with('…'),
        "{row:?}"
    );
}

#[test]
fn digit_beyond_the_listed_rows_picks_nothing_and_keeps_the_step_open() {
    let mut wizard = assignee_wizard(&roster());
    assert_eq!(wizard.options().len(), 5);
    let outcome = wizard.handle_key(key(KeyCode::Char('9')), 0);
    assert_eq!(
        (outcome, wizard.step_id().to_owned()),
        (WizardOutcome::Consumed, "assignee".to_owned())
    );
}

#[test]
fn digit_beyond_a_narrowed_list_picks_nothing() {
    let mut wizard = assignee_wizard(&roster());
    type_into(&mut wizard, "codex");
    assert_eq!(wizard.options().len(), 2);
    let outcome = wizard.handle_key(key(KeyCode::Char('3')), 0);
    assert_eq!(
        (outcome, wizard.step_id().to_owned()),
        (WizardOutcome::Consumed, "assignee".to_owned())
    );
}

#[test]
fn digit_beyond_the_listed_rows_does_not_move_the_cursor() {
    let mut wizard = assignee_wizard(&roster());
    wizard.handle_key(key(KeyCode::Down), 0);
    wizard.handle_key(key(KeyCode::Char('9')), 0);
    assert_eq!(wizard.select_index(), 1);
}

#[test]
fn digits_on_the_members_step_are_filter_text_not_picks() {
    let mut wizard = members_wizard(&roster());
    wizard.handle_key(key(KeyCode::Char('2')), 0);
    assert_eq!(
        (
            wizard.picker_query().to_owned(),
            wizard.multi_selected().len()
        ),
        ("2".to_owned(), 0)
    );
}

#[test]
fn whitespace_only_filter_lists_every_agent_and_does_not_block_enter() {
    let mut wizard = assignee_wizard(&roster());
    type_into(&mut wizard, "   ");
    assert_eq!(wizard.options().len(), 5);
    assert!(!wizard.picker_blocked());
}

#[test]
fn typing_a_name_then_enter_submits_the_first_match_not_the_sentinel() {
    let mut wizard = assignee_wizard(&roster());
    type_into(&mut wizard, "gam");
    assert_eq!(
        completes_with(&mut wizard, KeyCode::Enter, "assignee"),
        "agt_c"
    );
}

#[test]
fn ctrl_u_clears_the_filter_and_keeps_the_cursor_on_the_same_agent() {
    let mut wizard = assignee_wizard(&roster());
    type_into(&mut wizard, "gam");
    wizard.handle_key(ctrl('u'), 0);
    let row = wizard.options()[wizard.select_index()].value.clone();
    assert_eq!(
        (wizard.picker_query().to_owned(), row),
        (String::new(), "agt_c".to_owned())
    );
}

#[test]
fn filter_text_is_capped_at_256_characters_without_splitting_a_character() {
    let mut wizard = assignee_wizard(&roster());
    wizard.insert_paste(&"🚀".repeat(300), 0);
    assert_eq!(wizard.picker_query().chars().count(), 256);
}

#[test]
fn typed_filter_text_stops_growing_at_256_characters() {
    let mut wizard = assignee_wizard(&roster());
    type_into(&mut wizard, &"q".repeat(400));
    assert_eq!(wizard.picker_query().chars().count(), 256);
}

#[test]
fn pasted_escape_sequences_and_line_breaks_never_reach_the_filter() {
    let mut wizard = assignee_wizard(&roster());
    wizard.insert_paste("a\u{1b}[2Jb\u{7}\r\nc\u{202e}d", 0);
    assert_eq!(wizard.picker_query(), "a[2Jb  cd");
}

#[test]
fn disabled_agents_are_never_listed_whatever_the_query_or_order() {
    for query in ["", "never", "delta", "claude"] {
        for presses in 0..4 {
            let mut wizard = assignee_wizard(&roster());
            type_into(&mut wizard, query);
            for _ in 0..presses {
                wizard.handle_key(ctrl('o'), 0);
            }
            assert!(
                !values(&wizard.options()).contains(&"agt_d".to_owned()),
                "query {query:?} after {presses} order presses"
            );
        }
    }
}

#[test]
fn order_cycle_returns_to_the_original_list_after_four_presses_on_every_agent_step() {
    let agents = roster();
    let goal = make_goal("g1", "G", GoalStatus::Active);
    let task = make_task("t1", "T", TaskStatus::Todo);
    let wizards: Vec<(&str, Wizard)> = vec![
        ("new task", assignee_wizard(&agents)),
        (
            "edit task",
            step_wizard(
                WizardKind::EditTask,
                configs::edit_task_wizard_steps(&task, &agents),
                "assignee",
            ),
        ),
        (
            "new goal",
            step_wizard(
                WizardKind::Goal,
                configs::goal_wizard_steps(&agents),
                "assignee",
            ),
        ),
        (
            "edit goal",
            step_wizard(
                WizardKind::EditGoal,
                configs::edit_goal_wizard_steps(&goal, &agents),
                "assignee",
            ),
        ),
        ("team lead", lead_wizard(&agents)),
        ("team members", members_wizard(&agents)),
    ];
    for (name, mut wizard) in wizards {
        let original = values(&wizard.options());
        for _ in 0..4 {
            wizard.handle_key(ctrl('o'), 0);
        }
        assert_eq!(values(&wizard.options()), original, "{name}");
    }
}

#[test]
fn sentinel_stays_first_for_every_filter_under_every_order_on_every_sentinel_step() {
    let agents = roster();
    let goal = make_goal("g1", "G", GoalStatus::Active);
    let task = make_task("t1", "T", TaskStatus::Todo);
    type Build = Box<dyn Fn() -> Wizard>;
    let builders: Vec<(&str, Build)> = vec![
        (
            "new task",
            Box::new({
                let a = agents.clone();
                move || assignee_wizard(&a)
            }),
        ),
        (
            "edit task",
            Box::new({
                let (a, t) = (agents.clone(), task.clone());
                move || {
                    step_wizard(
                        WizardKind::EditTask,
                        configs::edit_task_wizard_steps(&t, &a),
                        "assignee",
                    )
                }
            }),
        ),
        (
            "new goal",
            Box::new({
                let a = agents.clone();
                move || step_wizard(WizardKind::Goal, configs::goal_wizard_steps(&a), "assignee")
            }),
        ),
        (
            "edit goal",
            Box::new({
                let (a, g) = (agents.clone(), goal.clone());
                move || {
                    step_wizard(
                        WizardKind::EditGoal,
                        configs::edit_goal_wizard_steps(&g, &a),
                        "assignee",
                    )
                }
            }),
        ),
    ];
    for (name, build) in &builders {
        for query in ["", "a", "ALPHA", "zzz", "研", " ", "claude", "docs"] {
            for presses in 0..4 {
                let mut wizard = build();
                type_into(&mut wizard, query);
                for _ in 0..presses {
                    wizard.handle_key(ctrl('o'), 0);
                }
                let options = wizard.options();
                assert_eq!(
                    options.first().map(|c| c.value.as_str()),
                    Some(""),
                    "{name}: query {query:?}, {presses} order presses"
                );
            }
        }
    }
}

#[test]
fn the_team_lead_picker_never_offers_a_sentinel_under_any_filter() {
    for query in ["", "zzz", "alpha"] {
        let mut wizard = lead_wizard(&roster());
        type_into(&mut wizard, query);
        assert!(
            !values(&wizard.options()).iter().any(String::is_empty),
            "query {query:?}"
        );
    }
}

#[test]
fn an_agent_id_typed_as_text_is_not_a_filter_hit_and_submits_nothing() {
    let mut wizard = assignee_wizard(&roster());
    type_into(&mut wizard, "agt_c");
    let outcome = wizard.handle_key(key(KeyCode::Enter), 0);
    assert_eq!(
        (outcome, values(&wizard.options())),
        (WizardOutcome::Consumed, vec![String::new()])
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Agent picker: pointer identity after filter and order (DQ1 contract)
// ═══════════════════════════════════════════════════════════════════════════

fn picker_harness() -> Harness {
    let mut harness = Harness::sized(FakeBackend::new().with_agents(roster()), 100, 30);
    harness.app.open_wizard(configs::new_task_wizard(&roster()));
    harness.type_str("Ship it");
    harness.press(KeyCode::Enter); // title
    harness.press(KeyCode::Enter); // priority
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        "assignee"
    );
    harness.draw();
    harness
}

fn option_cell(harness: &Harness, index: usize, part: RowPart) -> (u16, u16) {
    let rect = harness
        .hits()
        .zones()
        .iter()
        .find(|z| z.kind == ZoneKind::WizardOption { index, part })
        .unwrap_or_else(|| panic!("no {part:?} zone for row {index}"))
        .rect;
    (rect.x, rect.y)
}

fn cursor_value(harness: &Harness) -> String {
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    wizard.options()[wizard.select_index()].value.clone()
}

#[test]
fn clicking_a_filtered_row_puts_the_cursor_on_that_agent_and_stays_on_the_step() {
    let mut harness = picker_harness();
    harness.type_str("a"); // Auto, Alpha, beta, Gamma, alpha-2 all hold an `a`
    harness.type_str("l"); // Auto, Alpha, alpha-2
    harness.draw();
    let (x, y) = option_cell(&harness, 2, RowPart::Label);
    harness.click(x + 6, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(
        (cursor_value(&harness), wizard.step_id().to_owned()),
        ("agt_e".to_owned(), "assignee".to_owned())
    );
}

#[test]
fn clicking_after_an_order_change_follows_the_displayed_row_not_the_catalogue_row() {
    let mut harness = picker_harness();
    harness.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL); // Name order
    harness.draw();
    // Name order: Auto-assign, Alpha, alpha-2, beta, Gamma
    let (x, y) = option_cell(&harness, 2, RowPart::Label);
    harness.click(x + 6, y);
    assert_eq!(cursor_value(&harness), "agt_e");
}

#[test]
fn a_single_click_never_changes_the_backend_or_the_answer() {
    let mut harness = picker_harness();
    harness.type_str("beta");
    harness.draw();
    let (x, y) = option_cell(&harness, 1, RowPart::Label);
    let before = harness.calls().len();
    harness.click(x + 6, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(
        (
            harness.calls().len() - before,
            wizard.values().contains_key("assignee")
        ),
        (0, false)
    );
}

#[test]
fn double_clicking_a_filtered_row_accepts_exactly_that_agent() {
    let mut harness = picker_harness();
    harness.type_str("codex");
    harness.draw();
    let (x, y) = option_cell(&harness, 1, RowPart::Label);
    harness.double_click(x + 6, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(
        (
            wizard.step_id().to_owned(),
            wizard.values().get("assignee").cloned()
        ),
        ("description".to_owned(), Some("agt_b".to_owned()))
    );
}

#[test]
fn a_press_on_one_row_released_on_another_activates_neither() {
    let mut harness = picker_harness();
    let (x, first) = option_cell(&harness, 1, RowPart::Label);
    let (_, third) = option_cell(&harness, 3, RowPart::Label);
    harness.press_at(x + 6, first).release_at(x + 6, third);
    assert_eq!(cursor_value(&harness), "");
}

#[test]
fn a_release_after_the_list_was_rearranged_activates_nothing() {
    // Press arms the `beta` row; the list is reordered and a frame is
    // drawn, as the runner does; `alpha-2` now sits under the pointer. The
    // release must not activate it.
    let mut harness = picker_harness();
    let (x, y) = option_cell(&harness, 2, RowPart::Label);
    harness.press_at(x + 6, y);
    // Not through `press`: a key event cancels an armed click on its own,
    // while a tick-driven change of the list does not.
    harness
        .app
        .state
        .wizard
        .as_mut()
        .unwrap()
        .handle_key(ctrl('o'), 0);
    harness.draw();
    assert_eq!(option_cell(&harness, 2, RowPart::Label), (x, y));
    harness.release_at(x + 6, y);
    assert_eq!(cursor_value(&harness), "");
}

#[test]
fn a_checkbox_click_on_a_filtered_members_row_ticks_that_agent_only() {
    let mut harness = Harness::sized(FakeBackend::new(), 100, 30);
    harness.app.open_wizard(members_wizard(&roster()));
    harness.type_str("alp"); // Alpha, alpha-2
    harness.draw();
    let (x, y) = option_cell(&harness, 1, RowPart::Checkbox);
    harness.click(x + 1, y);
    let picked: Vec<String> = harness
        .app
        .state
        .wizard
        .as_ref()
        .unwrap()
        .multi_selected()
        .iter()
        .cloned()
        .collect();
    assert_eq!(picked, ["agt_e"]);
}

#[test]
fn clicking_the_members_label_twice_toggles_without_advancing_and_keeps_hidden_ticks() {
    let mut harness = Harness::sized(FakeBackend::new(), 100, 30);
    harness.app.open_wizard(members_wizard(&roster()));
    harness.type_str("beta");
    harness.key(' '); // tick beta
    harness.press(KeyCode::Esc); // clear the filter
    harness.type_str("alp"); // beta is now hidden by the filter
    harness.draw();
    let (x, y) = option_cell(&harness, 0, RowPart::Label);
    harness.double_click(x + 8, y);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    let picked: Vec<String> = wizard.multi_selected().iter().cloned().collect();
    assert_eq!(
        (wizard.step_id().to_owned(), picked),
        (
            "members".to_owned(),
            vec!["agt_b".to_owned(), "agt_a".to_owned()]
        )
    );
}
