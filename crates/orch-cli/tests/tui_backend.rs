//! `DaemonBackend` against a real project tree and its test daemon.
//!
//! Every case drives the backend the way the dashboard will: constructed on
//! the runtime against a daemon reached over its Unix socket, then used from
//! a plain thread that is **not** driving the runtime, so the
//! `Handle::block_on` inside each method is legal. The daemon's engine is
//! used only to seed and to peek; every assertion about the dashboard goes
//! through the backend.

mod tui_support;

use std::sync::Arc;
use std::time::Duration;

use orch_cli::context::{CliContext, EnvFlags};
use orch_cli::output::Styles;
use orch_cli::tui::DaemonBackend;
use tui_support::{TestDaemon, on_draw_thread};
use orch_core::clock::epoch_ms_from_iso;
use orch_core::storage::ConfigStore;
use orch_domain::{
    AgentStatus, ApprovalPolicy, CreateGoalInput, CreateTeamInput, DEFAULT_MESSAGE_TTL_MS,
    GoalStatus, MessageChannel, MessageStatus, NotificationPreferences, OrchestratorEventType,
    ReasoningEffort, Run, RunEvent, RunEventType, RunStatus, TaskStatus, TuiPaletteName,
    WorkspaceMode, default_config,
};
use orch_engine::FullEngine;
use orch_infra::storage::{FileConfigStore, Paths};
use orch_tui::backend::{
    AgentFields, AgentOpts, CreateTaskOpts, GoalFields, HistoryEntry, MessageTarget, TaskFields,
    TuiBackend,
};

/// Serializes the cases that touch a `global.yml`. Each project keeps its own
/// (inside the project directory), so this only guards against a future case
/// sharing one.
static GLOBAL_CONFIG_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A project with a written `config.yml` and its test daemon (dispatch OFF,
/// no maintenance timer, `global.yml` inside the project).
async fn project() -> (tempfile::TempDir, Arc<TestDaemon>) {
    let temp = project_tree().await;
    let daemon = Arc::new(TestDaemon::start_default(temp.path()).await);
    (temp, daemon)
}

/// [`project`]'s tree, so a test that needs its own daemon seams can start
/// one over it.
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
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    temp
}

async fn launch(daemon: Arc<TestDaemon>) -> DaemonBackend {
    daemon.launch().await
}

