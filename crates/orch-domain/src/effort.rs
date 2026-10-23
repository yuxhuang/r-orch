//! Reasoning-effort capability table and policy, per adapter.
//!
//! One pure table answers, for an adapter and an optional model: which
//! [`ReasoningEffort`] spellings may be authored, which native selector
//! carries them, and how much the table actually knows (its evidence). Two
//! policies are built on it:
//!
//! - [`validate_effort_authoring`]: strict, for every mutation of an agent
//!   definition (add, update, patch, file replacement, wizard). Validates the
//!   final candidate.
//! - [`plan_effort_dispatch`]: what a dispatch does with a stored value. A
//!   legacy spelling (`low|medium|high`, the only ones r-orch ≤ 0.9.4 wrote)
//!   never makes a dispatch fail; it is forwarded, or ignored with a warning
//!   where the adapter or model cannot carry it. Every other spelling is
//!   validated as strictly as at authoring.
//!
//! Effort is a **request**. Passing validation, or a successful spawn, never
//! means the provider applied that reasoning level: providers clamp, override
//! or reject per model. Nothing here reports an effective level; diagnostics
//! use the request-only vocabulary of [`EffortStatus`].
//!
//! No I/O. The one installed-binary check (OpenCode's v1/v2 model-suffix
//! syntax) is the adapter's job; this module only says when it is required
//! ([`EffortDispatch::Forward::requires_wire_check`]) and what a failed check
//! means ([`plan_after_wire_check_failure`]).
//!
//! The table is dated contract data. Expanding it is a reviewed row change
//! with fixtures, after re-checking the CLI's help and primary docs.

use crate::agent::ReasoningEffort;
use crate::model_tiers::AdapterKind;

use ReasoningEffort as E;

/// Date the table rows were checked against their sources.
pub const EFFORT_TABLE_CHECKED: &str = "2026-10-22";

/// Disclosure shown wherever effort is authored or reported.
pub const EFFORT_REQUEST_ONLY_NOTE: &str = "Effort requests the provider's native reasoning selector; the provider decides the effective level and may clamp, override or reject it.";

/// Required label for Codex `ultra` in help, wizard and manual (lead ruling 2).
pub const CODEX_ULTRA_LABEL: &str =
    "maximum reasoning with provider-internal sub-agent delegation; multiplies spend";

/// Why a Grok value outside `low|medium|high` is rejected (lead ruling 6). An
/// evidence gap, not a claim that Grok rejects it.
pub const GROK_EVIDENCE_GAP: &str =
    "not verified against Grok CLI documentation or an installed binary; see compatibility docs";

/// How much a capability row is backed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvidenceStatus {
    /// Checked against the locally installed CLI's help (and, for models, its
    /// local catalog or model list).
    InstalledCli,
    /// Primary upstream documentation; the CLI was not installed locally.
    PrimaryDocs,
    /// Not established by primary docs or an installed binary.
    Unverified,
    /// The adapter has no effort concept.
    NotApplicable,
}

impl EvidenceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceStatus::InstalledCli => "installed-cli",
            EvidenceStatus::PrimaryDocs => "primary-docs",
            EvidenceStatus::Unverified => "unverified",
            EvidenceStatus::NotApplicable => "not-applicable",
        }
    }
}

impl std::fmt::Display for EvidenceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Provenance of a capability row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortEvidence {
    pub status: EvidenceStatus,
    /// The CLI version the row was qualified against.
    pub cli_version: &'static str,
    /// The sources, in short form (full citations: the goal's research notes
    /// and the compatibility docs).
    pub sources: &'static str,
    pub checked: &'static str,
}

/// The native mechanism that carries an effort request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortSelector {
    /// `<flag> <level>`, e.g. `--effort high`.
    Flag(&'static str),
    /// `-c <key>="<level>"` (Codex config override).
    ConfigOverride(&'static str),
    /// `<model>#<level>` (OpenCode v2 model variant suffix).
    ModelVariantSuffix,
    /// No generic effort is exposed for this adapter.
    Unsupported,
}

/// A model whose effort menu the table knows, by exact ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelEffortRow {
    /// Exact model ID, provider namespace included where the adapter uses one.
    /// Matched exactly: no prefix, substring or alias inference.
    pub model: &'static str,
    pub levels: &'static [ReasoningEffort],
}

/// What the table does with a model that has no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownModelPolicy {
    /// Accept the adapter's authorable set, with a capability-unknown warning.
    /// The resolved model, and so its menu, is not known to r-orch.
    SyntaxOnly,
    /// Refuse effort unless the model has a row (OpenCode: variant names are
    /// per model and an unknown variant is a model-resolution error).
    RequireKnownModel,
    /// The adapter exposes no generic effort at all.
    NotApplicable,
}

/// One adapter's effort capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortCapability {
    pub adapter: AdapterKind,
    /// Every spelling that may be authored for this adapter with some model.
    pub authorable: &'static [ReasoningEffort],
    pub selector: EffortSelector,
    /// Known models and their menus. A known model narrows `authorable`.
    pub models: &'static [ModelEffortRow],
    pub unknown_model: UnknownModelPolicy,
    pub evidence: EffortEvidence,
}

const CLAUDE_LEVELS: &[ReasoningEffort] = &[E::Low, E::Medium, E::High, E::Xhigh, E::Max];
const CLAUDE_4_6_LEVELS: &[ReasoningEffort] = &[E::Low, E::Medium, E::High, E::Max];
const CODEX_LEVELS: &[ReasoningEffort] = &[E::Low, E::Medium, E::High, E::Xhigh, E::Max, E::Ultra];
const CODEX_NO_ULTRA: &[ReasoningEffort] = &[E::Low, E::Medium, E::High, E::Xhigh, E::Max];
const OPENCODE_LEVELS: &[ReasoningEffort] =
    &[E::None, E::Low, E::Medium, E::High, E::Xhigh, E::Max];
const LMH: &[ReasoningEffort] = &[E::Low, E::Medium, E::High];
const PI_LEVELS: &[ReasoningEffort] = &[
    E::Off,
    E::Minimal,
    E::Low,
    E::Medium,
    E::High,
    E::Xhigh,
    E::Max,
];

