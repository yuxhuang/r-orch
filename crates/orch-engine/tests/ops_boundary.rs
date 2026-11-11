use std::sync::{Arc, Mutex};

use orch_domain::{OrchestratorEvent, TaskStatus};
use orch_engine::events::EventBus;

#[tokio::test]
async fn nested_operations_share_sequence_and_publish_after_release() {
    let events = Arc::new(EventBus::new());
    let boundary = events.operation_boundary();
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&delivered);
    let observed = Arc::clone(&boundary);
    let _subscription = events.on_any(move |_| {
        captured.lock().unwrap().push(observed.current_commit_seq());
    });
    let result = boundary
        .run(false, async {
            let nested = boundary
                .run(true, async {
                    events.emit(OrchestratorEvent::TaskStatusChanged {
                        task_id: "tsk_example".into(),
                        from: TaskStatus::Todo,
                        to: TaskStatus::InProgress,
                    });
                    assert!(delivered.lock().unwrap().is_empty());
                    42
                })
                .await;
            nested.value
        })
        .await;
    assert_eq!(result.value, 42);
    assert_eq!(result.commit_seq, 1);
    assert_eq!(*delivered.lock().unwrap(), vec![1]);
    assert_eq!(boundary.run(false, async {}).await.commit_seq, 1);
}

#[tokio::test]
async fn failed_mutation_still_publishes_a_commit() {
    let events = EventBus::new();
    let boundary = events.operation_boundary();
    let mut commits = boundary.subscribe_commits();
    let outcome = boundary
        .run(true, async { Err::<(), _>("partial write") })
        .await;
    assert!(outcome.value.is_err());
    assert_eq!(outcome.commit_seq, 1);
    assert_eq!(commits.try_recv().unwrap(), 1);
}

#[tokio::test]
async fn config_refresh_is_visible_to_the_next_service_operation() {
    use orch_domain::{CreateAgentInput, CreateTaskInput};
    use orch_engine::container::{EngineSeams, build_full_with};

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
    let full = build_full_with(
        temp.path(),
        EngineSeams {
            global_config_path: Some(temp.path().join("global.yml")),
            ..EngineSeams::default()
        },
    )
    .await
    .unwrap();
    let boundary = full.operation_boundary();
    boundary
        .run(true, async {
            let mut config = full.config_store.read().await.unwrap();
            config.defaults.task.priority = 4;
            config.defaults.task.max_attempts = 7;
            config.defaults.agent.adapter = "shell".into();
            full.config_store.write(&config).await.unwrap();
            full.refresh_config().await.unwrap();
        })
        .await;
    boundary
        .run(true, async {
            let task = full
                .task_service
                .create(CreateTaskInput {
                    title: "fresh defaults".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(task.priority, 4);
            assert_eq!(task.max_attempts, 7);
            let agent = full
                .agent_service
                .create(CreateAgentInput {
                    name: "fresh defaults".into(),
                    command: Some("true".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(agent.adapter, "shell");
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_commit_ordinals_and_notifications_remain_in_order() {
    let boundary = Arc::new(orch_engine::container::OperationBoundary::new());
    let mut commits = boundary.subscribe_commits();
    let mut tasks = Vec::new();
    for _ in 0..100 {
        let boundary = Arc::clone(&boundary);
        tasks.push(tokio::spawn(async move {
            boundary
                .track(async {
                    let committed = boundary
                        .run(true, async {
                            tokio::task::yield_now().await;
                        })
                        .await;
                    tokio::task::yield_now().await;
                    committed.commit_seq
                })
                .await
        }));
    }
    let mut assigned = Vec::new();
    for task in tasks {
        let outcome = task.await.unwrap();
        assert_eq!(outcome.value, outcome.commit_seq);
        assigned.push(outcome.commit_seq);
    }
    assigned.sort();
    assert_eq!(assigned, (1..=100).collect::<Vec<_>>());
    for expected in 1..=100 {
        assert_eq!(commits.try_recv().unwrap(), expected);
    }
}

struct GoalReadBarrier {
    inner: orch_core::test_support::FakeGoalStore,
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl orch_core::storage::GoalStore for GoalReadBarrier {
    async fn list(
        &self,
        filter: &orch_core::storage::GoalFilter,
    ) -> orch_core::error::Result<Vec<orch_domain::Goal>> {
        self.inner.list(filter).await
    }
    async fn get(&self, id: &str) -> orch_core::error::Result<Option<orch_domain::Goal>> {
        let captured = self.inner.get(id).await?;
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(captured)
    }
    async fn save(&self, goal: &orch_domain::Goal) -> orch_core::error::Result<()> {
        self.inner.save(goal).await
    }
    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

#[tokio::test]
async fn goal_edit_cannot_resurrect_active() {
    use orch_core::storage::GoalStore;
    use orch_core::test_support::{FixedClock, SeqIdGenerator};
    use orch_domain::{Goal, GoalStatus};
    use orch_engine::goal_service::UpdateGoalFields;
    let store = Arc::new(GoalReadBarrier {
        inner: orch_core::test_support::FakeGoalStore::with_goals([Goal {
            id: "gol_barrier".into(),
            title: "old".into(),
            description: String::new(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: None,
        }]),
        armed: std::sync::atomic::AtomicBool::new(true),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let service = orch_engine::GoalService::new(
        store.clone(),
        Arc::new(EventBus::new()),
        Arc::new(FixedClock::new()),
        Arc::new(SeqIdGenerator::new()),
        None,
        None,
        None,
    );
    let edit = async {
        service
            .update(
                "gol_barrier",
                &UpdateGoalFields {
                    title: Some("new".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    };
    let pause = async {
        store.entered.notified().await;
        let pause = service.update_status("gol_barrier", GoalStatus::Paused, false);
        tokio::pin!(pause);
        let mut completed_early = false;
        for _ in 0..100 {
            tokio::select! { biased; result = &mut pause => { result.unwrap(); completed_early = true; break; }, () = tokio::task::yield_now() => {} }
        }
        store.release.notify_one();
        if !completed_early {
            pause.await.unwrap();
        }
    };
    tokio::join!(edit, pause);
    let goal = store.get("gol_barrier").await.unwrap().unwrap();
    assert_eq!(goal.title, "new");
    assert_eq!(goal.status, GoalStatus::Paused);
}

#[tokio::test]
async fn daemon_initialize_keeps_context_corruption_nonfatal() {
    use orch_engine::container::{EngineSeams, build_full_with};
    let temp = tempfile::tempdir().unwrap();
    let context = temp.path().join(".orchestry/context");
    std::fs::create_dir_all(&context).unwrap();
    let corrupt = context.join("bad.json");
    std::fs::write(&corrupt, "not json").unwrap();
    let full = build_full_with(
        temp.path(),
        EngineSeams {
            global_config_path: Some(temp.path().join("global.yml")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    full.daemon_initialize().await.unwrap();
    assert_eq!(std::fs::read_to_string(corrupt).unwrap(), "not json");
    assert_eq!(
        full.state.read().await.unwrap().pid,
        Some(std::process::id())
    );
    full.orchestrator.daemon_stop().await.unwrap();
}
