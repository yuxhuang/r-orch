//! In-memory store fakes for all ten storage traits.
//!
//! Each fake:
//! - **clones on read and on write**, so a caller cannot mutate stored state
//!   by holding on to a returned value (the TS fakes `structuredClone` for
//!   exactly this reason);
//! - records every trait call in a public [`Recorder`];
//! - runs every trait call through a public [`Faults`] first;
//! - offers non-recording `seed_*` / `peek_*` helpers for arranging and
//!   inspecting state without polluting the call log.
//!
//! Where the TS mock was a bare stub returning `[]` (`listForTask`,
//! `listForAgent`, `readEventsTail`, `streamEvents`), these fakes implement
//! the real filtering instead — a fake that lies is worse than one that works,
//! and nothing in the TS suite depended on the stub.

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use indexmap::IndexMap;
use orch_domain::{
    Agent, GlobalConfig, Goal, Message, MessageStatus, OrchestratorConfig, OrchestratorState, Run,
    RunEvent, Task, Team, default_config, default_global_config, default_state,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::storage::{
    AgentStore, ConfigStore, ContextEntry, ContextStore, GlobalConfigStore, GoalFilter, GoalStore,
    MessageStore, RunStore, StateStore, TaskFilter, TaskStore, TeamStore,
};
use crate::test_support::clock::FIXED_NOW_ISO;
use crate::test_support::faults::Faults;
use crate::test_support::recorder::Recorder;

fn lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `IndexManager::save` semantics: **remove the entity by id, then push it to
/// the end** (PARITY.md §6.10).
///
/// Index order is therefore *last-write* order, not creation order, and that
/// is observable — `findBestAgent` resolves equal-scoring agents by position
/// in `AgentStore::list()` and r-orch replicates it (PARITY.md §14.4). A plain
/// `IndexMap::insert` keeps the existing position and would silently change
/// which agent gets picked.
fn save_at_end<V>(map: &mut IndexMap<String, V>, id: String, value: V) {
    map.shift_remove(&id);
    map.insert(id, value);
}

// ── TaskStore ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStoreCall {
    List(TaskFilter),
    Get(String),
    Save(Box<Task>),
    Delete(String),
}

#[derive(Debug, Clone, Default)]
pub struct FakeTaskStore {
    tasks: Arc<Mutex<IndexMap<String, Task>>>,
    pub calls: Recorder<TaskStoreCall>,
    pub faults: Faults,
}

impl FakeTaskStore {
    pub fn new() -> Self {
        FakeTaskStore::default()
    }

    pub fn with_tasks(tasks: impl IntoIterator<Item = Task>) -> Self {
        let store = FakeTaskStore::new();
        for task in tasks {
            store.seed(task);
        }
        store
    }

    /// Insert without recording a call and without the push-to-end rule:
    /// `seed` arranges state, it does not model a write.
    pub fn seed(&self, task: Task) {
        lock(&self.tasks).insert(task.id.clone(), task);
    }

    /// Read without recording a call.
    pub fn peek(&self, id: &str) -> Option<Task> {
        lock(&self.tasks).get(id).cloned()
    }

    /// Every stored task, in insertion order, without recording a call.
    pub fn snapshot(&self) -> Vec<Task> {
        lock(&self.tasks).values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        lock(&self.tasks).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The tasks passed to `save`, in call order — the write history, which is
    /// what the orchestrator tests assert on.
    pub fn saved(&self) -> Vec<Task> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                TaskStoreCall::Save(task) => Some(*task),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl TaskStore for FakeTaskStore {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.calls.record(TaskStoreCall::List(filter.clone()));
        self.faults.check()?;
        Ok(lock(&self.tasks)
            .values()
            .filter(|t| filter.matches(t))
            .cloned()
            .collect())
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.calls.record(TaskStoreCall::Get(id.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.tasks).get(id).cloned())
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.calls
            .record(TaskStoreCall::Save(Box::new(task.clone())));
        self.faults.check()?;
        save_at_end(&mut lock(&self.tasks), task.id.clone(), task.clone());
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.calls.record(TaskStoreCall::Delete(id.to_owned()));
        self.faults.check()?;
        lock(&self.tasks).shift_remove(id);
        Ok(())
    }
}

// ── AgentStore ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AgentStoreCall {
    List,
    Get(String),
    GetByName(String),
    Save(Box<Agent>),
    Delete(String),
}

#[derive(Debug, Clone, Default)]
pub struct FakeAgentStore {
    agents: Arc<Mutex<IndexMap<String, Agent>>>,
    pub calls: Recorder<AgentStoreCall>,
    pub faults: Faults,
}

impl FakeAgentStore {
    pub fn new() -> Self {
        FakeAgentStore::default()
    }

    pub fn with_agents(agents: impl IntoIterator<Item = Agent>) -> Self {
        let store = FakeAgentStore::new();
        for agent in agents {
            store.seed(agent);
        }
        store
    }

    pub fn seed(&self, agent: Agent) {
        lock(&self.agents).insert(agent.id.clone(), agent);
    }

    pub fn peek(&self, id: &str) -> Option<Agent> {
        lock(&self.agents).get(id).cloned()
    }

