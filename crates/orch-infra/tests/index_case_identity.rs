//! The index follows the identity in each file, including when the filesystem
//! resolves another spelling to that file. Each test probes its own volume.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::storage::{
    AgentStore, ContextEntry, ContextStore, GoalFilter, GoalStore, MessageStore, TaskFilter,
    TaskStore,
};
use orch_core::test_support::{FixedClock, make_agent, make_task};
use orch_domain::{Agent, Goal, GoalStatus, Message, MessageChannel, MessageStatus, Task};
use orch_infra::storage::fs_utils::quote_ambiguous_scalars;
use orch_infra::storage::{
    FileAgentStore, FileContextStore, FileGoalStore, FileMessageStore, FileTaskStore, Paths,
};
use serde::Serialize;
use serde_json::Value;

const STAMP: &str = "2025-10-09T08:53:20.000Z";

#[derive(Clone, Copy, Debug)]
enum Kind {
    Agent,
    Context,
    Goal,
    Message,
    Task,
}

enum Store {
    Agent(FileAgentStore),
    Context(FileContextStore),
    Goal(FileGoalStore),
    Message(FileMessageStore),
    Task(FileTaskStore),
}

struct Fixture {
    _temp: tempfile::TempDir,
    kind: Kind,
    store: Store,
    dir: PathBuf,
    insensitive: bool,
    files: BTreeMap<String, Vec<u8>>,
}

fn record(kind: Kind, id: &str, value: &str) -> Value {
    match kind {
        Kind::Agent => serde_json::to_value(Agent {
            id: id.into(),
            ..make_agent()
        }),
        Kind::Context => serde_json::to_value(ContextEntry {
            key: id.into(),
            value: value.into(),
            created_at: STAMP.into(),
            updated_at: STAMP.into(),
            ttl_ms: None,
            expires_at: None,
        }),
        Kind::Goal => serde_json::to_value(Goal {
            id: id.into(),
            title: "goal".into(),
            description: String::new(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: STAMP.into(),
            updated_at: None,
        }),
        Kind::Message => serde_json::to_value(Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: "cli".into(),
            to_agent_id: Some("recipient".into()),
            subject: "subject".into(),
            body: "body".into(),
            created_at: STAMP.into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }),
        Kind::Task => serde_json::to_value(Task {
            id: id.into(),
            ..make_task()
        }),
    }
    .unwrap()
}

fn bytes<T: Serialize>(entity: &T, yaml: bool) -> Vec<u8> {
    if yaml {
        quote_ambiguous_scalars(&serde_yaml_ng::to_string(entity).unwrap()).into_bytes()
    } else {
        format!("{}\n", serde_json::to_string_pretty(entity).unwrap()).into_bytes()
    }
}

