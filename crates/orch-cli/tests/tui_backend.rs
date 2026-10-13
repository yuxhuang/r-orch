//! `EngineBackend` against a real project tree.
//!
//! Every case drives the backend the way the dashboard will: constructed on
//! the runtime, then used from a plain thread that is **not** driving it, so
//! the `Handle::block_on` inside each method is legal. A test that called the
//! methods directly from `#[tokio::test]` would panic, which is precisely the
//! mistake this file exists to make impossible.

use std::sync::Arc;
use std::time::Duration;

use orch_cli::tui::{
    AgentFields, AgentOpts, CreateTaskOpts, EngineBackend, GoalFields, TaskFields,
};
use orch_core::storage::ConfigStore;
use orch_domain::{
    AgentStatus, ApprovalPolicy, CreateGoalInput, CreateTeamInput, GoalStatus,
    NotificationPreferences, OrchestratorEventType, ReasoningEffort, Run, RunEvent, RunEventType,
    RunStatus, TaskStatus, TuiPaletteName, WorkspaceMode, default_config,
};
use orch_engine::{FullEngine, build_full};
use orch_infra::storage::{FileConfigStore, Paths};

/// A project with a written `config.yml`, which is what `require_init` wants.
async fn project() -> (tempfile::TempDir, Arc<FullEngine>) {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.root())
        .await
        .expect("create .orchestry");

    let mut config = default_config();
    config.project.name = "tui-e2e".into();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    // Long enough that the watch loop started by `launch` never self-fires.
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    let engine = Arc::new(build_full(temp.path()).await.expect("build_full"));
    (temp, engine)
}

/// Run `body` on a thread that is not driving the runtime, as the draw loop
/// is, and hand back whatever it returns.
async fn on_draw_thread<T, F>(mut backend: EngineBackend, body: F) -> (EngineBackend, T)
where
    T: Send + 'static,
    F: FnOnce(&mut EngineBackend) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let value = std::thread::scope(|scope| {
            scope
                .spawn(|| body(&mut backend))
                .join()
                .expect("the draw thread panicked")
        });
        (backend, value)
    })
    .await
    .expect("the blocking task panicked")
}

async fn launch(engine: Arc<FullEngine>) -> EngineBackend {
    EngineBackend::launch(engine, "9.9.9").await
}

