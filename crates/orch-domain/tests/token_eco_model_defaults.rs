//! T1 / F1 — D6-1: fresh model choices per adapter and tier are exactly the
//! D6 table (design-v3 §D6), saved model IDs are never rewritten, and the
//! shop/default-agent factories use those choices.
//!
//! The expectations are hard-coded from the design table, not read from
//! `MODEL_TIER_MAP`.

use orch_domain::{
    Agent, MODEL_TIER_MAP, ModelTier, SUPPORTED_ADAPTERS, default_agents,
    default_model_for_adapter, resolve_model, tier_map_for,
};

/// (adapter, capable, balanced, fast). `""` = delegate to the CLI's own default.
const D6: [(&str, &str, &str, &str); 8] = [
    (
        "claude",
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
    ),
    ("codex", "gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"),
    (
        "opencode",
        "openrouter/anthropic/claude-opus-5.5",
        "",
        "openrouter/google/gemini-3.8-flash",
    ),
    (
        "pi",
        "openai-codex/gpt-6-astra",
        "openai-codex/gpt-6.1-sol",
        "openai-codex/gpt-6-luna",
    ),
    ("grok", "grok-build", "", ""),
    ("antigravity", "gemini-4-argon", "", "gemini-3.8-flash"),
    ("cursor", "auto", "auto", "auto"),
    ("shell", "", "", ""),
];

#[test]
fn every_adapter_resolves_each_tier_to_the_exact_d6_model() {
    for (adapter, capable, balanced, fast) in D6 {
        assert_eq!(
            resolve_model(adapter, ModelTier::Capable),
            capable,
            "{adapter} capable"
        );
        assert_eq!(
            resolve_model(adapter, ModelTier::Balanced),
            balanced,
            "{adapter} balanced"
        );
        assert_eq!(
            resolve_model(adapter, ModelTier::Fast),
            fast,
            "{adapter} fast"
        );
    }
}

#[test]
fn the_default_model_for_each_adapter_is_the_balanced_tier() {
    for (adapter, _, balanced, _) in D6 {
        assert_eq!(default_model_for_adapter(adapter), balanced, "{adapter}");
    }
}

#[test]
fn the_tier_table_covers_exactly_the_supported_adapters_and_the_three_tiers() {
    assert_eq!(MODEL_TIER_MAP.len(), D6.len());
    for (kind, tiers) in MODEL_TIER_MAP {
        assert!(D6.iter().any(|(a, ..)| *a == kind.as_str()), "{kind}");
        assert_eq!(tiers.map(|(t, _)| t), ModelTier::ALL, "{kind}");
    }
    let order: Vec<&str> = SUPPORTED_ADAPTERS.iter().map(|a| a.as_str()).collect();
    let d6_order: Vec<&str> = D6.iter().map(|(a, ..)| *a).collect();
    let mut sorted_a = order.clone();
    let mut sorted_b = d6_order.clone();
    sorted_a.sort();
    sorted_b.sort();
    assert_eq!(sorted_a, sorted_b);
}

#[test]
fn antigravity_capable_is_argon_and_balanced_delegates() {
    assert_eq!(
        resolve_model("antigravity", ModelTier::Capable),
        "gemini-4-argon"
    );
    assert_eq!(resolve_model("antigravity", ModelTier::Balanced), "");
}

#[test]
fn grok_balanced_and_fast_delegate_to_the_cli_default_and_invent_no_model() {
    assert_eq!(resolve_model("grok", ModelTier::Balanced), "");
    assert_eq!(resolve_model("grok", ModelTier::Fast), "");
    assert_eq!(resolve_model("grok", ModelTier::Capable), "grok-build");
}

#[test]
fn an_unknown_adapter_resolves_to_the_empty_string_and_the_cli_decides() {
    for tier in ModelTier::ALL {
        assert_eq!(resolve_model("nonsense", tier), "");
    }
    assert_eq!(default_model_for_adapter(""), "");
}

#[test]
fn no_retired_model_id_remains_in_any_fresh_choice() {
    let retired = [
        "gpt-5.3-codex",
        "gpt-5.4",
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-haiku-4-5",
    ];
    for (kind, tiers) in MODEL_TIER_MAP {
        for (tier, model) in tiers {
            assert!(
                !retired.contains(&model),
                "{kind} {tier} still picks {model}"
            );
        }
    }
}

#[test]
fn tier_map_for_agrees_with_resolve_model() {
    for kind in SUPPORTED_ADAPTERS {
        for (tier, model) in tier_map_for(kind) {
            assert_eq!(resolve_model(kind.as_str(), *tier), *model);
        }
    }
}

#[test]
fn a_fresh_default_agent_gets_the_balanced_model_or_no_model_key_when_delegated() {
    for (adapter, _, balanced, _) in D6 {
        let agents: Vec<Agent> = default_agents(adapter);
        if adapter == "shell" {
            assert!(agents.is_empty());
            continue;
        }
        assert_eq!(agents.len(), 1, "{adapter}");
        let want = (!balanced.is_empty()).then(|| balanced.to_owned());
        assert_eq!(agents[0].config.model, want, "{adapter}");
        assert_eq!(
            agents[0].config.effort, None,
            "{adapter}: no default effort"
        );
        assert_eq!(agents[0].config.prompt_mode, None, "{adapter}: inherits");
        assert_eq!(agents[0].config.role_summary, None, "{adapter}");
    }
}

#[test]
fn a_saved_agent_model_id_is_left_verbatim_by_the_new_defaults() {
    // Saved definitions carry whatever the user had; the tier table is for
    // new selections only and never rewrites them.
    let yaml = "id: a\nname: n\nadapter: claude\nconfig:\n  model: claude-sonnet-4-6\nstatus: idle\nstats:\n  tasks_completed: 0\n  tasks_failed: 0\n  total_runs: 0\n  total_runtime_ms: 0\n";
    let agent: Agent = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(agent.config.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(serde_yaml_ng::to_string(&agent).unwrap(), yaml);
}