    pub fn snapshot(&self) -> Vec<Agent> {
        lock(&self.agents).values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        lock(&self.agents).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn saved(&self) -> Vec<Agent> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                AgentStoreCall::Save(agent) => Some(*agent),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl AgentStore for FakeAgentStore {
    async fn list(&self) -> Result<Vec<Agent>> {
        self.calls.record(AgentStoreCall::List);
        self.faults.check()?;
        Ok(self.snapshot())
    }

    async fn get(&self, id: &str) -> Result<Option<Agent>> {
        self.calls.record(AgentStoreCall::Get(id.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.agents).get(id).cloned())
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>> {
        self.calls
            .record(AgentStoreCall::GetByName(name.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.agents)
            .values()
            .find(|a| a.name == name)
            .cloned())
    }

    async fn save(&self, agent: &Agent) -> Result<()> {
        self.calls
            .record(AgentStoreCall::Save(Box::new(agent.clone())));
        self.faults.check()?;
        save_at_end(&mut lock(&self.agents), agent.id.clone(), agent.clone());
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.calls.record(AgentStoreCall::Delete(id.to_owned()));
        self.faults.check()?;
        lock(&self.agents).shift_remove(id);
        Ok(())
    }
}

// ── RunStore ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum RunStoreCall {
    Save(Box<Run>),
    Get(String),
    ListAll,
    ListForTask(String),
    ListForAgent(String),
    AppendEvent(String, RunEvent),
    ReadEvents(String),
    ReadEventsTail(String, usize),
    StreamEvents(String),
    CloseRunEvents(String),
}

/// `RunStore` has **no `_index.json`** — same caveat as [`FakeTeamStore`].
#[derive(Debug, Clone, Default)]
pub struct FakeRunStore {
    runs: Arc<Mutex<IndexMap<String, Run>>>,
    events: Arc<Mutex<IndexMap<String, Vec<RunEvent>>>>,
    pub calls: Recorder<RunStoreCall>,
    pub faults: Faults,
}

impl FakeRunStore {
    pub fn new() -> Self {
        FakeRunStore::default()
    }

    pub fn with_runs(runs: impl IntoIterator<Item = Run>) -> Self {
        let store = FakeRunStore::new();
        for run in runs {
            store.seed(run);
        }
        store
    }

    pub fn seed(&self, run: Run) {
        lock(&self.runs).insert(run.id.clone(), run);
    }

    pub fn seed_events(&self, run_id: &str, events: impl IntoIterator<Item = RunEvent>) {
        lock(&self.events)
            .entry(run_id.to_owned())
            .or_default()
            .extend(events);
    }

    pub fn peek(&self, id: &str) -> Option<Run> {
        lock(&self.runs).get(id).cloned()
    }

    pub fn snapshot(&self) -> Vec<Run> {
        lock(&self.runs).values().cloned().collect()
    }

    /// Stored events for a run, without recording a call.
    pub fn peek_events(&self, run_id: &str) -> Vec<RunEvent> {
        lock(&self.events).get(run_id).cloned().unwrap_or_default()
    }

    pub fn saved(&self) -> Vec<Run> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                RunStoreCall::Save(run) => Some(*run),
                _ => None,
            })
            .collect()
    }

    /// Run ids whose append handle was closed, in order.
    pub fn closed(&self) -> Vec<String> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                RunStoreCall::CloseRunEvents(id) => Some(id),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl RunStore for FakeRunStore {
    async fn save(&self, run: &Run) -> Result<()> {
        self.calls.record(RunStoreCall::Save(Box::new(run.clone())));
        self.faults.check()?;
        lock(&self.runs).insert(run.id.clone(), run.clone());
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.calls.record(RunStoreCall::Get(id.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.runs).get(id).cloned())
    }

    async fn list_all(&self) -> Result<Vec<Run>> {
        self.calls.record(RunStoreCall::ListAll);
        self.faults.check()?;
        Ok(self.snapshot())
    }

    async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
        self.calls
            .record(RunStoreCall::ListForTask(task_id.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.runs)
            .values()
            .filter(|r| r.task_id == task_id)
            .cloned()
            .collect())
    }

    async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>> {
        self.calls
            .record(RunStoreCall::ListForAgent(agent_id.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.runs)
            .values()
            .filter(|r| r.agent_id == agent_id)
            .cloned()
            .collect())
    }

    async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()> {
        self.calls
            .record(RunStoreCall::AppendEvent(run_id.to_owned(), event.clone()));
        self.faults.check()?;
        lock(&self.events)
            .entry(run_id.to_owned())
            .or_default()
            .push(event.clone());
        Ok(())
    }

    async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
        self.calls
            .record(RunStoreCall::ReadEvents(run_id.to_owned()));
        self.faults.check()?;
        Ok(self.peek_events(run_id))
    }

    async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>> {
        self.calls
            .record(RunStoreCall::ReadEventsTail(run_id.to_owned(), count));
        self.faults.check()?;
        let all = self.peek_events(run_id);
        let start = all.len().saturating_sub(count);
        Ok(all[start..].to_vec())
    }

    fn stream_events(
        &self,
        run_id: &str,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<RunEvent>> {
        self.calls
            .record(RunStoreCall::StreamEvents(run_id.to_owned()));
        if token.is_cancelled() {
            return Box::pin(stream::empty());
        }
        let events = self.peek_events(run_id);
        Box::pin(stream::iter(events.into_iter().map(Ok)))
    }

    fn close_run_events(&self, run_id: &str) {
        self.calls
            .record(RunStoreCall::CloseRunEvents(run_id.to_owned()));
    }
}

// ── StateStore ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum StateStoreCall {
    Read,
    Write(Box<OrchestratorState>),
}

#[derive(Debug, Clone)]
pub struct FakeStateStore {
    state: Arc<Mutex<OrchestratorState>>,
    pub calls: Recorder<StateStoreCall>,
    pub faults: Faults,
}

impl Default for FakeStateStore {
    fn default() -> Self {
        FakeStateStore {
            state: Arc::new(Mutex::new(default_state())),
            calls: Recorder::new(),
            faults: Faults::new(),
        }
    }
}

impl FakeStateStore {
    pub fn new() -> Self {
        FakeStateStore::default()
    }

    pub fn with_state(state: OrchestratorState) -> Self {
        let store = FakeStateStore::new();
        store.seed(state);
        store
    }

    pub fn seed(&self, state: OrchestratorState) {
        *lock(&self.state) = state;
    }

    pub fn peek(&self) -> OrchestratorState {
        lock(&self.state).clone()
    }

    /// Every state written, in order — the mutation history.
    pub fn written(&self) -> Vec<OrchestratorState> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                StateStoreCall::Write(state) => Some(*state),
                StateStoreCall::Read => None,
            })
            .collect()
    }
}

