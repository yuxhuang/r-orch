//! `EngineBackend` against a real project tree.
//!
//! Every case drives the backend the way the dashboard will: constructed on
//! the runtime, then used from a plain thread that is **not** driving it, so
//! the `Handle::block_on` inside each method is legal. A test that called the
//! methods directly from `#[tokio::test]` would panic, which is precisely the
//! mistake this file exists to make impossible.

mod common;

use std::sync::Arc;
use std::time::Duration;

use orch_cli::context::{CliContext, EnvFlags};
use orch_cli::output::Styles;
use orch_cli::tui::EngineBackend;
use orch_core::clock::epoch_ms_from_iso;
use orch_core::storage::ConfigStore;
use orch_domain::{
    AgentStatus, ApprovalPolicy, CreateGoalInput, CreateTeamInput, DEFAULT_MESSAGE_TTL_MS,
    GoalStatus, MessageChannel, MessageStatus, NotificationPreferences, OrchestratorEventType,
    ReasoningEffort, Run, RunEvent, RunEventType, RunStatus, TaskStatus, TuiPaletteName,
    WorkspaceMode, default_config,
};
use orch_engine::{EngineSeams, FullEngine, build_full_with};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_tui::backend::{
    AgentFields, AgentOpts, CreateTaskOpts, GoalFields, HistoryEntry, MessageTarget, TaskFields,
    TuiBackend,
};

/// Serializes the two cases that touch the **global** config.
///
/// Everything else here is isolated by its own tempdir, but `global.yml` is a
/// single file per machine — `~/.orchestry/global.yml`, or wherever
/// `ORCHESTRY_GLOBAL_CONFIG_PATH` points. Two `multi_thread` tests in this
/// binary write it, so they raced: observed as `palette: Ocean` where `Forest`
/// was expected, about 1 in 20 full-workspace runs.
///
/// A mutex rather than per-test isolation because `std::env::set_var` is
/// process-global and racy — the same class of bug. The *path* is redirected
/// into `target/` for the whole workspace by `[env]` in `.cargo/config.toml`,
/// which is what stops these tests from rewriting the developer's own
/// preferences; this only stops them from fighting each other.
static GLOBAL_CONFIG_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A project with a written `config.yml`, which is what `require_init` wants.
async fn project() -> (tempfile::TempDir, Arc<FullEngine>) {
    let temp = project_tree().await;
    let engine = Arc::new(build_isolated(temp.path()).await.expect("build_full"));
    (temp, engine)
}

/// [`project`]'s tree, so a test that needs its own engine seams can build one
/// over it.
async fn project_tree() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_core::test_support::assert_hermetic(temp.path());
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

    temp
}

/// `build_full`, with `global.yml` kept inside the project directory.
///
/// Every build in this file goes through here. The default resolution is
/// `~/.orchestry/global.yml` — one file per machine — and two tests below
/// write it, so using `build_full` directly meant rewriting the developer's
/// own preferences. An env var does not fix it: set through `[env]` in
/// `.cargo/config.toml` it only applies when cargo is run from inside the
/// workspace, and the writer came straight back for any other cwd (verified).
/// A seam passed explicitly cannot be bypassed.
async fn build_isolated(root: &std::path::Path) -> orch_core::error::Result<FullEngine> {
    build_isolated_with(root, EngineSeams::default()).await
}

