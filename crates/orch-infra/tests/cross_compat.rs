//! V7 and V8: the cross-compat round trip, executed through the **real
//! TypeScript binary's store classes**.
//!
//! This is the only check that tests what PARITY.md §6.4 calls the
//! cross-compat crux at the entity level. Verifying our own artifacts with our
//! own parser would make every bug of the form "js-yaml reads r-orch's bytes
//! differently" invisible by construction — and that bug class is exactly
//! what R1 is about: an unquoted `created_at` reads back in TypeScript as a
//! `Date`, and `MessageStore.list()` then calls `.localeCompare()` on it and
//! **throws**.
//!
//! Both directions run:
//!
//! - **V8 / rust → ts.** Rust writes one of every entity; the TS stores read
//!   them and report the JS *type* of each timestamp. Every one must be
//!   `string`.
//! - **V7 / ts → rust.** The TS stores write one of every entity — including
//!   the folded `>-` block scalars js-yaml picks at `lineWidth: 120` and
//!   `serde_yaml_ng` never emits — and Rust reads them back.
//!
//! Skipped with a warning when the vendored oracle is absent, so the suite
//! still runs on a machine without `parity/setup.sh`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::storage::{
    AgentStore, ContextStore, GoalFilter, GoalStore, MessageStore, RunStore, StateStore,
    TaskFilter, TaskStore, TeamStore,
};
use orch_core::test_support::{FixedClock, make_agent, make_run, make_task};
use orch_domain::*;
use orch_infra::storage::*;
use serde_json::Value;

const STAMP: &str = "2026-10-13T16:37:58.959Z";

/// A long single-line scalar: js-yaml folds anything past `lineWidth: 120`
/// with `>-`, which is the style our reader has to accept and never produces.
fn long_line() -> String {
    format!("rust-written {}end", "word ".repeat(60))
}

struct Fixture {
    project: tempfile::TempDir,
    ts_root: PathBuf,
}

impl Fixture {
    /// `None` when the vendored oracle has not been built.
    fn new() -> Option<Fixture> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let ts_root = repo_root.join("parity/orch-ts");
        if !ts_root.join("node_modules/.bin/tsx").exists() {
            eprintln!(
                "skipping: {} is missing — run parity/setup.sh to build the oracle",
                ts_root.join("node_modules/.bin/tsx").display()
            );
            return None;
        }
        Some(Fixture {
            project: tempfile::tempdir().expect("tempdir"),
            ts_root,
        })
    }

    fn paths(&self) -> Paths {
        Paths::new(self.project.path())
    }

    /// Run the oracle-side script and return its stdout.
    fn oracle(&self, mode: &str) -> String {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cross_compat.mts");
        let output = Command::new(self.ts_root.join("node_modules/.bin/tsx"))
            .arg(&script)
            .arg(mode)
            .arg(&self.ts_root)
            .arg(self.project.path())
            // tsx resolves its own dependencies from the package directory.
            .current_dir(&self.ts_root)
            .env("CI", "1")
            .output()
            .expect("run the oracle script");

        assert!(
            output.status.success(),
            "oracle `{mode}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 stdout")
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::at(1_760_000_000_000))
}

/// Whether a reported value counts as "absent" rather than a timestamp.
///
/// `null` and the `undefined` tag both mean the field simply was not set —
/// `StateStore.read()` assigns `raw.started_at` unconditionally, so a fresh
/// state reports it as `undefined`.
fn is_absent(value: &Value) -> bool {
    value.is_null() || value.get("__js").and_then(Value::as_str) == Some("undefined")
}