#[async_trait]
impl StateStore for FakeStateStore {
    async fn read(&self) -> Result<OrchestratorState> {
        self.calls.record(StateStoreCall::Read);
        self.faults.check()?;
        Ok(self.peek())
    }

    async fn write(&self, state: &OrchestratorState) -> Result<()> {
        self.calls
            .record(StateStoreCall::Write(Box::new(state.clone())));
        self.faults.check()?;
        *lock(&self.state) = state.clone();
        Ok(())
    }
}

// ── ConfigStore ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigStoreCall {
    Read,
    Write(Box<OrchestratorConfig>),
    Get(String),
    Set(String, Value),
}

/// Backed by a raw [`Value`] rather than a typed config, so `get`/`set`
/// exercise the real dot-path semantics — **including** the contract that
/// unknown keys survive a `set` round trip.
#[derive(Debug, Clone)]
pub struct FakeConfigStore {
    raw: Arc<Mutex<Value>>,
    pub calls: Recorder<ConfigStoreCall>,
    pub faults: Faults,
}

impl Default for FakeConfigStore {
    fn default() -> Self {
        FakeConfigStore::with_config(default_config())
    }
}

impl FakeConfigStore {
    pub fn new() -> Self {
        FakeConfigStore::default()
    }

    pub fn with_config(config: OrchestratorConfig) -> Self {
        FakeConfigStore {
            raw: Arc::new(Mutex::new(
                serde_json::to_value(config).expect("config serializes"),
            )),
            calls: Recorder::new(),
            faults: Faults::new(),
        }
    }

    /// Seed from a raw value, e.g. one carrying unknown keys.
    pub fn with_raw(raw: Value) -> Self {
        FakeConfigStore {
            raw: Arc::new(Mutex::new(raw)),
            calls: Recorder::new(),
            faults: Faults::new(),
        }
    }

    pub fn peek_raw(&self) -> Value {
        lock(&self.raw).clone()
    }

    pub fn peek(&self) -> OrchestratorConfig {
        serde_json::from_value(self.peek_raw()).expect("stored config is valid")
    }
}

#[async_trait]
impl ConfigStore for FakeConfigStore {
    async fn read(&self) -> Result<OrchestratorConfig> {
        self.calls.record(ConfigStoreCall::Read);
        self.faults.check()?;
        Ok(self.peek())
    }

    async fn write(&self, config: &OrchestratorConfig) -> Result<()> {
        self.calls
            .record(ConfigStoreCall::Write(Box::new(config.clone())));
        self.faults.check()?;
        *lock(&self.raw) = serde_json::to_value(config).expect("config serializes");
        Ok(())
    }

    async fn get(&self, key_path: &str) -> Result<Option<Value>> {
        self.calls.record(ConfigStoreCall::Get(key_path.to_owned()));
        self.faults.check()?;
        Ok(orch_domain::get_by_path(&lock(&self.raw), key_path).cloned())
    }

    async fn set(&self, key_path: &str, value: Value) -> Result<()> {
        self.calls
            .record(ConfigStoreCall::Set(key_path.to_owned(), value.clone()));
        self.faults.check()?;
        orch_domain::set_by_path(&mut lock(&self.raw), key_path, value);
        Ok(())
    }
}

// ── ContextStore ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ContextStoreCall {
    Get(String),
    Set(String, String, Option<u64>),
    Delete(String),
    List,
    GetAll,
}

/// Entries never expire here — TTL expiry is `orch-infra`'s behaviour and
/// needs a clock. `ttl_ms` is stored and returned so callers can still assert
/// on it.
#[derive(Debug, Clone, Default)]
pub struct FakeContextStore {
    entries: Arc<Mutex<IndexMap<String, ContextEntry>>>,
    pub calls: Recorder<ContextStoreCall>,
    pub faults: Faults,
}

impl FakeContextStore {
    pub fn new() -> Self {
        FakeContextStore::default()
    }

    pub fn with_entries<K, V>(entries: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let store = FakeContextStore::new();
        for (key, value) in entries {
            store.seed(&key.into(), &value.into());
        }
        store
    }

    pub fn seed(&self, key: &str, value: &str) {
        lock(&self.entries).insert(
            key.to_owned(),
            ContextEntry {
                key: key.to_owned(),
                value: value.to_owned(),
                created_at: FIXED_NOW_ISO.to_owned(),
                updated_at: FIXED_NOW_ISO.to_owned(),
                ttl_ms: None,
                expires_at: None,
            },
        );
    }

    pub fn peek(&self, key: &str) -> Option<ContextEntry> {
        lock(&self.entries).get(key).cloned()
    }

    /// Sorted by key ascending, like [`ContextStore::list`].
    fn sorted(&self) -> Vec<ContextEntry> {
        let mut entries: Vec<ContextEntry> = lock(&self.entries).values().cloned().collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        entries
    }
}

#[async_trait]
impl ContextStore for FakeContextStore {
    async fn get(&self, key: &str) -> Result<Option<ContextEntry>> {
        self.calls.record(ContextStoreCall::Get(key.to_owned()));
        self.faults.check()?;
        Ok(self.peek(key))
    }

    async fn set(&self, key: &str, value: &str, ttl_ms: Option<u64>) -> Result<()> {
        self.calls.record(ContextStoreCall::Set(
            key.to_owned(),
            value.to_owned(),
            ttl_ms,
        ));
        self.faults.check()?;
        let mut entries = lock(&self.entries);
        // created_at is preserved across overwrites; updated_at is refreshed.
        let created_at = entries
            .get(key)
            .map(|e| e.created_at.clone())
            .unwrap_or_else(|| FIXED_NOW_ISO.to_owned());
        save_at_end(
            &mut entries,
            key.to_owned(),
            ContextEntry {
                key: key.to_owned(),
                value: value.to_owned(),
                created_at,
                updated_at: FIXED_NOW_ISO.to_owned(),
                ttl_ms,
                expires_at: None,
            },
        );
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.calls.record(ContextStoreCall::Delete(key.to_owned()));
        self.faults.check()?;
        lock(&self.entries).shift_remove(key);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ContextEntry>> {
        self.calls.record(ContextStoreCall::List);
        self.faults.check()?;
        Ok(self.sorted())
    }

    async fn get_all(&self) -> Result<IndexMap<String, String>> {
        self.calls.record(ContextStoreCall::GetAll);
        self.faults.check()?;
        Ok(self
            .sorted()
            .into_iter()
            .map(|e| (e.key, e.value))
            .collect())
    }
}

// ── MessageStore ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum MessageStoreCall {
    Save(Box<Message>),
    Get(String),
    List,
    ListPending(String),
    MarkDelivered(String),
    Delete(String),
    PurgeExpired,
}

