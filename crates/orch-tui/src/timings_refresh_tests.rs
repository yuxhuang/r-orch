//! Deterministic bounds for push-triggered whole-history timing reads.

use super::App;
use crate::backend::{FakeBackend, Observation, ObservedStore, Read};
use crate::clock::{Clock, TestClock};
use crate::messages::REFRESH_DEBOUNCE_MS;
use crate::refresh::TIMINGS_REFRESH_INTERVAL_SECS;

fn app() -> App<FakeBackend, TestClock> {
    let mut backend = FakeBackend::new();
    backend.commit_seq = Some(0);
    let mut app = App::new(backend, TestClock::new(0));
    app.backend.reads.clear();
    app
}

fn push(app: &mut App<FakeBackend, TestClock>, store: ObservedStore, seq: u64) {
    app.backend.commit_seq = Some(seq);
    app.backend.pending_observations.push(Observation::Changed {
        store,
        id: None,
        commit_seq: seq,
    });
    app.tick();
}

fn advance(app: &mut App<FakeBackend, TestClock>, ms: i64) {
    app.clock.advance_ms(ms);
    app.tick();
}

#[test]
fn streaming_runs_have_bounded_timing_reads() {
    let mut app = app();
    let interval_ms = TIMINGS_REFRESH_INTERVAL_SECS * 1_000;
    let notifications = 100;
    let duration_ms = notifications * REFRESH_DEBOUNCE_MS;
    let mut read_times = Vec::new();
    for seq in 1..=notifications {
        let before = app.backend.read_count(Read::Timings);
        push(&mut app, ObservedStore::Runs, seq as u64);
        advance(&mut app, REFRESH_DEBOUNCE_MS);
        if app.backend.read_count(Read::Timings) > before {
            read_times.push(app.clock.now_ms());
        }
    }
    // Include the trailing read after the final notification.
    advance(&mut app, interval_ms);
    let reads = app.backend.read_count(Read::Timings);
    let bound = (duration_ms + interval_ms - 1) / interval_ms + 1;
    assert!(
        reads as i64 <= bound,
        "{notifications} Runs notifications over {duration_ms} ms caused {reads} timing reads; bound {bound}"
    );
    assert!(
        read_times
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= interval_ms)
    );
    assert_eq!(app.state.timings.sampled_at_ms, app.clock.now_ms());
    eprintln!(
        "{notifications} notifications over {duration_ms} ms: {reads} timing reads (bound {bound})"
    );
}

#[test]
fn the_final_run_change_gets_one_trailing_read_at_the_interval() {
    use crate::backend::make_task;
    use orch_domain::{Run, RunStatus, TaskStatus};

    let mut app = app();
    app.backend.tasks = vec![make_task("task", "Live task", TaskStatus::InProgress)];
    app.refresh_all(false);
    app.backend.reads.clear();
    advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 1_000);
    push(&mut app, ObservedStore::Runs, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    let first_read_at = app.clock.now_ms();
    assert_eq!(app.backend.read_count(Read::Timings), 1);
    assert_eq!(app.state.timings.tasks["task"].started_at_ms, None);

    advance(&mut app, REFRESH_DEBOUNCE_MS);
    app.backend.runs.push(Run {
        id: "run".into(),
        task_id: "task".into(),
        agent_id: "agent".into(),
        attempt: 1,
        status: RunStatus::Running,
        started_at: "1970-01-01T00:00:01.000Z".into(),
        finished_at: None,
        workspace_path: String::new(),
        prompt: String::new(),
        pid: None,
        error: None,
        error_kind: None,
        tokens: None,
    });
    push(&mut app, ObservedStore::Runs, 2);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Timings), 1, "deferred");
    let deadline = first_read_at + TIMINGS_REFRESH_INTERVAL_SECS * 1_000;
    app.clock.set_ms(deadline - 1);
    app.tick();
    assert_eq!(app.backend.read_count(Read::Timings), 1, "not early");
    advance(&mut app, 1);
    assert_eq!(app.backend.read_count(Read::Timings), 2);
    assert_eq!(app.state.timings.tasks["task"].started_at_ms, Some(1_000));
    advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
    assert_eq!(
        app.backend.read_count(Read::Timings),
        2,
        "one trailing read"
    );
}

