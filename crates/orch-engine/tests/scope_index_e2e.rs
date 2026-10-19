//! Scope scheduling through the **whole real stack**: file stores, the real
//! `AdapterRegistry` and `ShellAdapter`, the real `SystemProcessManager`, the
//! PID lock, and real child processes. The only external dependency is
//! `bash`, used as a deterministic short-lived workload.
//!
//! What it proves that a unit test cannot: the tick-scoped `ScopeIndex` is
//! rebuilt from what is genuinely `in_progress` on disk each tick, so an
//! overlapping task waits for the run that blocked it and a disjoint one fills
//! the free slot in the same tick.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orch_core::storage::ConfigStore;
use orch_domain::{
    CreateAgentInput, CreateTaskInput, OrchestratorEvent, OrchestratorEventType, RunStatus,
    TaskStatus, WorkspaceMode, default_config,
};
use orch_engine::orchestrator::WatchOptions;
use orch_engine::{FullEngine, build_full};
use orch_infra::storage::{FileConfigStore, Paths};

/// Long enough that two runs dispatched in the same tick are both still alive
/// when the next tick looks, short enough to keep the test quick.
const WORKLOAD_MS: u64 = 350;

/// A project with a written `config.yml` and nothing else.
async fn project() -> (tempfile::TempDir, FullEngine) {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();

    let paths = Paths::new(temp.path());
    tokio::fs::create_dir_all(paths.root())
        .await
        .expect("create .orchestry");

    let mut config = default_config();
    config.project.name = "scope-e2e".into();
    // `shared` keeps every run in the project root: no git repository, no
    // worktrees, no merge-back — the scheduling is what is under test.
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    config.scheduling.max_concurrent_agents = 4;
    config.scheduling.poll_interval_ms = 25;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");

    let engine = build_full(temp.path()).await.expect("build_full");
    (temp, engine)
}