/// [`build_isolated`] with the rest of the seams chosen by the caller — a
/// [`FixedClock`](orch_core::test_support::FixedClock), for a test that
/// asserts the exact `updated_at` a service writes.
async fn build_isolated_with(
    root: &std::path::Path,
    seams: EngineSeams,
) -> orch_core::error::Result<FullEngine> {
    build_full_with(
        root,
        EngineSeams {
            global_config_path: Some(root.join(".orchestry/global.yml")),
            ..seams
        },
    )
    .await
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
    EngineBackend::launch(engine, "9.9.9")
        .await
        .with_model_discovery(common::offline_model_discovery())
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
    // the observer badge rather than an error.
    assert_eq!(context.watch_error, None);
    assert!(!backend.is_watch_active());
    assert!(!engine.orchestrator.is_owner());

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_reads_the_tui_preferences_out_of_the_global_config() {
    let (temp, engine) = project().await;
    let _serial = GLOBAL_CONFIG_TESTS.lock().await;
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
    let engine = Arc::new(build_isolated(temp.path()).await.expect("rebuild"));
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

    // review → done, which is what the approve callback does directly.
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
                    // Empty strings are absent.
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

// ── messaging ────────────────────────────────────────────────────────────

/// A `shell` agent that never execs anything real.
async fn shell_agent(engine: &FullEngine, name: &str) -> orch_domain::Agent {
    engine
        .agent_service
        .create(orch_domain::CreateAgentInput {
            name: name.into(),
            adapter: "shell".into(),
            command: Some("true".into()),
            ..Default::default()
        })
        .await
        .expect("create agent")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_message_lands_in_the_recipients_inbox_with_the_cli_defaults() {
    let (temp, engine) = project().await;
    let one = shell_agent(&engine, "one").await;
    let two = shell_agent(&engine, "two").await;
    let backend = launch(Arc::clone(&engine)).await;

    let to = one.id.clone();
    let (_backend, ids) = on_draw_thread(backend, move |backend| {
        backend
            .send_message(MessageTarget::Agent(to), "ship it")
            .expect("send")
    })
    .await;
    assert_eq!(ids.len(), 1, "a direct send is always one message");

    let inbox = engine
        .message_service
        .list_pending_for_agent(&one.id)
        .await
        .expect("inbox");
    assert_eq!(inbox.len(), 1);
    let message = &inbox[0];
    // The id handed back is the id stored.
    assert_eq!(message.id, ids[0]);
    assert_eq!(message.channel, MessageChannel::Direct);
    assert_eq!(message.body, "ship it");
    assert_eq!(message.status, MessageStatus::Pending);
    // The three `orch msg send` defaults, mirrored: sender `cli`, no subject
    // (which the service renders as `(no subject)`) and the 24-hour TTL.
    assert_eq!(message.from_agent_id, "cli");
    assert_eq!(message.subject, "(no subject)");
    let created = epoch_ms_from_iso(&message.created_at).expect("created_at");
    let expires = epoch_ms_from_iso(message.expires_at.as_deref().expect("expires_at"))
        .expect("expires_at parses");
    assert_eq!(expires - created, DEFAULT_MESSAGE_TTL_MS as i64);

    // Addressed to `one` only.
    assert!(
        engine
            .message_service
            .list_pending_for_agent(&two.id)
            .await
            .expect("inbox")
            .is_empty()
    );

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_broadcast_fans_out_to_every_enabled_agent() {
    let (temp, engine) = project().await;
    let one = shell_agent(&engine, "one").await;
    let two = shell_agent(&engine, "two").await;
    let off = shell_agent(&engine, "off").await;
    engine
        .agent_service
        .disable(&off.id)
        .await
        .expect("disable");
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, ids) = on_draw_thread(backend, |backend| {
        backend
            .send_message(MessageTarget::Broadcast, "all hands")
            .expect("broadcast")
    })
    .await;
    // One id per recipient, and a disabled agent is not one. `cli` is not an
    // agent, so nothing is dropped as the sender either.
    assert_eq!(ids.len(), 2);
    // Fan-out follows `AgentStore::list` order, which is observable.
    let stored = engine.message_service.list_all().await.expect("list");
    let addressed: Vec<&str> = ids
        .iter()
        .map(|id| {
            stored
                .iter()
                .find(|m| &m.id == id)
                .and_then(|m| m.to_agent_id.as_deref())
                .expect("every returned id is stored and addressed")
        })
        .collect();
    assert_eq!(addressed, vec![one.id.as_str(), two.id.as_str()]);

    for agent in [&one, &two] {
        let inbox = engine
            .message_service
            .list_pending_for_agent(&agent.id)
            .await
            .expect("inbox");
        assert_eq!(inbox.len(), 1, "{} got {inbox:?}", agent.name);
        assert_eq!(inbox[0].channel, MessageChannel::Broadcast);
        assert_eq!(inbox[0].body, "all hands");
        assert!(ids.contains(&inbox[0].id));
    }
    assert!(
        engine
            .message_service
            .list_pending_for_agent(&off.id)
            .await
            .expect("inbox")
            .is_empty()
    );

    // Naming a disabled agent *directly*, on the other hand, delivers: only
    // the fan-out skips them, and the message waits in the inbox.
    let to = off.id.clone();
    let (_backend, ids) = on_draw_thread(backend, move |backend| {
        backend
            .send_message(MessageTarget::Agent(to), "wake up")
            .expect("a disabled agent can still be messaged directly")
    })
    .await;
    assert_eq!(ids.len(), 1);
    assert_eq!(
        engine
            .message_service
            .list_pending_for_agent(&off.id)
            .await
            .expect("inbox")
            .len(),
        1
    );

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

/// The CLI and the on-disk format must not change: the same body sent through
/// the dashboard and through `orch msg send` has to produce the same record.
///
/// This drives `commands::msg::send` rather than the `orch` binary on purpose
/// — it is the code path that builds the CLI's `CreateMessageInput`, so this
/// compares the two inputs where they differ, with no process to spawn.
#[tokio::test(flavor = "multi_thread")]
async fn a_tui_sent_message_is_byte_identical_on_disk_to_a_cli_sent_one() {
    let (temp, engine) = project().await;
    let one = shell_agent(&engine, "one").await;
    let backend = launch(Arc::clone(&engine)).await;

    let to = one.id.clone();
    let (_backend, ids) = on_draw_thread(backend, move |backend| {
        backend
            .send_message(MessageTarget::Agent(to), "same body, same record")
            .expect("send")
    })
    .await;

    orch_cli::commands::msg::send(
        &engine,
        &CliContext::new(
            temp.path().to_path_buf(),
            None,
            true,
            true,
            false,
            EnvFlags::default(),
        ),
        &Styles::plain(),
        orch_cli::commands::msg::MsgSendArgs {
            to_agent_id: &one.id,
            body: "same body, same record",
            subject: None,
            from: None,
            ttl_ms: None,
            reply_to: None,
        },
    )
    .await
    .expect("cli send");

    let stored = engine.message_service.list_all().await.expect("list");
    assert_eq!(stored.len(), 2, "one from each path");
    let cli_sent = stored
        .iter()
        .find(|m| m.id != ids[0])
        .expect("the CLI's record");

    // Read the files, not the structs: the claim is about the format on disk.
    let (from_tui, from_cli) = (
        read_message_json(&engine, &ids[0]).await,
        read_message_json(&engine, &cli_sent.id).await,
    );
    // Everything but the id and the two timestamps, which cannot match.
    for volatile in ["id", "created_at", "expires_at"] {
        assert!(
            from_tui.get(volatile).is_some(),
            "{volatile} is missing, so excluding it proves nothing"
        );
    }
    assert_eq!(without_volatile(from_tui), without_volatile(from_cli));

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

async fn read_message_json(engine: &FullEngine, id: &str) -> serde_json::Value {
    let path = engine.paths.message_path(id).expect("message path");
    let raw = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&raw).expect("a message file is JSON")
}

fn without_volatile(mut message: serde_json::Value) -> serde_json::Value {
    let object = message.as_object_mut().expect("a JSON object");
    for volatile in ["id", "created_at", "expires_at"] {
        object.remove(volatile);
    }
    message
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_over_the_cap_is_cut_by_the_service_and_not_by_the_backend() {
    let (temp, engine) = project().await;
    let one = shell_agent(&engine, "one").await;
    let backend = launch(Arc::clone(&engine)).await;

    let to = one.id.clone();
    let (_backend, ids) = on_draw_thread(backend, move |backend| {
        backend
            .send_message(MessageTarget::Agent(to), &"b".repeat(5_000))
            .expect("send")
    })
    .await;

    // 4000 UTF-16 units, applied by the service's `truncate_utf16` — the
    // backend hands it the raw 5000 and never cuts anything itself.
    let inbox = engine
        .message_service
        .list_pending_for_agent(&one.id)
        .await
        .expect("inbox");
    assert_eq!(inbox[0].id, ids[0]);
    let utf16_len: usize = inbox[0].body.chars().map(char::len_utf16).sum();
    assert_eq!(utf16_len, 4_000);

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_message_reports_the_services_own_text_and_writes_nothing() {
    let (temp, engine) = project().await;
    let one = shell_agent(&engine, "one").await;
    let backend = launch(Arc::clone(&engine)).await;

    let to = one.id.clone();
    let (backend, errors) = on_draw_thread(backend, move |backend| {
        [
            backend
                .send_message(MessageTarget::Agent(to), "   ")
                .expect_err("empty body"),
            backend
                .send_message(MessageTarget::Broadcast, "")
                .expect_err("empty body"),
            backend
                .send_message(MessageTarget::Agent("agt_ghost".into()), "hello?")
                .expect_err("unknown recipient"),
        ]
    })
    .await;
    assert_eq!(
        errors,
        [
            "Message body is required",
            "Message body is required",
            "Recipient agent not found: agt_ghost",
        ]
    );
    assert!(
        engine
            .message_service
            .list_all()
            .await
            .expect("list")
            .is_empty()
    );

    // Nobody to tell, on the other hand, is a success: `orch msg broadcast`
    // prints `0 agent(s)` and exits 0 rather than failing.
    engine.agent_service.remove(&one.id).await.expect("remove");
    let (_backend, ids) = on_draw_thread(backend, |backend| {
        backend
            .send_message(MessageTarget::Broadcast, "anyone?")
            .expect("an empty fan-out is not a failure")
    })
    .await;
    assert!(ids.is_empty());

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn in_observer_mode_a_message_still_reaches_the_other_processs_tree() {
    // Messaging is the one mutation that needs no orchestrator: `send` writes
    // files and takes no project lock, so an observer-mode dashboard can post
    // to a mailbox the *other* process will drain.
    let (temp, engine) = project().await;
    let one = shell_agent(&engine, "one").await;
    let held = orch_engine::FileLock::new(engine.paths.lock_path());
    assert!(orch_engine::ProjectLock::acquire(&held).await.acquired);

    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.context().observer_mode, "expected observer mode");

    let to = one.id.clone();
    let (_backend, ids) = on_draw_thread(backend, move |backend| {
        backend
            .send_message(MessageTarget::Agent(to), "from the observer")
            .expect("observer mode must still send")
    })
    .await;

    // Read back through a second engine on the same tree — the stand-in for
    // the process that owns the orchestrator. The message is on disk, not just
    // in this process's memory.
    let external = build_isolated(temp.path()).await.expect("second engine");
    let inbox = external
        .message_service
        .list_pending_for_agent(&one.id)
        .await
        .expect("inbox");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].id, ids[0]);
    assert_eq!(inbox[0].body, "from the observer");

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}

// ── settings ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn the_settings_writes_persist_where_the_next_launch_will_read_them() {
    let (temp, engine) = project().await;
    let _serial = GLOBAL_CONFIG_TESTS.lock().await;
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
    // running orchestrator keeps its own clone.
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

    let reopened = Arc::new(build_isolated(temp.path()).await.expect("rebuild"));
    assert_eq!(reopened.config.scheduling.max_concurrent_agents, 3);

    // An unrecognized preset is *stored*, not refused: the field is a plain
    // string and is not validated.
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
    // seven in the second.
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
            error_kind: None,
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
    // A `preparing` run is not history: only succeeded/failed are kept.
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
            error_kind: None,
            tokens: None,
        })
        .await
        .expect("save preparing");

    let backend = launch(Arc::clone(&engine)).await;

    // The load is spawned at launch; poll until both batches have landed.
    let mut batches: Vec<Vec<HistoryEntry>> = Vec::new();
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

// `poll_model_catalog` has no test here on purpose: the first poll starts a
// real probe, which execs `claude --help`, `cursor-agent --list-models` and six
// more, and reads the developer's `~/.codex`. No test may touch the real HOME,
// and the property an earlier version of this test asserted — "not probed until
// polled" — is not a contract: the dashboard loads the catalog on mount,
// and `App::tick` polls on the very first tick, so the
// deferral only spares a facade consumer that never polls at all.

// ── watch mode ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn stop_watch_waits_for_the_stop_and_releases_the_lock() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;
    assert!(engine.orchestrator.is_owner());

    let (backend, _) = on_draw_thread(backend, |backend| {
        backend.poll_events();
        backend.stop_watch().expect("stop watch");
    })
    .await;

    // Awaited, not spawned: by the time the call returns the orchestrator has
    // stopped and the lock is gone. Spawning would mean a failed stop could only
    // be reported as `orchestrator:error`, which nothing renders — so `/pause` would claim to
    // have paused a loop that is still running.
    assert!(
        !engine.orchestrator.is_owner(),
        "stop_watch returned before the lock was released"
    );
    assert!(!backend.is_watch_active());

    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_stop_watch_reports_the_error_and_keeps_the_watch_flag() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.is_watch_active());

    // `stop()` persists the cleared state, so a state file that cannot be
    // written is the one way it fails. A *directory* in its place is the
    // cheapest unwritable file: the running orchestrator would simply
    // recreate a deleted one.
    let state_path = engine.paths.state_path();
    let _ = tokio::fs::remove_file(&state_path).await;
    tokio::fs::create_dir_all(&state_path)
        .await
        .expect("block state.json");

    let (backend, result) = on_draw_thread(backend, |backend| backend.stop_watch()).await;

    let err = result.expect_err("a failed stop must reach the caller");
    assert!(!err.is_empty(), "the message is the whole contract");
    // The flag describes the watch loop, so it must not claim a pause that
    // did not happen.
    assert!(
        backend.is_watch_active(),
        "watch_active was cleared despite the failure"
    );

    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dispatch_that_cannot_take_the_lock_fails_the_call_with_the_holders_pid() {
    let (temp, engine) = project().await;
    // Another process owns the orchestrator, so this one launches as observer.
    let held = orch_engine::FileLock::new(engine.paths.lock_path());
    assert!(orch_engine::ProjectLock::acquire(&held).await.acquired);

    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.context().observer_mode);

    // A lock conflict is knowable before any agent starts, so it must come
    // back to the caller: the dashboard has no renderer for
    // `orchestrator:error`, and an error delivered only on the event stream
    // would be invisible (BACKEND.md §3).
    let (backend, results) = on_draw_thread(backend, |backend| {
        [
            ("run_all", backend.run_all()),
            ("run_task", backend.run_task("tsk_missing")),
            ("start_watch", backend.start_watch()),
        ]
        .map(|(name, result)| (name, result.err()))
    })
    .await;

    for (name, error) in &results {
        let error = error
            .as_deref()
            .unwrap_or_else(|| panic!("{name} returned Ok while the lock was held"));
        assert!(
            error.starts_with("Orchestrator already running (PID:"),
            "{name} reported {error:?}"
        );
    }
    // A refused `start_watch` must not leave the dashboard claiming otherwise.
    assert!(!backend.is_watch_active());

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn in_observer_mode_a_refresh_sees_another_process_writing_state() {
    let (temp, engine) = project().await;
    // Another process owns the orchestrator and the lock.
    let held = orch_engine::FileLock::new(engine.paths.lock_path());
    assert!(orch_engine::ProjectLock::acquire(&held).await.acquired);

    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.context().observer_mode);

    // A second engine on the same tree stands in for that other process.
    let external = build_isolated(temp.path()).await.expect("second engine");
    let mut state = external.state.read().await.expect("read state");
    state.pid = Some(4242);
    external.state.write(&state).await.expect("write state");

    let (_backend, observed) =
        on_draw_thread(backend, |backend| backend.refresh_state().expect("refresh")).await;

    // `refresh_state` must re-read `state.json` on every call and never cache:
    // in observer mode the periodic poll is the only thing moving, so a cached
    // read would freeze the dashboard at launch for as long as it is open.
    assert_eq!(observed.pid, Some(4242));

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn in_observer_mode_the_disk_observer_feeds_the_event_queue() {
    // The wiring `tui::command::run` does when watch mode is refused. Without
    // it an observer-mode dashboard re-reads the stores on every refresh but
    // never sees a live event.
    let (temp, engine) = project().await;
    let held = orch_engine::FileLock::new(engine.paths.lock_path());
    assert!(orch_engine::ProjectLock::acquire(&held).await.acquired);

    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.context().observer_mode, "expected observer mode");
    let observer = orch_cli::tui::command::attach_observer(&engine, backend.observer_sink());

    // Another process starts a run: a `running` entry appears in state.json,
    // which is all the observer needs to announce it.
    let external = build_isolated(temp.path()).await.expect("second engine");
    let mut state = external.state.read().await.expect("read state");
    state.running.insert(
        "tsk_watched".to_owned(),
        orch_domain::RunningEntry {
            remote: None,
            run_id: "run_watched".to_owned(),
            agent_id: "agt_watched".to_owned(),
            task_id: "tsk_watched".to_owned(),
            pid: 4242,
            started_at: "2026-10-13T16:00:00.000Z".to_owned(),
            last_event_at: "2026-10-13T16:00:00.000Z".to_owned(),
        },
    );
    external.state.write(&state).await.expect("write state");

    // The observer polls on its own interval, so this waits rather than
    // assuming the first poll has already happened.
    let mut backend = backend;
    let mut started = None;
    for _ in 0..80 {
        let (returned, events) = on_draw_thread(backend, |backend| backend.poll_events()).await;
        backend = returned;
        started = events.into_iter().find(|event| {
            matches!(
                event,
                orch_domain::OrchestratorEvent::AgentStarted { run_id, .. }
                    if run_id == "run_watched"
            )
        });
        if started.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        started.is_some(),
        "the observer never reported the external run"
    );

    // Dropping the handle is the `unsubscribe`: nothing more arrives.
    drop(observer);
    let mut state = external.state.read().await.expect("read state");
    state.running.clear();
    external.state.write(&state).await.expect("write state");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_backend, events) = on_draw_thread(backend, |backend| backend.poll_events()).await;
    assert!(
        events.is_empty(),
        "the observer kept polling after its handle was dropped: {events:?}"
    );

    orch_engine::ProjectLock::release(&held).await;
    drop(temp);
}

