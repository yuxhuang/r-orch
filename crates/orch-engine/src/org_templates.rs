//! One resolution path for template preview, deployment and shop installation.
use crate::Engine;
use orch_core::{Error, Result};
use orch_domain::org_templates::Bundle;
use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, CreateAgentInput, CreateGoalInput,
    CreateTeamInput, OrchestratorConfig, Team,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// Resolve all effective fields through the same pure path. The assembly
/// layer supplies its record encoder's size check, so the engine does not
/// depend on a concrete storage format or implementation.
pub fn resolve_inputs(
    bundle: &Bundle,
    config: &OrchestratorConfig,
    adapter: Option<&str>,
    model: Option<&str>,
    validate_record: fn(&Agent) -> Result<()>,
) -> Result<Vec<CreateAgentInput>> {
    bundle.validate().map_err(Error::invalid_arguments)?;
    let defaults = bundle.org.defaults.as_ref();
    let org_adapter = defaults
        .and_then(|d| d.adapter.as_deref())
        .unwrap_or(&config.defaults.agent.adapter);
    let mut inputs = Vec::with_capacity(bundle.agents.len());
    for agent in &bundle.agents {
        let bound = agent.adapter.as_deref().unwrap_or(org_adapter);
        let final_adapter = adapter.unwrap_or(bound);
        let mut fields = serde_json::to_value(defaults)
            .map_err(|e| Error::other(e.to_string()))?
            .as_object()
            .cloned()
            .unwrap_or_default();
        let authored = serde_json::to_value(agent).map_err(|e| Error::other(e.to_string()))?;
        for (key, value) in authored.as_object().expect("agent object") {
            if !matches!(key.as_str(), "rules" | "description" | "role") {
                fields.insert(key.clone(), value.clone());
            }
        }
        fields.insert("adapter".into(), Value::String(final_adapter.into()));
        fields.insert(
            "role".into(),
            Value::String(
                bundle
                    .render_role(agent)
                    .map_err(Error::invalid_arguments)?,
            ),
        );
        fields.remove("model");
        let selected = model
            .or_else(|| agent.model.as_deref().filter(|_| bound == final_adapter))
            .or_else(|| {
                defaults
                    .and_then(|d| d.model.as_deref())
                    .filter(|_| org_adapter == final_adapter)
            });
        if let Some(model) = selected {
            fields.insert("model".into(), Value::String(model.into()));
        }
        let mut input: CreateAgentInput = serde_json::from_value(Value::Object(fields))
            .map_err(|e| Error::invalid_arguments(format!("{}: {e}", agent.name)))?;
        input.approval_policy = Some(
            input
                .approval_policy
                .unwrap_or(config.defaults.agent.approval_policy),
        );
        input.max_turns = Some(input.max_turns.unwrap_or(config.defaults.agent.max_turns));
        input.timeout_ms = Some(input.timeout_ms.unwrap_or(config.defaults.agent.timeout_ms));
        input.stall_timeout_ms = Some(
            input
                .stall_timeout_ms
                .unwrap_or(config.defaults.agent.stall_timeout_ms),
        );
        validate_input(&input)?;
        if input.adapter == "shell" {
            input.model = None;
            input.effort = None;
        }
        let record = preview_record(&input);
        validate_record(&record)?;
        inputs.push(input);
    }
    Ok(inputs)
}

pub fn validate_input(input: &CreateAgentInput) -> Result<()> {
    if !orch_domain::SUPPORTED_ADAPTERS
        .iter()
        .any(|a| a.as_str() == input.adapter)
    {
        return Err(Error::invalid_arguments(format!(
            "{}: unsupported adapter {}",
            input.name, input.adapter
        )));
    }
    if input.adapter == "shell" && input.command.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err(Error::invalid_arguments(format!(
            "{}: shell requires command",
            input.name
        )));
    }
    if input.model.as_deref().is_some_and(|m| m.trim().is_empty())
        || input.max_turns == Some(0)
        || input.timeout_ms == Some(0)
        || input.stall_timeout_ms == Some(0)
    {
        return Err(Error::invalid_arguments(format!(
            "{}: model must be nonblank and runtime limits positive",
            input.name
        )));
    }
    Ok(())
}

fn preview_record(input: &CreateAgentInput) -> Agent {
    Agent {
        id: "agt_00000000000000000000000000000000".into(),
        name: input.name.clone(),
        adapter: input.adapter.clone(),
        role: input.role.clone(),
        config: AgentConfig {
            command: input.command.as_deref().map(str::trim).map(str::to_owned),
            model: input.model.clone(),
            effort: input.effort,
            approval_policy: input.approval_policy,
            max_turns: input.max_turns,
            timeout_ms: input.timeout_ms,
            stall_timeout_ms: input.stall_timeout_ms,
            env: input.env.clone(),
            system_prompt: input.system_prompt.clone(),
            workspace_mode: input.workspace_mode,
            skills: input.skills.clone(),
        },
        status: AgentStatus::Idle,
        current_task: None,
        autonomous: None,
        stats: AgentStats::default(),
        last_error: None,
        capabilities: vec![],
        remote: None,
    }
}

