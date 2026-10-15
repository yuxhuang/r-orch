//! Counted live-update cases: batching, caps, the run-id LRU, the soft-delete
//! clock, notifications, history ingestion and endurance
//! (docs/parity/tui.md §7, §8.1, §8.4; PARITY.md §12.6), numbered `l001..`.
//!
//! Every timer is driven by the injected clock — never a sleep.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, OrchestratorEvent, TaskStatus};
use orch_tui::backend::{Call, FakeBackend, HistoryEntry, make_agent, make_task};
use orch_tui::messages::{MAX_MESSAGES, MAX_TOASTS, MsgType};
use orch_tui::state::ViewId;

fn event_harness() -> Harness {
    let backend = FakeBackend::new()
        .with_tasks(vec![make_task("tsk_1", "Shipping", TaskStatus::InProgress)])
        .with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Running)]);
    Harness::with(backend)
}

fn output_event(data: &str) -> OrchestratorEvent {
    OrchestratorEvent::AgentOutput {
        run_id: "run_1".into(),
        agent_id: "agt_1".into(),
        data: data.into(),
    }
}

fn history(event_type: &str, data: &str) -> HistoryEntry {
    HistoryEntry {
        timestamp: "2026-10-13T11:00:00.000Z".into(),
        run_id: "run_1".into(),
        agent_id: "agt_1".into(),
        task_id: "tsk_1".into(),
        event_type: event_type.into(),
        data: data.into(),
    }
}

// ── Batching ───────────────────────────────────────────────────────────────

#[test]
fn l001_a_burst_is_invisible_before_the_eighty_millisecond_flush() {
    let mut harness = event_harness();
    for i in 0..100 {
        harness
            .app
            .handle_event(&output_event(&format!("line {i}")));
    }
    assert!(harness.app.state.messages.is_empty());
    assert_eq!(harness.app.state.pending_messages.len(), 100);
}

#[test]
fn l002_the_whole_burst_lands_in_one_flush() {
    let mut harness = event_harness();
    for i in 0..100 {
        harness
            .app
            .handle_event(&output_event(&format!("line {i}")));
    }
    harness.advance(80);
    assert_eq!(harness.app.state.messages.len(), 100);
}

#[test]
fn l003_the_flush_does_not_fire_early() {
    let mut harness = event_harness();
    harness.app.handle_event(&output_event("one"));
    harness.advance(79);
    assert!(harness.app.state.messages.is_empty());
    harness.advance(1);
    assert_eq!(harness.app.state.messages.len(), 1);
}

#[test]
fn l004_immediate_mode_drains_the_queue_synchronously() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    for i in 0..200 {
        harness
            .app
            .handle_event(&output_event(&format!("line {i}")));
    }
    assert_eq!(harness.app.state.messages.len(), 200);
}

#[test]
fn l005_the_feed_is_capped_at_five_hundred_entries() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    for i in 0..600 {
        harness
            .app
            .handle_event(&output_event(&format!("line {i}")));
    }
    assert_eq!(harness.app.state.messages.len(), MAX_MESSAGES);
    assert_eq!(harness.app.state.messages[0].text, "line 100");
}

#[test]
fn l006_a_single_batch_larger_than_the_cap_keeps_only_the_tail() {
    let mut harness = event_harness();
    for i in 0..700 {
        harness
            .app
            .handle_event(&output_event(&format!("line {i}")));
    }
    harness.advance(80);
    assert_eq!(harness.app.state.messages.len(), MAX_MESSAGES);
    assert_eq!(harness.app.state.messages.last().unwrap().text, "line 699");
}

#[test]
fn l007_the_refresh_debounce_coalesces_to_one_refresh() {
    let mut harness = event_harness();
    for _ in 0..5 {
        harness.app.handle_event(&OrchestratorEvent::TaskAssigned {
            task_id: "tsk_1".into(),
            agent_id: "agt_1".into(),
        });
    }
    assert!(harness.app.state.refresh_at.is_some());
    harness.advance(150);
    assert!(harness.app.state.refresh_at.is_none());
}

#[test]
fn l008_details_are_capped_at_two_kilobytes() {
    let mut harness = event_harness();
    let long = "x".repeat(5_000);
    harness.app.handle_event(&output_event(&format!(
        "{{\"type\":\"result\",\"result\":\"{long}\"}}"
    )));
    harness.advance(80);
    let detail = harness.app.state.messages[0].detail.as_deref().unwrap();
    assert!(detail.chars().count() <= 2_048 + 12, "{}", detail.len());
}

