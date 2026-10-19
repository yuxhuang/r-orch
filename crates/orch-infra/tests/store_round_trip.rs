//! Every entity written through the real stores reads back unchanged, and the
//! bytes on disk keep every timestamp-shaped scalar quoted.
//!
//! The byte-level half guards the YAML 1.1 date hazard: an unquoted
//! `created_at: 2026-10-13T16:37:58.959Z` is a string to `serde_yaml_ng` but a
//! timestamp to any YAML 1.1 reader. The check applies the YAML 1.1 date
//! grammar (`fs_utils::is_yaml11_timestamp`) to the raw lines of every file
//! the stores wrote, so it does not depend on the writer agreeing with itself.
//! No YAML 1.1 parser is involved.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::storage::{
    AgentStore, ContextStore, GoalFilter, GoalStore, MessageStore, RunStore, StateStore,
    TaskFilter, TaskStore, TeamStore,
};
use orch_core::test_support::{FixedClock, make_agent, make_run, make_task};
use orch_domain::*;
use orch_infra::storage::fs_utils::{self, is_yaml11_timestamp};
use orch_infra::storage::*;

const STAMP: &str = "2026-10-13T16:37:58.959Z";

/// Key suffix that marks a timestamp field in the stored entities.
const TIMESTAMP_KEY_SUFFIX: &str = "_at";

/// A ~4000-character single-line scalar, long enough that a YAML emitter is
/// tempted to fold it.
fn long_line() -> String {
    format!("rust-written {}end", "word ".repeat(800))
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::at(1_760_000_000_000))
}

fn project() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("tempdir");
    orch_core::test_support::assert_hermetic(project.path());
    project
}

fn task() -> Task {
    Task {
        created_at: STAMP.into(),
        updated_at: STAMP.into(),
        goal_id: Some("goal_x1".into()),
        description: long_line(),
        ..make_task()
    }
}

fn agent() -> Agent {
    Agent {
        role: Some(long_line()),
        ..make_agent()
    }
}

fn goal() -> Goal {
    Goal {
        id: "goal_x1".into(),
        title: "Ship v2".into(),
        description: long_line(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: STAMP.into(),
        updated_at: Some(STAMP.into()),
    }
}

fn team() -> Team {
    Team {
        id: "team_x1".into(),
        name: "squad".into(),
        description: None,
        status: TeamStatus::Active,
        members: vec![TeamMember {
            agent_id: "agt_test1".into(),
            role: TeamMemberRole::Lead,
            joined_at: STAMP.into(),
        }],
        task_pool: vec![],
        lead_agent_id: "agt_test1".into(),
        created_at: STAMP.into(),
        updated_at: STAMP.into(),
        config: DEFAULT_TEAM_CONFIG,
    }
}

fn message() -> Message {
    Message {
        id: "msg_x1".into(),
        channel: MessageChannel::Direct,
        from_agent_id: "cli".into(),
        to_agent_id: Some("agt_test1".into()),
        subject: "Hi".into(),
        body: long_line(),
        created_at: STAMP.into(),
        expires_at: Some(STAMP.into()),
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
        reply_to: None,
    }
}

fn run() -> Run {
    Run {
        started_at: STAMP.into(),
        finished_at: Some(STAMP.into()),
        pid: Some(1234),
        prompt: long_line(),
        ..make_run()
    }
}

fn run_event() -> RunEvent {
    RunEvent {
        timestamp: STAMP.into(),
        event_type: RunEventType::AgentOutput,
        data: serde_json::Value::String("rust event".into()),
    }
}

fn config() -> OrchestratorConfig {
    let mut config = default_config();
    config.project.name = "round-trip".into();
    config
}

/// One of every entity type, written through the real Rust stores.
async fn write_every_entity(paths: &Paths) {
    fs_utils::clear_ensured_dirs();

    FileTaskStore::new(paths.clone())
        .save(&task())
        .await
        .unwrap();
    FileAgentStore::new(paths.clone())
        .save(&agent())
        .await
        .unwrap();
    FileGoalStore::new(paths.clone())
        .save(&goal())
        .await
        .unwrap();
    FileTeamStore::new(paths.clone())
        .save(&team())
        .await
        .unwrap();
    FileMessageStore::new(paths.clone(), clock())
        .save(&message())
        .await
        .unwrap();

    let runs = FileRunStore::new(paths.clone());
    runs.save(&run()).await.unwrap();
    runs.append_event("run_test1", &run_event()).await.unwrap();
    fs_utils::close_all_append_handles().await;

    FileContextStore::new(paths.clone(), clock())
        .set("rust-key", &long_line(), None)
        .await
        .unwrap();

    FileStateStore::new(paths.clone())
        .write(&default_state())
        .await
        .unwrap();

    fs_utils::write_yaml(&paths.config_path(), &config())
        .await
        .unwrap();
}

/// Every `.yml` file under `dir`, recursively.
fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            yaml_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "yml") {
            out.push(path);
        }
    }
}

