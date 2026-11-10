//! Push-driven refresh (PR 3, brief D3/D7): the dashboard reads state only
//! when the daemon says a store changed, or when it lost continuity on the
//! push. Time alone reads nothing, a dirty view reads exactly what it is
//! built from, and an invalidation the last read already observed is not
//! read again.

mod common;

use common::Harness;
use orch_domain::{GoalStatus, OrchestratorEvent, TaskStatus};
use orch_tui::ViewId;
use orch_tui::backend::{
    FakeBackend, Observation, ObservedStore, Read, make_goal, make_task, make_team,
};
use orch_tui::messages::REFRESH_DEBOUNCE_MS;
use ratatui::crossterm::event::KeyCode;

/// A later `updated_at`: the daemon moves it on every task write, and the
/// dashboard commits a list only when an id or `updated_at` changed.
const LATER: &str = "2026-10-13T12:05:00.000Z";

fn changed(store: ObservedStore, commit_seq: u64) -> Observation {
    Observation::Changed {
        store,
        id: None,
        commit_seq,
    }
}

fn changed_id(store: ObservedStore, id: &str, commit_seq: u64) -> Observation {
    Observation::Changed {
        store,
        id: Some(id.into()),
        commit_seq,
    }
}

fn backend() -> FakeBackend {
    let mut backend = FakeBackend::new()
        .with_tasks(vec![
            make_task("tsk_1", "Alpha", TaskStatus::Todo),
            make_task("tsk_2", "Bravo", TaskStatus::Todo),
            make_task("tsk_3", "Charlie", TaskStatus::Todo),
        ])
        .with_goals(vec![make_goal("goal_1", "Ship it", GoalStatus::Active)]);
    backend.commit_seq = Some(10);
    backend
}

/// A dashboard whose bootstrap reads are done and forgotten.
fn harness() -> Harness {
    let mut harness = Harness::with(backend());
    harness.app.backend.reads.clear();
    harness
}

fn push(harness: &mut Harness, observation: Observation) {
    harness.app.backend.pending_observations.push(observation);
}

fn reads(harness: &Harness) -> Vec<Read> {
    harness.app.backend.reads.clone()
}

fn task_ids(harness: &Harness) -> Vec<String> {
    harness
        .app
        .state
        .tasks
        .iter()
        .map(|t| t.id.clone())
        .collect()
}

const EVERY_VIEW: [Read; 9] = [
    Read::Tasks,
    Read::Agents,
    Read::Goals,
    Read::State,
    Read::Teams,
    Read::Timings,
    Read::PromptMode,
    Read::PeerStatus,
    Read::Dispatch,
];

// ── Time alone reads nothing ───────────────────────────────────────────────

#[test]
fn p001_the_clock_crossing_the_old_poll_interval_reads_no_state() {
    let mut harness = harness();
    // Frame by frame through six former 5 s poll windows (and two of the
    // 3 s observer windows each), as the draw loop would.
    for _ in 0..(30_000 / 120) {
        harness.advance(120);
    }
    assert_eq!(reads(&harness), Vec::<Read>::new(), "no unsolicited reads");
    // The animation still runs on the same frames.
    assert!(
        harness.app.state.tick >= 240,
        "tick {}",
        harness.app.state.tick
    );
}

#[test]
fn p002_a_store_change_on_the_backend_stays_invisible_without_a_push() {
    let mut harness = harness();
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_4", "Delta", TaskStatus::Todo));
    harness.advance(60_000);
    assert_eq!(task_ids(&harness).len(), 3, "nothing polled the store");
}

// ── A dirty view reads exactly what it is built from ───────────────────────

#[test]
fn p003_a_task_change_reads_the_tasks_and_their_timings_only() {
    let mut harness = harness();
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_4", "Delta", TaskStatus::Todo));
    harness.app.backend.commit_seq = Some(11);
    push(&mut harness, changed_id(ObservedStore::Tasks, "tsk_4", 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), vec![Read::Tasks, Read::Timings]);
    assert!(task_ids(&harness).contains(&"tsk_4".to_owned()));
}

