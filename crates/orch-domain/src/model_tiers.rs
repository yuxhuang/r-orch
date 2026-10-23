//! Model tier system — the single source of truth for adapter → tier → model
//! resolution.
//!
//! Agent Shop templates reference semantic tiers rather than hard-coded model
//! strings; the concrete model is resolved at instantiation time from the
//! user's chosen adapter. This table is contract data: the resolved model names reach adapters
//! unchanged.

use serde::{Deserialize, Serialize};

/// The supported adapter kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Claude,
    Opencode,
    Codex,
    Cursor,
    Pi,
    Grok,
    Antigravity,
    Shell,
}

impl AdapterKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AdapterKind::Claude => "claude",
            AdapterKind::Opencode => "opencode",
            AdapterKind::Codex => "codex",
            AdapterKind::Cursor => "cursor",
            AdapterKind::Pi => "pi",
            AdapterKind::Grok => "grok",
            AdapterKind::Antigravity => "antigravity",
            AdapterKind::Shell => "shell",
        }
    }
}

impl std::fmt::Display for AdapterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AdapterKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SUPPORTED_ADAPTERS
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// All supported adapter names in display order.
pub const SUPPORTED_ADAPTERS: [AdapterKind; 8] = [
    AdapterKind::Claude,
    AdapterKind::Opencode,
    AdapterKind::Codex,
    AdapterKind::Cursor,
    AdapterKind::Pi,
    AdapterKind::Grok,
    AdapterKind::Antigravity,
    AdapterKind::Shell,
];

/// Semantic capability tiers — adapter-agnostic.
///
/// - `capable` — most powerful / highest quality
/// - `balanced` — good quality and speed
/// - `fast` — cheapest / fastest
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Capable,
    Balanced,
    Fast,
}

impl ModelTier {
    pub const ALL: [ModelTier; 3] = [ModelTier::Capable, ModelTier::Balanced, ModelTier::Fast];

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelTier::Capable => "capable",
            ModelTier::Balanced => "balanced",
            ModelTier::Fast => "fast",
        }
    }
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ModelTier {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ModelTier::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// Tier → model mapping per adapter.
///
/// New selections only; never resolve or rewrite a saved model through this table.
/// Evidence checked 2026-10-22 (token eco design D6 / provider CLI catalogs).
///
/// Conventions:
/// - `shell`: `""` for all tiers (model irrelevant)
/// - `opencode`: `""` for balanced (delegate to opencode's own config)
/// - `cursor`: `"auto"` for all tiers (Cursor handles selection)
/// - `antigravity`: `""` for balanced (delegate to Antigravity's default)
pub const MODEL_TIER_MAP: [(AdapterKind, [(ModelTier, &str); 3]); 8] = [
    (
        AdapterKind::Claude,
        [
            (ModelTier::Capable, "claude-opus-5-5"),
            (ModelTier::Balanced, "claude-sonnet-5-5"),
            (ModelTier::Fast, "claude-haiku-5-5"),
        ],
    ),
    (
        AdapterKind::Opencode,
        [
            (ModelTier::Capable, "openrouter/anthropic/claude-opus-5.5"),
            (ModelTier::Balanced, ""),
            (ModelTier::Fast, "openrouter/google/gemini-3.8-flash"),
        ],
    ),
    (
        AdapterKind::Codex,
        [
            (ModelTier::Capable, "gpt-6-astra"),
            (ModelTier::Balanced, "gpt-6.1-sol"),
            (ModelTier::Fast, "gpt-6-luna"),
        ],
    ),
    (
        AdapterKind::Cursor,
        [
            (ModelTier::Capable, "auto"),
            (ModelTier::Balanced, "auto"),
            (ModelTier::Fast, "auto"),
        ],
    ),
    (
        AdapterKind::Pi,
        [
            (ModelTier::Capable, "openai-codex/gpt-6-astra"),
            (ModelTier::Balanced, "openai-codex/gpt-6.1-sol"),
            (ModelTier::Fast, "openai-codex/gpt-6-luna"),
        ],
    ),
    (
        AdapterKind::Grok,
        [
            (ModelTier::Capable, "grok-build"),
            (ModelTier::Balanced, ""),
            (ModelTier::Fast, ""),
        ],
    ),
    (
        AdapterKind::Antigravity,
        [
            (ModelTier::Capable, "gemini-4-argon"),
            (ModelTier::Balanced, ""),
            (ModelTier::Fast, "gemini-3.8-flash"),
        ],
    ),
    (
        AdapterKind::Shell,
        [
            (ModelTier::Capable, ""),
            (ModelTier::Balanced, ""),
            (ModelTier::Fast, ""),
        ],
    ),
];

/// `MODEL_TIER_MAP[adapter]` — `None` for an unknown adapter.
pub fn tier_map_for(adapter: AdapterKind) -> &'static [(ModelTier, &'static str); 3] {
    MODEL_TIER_MAP
        .iter()
        .find(|(kind, _)| *kind == adapter)
        .map(|(_, tiers)| tiers)
        .expect("MODEL_TIER_MAP covers every AdapterKind")
}

