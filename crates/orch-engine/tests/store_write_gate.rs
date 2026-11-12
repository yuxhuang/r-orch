//! The daemon's store handles serialize each whole entity-and-index mutation.
//! A separately spawned save and delete of one id must wait for the same gate.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use orch_core::error::Result;
use orch_core::storage::{AgentStore, GoalStore, TaskStore};
use orch_domain::{Agent, CreateAgentInput, CreateGoalInput, CreateTaskInput, Goal, Task};
use orch_engine::container::{EngineSeams, OperationBoundary, build_full_with};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

// A held gate cannot produce an early completion, regardless of host load.
// The only timing risk is missing a bypass on a slow host. The fixed 250 ms
// observation window is checked by removing the gate and each store wrapper.
// Startup/release use signals, never sleeps. Ten seconds bounds positive waits.
const OBSERVE_BLOCKED: Duration = Duration::from_millis(250);
const COMPLETE: Duration = Duration::from_secs(10);

#[derive(Clone)]
enum Record {
    Task(Arc<dyn TaskStore>, Task),
    Agent(Arc<dyn AgentStore>, Agent),
    Goal(Arc<dyn GoalStore>, Goal),
}

impl Record {
    fn id(&self) -> &str {
        match self {
            Self::Task(_, record) => &record.id,
            Self::Agent(_, record) => &record.id,
            Self::Goal(_, record) => &record.id,
        }
    }

    async fn save(&self) -> Result<()> {
        match self {
            Self::Task(store, record) => store.save(record).await,
            Self::Agent(store, record) => store.save(record).await,
            Self::Goal(store, record) => store.save(record).await,
        }
    }

    async fn delete(&self) -> Result<()> {
        match self {
            Self::Task(store, record) => store.delete(&record.id).await,
            Self::Agent(store, record) => store.delete(&record.id).await,
            Self::Goal(store, record) => store.delete(&record.id).await,
        }
    }
}

async fn delete_and_save(record: Record, boundary: Arc<OperationBoundary>, held: bool) {
    let (release, released) = oneshot::channel();
    let holder = if held {
        let (started, entered) = oneshot::channel();
        let holder = tokio::spawn(async move {
            boundary
                .run(false, async {
                    started.send(()).unwrap();
                    released.await.unwrap();
                })
                .await;
        });
        tokio::time::timeout(COMPLETE, entered)
            .await
            .expect("operation must enter the gate")
            .unwrap();
        Some(holder)
    } else {
        None
    };

    let mut writers = JoinSet::new();
    let (delete_started, deleting) = oneshot::channel();
    let deleting_record = record.clone();
    writers.spawn(async move {
        delete_started.send(()).unwrap();
        ("delete", deleting_record.delete().await)
    });
    let (save_started, saving) = oneshot::channel();
    writers.spawn(async move {
        save_started.send(()).unwrap();
        ("save", record.save().await)
    });
    tokio::time::timeout(COMPLETE, async {
        deleting.await.unwrap();
        saving.await.unwrap();
    })
    .await
    .expect("both separately spawned writers must start");

    // current_thread makes each writer advance from its startup signal to its
    // first pending await before this observer resumes. Neither task inherits
    // the holder's operation context. Real filesystem IO uses Tokio's pool.
    let early = if held {
        tokio::time::timeout(OBSERVE_BLOCKED, writers.join_next())
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    if let Some(holder) = holder {
        release.send(()).unwrap();
        tokio::time::timeout(COMPLETE, holder)
            .await
            .expect("holder must release the gate")
            .unwrap();
    }

    // Drain before asserting so a failing negative check still releases the
    // gate and owns both writers through their completion.
    let mut outcomes = Vec::new();
    let completed_while_held = early.is_some();
    if let Some(outcome) = early {
        outcomes.push(outcome.unwrap());
    }
    tokio::time::timeout(COMPLETE, async {
        while let Some(outcome) = writers.join_next().await {
            outcomes.push(outcome.unwrap());
        }
    })
    .await
    .expect("save and delete must finish once the gate is available");
    assert!(
        !completed_while_held,
        "a store write completed while the operation gate was held: {outcomes:?}"
    );
    assert_eq!(outcomes.len(), 2, "both store calls must complete");
    for (operation, result) in outcomes {
        result.unwrap_or_else(|error| panic!("{operation} failed: {error}"));
    }
}

async fn assert_files_equal_index(directory: &Path, id: &str) {
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(directory).await.unwrap();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        if entry.path().extension().is_some_and(|ext| ext == "yml") {
            let content = tokio::fs::read_to_string(entry.path()).await.unwrap();
            // These service-created ids are plain YAML scalars. Read contents,
            // not filenames, and reject an unexpected shape or duplicate id.
            let ids: Vec<_> = content
                .lines()
                .filter_map(|line| line.strip_prefix("id: "))
                .collect();
            assert_eq!(ids.len(), 1, "one top-level id per entity: {content}");
            assert!(
                ids[0]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)),
                "expected a plain generated id: {}",
                ids[0]
            );
            files.push(ids[0].to_owned());
        }
    }
    let bytes = tokio::fs::read(directory.join("_index.json"))
        .await
        .unwrap();
    let index: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    let mut indexed: Vec<_> = index
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect();
    files.sort();
    indexed.sort();
    assert_eq!(files, indexed, "entity contents and index must agree");
    // Save then delete: absent and unlisted. Delete then save: present and
    // listed. Neither a ghost index entry nor an unlisted file is legal.
    assert!(
        files.is_empty() || files == [id],
        "only absent/unlisted or present/listed is legal: {files:?}"
    );
}