#[test]
fn p004_each_store_reads_its_own_views() {
    let cases: [(ObservedStore, &[Read]); 10] = [
        (ObservedStore::Tasks, &[Read::Tasks, Read::Timings]),
        (ObservedStore::Goals, &[Read::Goals, Read::Timings]),
        (ObservedStore::Agents, &[Read::Agents]),
        (ObservedStore::Teams, &[Read::Teams]),
        (ObservedStore::Runs, &[Read::Timings]),
        (
            ObservedStore::State,
            &[Read::State, Read::Timings, Read::Dispatch],
        ),
        (ObservedStore::Config, &[Read::PromptMode, Read::PeerStatus]),
        (ObservedStore::Remote, &[Read::Agents, Read::PeerStatus]),
        (ObservedStore::Messages, &[]),
        (ObservedStore::Context, &[]),
    ];
    for (store, expected) in cases {
        let mut harness = harness();
        harness.app.backend.commit_seq = Some(11);
        push(&mut harness, changed(store, 11));
        harness.advance(0);
        harness.advance(REFRESH_DEBOUNCE_MS);
        let mut got = reads(&harness);
        got.sort_by_key(|r| format!("{r:?}"));
        let mut want = expected.to_vec();
        want.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(got, want, "{store:?}");
    }
}

#[test]
fn p005_a_burst_collapses_into_one_read_per_view() {
    let mut harness = harness();
    harness.app.backend.commit_seq = Some(40);
    for seq in 11..=40 {
        push(&mut harness, changed(ObservedStore::Tasks, seq));
        push(&mut harness, changed(ObservedStore::Goals, seq));
    }
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(harness.app.backend.read_count(Read::Tasks), 1);
    assert_eq!(harness.app.backend.read_count(Read::Goals), 1);
    assert_eq!(harness.app.backend.read_count(Read::Timings), 1);
    // And nothing more afterwards.
    harness.app.backend.reads.clear();
    harness.advance(10_000);
    assert_eq!(reads(&harness), Vec::<Read>::new());
}

// ── Ordinals: per view, never one shared high-water mark ───────────────────

#[test]
fn p006_a_change_the_last_read_already_observed_is_not_read_again() {
    let mut harness = harness();
    // The bootstrap read everything at commit 10.
    push(&mut harness, changed(ObservedStore::Tasks, 9));
    push(&mut harness, changed(ObservedStore::Tasks, 10));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), Vec::<Read>::new());
}

#[test]
fn p007_out_of_order_view_ordinals_never_drop_a_late_change() {
    let mut harness = harness();
    // Goals move on to commit 12 …
    harness.app.backend.commit_seq = Some(12);
    push(&mut harness, changed(ObservedStore::Goals, 12));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(harness.app.backend.read_count(Read::Goals), 1);
    harness.app.backend.reads.clear();

    // … then a task change from commit 11 arrives late. The goals view is
    // newer than 11, the tasks view (read at 10) is not: it must read.
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_4", "Delta", TaskStatus::Todo));
    push(&mut harness, changed(ObservedStore::Tasks, 11));
    // A goal change from 11 is already in the goals read at 12.
    push(&mut harness, changed(ObservedStore::Goals, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(harness.app.backend.read_count(Read::Tasks), 1);
    assert_eq!(harness.app.backend.read_count(Read::Goals), 0);
    assert!(task_ids(&harness).contains(&"tsk_4".to_owned()));
}

#[test]
fn p008_a_backend_without_ordinals_reads_on_every_change() {
    let mut harness = harness();
    harness.app.backend.commit_seq = None;
    // Re-read everything without ordinals.
    push(&mut harness, Observation::Resynced);
    harness.advance(0);
    harness.app.backend.reads.clear();
    push(&mut harness, changed(ObservedStore::Teams, 1));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), vec![Read::Teams]);
}

// ── Subscribe-before-read: nothing between them is lost ────────────────────

#[test]
fn p009_a_mutation_between_subscribe_and_the_bootstrap_read_is_not_lost() {
    // The push for commit 11 was queued before the dashboard's first read,
    // which still observed commit 10: the task it created is read after.
    let mut backend = backend();
    backend
        .pending_observations
        .push(changed(ObservedStore::Tasks, 11));
    let mut harness = Harness::with(backend);
    harness.app.backend.reads.clear();
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_4", "Delta", TaskStatus::Todo));
    harness.app.backend.commit_seq = Some(11);
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(harness.app.backend.read_count(Read::Tasks), 1);
    assert!(task_ids(&harness).contains(&"tsk_4".to_owned()));
}

