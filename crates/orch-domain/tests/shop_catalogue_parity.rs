//! V28 — the agent-shop, org-shop and default-agent catalogues must match the
//! TypeScript source exactly, role prompts included.
//!
//! The fixtures under `fixtures/shop/` are generated from
//! `parity/orch-ts/src/domain/{agent-shop,org-shop,default-agents}.ts` by
//! `fixtures/shop/generate.mjs` (see `fixtures/shop/README.md`). Comparing parsed values rather than bytes,
//! because JSON object key order is not contract (PARITY.md §1.4).

use orch_domain::{AGENT_SHOP_TEMPLATES, ORG_TEMPLATES, default_agents};
use serde_json::Value;

const AGENT_SHOP_FIXTURE: &str = include_str!("../../../fixtures/shop/agent-shop.json");
const ORG_SHOP_FIXTURE: &str = include_str!("../../../fixtures/shop/org-shop.json");
const DEFAULT_AGENTS_FIXTURE: &str = include_str!("../../../fixtures/shop/default-agents.json");

fn parse(fixture: &str) -> Value {
    serde_json::from_str(fixture).expect("fixture is valid JSON")
}

#[test]
fn agent_shop_catalogue_matches_the_typescript_source() {
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
                "{key}.role diverges at byte {at}:\n  rust: {:?}\n  ts:   {:?}",
                &actual_role[at..actual_role.len().min(at + 60)],
                &expected_role[at..expected_role.len().min(at + 60)],
            );
        }
    }
}

#[test]
fn org_shop_catalogue_matches_the_typescript_source() {
    assert_eq!(
        serde_json::to_value(&ORG_TEMPLATES).expect("catalogue serializes"),
        parse(ORG_SHOP_FIXTURE)
    );
}

#[test]
fn default_agents_match_the_typescript_source_for_every_adapter_shape() {
    let expected = parse(DEFAULT_AGENTS_FIXTURE);
    // One representative per behaviour: claude (MCP skill + model), codex (no
    // MCP skills), antigravity (empty balanced model ⇒ no `model` key at all),
    // shell (no agent).
    for adapter in ["claude", "codex", "antigravity", "shell"] {
        let actual = serde_json::to_value(default_agents(adapter)).expect("agents serialize");
        assert_eq!(actual, expected[adapter], "default_agents({adapter})");
    }
}

#[test]
fn the_agent_creator_role_is_byte_identical_to_the_typescript_source() {
    let expected = parse(DEFAULT_AGENTS_FIXTURE);
    let expected_role = expected["claude"][0]["role"].as_str().expect("role");
    assert_eq!(orch_domain::AGENT_CREATOR_ROLE, expected_role);
}