// ── launch ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn launch_starts_watch_mode_and_reports_the_project_context() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let context = backend.context();
    assert_eq!(context.project_name, "tui-e2e");
    assert_eq!(context.version, "9.9.9");
    assert_eq!(context.default_adapter, "claude");
    assert_eq!(context.initial_max_concurrent, 6);
    assert!(context.initial_watch_active);
    assert!(!context.observer_mode);
    assert_eq!(context.watch_error, None);
    assert!(backend.is_watch_active());
    // Watch mode really did take the lock.
    assert!(engine.orchestrator.is_owner());

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_falls_back_to_observer_mode_when_the_lock_is_held() {
    let (temp, engine) = project().await;
    // Another process owns the orchestrator.
    let held = orch_engine::FileLock::new(engine.paths.lock_path());
    assert!(
        orch_engine::ProjectLock::acquire(&held).await.acquired,
        "the test could not take the lock"
    );

    let backend = launch(Arc::clone(&engine)).await;

    let context = backend.context();
    assert!(context.observer_mode);
    assert!(!context.initial_watch_active);
    // Suppressed: observer mode is not a failure state, so the header shows
    // the observer badge rather than an error (`tui.ts:314`).
    assert_eq!(context.watch_error, None);
    assert!(!backend.is_watch_active());
    assert!(!engine.orchestrator.is_owner());

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_reads_the_tui_preferences_out_of_the_global_config() {
    let (temp, engine) = project().await;
    let mut global = engine
        .global_config_store
        .read()
        .await
        .expect("read global");
    global.tui.palette = TuiPaletteName::Forest;
    global.tui.activity_filter = "errors".to_owned();
    global.tui.notifications = NotificationPreferences {
        toast: false,
        bell: true,
    };
    engine
        .global_config_store
        .write(&global)
        .await
        .expect("write global");

    // `build_full` reads `global.yml`, so a fresh engine is what the dashboard
    // would see on its next launch.
    let engine = Arc::new(build_full(temp.path()).await.expect("rebuild"));
    let backend = launch(Arc::clone(&engine)).await;

    let context = backend.context();
    assert_eq!(context.initial_palette, TuiPaletteName::Forest);
    assert_eq!(context.initial_activity_filter, "errors");
    assert!(!context.initial_notifications.toast);
    assert!(context.initial_notifications.bell);

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

// ── tasks ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn the_task_lifecycle_round_trips_through_the_real_stores() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, task) = on_draw_thread(backend, |backend| {
        backend
            .create_task(
                "Write the thing",
                CreateTaskOpts {
                    priority: Some(1),
                    description: Some("in detail".into()),
                    attachments: None,
                },
            )
            .expect("create")
    })
    .await;
    assert_eq!(task.title, "Write the thing");
    assert_eq!(task.priority, 1);
    assert_eq!(task.description, "in detail");

    let id = task.id.clone();
    let (backend, listed) =
        on_draw_thread(backend, |backend| backend.refresh_tasks().expect("refresh")).await;
    assert_eq!(listed.len(), 1);

    let edit_id = id.clone();
    let (backend, edited) = on_draw_thread(backend, move |backend| {
        backend
            .update_task(
                &edit_id,
                TaskFields {
                    title: Some("Renamed".into()),
                    priority: Some(4),
                    ..Default::default()
                },
            )
            .expect("update")
    })
    .await;
    assert_eq!(edited.title, "Renamed");
    assert_eq!(edited.priority, 4);

    // review → done, which is what the TS approve callback does directly.
    engine
        .task_service
        .update_status(&id, TaskStatus::InProgress)
        .await
        .expect("to in_progress");
    engine
        .task_service
        .update_status(&id, TaskStatus::Review)
        .await
        .expect("to review");

    let approve_id = id.clone();
    let (backend, _) = on_draw_thread(backend, move |backend| {
        backend.approve_task(&approve_id).expect("approve")
    })
    .await;
    assert_eq!(
        engine.task_service.get(&id).await.expect("read").status,
        TaskStatus::Done
    );

    let delete_id = id.clone();
    let (_backend, _) = on_draw_thread(backend, move |backend| {
        backend.delete_task(&delete_id).expect("delete")
    })
    .await;
    assert!(
        engine
            .task_service
            .list(&Default::default())
            .await
            .expect("list")
            .is_empty()
    );

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejecting_a_reviewed_task_records_the_feedback() {
    let (temp, engine) = project().await;
    let task = engine
        .task_service
        .create(orch_domain::CreateTaskInput {
            title: "Needs work".into(),
            ..Default::default()
        })
        .await
        .expect("create");
    engine
        .task_service
        .update_status(&task.id, TaskStatus::InProgress)
        .await
        .expect("to in_progress");
    engine
        .task_service
        .update_status(&task.id, TaskStatus::Review)
        .await
        .expect("to review");

    let backend = launch(Arc::clone(&engine)).await;
    let id = task.id.clone();
    let (_backend, _) = on_draw_thread(backend, move |backend| {
        backend.reject_task(&id, Some("add tests")).expect("reject")
    })
    .await;

    let after = engine.task_service.get(&task.id).await.expect("read");
    assert_eq!(after.status, TaskStatus::Todo);
    assert_eq!(after.feedback.as_deref(), Some("add tests"));

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_call_reports_a_message_rather_than_an_exit_code() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (_backend, err) = on_draw_thread(backend, |backend| {
        backend
            .update_task("tsk_nope", TaskFields::default())
            .expect_err("missing task")
    })
    .await;
    assert_eq!(err, "Task not found: tsk_nope");

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

// ── agents ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_lifecycle_round_trips_and_parses_its_stringly_typed_fields() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, agent) = on_draw_thread(backend, |backend| {
        backend
            .add_agent(
                "claudia",
                "claude",
                AgentOpts {
                    model: Some("claude-sonnet-4-6".into()),
                    effort: Some("high".into()),
                    role: Some("reviewer".into()),
                    approval_policy: Some("manual".into()),
                    // Empty strings are absent, as in JS.
                    command: Some(String::new()),
                    skills: Some(vec!["review".into()]),
                },
            )
            .expect("add")
    })
    .await;
    assert_eq!(agent.adapter, "claude");
    assert_eq!(agent.config.effort, Some(ReasoningEffort::High));
    assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Manual));
    assert_eq!(agent.config.command, None);
    assert_eq!(agent.role.as_deref(), Some("reviewer"));

    // `Some("")` *clears* on an update, where it meant "absent" on create.
    let id = agent.id.clone();
    let (backend, cleared) = on_draw_thread(backend, move |backend| {
        backend
            .update_agent(
                &id,
                AgentFields {
                    effort: Some(String::new()),
                    model: Some(String::new()),
                    ..Default::default()
                },
            )
            .expect("update")
    })
    .await;
    assert_eq!(cleared.config.effort, None);
    assert_eq!(cleared.config.model, None);

    let id = agent.id.clone();
    let (backend, toggled) = on_draw_thread(backend, move |backend| {
        backend.toggle_autonomous(&id, true).expect("toggle")
    })
    .await;
    assert_eq!(toggled.autonomous, Some(true));

    let id = agent.id.clone();
    let (backend, _) = on_draw_thread(backend, move |backend| {
        backend.disable_agent(&id).expect("disable")
    })
    .await;
    assert_eq!(
        engine
            .agent_service
            .get(&agent.id)
            .await
            .expect("read")
            .status,
        AgentStatus::Disabled
    );

    let id = agent.id.clone();
    let (backend, _) = on_draw_thread(backend, move |backend| {
        backend.enable_agent(&id).expect("enable")
    })
    .await;
    assert_eq!(
        engine
            .agent_service
            .get(&agent.id)
            .await
            .expect("read")
            .status,
        AgentStatus::Idle
    );

    let id = agent.id.clone();
    let (_backend, _) = on_draw_thread(backend, move |backend| {
        backend.delete_agent(&id).expect("delete")
    })
    .await;
    assert!(engine.agent_service.list().await.expect("list").is_empty());

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unparseable_enum_is_reported_before_anything_is_written() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, err) = on_draw_thread(backend, |backend| {
        backend
            .add_agent(
                "broken",
                "claude",
                AgentOpts {
                    effort: Some("extreme".into()),
                    ..Default::default()
                },
            )
            .expect_err("bad effort")
    })
    .await;
    assert_eq!(err, "Unknown effort: \"extreme\"");

    let (_backend, err) = on_draw_thread(backend, |backend| {
        backend
            .add_agent(
                "broken",
                "claude",
                AgentOpts {
                    approval_policy: Some("whenever".into()),
                    ..Default::default()
                },
            )
            .expect_err("bad policy")
    })
    .await;
    assert_eq!(err, "Unknown approval policy: \"whenever\"");
    // Nothing was created by either attempt.
    assert!(engine.agent_service.list().await.expect("list").is_empty());

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