pub async fn preflight(engine: &Engine, inputs: &[CreateAgentInput]) -> Result<()> {
    let existing = engine.agent_service.list().await?;
    for input in inputs {
        validate_input(input)?;
        if existing.iter().any(|a| a.name == input.name) {
            return Err(Error::invalid_arguments(format!(
                "Agent {:?} already exists; no agents created",
                input.name
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Deployment {
    pub team: Team,
    #[serde(rename = "agentIds")]
    pub agent_ids: Vec<String>,
    #[serde(rename = "goalId", skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
}

/// Preflight completes before any service mutation. A later I/O failure
/// names exactly the surviving entities; it never claims a rollback.
pub async fn deploy_bundle(
    engine: &Engine,
    bundle: &Bundle,
    adapter: Option<&str>,
    model: Option<&str>,
    goal: Option<&str>,
    validate_record: fn(&Agent) -> Result<()>,
) -> Result<Deployment> {
    let inputs = resolve_inputs(bundle, &engine.config, adapter, model, validate_record)?;
    preflight(engine, &inputs).await?;
    if goal.is_some_and(|s| s.trim().is_empty()) {
        return Err(Error::invalid_arguments(
            "Goal title must not be blank; no agents created",
        ));
    }
    let allocated = Arc::new(DeploymentIds {
        inner: engine.ids.clone(),
        allocated: Mutex::new(Vec::new()),
    });
    // Ordinary services retain ownership of creation and all invariants. This
    // per-attempt generator records IDs even if save fails after publication.
    let agent_service = crate::AgentService::new(
        engine.agents.clone(),
        engine.state.clone(),
        engine.events.clone(),
        engine.config.clone(),
        allocated.clone(),
    );
    let team_service = crate::TeamService::new(
        engine.teams.clone(),
        engine.agents.clone(),
        engine.tasks.clone(),
        engine.events.clone(),
        engine.clock.clone(),
        allocated.clone(),
    );
    let goal_service = crate::GoalService::new(
        engine.goals.clone(),
        engine.events.clone(),
        engine.clock.clone(),
        allocated.clone(),
        Some(engine.agent_service.clone()),
        Some(engine.task_service.clone()),
        Some(engine.context.clone()),
    );
    let mut ids = Vec::new();
    for input in inputs {
        let name = input.name.clone();
        match agent_service.create(input).await {
            Ok(agent) => ids.push(agent.id),
            Err(e) => {
                return Err(
                    deployment_failure(engine, &allocated, &format!("agent:{name}"), e).await,
                );
            }
        }
    }
    let lead = bundle
        .agents
        .iter()
        .position(|a| a.name == bundle.org.lead)
        .expect("validated lead");
    let lead_id = ids[lead].clone();
    let team = team_service
        .create(CreateTeamInput {
            name: bundle.org.name.clone(),
            description: Some(bundle.org.description.clone()),
            lead_agent_id: lead_id.clone(),
            member_agent_ids: Some(ids.iter().filter(|id| **id != lead_id).cloned().collect()),
            config: None,
        })
        .await;
    let team = match team {
        Ok(team) => team,
        Err(e) => return Err(deployment_failure(engine, &allocated, "team", e).await),
    };
    let goal_id = if let Some(title) = goal {
        match goal_service
            .create(CreateGoalInput {
                title: title.into(),
                description: None,
                assignee: Some(lead_id),
            })
            .await
        {
            Ok(goal) => Some(goal.id),
            Err(e) => return Err(deployment_failure(engine, &allocated, "goal", e).await),
        }
    } else {
        None
    };
    Ok(Deployment {
        team,
        agent_ids: ids,
        goal_id,
    })
}
struct DeploymentIds {
    inner: Arc<dyn orch_core::IdGenerator>,
    allocated: Mutex<Vec<(String, String)>>,
}
impl orch_core::IdGenerator for DeploymentIds {
    fn generate(&self, prefix: &str) -> String {
        let id = self.inner.generate(prefix);
        self.allocated
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((prefix.into(), id.clone()));
        id
    }
}
async fn deployment_failure(
    engine: &Engine,
    allocated: &DeploymentIds,
    stage: &str,
    error: Error,
) -> Error {
    let attempts = allocated
        .allocated
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut agents = Vec::new();
    let mut team = None;
    let mut goal = None;
    let mut unverified = Vec::new();
    for (kind, id) in attempts {
        let exists = match kind.as_str() {
            "agt" => engine.agents.get(&id).await.map(|a| a.is_some()),
            "team" => engine.teams.get(&id).await.map(|a| a.is_some()),
            "goal" => engine.goals.get(&id).await.map(|a| a.is_some()),
            _ => continue,
        };
        match exists {
            Ok(true) => match kind.as_str() {
                "agt" => agents.push(id),
                "team" => team = Some(id),
                "goal" => goal = Some(id),
                _ => {}
            },
            Ok(false) => {}
            Err(_) => unverified.push(id),
        }
    }
    Error::other(format!(
        "Template deployment failed at {stage}: {error}. Surviving entities: {}. Inspect the entity files and orch agent list / team list / goal list; repair a failed index if needed and remove survivors explicitly before retrying.",
        serde_json::json!({"agentIds":agents,"teamId":team,"goalId":goal,"unverifiedIds":unverified})
    ))
}
