//! Autonomous task seeding and the prompt-context assembly that dispatch
//! performs — PARITY.md §7.12 and §7.5 steps 6–8.
//!
//! The TS suite has no orchestrator-level file for either: seeding is only
//! exercised indirectly through `goal-service-autonomous.test.ts`, and the
//! context assembly only through the golden prompt fixtures. Both are L1
//! contract behaviour that the differential harness depends on, so they get
//! direct cases here.

use std::sync::Arc;

use orch_core::test_support::{FakeAdapter, done_event, make_agent, make_task};
use orch_domain::{
    AUTONOMOUS_LABEL, Agent, AgentStatus, CreateMessageInput, Goal, GoalStatus, MessageChannel,
    OrchestratorEvent, OrchestratorEventType, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO};

fn autonomous_agent(id: &str, name: &str, role: Option<&str>) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        role: role.map(str::to_owned),
        autonomous: Some(true),
        status: AgentStatus::Idle,
        ..make_agent()
    }
}

fn goal(id: &str, title: &str, description: &str, assignee: Option<&str>) -> Goal {
    Goal {
        id: id.into(),
        title: title.into(),
        description: description.into(),
        status: GoalStatus::Active,
        assignee: assignee.map(str::to_owned),
        created_at: NOW_ISO.into(),
        updated_at: None,
    }
}

fn done_adapter() -> Arc<FakeAdapter> {
    let adapter = FakeAdapter::new();
    adapter.set_events([done_event(NOW_ISO)]);
    Arc::new(adapter)
}

/// The seeded task for `agent_id`, if one was created.
fn seeded(h: &Harness, agent_id: &str) -> Option<Task> {
    h.tasks
        .snapshot()
        .into_iter()
        .find(|task| task.assignee.as_deref() == Some(agent_id) && task.is_autonomous())
}

// ── seeding without a goal ───────────────────────────────────────────────

#[tokio::test]
async fn an_idle_autonomous_agent_with_no_goal_is_seeded_from_its_role() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", Some("code reviewer"))])
        .adapter(done_adapter())
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    let task = seeded(&h, "agt_1").expect("a seeded task");
    assert_eq!(task.title, "[auto] Scout: code reviewer");
    assert_eq!(
        task.description,
        "Autonomous work cycle. Agent role: code reviewer"
    );
    assert_eq!(task.labels, vec![AUTONOMOUS_LABEL.to_owned()]);
    assert_eq!(task.priority, 3);
    assert_eq!(task.goal_id, None);
    assert_eq!(task.status, TaskStatus::Todo);
}

#[tokio::test]
async fn an_agent_without_a_role_is_seeded_as_a_general_assistant() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    let task = seeded(&h, "agt_1").expect("a seeded task");
    assert_eq!(task.title, "[auto] Scout: general assistant");
    assert_eq!(
        task.description,
        "Autonomous work cycle. Agent role: general assistant"
    );
}

#[tokio::test]
async fn a_long_role_or_goal_title_is_truncated_to_sixty_characters() {
    let long_role = "r".repeat(100);
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", Some(&long_role))])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    let task = seeded(&h, "agt_1").expect("a seeded task");
    assert_eq!(task.title, format!("[auto] Scout: {}", "r".repeat(60)));
    // The *description* keeps the whole role; only the title is truncated.
    assert!(task.description.ends_with(&long_role));
}

// ── seeding from a goal ──────────────────────────────────────────────────

#[tokio::test]
async fn a_goal_assigned_to_the_agent_is_preferred_and_shapes_the_prompt() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", Some("reviewer"))])
        .goals([
            goal("goal_other", "Unassigned work", "", None),
            goal(
                "goal_mine",
                "Ship the thing",
                "Get it over the line",
                Some("agt_1"),
            ),
        ])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    let task = seeded(&h, "agt_1").expect("a seeded task");
    assert_eq!(task.goal_id.as_deref(), Some("goal_mine"));
    assert_eq!(task.title, "[auto] Scout: Ship the thing");
    assert_eq!(
        task.description,
        "## GOAL (highest priority)\n\nGet it over the line\n\n---\nAgent role: reviewer"
    );
}

#[tokio::test]
async fn a_goal_with_no_description_falls_back_to_its_title() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .goals([goal("goal_1", "Ship the thing", "", Some("agt_1"))])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    assert_eq!(
        seeded(&h, "agt_1").unwrap().description,
        "## GOAL (highest priority)\n\nShip the thing\n\n---\nAgent role: general assistant"
    );
}