#[test]
fn semantic_changes_keep_their_debounce_during_a_runs_burst() {
    for (store, read) in [
        (ObservedStore::Tasks, Read::Tasks),
        (ObservedStore::Goals, Read::Goals),
        (ObservedStore::State, Read::State),
    ] {
        let mut app = app();
        push(&mut app, ObservedStore::Runs, 1);
        advance(&mut app, REFRESH_DEBOUNCE_MS);
        push(&mut app, ObservedStore::Runs, 2);
        advance(&mut app, REFRESH_DEBOUNCE_MS);
        app.backend.reads.clear();
        app.backend.dispatch_on = Some(true);
        push(&mut app, store, 3);
        advance(&mut app, REFRESH_DEBOUNCE_MS - 1);
        assert!(app.backend.reads.is_empty(), "{store:?} debounce");
        advance(&mut app, 1);
        assert_eq!(app.backend.read_count(read), 1, "{store:?}");
        assert_eq!(app.backend.read_count(Read::Timings), 1, "{store:?}");
        if store == ObservedStore::State {
            assert!(app.state.watch_active);
            assert_eq!(app.backend.read_count(Read::Dispatch), 1);
        }
        advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
        assert_eq!(
            app.backend.read_count(Read::Timings),
            1,
            "already satisfied"
        );
    }
}

#[test]
fn unrelated_view_refreshes_do_not_bypass_the_runs_limit() {
    let mut app = app();
    push(&mut app, ObservedStore::Runs, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    push(&mut app, ObservedStore::Runs, 2);
    push(&mut app, ObservedStore::Agents, 3);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Agents), 1);
    assert_eq!(app.backend.read_count(Read::Timings), 1);
    app.clock
        .set_ms(REFRESH_DEBOUNCE_MS + TIMINGS_REFRESH_INTERVAL_SECS * 1_000);
    app.tick();
    assert_eq!(app.backend.read_count(Read::Timings), 2);
}

#[test]
fn time_without_notifications_never_reads_timings() {
    let mut app = app();
    for _ in 0..100 {
        advance(&mut app, REFRESH_DEBOUNCE_MS);
    }
    assert!(app.backend.reads.is_empty());
}

#[test]
fn a_failed_timing_read_is_limited_and_only_a_change_retries_it() {
    let mut app = app();
    app.backend.timings_error = Some("timings unavailable".into());
    push(&mut app, ObservedStore::Runs, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(
        app.state.timing_error.as_deref(),
        Some("timings unavailable")
    );
    push(&mut app, ObservedStore::Runs, 2);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Timings), 1);
    app.clock
        .set_ms(REFRESH_DEBOUNCE_MS + TIMINGS_REFRESH_INTERVAL_SECS * 1_000);
    app.tick();
    assert_eq!(app.backend.read_count(Read::Timings), 2);
    advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
    assert_eq!(app.backend.read_count(Read::Timings), 2, "no error polling");
    app.backend.timings_error = None;
    push(&mut app, ObservedStore::Runs, 3);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Timings), 3);
    assert_eq!(app.state.timing_error, None);
}

#[test]
fn a_resync_satisfies_the_pending_runs_read_without_an_extra_timer_read() {
    let mut app = app();
    push(&mut app, ObservedStore::Runs, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    push(&mut app, ObservedStore::Runs, 2);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    app.backend.pending_observations.push(Observation::Resynced);
    app.tick();
    assert_eq!(app.backend.read_count(Read::Timings), 2, "immediate resync");
    advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
    assert_eq!(app.backend.read_count(Read::Timings), 2);
    push(&mut app, ObservedStore::Runs, 2);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Timings), 2, "ordinal retired");
}
