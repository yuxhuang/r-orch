//! T1 / F1 — D5 effort contract, policy half (vectors D5-2 … D5-8, D5-12 and
//! the domain side of D5-1/D5-7/D5-11).
//!
//! The expectations below are an **independent oracle written from design-v3
//! §D5.1–D5.4** (adapter × level × model matrix), not read back from
//! `EFFORT_CAPABILITIES`, so a table edit that drifts from the design fails
//! here even if the module's own inline tests still agree with themselves.
//!
//! Where design-v3 is silent and B1 made a judgement call (ordering of
//! rejection kinds; a legacy value that a *known* Codex/Antigravity row does
//! not list is ignored rather than forwarded — build-b1.md decision 1) the
//! matrix compares accept/reject and the dispatch category only; the specific
//! rejection kind is asserted in targeted tests.

use ReasoningEffort as E;
use orch_domain::{
    AdapterKind, CODEX_ULTRA_LABEL, EFFORT_CAPABILITIES, EFFORT_REQUEST_ONLY_NOTE,
    EFFORT_TABLE_CHECKED, EffortDiagnostic, EffortDispatch, EffortReason, EffortRejection,
    EffortSelector, EffortStatus, EffortWire, EvidenceStatus, GROK_EVIDENCE_GAP, ReasoningEffort,
    SUPPORTED_ADAPTERS, UnknownModelPolicy, allowed_efforts, effort_capability,
    plan_after_wire_check_failure, plan_effort_dispatch, split_variant_suffix,
    validate_effort_authoring,
};

const ALL: [ReasoningEffort; 9] = ReasoningEffort::ALL;
const LMH: [ReasoningEffort; 3] = [E::Low, E::Medium, E::High];
const CLAUDE_5: [ReasoningEffort; 5] = [E::Low, E::Medium, E::High, E::Xhigh, E::Max];
const CLAUDE_4_6: [ReasoningEffort; 4] = [E::Low, E::Medium, E::High, E::Max];
const CODEX_6: [ReasoningEffort; 6] = [E::Low, E::Medium, E::High, E::Xhigh, E::Max, E::Ultra];
const PI_7: [ReasoningEffort; 7] = [
    E::Off,
    E::Minimal,
    E::Low,
    E::Medium,
    E::High,
    E::Xhigh,
    E::Max,
];
const OPENCODE_6: [ReasoningEffort; 6] = [E::None, E::Low, E::Medium, E::High, E::Xhigh, E::Max];

const CODEX_ULTRA_MODELS: [&str; 5] = [
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
];
const CODEX_LUNA: [&str; 2] = ["gpt-6-luna", "gpt-5.6-luna"];

const OC_OPUS_5_5: &str = "openrouter/anthropic/claude-opus-5.5";
const OC_OPUS_4_6: &str = "openrouter/anthropic/claude-opus-4.6";
const OC_GEMINI_FLASH: &str = "openrouter/google/gemini-3.8-flash";
const OC_GEMINI_2_5: &str = "openrouter/google/gemini-2.5-flash";

// ── The independent oracle ──────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Verdict {
    Accept(Wire),
    Refuse,
}

#[derive(Debug, PartialEq)]
enum Wire {
    Flag(&'static str, ReasoningEffort),
    Config(&'static str, ReasoningEffort),
    Variant(String),
}

fn non_empty(model: Option<&str>) -> Option<&str> {
    model.filter(|m| !m.is_empty())
}

fn claude_row(model: &str) -> Option<&'static [ReasoningEffort]> {
    match model {
        "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-5-5" => Some(&CLAUDE_5),
        "claude-opus-4-6" | "claude-sonnet-4-6" => Some(&CLAUDE_4_6),
        _ => None,
    }
}

fn codex_row(model: &str) -> Option<&'static [ReasoningEffort]> {
    if CODEX_ULTRA_MODELS.contains(&model) {
        Some(&CODEX_6)
    } else if CODEX_LUNA.contains(&model) {
        Some(&CODEX_6[..5])
    } else {
        None
    }
}

fn agy_row(model: &str) -> Option<&'static [ReasoningEffort]> {
    match model {
        "gemini-4-argon" | "gemini-3.8-flash" | "claude-opus-5-5" | "claude-sonnet-5-5" => {
            Some(&LMH)
        }
        "gpt-oss-120b" => Some(&[E::Medium]),
        _ => None,
    }
}

fn is_agy_suffixed(model: &str) -> bool {
    [
        "gemini-4-argon",
        "gemini-3.8-flash",
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "gpt-oss-120b",
    ]
    .iter()
    .any(|base| {
        let Some(level) = model.strip_prefix(base).and_then(|r| r.strip_prefix('-')) else {
            return false;
        };
        agy_row(base).is_some_and(|row| row.iter().any(|l| l.as_str() == level))
    })
}

fn opencode_row(base: &str) -> Option<&'static [ReasoningEffort]> {
    match base {
        OC_OPUS_5_5 => Some(&[E::Low, E::Medium, E::High, E::Xhigh, E::Max]),
        OC_OPUS_4_6 => Some(&[E::None, E::Low, E::Medium, E::High, E::Max]),
        OC_GEMINI_FLASH => Some(&LMH),
        OC_GEMINI_2_5 => Some(&[E::None]),
        _ => None,
    }
}