#[test]
fn l009_the_run_id_maps_are_capped_at_five_hundred() {
    let mut harness = event_harness();
    for i in 0..600 {
        harness.app.handle_event(&OrchestratorEvent::AgentStarted {
            agent_id: format!("agt_{i}"),
            task_id: format!("tsk_{i}"),
            run_id: format!("run_{i}"),
        });
    }
    assert_eq!(harness.app.state.runs.agent_of_run.len(), 500);
    assert!(harness.app.state.runs.agent_of_run.get("run_0").is_none());
    assert!(harness.app.state.runs.agent_of_run.get("run_599").is_some());
}

#[test]
fn l010_the_animation_tick_advances_every_hundred_and_twenty_milliseconds() {
    let mut harness = event_harness();
    let before = harness.app.state.tick;
    harness.advance(120);
    assert_eq!(harness.app.state.tick, before + 1);
    harness.advance(600);
    assert_eq!(harness.app.state.tick, before + 6);
}

// ── Event mapping ──────────────────────────────────────────────────────────

#[test]
fn l011_agent_started_renders_started_task() {
    let mut harness = event_harness();
    harness.app.handle_event(&OrchestratorEvent::AgentStarted {
        agent_id: "agt_1".into(),
        task_id: "tsk_1".into(),
        run_id: "run_1".into(),
    });
    harness.assert_message("Started task");
}

#[test]
fn l012_agent_completed_renders_both_outcomes() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::AgentCompleted {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            success: true,
        });
    harness.assert_message("Completed successfully");
    harness
        .app
        .handle_event(&OrchestratorEvent::AgentCompleted {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            success: false,
        });
    harness.assert_message("Failed");
}

#[test]
fn l013_a_status_change_renders_the_arrow_form() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
    harness.assert_message("in_progress → done");
}

#[test]
fn l014_a_retry_renders_its_attempt_and_rounded_delay() {
    let mut harness = event_harness();
    harness.app.handle_event(&OrchestratorEvent::RunRetry {
        run_id: "run_1".into(),
        attempt: 3,
        delay_ms: 4_400,
    });
    harness.assert_message("Retry #3 (4s delay)");
}

#[test]
fn l015_an_idle_tick_produces_no_feed_entry() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::OrchestratorTick {
            running: 0,
            queued: 0,
        });
    assert!(harness.messages().is_empty());
}

#[test]
fn l016_a_busy_tick_renders_its_counts() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::OrchestratorTick {
            running: 2,
            queued: 3,
        });
    harness.assert_message("2 running · 3 queued");
}

#[test]
fn l017_a_cascade_failure_names_the_dependency() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskCascadeFailed {
            task_id: "tsk_2".into(),
            failed_dependency_id: "tsk_1".into(),
            reason: "upstream failed".into(),
        });
    harness.assert_message("Cascade failed (dep: tsk_1)");
}

#[test]
fn l018_a_legacy_codex_agent_message_renders_its_text() {
    let mut harness = event_harness();
    harness.app.handle_event(&output_event(
        r#"{"type":"agent_message","text":"hello there"}"#,
    ));
    harness.assert_message("hello there");
    harness.refute_message("[agent_message]");
}

#[test]
fn l019_a_null_reasoning_event_is_skipped_entirely() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&output_event(r#"{"type":"reasoning","text":null}"#));
    assert!(harness.messages().is_empty());
}

#[test]
fn l020_a_tool_call_is_classified_as_a_tool_message() {
    let mut harness = event_harness();
    harness.app.handle_event(&output_event(
        r#"{"type":"tool_use","name":"Read","input":{"file_path":"/a/b/c.rs"}}"#,
    ));
    harness.advance(80);
    let message = &harness.app.state.messages[0];
    assert_eq!(message.msg_type, MsgType::Tool);
    assert!(message.text.starts_with("⚙ Read("), "{}", message.text);
}

#[test]
fn l021_output_events_resolve_their_task_through_the_run_map() {
    let mut harness = event_harness();
    harness.app.handle_event(&OrchestratorEvent::AgentStarted {
        agent_id: "agt_1".into(),
        task_id: "tsk_1".into(),
        run_id: "run_1".into(),
    });
    harness.app.handle_event(&output_event("some text"));
    harness.advance(80);
    let last = harness.app.state.messages.last().unwrap();
    assert_eq!(last.task_id.as_deref(), Some("tsk_1"));
}