/// Every timestamp-ish value in a parsed tree, with its path, so a failure
/// names the field rather than dumping the document.
fn timestamp_fields(value: &Value, path: &str, out: &mut Vec<(String, Value)>) {
    const SUFFIXES: [&str; 6] = [
        "_at",
        "created_at",
        "updated_at",
        "expires_at",
        "delivered_at",
        "joined_at",
    ];
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if SUFFIXES.iter().any(|suffix| key.ends_with(suffix)) {
                    out.push((child_path.clone(), child.clone()));
                }
                timestamp_fields(child, &child_path, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                timestamp_fields(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

/// V8: Rust writes every entity, the TypeScript stores read them, and every
/// timestamp comes back a **string**.
#[test]
fn rust_written_entities_read_back_in_typescript_with_string_timestamps() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let paths = fixture.paths();

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        write_every_entity(&paths).await;
    });

    let raw = fixture.oracle("read");
    let parsed: Value = serde_json::from_str(&raw).expect("oracle emitted JSON");

    // Everything arrived.
    for (collection, expected) in [
        ("tasks", 1),
        ("agents", 1),
        ("goals", 1),
        ("teams", 1),
        ("messages", 1),
        ("runs", 1),
        ("runEvents", 1),
        ("context", 1),
    ] {
        assert_eq!(
            parsed[collection].as_array().map(Vec::len),
            Some(expected),
            "{collection} did not survive the round trip: {}",
            parsed[collection]
        );
    }

    // V8 proper: not one timestamp came back as a Date.
    let mut fields = Vec::new();
    timestamp_fields(&parsed, "", &mut fields);
    assert!(
        fields.len() >= 10,
        "expected many timestamp fields, found {}",
        fields.len()
    );
    for (path, value) in &fields {
        if is_absent(value) {
            continue;
        }
        assert!(
            value.is_string(),
            "{path} came back as {value} — js-yaml resolved it to a Date, which is the \
             R1 corruption: MessageStore.list() calls localeCompare on it and throws"
        );
    }
    // A set field really is reported as a string, so the loop above is not
    // vacuously skipping everything.
    assert!(
        fields.iter().filter(|(_, value)| value.is_string()).count() >= 10,
        "too few timestamps were actually present to call this a pass"
    );

    // The values themselves survived, not just their types.
    assert_eq!(parsed["tasks"][0]["id"], "tsk_test1");
    assert_eq!(parsed["tasks"][0]["created_at"], STAMP);
    assert_eq!(parsed["tasks"][0]["goalId"], "goal_x1");
    assert_eq!(parsed["agents"][0]["role"], long_line());
    assert_eq!(parsed["messages"][0]["to_agent_id"], "agt_test1");
    assert_eq!(parsed["runEvents"][0]["data"], "rust event");
    assert_eq!(parsed["context"][0]["value"], long_line());
    assert_eq!(
        parsed["state"]["claimed"],
        serde_json::json!({ "__js": "Set", "values": [] })
    );
    assert_eq!(parsed["config"]["project"]["name"], "crosscompat");
}

/// V7, the other direction: the TypeScript stores write every entity and Rust
/// reads them back, folded block scalars and all.
#[test]
fn typescript_written_entities_read_back_in_rust() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let paths = fixture.paths();

    fixture.oracle("write");

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let expected_long = format!("ts-written {}end", "word ".repeat(60));

        let tasks = FileTaskStore::new(paths.clone());
        let task = tasks
            .get("tsk_tsWR1TE")
            .await
            .unwrap()
            .expect("task readable");
        assert_eq!(task.title, "Written by ts");
        assert_eq!(
            task.created_at, STAMP,
            "a Date would have failed to deserialize"
        );
        assert_eq!(task.description, expected_long);
        assert_eq!(task.goal_id.as_deref(), Some("goal_tsWR1TE"));
        assert_eq!(task.priority, 2);
        assert_eq!(task.labels, vec!["from-ts"]);
        // And through the index, which the TS store wrote too.
        assert_eq!(tasks.list(&TaskFilter::default()).await.unwrap().len(), 1);

        let agents = FileAgentStore::new(paths.clone());
        let agent = agents
            .get("agt_tsWR1TE")
            .await
            .unwrap()
            .expect("agent readable");
        // Trailing spaces and a tab force js-yaml into a quoted style rather
        // than a literal block (PARITY.md §6.4.1).
        assert_eq!(
            agent.role.as_deref(),
            Some("Line one\n\nLine two with trailing spaces   \n\tand a tab")
        );
        assert_eq!(agent.config.command.as_deref(), Some("echo ts"));
        assert_eq!(agents.list().await.unwrap().len(), 1);

        let goals = FileGoalStore::new(paths.clone());
        let goal = goals
            .get("goal_tsWR1TE")
            .await
            .unwrap()
            .expect("goal readable");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.description, expected_long);
        assert_eq!(goal.created_at, STAMP);
        assert_eq!(goals.list(&GoalFilter::default()).await.unwrap().len(), 1);

        let teams = FileTeamStore::new(paths.clone());
        let team = teams
            .get("team_tsWR1TE")
            .await
            .unwrap()
            .expect("team readable");
        assert_eq!(team.name, "ts-squad");
        assert_eq!(team.members.len(), 1);
        assert_eq!(team.members[0].role, TeamMemberRole::Lead);
        assert_eq!(team.members[0].joined_at, STAMP);
        assert_eq!(team.config.message_ttl_ms, Some(86_400_000));

        let messages = FileMessageStore::new(paths.clone(), clock());
        let message = messages
            .get("msg_tsWR1TE")
            .await
            .unwrap()
            .expect("message readable");
        assert_eq!(message.body, expected_long);
        assert_eq!(message.created_at, STAMP);
        assert_eq!(message.status, MessageStatus::Pending);
        assert_eq!(messages.list().await.unwrap().len(), 1);
        assert_eq!(messages.list_pending("agt_tsWR1TE").await.unwrap().len(), 1);

        let runs = FileRunStore::new(paths.clone());
        let run = runs
            .get("run_tsWR1TE")
            .await
            .unwrap()
            .expect("run readable");
        assert_eq!(run.status, RunStatus::Succeeded);
        assert_eq!(run.pid, Some(4242));
        assert_eq!(run.prompt, expected_long);
        assert_eq!(runs.list_all().await.unwrap().len(), 1);
        let events = runs.read_events("run_tsWR1TE").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, RunEventType::AgentOutput);
        assert_eq!(events[0].data, serde_json::json!("written by ts"));

        let context = FileContextStore::new(paths.clone(), clock());
        let entry = context
            .get("ts-key")
            .await
            .unwrap()
            .expect("context readable");
        assert_eq!(entry.value, expected_long);
        assert_eq!(context.list().await.unwrap().len(), 1);
    });
}

