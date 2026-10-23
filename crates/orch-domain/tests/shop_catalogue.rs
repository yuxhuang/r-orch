//! Regression test: the agent-shop, org-shop and default-agent catalogues must
//! match the golden snapshots under `fixtures/shop/` exactly, role prompts
//! included (see `fixtures/shop/README.md`). Parsed values are compared rather
//! than bytes, because JSON object key order is not significant.

use orch_domain::{AGENT_SHOP_TEMPLATES, ORG_TEMPLATES, default_agents};
use serde_json::Value;

const AGENT_SHOP_FIXTURE: &str = include_str!("../../../fixtures/shop/agent-shop.json");
const ORG_SHOP_FIXTURE: &str = include_str!("../../../fixtures/shop/org-shop.json");
const DEFAULT_AGENTS_FIXTURE: &str = include_str!("../../../fixtures/shop/default-agents.json");

fn parse(fixture: &str) -> Value {
    serde_json::from_str(fixture).expect("fixture is valid JSON")
}

#[test]
fn agent_shop_catalogue_matches_the_golden_fixture() {
    let expected = parse(AGENT_SHOP_FIXTURE);
    let actual = serde_json::to_value(&AGENT_SHOP_TEMPLATES).expect("catalogue serializes");

    let expected = expected.as_array().expect("array");
    let actual = actual.as_array().expect("array");
    assert_eq!(actual.len(), expected.len(), "template count");

    // Compare per template so a mismatch names the offender rather than
    // dumping 26 KB of diff.
    for (actual, expected) in actual.iter().zip(expected) {
        let key = expected["key"].as_str().expect("key");
        for field in [
            "key",
            "name",
            "description",
            "tier",
            "approval_policy",
            "skills",
        ] {
            assert_eq!(actual[field], expected[field], "{key}.{field}");
        }
        // Role prompts are injected into system prompts, so compare them
        // explicitly and report the first divergence rather than the whole text.
        let (actual_role, expected_role) = (
            actual["role"].as_str().expect("role"),
            expected["role"].as_str().expect("role"),
        );
        assert_eq!(
            actual_role.len(),
            expected_role.len(),
            "{key}.role length ({} vs {})",
            actual_role.len(),
            expected_role.len()
        );
        if actual_role != expected_role {
            let at = actual_role
                .char_indices()
                .zip(expected_role.chars())
                .find(|((_, a), b)| a != b)
                .map(|((i, _), _)| i)
                .unwrap_or(0);
            panic!(
                "{key}.role diverges at byte {at}:\n  actual:   {:?}\n  expected: {:?}",
                &actual_role[at..actual_role.len().min(at + 60)],
                &expected_role[at..expected_role.len().min(at + 60)],
            );
        }
    }
}

#[test]
fn org_shop_catalogue_matches_the_golden_fixture() {
    assert_eq!(
        serde_json::to_value(&ORG_TEMPLATES).expect("catalogue serializes"),
        parse(ORG_SHOP_FIXTURE)
    );
}

#[test]
fn default_agents_match_the_golden_fixture_for_every_adapter_shape() {
    let expected = parse(DEFAULT_AGENTS_FIXTURE);
    // One representative per behaviour: claude (MCP skill + model), codex (no
    // MCP skills), antigravity (empty balanced model ⇒ no `model` key at all),
    // shell (no agent).
    //
    // `fixtures/shop/default-agents.json` is a frozen golden that predates the
    // token-eco model refresh (D6): its `config.model` values are the retired
    // choices. Everything else — id, name, role bytes, skills, limits — is
    // still compared to the golden unchanged; only `config.model` is replaced
    // by the D6 balanced default, which is spelled out here from the design
    // table rather than read back from `MODEL_TIER_MAP`.
    let d6_balanced = [
        ("claude", Some("claude-sonnet-5-5")),
        ("codex", Some("gpt-6.1-sol")),
        ("antigravity", None),
        ("shell", None),
    ];
    for (adapter, model) in d6_balanced {
        let mut golden = expected[adapter].clone();
        for agent in golden.as_array_mut().expect("array") {
            let config = agent["config"].as_object_mut().expect("config");
            match model {
                Some(model) => {
                    config.insert("model".into(), Value::String(model.into()));
                }
                None => {
                    config.remove("model");
                }
            }
        }
        let actual = serde_json::to_value(default_agents(adapter)).expect("agents serialize");
        assert_eq!(actual, golden, "default_agents({adapter})");
    }
}