/// `list_pending` filters on `status == pending` and `to_agent_id == agentId`,
/// like the real store, but **skips the expiry check** — that needs a clock.
/// `purge_expired` is a no-op returning 0 for the same reason.
#[derive(Debug, Clone, Default)]
pub struct FakeMessageStore {
    messages: Arc<Mutex<IndexMap<String, Message>>>,
    pub calls: Recorder<MessageStoreCall>,
    pub faults: Faults,
}

impl FakeMessageStore {
    pub fn new() -> Self {
        FakeMessageStore::default()
    }

    pub fn with_messages(messages: impl IntoIterator<Item = Message>) -> Self {
        let store = FakeMessageStore::new();
        for message in messages {
            store.seed(message);
        }
        store
    }

    pub fn seed(&self, message: Message) {
        lock(&self.messages).insert(message.id.clone(), message);
    }

    pub fn peek(&self, id: &str) -> Option<Message> {
        lock(&self.messages).get(id).cloned()
    }

    /// Sorted by `created_at` ascending, like the real store.
    fn sorted(&self) -> Vec<Message> {
        let mut messages: Vec<Message> = lock(&self.messages).values().cloned().collect();
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        messages
    }
}

#[async_trait]
impl MessageStore for FakeMessageStore {
    async fn save(&self, message: &Message) -> Result<()> {
        self.calls
            .record(MessageStoreCall::Save(Box::new(message.clone())));
        self.faults.check()?;
        save_at_end(
            &mut lock(&self.messages),
            message.id.clone(),
            message.clone(),
        );
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Message>> {
        self.calls.record(MessageStoreCall::Get(id.to_owned()));
        self.faults.check()?;
        Ok(self.peek(id))
    }

    async fn list(&self) -> Result<Vec<Message>> {
        self.calls.record(MessageStoreCall::List);
        self.faults.check()?;
        Ok(self.sorted())
    }

    async fn list_pending(&self, agent_id: &str) -> Result<Vec<Message>> {
        self.calls
            .record(MessageStoreCall::ListPending(agent_id.to_owned()));
        self.faults.check()?;
        Ok(self
            .sorted()
            .into_iter()
            .filter(|m| {
                m.status == MessageStatus::Pending && m.to_agent_id.as_deref() == Some(agent_id)
            })
            .collect())
    }

    async fn mark_delivered(&self, id: &str) -> Result<()> {
        self.calls
            .record(MessageStoreCall::MarkDelivered(id.to_owned()));
        self.faults.check()?;
        if let Some(message) = lock(&self.messages).get_mut(id) {
            message.status = MessageStatus::Delivered;
            message.delivered_at = Some(FIXED_NOW_ISO.to_owned());
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.calls.record(MessageStoreCall::Delete(id.to_owned()));
        self.faults.check()?;
        lock(&self.messages).shift_remove(id);
        Ok(())
    }

    async fn purge_expired(&self) -> Result<usize> {
        self.calls.record(MessageStoreCall::PurgeExpired);
        self.faults.check()?;
        Ok(0)
    }
}

// ── GoalStore ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum GoalStoreCall {
    List(GoalFilter),
    Get(String),
    Save(Box<Goal>),
    Delete(String),
}

#[derive(Debug, Clone, Default)]
pub struct FakeGoalStore {
    goals: Arc<Mutex<IndexMap<String, Goal>>>,
    pub calls: Recorder<GoalStoreCall>,
    pub faults: Faults,
}

impl FakeGoalStore {
    pub fn new() -> Self {
        FakeGoalStore::default()
    }

    pub fn with_goals(goals: impl IntoIterator<Item = Goal>) -> Self {
        let store = FakeGoalStore::new();
        for goal in goals {
            store.seed(goal);
        }
        store
    }

    pub fn seed(&self, goal: Goal) {
        lock(&self.goals).insert(goal.id.clone(), goal);
    }

    pub fn peek(&self, id: &str) -> Option<Goal> {
        lock(&self.goals).get(id).cloned()
    }

    pub fn snapshot(&self) -> Vec<Goal> {
        lock(&self.goals).values().cloned().collect()
    }

    pub fn saved(&self) -> Vec<Goal> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                GoalStoreCall::Save(goal) => Some(*goal),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl GoalStore for FakeGoalStore {
    async fn list(&self, filter: &GoalFilter) -> Result<Vec<Goal>> {
        self.calls.record(GoalStoreCall::List(filter.clone()));
        self.faults.check()?;
        Ok(lock(&self.goals)
            .values()
            .filter(|g| filter.matches(g))
            .cloned()
            .collect())
    }

    async fn get(&self, id: &str) -> Result<Option<Goal>> {
        self.calls.record(GoalStoreCall::Get(id.to_owned()));
        self.faults.check()?;
        Ok(self.peek(id))
    }

    async fn save(&self, goal: &Goal) -> Result<()> {
        self.calls
            .record(GoalStoreCall::Save(Box::new(goal.clone())));
        self.faults.check()?;
        save_at_end(&mut lock(&self.goals), goal.id.clone(), goal.clone());
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.calls.record(GoalStoreCall::Delete(id.to_owned()));
        self.faults.check()?;
        lock(&self.goals).shift_remove(id);
        Ok(())
    }
}

// ── TeamStore ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TeamStoreCall {
    Save(Box<Team>),
    Get(String),
    GetByName(String),
    List,
    Delete(String),
}

/// `TeamStore` has **no `_index.json`** — the real store scans its directory,
/// so list order is filesystem enumeration order and is not contract
/// (PARITY.md §6.10). This fake keeps insertion order for determinism; do not
/// write a test that depends on it.
#[derive(Debug, Clone, Default)]
pub struct FakeTeamStore {
    teams: Arc<Mutex<IndexMap<String, Team>>>,
    pub calls: Recorder<TeamStoreCall>,
    pub faults: Faults,
}

impl FakeTeamStore {
    pub fn new() -> Self {
        FakeTeamStore::default()
    }