// ── Timings (R11, R15) ───────────────────────────────────────────────────

/// A round instant with room either side; every number below is an offset
/// from it, so the assertions read as the durations they are.
const TIMING_BASE: i64 = 1_700_000_000_000;

fn at(offset_ms: i64) -> String {
    orch_core::clock::iso_from_epoch_ms(TIMING_BASE + offset_ms)
}

/// A run record for `task_id` started at `started_at`, which is the only
/// field the timings read. The id is a counter because a run id is a
/// filename: an ISO timestamp in it is rejected as an identifier.
fn timing_run(task_id: &str, started_at: &str) -> Run {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Run {
        id: format!("run_{seq:04}"),
        task_id: task_id.to_owned(),
        started_at: started_at.to_owned(),
        ..orch_core::test_support::make_run()
    }
}

/// A task whose status and last update the timings read; the store never
/// sees it, because `refresh_timings` is told which entities a refresh
/// committed.
fn timing_task(id: &str, status: TaskStatus, updated_at: &str) -> orch_domain::Task {
    orch_domain::Task {
        id: id.to_owned(),
        status,
        updated_at: updated_at.to_owned(),
        ..orch_core::test_support::make_task()
    }
}

fn running_entry(task_id: &str, started_at: &str) -> orch_domain::RunningEntry {
    orch_domain::RunningEntry {
        run_id: format!("run_live_{task_id}"),
        agent_id: "agt_1".to_owned(),
        task_id: task_id.to_owned(),
        pid: 4242,
        started_at: started_at.to_owned(),
        last_event_at: started_at.to_owned(),
        remote: None,
    }
}