impl Fixture {
    fn new(kind: Kind) -> Self {
        let temp = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(temp.path());
        std::fs::write(temp.path().join("a"), b"probe").unwrap();
        let insensitive = temp.path().join("A").try_exists().unwrap();
        println!(
            "ARM: {} store: {kind:?} directory: {}",
            if insensitive { "CI" } else { "CS" },
            temp.path().display()
        );
        let paths = Paths::new(temp.path());
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::at(1_760_000_000_000));
        let (store, dir) = match kind {
            Kind::Agent => (
                Store::Agent(FileAgentStore::new(paths.clone())),
                paths.agents_dir(),
            ),
            Kind::Context => (
                Store::Context(FileContextStore::new(paths.clone(), clock)),
                paths.context_dir(),
            ),
            Kind::Goal => (
                Store::Goal(FileGoalStore::new(paths.clone())),
                paths.goals_dir(),
            ),
            Kind::Message => (
                Store::Message(FileMessageStore::new(paths.clone(), clock)),
                paths.messages_dir(),
            ),
            Kind::Task => (
                Store::Task(FileTaskStore::new(paths.clone())),
                paths.tasks_dir(),
            ),
        };
        Self {
            _temp: temp,
            kind,
            store,
            dir,
            insensitive,
            files: BTreeMap::new(),
        }
    }

    fn identity(&self) -> &'static str {
        if matches!(self.kind, Kind::Context) {
            "key"
        } else {
            "id"
        }
    }

    fn filename(&self, id: &str) -> String {
        format!("{id}.{}", if self.yaml() { "yml" } else { "json" })
    }

    fn yaml(&self) -> bool {
        matches!(self.kind, Kind::Agent | Kind::Goal | Kind::Task)
    }

    fn resolved_name(&self, id: &str) -> String {
        let name = self.filename(id);
        self.files
            .keys()
            .find(|key| *key == &name || (self.insensitive && key.eq_ignore_ascii_case(&name)))
            .cloned()
            .unwrap_or(name)
    }

    async fn save(&mut self, id: &str, value: &str) {
        let entity = record(self.kind, id, value);
        let raw = match &self.store {
            Store::Agent(store) => {
                let entity: Agent = serde_json::from_value(entity).unwrap();
                store.save(&entity).await.unwrap();
                bytes(&entity, true)
            }
            Store::Context(store) => {
                store.set(id, value, None).await.unwrap();
                bytes(
                    &serde_json::from_value::<ContextEntry>(entity).unwrap(),
                    false,
                )
            }
            Store::Goal(store) => {
                let entity: Goal = serde_json::from_value(entity).unwrap();
                store.save(&entity).await.unwrap();
                bytes(&entity, true)
            }
            Store::Message(store) => {
                let entity: Message = serde_json::from_value(entity).unwrap();
                store.save(&entity).await.unwrap();
                bytes(&entity, false)
            }
            Store::Task(store) => {
                let entity: Task = serde_json::from_value(entity).unwrap();
                store.save(&entity).await.unwrap();
                bytes(&entity, true)
            }
        };
        self.files.insert(self.resolved_name(id), raw);
        self.check(&format!("save {id}"));
    }

    async fn delete(&mut self, id: &str) {
        match &self.store {
            Store::Agent(store) => store.delete(id).await,
            Store::Context(store) => store.delete(id).await,
            Store::Goal(store) => store.delete(id).await,
            Store::Message(store) => store.delete(id).await,
            Store::Task(store) => store.delete(id).await,
        }
        .unwrap();
        self.files.remove(&self.resolved_name(id));
        self.check(&format!("delete {id}"));
    }

    async fn get(&self, id: &str, expected: Option<Value>) {
        let actual = match &self.store {
            Store::Agent(store) => serde_json::to_value(store.get(id).await.unwrap()).unwrap(),
            Store::Context(store) => serde_json::to_value(store.get(id).await.unwrap()).unwrap(),
            Store::Goal(store) => serde_json::to_value(store.get(id).await.unwrap()).unwrap(),
            Store::Message(store) => serde_json::to_value(store.get(id).await.unwrap()).unwrap(),
            Store::Task(store) => serde_json::to_value(store.get(id).await.unwrap()).unwrap(),
        };
        assert_eq!(actual, expected.unwrap_or(Value::Null), "get {id}");
        self.check(&format!("get {id}"));
    }

    fn reply(&self, id: &str, value: &str) -> Value {
        let mut expected = record(self.kind, id, value);
        if matches!(self.kind, Kind::Agent) {
            expected["capabilities"] = serde_json::json!([std::env::consts::OS]);
        }
        expected
    }

    async fn list(&self) {
        let actual = match &self.store {
            Store::Agent(store) => serde_json::to_value(store.list().await.unwrap()).unwrap(),
            Store::Context(store) => serde_json::to_value(store.list().await.unwrap()).unwrap(),
            Store::Goal(store) => {
                serde_json::to_value(store.list(&GoalFilter::default()).await.unwrap()).unwrap()
            }
            Store::Message(store) => serde_json::to_value(store.list().await.unwrap()).unwrap(),
            Store::Task(store) => {
                serde_json::to_value(store.list(&TaskFilter::default()).await.unwrap()).unwrap()
            }
        };
        let mut expected: Vec<Value> = self.files.values().map(|raw| self.parse(raw)).collect();
        if matches!(self.kind, Kind::Agent) {
            for entry in &mut expected {
                entry["capabilities"] = serde_json::json!([std::env::consts::OS]);
            }
        }
        let mut actual = actual.as_array().unwrap().clone();
        actual.sort_by_key(|entry| entry[self.identity()].as_str().unwrap().to_owned());
        expected.sort_by_key(|entry| entry[self.identity()].as_str().unwrap().to_owned());
        assert_eq!(actual, expected, "list replies");
        self.check("list");
    }

    fn parse(&self, raw: &[u8]) -> Value {
        if self.yaml() {
            serde_yaml_ng::from_slice(raw).unwrap()
        } else {
            serde_json::from_slice(raw).unwrap()
        }
    }

    fn disk_files(&self) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(&self.dir)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name != "_index.json"
                    && name.ends_with(if self.yaml() { ".yml" } else { ".json" }))
                .then(|| (name, std::fs::read(entry.path()).unwrap()))
            })
            .collect()
    }

    fn index(&self) -> Vec<Value> {
        let raw = std::fs::read(self.dir.join("_index.json")).unwrap();
        assert!(raw.ends_with(b"\n"), "index retains its trailing newline");
        serde_json::from_slice(&raw).unwrap()
    }

    fn check(&self, step: &str) {
        let files = self.disk_files();
        assert_eq!(files, self.files, "file names and bytes after {step}");
        // The expected identities and values come only from parsing the entity
        // files ourselves, never list(), IndexManager or a store rebuild.
        let mut expected: Vec<Value> = files.values().map(|raw| self.parse(raw)).collect();
        let mut actual = self.index();
        expected.sort_by_key(|entry| entry[self.identity()].as_str().unwrap().to_owned());
        actual.sort_by_key(|entry| entry[self.identity()].as_str().unwrap().to_owned());
        assert_eq!(actual, expected, "I-EQ after {step} ({:?})", self.kind);
    }
}