// ── Soft delete ────────────────────────────────────────────────────────────

#[test]
fn l022_nothing_happens_two_hundred_milliseconds_after_scheduling() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Doomed", TaskStatus::Todo)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.key('d');
    harness.advance(200);
    harness.refute_called(|c| matches!(c, Call::DeleteTask(_)));
}

#[test]
fn l023_the_deletion_executes_after_the_window_and_the_next_sweep() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Doomed", TaskStatus::Todo)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.key('d');
    harness.advance(6_200);
    harness.assert_called(|c| matches!(c, Call::DeleteTask(id) if id == "tsk_1"));
    harness.assert_message("✓ Deleted \"Doomed\"");
}

#[test]
fn l024_multiple_deletions_coexist_and_undo_is_lifo() {
    let backend = FakeBackend::new().with_tasks(vec![
        make_task("tsk_1", "First", TaskStatus::Todo),
        make_task("tsk_2", "Second", TaskStatus::Todo),
    ]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.key('d');
    harness.key('j');
    harness.key('d');
    assert_eq!(harness.app.state.pending_deletions.len(), 2);
    harness.key('z');
    assert_eq!(harness.app.state.pending_deletions.len(), 1);
    harness.advance(6_200);
    harness.assert_called(|c| matches!(c, Call::DeleteTask(id) if id == "tsk_1"));
    harness.refute_called(|c| matches!(c, Call::DeleteTask(id) if id == "tsk_2"));
}

#[test]
fn l025_a_failed_deletion_reports_the_error() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Doomed", TaskStatus::Todo)]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Tasks;
    harness.key('d');
    harness.app.backend.fail_next = Some("disk full".into());
    harness.advance(6_200);
    harness.assert_message("Failed to delete \"Doomed\": disk full");
}

#[test]
fn l026_a_goal_deletion_runs_through_the_same_sweep() {
    let backend = FakeBackend::new().with_goals(vec![orch_tui::backend::make_goal(
        "goal_1",
        "Dead goal",
        orch_domain::GoalStatus::Active,
    )]);
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Goals;
    harness.key('d');
    harness.advance(6_200);
    harness.assert_called(|c| matches!(c, Call::DeleteGoal(id) if id == "goal_1"));
}

// ── Toasts, bell and flash ─────────────────────────────────────────────────

#[test]
fn l027_a_review_transition_raises_a_review_toast() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Review,
        });
    assert_eq!(harness.app.state.toasts.len(), 1);
    harness.assert_text("Task ready for review — press A to approve");
}

#[test]
fn l028_a_non_terminal_transition_raises_no_toast() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        });
    assert!(harness.app.state.toasts.is_empty());
}

#[test]
fn l029_the_toast_queue_caps_at_five_with_the_oldest_dropped() {
    let mut harness = event_harness();
    for _ in 0..10 {
        harness
            .app
            .handle_event(&OrchestratorEvent::TaskStatusChanged {
                task_id: "tsk_1".into(),
                from: TaskStatus::InProgress,
                to: TaskStatus::Done,
            });
    }
    assert_eq!(harness.app.state.toasts.len(), MAX_TOASTS);
    assert_eq!(harness.app.state.toasts[0].id, 6);
}

#[test]
fn l030_a_done_toast_auto_dismisses_after_four_seconds_plus_the_fade() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
    harness.advance(4_000);
    assert_eq!(harness.app.state.toasts.len(), 1);
    harness.advance(360);
    assert!(harness.app.state.toasts.is_empty());
}

#[test]
fn l031_a_failed_toast_lasts_eight_seconds() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Failed,
        });
    harness.advance(8_000);
    assert_eq!(harness.app.state.toasts.len(), 1);
    harness.advance(360);
    assert!(harness.app.state.toasts.is_empty());
}

#[test]
fn l032_a_review_toast_lasts_six_seconds() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Review,
        });
    harness.advance(6_000);
    assert_eq!(harness.app.state.toasts.len(), 1);
    harness.advance(360);
    assert!(harness.app.state.toasts.is_empty());
}

#[test]
fn l033_the_tab_flash_clears_after_twelve_animation_ticks() {
    let mut harness = event_harness();
    harness.app.state.view = ViewId::Logs;
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
    assert!(harness.app.state.flash.is_some());
    harness.advance(12 * 120);
    assert!(harness.app.state.flash.is_none());
}