/// The scalar part of a block-style YAML line: the text after `key: `, after a
/// `- ` sequence marker, or after both.
fn scalar_of(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(after) = rest.strip_prefix("- ") {
        rest = after.trim_start();
    }
    match rest.split_once(": ") {
        Some((_, value)) => value.trim(),
        None => rest.trim(),
    }
}

/// Every line whose scalar a YAML 1.1 reader would resolve to a timestamp,
/// i.e. a timestamp-shaped scalar that is not quoted.
fn unquoted_timestamps(yaml: &str) -> Vec<&str> {
    yaml.lines()
        .filter(|line| is_yaml11_timestamp(scalar_of(line)))
        .collect()
}

/// Every `*_at` string field in a parsed tree, with its path, so a failure
/// names the field rather than dumping the document.
fn timestamp_fields(value: &serde_yaml_ng::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_yaml_ng::Value::Mapping(map) => {
            for (key, child) in map {
                let Some(key) = key.as_str() else { continue };
                let child_path = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                if key.ends_with(TIMESTAMP_KEY_SUFFIX)
                    && let Some(text) = child.as_str()
                {
                    out.push((child_path.clone(), text.to_string()));
                }
                timestamp_fields(child, &child_path, out);
            }
        }
        serde_yaml_ng::Value::Sequence(items) => {
            for (index, child) in items.iter().enumerate() {
                timestamp_fields(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn every_entity_reads_back_as_written() {
    let project = project();
    let paths = Paths::new(project.path());
    write_every_entity(&paths).await;

    let tasks = FileTaskStore::new(paths.clone());
    assert_eq!(tasks.get("tsk_test1").await.unwrap(), Some(task()));
    assert_eq!(
        tasks.list(&TaskFilter::default()).await.unwrap(),
        vec![task()]
    );

    let agents = FileAgentStore::new(paths.clone());
    // The store fills `capabilities` with the host's detected ones on read.
    let read_agent = |agent: Option<Agent>| {
        agent.map(|agent| Agent {
            capabilities: vec![],
            ..agent
        })
    };
    assert_eq!(
        read_agent(agents.get("agt_test1").await.unwrap()),
        Some(agent())
    );
    assert_eq!(
        agents
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|agent| read_agent(Some(agent)).unwrap())
            .collect::<Vec<_>>(),
        vec![agent()]
    );

    let goals = FileGoalStore::new(paths.clone());
    assert_eq!(goals.get("goal_x1").await.unwrap(), Some(goal()));
    assert_eq!(
        goals.list(&GoalFilter::default()).await.unwrap(),
        vec![goal()]
    );

    let teams = FileTeamStore::new(paths.clone());
    assert_eq!(teams.get("team_x1").await.unwrap(), Some(team()));

    let messages = FileMessageStore::new(paths.clone(), clock());
    assert_eq!(messages.get("msg_x1").await.unwrap(), Some(message()));
    assert_eq!(messages.list().await.unwrap(), vec![message()]);

    let runs = FileRunStore::new(paths.clone());
    assert_eq!(runs.get("run_test1").await.unwrap(), Some(run()));
    assert_eq!(
        runs.read_events("run_test1").await.unwrap(),
        vec![run_event()]
    );

    let context = FileContextStore::new(paths.clone(), clock());
    let entry = context.get("rust-key").await.unwrap().expect("present");
    assert_eq!(entry.value, long_line());

    let state = FileStateStore::new(paths.clone()).read().await.unwrap();
    assert_eq!(state, default_state());

    let stored_config: OrchestratorConfig = fs_utils::read_yaml(&paths.config_path())
        .await
        .unwrap()
        .expect("config present");
    assert_eq!(stored_config, config());
}

/// The byte-level guard: no file the stores wrote carries a plain scalar that
/// the YAML 1.1 date grammar would resolve to a timestamp.
#[tokio::test]
async fn written_yaml_quotes_every_timestamp_shaped_scalar() {
    let project = project();
    let paths = Paths::new(project.path());
    write_every_entity(&paths).await;

    let mut files = Vec::new();
    yaml_files(&paths.root(), &mut files);
    assert!(
        files.len() >= 5,
        "expected one file per YAML store: {files:?}"
    );

    let mut quoted = 0;
    for file in &files {
        let yaml = std::fs::read_to_string(file).unwrap();
        let unquoted = unquoted_timestamps(&yaml);
        assert!(
            unquoted.is_empty(),
            "{} has timestamp-shaped scalars a YAML 1.1 reader resolves to a date: {unquoted:?}",
            file.display()
        );
        quoted += yaml.matches(&format!("'{STAMP}'")).count();
    }
    // Non-vacuous: the fixture really does write the hazard, quoted.
    assert!(quoted >= 7, "only {quoted} quoted timestamps were written");
}

/// Self-check on the detector: it flags the plain forms and passes the quoted
/// ones, so a clean result above means something.
#[test]
fn the_unquoted_timestamp_detector_flags_plain_scalars_only() {
    let yaml = format!(
        "created_at: {STAMP}\n\
         updated_at: '{STAMP}'\n\
         members:\n  - joined_at: {STAMP}\n  - joined_at: \"{STAMP}\"\n\
         when: 2026-10-13\n\
         title: not a date\n"
    );
    assert_eq!(
        unquoted_timestamps(&yaml),
        vec![
            format!("created_at: {STAMP}").as_str(),
            format!("  - joined_at: {STAMP}").as_str(),
            "when: 2026-10-13",
        ]
    );
}

/// The written fixture contains `*_at` timestamps nested in sequences, and the
/// walker finds them, so the checks above cover more than top-level fields.
#[tokio::test]
async fn the_fixture_contains_timestamp_fields_including_nested_ones() {
    let project = project();
    let paths = Paths::new(project.path());
    write_every_entity(&paths).await;

    let team_yaml = std::fs::read_to_string(paths.team_path("team_x1").unwrap()).unwrap();
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&team_yaml).unwrap();
    let mut fields = Vec::new();
    timestamp_fields(&value, "", &mut fields);
    let mut found: Vec<&str> = fields.iter().map(|(path, _)| path.as_str()).collect();
    found.sort();
    assert_eq!(found, ["created_at", "members[0].joined_at", "updated_at"]);
    assert!(fields.iter().all(|(_, text)| text == STAMP));

    let task_yaml = std::fs::read_to_string(paths.task_path("tsk_test1").unwrap()).unwrap();
    assert!(
        task_yaml.contains(&format!("created_at: '{STAMP}'")),
        "{task_yaml}"
    );
}

/// A folded `>-` scalar, the style other YAML emitters pick for long single
/// lines and this crate's writer never does, reads through a real store.
#[tokio::test]
async fn a_folded_block_scalar_reads_through_the_task_store() {
    let project = project();
    let paths = Paths::new(project.path());
    fs_utils::clear_ensured_dirs();
    let tasks = FileTaskStore::new(paths.clone());
    tasks.save(&make_task()).await.unwrap();

    let path = paths.task_path("tsk_test1").unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let words = "word ".repeat(60);
    let folded_lines = words
        .split_whitespace()
        .collect::<Vec<_>>()
        .chunks(20)
        .map(|chunk| format!("  {}\n", chunk.join(" ")))
        .collect::<String>();
    let description_line = written
        .lines()
        .find(|line| line.starts_with("description:"))
        .expect("description line");
    let rewritten = written.replacen(
        &format!("{description_line}\n"),
        &format!("description: >-\n{folded_lines}"),
        1,
    );
    assert_ne!(rewritten, written, "the description line was not replaced");
    std::fs::write(&path, rewritten).unwrap();

    let task = tasks.get("tsk_test1").await.unwrap().expect("readable");
    assert_eq!(task.description, words.trim_end());
}