    pub fn with_teams(teams: impl IntoIterator<Item = Team>) -> Self {
        let store = FakeTeamStore::new();
        for team in teams {
            store.seed(team);
        }
        store
    }

    pub fn seed(&self, team: Team) {
        lock(&self.teams).insert(team.id.clone(), team);
    }

    pub fn peek(&self, id: &str) -> Option<Team> {
        lock(&self.teams).get(id).cloned()
    }

    pub fn snapshot(&self) -> Vec<Team> {
        lock(&self.teams).values().cloned().collect()
    }

    pub fn saved(&self) -> Vec<Team> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                TeamStoreCall::Save(team) => Some(*team),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl TeamStore for FakeTeamStore {
    async fn save(&self, team: &Team) -> Result<()> {
        self.calls
            .record(TeamStoreCall::Save(Box::new(team.clone())));
        self.faults.check()?;
        lock(&self.teams).insert(team.id.clone(), team.clone());
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Team>> {
        self.calls.record(TeamStoreCall::Get(id.to_owned()));
        self.faults.check()?;
        Ok(self.peek(id))
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Team>> {
        self.calls.record(TeamStoreCall::GetByName(name.to_owned()));
        self.faults.check()?;
        Ok(lock(&self.teams).values().find(|t| t.name == name).cloned())
    }

    async fn list(&self) -> Result<Vec<Team>> {
        self.calls.record(TeamStoreCall::List);
        self.faults.check()?;
        Ok(self.snapshot())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.calls.record(TeamStoreCall::Delete(id.to_owned()));
        self.faults.check()?;
        lock(&self.teams).shift_remove(id);
        Ok(())
    }
}

// ── GlobalConfigStore ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum GlobalConfigStoreCall {
    Read,
    Write(Box<GlobalConfig>),
}

#[derive(Debug, Clone)]
pub struct FakeGlobalConfigStore {
    config: Arc<Mutex<GlobalConfig>>,
    pub calls: Recorder<GlobalConfigStoreCall>,
    pub faults: Faults,
}

impl Default for FakeGlobalConfigStore {
    fn default() -> Self {
        FakeGlobalConfigStore {
            config: Arc::new(Mutex::new(default_global_config())),
            calls: Recorder::new(),
            faults: Faults::new(),
        }
    }
}

impl FakeGlobalConfigStore {
    pub fn new() -> Self {
        FakeGlobalConfigStore::default()
    }

    pub fn with_config(config: GlobalConfig) -> Self {
        let store = FakeGlobalConfigStore::new();
        store.seed(config);
        store
    }

    pub fn seed(&self, config: GlobalConfig) {
        *lock(&self.config) = config;
    }

    pub fn peek(&self) -> GlobalConfig {
        lock(&self.config).clone()
    }
}

#[async_trait]
impl GlobalConfigStore for FakeGlobalConfigStore {
    async fn read(&self) -> Result<GlobalConfig> {
        self.calls.record(GlobalConfigStoreCall::Read);
        self.faults.check()?;
        Ok(self.peek())
    }

    async fn write(&self, config: &GlobalConfig) -> Result<()> {
        self.calls
            .record(GlobalConfigStoreCall::Write(Box::new(config.clone())));
        self.faults.check()?;
        *lock(&self.config) = config.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::factories::{make_agent, make_run, make_task};
    use futures::StreamExt;
    use orch_domain::{
        GoalStatus, MessageChannel, RunEvent, RunEventType, TaskStatus, TuiPaletteName,
    };

    // ── TaskStore ──────────────────────────────────────────────────────────

    fn ids(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.id.as_str()).collect()
    }

    #[tokio::test]
    async fn task_store_round_trips_and_filters() {
        let store = FakeTaskStore::with_tasks([
            make_task(),
            Task {
                id: "tsk_2".into(),
                status: TaskStatus::Done,
                ..make_task()
            },
        ]);

        assert_eq!(store.list(&TaskFilter::default()).await.unwrap().len(), 2);
        assert_eq!(
            store
                .list(&TaskFilter::with_status(TaskStatus::Done))
                .await
                .unwrap()[0]
                .id,
            "tsk_2"
        );
        assert_eq!(
            store.get("tsk_test1").await.unwrap().unwrap().title,
            "Test task"
        );
        assert!(store.get("nope").await.unwrap().is_none());

        store.delete("tsk_2").await.unwrap();
        assert_eq!(store.len(), 1);
    }

    #[tokio::test]
    async fn save_moves_the_entity_to_the_end_of_the_index() {
        // PARITY.md §6.10: IndexManager::save removes by id then pushes. Index
        // order is last-write order, which §14.4 makes observable through
        // findBestAgent tie-breaking.
        let store = FakeTaskStore::with_tasks([
            Task {
                id: "tsk_a".into(),
                ..make_task()
            },
            Task {
                id: "tsk_b".into(),
                ..make_task()
            },
            Task {
                id: "tsk_c".into(),
                ..make_task()
            },
        ]);
        assert_eq!(ids(&store.snapshot()), vec!["tsk_a", "tsk_b", "tsk_c"]);

        store
            .save(&Task {
                id: "tsk_a".into(),
                status: TaskStatus::Done,
                ..make_task()
            })
            .await
            .unwrap();
        assert_eq!(ids(&store.snapshot()), vec!["tsk_b", "tsk_c", "tsk_a"]);
        // The update itself still landed.
        assert_eq!(store.peek("tsk_a").unwrap().status, TaskStatus::Done);
        assert_eq!(store.len(), 3);

        // A brand-new entity also lands at the end.
        store
            .save(&Task {
                id: "tsk_d".into(),
                ..make_task()
            })
            .await
            .unwrap();
        assert_eq!(
            ids(&store.snapshot()),
            vec!["tsk_b", "tsk_c", "tsk_a", "tsk_d"]
        );
    }

    #[tokio::test]
    async fn agent_list_order_is_last_write_order() {
        // This is the ordering findBestAgent ties fall back to (§14.4).
        let store = FakeAgentStore::with_agents([
            Agent {
                id: "agt_a".into(),
                ..make_agent()
            },
            Agent {
                id: "agt_b".into(),
                ..make_agent()
            },
        ]);
        store
            .save(&Agent {
                id: "agt_a".into(),
                ..make_agent()
            })
            .await
            .unwrap();

        let listed: Vec<String> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(listed, vec!["agt_b".to_owned(), "agt_a".to_owned()]);
    }

    #[tokio::test]
    async fn goal_and_message_saves_also_push_to_the_end() {
        let goals = FakeGoalStore::with_goals([
            goal("goal_a", GoalStatus::Active),
            goal("goal_b", GoalStatus::Active),
        ]);
        goals
            .save(&goal("goal_a", GoalStatus::Paused))
            .await
            .unwrap();
        let ordered: Vec<String> = goals.snapshot().into_iter().map(|g| g.id).collect();
        assert_eq!(ordered, vec!["goal_b".to_owned(), "goal_a".to_owned()]);

        let messages = FakeMessageStore::with_messages([
            message("1", Some("agt_1"), MessageStatus::Pending),
            message("2", Some("agt_1"), MessageStatus::Pending),
        ]);
        messages
            .save(&message("1", Some("agt_1"), MessageStatus::Delivered))
            .await
            .unwrap();
        // `list()` sorts by created_at, so index order is invisible through the
        // trait — assert it on the backing map, which is what WP-1 replicates.
        assert_eq!(messages.peek("1").unwrap().status, MessageStatus::Delivered);
    }

    #[tokio::test]
    async fn a_failed_save_does_not_reorder_the_index() {
        let store = FakeTaskStore::with_tasks([
            Task {
                id: "tsk_a".into(),
                ..make_task()
            },
            Task {
                id: "tsk_b".into(),
                ..make_task()
            },
        ]);
        store.faults.fail_once("disk full");
        assert!(
            store
                .save(&Task {
                    id: "tsk_a".into(),
                    ..make_task()
                })
                .await
                .is_err()
        );
        assert_eq!(ids(&store.snapshot()), vec!["tsk_a", "tsk_b"]);
    }

    #[tokio::test]
    async fn task_store_clones_on_read_so_callers_cannot_mutate_it() {
        let store = FakeTaskStore::with_tasks([make_task()]);
        let mut fetched = store.get("tsk_test1").await.unwrap().unwrap();
        fetched.status = TaskStatus::Done;
        assert_eq!(store.peek("tsk_test1").unwrap().status, TaskStatus::Todo);
    }

    #[tokio::test]
    async fn task_store_clones_on_write_so_later_mutation_is_not_visible() {
        let store = FakeTaskStore::new();
        let mut task = make_task();
        store.save(&task).await.unwrap();
        task.status = TaskStatus::Done;
        assert_eq!(store.peek("tsk_test1").unwrap().status, TaskStatus::Todo);
    }

    #[tokio::test]
    async fn task_store_records_calls_and_the_save_history() {
        let store = FakeTaskStore::new();
        store.save(&make_task()).await.unwrap();
        store
            .save(&Task {
                status: TaskStatus::InProgress,
                ..make_task()
            })
            .await
            .unwrap();
        store.get("tsk_test1").await.unwrap();

        assert_eq!(store.calls.len(), 3);
        assert_eq!(
            store.calls.last(),
            Some(TaskStoreCall::Get("tsk_test1".into()))
        );
        let statuses: Vec<TaskStatus> = store.saved().iter().map(|t| t.status).collect();
        assert_eq!(statuses, vec![TaskStatus::Todo, TaskStatus::InProgress]);
    }

    #[tokio::test]
    async fn seed_and_peek_do_not_record_calls() {
        let store = FakeTaskStore::new();
        store.seed(make_task());
        assert!(store.peek("tsk_test1").is_some());
        assert!(!store.snapshot().is_empty());
        assert!(store.calls.is_empty());
    }

    #[tokio::test]
    async fn task_store_faults_fail_the_call_without_mutating() {
        let store = FakeTaskStore::new();
        store.faults.fail_once("disk full");
        assert_eq!(
            store.save(&make_task()).await.unwrap_err().to_string(),
            "disk full"
        );
        assert!(store.is_empty());
        // The call is still recorded, as a `vi.fn()` mock would.
        assert_eq!(store.calls.len(), 1);
        store.save(&make_task()).await.unwrap();
        assert_eq!(store.len(), 1);
    }

    // ── AgentStore ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn agent_store_looks_up_by_id_and_name() {
        let store = FakeAgentStore::with_agents([make_agent()]);
        assert_eq!(
            store.get("agt_test1").await.unwrap().unwrap().name,
            "TestAgent"
        );
        assert_eq!(
            store.get_by_name("TestAgent").await.unwrap().unwrap().id,
            "agt_test1"
        );
        assert!(store.get_by_name("nope").await.unwrap().is_none());
        assert_eq!(store.list().await.unwrap().len(), 1);
        store.delete("agt_test1").await.unwrap();
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn agent_store_records_the_save_history() {
        let store = FakeAgentStore::new();
        store.save(&make_agent()).await.unwrap();
        assert_eq!(store.saved().len(), 1);
        assert!(
            store
                .calls
                .contains(&AgentStoreCall::Save(Box::new(make_agent())))
        );
    }

    // ── RunStore ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn run_store_filters_by_task_and_agent() {
        let store = FakeRunStore::with_runs([
            make_run(),
            Run {
                id: "run_2".into(),
                task_id: "tsk_other".into(),
                ..make_run()
            },
        ]);
        assert_eq!(store.list_all().await.unwrap().len(), 2);
        assert_eq!(store.list_for_task("tsk_test1").await.unwrap().len(), 1);
        assert_eq!(store.list_for_agent("agt_test1").await.unwrap().len(), 2);
        assert!(store.list_for_task("nope").await.unwrap().is_empty());
    }

    fn event(n: u32) -> RunEvent {
        RunEvent {
            timestamp: format!("2025-01-01T00:00:0{n}.000Z"),
            event_type: RunEventType::AgentOutput,
            data: serde_json::Value::String(format!("line {n}")),
        }
    }

    #[tokio::test]
    async fn run_store_appends_and_tails_events() {
        let store = FakeRunStore::new();
        for n in 0..5 {
            store.append_event("run_test1", &event(n)).await.unwrap();
        }
        assert_eq!(store.read_events("run_test1").await.unwrap().len(), 5);

        let tail = store.read_events_tail("run_test1", 2).await.unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].data, serde_json::json!("line 3"));