/// The capability table, in [`crate::model_tiers::SUPPORTED_ADAPTERS`] order.
pub const EFFORT_CAPABILITIES: [EffortCapability; 8] = [
    EffortCapability {
        adapter: AdapterKind::Claude,
        authorable: CLAUDE_LEVELS,
        selector: EffortSelector::Flag("--effort"),
        // Claude Code falls back to the highest supported level at or below
        // the request on a known model; new authoring still refuses a level
        // the model's menu lacks rather than relying on that clamp.
        models: &[
            ModelEffortRow {
                model: "claude-opus-5-5",
                levels: CLAUDE_LEVELS,
            },
            ModelEffortRow {
                model: "claude-sonnet-5-5",
                levels: CLAUDE_LEVELS,
            },
            ModelEffortRow {
                model: "claude-haiku-5-5",
                levels: CLAUDE_LEVELS,
            },
            ModelEffortRow {
                model: "claude-opus-4-6",
                levels: CLAUDE_4_6_LEVELS,
            },
            ModelEffortRow {
                model: "claude-sonnet-4-6",
                levels: CLAUDE_4_6_LEVELS,
            },
        ],
        unknown_model: UnknownModelPolicy::SyntaxOnly,
        evidence: EffortEvidence {
            status: EvidenceStatus::InstalledCli,
            cli_version: "Claude Code 2.1.308",
            sources: "claude --help; code.claude.com/docs/en/model-config",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Opencode,
        authorable: OPENCODE_LEVELS,
        selector: EffortSelector::ModelVariantSuffix,
        models: &[
            ModelEffortRow {
                model: "openrouter/anthropic/claude-opus-5.5",
                levels: &[E::Low, E::Medium, E::High, E::Xhigh, E::Max],
            },
            ModelEffortRow {
                model: "openrouter/anthropic/claude-opus-4.6",
                levels: &[E::None, E::Low, E::Medium, E::High, E::Max],
            },
            ModelEffortRow {
                model: "openrouter/google/gemini-3.8-flash",
                levels: LMH,
            },
            // Catalog evidence only, not local availability; never a default.
            ModelEffortRow {
                model: "openrouter/google/gemini-2.5-flash",
                levels: &[E::None],
            },
        ],
        unknown_model: UnknownModelPolicy::RequireKnownModel,
        evidence: EffortEvidence {
            status: EvidenceStatus::InstalledCli,
            cli_version: "opencode v2.1.2 (major 2 only)",
            sources: "opencode run --help; opencode.ai/v2/docs/models; models.dev",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Codex,
        authorable: CODEX_LEVELS,
        selector: EffortSelector::ConfigOverride("model_reasoning_effort"),
        models: &[
            ModelEffortRow {
                model: "gpt-6.1-sol",
                levels: CODEX_LEVELS,
            },
            ModelEffortRow {
                model: "gpt-6-astra",
                levels: CODEX_LEVELS,
            },
            ModelEffortRow {
                model: "gpt-6-sol",
                levels: CODEX_LEVELS,
            },
            ModelEffortRow {
                model: "gpt-6-luna",
                levels: CODEX_NO_ULTRA,
            },
            ModelEffortRow {
                model: "gpt-5.6-sol",
                levels: CODEX_LEVELS,
            },
            ModelEffortRow {
                model: "gpt-5.6-terra",
                levels: CODEX_LEVELS,
            },
            ModelEffortRow {
                model: "gpt-5.6-luna",
                levels: CODEX_NO_ULTRA,
            },
        ],
        unknown_model: UnknownModelPolicy::SyntaxOnly,
        evidence: EffortEvidence {
            status: EvidenceStatus::InstalledCli,
            cli_version: "codex-cli 0.163.0",
            sources: "codex exec --help; local models cache; learn.chatgpt.com config reference",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Cursor,
        authorable: &[],
        selector: EffortSelector::Unsupported,
        models: &[],
        unknown_model: UnknownModelPolicy::NotApplicable,
        evidence: EffortEvidence {
            status: EvidenceStatus::Unverified,
            cli_version: "cursor-agent 2026.10.22 (not installed)",
            sources: "cursor.com/docs/cli; model-slug parameters only",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Pi,
        authorable: PI_LEVELS,
        selector: EffortSelector::Flag("--thinking"),
        models: &[],
        unknown_model: UnknownModelPolicy::SyntaxOnly,
        evidence: EffortEvidence {
            status: EvidenceStatus::PrimaryDocs,
            cli_version: "pi 1.4.0 (not installed)",
            sources: "earendil-works/pi v1.4.0 docs/cli.md, docs/rpc-commands.md",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Grok,
        authorable: LMH,
        selector: EffortSelector::Flag("--effort"),
        models: &[],
        unknown_model: UnknownModelPolicy::SyntaxOnly,
        evidence: EffortEvidence {
            status: EvidenceStatus::Unverified,
            cli_version: "Grok Build 1.0.67 (not installed)",
            sources: "docs.x.ai/build/cli/reference (no level list); legacy levels only",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Antigravity,
        authorable: LMH,
        selector: EffortSelector::Flag("--effort"),
        // Base names. `agy models` lists each as `<base>-<level>` slugs.
        models: &[
            ModelEffortRow {
                model: "gemini-4-argon",
                levels: LMH,
            },
            ModelEffortRow {
                model: "gemini-3.8-flash",
                levels: LMH,
            },
            ModelEffortRow {
                model: "claude-opus-5-5",
                levels: LMH,
            },
            ModelEffortRow {
                model: "claude-sonnet-5-5",
                levels: LMH,
            },
            ModelEffortRow {
                model: "gpt-oss-120b",
                levels: &[E::Medium],
            },
        ],
        unknown_model: UnknownModelPolicy::SyntaxOnly,
        evidence: EffortEvidence {
            status: EvidenceStatus::InstalledCli,
            cli_version: "agy 1.4.5",
            sources: "agy --help; agy models; antigravity.google/docs/cli/commands/effort",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
    EffortCapability {
        adapter: AdapterKind::Shell,
        authorable: &[],
        selector: EffortSelector::Unsupported,
        models: &[],
        unknown_model: UnknownModelPolicy::NotApplicable,
        evidence: EffortEvidence {
            status: EvidenceStatus::NotApplicable,
            cli_version: "-",
            sources: "shell agents run a configured command",
            checked: EFFORT_TABLE_CHECKED,
        },
    },
];

/// The capability row for `adapter`.
pub fn effort_capability(adapter: AdapterKind) -> &'static EffortCapability {
    EFFORT_CAPABILITIES
        .iter()
        .find(|c| c.adapter == adapter)
        .expect("EFFORT_CAPABILITIES has a row for every AdapterKind")
}

/// A generic effort choice as a command argument: `default` clears the stored
/// value (the provider or its config decides); any level sets it. `default`
/// is a command, never stored. Clearing never touches a native suffix written
/// into the model string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffortSelection {
    Default,
    Set(ReasoningEffort),
}

impl EffortSelection {
    /// The spelling that clears an agent's effort.
    pub const DEFAULT: &'static str = "default";

    /// The value to store in `AgentConfig::effort`.
    pub fn into_effort(self) -> Option<ReasoningEffort> {
        match self {
            EffortSelection::Default => None,
            EffortSelection::Set(level) => Some(level),
        }
    }
}

impl std::fmt::Display for EffortSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EffortSelection::Default => f.write_str(Self::DEFAULT),
            EffortSelection::Set(level) => level.fmt(f),
        }
    }
}

impl std::str::FromStr for EffortSelection {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == Self::DEFAULT {
            Ok(EffortSelection::Default)
        } else {
            s.parse().map(EffortSelection::Set)
        }
    }
}

// ── Diagnostics ─────────────────────────────────────────────────────────────

/// The request-only status vocabulary. These are diagnostic strings, not a
/// persisted schema; "effective" never appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffortStatus {
    /// Forwarded through the native selector; nothing is known about what the
    /// provider applies.
    RequestOnly,
    /// The provider documents clamping to the model's capability.
    ClampPossible,
    /// Local provider configuration can replace what the selector names.
    LocalOverridePossible,
    /// The resolved model, or how the request combines with it, is unknown.
    CapabilityUnknown,
    /// A legacy value is not forwarded for this dispatch.
    LegacyIgnored,
}

impl EffortStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffortStatus::RequestOnly => "request-only",
            EffortStatus::ClampPossible => "clamp-possible",
            EffortStatus::LocalOverridePossible => "local-override-possible",
            EffortStatus::CapabilityUnknown => "capability-unknown",
            EffortStatus::LegacyIgnored => "legacy-ignored",
        }
    }
}

impl std::fmt::Display for EffortStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The specific reason behind a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffortReason {
    /// Forwarded; effective reasoning is provider-defined.
    RequestOnly,
    /// Codex `ultra`: the spend/delegation disclosure.
    CodexUltra,
    /// Pi clamps `--thinking` to the model's support (a non-reasoning model
    /// runs `off`).
    PiClamp,
    /// Pi's explicit `--thinking` takes precedence over a `:level` suffix in
    /// the model string.
    PiFlagOverridesModelSuffix,
    /// OpenCode: a same-name local variant can replace the catalog variant.
    OpencodeLocalVariant,
    /// The model has no row: the request is syntax-checked only.
    UnknownModel,
    /// No model is configured: the provider's default model decides.
    DefaultModel,
    /// Codex with effort but no model: forwarding effort may bypass a managed
    /// new-thread model default, so Codex may select a different model.
    CodexManagedModelDefault,
    /// Antigravity model is already an effort-suffixed slug; how it combines
    /// with `--effort` is unresolved native precedence.
    AntigravitySuffixedModel,
    /// Legacy: this adapter exposes no generic effort.
    LegacyAdapterUnsupported,
    /// Legacy: OpenCode needs an explicit model with a known row.
    LegacyModelRequired,
    /// Legacy: the model's known menu lacks this level.
    LegacyLevelNotOnModel,
    /// Legacy: OpenCode model already carries a different `#variant`; the
    /// original model string is kept.
    LegacySuffixConflict,
    /// Legacy: the installed OpenCode binary did not prove the `#variant`
    /// syntax; the original model string is kept.
    LegacyWireCheckFailed,
}