/// Task Started is the earliest **retained** run start, from every run record
/// on disk plus the live `running` entry — never the ten-run history sample,
/// which cannot see an old first attempt.
#[tokio::test(flavor = "multi_thread")]
async fn task_start_times() {
    let (temp, engine) = project().await;

    // A retried task: two runs, and a third attempt live in state.json.
    for offset in [1_000, 3_000] {
        engine
            .runs
            .save(&timing_run("tsk_retry", &at(offset)))
            .await
            .expect("save run");
    }
    // The oldest run in the project, plus a much later retry. Ten newer runs
    // follow, so this first start is outside any ten-run sample.
    for offset in [500, 40_000] {
        engine
            .runs
            .save(&timing_run("tsk_old", &at(offset)))
            .await
            .expect("save run");
    }
    for index in 0..10 {
        engine
            .runs
            .save(&timing_run("tsk_fill", &at(10_000 + index * 1_000)))
            .await
            .expect("save run");
    }
    engine
        .runs
        .save(&timing_run("tsk_bad", "not-a-timestamp"))
        .await
        .expect("save run");
    engine
        .runs
        .save(&timing_run("tsk_unended", &at(2_000)))
        .await
        .expect("save run");
    engine
        .runs
        .save(&timing_run("tsk_future", &at(9_500)))
        .await
        .expect("save run");

    let tasks = vec![
        timing_task("tsk_retry", TaskStatus::InProgress, &at(3_000)),
        // Dispatched, no run record written yet.
        timing_task("tsk_live", TaskStatus::InProgress, &at(4_000)),
        timing_task("tsk_cold", TaskStatus::Todo, &at(0)),
        timing_task("tsk_old", TaskStatus::Done, &at(60_500)),
        timing_task("tsk_bad", TaskStatus::InProgress, &at(3_000)),
        timing_task("tsk_unended", TaskStatus::Cancelled, "whenever"),
        timing_task("tsk_future", TaskStatus::InProgress, &at(9_500)),
    ];
    let mut state = orch_domain::default_state();
    state
        .running
        .insert("tsk_retry".into(), running_entry("tsk_retry", &at(4_000)));
    state
        .running
        .insert("tsk_live".into(), running_entry("tsk_live", &at(4_000)));

    let backend = launch(Arc::clone(&engine)).await;
    let (_backend, (near, far)) = on_draw_thread(backend, move |backend| {
        let near = backend
            .refresh_timings(&tasks, &[], Some(&state), TIMING_BASE + 9_000)
            .expect("timings");
        let far = backend
            .refresh_timings(&tasks, &[], Some(&state), TIMING_BASE + 9_000_000)
            .expect("timings");
        (near, far)
    })
    .await;
    assert_eq!(near.sampled_at_ms, TIMING_BASE + 9_000);

    let timing = |snapshot: &orch_tui::backend::TimingSnapshot, id: &str| {
        let timing = snapshot.tasks[id];
        (
            timing.started_at_ms.map(|ms| ms - TIMING_BASE),
            timing.elapsed_ms,
        )
    };

    // The earliest run wins over the later retry and over the live entry.
    assert_eq!(timing(&near, "tsk_retry"), (Some(1_000), Some(8_000)));
    // Live only: the `running` entry is the start.
    assert_eq!(timing(&near, "tsk_live"), (Some(4_000), Some(5_000)));
    // Never started is blank, not a zero-length run.
    assert_eq!(timing(&near, "tsk_cold"), (None, None));
    // Outside any ten-run sample, and still the start.
    assert_eq!(timing(&near, "tsk_old"), (Some(500), Some(60_000)));
    // A terminal span ends at the last update, so `now` cannot move it.
    assert_eq!(timing(&far, "tsk_old"), (Some(500), Some(60_000)));
    // A malformed run start is unknown, never "never started at all".
    assert_eq!(timing(&near, "tsk_bad"), (None, None));
    // Terminal with no readable last update: no fabricated endpoint.
    assert_eq!(timing(&near, "tsk_unended"), (Some(2_000), None));
    // A start in the future clamps to zero rather than going negative.
    assert_eq!(timing(&near, "tsk_future"), (Some(9_500), Some(0)));
    assert_eq!(timing(&far, "tsk_future"), (Some(9_500), Some(8_990_500)));

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}

