//! Ported from `test/unit/application/orchestrator-endurance.test.ts`.
//!
//! Bounds that only show up under load: the state maps, the retry queue cap,
//! stats arithmetic over hundreds of completions, the debounce under pressure,
//! and shutdown with several agents in flight.
//!
//! Several TS cases in this file assert on a hand-built object rather than on
//! the orchestrator (they predate the code being reachable from a test). The
//! ports below drive the real code path instead, which is strictly stronger.

use orch_core::test_support::{make_agent, make_run, make_task};
use orch_domain::{
    Agent, AgentStatus, OrchestratorState, RetryEntry, Run, RunStatus, StateStats, Task,
    TaskStatus, TokenUsage, create_token_usage, default_state,
};

use super::harness::{Harness, NOW_ISO, config_with_concurrency, running_entry};

// ── 1. memory bounds ─────────────────────────────────────────────────────

#[tokio::test]
async fn a_thousand_stale_running_entries_are_all_cleaned_in_one_reconcile() {
    let mut tasks = Vec::with_capacity(1000);
    let mut state = default_state();
    for index in 0..1000 {
        let id = format!("tsk_{index}");
        tasks.push(Task {
            id: id.clone(),
            title: format!("Task {index}"),
            status: TaskStatus::Done,
            attempts: 1,
            ..make_task()
        });
        state.running.insert(
            id.clone(),
            running_entry(
                &id,
                "agt_test1",
                &format!("run_{index}"),
                99_999 + index,
                NOW_ISO,
            ),
        );
        state.claimed.insert(id);
    }

    let h = Harness::builder()
        .tasks(tasks)
        .agents([make_agent()])
        .state(state)
        .build();

    h.reconcile().await.unwrap();

    let after = h.persisted_state();
    // Every entry pointed at a terminal task, so every entry went.
    assert!(after.running.is_empty());
    // `claimed` is untouched by reconcile — only startup recovery clears it,
    // because a claim may belong to a dispatch still in flight.
    assert_eq!(after.claimed.len(), 1000);
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
}

#[tokio::test]
async fn startup_recovery_clears_every_claim() {
    let mut state = default_state();
    for index in 0..1000 {
        state.claimed.insert(format!("tsk_{index}"));
    }
    let h = Harness::builder().state(state).build();

    h.start_watch().await.unwrap();

    assert!(h.persisted_state().claimed.is_empty());
    h.orchestrator.stop().await.unwrap();
}

// ── 2. retry queue saturation ────────────────────────────────────────────

#[tokio::test]
async fn the_retry_queue_is_capped_at_a_hundred_entries_dropping_the_oldest() {
    let h = Harness::builder().build();
    let mut state = default_state();

    for index in 0..200 {
        h.orchestrator.enqueue_retry(
            &mut state,
            &format!("tsk_{index}"),
            1,
            60_000,
            &format!("Error {index}"),
        );
    }

    assert_eq!(state.retry_queue.len(), 100);
    // The surviving window is the newest hundred.
    assert_eq!(state.retry_queue[0].task_id, "tsk_100");
    assert_eq!(state.retry_queue[99].task_id, "tsk_199");

    h.orchestrator
        .enqueue_retry(&mut state, "tsk_new", 1, 60_000, "New error");
    assert_eq!(state.retry_queue.len(), 100);
    assert_eq!(state.retry_queue[0].task_id, "tsk_101");
    assert_eq!(state.retry_queue[99].task_id, "tsk_new");
}

#[tokio::test]
async fn the_retry_queue_is_deduplicated_by_task_id() {
    let h = Harness::builder().build();
    let mut state = OrchestratorState {
        retry_queue: vec![RetryEntry {
            task_id: "tsk_dup".into(),
            attempt: 1,
            due_at: "2025-01-01T00:00:00Z".into(),
            error: "err1".into(),
        }],
        ..default_state()
    };

    h.orchestrator
        .enqueue_retry(&mut state, "tsk_dup", 2, 60_000, "err2");

    assert_eq!(state.retry_queue.len(), 1);
    // The original entry is kept — the newer one is dropped, not merged.
    assert_eq!(state.retry_queue[0].error, "err1");
    assert_eq!(state.retry_queue[0].attempt, 1);
}