/// One diagnostic about an effort request, for show/wizard/watch/run output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffortDiagnostic {
    pub status: EffortStatus,
    pub reason: EffortReason,
}

impl EffortDiagnostic {
    fn new(status: EffortStatus, reason: EffortReason) -> Self {
        EffortDiagnostic { status, reason }
    }

    /// Whether an operator should be warned. A plain request-only note is not
    /// a warning; everything else is.
    pub fn is_warning(&self) -> bool {
        self.reason != EffortReason::RequestOnly
    }

    pub fn message(&self) -> &'static str {
        match self.reason {
            EffortReason::RequestOnly => EFFORT_REQUEST_ONLY_NOTE,
            EffortReason::CodexUltra => CODEX_ULTRA_LABEL,
            EffortReason::PiClamp => {
                "pi clamps the requested thinking level to the model's support; a non-reasoning model runs with thinking off"
            }
            EffortReason::PiFlagOverridesModelSuffix => {
                "the explicit --thinking level takes precedence over the model's :level suffix"
            }
            EffortReason::OpencodeLocalVariant => {
                "a local opencode variant with the same name can replace the catalog variant's settings"
            }
            EffortReason::UnknownModel => {
                "the model is not in r-orch's effort table; only the CLI's level spelling was checked"
            }
            EffortReason::DefaultModel => {
                "no model is configured; the provider's default model decides which levels apply"
            }
            EffortReason::CodexManagedModelDefault => {
                "codex effort with no model: forwarding effort can bypass a managed new-thread model default, so codex may select a different model; set an explicit model to avoid this"
            }
            EffortReason::AntigravitySuffixedModel => {
                "the model is an effort-suffixed slug; how agy combines it with --effort is unresolved, prefer a base model name"
            }
            EffortReason::LegacyAdapterUnsupported => {
                "this adapter exposes no generic effort; the stored value is ignored (clear it with --effort default)"
            }
            EffortReason::LegacyModelRequired => {
                "opencode effort needs an explicit provider/model known to r-orch; the stored value is ignored (set a supported model or clear it with --effort default)"
            }
            EffortReason::LegacyLevelNotOnModel => {
                "the model does not offer this level; the stored value is ignored (choose a listed level or clear it with --effort default)"
            }
            EffortReason::LegacySuffixConflict => {
                "the model string already carries a different #variant; it is kept and the stored effort is ignored (clear it with --effort default)"
            }
            EffortReason::LegacyWireCheckFailed => {
                "the installed opencode did not prove provider/model#variant support; the original model is kept and the stored effort is ignored"
            }
        }
    }
}

impl std::fmt::Display for EffortDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message())
    }
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why an effort value is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffortRejection {
    /// The adapter name is not a supported adapter.
    UnknownAdapter,
    /// The adapter exposes no generic effort (Cursor, Shell).
    AdapterUnsupported,
    /// The spelling is not authorable for this adapter.
    LevelNotOnAdapter,
    /// Grok: outside the levels with evidence (lead ruling 6).
    GrokEvidenceGap,
    /// The known model's menu lacks the level.
    LevelNotOnModel,
    /// Codex `ultra` needs an explicit model from the known-supporting set.
    UltraNeedsKnownModel,
    /// OpenCode effort needs an explicit, known provider/model.
    ModelRequired,
    /// OpenCode model already names a different `#variant`.
    SuffixConflict,
    /// Antigravity model is an effort-suffixed slug; author a base model.
    SuffixedModel,
    /// The installed OpenCode binary did not prove the `#variant` syntax.
    WireCheckFailed,
}

impl EffortRejection {
    fn explanation(&self) -> &'static str {
        match self {
            EffortRejection::UnknownAdapter => "the adapter is not a supported adapter",
            EffortRejection::AdapterUnsupported => {
                "this adapter exposes no generic effort setting (evidence-limited; a native model slug is kept as written)"
            }
            EffortRejection::LevelNotOnAdapter => "the adapter's CLI does not offer this level",
            EffortRejection::GrokEvidenceGap => GROK_EVIDENCE_GAP,
            EffortRejection::LevelNotOnModel => "the model does not offer this level",
            EffortRejection::UltraNeedsKnownModel => {
                "ultra needs an explicit model known to support it (gpt-6.1-sol, gpt-6-astra, gpt-6-sol, gpt-5.6-sol, gpt-5.6-terra)"
            }
            EffortRejection::ModelRequired => {
                "opencode effort needs an explicit provider/model known to r-orch"
            }
            EffortRejection::SuffixConflict => {
                "the model string already carries a different #variant"
            }
            EffortRejection::SuffixedModel => {
                "the model is an effort-suffixed slug; use its base model name with --effort"
            }
            EffortRejection::WireCheckFailed => {
                "the installed opencode did not prove provider/model#variant support (only major version 2 is supported)"
            }
        }
    }
}

/// An effort value refused at authoring or dispatch. The caller adds the
/// agent identity; the message carries adapter, model, value, the allowed
/// list and the remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortError {
    pub adapter: String,
    pub model: Option<String>,
    pub value: ReasoningEffort,
    /// What may be authored for this adapter and model (possibly empty).
    pub allowed: Vec<ReasoningEffort>,
    pub rejection: EffortRejection,
}

impl std::fmt::Display for EffortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let model = self.model.as_deref().unwrap_or("<unset>");
        write!(
            f,
            "r-orch does not accept effort `{}` for adapter `{}` with model `{}`: {}. Allowed: ",
            self.value,
            self.adapter,
            model,
            self.rejection.explanation()
        )?;
        if self.allowed.is_empty() {
            f.write_str("none")?;
        } else {
            let allowed: Vec<&str> = self.allowed.iter().map(|e| e.as_str()).collect();
            f.write_str(&allowed.join(", "))?;
        }
        f.write_str(". Clear it with `--effort default`")?;
        if self.allowed.is_empty() {
            f.write_str(".")
        } else {
            f.write_str(" or choose an allowed level.")
        }
    }
}

impl std::error::Error for EffortError {}

// ── Model-string helpers ────────────────────────────────────────────────────

/// Split an OpenCode model string at its first `#`: `("provider/model",
/// Some("variant"))`. No `#` → `(model, None)`.
pub fn split_variant_suffix(model: &str) -> (&str, Option<&str>) {
    match model.split_once('#') {
        Some((base, variant)) => (base, Some(variant)),
        None => (model, None),
    }
}

fn model_row(capability: &EffortCapability, model: &str) -> Option<&'static ModelEffortRow> {
    capability.models.iter().find(|row| row.model == model)
}

/// Whether `model` is exactly one of the `<base>-<level>` slugs a known
/// Antigravity base publishes. Only known bases count: no general suffix
/// stripping.
fn is_antigravity_suffixed_slug(capability: &EffortCapability, model: &str) -> bool {
    capability.models.iter().any(|row| {
        row.levels.iter().any(|level| {
            model
                .strip_prefix(row.model)
                .and_then(|rest| rest.strip_prefix('-'))
                == Some(level.as_str())
        })
    })
}

fn non_empty(model: Option<&str>) -> Option<&str> {
    model.filter(|m| !m.is_empty())
}

