//! Agent service — ported from `src/application/agent-service.ts`.
//!
//! WP-0b implements the walking-skeleton subset: `create`, `list` and `get`.
//! `remove`, `update`, availability matching and `find_best_agent` are WP-4's.

use std::sync::Arc;

use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::AgentStore;
use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, ApprovalPolicy, CreateAgentInput, DomainError,
    OrchestratorConfig,
};

pub struct AgentService {
    agents: Arc<dyn AgentStore>,
    config: OrchestratorConfig,
    ids: Arc<dyn IdGenerator>,
}

impl AgentService {
    pub fn new(
        agents: Arc<dyn AgentStore>,
        config: OrchestratorConfig,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        AgentService {
            agents,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::FakeAgentStore;
    use orch_domain::{ReasoningEffort, default_config};

    fn service(store: Arc<FakeAgentStore>) -> AgentService {
        AgentService::new(
            store as Arc<dyn AgentStore>,
            default_config(),
            Arc::new(orch_core::test_support::SeqIdGenerator::new()),
        )
    }

    fn input(name: &str, adapter: &str) -> CreateAgentInput {
        CreateAgentInput {
            name: name.into(),
            adapter: adapter.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_shell_agent_matches_the_reference_shape() {
        let store = Arc::new(FakeAgentStore::new());
        let service = service(Arc::clone(&store));
        let agent = service
            .create(CreateAgentInput {
                command: Some("echo hello".into()),
                ..input("shelly", "shell")
            })
            .await
            .unwrap();

        // PARITY.md §4.2 — shell forces approval_policy auto and drops
        // model/effort.
        assert_eq!(agent.id, "agt_0000001");
        assert_eq!(agent.adapter, "shell");
        assert_eq!(agent.config.command.as_deref(), Some("echo hello"));
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Auto));
        assert_eq!(agent.config.max_turns, Some(50));
        assert_eq!(agent.config.timeout_ms, Some(3_600_000));
        assert_eq!(agent.config.stall_timeout_ms, Some(600_000));
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.stats, AgentStats::default());
        assert_eq!(store.peek("agt_0000001").unwrap(), agent);
    }

    #[tokio::test]
    async fn shell_drops_model_and_effort_even_when_given() {
        let service = service(Arc::new(FakeAgentStore::new()));
        let agent = service
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
    async fn an_ai_agent_keeps_model_and_the_configured_approval_policy() {
        let service = service(Arc::new(FakeAgentStore::new()));
        let agent = service
            .create(CreateAgentInput {
                model: Some("claude-sonnet-4-6".into()),
                effort: Some(ReasoningEffort::Medium),
                ..input("claudia", "claude")
            })
            .await
            .unwrap();
        assert_eq!(agent.config.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(agent.config.effort, Some(ReasoningEffort::Medium));
        // The project default, which is also `auto`.
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Auto));
    }

    #[tokio::test]
    async fn a_shell_agent_without_a_command_is_rejected() {
        let service = service(Arc::new(FakeAgentStore::new()));
        for command in [None, Some(String::new()), Some("  ".into())] {
            let err = service
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
    async fn an_empty_name_is_rejected_and_the_name_is_trimmed() {
        let service = service(Arc::new(FakeAgentStore::new()));
        for name in ["", "   "] {
            let err = service
                .create(input(name, "claude"))
                .await
                .expect_err("rejected");
            assert_eq!(err.to_string(), "Agent name is required");
            assert_eq!(err.exit_code(), 2);
        }
        let agent = service.create(input("  spaced  ", "claude")).await.unwrap();
        assert_eq!(agent.name, "spaced");
    }

    #[tokio::test]
    async fn a_duplicate_name_is_rejected() {
        let service = service(Arc::new(FakeAgentStore::new()));
        service.create(input("dup", "claude")).await.unwrap();
        let err = service
            .create(input("dup", "claude"))
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Agent \"dup\" already exists");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn an_empty_adapter_falls_back_to_the_configured_default() {
        let service = service(Arc::new(FakeAgentStore::new()));
        let agent = service.create(input("a", "  ")).await.unwrap();
        assert_eq!(agent.adapter, "claude");
    }

    #[tokio::test]
    async fn get_reports_agent_not_found_with_exit_1() {
        let service = service(Arc::new(FakeAgentStore::new()));
        let err = service.get("agt_nope").await.expect_err("missing");
        assert_eq!(err.to_string(), "Agent not found: agt_nope");
        assert_eq!(err.exit_code(), 1);
    }

    #[tokio::test]
    async fn list_preserves_last_write_order() {
        let store = Arc::new(FakeAgentStore::new());
        let service = service(Arc::clone(&store));
        for name in ["c", "a", "b"] {
            service.create(input(name, "claude")).await.unwrap();
        }
        let names: Vec<String> = service
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, vec!["c".to_owned(), "a".to_owned(), "b".to_owned()]);
    }
}