/// design-v3 D5.1 table, as a verdict for a final-candidate definition.
fn oracle(adapter: &str, model: Option<&str>, level: ReasoningEffort) -> Verdict {
    use Verdict::{Accept, Refuse};
    let model = non_empty(model);
    match adapter {
        "claude" => {
            if !CLAUDE_5.contains(&level) {
                return Refuse;
            }
            if let Some(row) = model.and_then(claude_row)
                && !row.contains(&level)
            {
                return Refuse;
            }
            Accept(Wire::Flag("--effort", level))
        }
        "codex" => {
            if !CODEX_6.contains(&level) {
                return Refuse;
            }
            match model.and_then(codex_row) {
                Some(row) if !row.contains(&level) => return Refuse,
                Some(_) => {}
                None if level == E::Ultra => return Refuse,
                None => {}
            }
            Accept(Wire::Config("model_reasoning_effort", level))
        }
        "pi" => {
            if PI_7.contains(&level) {
                Accept(Wire::Flag("--thinking", level))
            } else {
                Refuse
            }
        }
        "grok" => {
            if LMH.contains(&level) {
                Accept(Wire::Flag("--effort", level))
            } else {
                Refuse
            }
        }
        "antigravity" => {
            if !LMH.contains(&level) {
                return Refuse;
            }
            if let Some(m) = model {
                if is_agy_suffixed(m) {
                    return Refuse;
                }
                if let Some(row) = agy_row(m)
                    && !row.contains(&level)
                {
                    return Refuse;
                }
            }
            Accept(Wire::Flag("--effort", level))
        }
        "opencode" => {
            if !OPENCODE_6.contains(&level) {
                return Refuse;
            }
            let Some(model) = model else { return Refuse };
            let (base, suffix) = model
                .split_once('#')
                .map_or((model, None), |(b, s)| (b, Some(s)));
            let Some(row) = opencode_row(base) else {
                return Refuse;
            };
            if suffix.is_some_and(|s| s != level.as_str()) || !row.contains(&level) {
                return Refuse;
            }
            Accept(Wire::Variant(format!("{base}#{level}")))
        }
        _ => Refuse, // cursor, shell, unknown adapters carry no generic effort
    }
}

/// The models exercised per adapter: unset, empty, unknown, aliases, every
/// documented row and the near-misses that must not be inferred.
fn models_for(adapter: &str) -> Vec<Option<&'static str>> {
    let mut v: Vec<Option<&'static str>> = vec![None, Some(""), Some("custom/unknown-model")];
    let extra: &[&str] = match adapter {
        "claude" => &[
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "opus",
            "sonnet",
            "claude-opus-4-6-20260101",
            "claude-haiku-4-5",
        ],
        "codex" => &[
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-6-luna",
            "gpt-5.6-luna",
            "gpt-5.3-codex",
            "gpt-6.1-sol-preview",
            "GPT-6.1-SOL",
        ],
        "opencode" => &[
            OC_OPUS_5_5,
            OC_OPUS_4_6,
            OC_GEMINI_FLASH,
            OC_GEMINI_2_5,
            "openrouter/anthropic/claude-opus-5.5#high",
            "openrouter/anthropic/claude-opus-5.5#low",
            "openrouter/anthropic/claude-opus-5.5#thinking",
            "openrouter/anthropic/claude-opus-5.5-custom",
            "openrouter/openai/gpt-5",
        ],
        "antigravity" => &[
            "gemini-4-argon",
            "gemini-3.8-flash",
            "gpt-oss-120b",
            "gemini-4-argon-high",
            "gemini-3.8-flash-low",
            "gpt-oss-120b-medium",
            "gpt-oss-120b-high",
            "gemini-9-unknown",
        ],
        "pi" => &[
            "openai-codex/gpt-6.1-sol",
            "anthropic/claude-sonnet-5-5:low",
        ],
        "grok" => &["grok-build"],
        "cursor" => &["auto", "composer-2[effort=high]"],
        _ => &[],
    };
    v.extend(extra.iter().map(|m| Some(*m)));
    v
}

fn wire_of(w: &EffortWire) -> Wire {
    match w {
        EffortWire::Flag { flag, level } => Wire::Flag(flag, *level),
        EffortWire::ConfigOverride { key, level } => Wire::Config(key, *level),
        EffortWire::ModelVariant { model } => Wire::Variant(model.clone()),
    }
}

fn adapters() -> Vec<&'static str> {
    SUPPORTED_ADAPTERS.iter().map(AdapterKind::as_str).collect()
}

fn reasons(diags: &[EffortDiagnostic]) -> Vec<EffortReason> {
    diags.iter().map(|d| d.reason).collect()
}

// ── D5-2: the whole cross product, authoring vs dispatch ────────────────────

#[test]
fn authoring_matches_the_design_table_for_every_adapter_level_and_model() {
    let mut checked = 0;
    for adapter in adapters() {
        for model in models_for(adapter) {
            for level in ALL {
                let got = validate_effort_authoring(adapter, model, Some(level));
                let want = oracle(adapter, model, level);
                match (&got, &want) {
                    (Ok(_), Verdict::Accept(_)) | (Err(_), Verdict::Refuse) => {}
                    _ => panic!(
                        "authoring {adapter} {model:?} {level}: got {got:?}, design says {want:?}"
                    ),
                }
                checked += 1;
            }
        }
    }
    assert!(checked >= 585, "{checked}");
}

