//! Agent factory — shop template → `CreateAgentInput`, ported from
//! `src/application/agent-factory.ts`.
//!
//! Two rules, both observable in the created agent: the model comes from the
//! template's semantic **tier** resolved against the chosen adapter, and MCP
//! skills (the colon-separated ones) are **dropped for every adapter except
//! claude**, because they only work through the Claude CLI.

use orch_core::error::{Error, Result};
use orch_core::skills::is_mcp_skill;
use orch_domain::{AgentShopTemplate, CreateAgentInput, resolve_model};

/// Convert a shop template into the input `AgentService::create` takes.
///
/// `shell` is rejected: a shop template has no command, and a shell agent
/// without one cannot run.
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
        command: None,
        // An empty resolution (the `balanced` tier of some adapters) means
        // "the adapter's own default", not an empty model string.
        model: (!model.is_empty()).then(|| model.to_owned()),
        effort: None,
        approval_policy: Some(template.approval_policy),
        max_turns: None,
        timeout_ms: None,
        stall_timeout_ms: None,
        env: None,
        system_prompt: None,
        workspace_mode: None,
        skills: Some(skills),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{AGENT_SHOP_TEMPLATES, ApprovalPolicy, ModelTier, get_shop_template_by_key};

    fn template(key: &str) -> &'static AgentShopTemplate {
        get_shop_template_by_key(key).unwrap_or_else(|| panic!("no template {key}"))
    }

    #[test]
    fn the_model_comes_from_the_tier_resolved_against_the_adapter() {
        let backend = template("backend-dev");
        assert_eq!(backend.tier, ModelTier::Balanced);

        let claude = template_to_agent_input(backend, "claude").expect("claude");
        assert_eq!(claude.model.as_deref(), Some("claude-sonnet-4-6"));

        let codex = template_to_agent_input(backend, "codex").expect("codex");
        assert_eq!(codex.model.as_deref(), Some("gpt-5.3-codex"));
    }

    #[test]
    fn an_empty_tier_resolution_means_the_adapter_default() {
        // opencode's `balanced` entry is the empty string.
        let input = template_to_agent_input(template("backend-dev"), "opencode").expect("opencode");
        assert_eq!(input.model, None);
    }

    #[test]
    fn mcp_skills_survive_only_for_claude() {
        let qa = template("qa-engineer");
        assert!(
            qa.skills.iter().any(|skill| is_mcp_skill(skill)),
            "the fixture needs an MCP skill to be meaningful"
        );

        let claude = template_to_agent_input(qa, "claude").expect("claude");
        assert_eq!(
            claude.skills.as_deref().expect("skills").len(),
            qa.skills.len()
        );

        let codex = template_to_agent_input(qa, "codex").expect("codex");
        let kept = codex.skills.expect("skills");
        assert!(kept.iter().all(|skill| !is_mcp_skill(skill)));
        assert!(kept.len() < qa.skills.len());
    }

    #[test]
    fn the_name_role_and_approval_policy_come_straight_from_the_template() {
        let reviewer = template("code-reviewer");
        let input = template_to_agent_input(reviewer, "claude").expect("claude");
        assert_eq!(input.name, reviewer.name);
        assert_eq!(input.role.as_deref(), Some(reviewer.role));
        assert_eq!(input.approval_policy, Some(reviewer.approval_policy));
        assert_eq!(input.command, None);
    }

    #[test]
    fn shell_is_rejected_with_exit_two() {
        let err = template_to_agent_input(template("backend-dev"), "shell")
            .expect_err("shell has no command");
        assert_eq!(
            err.to_string(),
            "Agent Shop templates require an AI adapter; create shell agents with --command"
        );
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn every_shipped_template_converts_for_every_ai_adapter() {
        for template in &AGENT_SHOP_TEMPLATES {
            for adapter in [
                "claude",
                "codex",
                "cursor",
                "opencode",
                "pi",
                "grok",
                "antigravity",
            ] {
                let input = template_to_agent_input(template, adapter)
                    .unwrap_or_else(|err| panic!("{}/{adapter}: {err}", template.key));
                assert_eq!(input.adapter, adapter);
                assert!(!input.name.is_empty());
                assert!(matches!(
                    input.approval_policy,
                    Some(ApprovalPolicy::Auto | ApprovalPolicy::Suggest | ApprovalPolicy::Manual)
                ));
            }
        }
    }
}
