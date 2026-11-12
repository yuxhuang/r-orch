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
fn runs_only_notifications_have_bounded_timing_reads() {
    for spacing_ms in [1, REFRESH_DEBOUNCE_MS] {
        let mut app = app();
        let interval_ms = TIMINGS_REFRESH_INTERVAL_SECS * 1_000;
        let duration_ms = 15_000;
        let mut read_times = Vec::new();
        for seq in 1..=duration_ms / spacing_ms {
            let before = app.backend.read_count(Read::Timings);
            push(&mut app, ObservedStore::Runs, seq as u64);
            advance(&mut app, spacing_ms);
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
            "Runs every {spacing_ms} ms over {duration_ms} ms caused {reads} timing reads; bound {bound}"
        );
        assert!(
            read_times
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= interval_ms)
        );
        assert_eq!(app.state.timings.sampled_at_ms, app.clock.now_ms());
        eprintln!("Runs every {spacing_ms} ms: {reads} timing reads (bound {bound})");
    }
}

#[test]
fn streaming_runs_and_state_have_bounded_timing_reads_and_show_the_last_change() {
    use crate::backend::make_task;
    use orch_domain::TaskStatus;

    let mut backend = FakeBackend::new();
    backend.commit_seq = Some(0);
    backend.tasks = vec![make_task("task", "Live task", TaskStatus::InProgress)];
    let mut app = App::new(backend, TestClock::new(0));
    app.backend.reads.clear();
    let duration_ms = 30_000;
    let interval_ms = TIMINGS_REFRESH_INTERVAL_SECS * 1_000;
    let mut seq = 0;
    let mut read_times = Vec::new();
    for now in (0..=duration_ms).step_by(10) {
        app.clock.set_ms(now);
        // The final output changes data too, so a timer that loses the last
        // invalidation cannot pass merely by returning an old snapshot.
        if now == duration_ms {
            app.backend.runs.push(live_run());
        }
        let before = app.backend.read_count(Read::Timings);
        seq += 1;
        push(&mut app, ObservedStore::Runs, seq);
        if now % 500 == 0 {
            seq += 1;
            push(&mut app, ObservedStore::State, seq);
        }
        if app.backend.read_count(Read::Timings) > before {
            read_times.push(now);
        }
    }
    // Service State/Dispatch's last debounce and the trailing timing read.
    advance(&mut app, interval_ms);
    let reads = app.backend.read_count(Read::Timings);
    let bound = (duration_ms + interval_ms - 1) / interval_ms + 1;
    assert!(
        reads as i64 <= bound,
        "Runs every 10 ms plus State every 500 ms over {duration_ms} ms caused {reads} timing reads; bound {bound}"
    );
    assert!(
        read_times
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= interval_ms)
    );
    assert_eq!(app.state.timings.tasks["task"].started_at_ms, Some(1_000));
    assert_eq!(app.state.timings.sampled_at_ms, app.clock.now_ms());
    advance(&mut app, interval_ms * 2);
    assert_eq!(app.backend.read_count(Read::Timings), reads, "no polling");
    eprintln!("Runs + State over {duration_ms} ms: {reads} timing reads (bound {bound})");
}

fn live_run() -> orch_domain::Run {
    use orch_domain::{Run, RunStatus};
    Run {
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
    }
}