#[test]
fn dispatch_matches_the_design_table_for_every_adapter_level_and_model() {
    for adapter in adapters() {
        for model in models_for(adapter) {
            for level in ALL {
                let plan = plan_effort_dispatch(adapter, model, Some(level));
                match (oracle(adapter, model, level), &plan) {
                    (
                        Verdict::Accept(wire),
                        EffortDispatch::Forward {
                            wire: got,
                            requires_wire_check,
                            ..
                        },
                    ) => {
                        assert_eq!(wire_of(got), wire, "{adapter} {model:?} {level}");
                        assert_eq!(
                            *requires_wire_check,
                            adapter == "opencode",
                            "{adapter} {model:?} {level}"
                        );
                    }
                    (Verdict::Refuse, EffortDispatch::Reject(err)) => {
                        assert!(
                            !level.is_legacy(),
                            "legacy never rejects: {adapter} {model:?} {level}"
                        );
                        assert_eq!(err.value, level);
                        assert_eq!(err.adapter, adapter);
                        assert_eq!(err.model.as_deref(), model);
                    }
                    (Verdict::Refuse, EffortDispatch::IgnoreLegacy { diagnostics }) => {
                        assert!(
                            level.is_legacy(),
                            "only legacy is ignored: {adapter} {model:?} {level}"
                        );
                        assert_eq!(diagnostics.len(), 1);
                        assert_eq!(diagnostics[0].status, EffortStatus::LegacyIgnored);
                    }
                    (Verdict::Refuse, EffortDispatch::Forward { diagnostics, .. }) => {
                        // The only legacy forwards of a refused combination:
                        // a known Antigravity effort-suffixed slug (lead
                        // exception) and Claude/Pi/Grok byte-compat.
                        assert!(level.is_legacy(), "{adapter} {model:?} {level}");
                        assert!(
                            adapter == "antigravity"
                                && model.is_some_and(is_agy_suffixed)
                                && reasons(diagnostics)
                                    .contains(&EffortReason::AntigravitySuffixedModel)
                                || matches!(adapter, "claude" | "pi" | "grok"),
                            "{adapter} {model:?} {level}: {plan:?}"
                        );
                    }
                    (want, got) => {
                        panic!("dispatch {adapter} {model:?} {level}: design {want:?}, got {got:?}")
                    }
                }
            }
        }
    }
}

#[test]
fn a_legacy_spelling_is_never_a_dispatch_failure_on_any_adapter_or_model() {
    for adapter in adapters() {
        for model in models_for(adapter) {
            for level in LMH {
                assert!(
                    !matches!(
                        plan_effort_dispatch(adapter, model, Some(level)),
                        EffortDispatch::Reject(_)
                    ),
                    "{adapter} {model:?} {level}"
                );
            }
        }
    }
}

#[test]
fn a_new_spelling_is_rejected_at_dispatch_exactly_when_authoring_refuses_it() {
    for adapter in adapters() {
        for model in models_for(adapter) {
            for level in ALL.into_iter().filter(|l| !l.is_legacy()) {
                let authoring_refuses =
                    validate_effort_authoring(adapter, model, Some(level)).is_err();
                let dispatch_rejects = matches!(
                    plan_effort_dispatch(adapter, model, Some(level)),
                    EffortDispatch::Reject(_)
                );
                assert_eq!(
                    authoring_refuses, dispatch_rejects,
                    "{adapter} {model:?} {level}"
                );
            }
        }
    }
}

#[test]
fn authoring_accepts_unset_effort_for_every_adapter_and_model() {
    for adapter in adapters().into_iter().chain(["nonsense"]) {
        for model in models_for(adapter) {
            assert_eq!(
                validate_effort_authoring(adapter, model, None),
                Ok(Vec::new()),
                "{adapter} {model:?}"
            );
        }
    }
}

#[test]
fn unset_effort_plans_no_selector_and_no_wire_check_for_every_adapter_and_model() {
    for adapter in adapters().into_iter().chain(["nonsense"]) {
        for model in models_for(adapter) {
            assert_eq!(
                plan_effort_dispatch(adapter, model, None),
                EffortDispatch::Unset,
                "{adapter} {model:?}"
            );
        }
    }
}

#[test]
fn an_unknown_adapter_refuses_every_level_and_names_no_allowed_levels() {
    for level in ALL {
        let err = validate_effort_authoring("nonsense", Some("m"), Some(level)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::UnknownAdapter);
        assert!(err.allowed.is_empty());
    }
}

#[test]
fn every_forward_starts_with_the_plain_request_only_note_and_never_claims_a_confirmed_level() {
    for adapter in adapters() {
        for model in models_for(adapter) {
            for level in ALL {
                let diags = match plan_effort_dispatch(adapter, model, Some(level)) {
                    EffortDispatch::Forward { diagnostics, .. } => diagnostics,
                    _ => continue,
                };
                if diags
                    .first()
                    .is_some_and(|d| d.reason == EffortReason::RequestOnly)
                {
                    assert_eq!(diags[0].status, EffortStatus::RequestOnly);
                    assert_eq!(diags[0].message(), EFFORT_REQUEST_ONLY_NOTE);
                    assert!(!diags[0].is_warning());
                }
                for d in &diags {
                    let text = d.to_string().to_lowercase();
                    assert!(
                        !text.contains("confirmed"),
                        "{adapter} {model:?} {level}: {text}"
                    );
                    assert!(text.starts_with(d.status.as_str()), "{text}");
                }
            }
        }
    }
}

// ── D5-3: Codex ───────────────────────────────────────────────────────────────

#[test]
fn codex_ultra_is_accepted_on_each_exactly_known_model_with_the_spend_label() {
    for model in CODEX_ULTRA_MODELS {
        let diags = validate_effort_authoring("codex", Some(model), Some(E::Ultra))
            .unwrap_or_else(|e| panic!("{model}: {e}"));
        let ultra = diags
            .iter()
            .find(|d| d.reason == EffortReason::CodexUltra)
            .expect("label");
        assert_eq!(ultra.message(), CODEX_ULTRA_LABEL, "{model}");
        assert_eq!(
            CODEX_ULTRA_LABEL,
            "maximum reasoning with provider-internal sub-agent delegation; multiplies spend"
        );
    }
}

#[test]
fn codex_ultra_is_refused_for_an_unset_model() {
    for model in [None, Some("")] {
        let err = validate_effort_authoring("codex", model, Some(E::Ultra)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::UltraNeedsKnownModel);
    }
}

#[test]
fn codex_ultra_is_refused_for_luna_models() {
    for model in CODEX_LUNA {
        let err = validate_effort_authoring("codex", Some(model), Some(E::Ultra)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::LevelNotOnModel, "{model}");
        assert!(!err.allowed.contains(&E::Ultra));
    }
}