async fn sequence(kind: Kind) {
    let mut f = Fixture::new(kind);
    f.save("foo", "first").await;
    f.get("FOO", f.insensitive.then(|| f.reply("foo", "first")))
        .await;
    if let Store::Agent(store) = &f.store {
        let agent = store.get("FOO").await.unwrap();
        f.check("get the agent by the other spelling before saving");
        if let Some(agent) = agent {
            store.save(&agent).await.unwrap();
            f.check("save the agent returned by the other spelling");
            let agent = store.get("FOO").await.unwrap().unwrap();
            f.check("get the agent by the other spelling again");
            store.save(&agent).await.unwrap();
            f.check("save the returned agent again");
        }
    }
    f.get("foo", Some(f.reply("foo", "first"))).await;
    f.get("FOO", f.insensitive.then(|| f.reply("foo", "first")))
        .await;
    f.list().await;
    f.delete("FOO").await;
    f.get("foo", (!f.insensitive).then(|| f.reply("foo", "first")))
        .await;
    f.get("FOO", None).await;
    f.list().await;
    if !f.insensitive {
        // Case-sensitive names remain distinct records, including when both
        // spellings have actually been written.
        f.save("FOO", "second").await;
        f.delete("foo").await;
        f.get("FOO", Some(f.reply("FOO", "second"))).await;
        f.get("foo", None).await;
        f.list().await;
    }
}

#[tokio::test]
async fn context_forward_sequence() {
    let mut f = Fixture::new(Kind::Context);
    f.save("foo", "first").await;
    f.save("FOO", "second").await;
    f.get(
        "foo",
        Some(f.reply(
            if f.insensitive { "FOO" } else { "foo" },
            if f.insensitive { "second" } else { "first" },
        )),
    )
    .await;
    f.get("FOO", Some(f.reply("FOO", "second"))).await;
    f.list().await;
    f.delete("FOO").await;
    f.get("foo", (!f.insensitive).then(|| f.reply("foo", "first")))
        .await;
    f.get("FOO", None).await;
    f.list().await;
    f.save("FOO", "third").await;
    f.list().await;
    f.get(
        "foo",
        Some(f.reply(
            if f.insensitive { "FOO" } else { "foo" },
            if f.insensitive { "third" } else { "first" },
        )),
    )
    .await;
    f.get("FOO", Some(f.reply("FOO", "third"))).await;
}