#[tokio::test]
async fn an_unassigned_active_goal_is_taken_when_the_agent_has_none_of_its_own() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .goals([goal("goal_free", "Free work", "d", None)])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;
    assert_eq!(
        seeded(&h, "agt_1").unwrap().goal_id.as_deref(),
        Some("goal_free")
    );
}

#[tokio::test]
async fn two_agents_do_not_claim_the_same_goal_in_one_pass() {
    let h = Harness::builder()
        .agents([
            autonomous_agent("agt_1", "One", None),
            autonomous_agent("agt_2", "Two", None),
        ])
        .goals([
            goal("goal_a", "A", "a", None),
            goal("goal_b", "B", "b", None),
        ])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    let first = seeded(&h, "agt_1").unwrap().goal_id;
    let second = seeded(&h, "agt_2").unwrap().goal_id;
    assert!(first.is_some() && second.is_some());
    assert_ne!(first, second, "both agents claimed the same goal");
}

#[tokio::test]
async fn only_active_goals_are_eligible() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .goals([Goal {
            status: GoalStatus::Paused,
            ..goal("goal_paused", "Paused", "d", Some("agt_1"))
        }])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;

    // Seeded, but role-based: a paused goal is not work to pick up.
    let task = seeded(&h, "agt_1").expect("a seeded task");
    assert_eq!(task.goal_id, None);
}

/// Fails the `n`-th `save`, passing everything else through.
struct FailingSaveTaskStore {
    inner: Arc<orch_core::test_support::FakeTaskStore>,
    saves: std::sync::atomic::AtomicUsize,
    fail_save: usize,
}

#[async_trait::async_trait]
impl orch_core::storage::TaskStore for FailingSaveTaskStore {
    async fn list(
        &self,
        filter: &orch_core::storage::TaskFilter,
    ) -> orch_core::error::Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> orch_core::error::Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> orch_core::error::Result<()> {
        let count = self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if count == self.fail_save {
            return Err(orch_core::error::Error::other("disk-full"));
        }
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

// ── the skip conditions ──────────────────────────────────────────────────

#[tokio::test]
async fn a_non_autonomous_agent_is_never_seeded() {
    let h = Harness::builder()
        .agents([Agent {
            autonomous: None,
            ..autonomous_agent("agt_1", "Scout", None)
        }])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;
    assert!(seeded(&h, "agt_1").is_none());
}

#[tokio::test]
async fn a_busy_autonomous_agent_is_not_seeded() {
    let h = Harness::builder()
        .agents([Agent {
            status: AgentStatus::Running,
            ..autonomous_agent("agt_1", "Scout", None)
        }])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;
    assert!(seeded(&h, "agt_1").is_none());
}

#[tokio::test]
async fn an_agent_that_already_has_non_terminal_work_is_not_seeded() {
    for status in [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Retrying,
        TaskStatus::Review,
    ] {
        let h = Harness::builder()
            .agents([autonomous_agent("agt_1", "Scout", None)])
            .tasks([Task {
                id: "tsk_existing".into(),
                assignee: Some("agt_1".into()),
                status,
                ..make_task()
            }])
            .build();

        h.orchestrator.seed_autonomous_tasks().await;
        assert_eq!(h.tasks.len(), 1, "seeded despite open work in {status}");
    }
}

#[tokio::test]
async fn an_agent_whose_only_work_is_terminal_is_seeded_again() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .tasks([Task {
            id: "tsk_done".into(),
            assignee: Some("agt_1".into()),
            status: TaskStatus::Done,
            ..make_task()
        }])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;
    assert!(seeded(&h, "agt_1").is_some());
}

#[tokio::test]
async fn the_cooldown_stops_a_second_seed_inside_thirty_seconds() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .build();

    h.orchestrator.seed_autonomous_tasks().await;
    assert_eq!(h.tasks.len(), 1);

    // The seeded task is terminal, so only the cooldown can stop a re-seed.
    let mut task = seeded(&h, "agt_1").unwrap();
    task.status = TaskStatus::Done;
    h.tasks.seed(task);

    h.orchestrator.seed_autonomous_tasks().await;
    assert_eq!(h.tasks.len(), 1, "re-seeded inside the cooldown");

