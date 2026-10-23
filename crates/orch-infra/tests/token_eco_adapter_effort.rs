//! T1 / D5-3, D5-4 (argv side), D5-5, D5-11, D5-12 (adapter side): the native
//! effort request exactly as each adapter puts it on the wire, driven through
//! the public adapters and a scripted real child.
//!
//! The expectations are written out from design-v3 D5.1 (the per-adapter
//! table) and D5.4, **not** derived from `orch_domain`'s table, so a table
//! edit that silently widens support fails here.
//!
//! "Rejected" always means: `execute` returned an error **and** the process
//! manager recorded no spawn (nothing ran). "Forwarded" means exactly one
//! spawn carrying exactly one occurrence of the selector.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, AgentEventKind};
use orch_core::process::ProcessManager;
use orch_domain::{
    AgentConfig, EffortDispatch, PromptMode, ReasoningEffort as E, plan_effort_dispatch,
};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter, effort_diagnostics,
};
use support::{ScriptedProcessManager, SpawnRecord, collect, fixed_clock, params};

fn build(kind: &str, pm: Arc<dyn ProcessManager>) -> Box<dyn Adapter> {
    match kind {
        "claude" => Box::new(ClaudeAdapter::new(pm).with_clock(fixed_clock())),
        "codex" => Box::new(CodexAdapter::new(pm).with_clock(fixed_clock())),
        "cursor" => Box::new(CursorAdapter::new(pm).with_clock(fixed_clock())),
        "shell" => Box::new(ShellAdapter::new(pm).with_clock(fixed_clock())),
        "opencode" => Box::new(OpenCodeAdapter::new(pm).with_clock(fixed_clock())),
        "pi" => Box::new(PiAdapter::new(pm).with_clock(fixed_clock())),
        "grok" => Box::new(GrokAdapter::new(pm).with_clock(fixed_clock())),
        "antigravity" => Box::new(AntigravityAdapter::new(pm).with_clock(fixed_clock())),
        other => panic!("unknown adapter {other}"),
    }
}

const STATUSES: [&str; 5] = [
    "request-only",
    "clamp-possible",
    "local-override-possible",
    "capability-unknown",
    "legacy-ignored",
];

struct Outcome {
    /// `Err(message)` when `execute` refused.
    result: Result<SpawnRecord, String>,
    spawn_count: usize,
    /// Effort diagnostics from the pure pre-execution surface the engine
    /// uses (review-b2 F1, ruling 8): `effort_diagnostics` for the seven
    /// non-OpenCode adapters, `prepare_effort` for OpenCode.
    notes: Vec<String>,
    /// Effort diagnostics that leaked into the provider event stream as
    /// `Output` events. Must always be empty.
    stream_notes: Vec<String>,
}

impl Outcome {
    fn argv(&self) -> &[String] {
        &self.result.as_ref().expect("forwarded").args
    }
    fn count(&self, needle: &str) -> usize {
        self.argv().iter().filter(|a| *a == needle).count()
    }
    fn error(&self) -> &str {
        self.result.as_ref().expect_err("rejected")
    }
    fn assert_rejected_before_spawn(&self) {
        assert!(self.result.is_err(), "expected a pre-spawn rejection");
        assert_eq!(self.spawn_count, 0, "a rejected request must spawn nothing");
    }
    fn note_with(&self, status: &str) -> Option<&String> {
        self.notes
            .iter()
            .find(|n| n.starts_with(&format!("{status}:")))
    }
}