/// The levels that may be authored for `adapter` with `model` (unset or empty
/// = provider default). Unknown adapter → empty.
pub fn allowed_efforts(adapter: &str, model: Option<&str>) -> Vec<ReasoningEffort> {
    let Ok(kind) = adapter.parse::<AdapterKind>() else {
        return Vec::new();
    };
    let capability = effort_capability(kind);
    let model = non_empty(model);
    match kind {
        AdapterKind::Opencode => model
            .map(|m| split_variant_suffix(m).0)
            .and_then(|base| model_row(capability, base))
            .map(|row| row.levels.to_vec())
            .unwrap_or_default(),
        _ => match model.and_then(|m| model_row(capability, m)) {
            Some(row) => row.levels.to_vec(),
            None if kind == AdapterKind::Codex => capability
                .authorable
                .iter()
                .copied()
                .filter(|e| *e != E::Ultra)
                .collect(),
            None => capability.authorable.to_vec(),
        },
    }
}

// ── Policy ──────────────────────────────────────────────────────────────────

/// How a forwarded request is spelled on the wire. The adapter builds argv
/// from this; for every adapter the level string is [`ReasoningEffort::as_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffortWire {
    /// `<flag> <level>`.
    Flag {
        flag: &'static str,
        level: ReasoningEffort,
    },
    /// `-c <key>="<level>"`.
    ConfigOverride {
        key: &'static str,
        level: ReasoningEffort,
    },
    /// Pass this model string instead of the configured one. It carries the
    /// `#<level>` suffix exactly once: appended, or already present.
    ModelVariant { model: String },
}

/// What a dispatch does with the stored effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffortDispatch {
    /// No effort stored: emit nothing and run no preflight. Argv is unchanged.
    Unset,
    /// Forward the request.
    Forward {
        wire: EffortWire,
        /// OpenCode: the installed binary's `#variant` syntax must be proven
        /// by the bounded preflight first; on failure use
        /// [`plan_after_wire_check_failure`].
        requires_wire_check: bool,
        diagnostics: Vec<EffortDiagnostic>,
    },
    /// Legacy value this adapter/model cannot carry: dispatch with effort
    /// cleared on the invocation-local config, model untouched, and warn.
    IgnoreLegacy { diagnostics: Vec<EffortDiagnostic> },
    /// Non-legacy invalid combination: configuration error before any model
    /// child is spawned.
    Reject(EffortError),
}

/// The outcome of checking one combination against the table.
enum Check {
    Accept {
        wire: EffortWire,
        requires_wire_check: bool,
        diagnostics: Vec<EffortDiagnostic>,
    },
    Refuse {
        rejection: EffortRejection,
        /// The diagnostic a legacy dispatch reports instead of failing.
        legacy: LegacyOutcome,
    },
}

/// What a legacy value does when authoring would refuse it.
enum LegacyOutcome {
    Ignore(EffortReason),
    /// Forward anyway (byte-identical argv or the Antigravity exception).
    Forward {
        wire: EffortWire,
        diagnostics: Vec<EffortDiagnostic>,
    },
}

fn diag(status: EffortStatus, reason: EffortReason) -> EffortDiagnostic {
    EffortDiagnostic::new(status, reason)
}

fn check(adapter: &str, model: Option<&str>, level: ReasoningEffort) -> Check {
    let Ok(kind) = adapter.parse::<AdapterKind>() else {
        return Check::Refuse {
            rejection: EffortRejection::UnknownAdapter,
            legacy: LegacyOutcome::Ignore(EffortReason::LegacyAdapterUnsupported),
        };
    };
    let capability = effort_capability(kind);
    let model = non_empty(model);
    let request_only = diag(EffortStatus::RequestOnly, EffortReason::RequestOnly);

    match capability.selector {
        EffortSelector::Unsupported => Check::Refuse {
            rejection: EffortRejection::AdapterUnsupported,
            legacy: LegacyOutcome::Ignore(EffortReason::LegacyAdapterUnsupported),
        },
        EffortSelector::ModelVariantSuffix => {
            let refuse = |rejection, reason| Check::Refuse {
                rejection,
                legacy: LegacyOutcome::Ignore(reason),
            };
            if !capability.authorable.contains(&level) {
                return refuse(
                    EffortRejection::LevelNotOnAdapter,
                    EffortReason::LegacyLevelNotOnModel,
                );
            }
            let Some(model) = model else {
                return refuse(
                    EffortRejection::ModelRequired,
                    EffortReason::LegacyModelRequired,
                );
            };
            let (base, suffix) = split_variant_suffix(model);
            let Some(row) = model_row(capability, base) else {
                return refuse(
                    EffortRejection::ModelRequired,
                    EffortReason::LegacyModelRequired,
                );
            };
            if suffix.is_some_and(|s| s != level.as_str()) {
                return refuse(
                    EffortRejection::SuffixConflict,
                    EffortReason::LegacySuffixConflict,
                );
            }
            if !row.levels.contains(&level) {
                return refuse(
                    EffortRejection::LevelNotOnModel,
                    EffortReason::LegacyLevelNotOnModel,
                );
            }
            let model = match suffix {
                Some(_) => model.to_owned(),
                None => format!("{base}#{level}"),
            };
            Check::Accept {
                wire: EffortWire::ModelVariant { model },
                requires_wire_check: true,
                diagnostics: vec![
                    request_only,
                    diag(
                        EffortStatus::LocalOverridePossible,
                        EffortReason::OpencodeLocalVariant,
                    ),
                ],
            }
        }
        EffortSelector::Flag(flag) => check_flag_adapter(
            kind,
            capability,
            model,
            level,
            EffortWire::Flag { flag, level },
            request_only,
        ),
        EffortSelector::ConfigOverride(key) => check_flag_adapter(
            kind,
            capability,
            model,
            level,
            EffortWire::ConfigOverride { key, level },
            request_only,
        ),
    }
}

fn check_flag_adapter(
    kind: AdapterKind,
    capability: &'static EffortCapability,
    model: Option<&str>,
    level: ReasoningEffort,
    wire: EffortWire,
    request_only: EffortDiagnostic,
) -> Check {
    // Claude, Pi and Grok forwarded low|medium|high before this table
    // existed; keep doing so byte-for-byte whatever the model (ruling 1).
    let legacy_passthrough = matches!(
        kind,
        AdapterKind::Claude | AdapterKind::Pi | AdapterKind::Grok
    );
    let refuse = |rejection, reason| Check::Refuse {
        rejection,
        legacy: if legacy_passthrough {
            LegacyOutcome::Forward {
                wire: wire.clone(),
                diagnostics: vec![request_only],
            }
        } else {
            LegacyOutcome::Ignore(reason)
        },
    };

    if !capability.authorable.contains(&level) {
        let rejection = if kind == AdapterKind::Grok {
            EffortRejection::GrokEvidenceGap
        } else {
            EffortRejection::LevelNotOnAdapter
        };
        return refuse(rejection, EffortReason::LegacyLevelNotOnModel);
    }

    let mut diagnostics = vec![request_only];
    if kind == AdapterKind::Antigravity
        && model.is_some_and(|m| is_antigravity_suffixed_slug(capability, m))
    {
        // New authoring asks for a base model; a stored legacy value is still
        // forwarded (ruling 2) and labelled.
        return Check::Refuse {
            rejection: EffortRejection::SuffixedModel,
            legacy: LegacyOutcome::Forward {
                wire,
                diagnostics: vec![
                    request_only,
                    diag(
                        EffortStatus::CapabilityUnknown,
                        EffortReason::AntigravitySuffixedModel,
                    ),
                ],
            },
        };
    }

    match model.and_then(|m| model_row(capability, m)) {
        Some(row) if !row.levels.contains(&level) => {
            return refuse(
                EffortRejection::LevelNotOnModel,
                EffortReason::LegacyLevelNotOnModel,
            );
        }
        Some(_) => {}
        None => {
            if level == E::Ultra {
                return refuse(
                    EffortRejection::UltraNeedsKnownModel,
                    EffortReason::LegacyLevelNotOnModel,
                );
            }
            let reason = match (kind, model) {
                (AdapterKind::Codex, None) => Some(EffortReason::CodexManagedModelDefault),
                // Pi's clamp disclosure below already covers an unknown model;
                // Grok has no model rows at all, its gap is the row's evidence.
                (AdapterKind::Pi | AdapterKind::Grok, _) => None,
                (_, None) => Some(EffortReason::DefaultModel),
                (_, Some(_)) => Some(EffortReason::UnknownModel),
            };
            if let Some(reason) = reason {
                diagnostics.push(diag(EffortStatus::CapabilityUnknown, reason));
            }
        }
    }

    if level == E::Ultra {
        diagnostics.push(diag(EffortStatus::RequestOnly, EffortReason::CodexUltra));
    }
    if kind == AdapterKind::Pi {
        diagnostics.push(diag(EffortStatus::ClampPossible, EffortReason::PiClamp));
        if model.is_some_and(|m| m.contains(':')) {
            diagnostics.push(diag(
                EffortStatus::RequestOnly,
                EffortReason::PiFlagOverridesModelSuffix,
            ));
        }
    }

    Check::Accept {
        wire,
        requires_wire_check: false,
        diagnostics,
    }
}