#[test]
fn the_final_run_change_gets_one_trailing_read_at_the_interval() {
    use crate::backend::make_task;
    use orch_domain::TaskStatus;

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
    app.backend.runs.push(live_run());
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
fn tasks_and_goals_keep_their_debounce_while_state_timings_are_limited() {
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
        assert_eq!(
            app.backend.read_count(Read::Timings),
            usize::from(store != ObservedStore::State),
            "{store:?}"
        );
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
fn tasks_and_goals_timings_are_not_deferred_by_later_state_or_runs() {
    for semantic in [ObservedStore::Tasks, ObservedStore::Goals] {
        for later in [ObservedStore::State, ObservedStore::Runs] {
            let mut app = app();
            push(&mut app, ObservedStore::Runs, 1);
            advance(&mut app, REFRESH_DEBOUNCE_MS);
            push(&mut app, ObservedStore::Runs, 2);
            advance(&mut app, 1);
            app.backend.reads.clear();
            push(&mut app, semantic, 3);
            advance(&mut app, 1);
            push(&mut app, later, 4);
            advance(&mut app, REFRESH_DEBOUNCE_MS - 2);
            assert_eq!(app.backend.read_count(Read::Timings), 0, "debounce");
            advance(&mut app, 1);
            assert_eq!(
                app.backend.read_count(Read::Timings),
                1,
                "{semantic:?} then {later:?}"
            );
            advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
            assert_eq!(
                app.backend.read_count(Read::Timings),
                1,
                "already satisfied"
            );
        }
    }
}

#[test]
fn a_task_change_already_seen_by_timings_does_not_suppress_the_next_trailing_read() {
    let mut app = app();
    push(&mut app, ObservedStore::Runs, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    // Timings has seen commit 1, but Tasks has only seen bootstrap's 0.
    push(&mut app, ObservedStore::Tasks, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Tasks), 1);
    assert_eq!(app.backend.read_count(Read::Timings), 1);
    push(&mut app, ObservedStore::Runs, 2);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(app.backend.read_count(Read::Timings), 1, "not early");
    app.clock
        .set_ms(REFRESH_DEBOUNCE_MS + TIMINGS_REFRESH_INTERVAL_SECS * 1_000);
    app.tick();
    assert_eq!(app.backend.read_count(Read::Timings), 2, "trailing read");
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

#[test]
fn dispatch_then_stream_has_bounded_timing_reads_and_shows_the_last_change() {
    use crate::backend::make_task;
    use orch_domain::TaskStatus;

    let mut app = app();
    app.backend.tasks = vec![make_task("task", "Live task", TaskStatus::InProgress)];
    // A dispatch publishes the task transition and State in one commit,
    // before the run's output starts arriving.
    push(&mut app, ObservedStore::Tasks, 1);
    push(&mut app, ObservedStore::State, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    assert_eq!(
        app.backend.read_count(Read::Timings),
        1,
        "dispatch debounce"
    );
    let initial_reads = app.backend.read_count(Read::Timings);
    let interval_ms = TIMINGS_REFRESH_INTERVAL_SECS * 1_000;
    let duration_ms = 30_000;
    let start = 200;
    let mut seq = 1;
    let mut read_times = Vec::new();
    for elapsed in (0..duration_ms).step_by(10) {
        app.clock.set_ms(start + elapsed);
        if elapsed == duration_ms - 10 {
            app.backend.runs.push(live_run());
        }
        let before = app.backend.read_count(Read::Timings);
        seq += 1;
        push(&mut app, ObservedStore::Runs, seq);
        if elapsed > 0 && elapsed % 500 == 0 {
            seq += 1;
            push(&mut app, ObservedStore::State, seq);
        }
        if app.backend.read_count(Read::Timings) > before {
            read_times.push(app.clock.now_ms());
        }
    }
    app.clock.set_ms(start + duration_ms + interval_ms);
    app.tick();
    let reads = app.backend.read_count(Read::Timings) - initial_reads;
    let bound = (duration_ms + interval_ms - 1) / interval_ms + 1;
    assert!(
        reads as i64 <= bound,
        "Dispatch then Runs every 10 ms plus State every 500 ms over {duration_ms} ms caused {reads} timing reads; bound {bound}"
    );
    assert!(
        read_times
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= interval_ms)
    );
    assert_eq!(app.state.timings.tasks["task"].started_at_ms, Some(1_000));
    assert_eq!(app.state.timings.sampled_at_ms, app.clock.now_ms());
    advance(&mut app, interval_ms * 2);
    assert_eq!(
        app.backend.read_count(Read::Timings),
        initial_reads + reads,
        "no polling"
    );
    eprintln!("Dispatch then stream over {duration_ms} ms: {reads} timing reads (bound {bound})");
}

// Timings has read commit 2 while the semantic view still holds bootstrap's
// commit 0. Its late notification dirties that view only, then Runs dirties
// Timings and arms the limiter before the semantic view's debounce.
fn timings_ahead_of_dirty_view(store: ObservedStore) -> App<FakeBackend, TestClock> {
    let mut app = app();
    push(&mut app, ObservedStore::Runs, 1);
    advance(&mut app, REFRESH_DEBOUNCE_MS);
    app.clock.set_ms(200);
    push(&mut app, ObservedStore::Runs, 2);
    app.clock
        .set_ms(REFRESH_DEBOUNCE_MS + TIMINGS_REFRESH_INTERVAL_SECS * 1_000);
    app.tick();
    assert_eq!(app.backend.read_count(Read::Timings), 2);
    app.clock.set_ms(6_000);
    push(&mut app, store, 2);
    advance(&mut app, 1);
    push(&mut app, ObservedStore::Runs, 3);
    assert!(
        app.freshness
            .dirty()
            .contains(crate::refresh::View::Timings)
    );
    assert!(app.run_timings_refresh_at.is_some());
    app
}

#[test]
fn new_tasks_change_while_tasks_view_already_dirty_reads_timings_at_the_debounce() {
    for (store, read) in [
        (ObservedStore::Tasks, Read::Tasks),
        (ObservedStore::Goals, Read::Goals),
    ] {
        let mut app = timings_ahead_of_dirty_view(store);
        advance(&mut app, 1);
        push(&mut app, store, 4);
        app.clock.set_ms(6_000 + REFRESH_DEBOUNCE_MS - 1);
        app.tick();
        assert_eq!(app.backend.read_count(read), 0, "{store:?} debounce");
        assert_eq!(
            app.backend.read_count(Read::Timings),
            2,
            "{store:?} debounce"
        );
        advance(&mut app, 1);
        assert_eq!(app.backend.read_count(read), 1, "{store:?}");
        assert_eq!(
            app.backend.read_count(Read::Timings),
            3,
            "new {store:?} commit reads timings at the debounce"
        );
        assert_eq!(app.state.timings.sampled_at_ms, app.clock.now_ms());
        advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
        assert_eq!(
            app.backend.read_count(Read::Timings),
            3,
            "already satisfied"
        );
    }
}

#[test]
fn stale_tasks_or_goals_notification_on_a_dirty_view_does_not_bypass_the_limiter() {
    for (store, read) in [
        (ObservedStore::Tasks, Read::Tasks),
        (ObservedStore::Goals, Read::Goals),
    ] {
        let mut app = timings_ahead_of_dirty_view(store);
        let deadline = app.run_timings_refresh_at.unwrap();
        advance(&mut app, 1);
        // This repeats commit 2, which Timings already read. Commit 3 is
        // still owed to Runs, so a dirty Timings bit alone cannot bypass.
        push(&mut app, store, 2);
        app.clock.set_ms(6_000 + REFRESH_DEBOUNCE_MS);
        app.tick();
        assert_eq!(
            app.backend.read_count(read),
            1,
            "{store:?} keeps its debounce"
        );
        assert_eq!(
            app.backend.read_count(Read::Timings),
            2,
            "stale {store:?} does not bypass"
        );
        app.clock.set_ms(deadline - 1);
        app.tick();
        assert_eq!(app.backend.read_count(Read::Timings), 2, "not early");
        advance(&mut app, 1);
        assert_eq!(app.backend.read_count(Read::Timings), 3, "trailing read");
        advance(&mut app, TIMINGS_REFRESH_INTERVAL_SECS * 2_000);
        assert_eq!(app.backend.read_count(Read::Timings), 3, "no polling");
    }
}