#[test]
fn codex_ultra_is_refused_for_unknown_prefix_or_differently_cased_model_ids() {
    for model in [
        "gpt-5.3-codex",
        "gpt-6.1-sol-preview",
        "GPT-6.1-SOL",
        "gpt-6",
        " gpt-6.1-sol",
    ] {
        let err = validate_effort_authoring("codex", Some(model), Some(E::Ultra)).unwrap_err();
        assert_eq!(
            err.rejection,
            EffortRejection::UltraNeedsKnownModel,
            "{model}"
        );
    }
}

#[test]
fn codex_xhigh_and_max_are_accepted_for_unset_known_luna_and_unknown_models() {
    for model in [
        None,
        Some("gpt-6.1-sol"),
        Some("gpt-6-luna"),
        Some("anything-else"),
    ] {
        for level in [E::Xhigh, E::Max] {
            assert!(
                validate_effort_authoring("codex", model, Some(level)).is_ok(),
                "{model:?} {level}"
            );
        }
    }
}

#[test]
fn codex_minimal_off_and_none_are_not_offered() {
    for level in [E::Minimal, E::Off, E::None] {
        for model in [None, Some("gpt-6.1-sol")] {
            let err = validate_effort_authoring("codex", model, Some(level)).unwrap_err();
            assert_eq!(err.rejection, EffortRejection::LevelNotOnAdapter, "{level}");
        }
    }
}

#[test]
fn codex_forwards_the_level_through_the_model_reasoning_effort_config_override() {
    for level in CODEX_6 {
        let plan = plan_effort_dispatch("codex", Some("gpt-6.1-sol"), Some(level));
        let EffortDispatch::Forward {
            wire,
            requires_wire_check,
            ..
        } = plan
        else {
            panic!("{level}");
        };
        assert_eq!(
            wire,
            EffortWire::ConfigOverride {
                key: "model_reasoning_effort",
                level
            }
        );
        assert!(!requires_wire_check);
    }
}

#[test]
fn codex_ultra_is_never_chosen_for_the_user_it_is_only_reported_for_an_explicit_value() {
    // no mode-dependent choice: the policy API has no mode parameter at all,
    // and an unset effort never plans anything for codex.
    assert_eq!(
        plan_effort_dispatch("codex", Some("gpt-6.1-sol"), None),
        EffortDispatch::Unset
    );
}

#[test]
fn allowed_codex_levels_exclude_ultra_without_a_known_ultra_row() {
    let no_ultra = vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max];
    assert_eq!(allowed_efforts("codex", None), no_ultra);
    assert_eq!(allowed_efforts("codex", Some("")), no_ultra);
    assert_eq!(allowed_efforts("codex", Some("gpt-9-unknown")), no_ultra);
    assert_eq!(allowed_efforts("codex", Some("gpt-6-luna")), no_ultra);
    assert_eq!(
        allowed_efforts("codex", Some("gpt-6.1-sol")),
        CODEX_6.to_vec()
    );
}

// ── D5-11: Codex effort with no model ────────────────────────────────────────

#[test]
fn codex_effort_with_no_model_forwards_and_warns_that_the_managed_default_may_be_bypassed() {
    for model in [None, Some("")] {
        let diags = validate_effort_authoring("codex", model, Some(E::High)).expect("allowed");
        let warn = diags
            .iter()
            .find(|d| d.reason == EffortReason::CodexManagedModelDefault)
            .expect("warning present at authoring/show");
        assert_eq!(warn.status, EffortStatus::CapabilityUnknown);
        assert!(warn.is_warning());
        assert!(
            warn.message().contains("different model"),
            "{}",
            warn.message()
        );
        let plan = plan_effort_dispatch("codex", model, Some(E::High));
        let EffortDispatch::Forward {
            wire, diagnostics, ..
        } = plan
        else {
            panic!()
        };
        assert_eq!(
            wire,
            EffortWire::ConfigOverride {
                key: "model_reasoning_effort",
                level: E::High
            }
        );
        assert!(reasons(&diagnostics).contains(&EffortReason::CodexManagedModelDefault));
    }
}

#[test]
fn codex_effort_with_an_explicit_model_has_no_managed_default_warning() {
    let diags = validate_effort_authoring("codex", Some("gpt-6.1-sol"), Some(E::High)).unwrap();
    assert!(!reasons(&diags).contains(&EffortReason::CodexManagedModelDefault));
}

// ── D5-4: OpenCode ───────────────────────────────────────────────────────────

#[test]
fn opencode_matching_suffix_and_effort_keeps_the_suffix_exactly_once() {
    let model = "openrouter/anthropic/claude-opus-5.5#high";
    let EffortDispatch::Forward {
        wire,
        requires_wire_check,
        ..
    } = plan_effort_dispatch("opencode", Some(model), Some(E::High))
    else {
        panic!()
    };
    assert_eq!(
        wire,
        EffortWire::ModelVariant {
            model: model.to_owned()
        }
    );
    assert!(requires_wire_check);
}

#[test]
fn opencode_an_unsuffixed_known_model_gets_the_level_appended_once() {
    let EffortDispatch::Forward { wire, .. } =
        plan_effort_dispatch("opencode", Some(OC_GEMINI_FLASH), Some(E::Medium))
    else {
        panic!()
    };
    assert_eq!(
        wire,
        EffortWire::ModelVariant {
            model: "openrouter/google/gemini-3.8-flash#medium".into()
        }
    );
}

#[test]
fn opencode_a_conflicting_suffix_is_refused_at_authoring_with_the_suffix_conflict_reason() {
    let err = validate_effort_authoring(
        "opencode",
        Some("openrouter/anthropic/claude-opus-5.5#high"),
        Some(E::Low),
    )
    .unwrap_err();
    assert_eq!(err.rejection, EffortRejection::SuffixConflict);
}

#[test]
fn opencode_a_conflicting_suffix_with_a_new_level_is_a_dispatch_error() {
    let plan = plan_effort_dispatch(
        "opencode",
        Some("openrouter/anthropic/claude-opus-5.5#high"),
        Some(E::Xhigh),
    );
    let EffortDispatch::Reject(err) = plan else {
        panic!("{plan:?}")
    };
    assert_eq!(err.rejection, EffortRejection::SuffixConflict);
}