/// The two directions in sequence over one directory: Rust writes, TypeScript
/// writes alongside it, and both sides still read the whole tree.
#[test]
fn both_binaries_can_extend_the_same_directory() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let paths = fixture.paths();

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        write_every_entity(&paths).await;
    });
    fixture.oracle("write");

    // The oracle sees both sets, and `MessageStore.list()` — the method that
    // throws on a Date — is called over a mixed tree.
    let parsed: Value = serde_json::from_str(&fixture.oracle("read")).unwrap();
    assert_eq!(parsed["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(parsed["messages"].as_array().unwrap().len(), 2);
    assert_eq!(parsed["agents"].as_array().unwrap().len(), 2);

    let mut fields = Vec::new();
    timestamp_fields(&parsed, "", &mut fields);
    for (path, value) in &fields {
        if !is_absent(value) {
            assert!(value.is_string(), "{path} came back as {value}");
        }
    }

    // And so does Rust.
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let tasks = FileTaskStore::new(paths.clone());
        assert_eq!(tasks.list(&TaskFilter::default()).await.unwrap().len(), 2);
        let messages = FileMessageStore::new(paths.clone(), clock());
        assert_eq!(messages.list().await.unwrap().len(), 2);
        let agents = FileAgentStore::new(paths.clone());
        assert_eq!(agents.list().await.unwrap().len(), 2);
    });
}

/// One of every entity type, written through the real Rust stores.
async fn write_every_entity(paths: &Paths) {
    orch_infra::storage::fs_utils::clear_ensured_dirs();

    FileTaskStore::new(paths.clone())
        .save(&Task {
            created_at: STAMP.into(),
            updated_at: STAMP.into(),
            goal_id: Some("goal_x1".into()),
            description: long_line(),
            ..make_task()
        })
        .await
        .unwrap();

    FileAgentStore::new(paths.clone())
        .save(&Agent {
            role: Some(long_line()),
            ..make_agent()
        })
        .await
        .unwrap();

    FileGoalStore::new(paths.clone())
        .save(&Goal {
            id: "goal_x1".into(),
            title: "Ship v2".into(),
            description: long_line(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: STAMP.into(),
            updated_at: Some(STAMP.into()),
        })
        .await
        .unwrap();

    FileTeamStore::new(paths.clone())
        .save(&Team {
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
        })
        .await
        .unwrap();

    FileMessageStore::new(paths.clone(), clock())
        .save(&Message {
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
        })
        .await
        .unwrap();

    let runs = FileRunStore::new(paths.clone());
    runs.save(&Run {
        started_at: STAMP.into(),
        finished_at: Some(STAMP.into()),
        pid: Some(1234),
        prompt: long_line(),
        ..make_run()
    })
    .await
    .unwrap();
    runs.append_event(
        "run_test1",
        &RunEvent {
            timestamp: STAMP.into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::Value::String("rust event".into()),
        },
    )
    .await
    .unwrap();
    orch_infra::storage::fs_utils::close_all_append_handles().await;

    FileContextStore::new(paths.clone(), clock())
        .set("rust-key", &long_line(), None)
        .await
        .unwrap();

    FileStateStore::new(paths.clone())
        .write(&default_state())
        .await
        .unwrap();

    let mut config = default_config();
    config.project.name = "crosscompat".into();
    orch_infra::storage::fs_utils::write_yaml(&paths.config_path(), &config)
        .await
        .unwrap();
}

/// Self-check: the fixture's own data really does contain the hazard, so a
/// green V8 is evidence rather than a vacuous pass.
#[test]
fn the_fixture_actually_contains_timestamp_shaped_scalars() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::new(temp.path());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        write_every_entity(&paths).await;

        let task_yaml = tokio::fs::read_to_string(paths.task_path("tsk_test1").unwrap())
            .await
            .unwrap();
        // Quoted, which is the guard working. Unquoted is the R1 bug.
        assert!(
            task_yaml.contains(&format!("created_at: '{STAMP}'")),
            "{task_yaml}"
        );
        assert!(
            !task_yaml.contains(&format!("created_at: {STAMP}")),
            "an unquoted timestamp reached disk:\n{task_yaml}"
        );
    });
}

/// The timestamp walker finds the fields it claims to, including nested ones.
#[test]
fn the_timestamp_walker_finds_nested_fields() {
    let value = serde_json::json!({
        "created_at": "a",
        "nested": { "updated_at": "b" },
        "list": [{ "joined_at": "c" }],
        "other": 1,
    });
    let mut fields = Vec::new();
    timestamp_fields(&value, "", &mut fields);
    let mut paths: Vec<String> = fields.into_iter().map(|(path, _)| path).collect();
    paths.sort();
    assert_eq!(
        paths,
        vec!["created_at", "list[0].joined_at", "nested.updated_at"]
    );
}