/// Effort diagnostics for `config` exactly as the engine obtains them before
/// `execute`: never from the event stream.
async fn pre_execute_notes(
    kind: &str,
    adapter: &dyn Adapter,
    p: &orch_core::ExecuteParams,
) -> Vec<String> {
    if kind != "opencode" {
        return effort_diagnostics(kind, &p.config).unwrap_or_default();
    }
    // OpenCode: only the statically resolved outcomes are reachable here. A
    // forwarded `#level` needs the bounded wire probe, which these argv tests
    // deliberately do not run (token_eco_adapter_probe.rs owns it).
    match plan_effort_dispatch(kind, p.config.model.as_deref(), p.config.effort) {
        EffortDispatch::Unset | EffortDispatch::IgnoreLegacy { .. } => adapter
            .prepare_effort(p)
            .await
            .map(|prepared| prepared.diagnostics)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn run(kind: &str, config: AgentConfig) -> Outcome {
    let pm = ScriptedProcessManager::silent();
    let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
    let mut p = params("the user prompt", "/ws");
    p.system_prompt = Some("the system prompt".to_owned());
    p.config = AgentConfig {
        command: Some("npm test".to_owned()),
        ..config
    };
    let notes = pre_execute_notes(kind, adapter.as_ref(), &p).await;
    assert!(
        pm.spawns().is_empty(),
        "reading the diagnostics must spawn nothing"
    );
    match adapter.execute(p) {
        Err(e) => Outcome {
            result: Err(e.message),
            spawn_count: pm.spawns().len(),
            notes: Vec::new(),
            stream_notes: Vec::new(),
        },
        Ok(handle) => {
            let collected = collect(handle).await;
            let stream_notes = collected
                .of_kind(AgentEventKind::Output)
                .into_iter()
                .filter_map(|e| e.data.get("text").and_then(|t| t.as_str()))
                .filter(|t| STATUSES.iter().any(|s| t.starts_with(&format!("{s}:"))))
                .map(str::to_owned)
                .collect();
            Outcome {
                result: Ok(pm.spawn_record()),
                spawn_count: pm.spawns().len(),
                notes,
                stream_notes,
            }
        }
    }
}

fn cfg(model: Option<&str>, effort: Option<E>) -> AgentConfig {
    AgentConfig {
        model: model.map(str::to_owned),
        effort,
        ..Default::default()
    }
}

fn codex_pair(level: &str) -> String {
    format!("model_reasoning_effort=\"{level}\"")
}

// ── D5-3: Codex ─────────────────────────────────────────────────────────────

const CODEX_ULTRA_ROWS: [&str; 5] = [
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
];

#[tokio::test]
async fn codex_forwards_each_authorable_level_as_one_config_override() {
    for level in [E::Low, E::Medium, E::High, E::Xhigh, E::Max] {
        let o = run("codex", cfg(Some("gpt-6.1-sol"), Some(level))).await;
        let sel = codex_pair(level.as_str());
        assert_eq!(o.count(&sel), 1, "{level}: {:?}", o.argv());
        let at = o.argv().iter().position(|a| *a == sel).unwrap();
        assert_eq!(o.argv()[at - 1], "-c", "{level}: selector must follow -c");
        assert_eq!(o.count("-c"), 1, "{level}: exactly one -c");
        assert_eq!(o.argv().last().unwrap(), "-", "prompt stays on stdin");
    }
}

#[tokio::test]
async fn codex_ultra_is_forwarded_only_for_the_exact_known_rows() {
    for model in CODEX_ULTRA_ROWS {
        let o = run("codex", cfg(Some(model), Some(E::Ultra))).await;
        assert_eq!(o.count(&codex_pair("ultra")), 1, "{model}");
    }
}

#[tokio::test]
async fn codex_ultra_is_rejected_before_spawn_for_unset_unknown_and_luna_models() {
    for model in [
        None,
        Some(""),
        Some("gpt-9-unknown"),
        Some("gpt-6-luna"),
        Some("gpt-5.6-luna"),
        // No prefix/substring inference from a known id.
        Some("gpt-6.1-sol-preview"),
        Some("GPT-6.1-SOL"),
    ] {
        let o = run("codex", cfg(model, Some(E::Ultra))).await;
        o.assert_rejected_before_spawn();
        assert!(o.error().contains("ultra"), "{model:?}: {}", o.error());
    }
}

#[tokio::test]
async fn codex_rejects_the_levels_it_does_not_offer() {
    for level in [E::None, E::Off, E::Minimal] {
        let o = run("codex", cfg(Some("gpt-6.1-sol"), Some(level))).await;
        o.assert_rejected_before_spawn();
    }
}

#[tokio::test]
async fn codex_ultra_note_carries_the_exact_delegation_label() {
    let o = run("codex", cfg(Some("gpt-6-astra"), Some(E::Ultra))).await;
    let label = "maximum reasoning with provider-internal sub-agent delegation; multiplies spend";
    assert!(
        o.notes.iter().any(|n| n.contains(label)),
        "notes: {:?}",
        o.notes
    );
}

#[tokio::test]
async fn codex_argv_is_the_same_whatever_the_prompt_mode_for_the_same_request() {
    let mut seen = Vec::new();
    for mode in [None, Some(PromptMode::Normal), Some(PromptMode::Eco)] {
        let mut c = cfg(Some("gpt-6.1-sol"), Some(E::Xhigh));
        c.prompt_mode = mode;
        seen.push(run("codex", c).await.argv().to_vec());
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[1], seen[2]);
}

// ── D5-11: Codex effort with no model ───────────────────────────────────────

#[tokio::test]
async fn codex_effort_with_no_model_sends_the_effort_without_a_model_flag_and_warns() {
    // `None` is how the factory and `AgentService` store an empty model.
    // (A hand-written `model: ""` is a pre-existing `--model ""` at d8c2319
    // too; it is out of this vector's scope.)
    for model in [None] {
        let o = run("codex", cfg(model, Some(E::High))).await;
        assert_eq!(o.count(&codex_pair("high")), 1);
        assert!(
            !o.argv().iter().any(|a| a == "--model" || a == "-m"),
            "{:?}",
            o.argv()
        );
        let note = o
            .note_with("capability-unknown")
            .unwrap_or_else(|| panic!("no capability-unknown note: {:?}", o.notes));
        assert!(note.contains("codex effort with no model"), "{note}");
        assert!(note.contains("different model"), "{note}");
    }
}

#[tokio::test]
async fn codex_without_effort_has_no_note_and_no_selector() {
    let o = run("codex", cfg(None, None)).await;
    assert!(o.notes.is_empty(), "{:?}", o.notes);
    assert!(
        !o.argv()
            .iter()
            .any(|a| a.starts_with("model_reasoning_effort"))
    );
}

// ── D5-4 argv: OpenCode ─────────────────────────────────────────────────────

const OC_OPUS: &str = "openrouter/anthropic/claude-opus-5.5";

fn model_arg(o: &Outcome) -> Option<String> {
    let at = o.argv().iter().position(|a| a == "--model")?;
    o.argv().get(at + 1).cloned()
}

#[tokio::test]
async fn opencode_appends_the_level_to_a_recognised_model_exactly_once() {
    for level in [E::Low, E::Medium, E::High, E::Xhigh, E::Max] {
        let o = run("opencode", cfg(Some(OC_OPUS), Some(level))).await;
        assert_eq!(
            model_arg(&o).as_deref(),
            Some(format!("{OC_OPUS}#{level}").as_str())
        );
        assert_eq!(
            o.argv().iter().filter(|a| a.contains('#')).count(),
            1,
            "{:?}",
            o.argv()
        );
    }
}

#[tokio::test]
async fn opencode_keeps_a_matching_suffix_once() {
    let o = run(
        "opencode",
        cfg(Some(&format!("{OC_OPUS}#high")), Some(E::High)),
    )
    .await;
    assert_eq!(
        model_arg(&o).as_deref(),
        Some(format!("{OC_OPUS}#high").as_str())
    );
}

#[tokio::test]
async fn opencode_new_value_conflicting_with_a_suffix_is_rejected_before_spawn() {
    let o = run(
        "opencode",
        cfg(Some(&format!("{OC_OPUS}#high")), Some(E::Xhigh)),
    )
    .await;
    o.assert_rejected_before_spawn();
}

#[tokio::test]
async fn opencode_legacy_value_conflicting_with_a_suffix_keeps_the_original_model_and_warns() {
    let original = format!("{OC_OPUS}#high");
    let o = run("opencode", cfg(Some(&original), Some(E::Low))).await;
    assert_eq!(model_arg(&o).as_deref(), Some(original.as_str()));
    assert!(o.note_with("legacy-ignored").is_some(), "{:?}", o.notes);
}

#[tokio::test]
async fn opencode_delegated_or_unknown_model_ignores_legacy_and_rejects_new_values() {
    // Delegated (no model) and an unrecognised model.
    for model in [None, Some("openrouter/other/unlisted-model")] {
        let legacy = run("opencode", cfg(model, Some(E::High))).await;
        assert_eq!(
            model_arg(&legacy).as_deref(),
            model.filter(|m| !m.is_empty()),
            "legacy must keep the model byte-for-byte ({model:?})"
        );
        assert!(legacy.note_with("legacy-ignored").is_some(), "{model:?}");
        assert!(!legacy.argv().iter().any(|a| a.contains('#')));

        let new = run("opencode", cfg(model, Some(E::Max))).await;
        new.assert_rejected_before_spawn();
    }
}

#[tokio::test]
async fn opencode_unlisted_level_for_a_known_model_follows_the_row() {
    // gemini-2.5-flash offers only `none`.
    let model = "openrouter/google/gemini-2.5-flash";
    let ok = run("opencode", cfg(Some(model), Some(E::None))).await;
    assert_eq!(
        model_arg(&ok).as_deref(),
        Some(format!("{model}#none").as_str())
    );
    let legacy = run("opencode", cfg(Some(model), Some(E::High))).await;
    assert_eq!(model_arg(&legacy).as_deref(), Some(model));
    assert!(legacy.note_with("legacy-ignored").is_some());
    let new = run("opencode", cfg(Some(model), Some(E::Max))).await;
    new.assert_rejected_before_spawn();
}

#[tokio::test]
async fn opencode_opaque_thinking_suffix_survives_with_effort_unset() {
    let model = "openrouter/anthropic/claude-opus-5.5#thinking";
    let o = run("opencode", cfg(Some(model), None)).await;
    assert_eq!(model_arg(&o).as_deref(), Some(model));
    assert!(o.notes.is_empty());
}

// ── D5-5: Pi, Claude, Antigravity ───────────────────────────────────────────

#[tokio::test]
async fn pi_forwards_its_seven_spellings_with_the_thinking_flag_and_rejects_none() {
    for level in [
        E::Off,
        E::Minimal,
        E::Low,
        E::Medium,
        E::High,
        E::Xhigh,
        E::Max,
    ] {
        let o = run("pi", cfg(Some("some/model"), Some(level))).await;
        let at = o.argv().iter().position(|a| a == "--thinking").unwrap();
        assert_eq!(o.argv()[at + 1], level.as_str());
        assert_eq!(o.count("--thinking"), 1);
    }
    run("pi", cfg(Some("some/model"), Some(E::None)))
        .await
        .assert_rejected_before_spawn();
    run("pi", cfg(Some("some/model"), Some(E::Ultra)))
        .await
        .assert_rejected_before_spawn();
}

#[tokio::test]
async fn pi_flag_wins_over_a_model_suffix_without_rewriting_the_slug_and_discloses_it() {
    let o = run("pi", cfg(Some("provider/model:low"), Some(E::High))).await;
    let at = o.argv().iter().position(|a| a == "--model").unwrap();
    assert_eq!(
        o.argv()[at + 1],
        "provider/model:low",
        "slug is not rewritten"
    );
    assert_eq!(
        o.argv()[o.argv().iter().position(|a| a == "--thinking").unwrap() + 1],
        "high"
    );
    assert!(o.note_with("clamp-possible").is_some(), "{:?}", o.notes);
    assert!(
        o.notes
            .iter()
            .any(|n| n.contains("--thinking level takes precedence")),
        "{:?}",
        o.notes
    );
}

#[tokio::test]
async fn pi_never_reports_the_request_as_confirmed() {
    let o = run("pi", cfg(Some("some/model"), Some(E::High))).await;
    for n in &o.notes {
        let l = n.to_lowercase();
        assert!(
            !l.contains("confirmed") && !l.contains("effective: "),
            "{n}"
        );
    }
}

#[tokio::test]
async fn claude_xhigh_follows_the_model_row_and_aliases_are_not_guessed() {
    // Known 4.6 models: xhigh excluded (new authoring), max allowed.
    for model in ["claude-opus-4-6", "claude-sonnet-4-6"] {
        run("claude", cfg(Some(model), Some(E::Xhigh)))
            .await
            .assert_rejected_before_spawn();
        let o = run("claude", cfg(Some(model), Some(E::Max))).await;
        assert_eq!(o.count("--effort"), 1);
    }
    let known = run("claude", cfg(Some("claude-opus-5-5"), Some(E::Xhigh))).await;
    assert_eq!(known.count("--effort"), 1);

    // Unresolved alias: syntax-only with a capability-unknown note, never a
    // made-up resolved row.
    let alias = run("claude", cfg(Some("opus"), Some(E::Xhigh))).await;
    assert_eq!(alias.count("--effort"), 1);
    assert!(
        alias.note_with("capability-unknown").is_some(),
        "{:?}",
        alias.notes
    );
}

#[tokio::test]
async fn claude_rejects_spellings_it_does_not_have() {
    for level in [E::None, E::Off, E::Minimal, E::Ultra] {
        run("claude", cfg(Some("claude-opus-5-5"), Some(level)))
            .await
            .assert_rejected_before_spawn();
    }
}

#[tokio::test]
async fn antigravity_forwards_low_medium_high_and_rejects_the_rest() {
    for level in [E::Low, E::Medium, E::High] {
        let o = run("antigravity", cfg(Some("gemini-4-argon"), Some(level))).await;
        let at = o.argv().iter().position(|a| a == "--effort").unwrap();
        assert_eq!(o.argv()[at + 1], level.as_str());
        assert_eq!(o.count("--effort"), 1);
    }
    for level in [E::Xhigh, E::Max, E::Minimal, E::Ultra, E::None, E::Off] {
        run("antigravity", cfg(Some("gemini-4-argon"), Some(level)))
            .await
            .assert_rejected_before_spawn();
    }
}

#[tokio::test]
async fn antigravity_gpt_oss_accepts_only_medium_and_a_legacy_other_is_ignored_with_a_warning() {
    let ok = run("antigravity", cfg(Some("gpt-oss-120b"), Some(E::Medium))).await;
    assert_eq!(ok.count("--effort"), 1);
    let legacy = run("antigravity", cfg(Some("gpt-oss-120b"), Some(E::High))).await;
    assert_eq!(legacy.count("--effort"), 0);
    assert!(legacy.note_with("legacy-ignored").is_some());
}

#[tokio::test]
async fn antigravity_legacy_suffixed_slug_with_effort_is_forwarded_and_labelled_unresolved() {
    let o = run(
        "antigravity",
        cfg(Some("gemini-4-argon-high"), Some(E::High)),
    )
    .await;
    assert_eq!(
        o.count("--effort"),
        1,
        "legacy value is forwarded, not newly rejected"
    );
    let at = o.argv().iter().position(|a| a == "--model").unwrap();
    assert_eq!(
        o.argv()[at + 1],
        "gemini-4-argon-high",
        "model bytes unchanged"
    );
    let note = o
        .note_with("capability-unknown")
        .expect("unresolved precedence note");
    assert!(note.contains("unresolved"), "{note}");
}

// ── D5-12: Grok / Cursor / Shell ────────────────────────────────────────────

#[tokio::test]
async fn grok_forwards_legacy_levels_and_rejects_every_other_spelling_with_the_evidence_gap() {
    for level in [E::Low, E::Medium, E::High] {
        let o = run("grok", cfg(Some("grok-build"), Some(level))).await;
        assert_eq!(o.count("--effort"), 1);
    }
    for level in [E::None, E::Off, E::Minimal, E::Xhigh, E::Max, E::Ultra] {
        let o = run("grok", cfg(Some("grok-build"), Some(level))).await;
        o.assert_rejected_before_spawn();
        let msg = o.error();
        assert!(
            msg.contains("not verified against Grok CLI documentation or an installed binary"),
            "{level}: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("invalid for grok") && !msg.contains("provider-invalid"),
            "never labelled provider-invalid: {msg}"
        );
    }
}

#[tokio::test]
async fn cursor_preserves_a_bracketed_model_and_synthesises_no_effort() {
    let model = "gpt-5[reasoning=high,fast=true]";
    let o = run("cursor", cfg(Some(model), None)).await;
    assert!(o.argv().iter().any(|a| a == model), "{:?}", o.argv());
    // A legacy request is ignored with a warning and the opaque slug is intact.
    let legacy = run("cursor", cfg(Some(model), Some(E::High))).await;
    assert!(legacy.argv().iter().any(|a| a == model));
    assert!(legacy.note_with("legacy-ignored").is_some());
    assert!(
        !legacy
            .argv()
            .iter()
            .any(|a| a.contains("effort") || a == "--thinking")
    );
    // A new value is a configuration error before spawn.
    run("cursor", cfg(Some(model), Some(E::Max)))
        .await
        .assert_rejected_before_spawn();
}

#[tokio::test]
async fn shell_runs_its_command_unchanged_and_rejects_new_effort_values() {
    let plain = run("shell", cfg(None, None)).await;
    let legacy = run("shell", cfg(None, Some(E::High))).await;
    assert_eq!(
        plain.argv(),
        legacy.argv(),
        "legacy value must not change the command"
    );
    assert!(legacy.note_with("legacy-ignored").is_some());
    run("shell", cfg(None, Some(E::Xhigh)))
        .await
        .assert_rejected_before_spawn();
}

// ── Single-emission sweep across all adapters ───────────────────────────────

#[tokio::test]
async fn no_adapter_ever_emits_two_effort_selectors() {
    for (kind, model) in [
        ("claude", "claude-opus-5-5"),
        ("codex", "gpt-6.1-sol"),
        ("pi", "x/y"),
        ("grok", "grok-build"),
        ("antigravity", "gemini-4-argon"),
    ] {
        for level in [E::Low, E::Medium, E::High] {
            let o = run(kind, cfg(Some(model), Some(level))).await;
            let selectors = o
                .argv()
                .iter()
                .filter(|a| {
                    matches!(a.as_str(), "--effort" | "--thinking")
                        || a.starts_with("model_reasoning_effort=")
                })
                .count();
            assert_eq!(selectors, 1, "{kind} {level}: {:?}", o.argv());
        }
    }
}

// ── review-b2 F1 (ruling 8): diagnostics never ride the provider stream ─────

#[tokio::test]
async fn no_adapter_ever_emits_effort_diagnostics_as_provider_output() {
    let cases = [
        ("claude", Some("claude-opus-5-5"), E::High),
        ("claude", None, E::High),
        ("codex", Some("gpt-6.1-sol"), E::High),
        ("codex", None, E::High),
        ("pi", Some("x/y"), E::High),
        ("grok", Some("grok-build"), E::High),
        ("antigravity", Some("gemini-4-argon"), E::High),
        ("antigravity", Some("gpt-oss-120b"), E::High),
        ("cursor", Some("auto"), E::High),
        ("shell", None, E::High),
        ("opencode", None, E::High),
        ("opencode", Some("openrouter/other/unlisted-model"), E::High),
    ];
    for (kind, model, level) in cases {
        let o = run(kind, cfg(model, Some(level))).await;
        assert!(
            o.stream_notes.is_empty(),
            "{kind} {model:?} {level}: diagnostics leaked into the provider stream: {:?}",
            o.stream_notes
        );
    }
}

#[tokio::test]
async fn the_pre_execute_surface_still_carries_the_diagnostic_the_stream_no_longer_does() {
    // Guards the other half of the move: a diagnostic that vanished from both
    // surfaces would pass the sweep above.
    let forwarded = run("claude", cfg(Some("claude-opus-5-5"), Some(E::High))).await;
    assert!(forwarded.stream_notes.is_empty());
    assert!(
        forwarded.note_with("request-only").is_some(),
        "{:?}",
        forwarded.notes
    );
    let ignored = run("shell", cfg(None, Some(E::High))).await;
    assert!(ignored.stream_notes.is_empty());
    assert!(
        ignored.note_with("legacy-ignored").is_some(),
        "{:?}",
        ignored.notes
    );
    let delegated = run("opencode", cfg(None, Some(E::High))).await;
    assert!(delegated.stream_notes.is_empty());
    assert!(
        delegated.note_with("legacy-ignored").is_some(),
        "{:?}",
        delegated.notes
    );
}