#[test]
fn opencode_a_conflicting_suffix_with_a_legacy_level_is_ignored_and_keeps_the_original_model() {
    let plan = plan_effort_dispatch(
        "opencode",
        Some("openrouter/anthropic/claude-opus-5.5#high"),
        Some(E::Low),
    );
    let EffortDispatch::IgnoreLegacy { diagnostics } = plan else {
        panic!("{plan:?}")
    };
    assert_eq!(diagnostics[0].reason, EffortReason::LegacySuffixConflict);
    assert!(
        diagnostics[0].message().contains("kept"),
        "{}",
        diagnostics[0].message()
    );
}

#[test]
fn opencode_a_delegated_or_unknown_model_with_a_legacy_level_is_ignored_not_rejected() {
    for model in [
        None,
        Some(""),
        Some("openrouter/openai/gpt-5"),
        Some(OC_OPUS_5_5.trim_end_matches('5')),
    ] {
        let plan = plan_effort_dispatch("opencode", model, Some(E::High));
        let EffortDispatch::IgnoreLegacy { diagnostics } = plan else {
            panic!("{model:?}: {plan:?}")
        };
        assert_eq!(
            diagnostics[0].reason,
            EffortReason::LegacyModelRequired,
            "{model:?}"
        );
    }
}

#[test]
fn opencode_a_delegated_or_unknown_model_with_a_new_level_is_a_configuration_error() {
    for model in [None, Some("openrouter/openai/gpt-5")] {
        let EffortDispatch::Reject(err) = plan_effort_dispatch("opencode", model, Some(E::Xhigh))
        else {
            panic!("{model:?}");
        };
        assert_eq!(err.rejection, EffortRejection::ModelRequired);
    }
}

#[test]
fn opencode_a_thinking_suffix_with_effort_unset_is_left_alone() {
    let model = Some("openrouter/anthropic/claude-opus-5.5#thinking");
    assert_eq!(
        plan_effort_dispatch("opencode", model, None),
        EffortDispatch::Unset
    );
    assert_eq!(
        validate_effort_authoring("opencode", model, None),
        Ok(Vec::new())
    );
}

#[test]
fn opencode_known_rows_offer_exactly_the_design_levels() {
    assert_eq!(
        allowed_efforts("opencode", Some(OC_OPUS_5_5)),
        vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max]
    );
    assert_eq!(
        allowed_efforts("opencode", Some(OC_OPUS_4_6)),
        vec![E::None, E::Low, E::Medium, E::High, E::Max]
    );
    assert_eq!(
        allowed_efforts("opencode", Some(OC_GEMINI_FLASH)),
        LMH.to_vec()
    );
    assert_eq!(
        allowed_efforts("opencode", Some(OC_GEMINI_2_5)),
        vec![E::None]
    );
}

#[test]
fn opencode_without_a_known_model_offers_no_levels() {
    for model in [None, Some(""), Some("openrouter/openai/gpt-5")] {
        assert!(allowed_efforts("opencode", model).is_empty(), "{model:?}");
    }
}

#[test]
fn opencode_allowed_levels_ignore_a_variant_suffix_on_a_known_base() {
    assert_eq!(
        allowed_efforts(
            "opencode",
            Some("openrouter/anthropic/claude-opus-5.5#high")
        ),
        allowed_efforts("opencode", Some(OC_OPUS_5_5))
    );
}

#[test]
fn opencode_minimal_off_and_ultra_are_not_offered_on_any_model() {
    for level in [E::Minimal, E::Off, E::Ultra] {
        let err =
            validate_effort_authoring("opencode", Some(OC_OPUS_5_5), Some(level)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::LevelNotOnAdapter, "{level}");
    }
}

#[test]
fn opencode_forwards_report_a_possible_local_override_of_the_variant() {
    let EffortDispatch::Forward { diagnostics, .. } =
        plan_effort_dispatch("opencode", Some(OC_OPUS_5_5), Some(E::High))
    else {
        panic!()
    };
    assert!(
        diagnostics
            .iter()
            .any(|d| d.status == EffortStatus::LocalOverridePossible
                && d.reason == EffortReason::OpencodeLocalVariant)
    );
}

// ── D5-5: Pi, Claude, Antigravity ───────────────────────────────────────────

#[test]
fn pi_off_is_accepted_and_none_is_not_aliased_to_it() {
    assert!(validate_effort_authoring("pi", None, Some(E::Off)).is_ok());
    let err = validate_effort_authoring("pi", None, Some(E::None)).unwrap_err();
    assert_eq!(err.rejection, EffortRejection::LevelNotOnAdapter);
    let EffortDispatch::Forward { wire, .. } = plan_effort_dispatch("pi", None, Some(E::Off))
    else {
        panic!()
    };
    assert_eq!(
        wire,
        EffortWire::Flag {
            flag: "--thinking",
            level: E::Off
        }
    );
}

#[test]
fn pi_minimal_is_accepted_and_ultra_is_not() {
    assert!(validate_effort_authoring("pi", None, Some(E::Minimal)).is_ok());
    assert!(validate_effort_authoring("pi", None, Some(E::Ultra)).is_err());
}

#[test]
fn pi_always_reports_possible_clamping_and_never_rewrites_a_colon_suffixed_model() {
    let EffortDispatch::Forward {
        wire, diagnostics, ..
    } = plan_effort_dispatch("pi", Some("anthropic/claude-sonnet-5-5:low"), Some(E::High))
    else {
        panic!()
    };
    assert_eq!(
        wire,
        EffortWire::Flag {
            flag: "--thinking",
            level: E::High
        }
    );
    let rs = reasons(&diagnostics);
    assert!(rs.contains(&EffortReason::PiClamp));
    assert!(
        rs.contains(&EffortReason::PiFlagOverridesModelSuffix),
        "explicit flag precedence disclosed"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.status == EffortStatus::ClampPossible)
    );
}