// ── teams and goals ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn teams_and_goals_round_trip() {
    let (temp, engine) = project().await;
    let lead = engine
        .agent_service
        .create(orch_domain::CreateAgentInput {
            name: "lead".into(),
            adapter: "shell".into(),
            command: Some("true".into()),
            ..Default::default()
        })
        .await
        .expect("lead");
    let member = engine
        .agent_service
        .create(orch_domain::CreateAgentInput {
            name: "member".into(),
            adapter: "shell".into(),
            command: Some("true".into()),
            ..Default::default()
        })
        .await
        .expect("member");

    let backend = launch(Arc::clone(&engine)).await;

    let lead_id = lead.id.clone();
    let (backend, team) = on_draw_thread(backend, move |backend| {
        backend
            .create_team(CreateTeamInput {
                name: "core".into(),
                lead_agent_id: lead_id,
                ..Default::default()
            })
            .expect("create team")
    })
    .await;

    let (team_id, member_id) = (team.id.clone(), member.id.clone());
    let (backend, joined) = on_draw_thread(backend, move |backend| {
        backend.join_team(&team_id, &member_id).expect("join")
    })
    .await;
    assert_eq!(joined.members.len(), 2);

    let (team_id, member_id) = (team.id.clone(), member.id.clone());
    let (backend, promoted) = on_draw_thread(backend, move |backend| {
        backend
            .set_team_lead(&team_id, &member_id)
            .expect("set lead")
    })
    .await;
    assert_eq!(promoted.lead_agent_id, member.id);

    let (team_id, lead_id) = (team.id.clone(), lead.id.clone());
    let (backend, left) = on_draw_thread(backend, move |backend| {
        backend.leave_team(&team_id, &lead_id).expect("leave")
    })
    .await;
    assert_eq!(left.members.len(), 1);

    let team_id = team.id.clone();
    let (backend, _) = on_draw_thread(backend, move |backend| {
        backend.disband_team(&team_id).expect("disband")
    })
    .await;
    let (backend, teams) =
        on_draw_thread(backend, |backend| backend.list_teams().expect("list teams")).await;
    assert_eq!(teams.len(), 1);
    assert_eq!(teams[0].status, orch_domain::TeamStatus::Disbanded);

    // Goals.
    let (backend, goal) = on_draw_thread(backend, |backend| {
        backend
            .create_goal(CreateGoalInput {
                title: "Ship it".into(),
                description: Some("by Friday".into()),
                assignee: None,
            })
            .expect("create goal")
    })
    .await;

    let id = goal.id.clone();
    let (backend, edited) = on_draw_thread(backend, move |backend| {
        backend
            .update_goal(
                &id,
                GoalFields {
                    title: Some("Ship it twice".into()),
                    ..Default::default()
                },
            )
            .expect("update goal")
    })
    .await;
    assert_eq!(edited.title, "Ship it twice");

    // No progress note has been written, which is not a failure.
    let id = goal.id.clone();
    let (backend, progress) = on_draw_thread(backend, move |backend| {
        backend.goal_progress(&id).expect("progress")
    })
    .await;
    assert_eq!(progress, "");

    let id = goal.id.clone();
    let (backend, achieved) = on_draw_thread(backend, move |backend| {
        backend
            .update_goal_status(&id, GoalStatus::Achieved, false)
            .expect("achieve")
    })
    .await;
    assert_eq!(achieved.status, GoalStatus::Achieved);

    let id = goal.id.clone();
    let (_backend, _) = on_draw_thread(backend, move |backend| {
        backend.delete_goal(&id).expect("delete")
    })
    .await;
    assert!(
        engine
            .goal_service
            .list(&Default::default())
            .await
            .expect("list")
            .is_empty()
    );

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

