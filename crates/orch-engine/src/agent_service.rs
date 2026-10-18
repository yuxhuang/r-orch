//! Agent service — ported from `src/application/agent-service.ts`.
//!
//! CRUD, availability and the scoring that picks an agent for a task.
//!
//! The one piece worth reading the source for is [`AgentService::find_best_agent`]:
//! its tie-breaking is `agentStore.list()` order, which is `_index.json` order,
//! which is **last-write** order. r-orch **replicates** that rather than fixing
//! it (PARITY.md §14.4), which is why the scoring sort must be stable — the
//! workspace `clippy.toml` denies `sort_unstable*` to enforce it.

use std::collections::HashSet;
use std::sync::Arc;

use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::{AgentStore, StateStore};
use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, ApprovalPolicy, CreateAgentInput, DomainError,
    OrchestratorConfig, OrchestratorEvent, ReasoningEffort, Task,
};

use crate::events::EventBus;

/// Fields `agent edit` may change.
///
/// Three-state on purpose, mirroring the TS `fields.x !== undefined` checks:
/// `None` leaves the field alone, and for the string fields an empty value
/// **clears** it (`fields.model || undefined`). `effort` needs the extra layer
/// because its cleared form in TS is the literal `''`, which is not a valid
/// `ReasoningEffort`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateAgentFields {
    pub name: Option<String>,
    pub adapter: Option<String>,
    pub role: Option<String>,
    pub command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Option<ReasoningEffort>>,
    pub approval_policy: Option<ApprovalPolicy>,
}

/// A partial update to [`AgentStats`]. `None` leaves a counter untouched,
/// which is what the TS `Object.assign(agent.stats, update)` does for an
/// absent key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentStatsUpdate {
    pub tasks_completed: Option<u64>,
    pub tasks_failed: Option<u64>,
    pub total_runs: Option<u64>,
    pub total_runtime_ms: Option<u64>,
    pub tokens_used: Option<u64>,
}

pub struct AgentService {
    agents: Arc<dyn AgentStore>,
    state: Arc<dyn StateStore>,
    events: Arc<EventBus>,
    config: OrchestratorConfig,
    ids: Arc<dyn IdGenerator>,
}

impl AgentService {
    pub fn new(
        agents: Arc<dyn AgentStore>,
        state: Arc<dyn StateStore>,
        events: Arc<EventBus>,
        config: OrchestratorConfig,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        AgentService {
            agents,
            state,
            events,
            config,
            ids,
        }
    }