    // Past the window it seeds again.
    h.clock.advance_ms(30_001);
    h.orchestrator.seed_autonomous_tasks().await;
    assert_eq!(h.tasks.len(), 2);
}

#[tokio::test]
async fn a_seeding_failure_is_reported_per_agent_and_does_not_stop_the_pass() {
    // An unknown assignee makes `TaskService::create` refuse, which is the
    // shape of failure the TS error path is written for.
    // The pass reads the task list once and then creates one task per agent,
    // so failing the *first save* targets the first agent's create precisely —
    // a shared fault flag would take the list down and abort the whole pass.
    let h = Harness::builder()
        .agents([
            autonomous_agent("agt_broken", "Broken", None),
            autonomous_agent("agt_ok", "Fine", None),
        ])
        .wrap_task_store(|inner| {
            Arc::new(FailingSaveTaskStore {
                inner,
                saves: std::sync::atomic::AtomicUsize::new(0),
                fail_save: 1,
            }) as Arc<dyn orch_core::storage::TaskStore>
        })
        .build();
    h.orchestrator.seed_autonomous_tasks().await;

    let errors = h.events_of(OrchestratorEventType::OrchestratorError);
    assert_eq!(errors.len(), 1);
    match &errors[0] {
        OrchestratorEvent::OrchestratorError { context, fatal, .. } => {
            assert!(
                context.starts_with("autonomous task for agent agt_"),
                "context was {context:?}"
            );
            assert!(!fatal);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    // The other agent was still seeded.
    assert_eq!(h.tasks.len(), 1);
}

#[tokio::test]
async fn seeding_is_skipped_entirely_when_the_watch_asked_for_it() {
    // `serve --once` sets `skip_autonomous_seeding`, because seeding new work
    // would make "process all todo tasks and exit" never terminate.
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .adapter(done_adapter())
        .build();

    h.start_watch_skipping_seeding().await.unwrap();
    assert!(seeded(&h, "agt_1").is_none());
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn seeding_runs_during_a_normal_watch() {
    let h = Harness::builder()
        .agents([autonomous_agent("agt_1", "Scout", None)])
        .adapter(done_adapter())
        .build();

    h.start_watch().await.unwrap();
    h.settle().await;
    assert!(seeded(&h, "agt_1").is_some());
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn a_seeded_task_is_auto_approved_on_completion() {
    // The `autonomous` label is one of the two things that imply auto-approve
    // (PARITY.md §7.8 step 5), independently of the agent's policy.
    let mut agent = autonomous_agent("agt_1", "Scout", None);
    agent.config.approval_policy = Some(orch_domain::ApprovalPolicy::Manual);

    let h = Harness::builder()
        .agents([agent])
        .adapter(done_adapter())
        .build();

    h.start_watch().await.unwrap();
    h.settle().await;

    let task = seeded(&h, "agt_1").expect("a seeded task");
    assert_eq!(task.status, TaskStatus::Done);
    h.orchestrator.stop().await.unwrap();
}

// ── dispatch-time context assembly (§7.5 steps 6–8) ──────────────────────

#[tokio::test]
async fn dispatch_drains_the_agents_mailbox_and_announces_each_delivery() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .with_messages()
        .adapter(done_adapter())
        .build();

    let sent = h
        .orchestrator
        .deps()
        .message_service
        .as_ref()
        .expect("a message service")
        .send(CreateMessageInput {
            channel: MessageChannel::Direct,
            from_agent_id: "cli".into(),
            to_agent_id: Some("agt_test1".into()),
            subject: "heads up".into(),
            body: "mind the gap".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(sent.len(), 1);
    h.clear_events();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let delivered = h.events_of(OrchestratorEventType::MessageDelivered);
    assert_eq!(delivered.len(), 1);
    match &delivered[0] {
        OrchestratorEvent::MessageDelivered {
            message_id,
            to_agent_id,
            task_id,
        } => {
            assert_eq!(message_id, &sent[0].id);
            assert_eq!(to_agent_id, "agt_test1");
            assert_eq!(task_id, "tsk_test1");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    // The message reached the prompt context.
    let context = h.templates.contexts().into_iter().next().expect("a render");
    let messages = context.messages.expect("messages in the context");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].subject, "heads up");
}

#[tokio::test]
async fn dispatch_without_a_message_service_leaves_the_context_without_messages() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let context = h.templates.contexts().into_iter().next().expect("a render");
    assert_eq!(context.messages, None);
}

#[tokio::test]
async fn dispatch_injects_the_shared_context_when_a_store_is_wired() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();
    h.context
        .seed("architecture", "hexagonal, ports and adapters");

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let context = h.templates.contexts().into_iter().next().expect("a render");
    let shared = context.shared_context.expect("shared context");
    assert_eq!(
        shared,
        vec![(
            "architecture".to_owned(),
            "hexagonal, ports and adapters".to_owned()
        )]
    );
}

#[tokio::test]
async fn dispatch_without_a_context_store_leaves_the_shared_context_absent() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .without_context()
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let context = h.templates.contexts().into_iter().next().expect("a render");
    assert_eq!(context.shared_context, None);
}

#[tokio::test]
async fn dispatch_builds_the_goal_context_from_the_goal_and_its_tasks() {
    let h = Harness::builder()
        .tasks([
            Task {
                goal_id: Some("goal_1".into()),
                ..make_task()
            },
            Task {
                id: "tsk_sibling".into(),
                title: "Sibling".into(),
                status: TaskStatus::Done,
                goal_id: Some("goal_1".into()),
                ..make_task()
            },
        ])
        .agents([make_agent()])
        .goals([goal("goal_1", "Ship", "Get it out", None)])
        .adapter(done_adapter())
        .build();
    h.context.seed("goal_1-progress", "halfway");

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let context = h.templates.contexts().into_iter().next().expect("a render");
    let goal_context = context.goal.expect("goal context");
    assert_eq!(goal_context.id, "goal_1");
    assert_eq!(goal_context.title, "Ship");
    assert_eq!(goal_context.description, "Get it out");
    assert_eq!(goal_context.status, GoalStatus::Active);
    assert_eq!(goal_context.progress.as_deref(), Some("halfway"));
    // `[<status>] <title>` per linked task, in store order.
    assert_eq!(
        goal_context.task_names,
        vec!["[todo] Test task".to_owned(), "[done] Sibling".to_owned()]
    );
}

#[tokio::test]
async fn a_retry_quotes_the_previous_failure_back_to_the_agent() {
    let h = Harness::builder()
        .tasks([Task {
            status: TaskStatus::Retrying,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();
    // A failed earlier run with output to quote.
    h.runs.seed(orch_domain::Run {
        id: "run_prev".into(),
        task_id: "tsk_test1".into(),
        agent_id: "agt_test1".into(),
        attempt: 1,
        status: orch_domain::RunStatus::Failed,
        finished_at: Some(NOW_ISO.into()),
        error: Some("tests failed".into()),
        ..orch_core::test_support::make_run()
    });
    h.runs.seed_events(
        "run_prev",
        [orch_domain::RunEvent {
            timestamp: NOW_ISO.into(),
            event_type: orch_domain::RunEventType::AgentOutput,
            data: serde_json::Value::String("assertion failed".into()),
        }],
    );

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let context = h.templates.contexts().into_iter().next().expect("a render");
    assert_eq!(context.attempt, Some(2));
    let retry = context.retry.expect("retry context");
    assert_eq!(retry.previous_error, "tests failed");
    assert_eq!(retry.previous_output, "assertion failed");
}

#[tokio::test]
async fn the_first_attempt_carries_no_retry_context() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let context = h.templates.contexts().into_iter().next().expect("a render");
    // `None`, never `Some(0)` or `Some(1)` — `0` is truthy in Liquid.
    assert_eq!(context.attempt, None);
    assert_eq!(context.retry, None);
}

#[tokio::test]
async fn dispatch_passes_the_orch_environment_variables_to_the_adapter() {
    let mut agent = make_agent();
    agent.config.env = Some(
        [("MY_VAR".to_owned(), "mine".to_owned())]
            .into_iter()
            .collect(),
    );
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    let call = h.adapter.execute_calls.first().expect("an execute call");
    assert_eq!(call.env.get("MY_VAR").map(String::as_str), Some("mine"));
    assert_eq!(
        call.env.get("ORCH_AGENT_ID").map(String::as_str),
        Some("agt_test1")
    );
    assert_eq!(
        call.env.get("ORCH_AGENT_NAME").map(String::as_str),
        Some("TestAgent")
    );
    assert_eq!(
        call.env.get("ORCH_TASK_ID").map(String::as_str),
        Some("tsk_test1")
    );
    assert_eq!(call.workspace, std::path::PathBuf::from("/tmp/ws"));
}
