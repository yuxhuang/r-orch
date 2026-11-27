//! The only file-specific part: constructing stores and checking import input.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orch_core::storage::*;
use orch_core::test_support::{FixedClock, assert_hermetic};
use orch_domain::*;
use orch_infra::storage::*;

use crate::contract::{self, Stores};

pub fn backend() -> Stores {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    let paths = Paths::new(root.path());
    let clock = Arc::new(FixedClock::at(1_735_689_600_000));
    Stores {
        tasks: Arc::new(FileTaskStore::new(paths.clone())),
        goals: Arc::new(FileGoalStore::new(paths.clone())),
        agents: Arc::new(FileAgentStore::new(paths.clone())),
        teams: Arc::new(FileTeamStore::new(paths.clone())),
        runs: Arc::new(FileRunStore::new(paths.clone())),
        messages: Arc::new(FileMessageStore::new(paths.clone(), clock.clone())),
        context: Arc::new(FileContextStore::new(paths.clone(), clock.clone())),
        config: Arc::new(FileConfigStore::new(paths.clone())),
        global: Arc::new(FileGlobalConfigStore::at(root.path().join("global.yml"))),
        state: Arc::new(FileStateStore::new(paths)),
        clock,
        _guard: Box::new(root),
    }
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/storage-v1")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            assert!(
                entry.file_type().unwrap().is_file(),
                "fixtures cannot contain symlinks"
            );
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn fixture(name: &str) -> (tempfile::TempDir, Paths) {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    // Reads can rebuild indices, so never open the checked-in input in place.
    copy_tree(&corpus().join(name), root.path());
    let paths = Paths::new(root.path());
    (root, paths)
}

#[tokio::test]
async fn empty_and_small_corpus_are_readable_without_a_daemon() {
    let (_root, paths) = fixture("empty");
    assert!(
        FileTaskStore::new(paths.clone())
            .list(&TaskFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        FileStateStore::new(paths).read().await.unwrap(),
        default_state()
    );
    let (_root, paths) = fixture("small");
    let tasks = FileTaskStore::new(paths.clone())
        .list(&TaskFilter::default())
        .await
        .unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["tsk_b", "tsk_a"]
    );
    assert_eq!(FileAgentStore::new(paths).list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn every_kind_corpus_loads_each_store_and_project_prompt_template() {
    let (_root, paths) = fixture("every-kind");
    let clock = Arc::new(FixedClock::at(1_735_689_600_000));
    assert_eq!(
        FileTaskStore::new(paths.clone())
            .get("tsk_b")
            .await
            .unwrap(),
        Some(Task {
            status: TaskStatus::Done,
            created_at: "2025-01-01T00:00:00Z".into(),
            assignee: Some("agt_b".into()),
            goal_id: Some("goal_b".into()),
            ..contract::task("tsk_b")
        })
    );
    assert_eq!(
        FileGoalStore::new(paths.clone())
            .get("goal_b")
            .await
            .unwrap(),
        Some(contract::goal("goal_b"))
    );
    assert_eq!(
        FileAgentStore::new(paths.clone())
            .list()
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        FileTeamStore::new(paths.clone())
            .get("team_b")
            .await
            .unwrap(),
        Some(Team {
            members: vec![TeamMember {
                agent_id: "agt_b".into(),
                role: TeamMemberRole::Lead,
                joined_at: contract::STAMP.into()
            }],
            task_pool: vec!["tsk_b".into()],
            ..contract::team("team_b")
        })
    );
    let runs = FileRunStore::new(paths.clone());
    assert_eq!(
        runs.get("run_b").await.unwrap(),
        Some(Run {
            task_id: "tsk_b".into(),
            agent_id: "agt_b".into(),
            status: RunStatus::Succeeded,
            finished_at: Some(contract::STAMP.into()),
            ..contract::run("run_b", contract::STAMP)
        })
    );
    assert_eq!(runs.read_events("run_b").await.unwrap().len(), 2);
    assert_eq!(
        FileMessageStore::new(paths.clone(), clock.clone())
            .get("msg_b")
            .await
            .unwrap(),
        Some(Message {
            from_agent_id: "cli".into(),
            ..contract::message("msg_b")
        })
    );
    assert_eq!(
        FileContextStore::new(paths.clone(), clock)
            .get_all()
            .await
            .unwrap()
            .get("synthetic"),
        Some(&"fixture value".to_owned())
    );
    assert_eq!(
        FileConfigStore::new(paths.clone())
            .read()
            .await
            .unwrap()
            .project
            .name,
        "synthetic"
    );
    assert_eq!(
        FileStateStore::new(paths.clone()).read().await.unwrap(),
        default_state()
    );
    // Templates are plain bytes in 1.0, with no TemplateStore port. Keep their
    // corpus oracle separate from the backend-neutral trait scenarios.
    assert_eq!(
        tokio::fs::read_to_string(paths.default_template_path())
            .await
            .unwrap(),
        "Synthetic prompt: {{ task.title }}\n"
    );
}

#[tokio::test]
async fn inflight_corpus_retains_running_claimed_retries_and_stats() {
    let (_root, paths) = fixture("in-flight");
    let store = FileStateStore::new(paths);
    let state = store.read().await.unwrap();
    assert_eq!(
        state.running.keys().map(String::as_str).collect::<Vec<_>>(),
        ["tsk_b", "tsk_a"]
    );
    assert_eq!(
        state.claimed.iter().map(String::as_str).collect::<Vec<_>>(),
        ["tsk_b", "tsk_a", "tsk_claimed"]
    );
    assert_eq!(
        state
            .retry_queue
            .iter()
            .map(|r| r.task_id.as_str())
            .collect::<Vec<_>>(),
        ["tsk_retry_b", "tsk_retry_a"]
    );
    assert_eq!(state.stats.total_runs, 9);
    store.write(&state).await.unwrap();
    assert_eq!(store.read().await.unwrap(), state);
}

#[tokio::test]
async fn disagreement_corpus_distinguishes_index_snapshot_from_entity_files() {
    let (_root, paths) = fixture("index-disagrees");
    let tasks = FileTaskStore::new(paths);
    let listed = tasks.list(&TaskFilter::default()).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|t| (t.id.as_str(), t.title.as_str()))
            .collect::<Vec<_>>(),
        [("tsk_b", "Stale index title"), ("tsk_ghost", "Index only")]
    );
    assert_eq!(
        tasks.get("tsk_b").await.unwrap().unwrap().title,
        "Entity file title"
    );
    assert!(tasks.get("tsk_ghost").await.unwrap().is_none());
    assert_eq!(
        tasks.get("tsk_orphan").await.unwrap().unwrap().title,
        "File only"
    );
}