/// Poll the backend until `ready` holds for something it returned, or fail.
async fn poll_until<T, F, P>(mut backend: DaemonBackend, what: &str, poll: F, ready: P) -> (DaemonBackend, Vec<T>)
where
    T: Send + 'static,
    F: Fn(&mut DaemonBackend) -> Vec<T> + Send + Sync + Copy + 'static,
    P: Fn(&[T]) -> bool,
{
    let mut seen = Vec::new();
    for _ in 0..400 {
        let (returned, polled) = on_draw_thread(backend, move |backend| poll(backend)).await;
        backend = returned;
        seen.extend(polled);
        if ready(&seen) {
            return (backend, seen);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{what} never arrived");
}

// ── launch ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn launch_reports_the_project_context_and_leaves_dispatch_alone() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let context = backend.context();
    assert_eq!(context.project_name, "tui-e2e");
    assert_eq!(context.version, "9.9.9");
    assert_eq!(context.default_adapter, "claude");
    assert_eq!(context.initial_max_concurrent, 6);
    // A fresh daemon starts with dispatch OFF, and opening the dashboard
    // does not change it: the header shows IDLE.
    assert!(!context.initial_watch_active);
    assert!(!context.observer_mode, "there is no observer mode");
    assert_eq!(context.watch_error, None);

    let mut probe = orch_client::Client::connect(&engine.client_config())
        .await
        .expect("a second client");
    let status = probe.status().await.expect("daemon status");
    assert!(!status.dispatch.dispatch_on, "the dashboard turned dispatch on");

    drop(backend);
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_against_a_daemon_with_dispatch_on_shows_watching() {
    let temp = project_tree().await;
    let engine = Arc::new(
        TestDaemon::start(
            temp.path(),
            orch_daemon::testing::DaemonHarness::builder()
                .maintenance_timer(false)
                .dispatch(orch_daemon::DispatchMode::On),
        )
        .await,
    );
    let backend = launch(Arc::clone(&engine)).await;
    assert!(backend.context().initial_watch_active);
    drop(backend);
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

    // `ui.bootstrap` reads `global.yml` when the dashboard opens.
    let backend = launch(Arc::clone(&engine)).await;

    let context = backend.context();
    assert_eq!(context.initial_palette, TuiPaletteName::Forest);
    assert_eq!(context.initial_activity_filter, "errors");
    assert!(!context.initial_notifications.toast);
    assert!(context.initial_notifications.bell);

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
                    prompt_mode: None,
                    role_summary: None,
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

    // An unrecognized preset is refused by the daemon now (it was stored as
    // given when the dashboard wrote the file itself), and nothing changes.
    let (_backend, refused) = on_draw_thread(backend, |backend| {
        backend.save_activity_filter("nonsense")
    })
    .await;
    let error = refused.expect_err("an unknown preset is refused");
    assert!(error.contains("unknown preset"), "{error}");
    assert_eq!(
        engine
            .global_config_store
            .read()
            .await
            .expect("read global")
            .tui
            .activity_filter,
        "tools"
    );

    drop(temp);
}

// ── polled streams ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn poll_events_surfaces_what_the_daemon_emitted() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    let (backend, _) = on_draw_thread(backend, |backend| {
        backend.poll_events();
        backend
            .create_task("Observed", CreateTaskOpts::default())
            .expect("create");
    })
    .await;

    // The feed is a second session (`logs.follow`), so the event arrives a
    // moment after the reply that created it.
    let (_backend, events) = poll_until(
        backend,
        "task:created",
        |backend| backend.poll_events(),
        |events| {
            events
                .iter()
                .any(|event| event.event_type() == OrchestratorEventType::TaskCreated)
        },
    )
    .await;
    assert!(!events.is_empty());
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

    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_history_is_empty_for_a_project_with_no_finished_runs() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let (_backend, batches) = on_draw_thread(backend, |backend| backend.poll_history()).await;
    assert!(batches.is_empty());

    drop(temp);
}

// `poll_model_catalog` has no test here on purpose: the first poll starts a
// real probe, which execs `claude --help`, `cursor-agent --list-models` and six
// more, and reads the developer's `~/.codex`. No test may touch the real HOME,
// and the property an earlier version of this test asserted — "not probed until
// polled" — is not a contract: the dashboard loads the catalog on mount,
// and `App::tick` polls on the very first tick, so the
// deferral only spares a facade consumer that never polls at all.

// ── watch mode = the daemon's dispatch switch (design D8) ────────────────

#[tokio::test(flavor = "multi_thread")]
async fn watch_and_pause_move_the_daemons_dispatch_switch() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;
    let mut probe = orch_client::Client::connect(&engine.client_config())
        .await
        .expect("a second client");

    let (backend, started) = on_draw_thread(backend, |backend| backend.start_watch()).await;
    started.expect("/watch");
    assert!(probe.status().await.expect("status").dispatch.dispatch_on);

    let (_backend, paused) = on_draw_thread(backend, |backend| backend.stop_watch()).await;
    paused.expect("/pause");
    assert!(!probe.status().await.expect("status").dispatch.dispatch_on);
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_run_comes_back_to_the_caller() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;

    // The dashboard has no renderer for `orchestrator:error`, so a refusal
    // that is knowable before any agent starts must be this call's result
    // (BACKEND.md §3).
    let (backend, refused) = on_draw_thread(backend, |backend| backend.run_task("tsk_missing")).await;
    let error = refused.expect_err("a missing task cannot run");
    assert!(error.contains("tsk_missing"), "{error}");
    // An ordinary refusal does not end the session.
    assert_eq!(backend.session_loss().failure(), None);
    drop(temp);
}