#[test]
fn pi_without_a_suffix_does_not_claim_flag_precedence_but_still_reports_clamping() {
    let EffortDispatch::Forward { diagnostics, .. } =
        plan_effort_dispatch("pi", Some("openai-codex/gpt-6.1-sol"), Some(E::High))
    else {
        panic!()
    };
    let rs = reasons(&diagnostics);
    assert!(rs.contains(&EffortReason::PiClamp));
    assert!(!rs.contains(&EffortReason::PiFlagOverridesModelSuffix));
}

#[test]
fn claude_4_6_models_do_not_offer_xhigh_in_new_authoring() {
    for model in ["claude-opus-4-6", "claude-sonnet-4-6"] {
        let err = validate_effort_authoring("claude", Some(model), Some(E::Xhigh)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::LevelNotOnModel, "{model}");
        assert_eq!(err.allowed, CLAUDE_4_6.to_vec());
    }
}

#[test]
fn claude_5_5_models_offer_xhigh_and_max() {
    for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] {
        for level in [E::Xhigh, E::Max] {
            assert!(
                validate_effort_authoring("claude", Some(model), Some(level)).is_ok(),
                "{model} {level}"
            );
        }
    }
}

#[test]
fn claude_unresolved_aliases_get_syntax_validation_and_a_capability_unknown_warning() {
    for model in [
        "opus",
        "sonnet",
        "claude-opus-4-6-20260101",
        "claude-haiku-4-5",
    ] {
        let diags = validate_effort_authoring("claude", Some(model), Some(E::Xhigh))
            .unwrap_or_else(|e| {
                panic!("{model}: syntax-only validation must not guess a row: {e}")
            });
        assert!(
            diags
                .iter()
                .any(|d| d.status == EffortStatus::CapabilityUnknown
                    && d.reason == EffortReason::UnknownModel),
            "{model}: {diags:?}"
        );
    }
}

#[test]
fn claude_with_no_model_warns_that_the_default_model_decides() {
    let diags = validate_effort_authoring("claude", None, Some(E::High)).unwrap();
    assert!(reasons(&diags).contains(&EffortReason::DefaultModel));
}

#[test]
fn claude_new_levels_outside_the_cli_syntax_are_refused_whatever_the_model() {
    for level in [E::None, E::Off, E::Minimal, E::Ultra] {
        let err =
            validate_effort_authoring("claude", Some("claude-opus-5-5"), Some(level)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::LevelNotOnAdapter, "{level}");
    }
}

#[test]
fn antigravity_legacy_effort_on_an_effort_suffixed_slug_is_forwarded_and_labelled_unresolved() {
    for model in [
        "gemini-4-argon-high",
        "gemini-3.8-flash-low",
        "gpt-oss-120b-medium",
    ] {
        let plan = plan_effort_dispatch("antigravity", Some(model), Some(E::High));
        let EffortDispatch::Forward {
            wire, diagnostics, ..
        } = plan
        else {
            panic!("{model}: {plan:?}")
        };
        assert_eq!(
            wire,
            EffortWire::Flag {
                flag: "--effort",
                level: E::High
            },
            "{model}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.reason == EffortReason::AntigravitySuffixedModel
                    && d.status == EffortStatus::CapabilityUnknown),
            "{model}"
        );
    }
}

#[test]
fn antigravity_new_authoring_of_an_effort_suffixed_slug_asks_for_the_base_model() {
    let err = validate_effort_authoring("antigravity", Some("gemini-4-argon-high"), Some(E::High))
        .unwrap_err();
    assert_eq!(err.rejection, EffortRejection::SuffixedModel);
    assert!(err.to_string().contains("base model"), "{err}");
}

#[test]
fn antigravity_does_not_treat_unlisted_or_unknown_suffixes_as_slugs() {
    // gpt-oss lists only medium, so "-high" is just an unknown model name.
    let diags = validate_effort_authoring("antigravity", Some("gpt-oss-120b-high"), Some(E::Low))
        .expect("no general suffix stripping");
    assert!(!reasons(&diags).contains(&EffortReason::AntigravitySuffixedModel));
}

#[test]
fn antigravity_gpt_oss_permits_medium_only_and_xhigh_max_are_not_offered() {
    assert!(
        validate_effort_authoring("antigravity", Some("gpt-oss-120b"), Some(E::Medium)).is_ok()
    );
    assert_eq!(
        validate_effort_authoring("antigravity", Some("gpt-oss-120b"), Some(E::High))
            .unwrap_err()
            .rejection,
        EffortRejection::LevelNotOnModel
    );
    for level in [E::Xhigh, E::Max] {
        assert_eq!(
            validate_effort_authoring("antigravity", Some("gemini-4-argon"), Some(level))
                .unwrap_err()
                .rejection,
            EffortRejection::LevelNotOnAdapter,
            "the CLI parser's xhigh/max is not evidence of an available model"
        );
    }
}

// ── D5-6: final-candidate validation ─────────────────────────────────────────

#[test]
fn switching_the_model_to_luna_while_keeping_ultra_fails_with_adapter_model_value_and_allowed() {
    let err = validate_effort_authoring("codex", Some("gpt-6-luna"), Some(E::Ultra)).unwrap_err();
    assert_eq!(err.adapter, "codex");
    assert_eq!(err.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(err.value, E::Ultra);
    assert_eq!(
        err.allowed,
        vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max]
    );
    let text = err.to_string();
    for needle in [
        "`ultra`",
        "`codex`",
        "`gpt-6-luna`",
        "Allowed: low, medium, high, xhigh, max",
        "--effort default",
    ] {
        assert!(text.contains(needle), "{needle} missing in {text}");
    }
}