#[tokio::test]
async fn merge_and_unknown_config_corpus_preserves_extensions_after_set() {
    let (_root, paths) = fixture("merge-config");
    let config = FileConfigStore::new(paths);
    assert_eq!(
        config
            .read()
            .await
            .unwrap()
            .scheduling
            .max_concurrent_agents,
        3
    );
    assert_eq!(
        config.get("legacy_extension.keep").await.unwrap(),
        Some(serde_json::json!(["alpha", "beta"]))
    );
    config
        .set("project.name", serde_json::json!("updated"))
        .await
        .unwrap();
    assert_eq!(
        config.get("legacy_extension.keep").await.unwrap(),
        Some(serde_json::json!(["alpha", "beta"]))
    );
}

#[tokio::test]
async fn malformed_state_fields_fall_back_but_syntax_errors_are_reported() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::new(root.path());
    tokio::fs::create_dir_all(paths.root()).await.unwrap();
    tokio::fs::write(
        paths.state_path(),
        r#"{"running":[],"claimed":{},"retry_queue":false,"stats":{"total_runs":7}}"#,
    )
    .await
    .unwrap();
    let store = FileStateStore::new(paths.clone());
    let state = store.read().await.unwrap();
    assert_eq!(
        (
            state.running.len(),
            state.claimed.len(),
            state.retry_queue.len(),
            state.stats.total_runs
        ),
        (0, 0, 0, 7)
    );
    tokio::fs::write(paths.state_path(), "{broken")
        .await
        .unwrap();
    assert!(store.read().await.is_err());
}

#[test]
fn corpus_has_six_synthetic_cases_and_no_symlinks_or_private_locations() {
    fn inspect(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink(), "{}", entry.path().display());
            if kind.is_dir() {
                inspect(&entry.path());
            } else {
                let text = std::fs::read_to_string(entry.path()).unwrap();
                for forbidden in [
                    "/Users/",
                    "/home/",
                    "192.168.",
                    "10.0.",
                    ".local",
                    "BEGIN PRIVATE KEY",
                ] {
                    assert!(
                        !text.contains(forbidden),
                        "{} contains {forbidden}",
                        entry.path().display()
                    );
                }
            }
        }
    }
    let mut cases: Vec<_> = std::fs::read_dir(corpus())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    cases.sort();
    assert_eq!(
        cases,
        [
            "empty",
            "every-kind",
            "in-flight",
            "index-disagrees",
            "merge-config",
            "small"
        ]
    );
    inspect(&corpus());
}