    /// `create` emits **no event** — the TS version does not either, which is
    /// why `agent add` produces no `task:created`-style record.
    pub async fn create(&self, input: CreateAgentInput) -> Result<Agent> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(Error::invalid_arguments("Agent name is required"));
        }

        let adapter = match input.adapter.trim() {
            "" => self.config.defaults.agent.adapter.clone(),
            given => given.to_owned(),
        };
        let is_shell = adapter == "shell";

        let command = input
            .command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        if is_shell && command.is_none() {
            return Err(Error::invalid_arguments("Shell agents require a command"));
        }

        if self.agents.get_by_name(name).await?.is_some() {
            return Err(Error::invalid_arguments(format!(
                "Agent \"{name}\" already exists"
            )));
        }

        let agent = Agent {
            id: self.ids.generate("agt"),
            name: name.to_owned(),
            adapter,
            role: input.role,
            config: AgentConfig {
                command: command.map(str::to_owned),
                // `model` and `effort` are forced absent for shell, whatever
                // was passed (PARITY.md §4.2).
                model: if is_shell { None } else { input.model },
                effort: if is_shell { None } else { input.effort },
                // Shell defaults to `auto` rather than the configured default.
                approval_policy: Some(input.approval_policy.unwrap_or(if is_shell {
                    ApprovalPolicy::Auto
                } else {
                    self.config.defaults.agent.approval_policy
                })),
                max_turns: Some(
                    input
                        .max_turns
                        .unwrap_or(self.config.defaults.agent.max_turns),
                ),
                timeout_ms: Some(
                    input
                        .timeout_ms
                        .unwrap_or(self.config.defaults.agent.timeout_ms),
                ),
                stall_timeout_ms: Some(
                    input
                        .stall_timeout_ms
                        .unwrap_or(self.config.defaults.agent.stall_timeout_ms),
                ),
                env: input.env,
                system_prompt: input.system_prompt,
                workspace_mode: input.workspace_mode,
                skills: input.skills,
            },
            status: AgentStatus::Idle,
            current_task: None,
            autonomous: None,
            stats: AgentStats::default(),
            last_error: None,
            capabilities: Vec::new(),
            remote: None,
        };

        self.agents.save(&agent).await?;
        Ok(agent)
    }

    /// Index order, i.e. last-write order. Not sorted — see
    /// `AgentStore::list`.
    pub async fn list(&self) -> Result<Vec<Agent>> {
        self.agents.list().await
    }

    pub async fn get(&self, id: &str) -> Result<Agent> {
        self.agents
            .get(id)
            .await?
            .ok_or_else(|| Error::Domain(DomainError::AgentNotFound(id.to_owned())))
    }

    /// Delete an agent.
    ///
    /// A `running` agent is only refused when `state.running` actually holds
    /// an entry for it. An agent *stuck* in `running` with no live run is
    /// reset to `idle` and then deleted, because otherwise a crash would leave
    /// an undeletable agent behind.
    pub async fn remove(&self, id: &str) -> Result<()> {
        reject_remote_id(id)?;
        let mut agent = self.get(id).await?;
        if agent.status == AgentStatus::Running {
            let state = self.state.read().await?;
            let is_actually_running = state.running.values().any(|entry| entry.agent_id == id);
            if is_actually_running {
                return Err(Error::invalid_arguments(
                    "Cannot remove a running agent. Stop it first.",
                ));
            }
            agent.status = AgentStatus::Idle;
            self.agents.save(&agent).await?;
        }
        self.agents.delete(id).await
    }

    pub async fn update(&self, id: &str, fields: UpdateAgentFields) -> Result<Agent> {
        reject_remote_id(id)?;
        let mut agent = self.get(id).await?;
        let was_shell = agent.adapter == "shell";

        let adapter = match fields.adapter.as_deref() {
            Some(given) => given.trim().to_owned(),
            None => agent.adapter.clone(),
        };
        if adapter.is_empty() {
            return Err(Error::invalid_arguments("Agent adapter cannot be empty"));
        }
        let entering_shell = adapter == "shell" && !was_shell;

        let command = match fields.command.as_deref() {
            Some(given) => Some(given.trim().to_owned()).filter(|c| !c.is_empty()),
            None => agent.config.command.clone(),
        };
        if adapter == "shell" && command.is_none() {
            return Err(Error::invalid_arguments("Shell agents require a command"));
        }

        if let Some(new_name) = fields.name.as_deref() {
            let trimmed = new_name.trim();
            if trimmed.is_empty() {
                return Err(Error::invalid_arguments("Agent name cannot be empty"));
            }
            // The duplicate check excludes self, so a no-op rename is allowed.
            // The message interpolates the *untrimmed* input, as the TS does.
            if let Some(existing) = self.agents.get_by_name(trimmed).await?
                && existing.id != id
            {
                return Err(Error::invalid_arguments(format!(
                    "Agent \"{new_name}\" already exists"
                )));
            }
            agent.name = trimmed.to_owned();
        }
        if fields.adapter.is_some() {
            agent.adapter = adapter;
        }
        if let Some(role) = fields.role {
            agent.role = Some(role).filter(|r| !r.is_empty());
        }
        if fields.command.is_some() {
            agent.config.command = command;
        }
        if let Some(model) = fields.model {
            agent.config.model = Some(model).filter(|m| !m.is_empty());
        }
        if let Some(effort) = fields.effort {
            agent.config.effort = effort;
        }
        // Switching *into* shell drops model/effort and, absent an explicit
        // choice, forces `auto` — the same normalization `create` applies.
        if entering_shell {
            agent.config.model = None;
            agent.config.effort = None;
        }
        match fields.approval_policy {
            Some(policy) => agent.config.approval_policy = Some(policy),
            None if entering_shell => {
                agent.config.approval_policy = Some(ApprovalPolicy::Auto);
            }
            None => {}
        }

        self.agents.save(&agent).await?;
        Ok(agent)
    }

    /// Replace only authored fields, re-reading runtime state immediately before
    /// the single store save. Legacy/runtime writers remain last-writer-wins.
    pub async fn replace_definition(
        &self,
        id: &str,
        name: String,
        adapter: String,
        role: Option<String>,
        mut config: AgentConfig,
    ) -> Result<Agent> {
        reject_remote_id(id)?;
        if name.trim().is_empty() || adapter.trim().is_empty() {
            return Err(Error::invalid_arguments("name/adapter: cannot be empty"));
        }
        if let Some(existing) = self.agents.get_by_name(name.trim()).await?
            && existing.id != id
        {
            return Err(Error::invalid_arguments(
                "name: another agent already has this name",
            ));
        }
        if adapter.trim() == "shell"
            && !config
                .command
                .as_deref()
                .is_some_and(|command| !command.trim().is_empty())
        {
            return Err(Error::invalid_arguments("Shell agents require a command"));
        }
        if config.max_turns == Some(0)
            || config.timeout_ms == Some(0)
            || config.stall_timeout_ms == Some(0)
        {
            return Err(Error::invalid_arguments(
                "config: runtime limits must be positive",
            ));
        }
        let mut agent = self.get(id).await?;
        if agent.status == AgentStatus::Running || agent.remote.is_some() {
            return Err(Error::invalid_arguments(
                "cannot mutate a running or remote agent definition",
            ));
        }
        if agent.name == name
            && agent.adapter == adapter
            && agent.role == role
            && agent.config == config
        {
            return Ok(agent);
        }
        if adapter.trim() == "shell" {
            config.command = config.command.map(|command| command.trim().to_owned());
            config.model = None;
            config.effort = None;
        }
        agent.name = name.trim().to_owned();
        agent.adapter = adapter.trim().to_owned();
        agent.role = role;
        agent.config = config;
        self.agents.save(&agent).await?;
        Ok(agent)
    }

    pub async fn disable(&self, id: &str) -> Result<Agent> {
        self.set_status(id, AgentStatus::Disabled).await
    }

    pub async fn enable(&self, id: &str) -> Result<Agent> {
        self.set_status(id, AgentStatus::Idle).await
    }

    pub async fn set_autonomous(&self, id: &str, enabled: bool) -> Result<Agent> {
        let mut agent = self.get(id).await?;
        agent.autonomous = Some(enabled);
        self.agents.save(&agent).await?;
        self.events.emit(OrchestratorEvent::AgentAutonomousToggled {
            agent_id: id.to_owned(),
            autonomous: enabled,
        });
        Ok(agent)
    }

    pub async fn set_status(&self, id: &str, status: AgentStatus) -> Result<Agent> {
        reject_remote_id(id)?;
        if status == AgentStatus::Offline {
            return Err(Error::invalid_arguments(
                "Offline is a remote display status",
            ));
        }
        let mut agent = self.get(id).await?;
        agent.status = status;
        self.agents.save(&agent).await?;
        Ok(agent)
    }

    /// Overwrite the given counters. The orchestrator computes absolute values
    /// (`old + 1`) and passes them in, so this assigns rather than adds —
    /// matching `Object.assign(agent.stats, update)`.
    pub async fn update_stats(&self, id: &str, update: AgentStatsUpdate) -> Result<Agent> {
        let mut agent = self.get(id).await?;
        if let Some(value) = update.tasks_completed {
            agent.stats.tasks_completed = value;
        }
        if let Some(value) = update.tasks_failed {
            agent.stats.tasks_failed = value;
        }
        if let Some(value) = update.total_runs {
            agent.stats.total_runs = value;
        }
        if let Some(value) = update.total_runtime_ms {
            agent.stats.total_runtime_ms = value;
        }
        if let Some(value) = update.tokens_used {
            agent.stats.tokens_used = Some(value);
        }
        self.agents.save(&agent).await?;
        Ok(agent)
    }

    /// Pick the best idle agent for a task (PARITY.md §7.7).
    ///
    /// - An explicit `assignee` is a **hard constraint**: it is matched by id
    ///   or name across *all* agents and returned only if idle. Otherwise the
    ///   task waits — it is never handed to a different agent.
    /// - Otherwise: **+50 per** task label equal (case-insensitively) to one of
    ///   the agent's skills, **+30** if any label is a case-insensitive
    ///   substring of the role, **+20** for being idle, and **+0..10** for the
    ///   success rate.
    ///
    /// The final `sort_by` is **stable**, so equal scores keep
    /// `AgentStore::list` order (PARITY.md §14.4).
    pub async fn find_best_agent(&self, task: &Task) -> Result<Option<Agent>> {
        let agents = self.agents.list().await?;
        Ok(Self::find_best_in(task, agents))
    }

    /// Dispatch supplies its explicitly merged directory and reservations.
    pub fn find_best_in(task: &Task, agents: Vec<Agent>) -> Option<Agent> {
        let available: Vec<Agent> = agents
            .iter()
            .filter(|a| agent_eligible(task, a))
            .cloned()
            .collect();

        if available.is_empty() {
            return None;
        }

        if let Some(assignee) = task.assignee.as_deref().filter(|a| !a.is_empty()) {
            let assigned = agents
                .into_iter()
                .find(|a| a.id == assignee || a.name == assignee);
            return assigned.filter(|a| agent_eligible(task, a));
        }

        // Lower-cased once, as the TS does, because the inner loops are O(n·m).
        let lower_labels: Vec<String> = task.labels.iter().map(|l| l.to_lowercase()).collect();
        let labels = (!lower_labels.is_empty()).then_some(lower_labels.as_slice());

        let mut scored: Vec<(i64, Agent)> = available
            .into_iter()
            .map(|agent| (score_agent(&agent, labels), agent))
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

        scored.into_iter().next().map(|(_, agent)| agent)
    }
}