#[test]
fn switching_the_adapter_to_antigravity_while_keeping_xhigh_fails_and_lists_the_menu() {
    let err = validate_effort_authoring("antigravity", Some("gemini-4-argon"), Some(E::Xhigh))
        .unwrap_err();
    assert_eq!(err.allowed, LMH.to_vec());
    assert!(
        err.to_string().contains("or choose an allowed level"),
        "{err}"
    );
}

#[test]
fn switching_the_adapter_to_cursor_while_keeping_an_effort_fails_with_no_allowed_levels() {
    let err = validate_effort_authoring("cursor", Some("auto"), Some(E::High)).unwrap_err();
    assert_eq!(err.rejection, EffortRejection::AdapterUnsupported);
    assert!(err.allowed.is_empty());
    let text = err.to_string();
    assert!(text.contains("Allowed: none"), "{text}");
    assert!(text.contains("--effort default"), "{text}");
    assert!(!text.contains("choose an allowed level"), "{text}");
}

#[test]
fn clearing_the_effort_in_the_same_mutation_makes_the_incompatible_candidate_valid() {
    assert!(validate_effort_authoring("cursor", Some("auto"), None).is_ok());
    assert!(validate_effort_authoring("codex", Some("gpt-6-luna"), None).is_ok());
}

#[test]
fn an_unrelated_edit_of_an_invalid_stored_legacy_combination_fails_authoring() {
    // The stored legacy value still dispatches (ignored), but a final-candidate
    // check of an unrelated edit refuses it: the caller must keep the bytes.
    for (adapter, model) in [
        ("cursor", Some("auto")),
        ("shell", None),
        ("opencode", None),
    ] {
        assert!(
            validate_effort_authoring(adapter, model, Some(E::High)).is_err(),
            "{adapter}"
        );
        assert!(matches!(
            plan_effort_dispatch(adapter, model, Some(E::High)),
            EffortDispatch::IgnoreLegacy { .. }
        ));
    }
}

// ── D5-7: legacy Cursor / Shell / OpenCode-delegated high ───────────────────

#[test]
fn legacy_effort_on_cursor_and_shell_is_ignored_with_a_legacy_ignored_warning() {
    for adapter in ["cursor", "shell"] {
        for level in LMH {
            let plan = plan_effort_dispatch(adapter, Some("auto"), Some(level));
            let EffortDispatch::IgnoreLegacy { diagnostics } = plan else {
                panic!("{adapter} {level}")
            };
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].status, EffortStatus::LegacyIgnored);
            assert_eq!(
                diagnostics[0].reason,
                EffortReason::LegacyAdapterUnsupported
            );
            assert!(diagnostics[0].message().contains("--effort default"));
        }
    }
}

#[test]
fn new_effort_values_on_cursor_and_shell_are_rejected_at_dispatch() {
    for adapter in ["cursor", "shell"] {
        for level in ALL.into_iter().filter(|l| !l.is_legacy()) {
            let EffortDispatch::Reject(err) = plan_effort_dispatch(adapter, None, Some(level))
            else {
                panic!("{adapter} {level}")
            };
            assert_eq!(err.rejection, EffortRejection::AdapterUnsupported);
        }
    }
}

// ── D5-12: Grok and Cursor ───────────────────────────────────────────────────

#[test]
fn grok_artifact_values_outside_low_medium_high_are_refused_with_the_evidence_gap_message() {
    for level in [E::None, E::Off, E::Minimal, E::Xhigh, E::Max, E::Ultra] {
        let err = validate_effort_authoring("grok", Some("grok-build"), Some(level)).unwrap_err();
        assert_eq!(err.rejection, EffortRejection::GrokEvidenceGap, "{level}");
        let text = err.to_string();
        assert!(text.contains(GROK_EVIDENCE_GAP), "{text}");
        assert_eq!(err.allowed, LMH.to_vec());
        for forbidden in [
            "invalid",
            "not supported by grok",
            "grok rejects",
            "unsupported by",
        ] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "{forbidden} in {text}"
            );
        }
    }
}

#[test]
fn grok_low_medium_high_forward_through_effort_flag_with_no_capability_unknown_flood() {
    for level in LMH {
        let EffortDispatch::Forward {
            wire, diagnostics, ..
        } = plan_effort_dispatch("grok", None, Some(level))
        else {
            panic!()
        };
        assert_eq!(
            wire,
            EffortWire::Flag {
                flag: "--effort",
                level
            }
        );
        assert_eq!(reasons(&diagnostics), vec![EffortReason::RequestOnly]);
    }
}

#[test]
fn grok_evidence_status_stays_unverified_with_a_date_and_a_version() {
    let row = effort_capability(AdapterKind::Grok);
    assert_eq!(row.evidence.status, EvidenceStatus::Unverified);
    assert_eq!(row.evidence.status.as_str(), "unverified");
    assert_eq!(row.evidence.checked, "2026-10-22");
    assert!(
        row.evidence.cli_version.contains("1.0.67"),
        "{}",
        row.evidence.cli_version
    );
}

#[test]
fn cursor_accepts_an_opaque_bracket_model_only_when_no_generic_effort_is_set() {
    let model = Some("composer-2[effort=high]");
    assert_eq!(
        validate_effort_authoring("cursor", model, None),
        Ok(Vec::new())
    );
    assert_eq!(
        plan_effort_dispatch("cursor", model, None),
        EffortDispatch::Unset
    );
    assert!(validate_effort_authoring("cursor", model, Some(E::High)).is_err());
}

#[test]
fn the_domain_never_synthesizes_a_cursor_effort_selector() {
    for level in ALL {
        assert!(!matches!(
            plan_effort_dispatch("cursor", Some("auto"), Some(level)),
            EffortDispatch::Forward { .. }
        ));
    }
}

// ── D5-8 / D5-9 (policy half) ─────────────────────────────────────────────────