#[tokio::test(flavor = "multi_thread")]
async fn overlapping_scopes_serialize_while_disjoint_work_fills_the_free_slots() {
    let (temp, runtime) = project().await;
    let command = format!("sleep {}; echo scope-e2e-done", WORKLOAD_MS as f64 / 1000.0);

    for index in 1..=4 {
        runtime
            .agent_service
            .create(CreateAgentInput {
                name: format!("scope-worker-{index}"),
                adapter: "shell".into(),
                command: Some(command.clone()),
                approval_policy: Some(orch_domain::ApprovalPolicy::Auto),
                workspace_mode: Some(WorkspaceMode::Shared),
                ..Default::default()
            })
            .await
            .expect("create agent");
    }

    // Priority fixes candidate order, which exercises both radix directions:
    // a shorter parent queried after a longer base, and a longer child queried
    // after its parent joined the same tick-scoped index.
    let create = |title: &str, priority: i64, scope: Vec<String>| {
        let runtime = &runtime;
        let title = title.to_owned();
        async move {
            runtime
                .task_service
                .create(CreateTaskInput {
                    title,
                    priority: Some(priority),
                    scope: Some(scope),
                    workspace_mode: Some(WorkspaceMode::Shared),
                    max_attempts: Some(1),
                    ..Default::default()
                })
                .await
                .expect("create task")
        }
    };
    let auth_child = create(
        "auth child first",
        1,
        vec!["src/auth/private/credentials/**".into()],
    )
    .await;
    let auth_parent = create("auth parent blocked", 2, vec!["src/auth/**".into()]).await;
    let db_parent = create("db parent first", 3, vec!["src/db/**".into()]).await;
    let db_child = create("db child blocked", 4, vec!["src/db/pool.ts".into()]).await;
    let task_ids = [
        auth_child.id.clone(),
        auth_parent.id.clone(),
        db_parent.id.clone(),
        db_child.id.clone(),
    ];

    // One ordered trace of starts and completions, keyed by task.
    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run_to_task: Arc<Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let overlaps: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let unsubscribe = {
        let trace = Arc::clone(&trace);
        let run_to_task = Arc::clone(&run_to_task);
        let overlaps = Arc::clone(&overlaps);
        runtime.events.on_any(move |event| match event {
            OrchestratorEvent::AgentStarted {
                run_id, task_id, ..
            } => {
                run_to_task
                    .lock()
                    .unwrap()
                    .insert(run_id.clone(), task_id.clone());
                trace.lock().unwrap().push(format!("started:{task_id}"));
            }
            OrchestratorEvent::AgentCompleted { run_id, .. } => {
                if let Some(task_id) = run_to_task.lock().unwrap().get(run_id) {
                    trace.lock().unwrap().push(format!("completed:{task_id}"));
                }
            }
            OrchestratorEvent::TaskScopeOverlap { task_id, .. } => {
                overlaps.lock().unwrap().insert(task_id.clone());
            }
            _ => {}
        })
    };

    runtime
        .orchestrator
        .start_watch(WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        })
        .await
        .expect("start_watch");

    // Every task reaches `done` (the agents are auto-approve).
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let mut all_done = true;
        for id in &task_ids {
            let task = runtime.task_service.get(id).await.expect("read task");
            all_done &= task.status == TaskStatus::Done;
        }
        if all_done {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tasks did not all finish; trace={:?}",
            trace.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    runtime.orchestrator.stop().await.expect("stop");
    unsubscribe.unsubscribe();

    let trace = trace.lock().unwrap().clone();
    let index_of = |entry: &str| {
        trace
            .iter()
            .position(|item| item == entry)
            .unwrap_or_else(|| panic!("{entry} never happened; trace={trace:?}"))
    };
    let first_completion = trace
        .iter()
        .position(|item| item.starts_with("completed:"))
        .expect("a completion");

    // The two heads of the scope families started before anything completed:
    // they ran concurrently, which is the point of the free-slot fill.
    let started_auth_child = index_of(&format!("started:{}", auth_child.id));
    let started_db_parent = index_of(&format!("started:{}", db_parent.id));
    assert!(started_auth_child < first_completion, "trace={trace:?}");
    assert!(started_db_parent < first_completion, "trace={trace:?}");

    // Each conflicting task started only after the run that blocked it ended.
    let completed_auth_child = index_of(&format!("completed:{}", auth_child.id));
    let started_auth_parent = index_of(&format!("started:{}", auth_parent.id));
    assert!(completed_auth_child > started_auth_child);
    assert!(
        started_auth_parent > completed_auth_child,
        "trace={trace:?}"
    );

    let completed_db_parent = index_of(&format!("completed:{}", db_parent.id));
    let started_db_child = index_of(&format!("started:{}", db_child.id));
    assert!(completed_db_parent > started_db_parent);
    assert!(started_db_child > completed_db_parent, "trace={trace:?}");

    // Exactly the two blocked tasks were reported as overlapping.
    assert_eq!(
        *overlaps.lock().unwrap(),
        BTreeSet::from([auth_parent.id.clone(), db_child.id.clone()])
    );

    // Four runs, all succeeded, one per task, each with at least one output
    // event written to its JSONL file.
    let runs = runtime.run_service.list_all().await.expect("list runs");
    assert_eq!(runs.len(), 4);
    assert!(runs.iter().all(|run| run.status == RunStatus::Succeeded));
    assert_eq!(
        runs.iter()
            .map(|run| run.task_id.clone())
            .collect::<BTreeSet<_>>(),
        task_ids.iter().cloned().collect::<BTreeSet<_>>()
    );
    for run in &runs {
        let events = runtime
            .run_service
            .read_events(&run.id)
            .await
            .expect("read run events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == orch_domain::RunEventType::AgentOutput),
            "run {} produced no output",
            run.id
        );
    }

    // The on-disk tree matches: one YAML per task, one JSON + one JSONL per run.
    let count_with_extension = |dir: std::path::PathBuf, extension: &str| {
        let extension = extension.to_owned();
        async move {
            let mut entries = tokio::fs::read_dir(&dir).await.expect("read_dir");
            let mut count = 0;
            while let Some(entry) = entries.next_entry().await.expect("next entry") {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|found| found == extension.as_str())
                {
                    count += 1;
                }
            }
            count
        }
    };
    assert_eq!(
        count_with_extension(runtime.paths.tasks_dir(), "yml").await,
        4
    );
    assert_eq!(
        count_with_extension(runtime.paths.runs_dir(), "json").await,
        4
    );
    assert_eq!(
        count_with_extension(runtime.paths.runs_dir(), "jsonl").await,
        4
    );

    orch_infra::storage::fs_utils::close_all_append_handles().await;
    drop(temp);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_with_no_scope_is_never_blocked_by_one_that_has_scope() {
    let (temp, runtime) = project().await;
    let command = format!("sleep {}; echo done", WORKLOAD_MS as f64 / 1000.0);

    for index in 1..=2 {
        runtime
            .agent_service
            .create(CreateAgentInput {
                name: format!("worker-{index}"),
                adapter: "shell".into(),
                command: Some(command.clone()),
                approval_policy: Some(orch_domain::ApprovalPolicy::Auto),
                workspace_mode: Some(WorkspaceMode::Shared),
                ..Default::default()
            })
            .await
            .expect("create agent");
    }

    let scoped = runtime
        .task_service
        .create(CreateTaskInput {
            title: "scoped".into(),
            priority: Some(1),
            scope: Some(vec!["src/auth/**".into()]),
            max_attempts: Some(1),
            ..Default::default()
        })
        .await
        .expect("create scoped task");
    let unscoped = runtime
        .task_service
        .create(CreateTaskInput {
            title: "unscoped".into(),
            priority: Some(2),
            max_attempts: Some(1),
            ..Default::default()
        })
        .await
        .expect("create unscoped task");

    let overlaps: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let unsubscribe = {
        let overlaps = Arc::clone(&overlaps);
        runtime
            .events
            .on(OrchestratorEventType::TaskScopeOverlap, move |event| {
                if let OrchestratorEvent::TaskScopeOverlap { task_id, .. } = event {
                    overlaps.lock().unwrap().insert(task_id.clone());
                }
            })
    };

    runtime
        .orchestrator
        .start_watch(WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        })
        .await
        .expect("start_watch");

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let both_done = runtime
            .task_service
            .get(&scoped.id)
            .await
            .expect("read scoped")
            .status
            == TaskStatus::Done
            && runtime
                .task_service
                .get(&unscoped.id)
                .await
                .expect("read unscoped")
                .status
                == TaskStatus::Done;
        if both_done {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "tasks did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    runtime.orchestrator.stop().await.expect("stop");
    unsubscribe.unsubscribe();

    // A task with no scope is exempt from the index entirely, so nothing was
    // reported as overlapping and both runs succeeded.
    assert!(overlaps.lock().unwrap().is_empty());
    let runs = runtime.run_service.list_all().await.expect("list runs");
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.status == RunStatus::Succeeded));

    orch_infra::storage::fs_utils::close_all_append_handles().await;
    drop(temp);
}