/// Resolve a concrete model string from adapter + tier.
/// Returns `""` for an unknown adapter — let the adapter decide.
pub fn resolve_model(adapter: &str, tier: ModelTier) -> &'static str {
    let Ok(kind) = adapter.parse::<AdapterKind>() else {
        return "";
    };
    tier_map_for(kind)
        .iter()
        .find(|(t, _)| *t == tier)
        .map(|(_, model)| *model)
        .expect("every tier is present")
}

/// The default (balanced) model for an adapter.
pub fn default_model_for_adapter(adapter: &str) -> &'static str {
    resolve_model(adapter, ModelTier::Balanced)
}

/// Whether a string names a supported adapter.
pub fn is_adapter_kind(value: &str) -> bool {
    value.parse::<AdapterKind>().is_ok()
}

/// Whether a string names a valid model tier.
pub fn is_model_tier(value: &str) -> bool {
    value.parse::<ModelTier>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_choices_match_the_dated_catalog_for_all_tiers() {
        for (kind, expected) in [
            (
                "claude",
                ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
            ),
            ("codex", ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"]),
            (
                "opencode",
                [
                    "openrouter/anthropic/claude-opus-5.5",
                    "",
                    "openrouter/google/gemini-3.8-flash",
                ],
            ),
            (
                "pi",
                [
                    "openai-codex/gpt-6-astra",
                    "openai-codex/gpt-6.1-sol",
                    "openai-codex/gpt-6-luna",
                ],
            ),
            ("grok", ["grok-build", "", ""]),
            ("antigravity", ["gemini-4-argon", "", "gemini-3.8-flash"]),
            ("cursor", ["auto", "auto", "auto"]),
            ("shell", ["", "", ""]),
        ] {
            assert_eq!(
                ModelTier::ALL.map(|tier| resolve_model(kind, tier)),
                expected,
                "{kind}"
            );
        }
    }

    #[test]
    fn claude_tiers() {
        assert_eq!(
            resolve_model("claude", ModelTier::Capable),
            "claude-opus-5-5"
        );
        assert_eq!(
            resolve_model("claude", ModelTier::Balanced),
            "claude-sonnet-5-5"
        );
        assert_eq!(resolve_model("claude", ModelTier::Fast), "claude-haiku-5-5");
    }

    #[test]
    fn opencode_balanced_delegates_and_capable_is_concrete() {
        assert_eq!(resolve_model("opencode", ModelTier::Balanced), "");
        assert!(!resolve_model("opencode", ModelTier::Capable).is_empty());
    }

    #[test]
    fn codex_tiers() {
        assert_eq!(resolve_model("codex", ModelTier::Capable), "gpt-6-astra");
        assert_eq!(resolve_model("codex", ModelTier::Balanced), "gpt-6.1-sol");
    }

    #[test]
    fn pi_grok_and_antigravity_balanced() {
        assert_eq!(
            resolve_model("pi", ModelTier::Balanced),
            "openai-codex/gpt-6.1-sol"
        );
        assert_eq!(resolve_model("grok", ModelTier::Balanced), "");
        assert_eq!(resolve_model("antigravity", ModelTier::Balanced), "");
    }

    #[test]
    fn shell_returns_empty_for_all_tiers() {
        for tier in ModelTier::ALL {
            assert_eq!(resolve_model("shell", tier), "");
        }
    }

    #[test]
    fn cursor_returns_auto_for_all_tiers() {
        for tier in ModelTier::ALL {
            assert_eq!(resolve_model("cursor", tier), "auto");
        }
    }

    #[test]
    fn unknown_adapter_returns_empty() {
        assert_eq!(resolve_model("unknown-adapter", ModelTier::Balanced), "");
        assert_eq!(resolve_model("unknown-adapter", ModelTier::Capable), "");
        assert_eq!(resolve_model("", ModelTier::Capable), "");
    }

    #[test]
    fn default_model_is_the_balanced_tier() {
        assert_eq!(default_model_for_adapter("claude"), "claude-sonnet-5-5");
        assert_eq!(default_model_for_adapter("codex"), "gpt-6.1-sol");
        assert_eq!(default_model_for_adapter("pi"), "openai-codex/gpt-6.1-sol");
        assert_eq!(default_model_for_adapter("grok"), "");
        assert_eq!(default_model_for_adapter("antigravity"), "");
        assert_eq!(default_model_for_adapter("shell"), "");
    }

    #[test]
    fn tier_map_is_complete_for_every_supported_adapter() {
        for adapter in SUPPORTED_ADAPTERS {
            let tiers = tier_map_for(adapter);
            let names: Vec<ModelTier> = tiers.iter().map(|(t, _)| *t).collect();
            assert_eq!(names, ModelTier::ALL.to_vec(), "{adapter}");
        }
        assert_eq!(MODEL_TIER_MAP.len(), SUPPORTED_ADAPTERS.len());
    }

    #[test]
    fn is_adapter_kind_accepts_only_the_eight() {
        for adapter in SUPPORTED_ADAPTERS {
            assert!(is_adapter_kind(adapter.as_str()));
        }
        assert!(!is_adapter_kind("unknown"));
        assert!(!is_adapter_kind(""));
    }

    #[test]
    fn is_model_tier_accepts_only_the_three() {
        for tier in ModelTier::ALL {
            assert!(is_model_tier(tier.as_str()));
        }
        assert!(!is_model_tier("unknown"));
        assert!(!is_model_tier(""));
    }

    #[test]
    fn supported_adapters_has_8_entries_in_display_order() {
        let names: Vec<&str> = SUPPORTED_ADAPTERS.iter().map(|a| a.as_str()).collect();
        assert_eq!(
            names,
            [
                "claude",
                "opencode",
                "codex",
                "cursor",
                "pi",
                "grok",
                "antigravity",
                "shell"
            ]
        );
    }

    #[test]
    fn adapter_and_tier_strings_round_trip() {
        for adapter in SUPPORTED_ADAPTERS {
            assert_eq!(
                serde_json::to_string(&adapter).unwrap(),
                format!("\"{}\"", adapter.as_str())
            );
            assert_eq!(adapter.as_str().parse::<AdapterKind>(), Ok(adapter));
        }
        for tier in ModelTier::ALL {
            assert_eq!(
                serde_json::to_string(&tier).unwrap(),
                format!("\"{}\"", tier.as_str())
            );
            assert_eq!(tier.as_str().parse::<ModelTier>(), Ok(tier));
        }
    }
}
