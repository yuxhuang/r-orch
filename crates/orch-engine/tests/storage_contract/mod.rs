//! The 1.0 compatibility contract, expressed only through today's ports.
//! These are reference semantics, not a prescription for the v2 port shape.

use std::sync::Arc;

use futures::TryStreamExt;
use orch_core::clock::Clock;
use orch_core::storage::*;
use orch_core::test_support::{FixedClock, SeqIdGenerator, make_agent, make_run, make_task};
use orch_domain::*;
use orch_engine::AgentService;
use tokio_util::sync::CancellationToken;

pub struct Stores {
    // A backend may retain a temp root, database, or executor here until all
    // handles are dropped. Factories must provide a fresh empty project.
    pub tasks: Arc<dyn TaskStore>,
    pub goals: Arc<dyn GoalStore>,
    pub agents: Arc<dyn AgentStore>,
    pub teams: Arc<dyn TeamStore>,
    pub runs: Arc<dyn RunStore>,
    pub messages: Arc<dyn MessageStore>,
    pub context: Arc<dyn ContextStore>,
    pub config: Arc<dyn ConfigStore>,
    pub global: Arc<dyn GlobalConfigStore>,
    pub state: Arc<dyn StateStore>,
    pub clock: Arc<FixedClock>,
    pub _guard: Box<dyn Send + Sync>,
}

pub const STAMP: &str = "2025-01-01T00:00:00.000Z";

pub fn task(id: &str) -> Task {
    Task {
        id: id.into(),
        updated_at: STAMP.into(),
        ..make_task()
    }
}

pub fn agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
        name: id.into(),
        ..make_agent()
    }
}