async fn exercise(store: &str, held: bool) {
    let temp = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(temp.path().join(".orchestry"))
        .await
        .unwrap();
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
    // This is the switch FullEngine::daemon_initialize enables before serving.
    // No listener, agent process or dispatch loop is needed for store handles.
    boundary.enable_store_operations();
    let record = match store {
        "tasks" => Record::Task(
            full.tasks.clone(),
            full.task_service
                .create(CreateTaskInput {
                    title: "store gate".into(),
                    ..Default::default()
                })
                .await
                .unwrap(),
        ),
        "agents" => Record::Agent(
            full.agents.clone(),
            full.agent_service
                .create(CreateAgentInput {
                    name: "store gate".into(),
                    command: Some("true".into()),
                    ..Default::default()
                })
                .await
                .unwrap(),
        ),
        "goals" => Record::Goal(
            full.goals.clone(),
            full.goal_service
                .create(CreateGoalInput {
                    title: "store gate".into(),
                    ..Default::default()
                })
                .await
                .unwrap(),
        ),
        _ => panic!("unknown test store"),
    };
    let id = record.id().to_owned();
    delete_and_save(record, boundary, held).await;
    if held {
        assert_files_equal_index(&temp.path().join(".orchestry").join(store), &id).await;
    }
}

// Deliberately use one runtime thread: signal-to-first-poll ordering is clear,
// and separately spawned tasks still exercise independent operation contexts.
#[tokio::test(flavor = "current_thread")]
async fn task_delete_and_save_wait_for_held_gate_then_leave_files_equal_to_index() {
    exercise("tasks", true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn agent_delete_and_save_wait_for_held_gate_then_leave_files_equal_to_index() {
    exercise("agents", true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn goal_delete_and_save_wait_for_held_gate_then_leave_files_equal_to_index() {
    exercise("goals", true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn task_delete_and_save_complete_when_gate_is_not_held() {
    exercise("tasks", false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn agent_delete_and_save_complete_when_gate_is_not_held() {
    exercise("agents", false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn goal_delete_and_save_complete_when_gate_is_not_held() {
    exercise("goals", false).await;
}