fn effort_error(
    adapter: &str,
    model: Option<&str>,
    value: ReasoningEffort,
    rejection: EffortRejection,
) -> EffortError {
    EffortError {
        adapter: adapter.to_owned(),
        model: model.map(str::to_owned),
        value,
        allowed: allowed_efforts(adapter, model),
        rejection,
    }
}

/// Strict validation of a final candidate definition, for every authoring
/// surface. Unset effort is always valid. Returns the diagnostics to show
/// (warnings stay warnings; they never become errors here).
///
/// This applies to legacy spellings too: an unrelated edit of an agent whose
/// stored legacy combination is invalid fails, with the `--effort default`
/// remedy, and the caller must leave the stored definition untouched.
pub fn validate_effort_authoring(
    adapter: &str,
    model: Option<&str>,
    effort: Option<ReasoningEffort>,
) -> Result<Vec<EffortDiagnostic>, EffortError> {
    let Some(level) = effort else {
        return Ok(Vec::new());
    };
    match check(adapter, model, level) {
        Check::Accept { diagnostics, .. } => Ok(diagnostics),
        Check::Refuse { rejection, .. } => Err(effort_error(adapter, model, level, rejection)),
    }
}

/// What a dispatch does with a stored effort.
///
/// - unset → [`EffortDispatch::Unset`] (no selector, no preflight);
/// - valid → [`EffortDispatch::Forward`];
/// - invalid legacy `low|medium|high` → forwarded unchanged on Claude, Pi and
///   Grok and for a suffixed Antigravity slug, else
///   [`EffortDispatch::IgnoreLegacy`]; never a failure;
/// - invalid new value → [`EffortDispatch::Reject`].
pub fn plan_effort_dispatch(
    adapter: &str,
    model: Option<&str>,
    effort: Option<ReasoningEffort>,
) -> EffortDispatch {
    let Some(level) = effort else {
        return EffortDispatch::Unset;
    };
    match check(adapter, model, level) {
        Check::Accept {
            wire,
            requires_wire_check,
            diagnostics,
        } => EffortDispatch::Forward {
            wire,
            requires_wire_check,
            diagnostics,
        },
        Check::Refuse { rejection, legacy } => {
            if !level.is_legacy() {
                return EffortDispatch::Reject(effort_error(adapter, model, level, rejection));
            }
            match legacy {
                LegacyOutcome::Ignore(reason) => EffortDispatch::IgnoreLegacy {
                    diagnostics: vec![diag(EffortStatus::LegacyIgnored, reason)],
                },
                LegacyOutcome::Forward { wire, diagnostics } => EffortDispatch::Forward {
                    wire,
                    requires_wire_check: false,
                    diagnostics,
                },
            }
        }
    }
}