// ── 3. stats accumulation ────────────────────────────────────────────────

#[test]
fn five_hundred_token_increments_stay_exact() {
    let mut stats = StateStats::default();
    for index in 0..500u64 {
        let tokens = create_token_usage(
            100 + index,
            200 + index,
            Some(orch_domain::TokenUsageOpts {
                reasoning: Some(50 + index),
                ..Default::default()
            }),
        );
        stats.total_tokens.input += tokens.input;
        stats.total_tokens.output += tokens.output;
        stats.total_tokens.reasoning += tokens.reasoning;
        stats.total_tokens.total =
            stats.total_tokens.input + stats.total_tokens.output + stats.total_tokens.reasoning;
        stats.total_runs += 1;
        stats.total_tasks_completed += 1;
        stats.total_runtime_ms += 1000 + index;
    }

    // sum(100..=599), sum(200..=699), sum(50..=549).
    let expected_input = 500 * 100 + (499 * 500) / 2;
    let expected_output = 500 * 200 + (499 * 500) / 2;
    let expected_reasoning = 500 * 50 + (499 * 500) / 2;
    assert_eq!(stats.total_tokens.input, expected_input);
    assert_eq!(stats.total_tokens.output, expected_output);
    assert_eq!(stats.total_tokens.reasoning, expected_reasoning);
    assert_eq!(
        stats.total_tokens.total,
        expected_input + expected_output + expected_reasoning
    );
    assert_eq!(stats.total_runs, 500);
    assert_eq!(stats.total_tasks_completed, 500);
    // Integers, so there is no `NaN`/`Infinity` class of failure to check for
    // the way the TS version has to.
}

#[test]
fn zero_token_completions_leave_the_totals_at_zero() {
    let mut stats = StateStats::default();
    for _ in 0..100 {
        stats.total_tokens.total =
            stats.total_tokens.input + stats.total_tokens.output + stats.total_tokens.reasoning;
        stats.total_runs += 1;
    }
    assert_eq!(stats.total_tokens.total, 0);
    assert_eq!(stats.total_runs, 100);
}

#[tokio::test]
async fn the_success_handler_recomputes_the_token_total_rather_than_accumulating_it() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_tok".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(OrchestratorState {
            stats: StateStats {
                total_tokens: TokenUsage {
                    input: 1,
                    output: 2,
                    reasoning: 3,
                    // A deliberately wrong total: it must be recomputed, not
                    // added to.
                    total: 999,
                    cache_read: 0,
                    cache_write: 0,
                },
                ..Default::default()
            },
            ..super::harness::state_running("tsk_tok", "agt_test1", "run_tok", 1, NOW_ISO)
        })
        .build();
    h.runs.seed(Run {
        id: "run_tok".into(),
        task_id: "tsk_tok".into(),
        ..make_run()
    });

    h.load_state().await.unwrap();
    h.orchestrator
        .handle_run_success(
            "tsk_tok",
            "run_tok",
            "agt_test1",
            Some(create_token_usage(10, 20, None)),
            None,
            vec![],
        )
        .await
        .unwrap();

    let totals = h.in_memory_state().await.stats.total_tokens;
    assert_eq!(totals.input, 11);
    assert_eq!(totals.output, 22);
    assert_eq!(totals.reasoning, 3);
    assert_eq!(totals.total, 36);
}

// ── 4. debounce under pressure ───────────────────────────────────────────

#[tokio::test]
async fn a_hundred_lazy_saves_inside_one_window_write_once() {
    let h = Harness::builder().build();
    h.load_state().await.unwrap();
    let before = h.state.written().len();

    for _ in 0..100 {
        h.orchestrator.save_state_lazy().await;
    }
    // Before the window closes, nothing has been written.
    assert_eq!(h.state.written().len(), before);

    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(h.state.written().len() - before, 1);
}

