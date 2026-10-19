//! Agent factory.
//!
//! Turns an Agent Shop template into a [`CreateAgentInput`]: resolves the
//! template's semantic tier to a concrete model string for the chosen adapter,
//! and drops MCP skills for every adapter but `claude`, which is the only one
//! whose CLI handles them natively.

use orch_core::error::{Error, Result};
use orch_domain::{AgentShopTemplate, CreateAgentInput, resolve_model};

/// MCP skills use colon-separated names (e.g. `feature-dev:code-explorer`).
///
/// Re-exported here because the CLI imports it from this module; the canonical
/// implementation is [`orch_core::skills::is_mcp_skill`].
pub fn is_mcp_skill(skill: &str) -> bool {
    orch_core::skills::is_mcp_skill(skill)
}

/// Build the input for `agent add` from a shop template.
///
/// `shell` is refused: a shop template carries a role and a model tier, never
/// an executable command, so a shell agent built from one could never run.
/// An **unknown** adapter is *not* refused — `resolve_model` yields `""` for
/// it, which becomes an absent `model`, and the agent is created with the
/// adapter string as given. That asymmetry is intended and the suite
/// asserts it.
pub fn template_to_agent_input(
    template: &AgentShopTemplate,
    adapter: &str,
) -> Result<CreateAgentInput> {
    if adapter == "shell" {
        return Err(Error::invalid_arguments(
            "Agent Shop templates require an AI adapter; create shell agents with --command",
        ));
    }

    let model = resolve_model(adapter, template.tier);
    let skills: Vec<String> = template
        .skills
        .iter()
        .filter(|skill| adapter == "claude" || !is_mcp_skill(skill))
        .map(|skill| (*skill).to_owned())
        .collect();

    Ok(CreateAgentInput {
        name: template.name.to_owned(),
        adapter: adapter.to_owned(),
        role: Some(template.role.to_owned()),
        model: (!model.is_empty()).then(|| model.to_owned()),
        skills: Some(skills),
        approval_policy: Some(template.approval_policy),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{ApprovalPolicy, ModelTier};

    /// The base template, which is deliberately not one of the shipped catalogue entries.
    const BASE: AgentShopTemplate = AgentShopTemplate {
        key: "test",
        name: "Test Agent",
        description: "A test agent",
        tier: ModelTier::Balanced,
        approval_policy: ApprovalPolicy::Auto,
        skills: &[
            "review",
            "careful",
            "feature-dev:feature-dev",
            "testing-suite:generate-tests",
        ],
        role: "Test role",
    };

    fn skills(input: &CreateAgentInput) -> Vec<String> {
        input.skills.clone().unwrap_or_default()
    }

    #[test]
    fn mcp_skills_are_the_colon_separated_ones() {
        assert!(is_mcp_skill("feature-dev:feature-dev"));
        assert!(is_mcp_skill("testing-suite:generate-tests"));
        assert!(!is_mcp_skill("review"));
        assert!(!is_mcp_skill("careful"));
        assert!(!is_mcp_skill("qa"));
    }

    #[test]
    fn claude_gets_its_balanced_model_and_every_field_from_the_template() {
        let input = template_to_agent_input(&BASE, "claude").unwrap();
        assert_eq!(input.adapter, "claude");
        assert_eq!(input.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(input.name, "Test Agent");
        assert_eq!(input.role.as_deref(), Some("Test role"));
        assert_eq!(input.approval_policy, Some(ApprovalPolicy::Auto));
    }

    #[test]
    fn codex_gets_its_own_balanced_model() {
        let input = template_to_agent_input(&BASE, "codex").unwrap();
        assert_eq!(input.adapter, "codex");
        assert_eq!(input.model.as_deref(), Some("gpt-5.3-codex"));
    }

    #[test]
    fn pi_gets_its_own_balanced_model() {
        let input = template_to_agent_input(&BASE, "pi").unwrap();
        assert_eq!(input.adapter, "pi");
        assert_eq!(input.model.as_deref(), Some("openai-codex/gpt-5.5"));
    }

    #[test]
    fn shell_is_refused_because_a_template_carries_no_command() {
        let err = template_to_agent_input(&BASE, "shell").expect_err("refused");
        assert_eq!(
            err.to_string(),
            "Agent Shop templates require an AI adapter; create shell agents with --command"
        );
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn claude_keeps_every_skill_including_the_mcp_ones() {
        let input = template_to_agent_input(&BASE, "claude").unwrap();
        assert_eq!(
            skills(&input),
            [
                "review",
                "careful",
                "feature-dev:feature-dev",
                "testing-suite:generate-tests"
            ]
        );
    }

    #[test]
    fn every_other_adapter_drops_the_mcp_skills() {
        for adapter in ["pi", "opencode", "codex", "cursor", "grok", "antigravity"] {
            let input = template_to_agent_input(&BASE, adapter).unwrap();
            assert_eq!(skills(&input), ["review", "careful"], "adapter={adapter}");
        }
    }

    #[test]
    fn the_capable_tier_resolves_to_the_capable_model() {
        let capable = AgentShopTemplate {
            tier: ModelTier::Capable,
            ..BASE
        };
        let input = template_to_agent_input(&capable, "claude").unwrap();
        assert_eq!(input.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn an_unknown_adapter_is_accepted_with_no_model() {
        let input = template_to_agent_input(&BASE, "unknown-adapter").unwrap();
        assert_eq!(input.adapter, "unknown-adapter");
        assert_eq!(input.model, None);
        // And it is treated as non-claude, so MCP skills are dropped.
        assert_eq!(skills(&input), ["review", "careful"]);
    }
}