#[tokio::test]
async fn context_reverse_sequence() {
    let mut f = Fixture::new(Kind::Context);
    f.save("FOO", "first").await;
    f.get("foo", f.insensitive.then(|| f.reply("FOO", "first")))
        .await;
    f.list().await;
    f.delete("foo").await;
    f.get("FOO", (!f.insensitive).then(|| f.reply("FOO", "first")))
        .await;
    f.get("foo", None).await;
    f.list().await;
}

async fn alias(kind: Kind, set: bool) {
    let mut f = Fixture::new(kind);
    f.save("real", "first").await;
    let raw = f.files[&f.filename("real")].clone();
    std::fs::write(f.dir.join(f.filename("alias")), &raw).unwrap();
    f.files.insert(f.filename("alias"), raw);
    let index_before = std::fs::read(f.dir.join("_index.json")).unwrap();
    if set {
        f.save("alias", "second").await;
    } else {
        f.delete("alias").await;
        assert_eq!(
            std::fs::read(f.dir.join("_index.json")).unwrap(),
            index_before
        );
    }
    f.get("real", Some(f.reply("real", "first"))).await;
    f.list().await;
}

async fn malformed(kind: Kind, invalid_id: bool) {
    let mut f = Fixture::new(kind);
    f.save("foo", "first").await;
    let path = f.dir.join(f.filename("foo"));
    if invalid_id {
        let mut entity = record(kind, "foo", "first");
        entity[f.identity()] = Value::String("../x".into());
        std::fs::write(&path, bytes(&entity, f.yaml())).unwrap();
        // Even a pre-existing index entry with an invalid stored identity is
        // not a reason to construct its path or change the deletion reply.
        let mut index = f.index();
        index.push(entity);
        std::fs::write(f.dir.join("_index.json"), bytes(&index, false)).unwrap();
    } else {
        std::fs::write(&path, b"{broken [").unwrap();
    }
    let before = f.index();
    match &f.store {
        Store::Agent(store) => store.delete("foo").await,
        Store::Context(store) => store.delete("foo").await,
        Store::Goal(store) => store.delete("foo").await,
        Store::Message(store) => store.delete("foo").await,
        Store::Task(store) => store.delete("foo").await,
    }
    .unwrap();
    assert!(!path.exists());
    assert_eq!(
        f.index(),
        before
            .into_iter()
            .filter(|entry| entry[f.identity()] != "foo")
            .collect::<Vec<_>>()
    );
}

async fn missing(kind: Kind) {
    let mut f = Fixture::new(kind);
    f.save("foo", "first").await;
    std::fs::remove_file(f.dir.join(f.filename("foo"))).unwrap();
    f.files.clear();
    f.delete("foo").await;
    f.get("foo", None).await;
    f.list().await;
}

macro_rules! store_tests {
    ($module:ident, $kind:ident) => {
        mod $module {
            use super::*;

            #[tokio::test]
            async fn variant_delete_sequence() {
                sequence(Kind::$kind).await;
            }
            #[tokio::test]
            async fn alias_beside_real_record() {
                alias(Kind::$kind, false).await;
            }
            #[tokio::test]
            async fn malformed_file_delete() {
                malformed(Kind::$kind, false).await;
            }
            #[tokio::test]
            async fn invalid_stored_identity_delete() {
                malformed(Kind::$kind, true).await;
            }
            #[tokio::test]
            async fn missing_file_delete() {
                missing(Kind::$kind).await;
            }
        }
    };
}

store_tests!(agent, Agent);
store_tests!(goal, Goal);
store_tests!(message, Message);
store_tests!(task, Task);
store_tests!(context, Context);

#[tokio::test]
async fn context_set_alias_beside_real_record() {
    alias(Kind::Context, true).await;
}