fn reject_remote_id(id: &str) -> Result<()> {
    if id.contains('@') {
        Err(Error::invalid_arguments(
            "Remote agents cannot be mutated locally",
        ))
    } else {
        Ok(())
    }
}

/// Capability labels constrain explicit and automatic assignment alike.
pub fn agent_eligible(task: &Task, agent: &Agent) -> bool {
    agent.status == AgentStatus::Idle
        && agent.remote.as_ref().is_none_or(|remote| remote.online)
        && task
            .labels
            .iter()
            .filter_map(|label| label.strip_prefix("cap:"))
            .all(|cap| agent.capabilities.iter().any(|actual| actual == cap))
}

/// The scoring from PARITY.md §7.7. Split out so it is unit-testable without
/// a store, and so the weights sit in one place.
fn score_agent(agent: &Agent, lower_labels: Option<&[String]>) -> i64 {
    let mut score = 0i64;

    if let (Some(labels), Some(skills)) = (lower_labels, agent.config.skills.as_deref())
        && !skills.is_empty()
    {
        let skill_set: HashSet<String> = skills.iter().map(|s| s.to_lowercase()).collect();
        score += 50 * labels.iter().filter(|l| skill_set.contains(*l)).count() as i64;
    }

    // `contains`, not equality: a label of "end" matches both "frontend" and
    // "backend". That is a known false-positive source the TS suite asserts on
    // deliberately, so it is replicated.
    if let (Some(labels), Some(role)) = (lower_labels, agent.role.as_deref()) {
        let lower_role = role.to_lowercase();
        if labels.iter().any(|l| lower_role.contains(l.as_str())) {
            score += 30;
        }
    }

    if agent.status == AgentStatus::Idle {
        score += 20;
    }

    let total = agent.stats.tasks_completed + agent.stats.tasks_failed;
    if total > 0 {
        score += js_round(agent.stats.tasks_completed as f64 / total as f64 * 10.0);
    }

    score
}