/// The shell agent the dispatch tests launch: a real child that waits.
async fn sleeper(engine: &TestDaemon) -> (orch_domain::Agent, orch_domain::Task) {
    let mut client = orch_client::Client::connect(&engine.client_config())
        .await
        .expect("seeding client");
    let root = engine.root().to_str().expect("utf-8 root").to_owned();
    let agent = client
        .agent_create(
            serde_json::from_value(serde_json::json!({
                "name": "sleeper", "adapter": "shell", "command": "exec /bin/sleep 60",
                "workspace_mode": "shared",
                "env": [{"name": "HOME", "value": root},
                        {"name": "PATH", "value": "/usr/bin:/bin"}]
            }))
            .expect("agent args"),
        )
        .await
        .expect("create the sleeper")
        .agent;
    let task = client
        .task_create(
            serde_json::from_value(serde_json::json!({"title": "sleep", "assignee": agent.id}))
                .expect("task args"),
        )
        .await
        .expect("create the task");
    (agent, task)
}

/// R10 through the dashboard: `run` launches a real child in the daemon and
/// returns without waiting for it; `cancel` stops that live process before it
/// answers, whichever client asked.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_from_the_dashboard_kills_the_live_agent_process() {
    let (temp, engine) = project().await;
    let (_agent, task) = sleeper(&engine).await;
    let backend = launch(Arc::clone(&engine)).await;

    let id = task.id.clone();
    let (backend, ran) = on_draw_thread(backend, move |backend| backend.run_task(&id)).await;
    ran.expect("run the task");
    let mut pid = None;
    for _ in 0..500 {
        if let Some(entry) = engine.state.read().await.expect("state").running.get(&task.id) {
            pid = Some(entry.pid);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid = pid.expect("the daemon launched the task");
    assert!(engine.processes.is_alive(pid), "the agent is running");

    let id = task.id.clone();
    let (_backend, cancelled) = on_draw_thread(backend, move |backend| backend.cancel_task(&id)).await;
    cancelled.expect("cancel");
    assert!(
        !engine.processes.is_alive(pid),
        "cancel answered while the agent process was still alive"
    );
    assert_eq!(
        engine.task_service.get(&task.id).await.expect("read").status,
        TaskStatus::Cancelled
    );
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_reads_the_daemons_current_state_every_time() {
    let (temp, engine) = project().await;
    let backend = launch(Arc::clone(&engine)).await;
    let (backend, before) = on_draw_thread(backend, |backend| backend.refresh_state()).await;
    assert_ne!(before.expect("state").onboarding_completed, Some(true));

    // Another client changes it.
    let mut other = orch_client::Client::connect(&engine.client_config())
        .await
        .expect("another client");
    other
        .state_complete_onboarding(orch_proto::ops::args::StateCompleteOnboardingArgs {})
        .await
        .expect("complete onboarding");

    let (_backend, after) = on_draw_thread(backend, |backend| backend.refresh_state()).await;
    assert_eq!(after.expect("state").onboarding_completed, Some(true));
    drop(temp);
}

// ── no daemon (goal row 3; spec R4–R6) ───────────────────────────────────

/// The daemon going away under an open dashboard ends the session: the next
/// call fails, the loss latch holds the frozen message, and the draw loop's
/// stop flag is up so `tui::command` can show the no-daemon screen. Nothing
/// is retried and nothing is started.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_going_away_ends_the_session_with_the_frozen_message() {
    let temp = project_tree().await;
    let daemon = TestDaemon::start_default(temp.path()).await;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let backend = DaemonBackend::launch(
        daemon.connector(),
        "9.9.9",
        daemon.root().to_path_buf(),
        Arc::clone(&stop),
    )
    .await
    .expect("launch");
    let root = daemon.root().display().to_string();
    let loss = backend.session_loss();
    daemon.shutdown().await;

    let (backend, refreshed) = on_draw_thread(backend, |backend| backend.refresh_tasks()).await;
    let refreshed = refreshed.expect_err("a call after the daemon stopped succeeded");
    eprintln!("after stop: {refreshed}");
    // The feed notices too, even with no call in flight.
    for _ in 0..300 {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(stop.load(std::sync::atomic::Ordering::SeqCst), "the draw loop was not stopped");
    let message = loss.failure().expect("the loss is recorded");
    assert!(
        message.starts_with(&format!("  ✕ No orch daemon is running for {root} (")),
        "{message}"
    );
    assert!(message.ends_with("Start it with: orch daemon start"), "{message}");

    // Every later call fails fast without touching a socket.
    let (_backend, later) = on_draw_thread(backend, |backend| backend.refresh_goals()).await;
    assert_eq!(later.unwrap_err(), "the orch daemon is no longer reachable");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn launching_with_no_daemon_is_the_frozen_message_and_exit_seven() {
    let temp = project_tree().await;
    let scratch = tui_support::short_scratch();
    let root = temp.path().canonicalize().expect("root");
    let connector = tui_support::absent_daemon_connector(&root, scratch.path());

    let error = DaemonBackend::launch(
        connector,
        "9.9.9",
        root.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .await
    .expect_err("no daemon answers");
    assert_eq!(error.exit_code(), 7);
    assert_eq!(
        error.render(false),
        format!(
            "  ✕ No orch daemon is running for {} (no socket)\n    Start it with: orch daemon start",
            root.display()
        )
    );
    // A stale socket file (nothing accepts on it) is the same message with
    // its own detail.
    let stale = std::os::unix::net::UnixListener::bind(scratch.path().join("s")).expect("bind");
    drop(stale);
    let error = DaemonBackend::launch(
        tui_support::absent_daemon_connector(&root, scratch.path()),
        "9.9.9",
        root.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .await
    .expect_err("a stale socket answers nothing");
    assert_eq!(error.exit_code(), 7);
    let text = error.render(false);
    assert!(text.contains("No orch daemon is running for"), "{text}");
    assert!(text.ends_with("Start it with: orch daemon start"), "{text}");
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_of_another_protocol_names_both_versions_and_the_restart() {
    let temp = project_tree().await;
    let daemon = TestDaemon::start(
        temp.path(),
        orch_daemon::testing::DaemonHarness::builder()
            .maintenance_timer(false)
            .versions(orch_proto::LOCAL_PROTOCOL + 1, "0.99.0"),
    )
    .await;
    let error = DaemonBackend::launch(
        daemon.connector(),
        "9.9.9",
        daemon.root().to_path_buf(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .await
    .expect_err("skew is refused");
    assert_eq!(error.exit_code(), 7);
    let text = error.render(false);
    assert!(text.contains("daemon is orch 0.99.0 protocol"), "{text}");
    assert!(
        text.ends_with("Start it with: orch daemon stop, then orch daemon start"),
        "{text}"
    );
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
    let temp = project_tree().await;
    // `run.timings` samples the daemon's clock, so the test holds it.
    let clock = Arc::new(orch_core::test_support::FixedClock::at(TIMING_BASE + 9_000));
    let engine = Arc::new(
        TestDaemon::start(
            temp.path(),
            orch_daemon::testing::DaemonHarness::builder()
                .maintenance_timer(false)
                .clock(Arc::clone(&clock) as Arc<dyn orch_core::clock::Clock>),
        )
        .await,
    );

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

    // The daemon derives timings from its own tasks and state in one
    // coordinated read, so they go into the stores.
    for task in &tasks {
        engine.tasks.save(task).await.expect("save task");
    }
    engine.state.write(&state).await.expect("write state");

    let backend = launch(Arc::clone(&engine)).await;
    let (backend, near) = on_draw_thread(backend, |backend| {
        backend
            .refresh_timings(&[], &[], None, 0)
            .expect("timings")
    })
    .await;
    clock.set_ms(TIMING_BASE + 9_000_000);
    let (_backend, far) = on_draw_thread(backend, |backend| {
        backend
            .refresh_timings(&[], &[], None, 0)
            .expect("timings")
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
        TestDaemon::start(
            temp.path(),
            orch_daemon::testing::DaemonHarness::builder()
                .maintenance_timer(false)
                .clock(Arc::clone(&clock) as Arc<dyn orch_core::clock::Clock>),
        )
        .await,
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

    // A fresh dashboard over the same daemon reads the same spans: the value
    // comes from `updated_at` in the store, not from anything the session held.
    let backend = launch(Arc::clone(&engine)).await;
    let (_backend, reread) =
        on_draw_thread(backend, move |backend| timing_spans(backend, EDITED)).await;
    assert_eq!(reread, edited, "a fresh backend over the same store");

    drop(temp);
}

/// The one task's and the one goal's `Elapsed*` span at `now_ms`, read the way
/// a refresh does: entities first, then the timings for them.
fn timing_spans(backend: &mut DaemonBackend, now_ms: i64) -> (Option<i64>, Option<i64>) {
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

    drop(temp);
}