// ── settings ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn the_settings_writes_persist_where_the_next_launch_will_read_them() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, _) = on_draw_thread(backend, |backend| {
        backend
            .save_palette(TuiPaletteName::Ocean)
            .expect("palette");
        backend
            .save_activity_filter("tools")
            .expect("activity filter");
        backend
            .save_notifications(NotificationPreferences {
                toast: false,
                bell: true,
            })
            .expect("notifications");
        backend.save_max_concurrent(3).expect("max concurrent");
        backend.complete_onboarding().expect("onboarding");
    })
    .await;

    let global = engine
        .global_config_store
        .read()
        .await
        .expect("read global");
    assert_eq!(global.tui.palette, TuiPaletteName::Ocean);
    assert_eq!(global.tui.activity_filter, "tools");
    assert!(!global.tui.notifications.toast);
    assert!(global.tui.notifications.bell);

    // The cap went to `config.yml`, so the *next* launch reports it; the
    // running orchestrator keeps its own clone, as in the TS.
    assert_eq!(
        engine
            .config_store
            .get("scheduling.max_concurrent_agents")
            .await
            .expect("read config"),
        Some(serde_json::json!(3))
    );
    assert_eq!(
        engine
            .state
            .read()
            .await
            .expect("read state")
            .onboarding_completed,
        Some(true)
    );
    assert_eq!(backend.context().initial_max_concurrent, 6);

    let reopened = Arc::new(build_full(temp.path()).await.expect("rebuild"));
    assert_eq!(reopened.config.scheduling.max_concurrent_agents, 3);

    // An unrecognized preset is *stored*, not refused: the field is a plain
    // string and the TS does not validate it either.
    let (_backend, _) = on_draw_thread(backend, |backend| {
        backend
            .save_activity_filter("nonsense")
            .expect("stored as given");
    })
    .await;
    assert_eq!(
        engine
            .global_config_store
            .read()
            .await
            .expect("read global")
            .tui
            .activity_filter,
        "nonsense"
    );

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