#[test]
fn split_variant_suffix_splits_at_the_first_hash_only() {
    assert_eq!(split_variant_suffix("a/b"), ("a/b", None));
    assert_eq!(split_variant_suffix("a/b#high"), ("a/b", Some("high")));
    assert_eq!(split_variant_suffix("a/b#high#x"), ("a/b", Some("high#x")));
    assert_eq!(split_variant_suffix("a/b#"), ("a/b", Some("")));
    assert_eq!(split_variant_suffix("#high"), ("", Some("high")));
    assert_eq!(split_variant_suffix(""), ("", None));
}

#[test]
fn a_failed_wire_check_for_a_legacy_value_ignores_it_and_keeps_the_model() {
    let plan = plan_after_wire_check_failure("opencode", Some(OC_OPUS_5_5), E::High);
    let EffortDispatch::IgnoreLegacy { diagnostics } = plan else {
        panic!("{plan:?}")
    };
    assert_eq!(diagnostics[0].reason, EffortReason::LegacyWireCheckFailed);
    assert_eq!(diagnostics[0].status, EffortStatus::LegacyIgnored);
}

#[test]
fn a_failed_wire_check_for_a_new_value_is_a_configuration_error_before_any_model_child() {
    for level in [E::None, E::Xhigh, E::Max] {
        let plan = plan_after_wire_check_failure("opencode", Some(OC_OPUS_5_5), level);
        let EffortDispatch::Reject(err) = plan else {
            panic!("{level}")
        };
        assert_eq!(err.rejection, EffortRejection::WireCheckFailed);
        assert_eq!(err.value, level);
        assert!(err.to_string().contains("major version 2"), "{err}");
    }
}

// ── Table integrity (evidence, selectors, ordering) ──────────────────────────

#[test]
fn the_capability_table_has_one_row_per_adapter_in_supported_order() {
    let rows: Vec<AdapterKind> = EFFORT_CAPABILITIES.iter().map(|c| c.adapter).collect();
    assert_eq!(rows, SUPPORTED_ADAPTERS.to_vec());
}

#[test]
fn every_row_is_dated_and_carries_a_version_and_a_source() {
    for row in &EFFORT_CAPABILITIES {
        assert_eq!(
            row.evidence.checked, EFFORT_TABLE_CHECKED,
            "{}",
            row.adapter
        );
        assert_eq!(EFFORT_TABLE_CHECKED, "2026-10-22");
        assert!(!row.evidence.cli_version.is_empty(), "{}", row.adapter);
        assert!(!row.evidence.sources.is_empty(), "{}", row.adapter);
    }
}

#[test]
fn evidence_status_vocabulary_is_exact() {
    let status = |a| effort_capability(a).evidence.status.as_str();
    assert_eq!(status(AdapterKind::Claude), "installed-cli");
    assert_eq!(status(AdapterKind::Codex), "installed-cli");
    assert_eq!(status(AdapterKind::Opencode), "installed-cli");
    assert_eq!(status(AdapterKind::Antigravity), "installed-cli");
    assert_eq!(status(AdapterKind::Pi), "primary-docs");
    assert_eq!(status(AdapterKind::Grok), "unverified");
    assert_eq!(status(AdapterKind::Cursor), "unverified");
    assert_eq!(status(AdapterKind::Shell), "not-applicable");
}

#[test]
fn native_selectors_are_the_documented_ones() {
    let sel = |a| effort_capability(a).selector;
    assert_eq!(sel(AdapterKind::Claude), EffortSelector::Flag("--effort"));
    assert_eq!(
        sel(AdapterKind::Codex),
        EffortSelector::ConfigOverride("model_reasoning_effort")
    );
    assert_eq!(
        sel(AdapterKind::Opencode),
        EffortSelector::ModelVariantSuffix
    );
    assert_eq!(
        sel(AdapterKind::Antigravity),
        EffortSelector::Flag("--effort")
    );
    assert_eq!(sel(AdapterKind::Pi), EffortSelector::Flag("--thinking"));
    assert_eq!(sel(AdapterKind::Grok), EffortSelector::Flag("--effort"));
    assert_eq!(sel(AdapterKind::Cursor), EffortSelector::Unsupported);
    assert_eq!(sel(AdapterKind::Shell), EffortSelector::Unsupported);
}

#[test]
fn unknown_model_policy_is_syntax_only_except_opencode_and_the_unsupported_adapters() {
    let policy = |a| effort_capability(a).unknown_model;
    assert_eq!(
        policy(AdapterKind::Opencode),
        UnknownModelPolicy::RequireKnownModel
    );
    assert_eq!(
        policy(AdapterKind::Cursor),
        UnknownModelPolicy::NotApplicable
    );
    assert_eq!(
        policy(AdapterKind::Shell),
        UnknownModelPolicy::NotApplicable
    );
    for a in [
        AdapterKind::Claude,
        AdapterKind::Codex,
        AdapterKind::Pi,
        AdapterKind::Grok,
        AdapterKind::Antigravity,
    ] {
        assert_eq!(policy(a), UnknownModelPolicy::SyntaxOnly, "{a}");
    }
}

#[test]
fn authorable_sets_match_the_design_per_adapter() {
    let authorable = |a| effort_capability(a).authorable.to_vec();
    assert_eq!(authorable(AdapterKind::Claude), CLAUDE_5.to_vec());
    assert_eq!(authorable(AdapterKind::Codex), CODEX_6.to_vec());
    assert_eq!(authorable(AdapterKind::Opencode), OPENCODE_6.to_vec());
    assert_eq!(authorable(AdapterKind::Antigravity), LMH.to_vec());
    assert_eq!(authorable(AdapterKind::Pi), PI_7.to_vec());
    assert_eq!(authorable(AdapterKind::Grok), LMH.to_vec());
    assert!(authorable(AdapterKind::Cursor).is_empty());
    assert!(authorable(AdapterKind::Shell).is_empty());
}

#[test]
fn the_effort_error_display_names_the_unset_model_when_none_is_configured() {
    let err = validate_effort_authoring("cursor", None, Some(E::High)).unwrap_err();
    assert!(err.to_string().contains("with model `<unset>`"), "{err}");
}
