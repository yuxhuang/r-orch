//! The shop catalogues and the bootstrap agent.
//!
//! Role prompts live as `roles/*.md` files, pulled in with `include_str!`. They
//! are injected into agent system prompts, so their bytes are contract: they
//! must never be reflowed, re-indented or retyped. `tests/shop_catalogue.rs`
//! pins them against the golden snapshots under `fixtures/shop/`.

mod catalogue;

use serde::Serialize;

use crate::agent::{Agent, AgentConfig, AgentStats, AgentStatus, ApprovalPolicy};
use crate::model_tiers::{ModelTier, resolve_model};

pub use catalogue::{AGENT_SHOP_TEMPLATES, ORG_TEMPLATES};

/// A ready-to-use agent template: identity, high-level approach, recommended
/// tier, skills and approval policy. Detailed methodology comes from the
/// library skills injected at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentShopTemplate {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub tier: ModelTier,
    pub approval_policy: ApprovalPolicy,
    pub skills: &'static [&'static str],
    pub role: &'static str,
}

/// Look up a shop template by its key.
pub fn get_shop_template_by_key(key: &str) -> Option<&'static AgentShopTemplate> {
    AGENT_SHOP_TEMPLATES.iter().find(|t| t.key == key)
}

/// One member of an org template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrgAgentEntry {
    /// Key into [`AGENT_SHOP_TEMPLATES`].
    pub shop_key: &'static str,
    /// Display-name override (e.g. "CTO" rather than "Architect").
    pub name: &'static str,
}

/// A pre-built AI company: a team of agents from the agent shop, ready to
/// deploy with `orch org deploy <key>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrgTemplate {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub agents: &'static [OrgAgentEntry],
    /// Index into `agents` — which agent is the team lead.
    pub lead_index: usize,
}

pub fn get_org_template_by_key(key: &str) -> Option<&'static OrgTemplate> {
    ORG_TEMPLATES.iter().find(|t| t.key == key)
}

/// The bootstrap "Agent Creator" role prompt, ~3.5 KB of Markdown written
/// verbatim into `.orchestry/agents/agt_creator.yml` by `init`.
pub const AGENT_CREATOR_ROLE: &str = include_str!("roles/agent-creator.md");