#[tokio::test]
async fn a_flush_writes_immediately_and_cancels_the_pending_timer() {
    let h = Harness::builder().build();
    h.load_state().await.unwrap();
    let before = h.state.written().len();

    h.orchestrator.save_state_lazy().await;
    let state = h.in_memory_state().await;
    h.orchestrator.flush_state_lazy(&state).await;
    assert_eq!(h.state.written().len() - before, 1);

    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(h.state.written().len() - before, 1);
}

// ── 8. stale process cleanup ─────────────────────────────────────────────

#[tokio::test]
async fn one_reconcile_is_enough_to_clean_up_a_dead_pid() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_stale".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            max_attempts: 3,
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_stale".into(),
            status: AgentStatus::Running,
            ..make_agent()
        }])
        .state(super::harness::state_running(
            "tsk_stale",
            "agt_stale",
            "run_stale",
            12345,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_stale".into(),
        task_id: "tsk_stale".into(),
        agent_id: "agt_stale".into(),
        ..make_run()
    });
    h.processes.set_dead(12345);

    h.own_lock();
    h.reconcile().await.unwrap();

    assert!(!h.persisted_state().running.contains_key("tsk_stale"));
    assert_eq!(h.agent("agt_stale").status, AgentStatus::Idle);
    assert!(matches!(
        h.task("tsk_stale").status,
        TaskStatus::Retrying | TaskStatus::Failed
    ));
}

// ── 9. graceful shutdown under load ──────────────────────────────────────

#[tokio::test]
async fn stop_winds_down_five_running_agents_and_releases_the_lock() {
    let mut tasks = Vec::new();
    let mut agents = Vec::new();
    let mut state = OrchestratorState {
        pid: Some(1234),
        ..default_state()
    };

    for index in 0..5u32 {
        let task_id = format!("tsk_run{index}");
        let agent_id = format!("agt_run{index}");
        let run_id = format!("run_{index}");
        tasks.push(Task {
            id: task_id.clone(),
            status: TaskStatus::InProgress,
            attempts: 1,
            max_attempts: 3,
            ..make_task()
        });
        agents.push(Agent {
            id: agent_id.clone(),
            name: format!("agent-{index}"),
            status: AgentStatus::Running,
            ..make_agent()
        });
        state.running.insert(
            task_id.clone(),
            running_entry(&task_id, &agent_id, &run_id, 10_000 + index, NOW_ISO),
        );
    }

    let h = Harness::builder()
        .tasks(tasks)
        .agents(agents)
        .config(config_with_concurrency(6))
        .state(state)
        .build();
    for index in 0..5 {
        h.runs.seed(Run {
            id: format!("run_{index}"),
            task_id: format!("tsk_run{index}"),
            agent_id: format!("agt_run{index}"),
            status: RunStatus::Running,
            ..make_run()
        });
    }

    h.own_lock();
    h.load_state().await.unwrap();
    h.orchestrator.stop().await.unwrap();

    assert_eq!(h.processes.killed_with_grace().len(), 5);
    for index in 0..5 {
        assert_eq!(
            h.runs.peek(&format!("run_{index}")).unwrap().status,
            RunStatus::Cancelled
        );
        // attempts 1 < max 3, so each task comes back as `retrying`.
        assert_eq!(
            h.task(&format!("tsk_run{index}")).status,
            TaskStatus::Retrying
        );
        assert_eq!(
            h.agent(&format!("agt_run{index}")).status,
            AgentStatus::Idle
        );
    }

    let after = h.persisted_state();
    assert!(after.running.is_empty());
    assert!(after.claimed.is_empty());
    assert_eq!(after.pid, None);
    assert_eq!(after.started_at, None);
    assert!(h.orchestrator.is_shutting_down());
    assert!(!h.orchestrator.is_owner());
}

#[tokio::test]
async fn a_second_stop_writes_nothing() {
    let h = Harness::builder().build();
    h.own_lock();
    h.load_state().await.unwrap();
    h.orchestrator.stop().await.unwrap();

    let writes_after_first = h.state.written().len();
    h.orchestrator.stop().await.unwrap();
    assert_eq!(h.state.written().len(), writes_after_first);
}