#[test]
fn l034_an_inactive_tab_flashes_to_its_inverted_pill() {
    let mut harness = event_harness();
    harness.app.state.view = ViewId::Logs;
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
    harness.assert_text(" T TASKS ");
}

#[test]
fn l035_the_bell_never_rings_for_a_done_transition() {
    let mut backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Shipping", TaskStatus::Done)]);
    backend.context.initial_notifications.bell = true;
    let mut harness = Harness::with(backend);
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
    harness.refute_called(|c| matches!(c, Call::Bell));
}

#[test]
fn l036_a_disabled_bell_stays_silent_for_failures() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Failed,
        });
    harness.refute_called(|c| matches!(c, Call::Bell));
}

// ── History loading ────────────────────────────────────────────────────────

#[test]
fn l037_a_done_history_entry_renders_completed() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("done", "")]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("Completed");
}

#[test]
fn l038_a_raw_error_history_entry_renders_verbatim() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("error", "spawn claude ENOENT")]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("spawn claude ENOENT");
}

#[test]
fn l039_repeated_nested_provider_errors_render_once_per_run() {
    let mut backend = FakeBackend::new();
    let nested = r#"{"error":{"message":"rate limited"}}"#;
    backend.pending_history = vec![vec![
        history("error", nested),
        history("error", nested),
        history("error", nested),
    ]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    let count = harness
        .messages()
        .iter()
        .filter(|m| m.contains("rate limited"))
        .count();
    assert_eq!(count, 1);
}

#[test]
fn l040_a_file_changed_history_entry_renders_its_path() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("file_changed", "src/main.rs")]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("src/main.rs");
    assert_eq!(harness.app.state.messages[0].msg_type, MsgType::File);
}

#[test]
fn l041_a_tool_call_history_entry_renders_with_parentheses() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("tool_call", "Bash")]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("⚙ Bash()");
}

#[test]
fn l042_two_history_batches_accumulate() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![
        vec![history("done", "")],
        vec![history("file_changed", "src/lib.rs")],
    ];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("Completed");
    harness.assert_message("src/lib.rs");
}

#[test]
fn l043_an_empty_history_batch_leaves_the_waiting_box_intact() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![]];
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness.app.tick();
    harness.assert_text("Waiting for activity");
}

// ── Endurance ──────────────────────────────────────────────────────────────

#[test]
fn l044_five_thousand_mixed_events_leave_the_app_rendering_and_capped() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    for i in 0..5_000 {
        let event = match i % 7 {
            0 => OrchestratorEvent::AgentStarted {
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                run_id: format!("run_{i}"),
            },
            1 => output_event(&format!("output {i}")),
            2 => OrchestratorEvent::AgentFileChanged {
                run_id: format!("run_{i}"),
                agent_id: "agt_1".into(),
                path: format!("src/file_{i}.rs"),
            },
            3 => OrchestratorEvent::AgentCompleted {
                run_id: format!("run_{i}"),
                agent_id: "agt_1".into(),
                success: i % 2 == 0,
            },
            4 => OrchestratorEvent::AgentError {
                run_id: format!("run_{i}"),
                agent_id: "agt_1".into(),
                error: format!("boom {i}"),
                error_kind: None,
            },
            5 => OrchestratorEvent::OrchestratorTick {
                running: 1,
                queued: 2,
            },
            _ => OrchestratorEvent::TaskStatusChanged {
                task_id: "tsk_1".into(),
                from: TaskStatus::InProgress,
                to: TaskStatus::Done,
            },
        };
        harness.app.handle_event(&event);
    }
    assert_eq!(harness.app.state.messages.len(), MAX_MESSAGES);
    assert!(harness.app.state.runs.agent_of_run.len() <= 500);
    harness.assert_text("ORCH");
}

#[test]
fn l045_a_ten_kilobyte_error_keeps_the_frame_small() {
    let mut harness = event_harness();
    harness.app.state.view = ViewId::Logs;
    let huge = "e".repeat(10_000);
    harness.app.handle_event(&OrchestratorEvent::AgentError {
        run_id: "run_1".into(),
        agent_id: "agt_1".into(),
        error: huge,
        error_kind: None,
    });
    harness.advance(80);
    let text = harness.text();
    assert!(text.len() < 8_192, "frame was {} bytes", text.len());
}

// ── Periodic disk poll ─────────────────────────────────────────────────────