        // A tail longer than the log returns everything, not an error.
        assert_eq!(
            store.read_events_tail("run_test1", 99).await.unwrap().len(),
            5
        );
        assert!(store.read_events("nope").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_store_streams_stored_events_then_ends() {
        let store = FakeRunStore::new();
        store.seed_events("run_test1", [event(0), event(1)]);

        let streamed: Vec<RunEvent> = store
            .stream_events("run_test1", CancellationToken::new())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(streamed.len(), 2);
    }

    #[tokio::test]
    async fn run_store_streams_nothing_for_an_already_cancelled_token() {
        let store = FakeRunStore::new();
        store.seed_events("run_test1", [event(0)]);
        let token = CancellationToken::new();
        token.cancel();

        let streamed: Vec<_> = store.stream_events("run_test1", token).collect().await;
        assert!(streamed.is_empty());
    }

    #[tokio::test]
    async fn run_store_records_closed_handles() {
        let store = FakeRunStore::new();
        store.close_run_events("run_test1");
        store.close_run_events("run_2");
        assert_eq!(
            store.closed(),
            vec!["run_test1".to_owned(), "run_2".to_owned()]
        );
    }

    // ── StateStore ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn state_store_round_trips_and_records_the_write_history() {
        let store = FakeStateStore::new();
        assert_eq!(store.read().await.unwrap(), default_state());

        let mut state = default_state();
        state.pid = Some(4242);
        store.write(&state).await.unwrap();
        assert_eq!(store.read().await.unwrap().pid, Some(4242));

        state.claimed.insert("tsk_1".into());
        store.write(&state).await.unwrap();

        let history = store.written();
        assert_eq!(history.len(), 2);
        assert!(history[0].claimed.is_empty());
        assert_eq!(history[1].claimed.len(), 1);
    }