#[test]
fn the_default_agents_golden_differs_from_current_output_only_in_the_model_choice() {
    // Guards the substitution above: if anything besides `config.model`
    // drifts, the golden comparison must not silently absorb it.
    let expected = parse(DEFAULT_AGENTS_FIXTURE);
    for adapter in ["claude", "codex", "antigravity"] {
        let mut actual = serde_json::to_value(default_agents(adapter)).expect("agents serialize");
        let mut golden = expected[adapter].clone();
        for value in [&mut actual, &mut golden] {
            for agent in value.as_array_mut().expect("array") {
                agent["config"]
                    .as_object_mut()
                    .expect("config")
                    .remove("model");
            }
        }
        assert_eq!(actual, golden, "default_agents({adapter}) minus model");
    }
}

#[test]
fn the_agent_creator_role_is_byte_identical_to_the_golden_fixture() {
    let expected = parse(DEFAULT_AGENTS_FIXTURE);
    let expected_role = expected["claude"][0]["role"].as_str().expect("role");
    assert_eq!(orch_domain::AGENT_CREATOR_ROLE, expected_role);
}

// ── The catalogue keys ─────────────────────────────────────────────────────

#[test]
fn the_fifteen_agent_shop_keys_are_exactly_these_in_this_order() {
    let keys: Vec<&str> = AGENT_SHOP_TEMPLATES.iter().map(|t| t.key).collect();
    assert_eq!(
        keys,
        [
            "backend-dev",
            "frontend-dev",
            "qa-engineer",
            "code-reviewer",
            "architect",
            "devops-engineer",
            "bug-hunter",
            "tech-writer",
            "marketer",
            "content-creator",
            "growth-hacker",
            "security-auditor",
            "performance-engineer",
            "data-engineer",
            "fullstack-dev",
        ]
    );
}

#[test]
fn exactly_three_templates_are_capable_plus_suggest_and_the_rest_balanced_plus_auto() {
    use orch_domain::{ApprovalPolicy, ModelTier};

    let capable: Vec<&str> = AGENT_SHOP_TEMPLATES
        .iter()
        .filter(|t| t.tier == ModelTier::Capable)
        .map(|t| t.key)
        .collect();
    assert_eq!(capable, ["code-reviewer", "architect", "security-auditor"]);

    for template in &AGENT_SHOP_TEMPLATES {
        let (expected_tier, expected_policy) = if capable.contains(&template.key) {
            (ModelTier::Capable, ApprovalPolicy::Suggest)
        } else {
            (ModelTier::Balanced, ApprovalPolicy::Auto)
        };
        assert_eq!(template.tier, expected_tier, "{}", template.key);
        assert_eq!(
            template.approval_policy, expected_policy,
            "{}",
            template.key
        );
    }
    // No template is `fast`, and none is `manual`.
    assert!(
        AGENT_SHOP_TEMPLATES
            .iter()
            .all(|t| t.tier != ModelTier::Fast)
    );
    assert!(
        AGENT_SHOP_TEMPLATES
            .iter()
            .all(|t| t.approval_policy != ApprovalPolicy::Manual)
    );
}

#[test]
fn the_ten_org_shop_keys_are_exactly_these_and_all_lead_index_zero() {
    let keys: Vec<&str> = ORG_TEMPLATES.iter().map(|t| t.key).collect();
    assert_eq!(
        keys,
        [
            "startup-mvp",
            "pr-review-corp",
            "migration-squad",
            "security-dept",
            "test-factory",
            "content-agency",
            "data-lab",
            "sales-machine",
            "bugfix-dept",
            "docs-team",
        ]
    );
    for template in &ORG_TEMPLATES {
        assert_eq!(template.lead_index, 0, "{}", template.key);
    }
}

#[test]
fn init_creates_exactly_one_default_agent() {
    let agents = default_agents("claude");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, "agt_creator");
    assert_eq!(agents[0].name, "Agent Creator");
}