pub fn goal(id: &str) -> Goal {
    Goal {
        id: id.into(),
        title: id.into(),
        description: "Synthetic goal".into(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: STAMP.into(),
        updated_at: Some(STAMP.into()),
    }
}

pub fn team(id: &str) -> Team {
    Team {
        id: id.into(),
        name: id.into(),
        description: None,
        status: TeamStatus::Active,
        members: vec![],
        task_pool: vec![],
        lead_agent_id: "agt_b".into(),
        created_at: STAMP.into(),
        updated_at: STAMP.into(),
        config: DEFAULT_TEAM_CONFIG,
    }
}

pub fn message(id: &str) -> Message {
    Message {
        id: id.into(),
        channel: MessageChannel::Direct,
        from_agent_id: "agt_a".into(),
        to_agent_id: Some("agt_b".into()),
        subject: id.into(),
        body: "Synthetic message".into(),
        created_at: STAMP.into(),
        expires_at: None,
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
        reply_to: None,
    }
}

pub fn run(id: &str, time: &str) -> Run {
    Run {
        id: id.into(),
        started_at: time.into(),
        workspace_path: "workspace".into(),
        ..make_run()
    }
}

fn ids<T>(items: &[T], id: impl Fn(&T) -> &str) -> Vec<String> {
    items.iter().map(|item| id(item).to_owned()).collect()
}

pub async fn empty_stores_return_no_records_and_default_documents(s: Stores) {
    assert_eq!(s.tasks.get("tsk_absent").await.unwrap(), None);
    assert_eq!(s.goals.get("goal_absent").await.unwrap(), None);
    assert_eq!(s.agents.get("agt_absent").await.unwrap(), None);
    assert_eq!(s.agents.get_by_name("absent").await.unwrap(), None);
    assert_eq!(s.teams.get("team_absent").await.unwrap(), None);
    assert_eq!(s.teams.get_by_name("absent").await.unwrap(), None);
    assert_eq!(s.runs.get("run_absent").await.unwrap(), None);
    assert_eq!(s.messages.get("msg_absent").await.unwrap(), None);
    assert_eq!(s.context.get("absent").await.unwrap(), None);
    assert!(
        s.tasks
            .list(&TaskFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.goals
            .list(&GoalFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(s.agents.list().await.unwrap().is_empty());
    assert!(s.teams.list().await.unwrap().is_empty());
    assert!(s.runs.list_all().await.unwrap().is_empty());
    assert!(s.messages.list().await.unwrap().is_empty());
    assert!(s.context.get_all().await.unwrap().is_empty());
    assert_eq!(s.config.read().await.unwrap(), default_config());
    assert_eq!(s.global.read().await.unwrap(), default_global_config());
    assert_eq!(s.state.read().await.unwrap(), default_state());
}

// Tied timestamps deliberately expose the index last-write tie breaker.
pub async fn task_create_update_delete_preserves_last_write_ties(s: Stores) {
    let store = &s.tasks;
    assert_eq!(store.get("tsk_b").await.unwrap(), None);
    for id in ["tsk_b", "tsk_a", "tsk_c"] {
        store.save(&task(id)).await.unwrap();
    }
    assert_eq!(
        ids(&store.list(&TaskFilter::default()).await.unwrap(), |t| &t
            .id),
        ["tsk_b", "tsk_a", "tsk_c"]
    );
    let updated = Task {
        title: "updated".into(),
        ..task("tsk_b")
    };
    store.save(&updated).await.unwrap();
    assert_eq!(store.get("tsk_b").await.unwrap(), Some(updated));
    assert_eq!(
        ids(&store.list(&TaskFilter::default()).await.unwrap(), |t| &t
            .id),
        ["tsk_a", "tsk_c", "tsk_b"]
    );
    assert_eq!(
        store.list(&TaskFilter::default()).await.unwrap(),
        vec![
            task("tsk_a"),
            task("tsk_c"),
            Task {
                title: "updated".into(),
                ..task("tsk_b")
            }
        ]
    );
    store.delete("tsk_c").await.unwrap();
    store.delete("tsk_c").await.unwrap();
    assert_eq!(store.get("tsk_c").await.unwrap(), None);
    assert_eq!(
        ids(&store.list(&TaskFilter::default()).await.unwrap(), |t| &t
            .id),
        ["tsk_a", "tsk_b"]
    );
}

pub async fn goal_create_update_delete_preserves_last_write_ties(s: Stores) {
    let store = &s.goals;
    for id in ["goal_b", "goal_a", "goal_c"] {
        store.save(&goal(id)).await.unwrap();
    }
    assert_eq!(
        ids(&store.list(&GoalFilter::default()).await.unwrap(), |g| &g
            .id),
        ["goal_b", "goal_a", "goal_c"]
    );
    let updated = Goal {
        title: "updated".into(),
        ..goal("goal_b")
    };
    store.save(&updated).await.unwrap();
    assert_eq!(store.get("goal_b").await.unwrap(), Some(updated));
    assert_eq!(
        ids(&store.list(&GoalFilter::default()).await.unwrap(), |g| &g
            .id),
        ["goal_a", "goal_c", "goal_b"]
    );
    assert_eq!(
        store.list(&GoalFilter::default()).await.unwrap(),
        vec![
            goal("goal_a"),
            goal("goal_c"),
            Goal {
                title: "updated".into(),
                ..goal("goal_b")
            }
        ]
    );
    store.delete("goal_c").await.unwrap();
    store.delete("goal_c").await.unwrap();
    assert_eq!(store.get("goal_c").await.unwrap(), None);
    assert_eq!(
        ids(&store.list(&GoalFilter::default()).await.unwrap(), |g| &g
            .id),
        ["goal_a", "goal_b"]
    );
}

pub async fn agent_create_update_delete_preserves_last_write_order(s: Stores) {
    for id in ["agt_b", "agt_a", "agt_c"] {
        s.agents.save(&agent(id)).await.unwrap();
    }
    assert_eq!(
        ids(&s.agents.list().await.unwrap(), |a| &a.id),
        ["agt_b", "agt_a", "agt_c"]
    );
    let updated = Agent {
        role: Some("updated".into()),
        ..agent("agt_b")
    };
    s.agents.save(&updated).await.unwrap();
    // Local capabilities are host-derived; compare the persistent fields.
    let mut read = s.agents.get_by_name("agt_b").await.unwrap().unwrap();
    read.capabilities.clear();
    assert_eq!(read, updated);
    assert_eq!(
        ids(&s.agents.list().await.unwrap(), |a| &a.id),
        ["agt_a", "agt_c", "agt_b"]
    );
    let stored = s
        .agents
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|mut a| {
            a.capabilities.clear();
            a
        })
        .collect::<Vec<_>>();
    assert_eq!(
        stored,
        vec![
            agent("agt_a"),
            agent("agt_c"),
            Agent {
                role: Some("updated".into()),
                ..agent("agt_b")
            }
        ]
    );
    s.agents.delete("agt_c").await.unwrap();
    s.agents.delete("agt_c").await.unwrap();
    assert_eq!(s.agents.get("agt_c").await.unwrap(), None);
    assert_eq!(
        ids(&s.agents.list().await.unwrap(), |a| &a.id),
        ["agt_a", "agt_b"]
    );
}

pub async fn tasks_sort_status_then_newest_and_filters_keep_tie_order(s: Stores) {
    for (id, status, time, goal) in [
        ("tsk_done", TaskStatus::Done, "z", Some("goal_a")),
        ("tsk_old", TaskStatus::Todo, "a", Some("goal_a")),
        ("tsk_old_tie", TaskStatus::Todo, "a", Some("goal_a")),
        ("tsk_new", TaskStatus::Todo, "z", Some("goal_b")),
        ("tsk_run", TaskStatus::InProgress, "a", None),
        ("tsk_retry", TaskStatus::Retrying, "a", None),
        ("tsk_review", TaskStatus::Review, "a", None),
        ("tsk_fail", TaskStatus::Failed, "a", None),
        ("tsk_cancel", TaskStatus::Cancelled, "a", None),
    ] {
        s.tasks
            .save(&Task {
                status,
                updated_at: time.into(),
                goal_id: goal.map(str::to_owned),
                ..task(id)
            })
            .await
            .unwrap();
    }
    assert_eq!(
        ids(&s.tasks.list(&TaskFilter::default()).await.unwrap(), |t| &t
            .id),
        [
            "tsk_run",
            "tsk_retry",
            "tsk_review",
            "tsk_new",
            "tsk_old",
            "tsk_old_tie",
            "tsk_done",
            "tsk_fail",
            "tsk_cancel"
        ]
    );
    assert_eq!(
        ids(
            &s.tasks
                .list(&TaskFilter {
                    status: Some(TaskStatus::Todo),
                    goal_id: Some("goal_a".into())
                })
                .await
                .unwrap(),
            |t| &t.id
        ),
        ["tsk_old", "tsk_old_tie"]
    );
}

pub async fn goals_sort_status_then_newest_with_missing_time_last(s: Stores) {
    for (id, status, time) in [
        ("goal_paused", GoalStatus::Paused, Some("z")),
        ("goal_absent", GoalStatus::Active, None),
        ("goal_old", GoalStatus::Active, Some("a")),
        ("goal_new", GoalStatus::Active, Some("z")),
        ("goal_achieved", GoalStatus::Achieved, Some("z")),
        ("goal_abandoned", GoalStatus::Abandoned, Some("z")),
    ] {
        s.goals
            .save(&Goal {
                status,
                updated_at: time.map(str::to_owned),
                ..goal(id)
            })
            .await
            .unwrap();
    }
    assert_eq!(
        ids(&s.goals.list(&GoalFilter::default()).await.unwrap(), |g| &g
            .id),
        [
            "goal_new",
            "goal_old",
            "goal_absent",
            "goal_paused",
            "goal_achieved",
            "goal_abandoned"
        ]
    );
    assert_eq!(
        ids(
            &s.goals
                .list(&GoalFilter::with_status(GoalStatus::Paused))
                .await
                .unwrap(),
            |g| &g.id
        ),
        ["goal_paused"]
    );
}

pub async fn team_lifecycle_returns_every_record_without_promising_enumeration_order(s: Stores) {
    // Teams have no index and no specified enumeration order. Capture raw
    // order at each stage but assert membership, not a host's readdir order.
    for id in ["team_b", "team_a", "team_c"] {
        s.teams.save(&team(id)).await.unwrap();
    }
    let snapshot = s.teams.list().await.unwrap();
    let mut members = ids(&snapshot, |t| &t.id);
    members.sort();
    assert_eq!(members, ["team_a", "team_b", "team_c"]);
    let updated = Team {
        description: Some("updated".into()),
        ..team("team_b")
    };
    s.teams.save(&updated).await.unwrap();
    assert_eq!(s.teams.get_by_name("team_b").await.unwrap(), Some(updated));
    let mut members = ids(&s.teams.list().await.unwrap(), |t| &t.id);
    members.sort();
    assert_eq!(members, ["team_a", "team_b", "team_c"]);
    s.teams.delete("team_c").await.unwrap();
    s.teams.delete("team_c").await.unwrap();
    assert_eq!(s.teams.get("team_c").await.unwrap(), None);
    let mut members = ids(&s.teams.list().await.unwrap(), |t| &t.id);
    members.sort();
    assert_eq!(members, ["team_a", "team_b"]);
}

pub async fn run_lists_are_complete_newest_first_and_updates_do_not_rotate(s: Stores) {
    for (id, time) in [("run_b", "b"), ("run_a", "a"), ("run_c", "c")] {
        s.runs.save(&run(id, time)).await.unwrap();
    }
    assert_eq!(
        ids(&s.runs.list_all().await.unwrap(), |r| &r.id),
        ["run_c", "run_b", "run_a"]
    );
    let updated = Run {
        error: Some("updated".into()),
        ..run("run_b", "b")
    };
    s.runs.save(&updated).await.unwrap();
    assert_eq!(s.runs.get("run_b").await.unwrap(), Some(updated));
    assert_eq!(
        ids(&s.runs.list_all().await.unwrap(), |r| &r.id),
        ["run_c", "run_b", "run_a"]
    );
    assert_eq!(
        ids(&s.runs.list_for_task("tsk_test1").await.unwrap(), |r| &r.id),
        ["run_c", "run_b", "run_a"]
    );
    assert_eq!(
        ids(&s.runs.list_for_agent("agt_test1").await.unwrap(), |r| &r
            .id),
        ["run_c", "run_b", "run_a"]
    );
    assert!(
        s.runs
            .list_for_task("tsk_missing")
            .await
            .unwrap()
            .is_empty()
    );
    // RunStore has no deletion operation in 1.0.
}

pub async fn events_preserve_append_order_duplicates_tail_and_finite_stream(s: Stores) {
    assert!(s.runs.read_events("run_a").await.unwrap().is_empty());
    let events: Vec<_> = ["z", "a", "z"]
        .into_iter()
        .map(|text| RunEvent {
            timestamp: text.into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!(text),
        })
        .collect();
    for event in &events {
        s.runs.append_event("run_a", event).await.unwrap();
    }
    assert_eq!(s.runs.read_events("run_a").await.unwrap(), events);
    assert_eq!(
        s.runs.read_events_tail("run_a", 2).await.unwrap(),
        events[1..]
    );
    assert!(
        s.runs
            .read_events_tail("run_a", 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(s.runs.read_events_tail("run_a", 20).await.unwrap(), events);
    assert_eq!(
        s.runs
            .stream_events("run_a", CancellationToken::new())
            .try_collect::<Vec<_>>()
            .await
            .unwrap(),
        events
    );
    s.runs.close_run_events("run_a");
    // Append-only: no update or delete is offered by the event port.
}

pub async fn message_lifecycle_sorts_time_with_last_write_ties(s: Stores) {
    for id in ["msg_b", "msg_a", "msg_c"] {
        s.messages.save(&message(id)).await.unwrap();
    }
    assert_eq!(
        ids(&s.messages.list().await.unwrap(), |m| &m.id),
        ["msg_b", "msg_a", "msg_c"]
    );
    let updated = Message {
        body: "updated".into(),
        ..message("msg_b")
    };
    s.messages.save(&updated).await.unwrap();
    assert_eq!(s.messages.get("msg_b").await.unwrap(), Some(updated));
    assert_eq!(
        ids(&s.messages.list().await.unwrap(), |m| &m.id),
        ["msg_a", "msg_c", "msg_b"]
    );
    assert_eq!(
        s.messages.list().await.unwrap(),
        vec![
            message("msg_a"),
            message("msg_c"),
            Message {
                body: "updated".into(),
                ..message("msg_b")
            }
        ]
    );
    s.messages.delete("msg_c").await.unwrap();
    s.messages.delete("msg_c").await.unwrap();
    assert_eq!(s.messages.get("msg_c").await.unwrap(), None);
    assert_eq!(
        ids(&s.messages.list().await.unwrap(), |m| &m.id),
        ["msg_a", "msg_b"]
    );
    s.messages
        .save(&Message {
            created_at: "0".into(),
            ..message("msg_early")
        })
        .await
        .unwrap();
    assert_eq!(
        ids(&s.messages.list().await.unwrap(), |m| &m.id),
        ["msg_early", "msg_a", "msg_b"]
    );
}

pub async fn pending_messages_exclude_broadcast_other_recipient_and_expired(s: Stores) {
    for m in [
        message("msg_pending"),
        Message {
            to_agent_id: None,
            ..message("msg_broadcast")
        },
        Message {
            to_agent_id: Some("agt_other".into()),
            ..message("msg_other")
        },
        Message {
            expires_at: Some("2000-01-01T00:00:00.000Z".into()),
            ..message("msg_expired")
        },
    ] {
        s.messages.save(&m).await.unwrap();
    }
    assert_eq!(
        ids(&s.messages.list_pending("agt_b").await.unwrap(), |m| &m.id),
        ["msg_pending"]
    );
    s.messages.mark_delivered("msg_pending").await.unwrap();
    let delivered = s.messages.get("msg_pending").await.unwrap().unwrap();
    assert_eq!(
        (delivered.status, delivered.delivered_at),
        (MessageStatus::Delivered, Some(s.clock.now_iso()))
    );
    s.messages.mark_delivered("msg_missing").await.unwrap();
    assert!(s.messages.list_pending("agt_b").await.unwrap().is_empty());
    assert_eq!(s.messages.purge_expired().await.unwrap(), 1);
    assert_eq!(s.messages.get("msg_expired").await.unwrap(), None);
}

pub async fn context_create_update_delete_is_key_ordered_and_ttl_is_read_only(s: Stores) {
    for key in ["b", "a", "c"] {
        s.context.set(key, key, None).await.unwrap();
    }
    assert_eq!(
        ids(&s.context.list().await.unwrap(), |c| &c.key),
        ["a", "b", "c"]
    );
    let created = s.context.get("b").await.unwrap().unwrap().created_at;
    s.clock.advance_ms(1);
    s.context.set("b", "updated", None).await.unwrap();
    let updated = s.context.get("b").await.unwrap().unwrap();
    assert_eq!(
        (updated.value, updated.created_at, updated.updated_at),
        ("updated".into(), created, s.clock.now_iso())
    );
    assert_eq!(
        s.context
            .get_all()
            .await
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![
            ("a".into(), "a".into()),
            ("b".into(), "updated".into()),
            ("c".into(), "c".into())
        ]
    );
    s.context.delete("c").await.unwrap();
    s.context.delete("c").await.unwrap();
    assert_eq!(
        ids(&s.context.list().await.unwrap(), |c| &c.key),
        ["a", "b"]
    );
    s.context.set("expires", "ttl", Some(10)).await.unwrap();
    s.clock.advance_ms(10);
    assert!(s.context.get("expires").await.unwrap().is_some());
    s.clock.advance_ms(1);
    assert_eq!(s.context.get("expires").await.unwrap(), None);
    assert_eq!(
        ids(&s.context.list().await.unwrap(), |c| &c.key),
        ["a", "b"]
    );
    // Rewind proves reads did not delete the expired record.
    s.clock.advance_ms(-1);
    assert!(s.context.get("expires").await.unwrap().is_some());
}

pub async fn config_round_trips_replaces_and_dot_paths_preserve_extensions(s: Stores) {
    let mut config = default_config();
    config.project.name = "synthetic".into();
    config.prompt = Some(PromptConfig {
        template: Some("Synthetic legacy {{ task.title }}".into()),
        system_template: Some("Synthetic system".into()),
        user_template: Some("Synthetic user\n{{ task.description }}".into()),
    });
    s.config.write(&config).await.unwrap();
    assert_eq!(s.config.read().await.unwrap(), config);
    s.config
        .set("legacy.nested", serde_json::json!({"flag": true}))
        .await
        .unwrap();
    s.config
        .set("project.name", serde_json::json!("changed"))
        .await
        .unwrap();
    assert_eq!(
        s.config.get("legacy.nested.flag").await.unwrap(),
        Some(serde_json::json!(true))
    );
    assert_eq!(s.config.get("project.name.child").await.unwrap(), None);
    assert_eq!(s.config.read().await.unwrap().project.name, "changed");
    s.config.write(&default_config()).await.unwrap();
    assert_eq!(s.config.get("legacy").await.unwrap(), None);
    // Single document; there is no list order or delete method.
}

pub async fn state_preserves_running_claimed_retry_order_and_all_stats_on_replace(s: Stores) {
    let mut state = default_state();
    for id in ["tsk_b", "tsk_a", "tsk_c"] {
        state.running.insert(
            id.into(),
            RunningEntry {
                run_id: format!("run_{id}"),
                agent_id: "agt_b".into(),
                task_id: id.into(),
                pid: 42,
                started_at: STAMP.into(),
                last_event_at: STAMP.into(),
                remote: None,
            },
        );
        state.claimed.insert(id.into());
        state.retry_queue.push(RetryEntry {
            task_id: id.into(),
            attempt: 2,
            due_at: STAMP.into(),
            error: "synthetic".into(),
        });
    }
    state.stats = StateStats {
        total_runs: 9,
        total_tasks_completed: 3,
        total_tasks_failed: 2,
        total_tokens: TokenUsage {
            input: 5,
            output: 7,
            reasoning: 2,
            total: 14,
            cache_read: 11,
            cache_write: 13,
        },
        total_runtime_ms: 17,
    };
    s.state.write(&state).await.unwrap();
    let read = s.state.read().await.unwrap();
    assert_eq!(read, state);
    assert_eq!(
        read.running.keys().map(String::as_str).collect::<Vec<_>>(),
        ["tsk_b", "tsk_a", "tsk_c"]
    );
    assert_eq!(
        read.claimed.iter().map(String::as_str).collect::<Vec<_>>(),
        ["tsk_b", "tsk_a", "tsk_c"]
    );
    assert_eq!(
        ids(&read.retry_queue, |r| &r.task_id),
        ["tsk_b", "tsk_a", "tsk_c"]
    );
    state.running.get_mut("tsk_b").unwrap().pid = 43;
    state.running.shift_remove("tsk_a");
    state.claimed.shift_remove("tsk_a");
    state.retry_queue.remove(1);
    s.state.write(&state).await.unwrap();
    let read = s.state.read().await.unwrap();
    assert_eq!(
        read.running.keys().map(String::as_str).collect::<Vec<_>>(),
        ["tsk_b", "tsk_c"]
    );
    assert_eq!(
        read.claimed.iter().map(String::as_str).collect::<Vec<_>>(),
        ["tsk_b", "tsk_c"]
    );
    assert_eq!(ids(&read.retry_queue, |r| &r.task_id), ["tsk_b", "tsk_c"]);
    assert_eq!(read, state);
    s.state.write(&default_state()).await.unwrap();
    assert_eq!(s.state.read().await.unwrap(), default_state());
    // The 100-entry cap is enforced by the engine, not StateStore::write.
}

pub async fn equal_score_selections_rotate_after_run_lifecycle_saves(s: Stores) {
    for id in ["agt_b", "agt_a", "agt_c"] {
        s.agents.save(&agent(id)).await.unwrap();
    }
    let service = AgentService::new(
        s.agents.clone(),
        s.state.clone(),
        Arc::new(orch_engine::events::EventBus::new()),
        default_config(),
        Arc::new(SeqIdGenerator::new()),
    );
    let mut chosen = Vec::new();
    for turn in 0..6 {
        let mut picked = service
            .find_best_agent(&task("tsk_work"))
            .await
            .unwrap()
            .unwrap();
        chosen.push(picked.id.clone());
        picked.status = AgentStatus::Running;
        s.agents.save(&picked).await.unwrap();
        s.runs
            .save(&Run {
                agent_id: picked.id.clone(),
                status: RunStatus::Running,
                ..run(&format!("run_{turn}"), STAMP)
            })
            .await
            .unwrap();
        picked.status = AgentStatus::Idle;
        // Zero successes/failures keeps scores equal, as a cancelled run does.
        picked.stats.total_runs += 1;
        s.agents.save(&picked).await.unwrap();
        s.runs
            .save(&Run {
                agent_id: picked.id.clone(),
                status: RunStatus::Cancelled,
                finished_at: Some(STAMP.into()),
                ..run(&format!("run_{turn}"), STAMP)
            })
            .await
            .unwrap();
    }
    assert_eq!(
        chosen,
        ["agt_b", "agt_a", "agt_c", "agt_b", "agt_a", "agt_c"]
    );
}

pub async fn lists_return_all_records_beyond_a_future_default_page(s: Stores) {
    for n in 0..103 {
        s.tasks.save(&task(&format!("tsk_{n:03}"))).await.unwrap();
        s.goals.save(&goal(&format!("goal_{n:03}"))).await.unwrap();
        s.agents.save(&agent(&format!("agt_{n:03}"))).await.unwrap();
        s.teams.save(&team(&format!("team_{n:03}"))).await.unwrap();
        s.runs
            .save(&run(&format!("run_{n:03}"), &format!("{n:03}")))
            .await
            .unwrap();
        s.messages
            .save(&message(&format!("msg_{n:03}")))
            .await
            .unwrap();
        s.context
            .set(&format!("key_{n:03}"), "synthetic", None)
            .await
            .unwrap();
    }
    let expected = |prefix: &str| {
        (0..103)
            .map(|n| format!("{prefix}_{n:03}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&s.tasks.list(&TaskFilter::default()).await.unwrap(), |t| &t
            .id),
        expected("tsk")
    );
    assert_eq!(
        ids(&s.goals.list(&GoalFilter::default()).await.unwrap(), |g| &g
            .id),
        expected("goal")
    );
    assert_eq!(
        ids(&s.agents.list().await.unwrap(), |a| &a.id),
        expected("agt")
    );
    let mut teams = ids(&s.teams.list().await.unwrap(), |t| &t.id);
    teams.sort();
    assert_eq!(teams, expected("team"));
    let mut runs = expected("run");
    runs.reverse();
    assert_eq!(ids(&s.runs.list_all().await.unwrap(), |r| &r.id), runs);
    assert_eq!(
        ids(&s.runs.list_for_task("tsk_test1").await.unwrap(), |r| &r.id),
        runs
    );
    assert_eq!(
        ids(&s.runs.list_for_agent("agt_test1").await.unwrap(), |r| &r
            .id),
        runs
    );
    assert_eq!(
        ids(&s.messages.list_pending("agt_b").await.unwrap(), |m| &m.id),
        expected("msg")
    );
    assert_eq!(
        ids(&s.context.list().await.unwrap(), |e| &e.key),
        expected("key")
    );
    assert_eq!(
        s.context
            .get_all()
            .await
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        expected("key")
    );
}

pub async fn global_config_replaces_preferences_without_touching_project_config(s: Stores) {
    let mut config = default_global_config();
    config.tui.activity_filter = "synthetic".into();
    s.global.write(&config).await.unwrap();
    assert_eq!(s.global.read().await.unwrap(), config);
    assert_eq!(s.config.read().await.unwrap(), default_config());
    s.global.write(&default_global_config()).await.unwrap();
    assert_eq!(s.global.read().await.unwrap(), default_global_config());
}

macro_rules! storage_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::contract::$name($factory()).await;
                    }
                };
            }
            case!(empty_stores_return_no_records_and_default_documents);
            case!(task_create_update_delete_preserves_last_write_ties);
            case!(goal_create_update_delete_preserves_last_write_ties);
            case!(agent_create_update_delete_preserves_last_write_order);
            case!(tasks_sort_status_then_newest_and_filters_keep_tie_order);
            case!(goals_sort_status_then_newest_with_missing_time_last);
            case!(team_lifecycle_returns_every_record_without_promising_enumeration_order);
            case!(run_lists_are_complete_newest_first_and_updates_do_not_rotate);
            case!(events_preserve_append_order_duplicates_tail_and_finite_stream);
            case!(message_lifecycle_sorts_time_with_last_write_ties);
            case!(pending_messages_exclude_broadcast_other_recipient_and_expired);
            case!(context_create_update_delete_is_key_ordered_and_ttl_is_read_only);
            case!(config_round_trips_replaces_and_dot_paths_preserve_extensions);
            case!(state_preserves_running_claimed_retry_order_and_all_stats_on_replace);
            case!(equal_score_selections_rotate_after_run_lifecycle_saves);
            case!(lists_return_all_records_beyond_a_future_default_page);
            case!(global_config_replaces_preferences_without_touching_project_config);
        }
    };
}
pub(crate) use storage_suite;