#[test]
fn l046_watch_mode_polls_the_stores_every_five_seconds() {
    // The poll refreshes rather than recording a call, so observe its effect: a
    // store change becomes visible only once the interval has elapsed.
    let mut harness = event_harness();
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_2", "Added", TaskStatus::Todo));
    harness.advance(4_999);
    assert_eq!(harness.app.state.tasks.len(), 1);
    harness.advance(1);
    assert_eq!(harness.app.state.tasks.len(), 2);
}

#[test]
fn l047_observer_mode_polls_every_three_seconds() {
    let mut backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Shipping", TaskStatus::Todo)]);
    backend.context.observer_mode = true;
    let mut harness = Harness::with(backend);
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_2", "Added", TaskStatus::Todo));
    harness.advance(2_999);
    assert_eq!(harness.app.state.tasks.len(), 1);
    harness.advance(1);
    assert_eq!(harness.app.state.tasks.len(), 2);
}

#[test]
fn l048_a_recent_refresh_postpones_the_periodic_poll() {
    let mut harness = event_harness();
    harness.advance(4_000);
    harness.app.refresh_all(false);
    harness
        .app
        .backend
        .tasks
        .push(make_task("tsk_2", "Added", TaskStatus::Todo));
    harness.advance(1_500);
    assert_eq!(
        harness.app.state.tasks.len(),
        1,
        "the interval restarts from the last refresh"
    );
    harness.advance(3_500);
    assert_eq!(harness.app.state.tasks.len(), 2);
}

// ── History timestamps ─────────────────────────────────────────────────────

#[test]
fn l049_a_backfilled_entry_keeps_its_own_instant() {
    // `historyEntryToMsg` sets `ts: new Date(entry.timestamp).getTime()`, so a
    // run recorded hours ago must not read as `now`.
    let mut backend = FakeBackend::new();
    let mut entry = history("done", "");
    // Two hours before the harness clock (which starts at 1_000_000 ms).
    entry.timestamp = "1970-01-01T00:00:00.000Z".into();
    backend.pending_history = vec![vec![entry]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    let message = &harness.app.state.messages[0];
    assert_eq!(
        message.ts_ms, 0,
        "the entry's own instant, not the flush time"
    );
    assert_ne!(message.ts_ms, harness.now());
}

#[test]
fn l050_a_backfilled_entry_renders_its_original_relative_time() {
    let mut backend =
        FakeBackend::new().with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]);
    let mut entry = history("done", "");
    // 1970 against a 1_000_000 ms clock is ~16 minutes of age.
    entry.timestamp = "1970-01-01T00:00:00.000Z".into();
    backend.pending_history = vec![vec![entry]];
    let mut harness = Harness::with(backend);
    harness.app.state.view = ViewId::Logs;
    harness.app.tick();
    harness.assert_text("16m");
    harness.refute_text(" now ");
}

#[test]
fn l051_an_unparseable_history_timestamp_falls_back_to_the_flush_time() {
    let mut backend = FakeBackend::new();
    let mut entry = history("done", "");
    entry.timestamp = "not a date".into();
    backend.pending_history = vec![vec![entry]];
    let mut harness = Harness::with(backend);
    let now = harness.now();
    harness.app.tick();
    assert_eq!(harness.app.state.messages[0].ts_ms, now);
}

#[test]
fn l052_a_live_event_is_stamped_with_the_flush_time() {
    let mut harness = event_harness();
    harness.advance(500);
    let now = harness.now();
    harness.app.handle_event(&output_event("live line"));
    harness.app.state.flush_messages(now);
    assert_eq!(harness.app.state.messages.last().unwrap().ts_ms, now);
}

#[test]
fn l053_a_tool_call_payload_object_renders_its_name() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("tool_call", r#"{"name":"Bash","input":{}}"#)]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("⚙ Bash()");
}

#[test]
fn l054_a_bare_tool_call_name_still_renders() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("tool_call", "Read")]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("⚙ Read()");
}