/// The dispatch outcome when a required wire check (see
/// [`EffortDispatch::Forward::requires_wire_check`]) failed, timed out or was
/// inconclusive: a legacy value is ignored with the original model kept; a
/// new value is a configuration error before any model child.
pub fn plan_after_wire_check_failure(
    adapter: &str,
    model: Option<&str>,
    level: ReasoningEffort,
) -> EffortDispatch {
    if level.is_legacy() {
        EffortDispatch::IgnoreLegacy {
            diagnostics: vec![diag(
                EffortStatus::LegacyIgnored,
                EffortReason::LegacyWireCheckFailed,
            )],
        }
    } else {
        EffortDispatch::Reject(effort_error(
            adapter,
            model,
            level,
            EffortRejection::WireCheckFailed,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_tiers::SUPPORTED_ADAPTERS;

    fn authoring(
        adapter: &str,
        model: Option<&str>,
        level: ReasoningEffort,
    ) -> Result<Vec<EffortDiagnostic>, EffortError> {
        validate_effort_authoring(adapter, model, Some(level))
    }

    fn dispatch(adapter: &str, model: Option<&str>, level: ReasoningEffort) -> EffortDispatch {
        plan_effort_dispatch(adapter, model, Some(level))
    }

    fn rejection(adapter: &str, model: Option<&str>, level: ReasoningEffort) -> EffortRejection {
        authoring(adapter, model, level)
            .expect_err("authoring should refuse")
            .rejection
    }

    fn reasons(diagnostics: &[EffortDiagnostic]) -> Vec<EffortReason> {
        diagnostics.iter().map(|d| d.reason).collect()
    }

    fn forward_reasons(plan: &EffortDispatch) -> Vec<EffortReason> {
        match plan {
            EffortDispatch::Forward { diagnostics, .. } => reasons(diagnostics),
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    fn ignored_reason(plan: &EffortDispatch) -> EffortReason {
        match plan {
            EffortDispatch::IgnoreLegacy { diagnostics } => {
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0].status, EffortStatus::LegacyIgnored);
                diagnostics[0].reason
            }
            other => panic!("expected IgnoreLegacy, got {other:?}"),
        }
    }

    fn wire(plan: &EffortDispatch) -> &EffortWire {
        match plan {
            EffortDispatch::Forward { wire, .. } => wire,
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    /// Models that exercise each adapter's unset / known / unknown / suffixed
    /// paths.
    fn models_for(kind: AdapterKind) -> Vec<Option<&'static str>> {
        let mut models: Vec<Option<&'static str>> =
            vec![None, Some(""), Some("custom-gateway/model")];
        models.extend(effort_capability(kind).models.iter().map(|r| Some(r.model)));
        models.extend(match kind {
            AdapterKind::Opencode => vec![
                Some("openrouter/anthropic/claude-opus-5.5#high"),
                Some("openrouter/anthropic/claude-opus-5.5#thinking"),
            ],
            AdapterKind::Antigravity => {
                vec![Some("gemini-3.8-flash-high"), Some("gpt-oss-120b-medium")]
            }
            AdapterKind::Pi => vec![Some("openai-codex/gpt-6.1-sol:low")],
            AdapterKind::Claude => vec![Some("opus"), Some("claude-haiku-4-5")],
            AdapterKind::Cursor => vec![Some("auto"), Some("claude-opus-4-8[effort=high]")],
            _ => vec![],
        });
        models
    }

    #[test]
    fn the_table_has_one_row_per_adapter_in_display_order() {
        let adapters: Vec<AdapterKind> = EFFORT_CAPABILITIES.iter().map(|c| c.adapter).collect();
        assert_eq!(adapters, SUPPORTED_ADAPTERS.to_vec());
        for capability in &EFFORT_CAPABILITIES {
            assert_eq!(capability.evidence.checked, "2026-10-22");
            assert!(!capability.evidence.cli_version.is_empty());
            for row in capability.models {
                for level in row.levels {
                    assert!(
                        capability.authorable.contains(level),
                        "{} row {} lists {level} outside the authorable set",
                        capability.adapter,
                        row.model
                    );
                }
            }
        }
    }

    #[test]
    fn authorable_sets_match_the_approved_contract() {
        let set = |kind| effort_capability(kind).authorable.to_vec();
        assert_eq!(
            set(AdapterKind::Claude),
            vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max]
        );
        assert_eq!(
            set(AdapterKind::Codex),
            vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max, E::Ultra]
        );
        assert_eq!(
            set(AdapterKind::Opencode),
            vec![E::None, E::Low, E::Medium, E::High, E::Xhigh, E::Max]
        );
        assert_eq!(
            set(AdapterKind::Antigravity),
            vec![E::Low, E::Medium, E::High]
        );
        assert_eq!(
            set(AdapterKind::Pi),
            vec![
                E::Off,
                E::Minimal,
                E::Low,
                E::Medium,
                E::High,
                E::Xhigh,
                E::Max
            ]
        );
        assert_eq!(set(AdapterKind::Grok), vec![E::Low, E::Medium, E::High]);
        assert_eq!(set(AdapterKind::Cursor), vec![]);
        assert_eq!(set(AdapterKind::Shell), vec![]);
    }

    #[test]
    fn evidence_status_is_honest_per_adapter() {
        let status = |kind| effort_capability(kind).evidence.status;
        assert_eq!(status(AdapterKind::Grok), EvidenceStatus::Unverified);
        assert_eq!(status(AdapterKind::Cursor), EvidenceStatus::Unverified);
        assert_eq!(status(AdapterKind::Pi), EvidenceStatus::PrimaryDocs);
        assert_eq!(status(AdapterKind::Shell), EvidenceStatus::NotApplicable);
        for kind in [
            AdapterKind::Claude,
            AdapterKind::Codex,
            AdapterKind::Opencode,
            AdapterKind::Antigravity,
        ] {
            assert_eq!(status(kind), EvidenceStatus::InstalledCli, "{kind}");
        }
    }

    #[test]
    fn the_status_vocabulary_is_request_only() {
        let all = [
            EffortStatus::RequestOnly,
            EffortStatus::ClampPossible,
            EffortStatus::LocalOverridePossible,
            EffortStatus::CapabilityUnknown,
            EffortStatus::LegacyIgnored,
        ];
        let strings: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            strings,
            [
                "request-only",
                "clamp-possible",
                "local-override-possible",
                "capability-unknown",
                "legacy-ignored"
            ]
        );
    }

    /// The cross product of adapter × model × level: the invariants that tie
    /// authoring and dispatch together.
    #[test]
    fn authoring_and_dispatch_agree_across_the_full_matrix() {
        for kind in SUPPORTED_ADAPTERS {
            let adapter = kind.as_str();
            for model in models_for(kind) {
                for level in ReasoningEffort::ALL {
                    let authored = authoring(adapter, model, level);
                    let planned = dispatch(adapter, model, level);
                    let case = format!("{adapter} / {model:?} / {level}");
                    match (&authored, &planned) {
                        // A valid authoring is forwarded with the same diagnostics.
                        (Ok(diagnostics), EffortDispatch::Forward { diagnostics: d, .. }) => {
                            assert_eq!(diagnostics, d, "{case}")
                        }
                        (Ok(_), other) => panic!("{case}: authored but planned {other:?}"),
                        // Legacy never fails a dispatch.
                        (Err(_), EffortDispatch::Reject(_)) => {
                            assert!(
                                !level.is_legacy(),
                                "{case}: legacy value rejected at dispatch"
                            )
                        }
                        (
                            Err(_),
                            EffortDispatch::Forward { .. } | EffortDispatch::IgnoreLegacy { .. },
                        ) => {
                            assert!(level.is_legacy(), "{case}: invalid new value dispatched")
                        }
                        (Err(_), EffortDispatch::Unset) => {
                            panic!("{case}: unset for a stored value")
                        }
                    }
                    if let EffortDispatch::Forward { diagnostics, .. } = &planned {
                        assert_eq!(
                            diagnostics.first().map(|d| d.reason),
                            Some(EffortReason::RequestOnly),
                            "{case}: a forwarded request is always labelled request-only"
                        );
                        assert!(
                            diagnostics
                                .iter()
                                .all(|d| d.status != EffortStatus::LegacyIgnored),
                            "{case}"
                        );
                    }
                    if let Err(error) = &authored {
                        assert_eq!(error.allowed, allowed_efforts(adapter, model), "{case}");
                        assert!(
                            !error.allowed.contains(&level)
                                || matches!(
                                    error.rejection,
                                    EffortRejection::SuffixConflict
                                        | EffortRejection::SuffixedModel
                                ),
                            "{case}: refused a level the allowed list offers"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unset_effort_needs_nothing_on_any_adapter() {
        for kind in SUPPORTED_ADAPTERS {
            for model in models_for(kind) {
                assert_eq!(
                    validate_effort_authoring(kind.as_str(), model, None),
                    Ok(Vec::new())
                );
                assert_eq!(
                    plan_effort_dispatch(kind.as_str(), model, None),
                    EffortDispatch::Unset
                );
            }
        }
    }

    #[test]
    fn no_diagnostic_claims_a_confirmed_effective_level() {
        for kind in SUPPORTED_ADAPTERS {
            for model in models_for(kind) {
                for level in ReasoningEffort::ALL {
                    let diagnostics = match dispatch(kind.as_str(), model, level) {
                        EffortDispatch::Forward { diagnostics, .. }
                        | EffortDispatch::IgnoreLegacy { diagnostics } => diagnostics,
                        _ => Vec::new(),
                    };
                    for d in diagnostics {
                        let text = d.to_string().to_lowercase();
                        assert!(
                            !text.contains("confirmed") && !text.contains("verified effective"),
                            "{text}"
                        );
                    }
                }
            }
        }
    }

    // ── Claude ──

    #[test]
    fn claude_known_models_refine_the_menu() {
        assert_eq!(
            rejection("claude", Some("claude-opus-4-6"), ReasoningEffort::Xhigh),
            EffortRejection::LevelNotOnModel
        );
        assert!(authoring("claude", Some("claude-opus-4-6"), ReasoningEffort::Max).is_ok());
        assert_eq!(
            reasons(&authoring("claude", Some("claude-opus-5-5"), ReasoningEffort::Xhigh).unwrap()),
            vec![EffortReason::RequestOnly]
        );
        // A hand-edited new value the known model lacks fails before spawn.
        assert!(matches!(
            dispatch("claude", Some("claude-opus-4-6"), ReasoningEffort::Xhigh),
            EffortDispatch::Reject(_)
        ));
    }

    #[test]
    fn claude_aliases_and_unset_models_are_syntax_only_not_guessed() {
        // "opus" is not prefix-matched to any Opus row.
        assert_eq!(
            reasons(&authoring("claude", Some("opus"), ReasoningEffort::Max).unwrap()),
            vec![EffortReason::RequestOnly, EffortReason::UnknownModel]
        );
        assert_eq!(
            reasons(&authoring("claude", None, ReasoningEffort::High).unwrap()),
            vec![EffortReason::RequestOnly, EffortReason::DefaultModel]
        );
        for level in [
            ReasoningEffort::None,
            ReasoningEffort::Off,
            ReasoningEffort::Minimal,
            ReasoningEffort::Ultra,
        ] {
            assert_eq!(
                rejection("claude", Some("claude-opus-5-5"), level),
                EffortRejection::LevelNotOnAdapter
            );
        }
    }

    #[test]
    fn claude_pi_and_grok_forward_legacy_values_through_the_existing_flags() {
        for (adapter, flag) in [
            ("claude", "--effort"),
            ("pi", "--thinking"),
            ("grok", "--effort"),
        ] {
            for level in ReasoningEffort::LEGACY {
                for model in [None, Some("anything-at-all")] {
                    assert_eq!(
                        wire(&dispatch(adapter, model, level)),
                        &EffortWire::Flag { flag, level },
                        "{adapter} {model:?} {level}"
                    );
                }
            }
        }
    }

    // ── Codex ──

    #[test]
    fn codex_ultra_is_explicit_and_needs_a_known_supporting_model() {
        for model in [
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
        ] {
            let diagnostics = authoring("codex", Some(model), ReasoningEffort::Ultra).unwrap();
            assert_eq!(
                reasons(&diagnostics),
                vec![EffortReason::RequestOnly, EffortReason::CodexUltra],
                "{model}"
            );
            assert!(diagnostics[1].to_string().contains(CODEX_ULTRA_LABEL));
        }
        assert_eq!(
            rejection("codex", None, ReasoningEffort::Ultra),
            EffortRejection::UltraNeedsKnownModel
        );
        assert_eq!(
            rejection("codex", Some("gpt-7-preview"), ReasoningEffort::Ultra),
            EffortRejection::UltraNeedsKnownModel
        );
        assert_eq!(
            rejection("codex", Some("gpt-6-luna"), ReasoningEffort::Ultra),
            EffortRejection::LevelNotOnModel
        );
        assert!(matches!(
            dispatch("codex", None, ReasoningEffort::Ultra),
            EffortDispatch::Reject(_)
        ));
        assert_eq!(
            CODEX_ULTRA_LABEL,
            "maximum reasoning with provider-internal sub-agent delegation; multiplies spend"
        );
    }

    #[test]
    fn codex_does_not_offer_no_thinking_levels() {
        for level in [
            ReasoningEffort::None,
            ReasoningEffort::Off,
            ReasoningEffort::Minimal,
        ] {
            assert_eq!(
                rejection("codex", Some("gpt-6.1-sol"), level),
                EffortRejection::LevelNotOnAdapter
            );
        }
    }

    #[test]
    fn codex_effort_without_a_model_is_allowed_with_the_managed_default_warning() {
        let plan = dispatch("codex", None, ReasoningEffort::High);
        assert_eq!(
            wire(&plan),
            &EffortWire::ConfigOverride {
                key: "model_reasoning_effort",
                level: ReasoningEffort::High
            }
        );
        assert_eq!(
            forward_reasons(&plan),
            vec![
                EffortReason::RequestOnly,
                EffortReason::CodexManagedModelDefault
            ]
        );
        assert_eq!(
            reasons(&authoring("codex", None, ReasoningEffort::Xhigh).unwrap()),
            vec![
                EffortReason::RequestOnly,
                EffortReason::CodexManagedModelDefault
            ]
        );
        assert_eq!(
            reasons(&authoring("codex", Some("my-proxy-model"), ReasoningEffort::Max).unwrap()),
            vec![EffortReason::RequestOnly, EffortReason::UnknownModel]
        );
    }

    // ── OpenCode ──

    #[test]
    fn opencode_synthesizes_the_variant_suffix_once_and_needs_the_wire_check() {
        let plan = dispatch(
            "opencode",
            Some("openrouter/anthropic/claude-opus-5.5"),
            ReasoningEffort::High,
        );
        match &plan {
            EffortDispatch::Forward {
                wire,
                requires_wire_check,
                diagnostics,
            } => {
                assert_eq!(
                    wire,
                    &EffortWire::ModelVariant {
                        model: "openrouter/anthropic/claude-opus-5.5#high".into()
                    }
                );
                assert!(requires_wire_check);
                assert_eq!(
                    reasons(diagnostics),
                    vec![
                        EffortReason::RequestOnly,
                        EffortReason::OpencodeLocalVariant
                    ]
                );
                assert_eq!(diagnostics[1].status, EffortStatus::LocalOverridePossible);
            }
            other => panic!("{other:?}"),
        }
        // A matching suffix is retained once, not doubled.
        assert_eq!(
            wire(&dispatch(
                "opencode",
                Some("openrouter/anthropic/claude-opus-5.5#high"),
                ReasoningEffort::High
            )),
            &EffortWire::ModelVariant {
                model: "openrouter/anthropic/claude-opus-5.5#high".into()
            }
        );
        assert_eq!(
            wire(&dispatch(
                "opencode",
                Some("openrouter/anthropic/claude-opus-4.6"),
                ReasoningEffort::None
            )),
            &EffortWire::ModelVariant {
                model: "openrouter/anthropic/claude-opus-4.6#none".into()
            }
        );
    }

    #[test]
    fn opencode_conflicting_suffix_is_an_authoring_error_and_a_legacy_ignore() {
        let model = Some("openrouter/anthropic/claude-opus-5.5#high");
        assert_eq!(
            rejection("opencode", model, ReasoningEffort::Low),
            EffortRejection::SuffixConflict
        );
        assert_eq!(
            ignored_reason(&dispatch("opencode", model, ReasoningEffort::Low)),
            EffortReason::LegacySuffixConflict
        );
        assert!(matches!(
            dispatch("opencode", model, ReasoningEffort::Max),
            EffortDispatch::Reject(_)
        ));
        // An opaque native selector is fine while generic effort is unset.
        let thinking = Some("openrouter/anthropic/claude-opus-5.5#thinking");
        assert_eq!(
            validate_effort_authoring("opencode", thinking, None),
            Ok(Vec::new())
        );
        assert_eq!(
            rejection("opencode", thinking, ReasoningEffort::High),
            EffortRejection::SuffixConflict
        );
    }

    #[test]
    fn opencode_needs_an_explicit_known_model() {
        for model in [None, Some(""), Some("openrouter/acme/new-model")] {
            assert_eq!(
                rejection("opencode", model, ReasoningEffort::High),
                EffortRejection::ModelRequired,
                "{model:?}"
            );
            assert_eq!(
                ignored_reason(&dispatch("opencode", model, ReasoningEffort::High)),
                EffortReason::LegacyModelRequired
            );
            assert!(matches!(
                dispatch("opencode", model, ReasoningEffort::Xhigh),
                EffortDispatch::Reject(_)
            ));
        }
        let flash = Some("openrouter/google/gemini-2.5-flash");
        assert_eq!(
            rejection("opencode", flash, ReasoningEffort::High),
            EffortRejection::LevelNotOnModel
        );
        assert_eq!(
            ignored_reason(&dispatch("opencode", flash, ReasoningEffort::High)),
            EffortReason::LegacyLevelNotOnModel
        );
        assert_eq!(
            rejection(
                "opencode",
                Some("openrouter/anthropic/claude-opus-5.5"),
                ReasoningEffort::Minimal
            ),
            EffortRejection::LevelNotOnAdapter
        );
    }

    #[test]
    fn a_failed_wire_check_ignores_legacy_and_rejects_new_values() {
        let model = Some("openrouter/anthropic/claude-opus-5.5");
        assert_eq!(
            ignored_reason(&plan_after_wire_check_failure(
                "opencode",
                model,
                ReasoningEffort::Medium
            )),
            EffortReason::LegacyWireCheckFailed
        );
        match plan_after_wire_check_failure("opencode", model, ReasoningEffort::Xhigh) {
            EffortDispatch::Reject(error) => {
                assert_eq!(error.rejection, EffortRejection::WireCheckFailed)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_opencode_requires_a_wire_check() {
        for kind in SUPPORTED_ADAPTERS {
            for model in models_for(kind) {
                for level in ReasoningEffort::ALL {
                    if let EffortDispatch::Forward {
                        requires_wire_check,
                        ..
                    } = dispatch(kind.as_str(), model, level)
                    {
                        assert_eq!(
                            requires_wire_check,
                            kind == AdapterKind::Opencode,
                            "{kind} {model:?} {level}"
                        );
                    }
                }
            }
        }
    }

    // ── Antigravity ──

    #[test]
    fn antigravity_base_models_use_effort_and_suffixed_slugs_are_legacy_only() {
        assert_eq!(
            wire(&dispatch(
                "antigravity",
                Some("gemini-3.8-flash"),
                ReasoningEffort::Low
            )),
            &EffortWire::Flag {
                flag: "--effort",
                level: ReasoningEffort::Low
            }
        );
        let slug = Some("gemini-3.8-flash-high");
        assert_eq!(
            rejection("antigravity", slug, ReasoningEffort::Low),
            EffortRejection::SuffixedModel
        );
        assert_eq!(
            forward_reasons(&dispatch("antigravity", slug, ReasoningEffort::Low)),
            vec![
                EffortReason::RequestOnly,
                EffortReason::AntigravitySuffixedModel
            ]
        );
        for level in [ReasoningEffort::Xhigh, ReasoningEffort::Max] {
            assert_eq!(
                rejection("antigravity", Some("gemini-4-argon"), level),
                EffortRejection::LevelNotOnAdapter
            );
        }
    }

    #[test]
    fn antigravity_gpt_oss_accepts_medium_only() {
        assert!(authoring("antigravity", Some("gpt-oss-120b"), ReasoningEffort::Medium).is_ok());
        assert_eq!(
            rejection("antigravity", Some("gpt-oss-120b"), ReasoningEffort::High),
            EffortRejection::LevelNotOnModel
        );
        assert_eq!(
            ignored_reason(&dispatch(
                "antigravity",
                Some("gpt-oss-120b"),
                ReasoningEffort::High
            )),
            EffortReason::LegacyLevelNotOnModel
        );
    }

    // ── Pi ──

    #[test]
    fn pi_always_discloses_clamping_and_flag_precedence() {
        assert_eq!(
            reasons(
                &authoring("pi", Some("openai-codex/gpt-6.1-sol"), ReasoningEffort::Off).unwrap()
            ),
            vec![EffortReason::RequestOnly, EffortReason::PiClamp]
        );
        assert_eq!(
            reasons(
                &authoring(
                    "pi",
                    Some("openai-codex/gpt-6.1-sol:low"),
                    ReasoningEffort::High
                )
                .unwrap()
            ),
            vec![
                EffortReason::RequestOnly,
                EffortReason::PiClamp,
                EffortReason::PiFlagOverridesModelSuffix
            ]
        );
        // No none→off alias.
        assert_eq!(
            rejection("pi", None, ReasoningEffort::None),
            EffortRejection::LevelNotOnAdapter
        );
        assert_eq!(
            rejection("pi", None, ReasoningEffort::Ultra),
            EffortRejection::LevelNotOnAdapter
        );
    }

    // ── Grok ──

    #[test]
    fn grok_new_values_report_the_evidence_gap_not_provider_invalidity() {
        for level in [
            ReasoningEffort::None,
            ReasoningEffort::Off,
            ReasoningEffort::Minimal,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
            ReasoningEffort::Ultra,
        ] {
            let error = authoring("grok", Some("grok-build"), level).unwrap_err();
            assert_eq!(error.rejection, EffortRejection::GrokEvidenceGap);
            let text = error.to_string();
            assert!(text.contains(GROK_EVIDENCE_GAP), "{text}");
            assert!(
                !text.contains("invalid") && !text.contains("unsupported by"),
                "{text}"
            );
            assert!(matches!(
                dispatch("grok", Some("grok-build"), level),
                EffortDispatch::Reject(_)
            ));
        }
        assert_eq!(
            reasons(&authoring("grok", Some("grok-build"), ReasoningEffort::Medium).unwrap()),
            vec![EffortReason::RequestOnly]
        );
    }

    // ── Cursor, Shell, unknown adapters ──

    #[test]
    fn adapters_without_generic_effort_refuse_authoring_and_ignore_legacy() {
        for adapter in ["cursor", "shell", "not-an-adapter"] {
            let expected = if adapter == "not-an-adapter" {
                EffortRejection::UnknownAdapter
            } else {
                EffortRejection::AdapterUnsupported
            };
            assert_eq!(
                rejection(adapter, Some("auto"), ReasoningEffort::High),
                expected
            );
            assert_eq!(
                ignored_reason(&dispatch(adapter, Some("auto"), ReasoningEffort::High)),
                EffortReason::LegacyAdapterUnsupported
            );
            assert!(matches!(
                dispatch(adapter, None, ReasoningEffort::Max),
                EffortDispatch::Reject(_)
            ));
        }
    }

    // ── Selection and errors ──

    #[test]
    fn default_is_a_clearing_command_not_a_level() {
        assert_eq!(
            "default".parse::<EffortSelection>(),
            Ok(EffortSelection::Default)
        );
        assert_eq!(
            "xhigh".parse::<EffortSelection>(),
            Ok(EffortSelection::Set(ReasoningEffort::Xhigh))
        );
        assert_eq!("default".parse::<ReasoningEffort>(), Err(()));
        assert!(serde_json::from_str::<ReasoningEffort>("\"default\"").is_err());
        assert_eq!("inherit".parse::<EffortSelection>(), Err(()));
        assert_eq!("High".parse::<EffortSelection>(), Err(()));
        assert_eq!(EffortSelection::Default.into_effort(), None);
        assert_eq!(
            EffortSelection::Set(ReasoningEffort::Max).into_effort(),
            Some(ReasoningEffort::Max)
        );
        for selection in [
            EffortSelection::Default,
            EffortSelection::Set(ReasoningEffort::Ultra),
        ] {
            assert_eq!(
                selection.to_string().parse::<EffortSelection>(),
                Ok(selection)
            );
        }
    }

    #[test]
    fn an_effort_error_names_the_combination_the_allowed_list_and_the_remedy() {
        let error =
            authoring("claude", Some("claude-opus-4-6"), ReasoningEffort::Xhigh).unwrap_err();
        assert_eq!(
            error.to_string(),
            "r-orch does not accept effort `xhigh` for adapter `claude` with model `claude-opus-4-6`: \
             the model does not offer this level. Allowed: low, medium, high, max. \
             Clear it with `--effort default` or choose an allowed level."
        );
        let error = authoring("opencode", None, ReasoningEffort::High).unwrap_err();
        assert_eq!(
            error.to_string(),
            "r-orch does not accept effort `high` for adapter `opencode` with model `<unset>`: \
             opencode effort needs an explicit provider/model known to r-orch. Allowed: none. \
             Clear it with `--effort default`."
        );
    }

    #[test]
    fn allowed_efforts_narrow_by_known_model() {
        assert_eq!(
            allowed_efforts("codex", None),
            vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max]
        );
        assert_eq!(
            allowed_efforts("codex", Some("gpt-6-astra")),
            vec![E::Low, E::Medium, E::High, E::Xhigh, E::Max, E::Ultra]
        );
        assert_eq!(
            allowed_efforts("opencode", Some("openrouter/google/gemini-3.8-flash#low")),
            vec![E::Low, E::Medium, E::High]
        );
        assert_eq!(allowed_efforts("opencode", None), vec![]);
        assert_eq!(
            allowed_efforts("antigravity", Some("gpt-oss-120b")),
            vec![E::Medium]
        );
        assert_eq!(allowed_efforts("cursor", Some("auto")), vec![]);
        assert_eq!(allowed_efforts("nope", None), vec![]);
    }

    #[test]
    fn split_variant_suffix_splits_at_the_first_hash() {
        assert_eq!(split_variant_suffix("p/m#high"), ("p/m", Some("high")));
        assert_eq!(split_variant_suffix("p/m"), ("p/m", None));
        assert_eq!(split_variant_suffix("p/m#a#b"), ("p/m", Some("a#b")));
    }

    #[test]
    fn diagnostics_render_status_then_message() {
        let d = EffortDiagnostic::new(
            EffortStatus::LegacyIgnored,
            EffortReason::LegacyAdapterUnsupported,
        );
        assert!(d.is_warning());
        assert!(
            d.to_string()
                .starts_with("legacy-ignored: this adapter exposes no generic effort")
        );
        let note = EffortDiagnostic::new(EffortStatus::RequestOnly, EffortReason::RequestOnly);
        assert!(!note.is_warning());
        assert_eq!(
            note.to_string(),
            format!("request-only: {EFFORT_REQUEST_ONLY_NOTE}")
        );
    }
}