/// The `Elapsed*` contract (R15): a terminal row's span ends at its **last
/// update**, so `now` never moves it and a later metadata edit does — through
/// the real `TaskService`/`GoalService` update paths, on a clock the test
/// holds, and the same after a fresh backend reopens the project.
#[tokio::test(flavor = "multi_thread")]
async fn terminal_elapsed_follows_last_update_edits() {
    const START: i64 = TIMING_BASE;
    const ENDED: i64 = TIMING_BASE + 6_000;
    const EDITED: i64 = TIMING_BASE + 86_407_000;

    let temp = project_tree().await;
    let clock = Arc::new(orch_core::test_support::FixedClock::at(START));
    let engine = Arc::new(
        build_isolated_with(
            temp.path(),
            EngineSeams {
                clock: Arc::clone(&clock) as Arc<dyn orch_core::clock::Clock>,
                ..EngineSeams::default()
            },
        )
        .await
        .expect("build_full"),
    );

    // Created at START, so the goal starts there; the task's start is its run
    // record, written at the same instant.
    let task = engine
        .task_service
        .create(orch_domain::CreateTaskInput {
            title: "Terminal task".into(),
            ..Default::default()
        })
        .await
        .expect("create task");
    let goal = engine
        .goal_service
        .create(CreateGoalInput {
            title: "Terminal goal".into(),
            ..Default::default()
        })
        .await
        .expect("create goal");
    engine
        .runs
        .save(&timing_run(
            &task.id,
            &orch_core::clock::iso_from_epoch_ms(START),
        ))
        .await
        .expect("save run");

    // Both end 6 000 ms in, through the real status transitions.
    clock.set_ms(ENDED);
    engine
        .task_service
        .cancel(&task.id)
        .await
        .expect("cancel task");
    engine
        .goal_service
        .update_status(&goal.id, GoalStatus::Abandoned, false)
        .await
        .expect("abandon goal");

    let backend = launch(Arc::clone(&engine)).await;
    let (backend, spans) = on_draw_thread(backend, move |backend| {
        let ended = timing_spans(backend, ENDED);
        // `now` alone, a tick and a whole day later: a terminal span is
        // frozen against the clock.
        let later = timing_spans(backend, ENDED + 2_000);
        let much_later = timing_spans(backend, START + 86_400_000);
        (ended, later, much_later)
    })
    .await;
    assert_eq!(spans.0, (Some(6_000), Some(6_000)), "at the last update");
    assert_eq!(spans.1, (Some(6_000), Some(6_000)), "two seconds later");
    assert_eq!(spans.2, (Some(6_000), Some(6_000)), "a day later");

    // A title edit a day later moves `updated_at`, and with it the span. That
    // is the disclosed behaviour, not a completion duration.
    clock.set_ms(EDITED);
    let (task_id, goal_id) = (task.id.clone(), goal.id.clone());
    let (backend, edited) = on_draw_thread(backend, move |backend| {
        backend
            .update_task(
                &task_id,
                TaskFields {
                    title: Some("Terminal task, retitled".into()),
                    ..Default::default()
                },
            )
            .expect("edit task");
        backend
            .update_goal(
                &goal_id,
                GoalFields {
                    title: Some("Terminal goal, retitled".into()),
                    ..Default::default()
                },
            )
            .expect("edit goal");
        timing_spans(backend, EDITED)
    })
    .await;
    assert_eq!(
        edited,
        (Some(86_407_000), Some(86_407_000)),
        "the edit moved the last update, so the span follows it"
    );
    drop(backend);
    engine.orchestrator.stop().await.expect("stop");

    // A fresh dashboard over the same files reads the same spans: the value
    // comes from `updated_at` on disk, not from anything the session held.
    let reopened = Arc::new(
        build_isolated_with(
            temp.path(),
            EngineSeams {
                clock: Arc::clone(&clock) as Arc<dyn orch_core::clock::Clock>,
                ..EngineSeams::default()
            },
        )
        .await
        .expect("rebuild"),
    );
    let backend = launch(Arc::clone(&reopened)).await;
    let (_backend, reread) =
        on_draw_thread(backend, move |backend| timing_spans(backend, EDITED)).await;
    assert_eq!(reread, edited, "a fresh backend over the same store");

    reopened.orchestrator.stop().await.expect("stop");
    drop(temp);
}

