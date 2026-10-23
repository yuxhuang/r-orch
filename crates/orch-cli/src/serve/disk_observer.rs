//! The disk observer.
//!
//! `serve` never uses it. `tui` falls back to it when starting watch mode fails
//! because another process holds the lock: instead of orchestrating, it polls
//! `state.json` and tails each running run's JSONL, synthesising the bus
//! events a live orchestrator would have emitted.
//!
//! The byte accounting in [`DiskObserver::tail_run_events`] is the whole
//! point: a run's JSONL is appended to while being read, so a tick must stop
//! at the last **complete** line and remember the partial one, or every
//! truncated write would turn into a dropped event.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use orch_core::clock::Clock;
use orch_core::storage::{RunStore, StateStore};
use orch_domain::{OrchestratorEvent, RunEvent, RunEventType, RunStatus, TaskStatus};
use orch_infra::storage::Paths;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Keep a run's offsets this long after it leaves `state.running`.
pub const EVICTION_TTL_MS: i64 = 5 * 60_000;
/// Bytes read per JSONL file per tick.
pub const MAX_READ_PER_TICK: u64 = 512 * 1024;
/// A partial line longer than this is discarded rather than retained.
pub const MAX_REMAINDER_LEN: usize = 64 * 1024;
/// The default poll interval.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1_000);

/// Per-run read position.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedRun {
    agent_id: String,
    task_id: String,
    offset: u64,
    remainder: String,
    last_seen_at: i64,
}

pub struct DiskObserver {
    paths: Paths,
    state: Arc<dyn StateStore>,
    runs: Arc<dyn RunStore>,
    clock: Arc<dyn Clock>,
    poll_interval: Duration,
    tracked: HashMap<String, TrackedRun>,
    prev_running: Vec<String>,
}