    #[tokio::test]
    async fn state_store_clones_so_a_read_cannot_be_mutated_in_place() {
        let store = FakeStateStore::new();
        let mut read = store.read().await.unwrap();
        read.pid = Some(1);
        assert_eq!(store.peek().pid, None);
    }

    // ── ConfigStore ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn config_store_reads_the_defaults() {
        let store = FakeConfigStore::new();
        let config = store.read().await.unwrap();
        assert_eq!(config.project.name, "my-project");
        assert_eq!(config.scheduling.poll_interval_ms, 10_000);
    }

    #[tokio::test]
    async fn config_store_get_and_set_use_dot_paths() {
        let store = FakeConfigStore::new();
        assert_eq!(
            store.get("defaults.agent.max_turns").await.unwrap(),
            Some(serde_json::json!(50))
        );
        assert_eq!(store.get("nope.nope").await.unwrap(), None);

        store
            .set("defaults.agent.max_turns", serde_json::json!(5))
            .await
            .unwrap();
        assert_eq!(store.read().await.unwrap().defaults.agent.max_turns, 5);
    }

    #[tokio::test]
    async fn config_store_set_preserves_unknown_keys() {
        let mut raw = serde_json::to_value(default_config()).unwrap();
        raw.as_object_mut()
            .unwrap()
            .insert("my_extension".into(), serde_json::json!({ "k": "v" }));
        let store = FakeConfigStore::with_raw(raw);

        store
            .set("project.name", serde_json::json!("proj"))
            .await
            .unwrap();
        assert_eq!(store.peek_raw()["my_extension"]["k"], "v");
        assert_eq!(store.peek_raw()["project"]["name"], "proj");
    }