/// The one task's and the one goal's `Elapsed*` span at `now_ms`, read the way
/// a refresh does: entities first, then the timings for them.
fn timing_spans(backend: &mut EngineBackend, now_ms: i64) -> (Option<i64>, Option<i64>) {
    let tasks = backend.refresh_tasks().expect("tasks");
    let goals = backend.refresh_goals().expect("goals");
    let state = backend.refresh_state().expect("state");
    let snapshot = backend
        .refresh_timings(&tasks, &goals, Some(&state), now_ms)
        .expect("timings");
    (
        snapshot.tasks[&tasks[0].id].elapsed_ms,
        snapshot.goals[&goals[0].id].elapsed_ms,
    )
}

/// An unreadable run record fails the refresh. The dashboard then shows
/// "Task timings unavailable" and leaves every span unknown, rather than
/// asserting that nothing ever started.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_timings_reports_an_unreadable_run_store() {
    let (temp, engine) = project().await;
    engine
        .runs
        .save(&timing_run("tsk_1", &at(0)))
        .await
        .expect("save run");
    tokio::fs::write(engine.paths.runs_dir().join("broken.json"), "{not json")
        .await
        .expect("write a corrupt run");

    let backend = launch(Arc::clone(&engine)).await;
    let (_backend, error) = on_draw_thread(backend, |backend| {
        let tasks = backend.refresh_tasks().expect("tasks");
        backend
            .refresh_timings(&tasks, &[], None, TIMING_BASE)
            .err()
    })
    .await;
    let error = error.expect("an unreadable run store is an error");
    assert!(error.contains("broken.json"), "{error}");

    engine.orchestrator.stop().await.expect("stop");
    drop(temp);
}