#[test]
fn l055_a_nameless_tool_call_payload_falls_back_to_tool() {
    let mut backend = FakeBackend::new();
    backend.pending_history = vec![vec![history("tool_call", r#"{"input":{}}"#)]];
    let mut harness = Harness::with(backend);
    harness.app.tick();
    harness.assert_message("⚙ tool()");
}

// ── Repeated lines collapse (PARITY.md §12.8 (k)) ──────────────────────────

fn tick(running: usize, queued: usize) -> OrchestratorEvent {
    OrchestratorEvent::OrchestratorTick { running, queued }
}

fn started(run_id: &str, agent_id: &str, task_id: &str) -> OrchestratorEvent {
    OrchestratorEvent::AgentStarted {
        run_id: run_id.into(),
        agent_id: agent_id.into(),
        task_id: task_id.into(),
    }
}

fn output_from(run_id: &str, agent_id: &str, data: &str) -> OrchestratorEvent {
    OrchestratorEvent::AgentOutput {
        run_id: run_id.into(),
        agent_id: agent_id.into(),
        data: data.into(),
    }
}

fn texts(harness: &Harness) -> Vec<String> {
    harness
        .app
        .state
        .messages
        .iter()
        .map(|m| m.text.clone())
        .collect()
}

#[test]
fn l056_an_unchanged_tick_adds_no_line_and_keeps_the_first_timestamp() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    harness.app.handle_event(&tick(1, 0));
    let first_ts = harness.app.state.messages[0].ts_ms;
    for _ in 0..30 {
        harness.advance(1000);
        harness.app.handle_event(&tick(1, 0));
    }
    assert_eq!(texts(&harness), vec!["1 running · 0 queued"]);
    assert_eq!(harness.app.state.messages[0].ts_ms, first_ts);
}

#[test]
fn l057_a_changed_tick_adds_a_line() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    harness.app.handle_event(&tick(1, 0));
    harness.app.handle_event(&tick(1, 0));
    harness.app.handle_event(&tick(2, 1));
    harness.app.handle_event(&tick(2, 1));
    assert_eq!(
        texts(&harness),
        vec!["1 running · 0 queued", "2 running · 1 queued"]
    );
}

#[test]
fn l058_work_resuming_after_idle_adds_a_line_even_with_the_same_counts() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    harness.app.handle_event(&tick(1, 0));
    harness.app.handle_event(&tick(0, 0)); // idle: no line, but resets
    harness.app.handle_event(&tick(1, 0));
    assert_eq!(
        texts(&harness),
        vec!["1 running · 0 queued", "1 running · 0 queued"]
    );
}

#[test]
fn l059_an_unchanged_tick_is_suppressed_even_after_other_system_lines() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    harness.app.handle_event(&tick(1, 0));
    harness
        .app
        .handle_event(&started("run_1", "agt_1", "tsk_1"));
    let after_start = harness.app.state.messages.len();
    harness.app.handle_event(&tick(1, 0));
    assert_eq!(harness.app.state.messages.len(), after_start);
}

#[test]
fn l060_a_repeated_agent_line_collapses_within_and_across_flushes() {
    let mut harness = event_harness();
    harness
        .app
        .handle_event(&started("run_1", "agt_1", "tsk_1"));
    harness.advance(80);
    let base = harness.app.state.messages.len();
    let thinking = r#"{"type":"system","subtype":"thinking_token"}"#;
    // A burst inside one 80 ms batch …
    for _ in 0..20 {
        harness
            .app
            .handle_event(&output_from("run_1", "agt_1", thinking));
    }
    harness.advance(80);
    // … and more in later batches.
    for _ in 0..20 {
        harness
            .app
            .handle_event(&output_from("run_1", "agt_1", thinking));
        harness.advance(80);
    }
    let lines = &texts(&harness)[base..];
    assert_eq!(lines, ["[thinking_token]"]);
}

#[test]
fn l061_identical_lines_from_different_agents_both_stay() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    harness
        .app
        .handle_event(&started("run_1", "agt_1", "tsk_1"));
    harness
        .app
        .handle_event(&started("run_2", "agt_2", "tsk_2"));
    let base = harness.app.state.messages.len();
    harness
        .app
        .handle_event(&output_from("run_1", "agt_1", "Running tests"));
    harness
        .app
        .handle_event(&output_from("run_2", "agt_2", "Running tests"));
    harness
        .app
        .handle_event(&output_from("run_1", "agt_1", "Running tests"));
    assert_eq!(&texts(&harness)[base..], ["Running tests", "Running tests"]);
}

#[test]
fn l062_a_line_repeated_after_a_different_one_from_the_same_agent_stays() {
    let mut harness = event_harness();
    harness.app.batch_ms = 0;
    harness
        .app
        .handle_event(&started("run_1", "agt_1", "tsk_1"));
    let base = harness.app.state.messages.len();
    for line in ["Running tests", "3 passed", "Running tests"] {
        harness
            .app
            .handle_event(&output_from("run_1", "agt_1", line));
    }
    assert_eq!(
        &texts(&harness)[base..],
        ["Running tests", "3 passed", "Running tests"]
    );
}