impl DiskObserver {
    pub fn new(
        paths: Paths,
        state: Arc<dyn StateStore>,
        runs: Arc<dyn RunStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        DiskObserver {
            paths,
            state,
            runs,
            clock,
            poll_interval: DEFAULT_POLL_INTERVAL,
            tracked: HashMap::new(),
            prev_running: Vec::new(),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// One tick, as the events it would emit.
    ///
    /// Returning them rather than invoking a callback is what makes the
    /// accounting testable; [`DiskObserver::observe`] is the callback form.
    /// Every error is swallowed, like every poll error: a half-written
    /// `state.json` must not take down the TUI.
    pub async fn poll(&mut self) -> Vec<OrchestratorEvent> {
        let Ok(state) = self.state.read().await else {
            return Vec::new();
        };

        let mut events = Vec::new();
        let now = self.clock.now_ms();
        let mut current: Vec<String> = Vec::new();

        // 1. Track every running run, announcing the ones that are new.
        for entry in state.running.values() {
            current.push(entry.run_id.clone());
            match self.tracked.get_mut(&entry.run_id) {
                Some(tracked) => tracked.last_seen_at = now,
                None => {
                    self.tracked.insert(
                        entry.run_id.clone(),
                        TrackedRun {
                            agent_id: entry.agent_id.clone(),
                            task_id: entry.task_id.clone(),
                            offset: 0,
                            remainder: String::new(),
                            last_seen_at: now,
                        },
                    );
                    events.push(OrchestratorEvent::AgentStarted {
                        agent_id: entry.agent_id.clone(),
                        task_id: entry.task_id.clone(),
                        run_id: entry.run_id.clone(),
                    });
                }
            }
        }

        // 2. Runs that have just left `state.running`: one last tail, then the
        //    completion pair.
        let finished: Vec<String> = self
            .prev_running
            .iter()
            .filter(|run_id| !current.contains(run_id))
            .cloned()
            .collect();
        for run_id in finished {
            events.extend(self.tail_run_events(&run_id).await);

            let run = self.runs.get(&run_id).await.ok().flatten();
            let status = run.as_ref().map(|run| run.status);
            let success = status == Some(RunStatus::Succeeded);
            let agent_id = self
                .tracked
                .get(&run_id)
                .map(|tracked| tracked.agent_id.clone())
                .unwrap_or_default();

            events.push(OrchestratorEvent::AgentCompleted {
                run_id: run_id.clone(),
                agent_id,
                success,
            });
            if let Some(task_id) = run.as_ref().map(|run| run.task_id.clone()) {
                events.push(OrchestratorEvent::TaskStatusChanged {
                    task_id,
                    from: TaskStatus::InProgress,
                    // A cancel finishes the run `cancelled` and the task
                    // `cancelled`; reporting it as a failure would raise a
                    // "Task failed" toast for a task nobody saw fail.
                    to: match status {
                        Some(RunStatus::Succeeded) => TaskStatus::Review,
                        Some(RunStatus::Cancelled) => TaskStatus::Cancelled,
                        _ => TaskStatus::Failed,
                    },
                });
            }
        }

        // 3. Tail everything still running.
        for run_id in &current {
            events.extend(self.tail_run_events(run_id).await);
        }

        // 4. A synthetic tick, but only while something is running. `queued`
        //    is always 0: the observer cannot see the queue.
        if !current.is_empty() {
            events.push(OrchestratorEvent::OrchestratorTick {
                running: current.len(),
                queued: 0,
            });
        }

        // 5. Rebuild, then evict runs absent for longer than the TTL.
        self.prev_running = current;
        self.tracked.retain(|run_id, tracked| {
            self.prev_running.contains(run_id) || now - tracked.last_seen_at <= EVICTION_TTL_MS
        });

        events
    }

    /// Read whatever has been appended since the last tick and translate it.
    async fn tail_run_events(&mut self, run_id: &str) -> Vec<OrchestratorEvent> {
        let Some(tracked) = self.tracked.get(run_id).cloned() else {
            return Vec::new();
        };
        let Ok(path) = self.paths.run_events_path(run_id) else {
            return Vec::new();
        };

        let Ok(mut file) = tokio::fs::File::open(&path).await else {
            // The file does not exist yet: nothing to read, and not an error.
            return Vec::new();
        };
        let Ok(metadata) = file.metadata().await else {
            return Vec::new();
        };
        if metadata.len() <= tracked.offset {
            return Vec::new();
        }

        let to_read = (metadata.len() - tracked.offset).min(MAX_READ_PER_TICK);
        if file
            .seek(std::io::SeekFrom::Start(tracked.offset))
            .await
            .is_err()
        {
            return Vec::new();
        }
        let mut buffer = vec![0u8; to_read as usize];
        let Ok(read) = file.read(&mut buffer).await else {
            return Vec::new();
        };
        if read == 0 {
            return Vec::new();
        }

        let chunk = format!(
            "{}{}",
            tracked.remainder,
            String::from_utf8_lossy(&buffer[..read])
        );
        let mut lines: Vec<&str> = chunk.split('\n').collect();
        let last_line = lines.pop().unwrap_or("");

        // The offset always advances by everything that was read, and the
        // partial tail is carried in `remainder`.
        //
        // Advancing the offset by `bytesRead - remainderBytes` *and* keeping
        // the remainder would re-read the partial bytes on the next tick and
        // prepend them again, producing `partial + partial + rest`, which
        // fails to parse — so an event whose write straddles a tick would be
        // **lost**. Silently dropping an event is a correctness bug rather
        // than a cosmetic one.
        let (remainder, offset) = if chunk.ends_with('\n') {
            (String::new(), tracked.offset + read as u64)
        } else if last_line.len() > MAX_REMAINDER_LEN {
            // A pathologically long partial line is discarded.
            (String::new(), tracked.offset + read as u64)
        } else {
            (last_line.to_owned(), tracked.offset + read as u64)
        };

        if let Some(entry) = self.tracked.get_mut(run_id) {
            entry.remainder = remainder;
            entry.offset = offset;
        }

        lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str::<RunEvent>(line).ok())
            .filter_map(|event| translate_event(&event, run_id, &tracked.agent_id))
            .collect()
    }

    /// Poll at the configured interval, handing each event to `handler`, until
    /// the returned guard is dropped.
    pub fn observe<H>(mut self, mut handler: H) -> ObserverHandle
    where
        H: FnMut(OrchestratorEvent) + Send + 'static,
    {
        let interval = self.poll_interval;
        let task = tokio::spawn(async move {
            loop {
                // The first poll runs immediately, then on the interval.
                for event in self.poll().await {
                    handler(event);
                }
                tokio::time::sleep(interval).await;
            }
        });
        ObserverHandle(task)
    }
}

/// Stops the polling loop when dropped.
pub struct ObserverHandle(tokio::task::JoinHandle<()>);

impl Drop for ObserverHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Five of the six run-event types become bus events;
/// `done` and anything unknown are dropped.
pub fn translate_event(
    event: &RunEvent,
    run_id: &str,
    agent_id: &str,
) -> Option<OrchestratorEvent> {
    let data_string = || match &event.data {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };

    match event.event_type {
        RunEventType::AgentOutput | RunEventType::ToolCall | RunEventType::CommandRun => {
            Some(OrchestratorEvent::AgentOutput {
                run_id: run_id.to_owned(),
                agent_id: agent_id.to_owned(),
                data: data_string(),
            })
        }
        RunEventType::FileChanged => Some(OrchestratorEvent::AgentFileChanged {
            run_id: run_id.to_owned(),
            agent_id: agent_id.to_owned(),
            // The string form, else `data.path`, else empty.
            path: match &event.data {
                serde_json::Value::String(path) => path.clone(),
                other => other
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
        }),
        RunEventType::Error => Some(OrchestratorEvent::AgentError {
            run_id: run_id.to_owned(),
            agent_id: agent_id.to_owned(),
            error: data_string(),
            error_kind: None,
        }),
        RunEventType::Done => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{FakeRunStore, FakeStateStore, FixedClock, make_run};
    use orch_domain::{RunningEntry, default_state};
    use serde_json::json;

    fn event(event_type: RunEventType, data: serde_json::Value) -> RunEvent {
        RunEvent {
            timestamp: "2026-10-13T16:03:45.086Z".to_owned(),
            event_type,
            data,
        }
    }

    struct Fixture {
        observer: DiskObserver,
        paths: Paths,
        state: Arc<FakeStateStore>,
        runs: Arc<FakeRunStore>,
        clock: Arc<FixedClock>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::new(dir.path().to_path_buf());
        std::fs::create_dir_all(paths.runs_dir()).expect("runs dir");

        let state = Arc::new(FakeStateStore::new());
        let runs = Arc::new(FakeRunStore::new());
        let clock = Arc::new(FixedClock::new());
        let observer = DiskObserver::new(
            paths.clone(),
            Arc::clone(&state) as Arc<dyn StateStore>,
            Arc::clone(&runs) as Arc<dyn RunStore>,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        Fixture {
            observer,
            paths,
            state,
            runs,
            clock,
            _dir: dir,
        }
    }

    fn running(run_id: &str) -> RunningEntry {
        RunningEntry {
            remote: None,
            run_id: run_id.to_owned(),
            agent_id: "agt_1".to_owned(),
            task_id: "tsk_1".to_owned(),
            pid: 1234,
            started_at: "2026-10-13T16:00:00.000Z".to_owned(),
            last_event_at: "2026-10-13T16:00:00.000Z".to_owned(),
        }
    }

    fn with_running(run_ids: &[&str]) -> orch_domain::OrchestratorState {
        let mut state = default_state();
        for run_id in run_ids {
            state.running.insert((*run_id).to_owned(), running(run_id));
        }
        state
    }

    async fn append(paths: &Paths, run_id: &str, lines: &[RunEvent]) {
        let path = paths.run_events_path(run_id).expect("path");
        let mut body = String::new();
        for line in lines {
            body.push_str(&serde_json::to_string(line).expect("serialize"));
            body.push('\n');
        }
        append_raw(&path, &body).await;
    }

    async fn append_raw(path: &std::path::Path, body: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        file.write_all(body.as_bytes()).expect("write");
    }

    #[tokio::test]
    async fn a_new_running_entry_announces_a_start_and_a_tick() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));

        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![
                OrchestratorEvent::AgentStarted {
                    agent_id: "agt_1".to_owned(),
                    task_id: "tsk_1".to_owned(),
                    run_id: "run_1".to_owned(),
                },
                OrchestratorEvent::OrchestratorTick {
                    running: 1,
                    queued: 0,
                },
            ]
        );

        // A second tick does not re-announce it.
        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![OrchestratorEvent::OrchestratorTick {
                running: 1,
                queued: 0,
            }]
        );
    }

    #[tokio::test]
    async fn an_empty_state_emits_nothing_at_all() {
        let mut fixture = fixture();
        fixture.state.seed(default_state());
        assert!(fixture.observer.poll().await.is_empty());
    }

    #[tokio::test]
    async fn appended_events_are_translated_once_each() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;

        append(
            &fixture.paths,
            "run_1",
            &[
                event(RunEventType::AgentOutput, json!("hello")),
                event(RunEventType::FileChanged, json!({ "path": "src/a.ts" })),
                event(RunEventType::Error, json!("boom")),
                event(RunEventType::Done, json!({ "result": "ok" })),
            ],
        )
        .await;

        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![
                OrchestratorEvent::AgentOutput {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    data: "hello".to_owned(),
                },
                OrchestratorEvent::AgentFileChanged {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    path: "src/a.ts".to_owned(),
                },
                OrchestratorEvent::AgentError {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    error: "boom".to_owned(),
                    error_kind: None,
                },
                OrchestratorEvent::OrchestratorTick {
                    running: 1,
                    queued: 0,
                },
            ],
            "`done` is dropped"
        );

        // Nothing new: the offset means they are not re-read.
        let events = fixture.observer.poll().await;
        assert_eq!(events.len(), 1, "only the tick");
    }

    #[tokio::test]
    async fn a_partial_line_is_held_until_it_is_complete() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;

        let path = fixture.paths.run_events_path("run_1").expect("path");
        let full = serde_json::to_string(&event(RunEventType::AgentOutput, json!("split")))
            .expect("serialize");
        let (head, tail) = full.split_at(full.len() / 2);

        append_raw(&path, head).await;
        let events = fixture.observer.poll().await;
        assert_eq!(events.len(), 1, "the half line yields nothing but the tick");

        append_raw(&path, &format!("{tail}\n")).await;
        let events = fixture.observer.poll().await;
        assert_eq!(
            events[0],
            OrchestratorEvent::AgentOutput {
                run_id: "run_1".to_owned(),
                agent_id: "agt_1".to_owned(),
                data: "split".to_owned(),
            },
            "the completed line is emitted exactly once"
        );
    }

    #[tokio::test]
    async fn a_corrupt_line_is_skipped_and_the_rest_still_arrive() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;

        let path = fixture.paths.run_events_path("run_1").expect("path");
        let good = serde_json::to_string(&event(RunEventType::AgentOutput, json!("ok")))
            .expect("serialize");
        append_raw(&path, &format!("{{not json\n\n{good}\n")).await;

        let events = fixture.observer.poll().await;
        assert_eq!(events.len(), 2, "one output plus the tick: {events:?}");
        assert!(matches!(events[0], OrchestratorEvent::AgentOutput { .. }));
    }

    #[tokio::test]
    async fn a_run_leaving_the_state_reports_completion_and_a_status_change() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;

        fixture.runs.seed(orch_domain::Run {
            id: "run_1".to_owned(),
            task_id: "tsk_1".to_owned(),
            status: RunStatus::Succeeded,
            ..make_run()
        });
        // One last event, written just before the run left the state.
        append(
            &fixture.paths,
            "run_1",
            &[event(RunEventType::AgentOutput, json!("last words"))],
        )
        .await;
        fixture.state.seed(default_state());

        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![
                OrchestratorEvent::AgentOutput {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    data: "last words".to_owned(),
                },
                OrchestratorEvent::AgentCompleted {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    success: true,
                },
                OrchestratorEvent::TaskStatusChanged {
                    task_id: "tsk_1".to_owned(),
                    from: TaskStatus::InProgress,
                    to: TaskStatus::Review,
                },
            ],
            "no tick: nothing is running any more"
        );
    }

    #[tokio::test]
    async fn a_failed_run_reports_failure_and_the_failed_status() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;

        fixture.runs.seed(orch_domain::Run {
            id: "run_1".to_owned(),
            task_id: "tsk_1".to_owned(),
            status: RunStatus::Failed,
            ..make_run()
        });
        fixture.state.seed(default_state());

        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![
                OrchestratorEvent::AgentCompleted {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    success: false,
                },
                OrchestratorEvent::TaskStatusChanged {
                    task_id: "tsk_1".to_owned(),
                    from: TaskStatus::InProgress,
                    to: TaskStatus::Failed,
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_cancelled_run_reports_the_cancelled_status_not_a_failure() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;

        fixture.runs.seed(orch_domain::Run {
            id: "run_1".to_owned(),
            task_id: "tsk_1".to_owned(),
            status: RunStatus::Cancelled,
            ..make_run()
        });
        fixture.state.seed(default_state());

        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![
                OrchestratorEvent::AgentCompleted {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    success: false,
                },
                OrchestratorEvent::TaskStatusChanged {
                    task_id: "tsk_1".to_owned(),
                    from: TaskStatus::InProgress,
                    to: TaskStatus::Cancelled,
                },
            ],
            "the pair a live orchestrator's cancel emits"
        );
    }

    #[tokio::test]
    async fn a_missing_run_record_still_reports_completion_without_a_status_change() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;
        fixture.state.seed(default_state());

        let events = fixture.observer.poll().await;
        assert_eq!(
            events,
            vec![OrchestratorEvent::AgentCompleted {
                run_id: "run_1".to_owned(),
                agent_id: "agt_1".to_owned(),
                success: false,
            }]
        );
    }

    #[tokio::test]
    async fn offsets_are_evicted_only_after_the_ttl() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;
        fixture.state.seed(default_state());
        fixture.observer.poll().await;

        // Still tracked just inside the TTL…
        fixture.clock.advance_ms(EVICTION_TTL_MS);
        fixture.observer.poll().await;
        assert!(fixture.observer.tracked.contains_key("run_1"));

        // …and gone just past it.
        fixture.clock.advance_ms(1);
        fixture.observer.poll().await;
        assert!(!fixture.observer.tracked.contains_key("run_1"));
    }

    #[tokio::test]
    async fn a_run_that_comes_back_is_announced_again_after_eviction() {
        let mut fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));
        fixture.observer.poll().await;
        fixture.state.seed(default_state());
        fixture.observer.poll().await;
        fixture.clock.advance_ms(EVICTION_TTL_MS + 1);
        fixture.observer.poll().await;

        fixture.state.seed(with_running(&["run_1"]));
        let events = fixture.observer.poll().await;
        assert!(matches!(events[0], OrchestratorEvent::AgentStarted { .. }));
    }

    #[tokio::test]
    async fn the_tick_counts_every_running_run_and_never_reports_a_queue() {
        let mut fixture = fixture();
        fixture
            .state
            .seed(with_running(&["run_1", "run_2", "run_3"]));
        let events = fixture.observer.poll().await;
        assert_eq!(
            events.last(),
            Some(&OrchestratorEvent::OrchestratorTick {
                running: 3,
                queued: 0,
            })
        );
    }

    #[test]
    fn tool_call_and_command_run_become_output_and_done_is_dropped() {
        for event_type in [RunEventType::ToolCall, RunEventType::CommandRun] {
            let translated = translate_event(
                &event(event_type, json!({ "name": "read" })),
                "run_1",
                "agt_1",
            );
            assert_eq!(
                translated,
                Some(OrchestratorEvent::AgentOutput {
                    run_id: "run_1".to_owned(),
                    agent_id: "agt_1".to_owned(),
                    data: r#"{"name":"read"}"#.to_owned(),
                }),
                "{event_type:?}"
            );
        }
        assert_eq!(
            translate_event(&event(RunEventType::Done, json!(null)), "run_1", "agt_1"),
            None
        );
    }

    #[test]
    fn a_file_change_falls_back_through_string_then_path_then_empty() {
        let string_form = translate_event(
            &event(RunEventType::FileChanged, json!("src/a.ts")),
            "run_1",
            "agt_1",
        );
        assert!(matches!(
            string_form,
            Some(OrchestratorEvent::AgentFileChanged { ref path, .. }) if path == "src/a.ts"
        ));

        let no_path = translate_event(
            &event(RunEventType::FileChanged, json!({ "other": 1 })),
            "run_1",
            "agt_1",
        );
        assert!(matches!(
            no_path,
            Some(OrchestratorEvent::AgentFileChanged { ref path, .. }) if path.is_empty()
        ));
    }

    #[test]
    fn the_constants_are_the_reference_ones() {
        assert_eq!(EVICTION_TTL_MS, 300_000);
        assert_eq!(MAX_READ_PER_TICK, 524_288);
        assert_eq!(MAX_REMAINDER_LEN, 65_536);
        assert_eq!(DEFAULT_POLL_INTERVAL, Duration::from_millis(1_000));
    }

    #[tokio::test]
    async fn the_observer_polls_immediately_then_on_the_interval() {
        let fixture = fixture();
        fixture.state.seed(with_running(&["run_1"]));

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handle = fixture
            .observer
            .with_poll_interval(Duration::from_millis(10))
            .observe({
                let seen = Arc::clone(&seen);
                move |event| seen.lock().unwrap().push(event)
            });

        // The first poll is immediate, so the start arrives without waiting a
        // full interval — but "immediate" is still a spawned task, so this
        // waits for it rather than assuming it has been scheduled already.
        for _ in 0..200 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!seen.lock().unwrap().is_empty());

        let before = seen.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(seen.lock().unwrap().len() > before, "it keeps polling");

        drop(handle);
        let after_drop = seen.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            seen.lock().unwrap().len(),
            after_drop,
            "dropping the handle stops the loop"
        );
    }
}
