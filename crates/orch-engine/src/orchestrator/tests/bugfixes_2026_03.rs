//! Three regressions from the March 2026 debug report: file paths recovered
//! from `tool_call` events, orphaned `preparing` runs finalized at startup, and
//! the per-agent stall timeout actually being used.

use std::sync::Arc;

use orch_core::adapter::{AgentEvent, AgentEventKind};
use orch_core::test_support::{FakeAdapter, make_agent, make_run, make_task};
use orch_domain::{
    Agent, AgentStatus, OrchestratorConfig, OrchestratorEvent, OrchestratorEventType, Run,
    RunStatus, Task, TaskStatus, default_config,
};

use super::harness::{Harness, NOW_ISO, iso_before, state_running, test_config};

fn tool_call(name: &str, file_path: &str) -> AgentEvent {
    AgentEvent::new(
        AgentEventKind::ToolCall,
        NOW_ISO,
        serde_json::json!({
            "type": "tool_use",
            "name": name,
            "input": { "file_path": file_path },
        }),
    )
}

fn done(result: &str) -> AgentEvent {
    AgentEvent::new(
        AgentEventKind::Done,
        NOW_ISO,
        serde_json::json!({ "type": "result", "result": result }),
    )
}

async fn run_with_events(task_id: &str, events: Vec<AgentEvent>) -> Harness {
    let adapter = FakeAdapter::new();
    adapter.set_events(events);
    let h = Harness::builder()
        .tasks([Task {
            id: task_id.into(),
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter(Arc::new(adapter))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;
    h
}

// ── P2: file paths out of tool_call events ───────────────────────────────

#[tokio::test]
async fn write_shaped_tool_calls_become_proof_files_changed() {
    // Claude emits `tool_use` with the path but no `file_change` event, so the
    // proof would otherwise be empty for a run that wrote files.
    let h = run_with_events(
        "tsk_p2",
        vec![
            tool_call("Write", "/tmp/project/src/main.ts"),
            tool_call("Edit", "/tmp/project/src/utils.ts"),
            done("Done writing files"),
        ],
    )
    .await;

    let files = h.task("tsk_p2").proof.expect("proof").files_changed;
    assert!(files.contains(&"/tmp/project/src/main.ts".to_owned()));
    assert!(files.contains(&"/tmp/project/src/utils.ts".to_owned()));
}

#[tokio::test]
async fn a_tool_call_derived_path_is_also_announced_on_the_bus() {
    let h = run_with_events(
        "tsk_p2e",
        vec![tool_call("Write", "/tmp/file.ts"), done("ok")],
    )
    .await;

    let paths: Vec<String> = h
        .events_of(OrchestratorEventType::AgentFileChanged)
        .into_iter()
        .filter_map(|event| match event {
            OrchestratorEvent::AgentFileChanged { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    assert!(paths.contains(&"/tmp/file.ts".to_owned()), "{paths:?}");
}

#[tokio::test]
async fn a_read_or_bash_tool_call_contributes_no_files() {
    let h = run_with_events(
        "tsk_p2n",
        vec![
            tool_call("Read", "/tmp/readme.md"),
            AgentEvent::new(
                AgentEventKind::ToolCall,
                NOW_ISO,
                serde_json::json!({
                    "type": "tool_use",
                    "name": "Bash",
                    "input": { "command": "ls" },
                }),
            ),
            done("done"),
        ],
    )
    .await;

    let files = h
        .task("tsk_p2n")
        .proof
        .map(|proof| proof.files_changed)
        .unwrap_or_default();
    assert!(!files.contains(&"/tmp/readme.md".to_owned()), "{files:?}");
}

#[tokio::test]
async fn the_write_tool_match_is_case_insensitive_and_covers_the_four_names() {
    let h = run_with_events(
        "tsk_p2c",
        vec![
            tool_call("write", "/a.ts"),
            tool_call("MULTIEDIT", "/b.ts"),
            tool_call("NotebookEdit", "/c.ipynb"),
            done("ok"),
        ],
    )
    .await;

    let files = h.task("tsk_p2c").proof.expect("proof").files_changed;
    assert_eq!(
        files,
        vec![
            "/a.ts".to_owned(),
            "/b.ts".to_owned(),
            "/c.ipynb".to_owned()
        ]
    );
}

// ── P3: orphaned `preparing` runs ────────────────────────────────────────

#[tokio::test]
async fn startup_finalizes_an_orphaned_preparing_run_as_cancelled() {
    let h = Harness::builder().build();
    h.runs.seed(Run {
        id: "run_orphan1".into(),
        status: RunStatus::Preparing,
        started_at: iso_before(60_000),
        ..make_run()
    });

    h.start_watch().await.unwrap();

    let run = h.runs.peek("run_orphan1").expect("the run");
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(
        run.error.as_deref(),
        Some("Orphaned preparing run (orchestrator restarted)")
    );
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn startup_leaves_a_preparing_run_that_belongs_to_a_live_entry_alone() {
    let h = Harness::builder()
        .tasks([Task {
            status: TaskStatus::InProgress,
            ..make_task()
        }])
        .agents([Agent {
            status: AgentStatus::Running,
            ..make_agent()
        }])
        .state(state_running(
            "tsk_test1",
            "agt_test1",
            "run_active1",
            12345,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_active1".into(),
        status: RunStatus::Preparing,
        ..make_run()
    });

    h.start_watch().await.unwrap();

    assert_eq!(
        h.runs.peek("run_active1").unwrap().status,
        RunStatus::Preparing
    );
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn startup_finalizes_every_orphaned_preparing_run() {
    let h = Harness::builder().build();
    for id in ["run_o1", "run_o2", "run_o3"] {
        h.runs.seed(Run {
            id: id.into(),
            status: RunStatus::Preparing,
            ..make_run()
        });
    }

    h.start_watch().await.unwrap();

    for id in ["run_o1", "run_o2", "run_o3"] {
        assert_eq!(
            h.runs.peek(id).unwrap().status,
            RunStatus::Cancelled,
            "run {id}"
        );
    }
    h.orchestrator.stop().await.unwrap();
}

// ── per-agent stall timeout ──────────────────────────────────────────────

/// `DEFAULT_CONFIG` with the global stall timeout pinned at 10 minutes.
fn config_with_global_stall(stall_timeout_ms: u64) -> OrchestratorConfig {
    let mut config = test_config();
    config.defaults.agent.stall_timeout_ms = stall_timeout_ms;
    config
}

#[tokio::test]
async fn a_generous_per_agent_stall_timeout_overrides_the_global_one() {
    // 11.7 minutes of silence: past the 10-minute global timeout, inside the
    // agent's own 30-minute one.
    let mut agent = Agent {
        id: "agt_long".into(),
        status: AgentStatus::Running,
        ..make_agent()
    };
    agent.config.stall_timeout_ms = Some(1_800_000);

    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_stall".into(),
            status: TaskStatus::InProgress,
            ..make_task()
        }])
        .agents([agent])
        .config(config_with_global_stall(600_000))
        .state(state_running(
            "tsk_stall",
            "agt_long",
            "run_stall",
            55555,
            &iso_before(700_000),
        ))
        .build();

    h.reconcile().await.unwrap();

    assert!(
        h.events_of(OrchestratorEventType::OrchestratorStallDetected)
            .is_empty()
    );
    assert!(h.processes.killed_with_grace().is_empty());
    assert_eq!(h.task("tsk_stall").status, TaskStatus::InProgress);
}

#[tokio::test]
async fn an_agent_without_its_own_stall_timeout_falls_back_to_the_global_one() {
    let mut agent = Agent {
        id: "agt_default".into(),
        status: AgentStatus::Running,
        ..make_agent()
    };
    agent.config.stall_timeout_ms = None;

    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_stall2".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([agent])
        .config(config_with_global_stall(600_000))
        .state(state_running(
            "tsk_stall2",
            "agt_default",
            "run_stall2",
            55556,
            &iso_before(700_000),
        ))
        .build();
    h.runs.seed(Run {
        id: "run_stall2".into(),
        task_id: "tsk_stall2".into(),
        agent_id: "agt_default".into(),
        ..make_run()
    });

    h.reconcile().await.unwrap();

    let stalls = h.events_of(OrchestratorEventType::OrchestratorStallDetected);
    assert_eq!(stalls.len(), 1);
    match &stalls[0] {
        OrchestratorEvent::OrchestratorStallDetected { run_id } => {
            assert_eq!(run_id, "run_stall2")
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(!h.processes.killed_with_grace().is_empty());
}

#[tokio::test]
async fn the_stall_check_is_strictly_greater_than_the_timeout() {
    // Exactly at the timeout is *not* a stall — the comparison is `>`.
    let mut agent = make_agent();
    agent.config.stall_timeout_ms = Some(300_000);

    for (age, expect_stall) in [(300_000i64, false), (300_001, true)] {
        let h = Harness::builder()
            .tasks([Task {
                id: "tsk_edge".into(),
                status: TaskStatus::InProgress,
                attempts: 1,
                ..make_task()
            }])
            .agents([agent.clone()])
            .config(config_with_global_stall(600_000))
            .state(state_running(
                "tsk_edge",
                "agt_test1",
                "run_edge",
                1,
                &iso_before(age),
            ))
            .build();
        h.runs.seed(Run {
            id: "run_edge".into(),
            task_id: "tsk_edge".into(),
            ..make_run()
        });

        h.reconcile().await.unwrap();

        assert_eq!(
            !h.events_of(OrchestratorEventType::OrchestratorStallDetected)
                .is_empty(),
            expect_stall,
            "age={age}"
        );
    }
}

#[tokio::test]
async fn the_default_config_global_stall_timeout_is_ten_minutes() {
    // The value the fallback above depends on.
    assert_eq!(default_config().defaults.agent.stall_timeout_ms, 600_000);
}