/// The agents `orch init` creates.
///
/// Adapter and model are resolved from the chosen default adapter. Shell
/// agents need an explicit command, so `init` cannot create a useful generic
/// one and the list is empty for `shell`. Note `stall_timeout_ms` is 300000
/// here, **not** the 600000 project default.
pub fn default_agents(adapter: &str) -> Vec<Agent> {
    if adapter == "shell" {
        return Vec::new();
    }

    let model = resolve_model(adapter, ModelTier::Balanced);
    // MCP skills (colon format) only work with the Claude CLI.
    let skills: Vec<String> = if adapter == "claude" {
        vec!["document-skills:skill-creator".into()]
    } else {
        Vec::new()
    };

    vec![Agent {
        // A fixed id, not a nanoid.
        id: "agt_creator".into(),
        name: "Agent Creator".into(),
        adapter: adapter.to_owned(),
        role: Some(AGENT_CREATOR_ROLE.to_owned()),
        config: AgentConfig {
            command: None,
            model: if model.is_empty() {
                None
            } else {
                Some(model.to_owned())
            },
            effort: None,
            approval_policy: Some(ApprovalPolicy::Suggest),
            max_turns: Some(50),
            timeout_ms: Some(3_600_000),
            stall_timeout_ms: Some(300_000),
            env: None,
            system_prompt: None,
            workspace_mode: None,
            skills: Some(skills),
            prompt_mode: None,
            role_summary: None,
        },
        status: AgentStatus::Idle,
        current_task: None,
        autonomous: None,
        stats: AgentStats::default(),
        last_error: None,
        capabilities: Vec::new(),
        remote: None,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── Agent shop catalogue ───────────────────────────────────────────────

    #[test]
    fn has_15_templates() {
        assert_eq!(AGENT_SHOP_TEMPLATES.len(), 15);
    }

    #[test]
    fn every_template_has_required_fields() {
        for t in &AGENT_SHOP_TEMPLATES {
            assert!(!t.key.is_empty());
            assert!(!t.name.is_empty());
            assert!(!t.description.is_empty());
            assert!(!t.role.is_empty());
            assert!(!t.skills.is_empty(), "{}", t.key);
        }
    }

    #[test]
    fn all_keys_are_unique() {
        let keys: HashSet<&str> = AGENT_SHOP_TEMPLATES.iter().map(|t| t.key).collect();
        assert_eq!(keys.len(), AGENT_SHOP_TEMPLATES.len());
    }

    #[test]
    fn all_names_are_unique() {
        let names: HashSet<&str> = AGENT_SHOP_TEMPLATES.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), AGENT_SHOP_TEMPLATES.len());
    }

    #[test]
    fn get_shop_template_by_key_returns_the_right_template() {
        let t = get_shop_template_by_key("backend-dev").expect("backend-dev exists");
        assert_eq!(t.name, "Backend Developer");
    }

    #[test]
    fn get_shop_template_by_key_returns_none_for_an_unknown_key() {
        assert!(get_shop_template_by_key("nonexistent").is_none());
    }

    #[test]
    fn every_role_prompt_contains_workflow_and_rules_sections() {
        for t in &AGENT_SHOP_TEMPLATES {
            assert!(t.role.contains("## WORKFLOW"), "{}", t.key);
            assert!(t.role.contains("## RULES"), "{}", t.key);
        }
    }

    #[test]
    fn capable_tier_is_used_for_the_complex_roles() {
        let keys: Vec<&str> = AGENT_SHOP_TEMPLATES
            .iter()
            .filter(|t| t.tier == ModelTier::Capable)
            .map(|t| t.key)
            .collect();
        assert!(keys.contains(&"code-reviewer"));
        assert!(keys.contains(&"architect"));
        assert!(keys.contains(&"security-auditor"));
    }

    #[test]
    fn no_template_has_more_than_4_skills() {
        // The ANTI-PATTERNS limit from the agent-creator role prompt.
        for t in &AGENT_SHOP_TEMPLATES {
            assert!(
                t.skills.len() <= 4,
                "key={} skills={}",
                t.key,
                t.skills.len()
            );
        }
    }

    #[test]
    fn every_template_has_at_least_one_skill() {
        for t in &AGENT_SHOP_TEMPLATES {
            assert!(!t.skills.is_empty(), "key={}", t.key);
        }
    }

    #[test]
    fn role_prompts_have_no_trailing_newline() {
        // They end in `.`; a stray newline
        // would change every prompt that embeds them.
        for t in &AGENT_SHOP_TEMPLATES {
            assert!(!t.role.ends_with('\n'), "{}", t.key);
        }
        assert!(!AGENT_CREATOR_ROLE.ends_with('\n'));
    }

    // ── Org shop catalogue ─────────────────────────────────────────────────

    #[test]
    fn has_10_org_templates() {
        assert_eq!(ORG_TEMPLATES.len(), 10);
    }

    #[test]
    fn all_org_keys_are_unique() {
        let keys: HashSet<&str> = ORG_TEMPLATES.iter().map(|t| t.key).collect();
        assert_eq!(keys.len(), ORG_TEMPLATES.len());
    }

    #[test]
    fn all_org_templates_have_at_least_2_agents() {
        for t in &ORG_TEMPLATES {
            assert!(t.agents.len() >= 2, "{}", t.key);
        }
    }

    #[test]
    fn lead_index_is_valid_for_all_org_templates() {
        for t in &ORG_TEMPLATES {
            assert!(t.lead_index < t.agents.len(), "{}", t.key);
        }
    }

    #[test]
    fn all_org_shop_keys_reference_valid_shop_templates() {
        for t in &ORG_TEMPLATES {
            for entry in t.agents {
                assert!(
                    get_shop_template_by_key(entry.shop_key).is_some(),
                    "{} → {}",
                    t.key,
                    entry.shop_key
                );
            }
        }
    }

    #[test]
    fn all_org_agents_have_non_empty_names() {
        for t in &ORG_TEMPLATES {
            for entry in t.agents {
                assert!(!entry.name.trim().is_empty(), "{}", t.key);
            }
        }
    }

    #[test]
    fn all_org_templates_have_required_fields() {
        for t in &ORG_TEMPLATES {
            assert!(!t.key.is_empty());
            assert!(!t.name.is_empty());
            assert!(!t.description.is_empty());
            assert!(!t.agents.is_empty());
        }
    }

    #[test]
    fn get_org_template_by_key_finds_startup_mvp() {
        let t = get_org_template_by_key("startup-mvp").expect("startup-mvp exists");
        assert_eq!(t.name, "Startup MVP");
    }

    #[test]
    fn get_org_template_by_key_returns_none_for_an_unknown_key() {
        assert!(get_org_template_by_key("nonexistent").is_none());
    }

    #[test]
    fn startup_mvp_has_6_agents_with_architect_as_lead() {
        let t = get_org_template_by_key("startup-mvp").expect("startup-mvp exists");
        assert_eq!(t.agents.len(), 6);
        assert_eq!(t.agents[t.lead_index].shop_key, "architect");
        assert_eq!(t.agents[t.lead_index].name, "CTO");
    }

    // ── Default agents ─────────────────────────────────────────────────────

    #[test]
    fn claude_default_agent_matches_the_init_contract() {
        let agents = default_agents("claude");
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.id, "agt_creator");
        assert_eq!(agent.name, "Agent Creator");
        assert_eq!(agent.adapter, "claude");
        assert_eq!(agent.role.as_deref(), Some(AGENT_CREATOR_ROLE));
        assert_eq!(agent.config.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Suggest));
        assert_eq!(agent.config.max_turns, Some(50));
        assert_eq!(agent.config.timeout_ms, Some(3_600_000));
        // Deliberately 300000, not the 600000 project default.
        assert_eq!(agent.config.stall_timeout_ms, Some(300_000));
        assert_eq!(
            agent.config.skills.as_deref(),
            Some(["document-skills:skill-creator".to_owned()].as_slice())
        );
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.stats, AgentStats::default());
    }

    #[test]
    fn shell_gets_no_default_agent() {
        assert!(default_agents("shell").is_empty());
    }

    #[test]
    fn non_claude_adapters_get_no_mcp_skills() {
        let agents = default_agents("codex");
        assert_eq!(agents[0].config.skills.as_deref(), Some([].as_slice()));
        assert_eq!(agents[0].config.model.as_deref(), Some("gpt-6.1-sol"));
    }

    #[test]
    fn an_adapter_with_an_empty_balanced_model_omits_the_model_field() {
        // An empty model string is omitted rather than serialized.
        let agents = default_agents("antigravity");
        assert_eq!(agents[0].config.model, None);
        let value = serde_json::to_value(&agents[0]).unwrap();
        assert!(value["config"].get("model").is_none());
    }

    #[test]
    fn an_unknown_adapter_still_produces_an_agent_without_a_model() {
        let agents = default_agents("made-up");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].adapter, "made-up");
        assert_eq!(agents[0].config.model, None);
    }

    #[test]
    fn agent_creator_role_is_the_expected_size_and_shape() {
        // A byte-length check is the cheapest guard against an accidental
        // re-wrap of the file; the full text is the fixture itself.
        assert_eq!(AGENT_CREATOR_ROLE.len(), 3552);
        assert!(AGENT_CREATOR_ROLE.starts_with("Agent architect — designs and creates AI agents"));
        assert!(AGENT_CREATOR_ROLE.contains("## CREATION PROCESS"));
        assert!(AGENT_CREATOR_ROLE.contains("## SKILL TYPES"));
        assert!(AGENT_CREATOR_ROLE.contains("## ANTI-PATTERNS"));
        assert!(
            AGENT_CREATOR_ROLE.ends_with("`orch context set agent-<name> \"<capabilities>\"`.")
        );
    }
}