#[test]
fn p010_a_mutation_the_bootstrap_already_saw_is_not_read_twice() {
    let mut backend = backend();
    backend.commit_seq = Some(11);
    backend
        .pending_observations
        .push(changed(ObservedStore::Tasks, 11));
    let mut harness = Harness::with(backend);
    harness.app.backend.reads.clear();
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), Vec::<Read>::new());
}

// ── Delete / reassign keep the selection by id ─────────────────────────────

#[test]
fn p011_a_pushed_delete_removes_the_row_and_keeps_the_selection_by_id() {
    let mut harness = harness();
    harness.app.state.view = ViewId::Tasks;
    harness.press(KeyCode::Down).press(KeyCode::Down);
    let selected = harness.app.state.selected_task().unwrap().id.clone();
    let deleted = task_ids(&harness)
        .into_iter()
        .find(|id| *id != selected)
        .unwrap();
    harness.app.backend.tasks.retain(|t| t.id != deleted);
    harness.app.backend.commit_seq = Some(11);
    push(&mut harness, changed_id(ObservedStore::Tasks, &deleted, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert!(!task_ids(&harness).contains(&deleted));
    assert_eq!(harness.app.state.selected_task().unwrap().id, selected);
}

#[test]
fn p012_a_pushed_reassignment_shows_the_new_assignee() {
    let mut harness = harness();
    harness.app.backend.tasks[0].assignee = Some("agt_new".into());
    harness.app.backend.tasks[0].updated_at = LATER.into();
    harness.app.backend.commit_seq = Some(11);
    push(&mut harness, changed_id(ObservedStore::Tasks, "tsk_1", 11));
    push(
        &mut harness,
        changed_id(ObservedStore::Agents, "agt_old", 11),
    );
    push(
        &mut harness,
        changed_id(ObservedStore::Agents, "agt_new", 11),
    );
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    let task = harness.app.state.task_by_id("tsk_1").unwrap();
    assert_eq!(task.assignee.as_deref(), Some("agt_new"));
    assert_eq!(harness.app.backend.read_count(Read::Agents), 1);
}

// ── Resync: everything stale, read again before it is shown ────────────────

#[test]
fn p013_a_resync_reads_every_view_in_the_same_frame() {
    let mut harness = harness();
    harness.app.backend.teams = vec![make_team("team_1", "Core", "agt_1", &["agt_1"])];
    push(&mut harness, Observation::Resynced);
    // No debounce: the frame after the resync is already synchronized.
    harness.advance(0);
    let mut got = reads(&harness);
    got.sort_by_key(|r| format!("{r:?}"));
    got.dedup();
    let mut want = EVERY_VIEW.to_vec();
    want.sort_by_key(|r| format!("{r:?}"));
    assert_eq!(got, want);
    assert_eq!(harness.app.state.teams.len(), 1, "teams are refetched too");
}

#[test]
fn p014_a_resync_keeps_the_selection_by_id_when_the_order_changes() {
    let mut harness = harness();
    harness.app.state.view = ViewId::Tasks;
    harness.press(KeyCode::Down);
    let selected = harness.app.state.selected_task().unwrap().id.clone();
    harness.app.backend.tasks.reverse();
    harness
        .app
        .backend
        .tasks
        .insert(0, make_task("tsk_0", "Zero", TaskStatus::Todo));
    push(&mut harness, Observation::Resynced);
    harness.advance(0);
    assert_eq!(harness.app.state.selected_task().unwrap().id, selected);
}

#[test]
fn p015_a_resync_during_a_refresh_is_honoured_on_the_next_frame() {
    let mut harness = harness();
    harness.app.backend.commit_seq = Some(11);
    // While the tasks read runs, the push loses continuity.
    harness
        .app
        .backend
        .on_read
        .push((Read::Tasks, Observation::Resynced));
    push(&mut harness, changed(ObservedStore::Tasks, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), vec![Read::Tasks, Read::Timings]);
    harness.app.backend.reads.clear();
    harness
        .app
        .backend
        .goals
        .push(make_goal("goal_2", "Later", GoalStatus::Active));
    harness.advance(0);
    for view in EVERY_VIEW {
        assert!(
            harness.app.backend.read_count(view) >= 1,
            "{view:?} was not read again after the resync"
        );
    }
    assert_eq!(harness.app.state.goals.len(), 2);
}

#[test]
fn p016_a_change_after_a_resync_is_still_read() {
    let mut harness = harness();
    push(&mut harness, Observation::Resynced);
    harness.advance(0);
    harness.app.backend.reads.clear();
    harness.app.backend.commit_seq = Some(11);
    push(&mut harness, changed(ObservedStore::Teams, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), vec![Read::Teams]);
}

// ── Bus and invalidation: one activity line, one read ──────────────────────

#[test]
fn p017_a_bus_event_and_its_invalidation_do_not_duplicate_activity_or_reads() {
    let mut harness = harness();
    harness.app.batch_ms = 0;
    let before = harness.app.state.messages.len();
    harness.app.backend.tasks[0].status = TaskStatus::Done;
    harness.app.backend.tasks[0].updated_at = LATER.into();
    harness.app.backend.commit_seq = Some(11);
    harness
        .app
        .backend
        .pending_events
        .push(OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::Done,
        });
    push(&mut harness, changed_id(ObservedStore::Tasks, "tsk_1", 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(
        harness.app.state.messages.len() - before,
        1,
        "one activity line for one event"
    );
    assert_eq!(harness.app.state.toasts.len(), 1);
    // The event itself schedules no read: the invalidation is the trigger.
    assert_eq!(reads(&harness), vec![Read::Tasks, Read::Timings]);
    assert_eq!(
        harness.app.state.task_by_id("tsk_1").unwrap().status,
        TaskStatus::Done
    );
}

#[test]
fn p018_a_bus_event_alone_reads_nothing() {
    let mut harness = harness();
    harness
        .app
        .backend
        .pending_events
        .push(OrchestratorEvent::TaskAssigned {
            task_id: "tsk_1".into(),
            agent_id: "agt_1".into(),
        });
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert_eq!(reads(&harness), Vec::<Read>::new());
}

// ── Header and error states ────────────────────────────────────────────────

#[test]
fn p019_a_state_change_brings_the_dispatch_chip_up_to_date() {
    let mut harness = harness();
    assert!(!harness.app.state.watch_active);
    // Another client switched dispatch on.
    harness.app.backend.dispatch_on = Some(true);
    harness.app.backend.commit_seq = Some(11);
    push(&mut harness, changed(ObservedStore::State, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert!(harness.app.state.watch_active);
    harness.assert_text("WATCHING");
}

#[test]
fn p020_a_failed_read_is_reported_once_and_read_again_on_the_next_change() {
    let mut harness = harness();
    harness.app.batch_ms = 0;
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_4", "Delta", TaskStatus::Todo));
    harness.app.backend.fail_read = Some((Read::Tasks, "daemon refused".into()));
    harness.app.backend.commit_seq = Some(11);
    push(&mut harness, changed(ObservedStore::Tasks, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    let errors: Vec<_> = harness
        .app
        .state
        .messages
        .iter()
        .filter(|m| m.text.contains("daemon refused"))
        .collect();
    assert_eq!(errors.len(), 1, "the failure is visible");
    assert!(errors[0].text.contains("Tasks"), "{}", errors[0].text);
    assert_eq!(task_ids(&harness).len(), 3, "the old rows stay");
    // No retry timer: time alone still reads nothing …
    harness.app.backend.reads.clear();
    harness.advance(30_000);
    assert_eq!(reads(&harness), Vec::<Read>::new());
    // … but the stale view is read on the next push, even one the failed
    // read's ordinal would otherwise cover.
    push(&mut harness, changed(ObservedStore::Tasks, 11));
    harness.advance(0);
    harness.advance(REFRESH_DEBOUNCE_MS);
    assert!(task_ids(&harness).contains(&"tsk_4".to_owned()));
}

#[test]
fn p021_the_launch_banner_no_longer_mentions_disk_polling() {
    // Even a context that still claims observer mode: no client polls.
    let mut backend = backend();
    backend.context.observer_mode = true;
    let mut harness = Harness::with(backend);
    harness.advance(100);
    let text = harness.text();
    assert!(!text.contains("disk polling"), "{text}");
    assert!(
        !harness
            .app
            .state
            .messages
            .iter()
            .any(|m| m.text.contains("disk polling"))
    );
}