/// `Math.round`: half rounds **up** (towards +∞), unlike Rust's `f64::round`,
/// which rounds half away from zero. The inputs here are non-negative so the
/// two agree, but spelling it out keeps the parity claim checkable.
fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{FakeAgentStore, FakeStateStore, SeqIdGenerator, make_task};
    use orch_domain::{OrchestratorEventType, OrchestratorState, RunningEntry, default_config};

    struct Harness {
        service: AgentService,
        agents: Arc<FakeAgentStore>,
        state: Arc<FakeStateStore>,
        bus: Arc<EventBus>,
    }

    fn harness(agents: Vec<Agent>) -> Harness {
        let store = Arc::new(FakeAgentStore::with_agents(agents));
        let state = Arc::new(FakeStateStore::new());
        let bus = Arc::new(EventBus::new());
        let service = AgentService::new(
            Arc::clone(&store) as Arc<dyn AgentStore>,
            Arc::clone(&state) as Arc<dyn StateStore>,
            Arc::clone(&bus),
            default_config(),
            Arc::new(SeqIdGenerator::new()),
        );
        Harness {
            service,
            agents: store,
            state,
            bus,
        }
    }

    #[tokio::test]
    async fn definition_replacement_clears_options_preserves_runtime_and_skips_noop() {
        let mut before = agent("agt_1");
        before.role = Some("old".into());
        before.stats.tasks_completed = 7;
        let h = harness(vec![before.clone()]);
        let after = h
            .service
            .replace_definition(
                "agt_1",
                "renamed".into(),
                "codex".into(),
                None,
                AgentConfig::default(),
            )
            .await
            .unwrap();
        assert_eq!(after.stats, before.stats);
        assert_eq!(after.config, AgentConfig::default());
        assert_eq!(after.role, None);
        assert_eq!(after.name, "renamed");
        let again = h
            .service
            .replace_definition(
                "agt_1",
                after.name.clone(),
                after.adapter.clone(),
                None,
                after.config.clone(),
            )
            .await
            .unwrap();
        assert_eq!(again, after);
        assert_eq!(h.agents.saved().len(), 1, "no-op must not save");
    }

    #[tokio::test]
    async fn identical_imported_definition_does_not_normalize_or_write() {
        let mut before = agent("agt_1");
        before.name = "  imported name  ".into();
        before.adapter = "shell".into();
        before.config.command = Some("  printf stub  ".into());
        let h = harness(vec![before.clone()]);
        let after = h
            .service
            .replace_definition(
                &before.id,
                before.name.clone(),
                before.adapter.clone(),
                before.role.clone(),
                before.config.clone(),
            )
            .await
            .unwrap();
        assert_eq!(after, before);
        assert!(h.agents.saved().is_empty());
    }

    #[tokio::test]
    async fn definition_replacement_refuses_running_and_invalid_shell_without_writes() {
        let mut before = agent("agt_1");
        before.status = AgentStatus::Running;
        let h = harness(vec![before.clone()]);
        assert!(
            h.service
                .replace_definition(
                    "agt_1",
                    "renamed".into(),
                    "codex".into(),
                    None,
                    AgentConfig::default()
                )
                .await
                .is_err()
        );
        assert_eq!(h.service.get("agt_1").await.unwrap(), before);
        assert!(h.agents.saved().is_empty());
        let h = harness(vec![agent("agt_2")]);
        assert!(
            h.service
                .replace_definition(
                    "agt_2",
                    "test".into(),
                    "shell".into(),
                    None,
                    AgentConfig::default()
                )
                .await
                .is_err()
        );
    }

    /// The local factory from `agent-service.test.ts`, which differs from the
    /// shared one: adapter `claude`, approval policy `suggest`.
    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: "test-agent".into(),
            adapter: "claude".into(),
            role: None,
            config: AgentConfig {
                approval_policy: Some(ApprovalPolicy::Suggest),
                max_turns: Some(50),
                timeout_ms: Some(3_600_000),
                stall_timeout_ms: Some(300_000),
                ..AgentConfig::default()
            },
            status: AgentStatus::Idle,
            current_task: None,
            autonomous: None,
            stats: AgentStats::default(),
            last_error: None,
            capabilities: Vec::new(),
            remote: None,
        }
    }

    fn with_skills(mut a: Agent, skills: &[&str]) -> Agent {
        a.config.skills = Some(skills.iter().map(|s| (*s).to_owned()).collect());
        a
    }

    fn with_stats(mut a: Agent, completed: u64, failed: u64) -> Agent {
        a.stats.tasks_completed = completed;
        a.stats.tasks_failed = failed;
        a.stats.total_runs = completed + failed;
        a
    }

    fn input(name: &str, adapter: &str) -> CreateAgentInput {
        CreateAgentInput {
            name: name.into(),
            adapter: adapter.into(),
            ..Default::default()
        }
    }

    fn labelled(labels: &[&str]) -> Task {
        Task {
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
            ..make_task()
        }
    }

    // ── create ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_applies_the_configured_defaults() {
        let h = harness(vec![]);
        let agent = h.service.create(input("claude-1", "claude")).await.unwrap();
        assert_eq!(agent.id, "agt_0000001");
        assert_eq!(agent.name, "claude-1");
        assert_eq!(agent.adapter, "claude");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(
            agent.config.approval_policy,
            Some(default_config().defaults.agent.approval_policy)
        );
        assert_eq!(h.agents.peek("agt_0000001").unwrap(), agent);
    }

    #[tokio::test]
    async fn a_shell_agent_matches_the_reference_shape() {
        let h = harness(vec![]);
        let agent = h
            .service
            .create(CreateAgentInput {
                command: Some("echo hello".into()),
                ..input("shelly", "shell")
            })
            .await
            .unwrap();

        // PARITY.md §4.2 — shell forces approval_policy auto and drops
        // model/effort.
        assert_eq!(agent.adapter, "shell");
        assert_eq!(agent.config.command.as_deref(), Some("echo hello"));
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Auto));
        assert_eq!(agent.config.max_turns, Some(50));
        assert_eq!(agent.config.timeout_ms, Some(3_600_000));
        assert_eq!(agent.config.stall_timeout_ms, Some(600_000));
        assert_eq!(agent.stats, AgentStats::default());
    }

    #[tokio::test]
    async fn shell_drops_model_and_effort_even_when_given() {
        let h = harness(vec![]);
        let agent = h
            .service
            .create(CreateAgentInput {
                command: Some("echo hi".into()),
                model: Some("claude-opus-4-6".into()),
                effort: Some(ReasoningEffort::High),
                ..input("shelly", "shell")
            })
            .await
            .unwrap();
        assert_eq!(agent.config.model, None);
        assert_eq!(agent.config.effort, None);

        let value = serde_json::to_value(&agent).unwrap();
        assert!(value["config"].get("model").is_none());
        assert!(value["config"].get("effort").is_none());
    }

    #[tokio::test]
    async fn create_stores_effort_when_provided_and_leaves_it_unset_otherwise() {
        let h = harness(vec![]);
        let with_effort = h
            .service
            .create(CreateAgentInput {
                effort: Some(ReasoningEffort::High),
                ..input("effort-agent", "claude")
            })
            .await
            .unwrap();
        assert_eq!(with_effort.config.effort, Some(ReasoningEffort::High));

        let without = h
            .service
            .create(input("plain-agent", "claude"))
            .await
            .unwrap();
        assert_eq!(without.config.effort, None);
    }

    #[tokio::test]
    async fn an_ai_agent_keeps_model_and_the_configured_approval_policy() {
        let h = harness(vec![]);
        let agent = h
            .service
            .create(CreateAgentInput {
                model: Some("claude-sonnet-4-6".into()),
                effort: Some(ReasoningEffort::Medium),
                ..input("claudia", "claude")
            })
            .await
            .unwrap();
        assert_eq!(agent.config.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(agent.config.effort, Some(ReasoningEffort::Medium));
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Auto));
    }

    #[tokio::test]
    async fn a_shell_agent_without_a_command_is_rejected() {
        let h = harness(vec![]);
        for command in [None, Some(String::new()), Some("  ".into())] {
            let err = h
                .service
                .create(CreateAgentInput {
                    command,
                    ..input("shelly", "shell")
                })
                .await
                .expect_err("rejected");
            assert_eq!(err.to_string(), "Shell agents require a command");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[tokio::test]
    async fn create_trims_the_shell_command() {
        let h = harness(vec![]);
        let agent = h
            .service
            .create(CreateAgentInput {
                command: Some("  npm test  ".into()),
                ..input("shelly", "shell")
            })
            .await
            .unwrap();
        assert_eq!(agent.config.command.as_deref(), Some("npm test"));
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Auto));
    }

    #[tokio::test]
    async fn an_empty_name_is_rejected_and_the_name_is_trimmed() {
        let h = harness(vec![]);
        for name in ["", "   "] {
            let err = h
                .service
                .create(input(name, "claude"))
                .await
                .expect_err("rejected");
            assert_eq!(err.to_string(), "Agent name is required");
            assert_eq!(err.exit_code(), 2);
        }
        let agent = h
            .service
            .create(input("  spaced  ", "claude"))
            .await
            .unwrap();
        assert_eq!(agent.name, "spaced");
    }

    #[tokio::test]
    async fn a_duplicate_name_is_rejected() {
        let h = harness(vec![]);
        h.service.create(input("dup", "claude")).await.unwrap();
        let err = h
            .service
            .create(input("dup", "claude"))
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Agent \"dup\" already exists");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn an_empty_adapter_falls_back_to_the_configured_default() {
        let h = harness(vec![]);
        let agent = h.service.create(input("a", "  ")).await.unwrap();
        assert_eq!(agent.adapter, "claude");
    }

    // ── get / list ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_returns_the_agent_or_reports_not_found_with_exit_1() {
        let h = harness(vec![agent("agt_test1")]);
        assert_eq!(h.service.get("agt_test1").await.unwrap().id, "agt_test1");

        let err = h.service.get("agt_missing").await.expect_err("missing");
        assert_eq!(err.to_string(), "Agent not found: agt_missing");
        assert_eq!(err.exit_code(), 1);
    }

    #[tokio::test]
    async fn list_preserves_last_write_order() {
        let h = harness(vec![]);
        for name in ["c", "a", "b"] {
            h.service.create(input(name, "claude")).await.unwrap();
        }
        let names: Vec<String> = h
            .service
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, vec!["c".to_owned(), "a".to_owned(), "b".to_owned()]);
    }

    // ── remove ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn remove_deletes_an_idle_agent() {
        let h = harness(vec![agent("agt_test1")]);
        h.service.remove("agt_test1").await.unwrap();
        assert!(h.agents.peek("agt_test1").is_none());
    }

    #[tokio::test]
    async fn remove_refuses_a_running_agent_that_has_a_live_run() {
        let running = Agent {
            status: AgentStatus::Running,
            ..agent("agt_test1")
        };
        let h = harness(vec![running]);
        h.state.seed(OrchestratorState {
            running: [(
                "tsk_1".to_owned(),
                RunningEntry {
                    run_id: "run_1".into(),
                    agent_id: "agt_test1".into(),
                    task_id: "tsk_1".into(),
                    pid: 1234,
                    started_at: "2025-01-01T00:00:00Z".into(),
                    last_event_at: "2025-01-01T00:00:00Z".into(),
                    remote: None,
                },
            )]
            .into_iter()
            .collect(),
            ..OrchestratorState::default()
        });

        let err = h.service.remove("agt_test1").await.expect_err("refused");
        assert_eq!(
            err.to_string(),
            "Cannot remove a running agent. Stop it first."
        );
        assert_eq!(err.exit_code(), 2);
        assert!(h.agents.peek("agt_test1").is_some());
    }

    #[tokio::test]
    async fn remove_resets_a_stale_running_status_and_then_deletes() {
        let running = Agent {
            status: AgentStatus::Running,
            ..agent("agt_test1")
        };
        let h = harness(vec![running]);

        h.service.remove("agt_test1").await.unwrap();
        // The reset was persisted before the delete, which is what makes the
        // agent recoverable if the delete itself fails.
        let saved = h.agents.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].status, AgentStatus::Idle);
        assert!(h.agents.peek("agt_test1").is_none());
    }

    // ── disable / enable / set_autonomous ────────────────────────────────

    #[tokio::test]
    async fn disable_and_enable_move_the_status() {
        let h = harness(vec![agent("agt_test1")]);
        assert_eq!(
            h.service.disable("agt_test1").await.unwrap().status,
            AgentStatus::Disabled
        );
        assert_eq!(
            h.service.enable("agt_test1").await.unwrap().status,
            AgentStatus::Idle
        );
    }

    #[tokio::test]
    async fn set_autonomous_persists_and_emits() {
        let h = harness(vec![agent("agt_test1")]);
        let mut sub = h.bus.subscribe();

        let agent = h.service.set_autonomous("agt_test1", true).await.unwrap();
        assert_eq!(agent.autonomous, Some(true));
        assert_eq!(h.agents.peek("agt_test1").unwrap().autonomous, Some(true));

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type(),
            OrchestratorEventType::AgentAutonomousToggled
        );

        assert_eq!(
            h.service
                .set_autonomous("agt_test1", false)
                .await
                .unwrap()
                .autonomous,
            Some(false)
        );
    }

    #[tokio::test]
    async fn update_stats_assigns_only_the_given_counters() {
        let h = harness(vec![with_stats(agent("agt_test1"), 2, 1)]);
        let updated = h
            .service
            .update_stats(
                "agt_test1",
                AgentStatsUpdate {
                    tasks_completed: Some(3),
                    tokens_used: Some(1234),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.stats.tasks_completed, 3);
        assert_eq!(updated.stats.tasks_failed, 1);
        assert_eq!(updated.stats.tokens_used, Some(1234));
    }

    // ── update ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_sets_the_adapter() {
        let h = harness(vec![agent("agt_test1")]);
        let updated = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    adapter: Some("grok".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.adapter, "grok");
    }

    #[tokio::test]
    async fn update_rejects_a_blank_adapter() {
        let h = harness(vec![agent("agt_test1")]);
        let err = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    adapter: Some("   ".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Agent adapter cannot be empty");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn update_clears_the_model_when_set_to_an_empty_string() {
        let mut existing = agent("agt_test1");
        existing.config.model = Some("claude-sonnet-4-6".into());
        let h = harness(vec![existing]);

        let updated = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    model: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.config.model, None);
    }

    #[tokio::test]
    async fn update_sets_and_clears_the_effort() {
        let h = harness(vec![agent("agt_test1")]);
        let set = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    effort: Some(Some(ReasoningEffort::Low)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(set.config.effort, Some(ReasoningEffort::Low));

        let cleared = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    effort: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(cleared.config.effort, None);
    }

    #[tokio::test]
    async fn update_requires_a_command_when_switching_to_shell() {
        let h = harness(vec![agent("agt_test1")]);
        let err = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    adapter: Some("shell".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Shell agents require a command");
        // Nothing was persisted: validation precedes every mutation.
        assert!(h.agents.saved().is_empty());
    }

    #[tokio::test]
    async fn update_trims_a_shell_command() {
        let mut existing = agent("agt_test1");
        existing.adapter = "shell".into();
        existing.config.command = Some("npm test".into());
        let h = harness(vec![existing]);

        let updated = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    command: Some("  npm run lint  ".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.config.command.as_deref(), Some("npm run lint"));
    }

    #[tokio::test]
    async fn update_preserves_an_explicit_policy_on_an_existing_shell_agent() {
        let mut existing = agent("agt_test1");
        existing.adapter = "shell".into();
        existing.config.command = Some("npm test".into());
        existing.config.approval_policy = Some(ApprovalPolicy::Manual);
        let h = harness(vec![existing]);

        let updated = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    command: Some("npm run lint".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // Already shell, so `entering_shell` is false and `auto` is not forced.
        assert_eq!(updated.config.approval_policy, Some(ApprovalPolicy::Manual));
    }

    #[tokio::test]
    async fn update_can_switch_to_shell_when_a_command_comes_in_the_same_call() {
        let mut existing = agent("agt_test1");
        existing.config.model = Some("claude-sonnet-4-6".into());
        existing.config.effort = Some(ReasoningEffort::High);
        existing.config.approval_policy = Some(ApprovalPolicy::Suggest);
        let h = harness(vec![existing]);

        let updated = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    adapter: Some("shell".into()),
                    command: Some("npm test".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.adapter, "shell");
        assert_eq!(updated.config.command.as_deref(), Some("npm test"));
        assert_eq!(updated.config.approval_policy, Some(ApprovalPolicy::Auto));
        assert_eq!(updated.config.model, None);
        assert_eq!(updated.config.effort, None);
    }

    #[tokio::test]
    async fn update_rejects_an_empty_name_and_a_duplicate() {
        let h = harness(vec![agent("agt_test1"), {
            let mut other = agent("agt_test2");
            other.name = "taken".into();
            other
        }]);

        let err = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    name: Some("  ".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Agent name cannot be empty");

        let err = h
            .service
            .update(
                "agt_test1",
                UpdateAgentFields {
                    name: Some("taken".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Agent \"taken\" already exists");

        // Renaming to its own name is allowed.
        assert_eq!(
            h.service
                .update(
                    "agt_test1",
                    UpdateAgentFields {
                        name: Some("test-agent".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
                .name,
            "test-agent"
        );
    }

    // ── find_best_agent ──────────────────────────────────────────────────

    #[tokio::test]
    async fn find_best_agent_returns_none_when_no_agents_are_available() {
        let h = harness(vec![]);
        assert!(
            h.service
                .find_best_agent(&make_task())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_explicit_assignee_is_honoured_only_while_idle() {
        let h = harness(vec![agent("agt_1")]);
        let task = Task {
            assignee: Some("agt_1".into()),
            ..make_task()
        };
        assert_eq!(
            h.service.find_best_agent(&task).await.unwrap().unwrap().id,
            "agt_1"
        );

        let busy = harness(vec![Agent {
            status: AgentStatus::Running,
            ..agent("agt_1")
        }]);
        // An empty idle pool short-circuits before the assignee is consulted.
        assert!(busy.service.find_best_agent(&task).await.unwrap().is_none());

        // With another idle agent present, the pool is non-empty and the
        // assignee constraint is what refuses: the task waits rather than
        // being handed to the other agent.
        let mixed = harness(vec![
            Agent {
                status: AgentStatus::Running,
                ..agent("agt_1")
            },
            {
                let mut other = agent("agt_2");
                other.name = "other".into();
                other
            },
        ]);
        assert!(
            mixed
                .service
                .find_best_agent(&task)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_assignee_can_be_an_agent_name() {
        let mut named = agent("agt_1");
        named.name = "backend-dev".into();
        let h = harness(vec![named]);
        let task = Task {
            assignee: Some("backend-dev".into()),
            ..make_task()
        };
        assert_eq!(
            h.service.find_best_agent(&task).await.unwrap().unwrap().id,
            "agt_1"
        );
    }

    #[tokio::test]
    async fn an_assignee_that_matches_no_agent_yields_none() {
        let h = harness(vec![agent("agt_1")]);
        let task = Task {
            assignee: Some("agt_nonexistent".into()),
            ..make_task()
        };
        assert!(h.service.find_best_agent(&task).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_label_matches_through_the_role() {
        let mut first = agent("agt_1");
        first.name = "agent-1".into();
        first.role = Some("frontend".into());
        let mut second = agent("agt_2");
        second.name = "agent-2".into();
        second.role = Some("backend developer".into());
        let h = harness(vec![first, second]);

        assert_eq!(
            h.service
                .find_best_agent(&labelled(&["backend"]))
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_2"
        );
    }

    #[tokio::test]
    async fn a_skill_match_beats_a_role_match() {
        let mut first = with_skills(agent("agt_1"), &["frontend", "css"]);
        first.name = "agent-1".into();
        first.role = Some("backend developer".into());
        let mut second = with_skills(agent("agt_2"), &["backend", "api"]);
        second.name = "agent-2".into();
        second.role = Some("generalist".into());
        let h = harness(vec![first, second]);

        // agt_2: skill 50 + idle 20 = 70; agt_1: role 30 + idle 20 = 50.
        assert_eq!(
            h.service
                .find_best_agent(&labelled(&["backend"]))
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_2"
        );
    }

    #[tokio::test]
    async fn skill_matches_accumulate() {
        let mut first = with_skills(agent("agt_1"), &["typescript", "testing"]);
        first.name = "agent-1".into();
        let mut second = with_skills(agent("agt_2"), &["typescript"]);
        second.name = "agent-2".into();
        let h = harness(vec![first, second]);

        // agt_1: 50 + 50 + 20 = 120; agt_2: 50 + 20 = 70.
        assert_eq!(
            h.service
                .find_best_agent(&labelled(&["typescript", "testing"]))
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_1"
        );
    }

    #[tokio::test]
    async fn the_success_rate_bonus_breaks_an_otherwise_equal_tie() {
        let h = harness(vec![with_stats(agent("agt_1"), 9, 1), {
            let mut second = with_stats(agent("agt_2"), 1, 9);
            second.name = "agent-2".into();
            second
        }]);
        // agt_1: 20 + 9 = 29; agt_2: 20 + 1 = 21.
        assert_eq!(
            h.service
                .find_best_agent(&make_task())
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_1"
        );
    }

    #[tokio::test]
    async fn zero_task_history_scores_no_success_bonus_and_does_not_divide_by_zero() {
        let h = harness(vec![agent("agt_1")]);
        assert_eq!(
            h.service
                .find_best_agent(&make_task())
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_1"
        );
        assert_eq!(score_agent(&agent("agt_1"), None), 20);
    }

    #[tokio::test]
    async fn only_idle_agents_are_candidates() {
        let h = harness(vec![
            Agent {
                status: AgentStatus::Running,
                ..agent("agt_1")
            },
            {
                let mut second = agent("agt_2");
                second.name = "agent-2".into();
                second
            },
        ]);
        assert_eq!(
            h.service
                .find_best_agent(&make_task())
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_2"
        );
    }

    #[tokio::test]
    async fn a_disabled_agent_is_excluded_even_with_a_perfect_skill_match() {
        let mut disabled = with_skills(agent("agt_1"), &["backend"]);
        disabled.status = AgentStatus::Disabled;
        let mut idle = agent("agt_2");
        idle.name = "agent-2".into();
        let h = harness(vec![disabled, idle]);

        assert_eq!(
            h.service
                .find_best_agent(&labelled(&["backend"]))
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_2"
        );
    }

    #[tokio::test]
    async fn skills_match_case_insensitively() {
        let mut only = with_skills(agent("agt_1"), &["TypeScript", "REACT"]);
        only.name = "agent-1".into();
        let h = harness(vec![only]);

        let best = h
            .service
            .find_best_agent(&labelled(&["typescript", "react"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(best.id, "agt_1");
        assert_eq!(
            score_agent(&best, Some(&["typescript".into(), "react".into()])),
            120
        );
    }

    #[tokio::test]
    async fn role_matching_is_a_substring_test_and_can_false_positive() {
        // Replicated deliberately: "end" is inside both "frontend" and
        // "backend", so both score 50 and the earlier pool entry wins.
        let mut first = agent("agt_1");
        first.name = "agent-1".into();
        first.role = Some("frontend developer".into());
        let mut second = agent("agt_2");
        second.name = "agent-2".into();
        second.role = Some("backend developer".into());
        let h = harness(vec![first, second]);

        let best = h
            .service
            .find_best_agent(&labelled(&["end"]))
            .await
            .unwrap()
            .unwrap();
        assert!(best.role.unwrap().contains("end"));
    }

    #[tokio::test]
    async fn skill_role_and_success_rate_combine() {
        let mut first = with_stats(with_skills(agent("agt_1"), &["api"]), 10, 0);
        first.name = "agent-1".into();
        first.role = Some("backend developer".into());
        let mut second = with_stats(with_skills(agent("agt_2"), &["api", "backend"]), 5, 5);
        second.name = "agent-2".into();
        second.role = Some("generalist".into());
        let h = harness(vec![first, second]);

        // agt_1: 50 + 30 + 20 + 10 = 110; agt_2: 50 + 50 + 20 + 5 = 125.
        assert_eq!(
            h.service
                .find_best_agent(&labelled(&["api", "backend"]))
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_2"
        );
    }

    #[tokio::test]
    async fn none_when_every_agent_is_disabled_or_running() {
        let h = harness(vec![
            Agent {
                status: AgentStatus::Disabled,
                ..agent("agt_1")
            },
            Agent {
                status: AgentStatus::Running,
                name: "agent-2".into(),
                ..agent("agt_2")
            },
        ]);
        assert!(
            h.service
                .find_best_agent(&make_task())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn labels_do_not_match_an_agent_with_neither_skills_nor_a_role() {
        let bare = agent("agt_1");
        let mut skilled = with_skills(agent("agt_2"), &["backend"]);
        skilled.name = "agent-2".into();
        let h = harness(vec![bare, skilled]);

        assert_eq!(
            h.service
                .find_best_agent(&labelled(&["backend"]))
                .await
                .unwrap()
                .unwrap()
                .id,
            "agt_2"
        );
    }

    /// `defensive-validation.test.ts` Fix #4: a task whose `labels` is absent
    /// or `null` must not crash the scorer. Rust's `Vec<String>` with
    /// `#[serde(default)]` makes both deserialize to an empty vector, so the
    /// crash is unrepresentable — this pins the resulting score instead.
    #[tokio::test]
    async fn an_empty_or_absent_label_list_scores_the_idle_bonus_only() {
        let skilled = with_skills(agent("agt_1"), &["backend"]);
        let h = harness(vec![skilled.clone()]);

        let best = h
            .service
            .find_best_agent(&make_task())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(best.id, "agt_1");
        assert_eq!(score_agent(&skilled, None), 20);
        assert_eq!(score_agent(&skilled, Some(&[])), 20);
        // And the match still scores when labels are supplied.
        assert_eq!(score_agent(&skilled, Some(&["backend".into()])), 70);
    }

    #[test]
    fn the_success_rate_bonus_rounds_like_math_round() {
        // 1/3 → 3.33 → 3; 1/2 → 5; 2/3 → 6.67 → 7; 1/1 → 10.
        assert_eq!(score_agent(&with_stats(agent("a"), 1, 2), None), 23);
        assert_eq!(score_agent(&with_stats(agent("a"), 1, 1), None), 25);
        assert_eq!(score_agent(&with_stats(agent("a"), 2, 1), None), 27);
        assert_eq!(score_agent(&with_stats(agent("a"), 1, 0), None), 30);
        assert_eq!(score_agent(&with_stats(agent("a"), 0, 1), None), 20);
    }
}