    // ── ContextStore ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn context_store_lists_sorted_by_key() {
        let store = FakeContextStore::with_entries([("zeta", "z"), ("alpha", "a")]);
        let keys: Vec<String> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, vec!["alpha".to_owned(), "zeta".to_owned()]);

        let all = store.get_all().await.unwrap();
        assert_eq!(all.keys().collect::<Vec<_>>(), vec!["alpha", "zeta"]);
        assert_eq!(all["alpha"], "a");
    }

    #[tokio::test]
    async fn context_store_set_preserves_created_at_and_stores_the_ttl() {
        let store = FakeContextStore::new();
        store.set("k", "v1", None).await.unwrap();
        let first = store.peek("k").unwrap();

        store.set("k", "v2", Some(5_000)).await.unwrap();
        let second = store.peek("k").unwrap();
        assert_eq!(second.value, "v2");
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(second.ttl_ms, Some(5_000));

        store.delete("k").await.unwrap();
        assert!(store.get("k").await.unwrap().is_none());
        assert!(
            store
                .calls
                .contains(&ContextStoreCall::Set("k".into(), "v2".into(), Some(5_000)))
        );
    }

    // ── MessageStore ───────────────────────────────────────────────────────

    fn message(id: &str, to: Option<&str>, status: MessageStatus) -> Message {
        Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: "cli".into(),
            to_agent_id: to.map(str::to_owned),
            subject: "s".into(),
            body: "b".into(),
            created_at: format!("2025-01-01T00:00:00.00{id}Z"),
            expires_at: None,
            status,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }
    }

    #[tokio::test]
    async fn message_store_lists_pending_for_one_recipient() {
        let store = FakeMessageStore::with_messages([
            message("1", Some("agt_1"), MessageStatus::Pending),
            message("2", Some("agt_2"), MessageStatus::Pending),
            message("3", Some("agt_1"), MessageStatus::Delivered),
            message("4", None, MessageStatus::Pending),
        ]);
        let pending = store.list_pending("agt_1").await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "1");
        assert_eq!(store.list().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn message_store_mark_delivered_sets_the_status_and_timestamp() {
        let store =
            FakeMessageStore::with_messages([message("1", Some("agt_1"), MessageStatus::Pending)]);
        store.mark_delivered("1").await.unwrap();
        let message = store.peek("1").unwrap();
        assert_eq!(message.status, MessageStatus::Delivered);
        assert_eq!(message.delivered_at.as_deref(), Some(FIXED_NOW_ISO));

        // A missing id is a no-op, like the real store.
        store.mark_delivered("nope").await.unwrap();
        assert_eq!(store.purge_expired().await.unwrap(), 0);
    }

    // ── GoalStore ──────────────────────────────────────────────────────────

    fn goal(id: &str, status: GoalStatus) -> Goal {
        Goal {
            id: id.into(),
            title: "t".into(),
            description: String::new(),
            status,
            assignee: None,
            created_at: "t0".into(),
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn goal_store_filters_by_status() {
        let store = FakeGoalStore::with_goals([
            goal("goal_1", GoalStatus::Active),
            goal("goal_2", GoalStatus::Achieved),
        ]);
        assert_eq!(store.list(&GoalFilter::default()).await.unwrap().len(), 2);
        let active = store
            .list(&GoalFilter::with_status(GoalStatus::Active))
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "goal_1");

        store
            .save(&goal("goal_3", GoalStatus::Paused))
            .await
            .unwrap();
        assert_eq!(store.snapshot().len(), 3);
        assert_eq!(store.saved().len(), 1);
        store.delete("goal_3").await.unwrap();
        assert!(store.peek("goal_3").is_none());
    }

    // ── TeamStore ──────────────────────────────────────────────────────────

    fn team(id: &str, name: &str) -> Team {
        Team {
            id: id.into(),
            name: name.into(),
            description: None,
            status: orch_domain::TeamStatus::Active,
            members: vec![],
            task_pool: vec![],
            lead_agent_id: "agt_1".into(),
            created_at: "t0".into(),
            updated_at: "t0".into(),
            config: orch_domain::DEFAULT_TEAM_CONFIG,
        }
    }

    #[tokio::test]
    async fn team_store_looks_up_by_id_and_name() {
        let store = FakeTeamStore::with_teams([team("team_1", "squad")]);
        assert_eq!(store.get("team_1").await.unwrap().unwrap().name, "squad");
        assert_eq!(
            store.get_by_name("squad").await.unwrap().unwrap().id,
            "team_1"
        );
        assert!(store.get_by_name("nope").await.unwrap().is_none());
        assert_eq!(store.list().await.unwrap().len(), 1);

        store.save(&team("team_2", "other")).await.unwrap();
        assert_eq!(store.saved().len(), 1);
        store.delete("team_1").await.unwrap();
        assert!(store.peek("team_1").is_none());
    }

    // ── GlobalConfigStore ──────────────────────────────────────────────────

    #[tokio::test]
    async fn global_config_store_round_trips() {
        let store = FakeGlobalConfigStore::new();
        assert_eq!(store.read().await.unwrap(), default_global_config());

        let mut config = default_global_config();
        config.tui.palette = TuiPaletteName::Ocean;
        store.write(&config).await.unwrap();
        assert_eq!(
            store.read().await.unwrap().tui.palette,
            TuiPaletteName::Ocean
        );
        assert_eq!(store.calls.len(), 3);
    }

    // ── Shared behaviour across every store ────────────────────────────────

    #[tokio::test]
    async fn every_store_is_usable_behind_an_arc_dyn_trait_object() {
        let tasks: Arc<dyn TaskStore> = Arc::new(FakeTaskStore::new());
        let agents: Arc<dyn AgentStore> = Arc::new(FakeAgentStore::new());
        let runs: Arc<dyn RunStore> = Arc::new(FakeRunStore::new());
        let state: Arc<dyn StateStore> = Arc::new(FakeStateStore::new());
        let config: Arc<dyn ConfigStore> = Arc::new(FakeConfigStore::new());
        let context: Arc<dyn ContextStore> = Arc::new(FakeContextStore::new());
        let messages: Arc<dyn MessageStore> = Arc::new(FakeMessageStore::new());
        let goals: Arc<dyn GoalStore> = Arc::new(FakeGoalStore::new());
        let teams: Arc<dyn TeamStore> = Arc::new(FakeTeamStore::new());
        let global: Arc<dyn GlobalConfigStore> = Arc::new(FakeGlobalConfigStore::new());

        tasks.save(&make_task()).await.unwrap();
        agents.save(&make_agent()).await.unwrap();
        runs.save(&make_run()).await.unwrap();
        state.write(&default_state()).await.unwrap();
        config.read().await.unwrap();
        context.set("k", "v", None).await.unwrap();
        messages.list().await.unwrap();
        goals.list(&GoalFilter::default()).await.unwrap();
        teams.list().await.unwrap();
        global.read().await.unwrap();
    }

    #[tokio::test]
    async fn a_cloned_store_handle_shares_its_state() {
        let store = FakeTaskStore::new();
        let handle = store.clone();
        handle.save(&make_task()).await.unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.calls.len(), 1);
    }
}