// ── polled streams ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn poll_events_surfaces_what_the_engine_emitted() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, _) = on_draw_thread(backend, |backend| {
        // Drain whatever the launch tick produced, so the assertion below is
        // about this create alone.
        backend.poll_events();
        backend
            .create_task("Observed", CreateTaskOpts::default())
            .expect("create");
    })
    .await;

    let (backend, events) = on_draw_thread(backend, |backend| backend.poll_events()).await;
    assert!(
        events
            .iter()
            .any(|event| event.event_type() == OrchestratorEventType::TaskCreated),
        "events were {:?}",
        events
            .iter()
            .map(|event| event.event_type())
            .collect::<Vec<_>>()
    );

    // An externally observed event — what the disk observer will push — comes
    // out of the same queue.
    backend.push_observed_event(orch_domain::OrchestratorEvent::OrchestratorTick {
        running: 1,
        queued: 2,
    });
    let (_backend, events) = on_draw_thread(backend, |backend| backend.poll_events()).await;
    assert_eq!(
        events.iter().map(|e| e.event_type()).collect::<Vec<_>>(),
        vec![OrchestratorEventType::OrchestratorTick]
    );

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_history_delivers_the_two_batches_newest_runs_first() {
    let (temp, engine) = project().await;

    // Twelve finished runs; only ten are read, three in the first batch and
    // seven in the second (`tui.ts:144-148`).
    for index in 0..12u32 {
        let run = Run {
            id: format!("run_{index:02}"),
            task_id: format!("tsk_{index:02}"),
            agent_id: "agt_1".into(),
            attempt: 1,
            status: if index % 2 == 0 {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            },
            // Ascending, so run_11 is the newest.
            started_at: orch_core::clock::iso_from_epoch_ms(
                1_700_000_000_000 + index as i64 * 1000,
            ),
            finished_at: None,
            workspace_path: "/tmp/ws".into(),
            prompt: "p".into(),
            pid: None,
            error: None,
            tokens: None,
        };
        engine.runs.save(&run).await.expect("save run");
        engine
            .runs
            .append_event(
                &run.id,
                &RunEvent {
                    timestamp: run.started_at.clone(),
                    event_type: RunEventType::AgentOutput,
                    data: serde_json::Value::String(format!("line {index}")),
                },
            )
            .await
            .expect("append event");
    }
    // A `preparing` run is not history: the TS keeps only succeeded/failed.
    engine
        .runs
        .save(&Run {
            id: "run_preparing".into(),
            task_id: "tsk_p".into(),
            agent_id: "agt_1".into(),
            attempt: 1,
            status: RunStatus::Preparing,
            started_at: orch_core::clock::iso_from_epoch_ms(1_800_000_000_000),
            finished_at: None,
            workspace_path: "/tmp/ws".into(),
            prompt: "p".into(),
            pid: None,
            error: None,
            tokens: None,
        })
        .await
        .expect("save preparing");

    let backend = launch(Arc::clone(&engine)).await;

    // The load is spawned at launch; poll until both batches have landed.
    let mut batches: Vec<Vec<orch_cli::tui::HistoryEntry>> = Vec::new();
    let mut backend = backend;
    for _ in 0..200 {
        let (returned, polled) = on_draw_thread(backend, |backend| backend.poll_history()).await;
        backend = returned;
        batches.extend(polled);
        if batches.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert_eq!(batches.len(), 2, "expected a first and a second batch");
    // One event per run: three newest, then the next seven.
    assert_eq!(batches[0].len(), 3);
    assert_eq!(batches[1].len(), 7);

    let first_runs: Vec<&str> = batches[0]
        .iter()
        .map(|entry| entry.run_id.as_str())
        .collect();
    // Runs are selected newest-first, but each batch is sorted oldest-first
    // before delivery.
    assert_eq!(first_runs, vec!["run_09", "run_10", "run_11"]);
    assert!(
        batches[0]
            .windows(2)
            .all(|pair| pair[0].timestamp <= pair[1].timestamp)
    );
    assert!(
        !batches[1]
            .iter()
            .any(|entry| entry.run_id == "run_preparing")
    );

    let entry = &batches[0][2];
    assert_eq!(entry.run_id, "run_11");
    assert_eq!(entry.task_id, "tsk_11");
    assert_eq!(entry.agent_id, "agt_1");
    assert_eq!(entry.event_type, "agent_output");
    // The payload is the already-serialized string, not a re-encoded JSON
    // value.
    assert_eq!(entry.data, "line 11");

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_history_is_empty_for_a_project_with_no_finished_runs() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let (_backend, batches) = on_draw_thread(backend, |backend| backend.poll_history()).await;
    assert!(batches.is_empty());

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_catalog_is_not_probed_until_it_is_first_polled() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    // The first poll starts the probe and reports nothing, which is what
    // keeps eight adapter spawns off the startup path.
    let (_backend, first) = on_draw_thread(backend, |backend| backend.poll_model_catalog()).await;
    assert!(first.is_none());

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

// ── watch mode ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn stop_watch_returns_at_once_and_the_shutdown_arrives_on_the_bus() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;
    assert!(engine.orchestrator.is_owner());

    let (backend, _) = on_draw_thread(backend, |backend| {
        backend.poll_events();
        backend.stop_watch().expect("stop watch");
    })
    .await;
    assert!(!backend.is_watch_active());

    // Spawned, so the lock release happens after the call returned.
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.orchestrator.is_owner() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("watch mode never stopped");

    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dispatch_that_cannot_take_the_lock_reports_on_the_bus_not_to_the_caller() {
    let (temp, engine) = project().await;
    // Launch in observer mode, so the orchestrator owns no lock.
    let held = orch_engine::FileLock::new(engine.paths.lock_path());
    assert!(orch_engine::ProjectLock::acquire(&held).await.acquired);

    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.context().observer_mode);

    // Fire-and-forget: the call itself succeeds.
    let (backend, result) = on_draw_thread(backend, |backend| {
        backend.poll_events();
        backend.run_all()
    })
    .await;
    assert_eq!(result, Ok(()));

    // The lock conflict arrives as an event instead.
    let mut backend = backend;
    let mut reported = None;
    for _ in 0..400 {
        let (returned, events) = on_draw_thread(backend, |backend| backend.poll_events()).await;
        backend = returned;
        reported = events.into_iter().find_map(|event| match event {
            orch_domain::OrchestratorEvent::OrchestratorError { error, context, .. }
                if context == "tui run all" =>
            {
                Some(error)
            }
            _ => None,
        });
        if reported.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let error = reported.expect("the lock conflict was never reported");
    assert!(
        error.starts_with("Orchestrator already running (PID:"),
        "error was {error:?}"
    );

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}
