//! T1 / F1 — D1 parse side: old files read as normal/inherit and are never
//! backfilled; `inherit` and `default` are commands that are never stored;
//! unknown extension keys survive the raw (JSON value) edit path the CLI/TUI
//! use. Vectors: D1-3 (domain resolution), D1-5 (domain half), D5-1/D5-8 (parse
//! side), D5-7 (parse/list of legacy scalars).
//!
//! External: only the public `orch_domain` API plus serde formats.

use orch_domain::{
    Agent, AgentConfig, AgentDefaults, CreateAgentInput, EffortSelection, OrchestratorConfig,
    PromptMode, PromptModeSelection, PromptModeSource, ReasoningEffort, deep_merge, default_config,
    effective_prompt_mode, get_by_path, set_by_path,
};
use serde_json::{Value, json};

/// An agent file exactly as v0.9.4 (baseline d8c2319) wrote it, with every
/// optional config key present so byte preservation covers all of them.
const OLD_FULL_AGENT_YAML: &str = "\
id: agt_old
name: Old Agent
adapter: codex
role: Does things.
config:
  command: echo hi
  model: gpt-5.3-codex
  effort: medium
  approval_policy: auto
  max_turns: 50
  timeout_ms: 3600000
  stall_timeout_ms: 300000
  env:
    B: '2'
    A: '1'
  system_prompt: be brief
  workspace_mode: worktree
  skills:
  - review
status: idle
stats:
  tasks_completed: 3
  tasks_failed: 1
  total_runs: 4
  total_runtime_ms: 1234
";

fn stripped_default_config_yaml() -> String {
    let yaml = serde_yaml_ng::to_string(&default_config()).expect("serializes");
    yaml.lines()
        .filter(|l| !l.trim_start().starts_with("prompt_mode:"))
        .map(|l| format!("{l}\n"))
        .collect()
}

#[test]
fn an_old_agent_file_with_every_legacy_key_round_trips_byte_for_byte() {
    let agent: Agent = serde_yaml_ng::from_str(OLD_FULL_AGENT_YAML).expect("old file parses");
    assert_eq!(
        serde_yaml_ng::to_string(&agent).expect("serializes"),
        OLD_FULL_AGENT_YAML
    );
}

#[test]
fn an_old_agent_file_reads_with_no_mode_override_and_no_summary() {
    let agent: Agent = serde_yaml_ng::from_str(OLD_FULL_AGENT_YAML).expect("old file parses");
    assert_eq!(agent.config.prompt_mode, None);
    assert_eq!(agent.config.role_summary, None);
}

#[test]
fn an_old_agent_json_shape_gains_no_keys() {
    let agent: Agent = serde_yaml_ng::from_str(OLD_FULL_AGENT_YAML).expect("old file parses");
    let json = serde_json::to_value(&agent).expect("serializes");
    let config = json["config"].as_object().expect("config object");
    assert!(!config.contains_key("prompt_mode"), "{config:?}");
    assert!(!config.contains_key("role_summary"), "{config:?}");
}

#[test]
fn an_old_config_without_prompt_mode_reads_as_normal() {
    let config: OrchestratorConfig =
        serde_yaml_ng::from_str(&stripped_default_config_yaml()).expect("old config parses");
    assert_eq!(config.defaults.agent.prompt_mode, PromptMode::Normal);
}

#[test]
fn a_freshly_initialised_config_states_normal_explicitly_after_workspace_mode() {
    let yaml = serde_yaml_ng::to_string(&default_config()).expect("serializes");
    assert!(
        yaml.contains("    workspace_mode: worktree\n    prompt_mode: normal\n"),
        "{yaml}"
    );
}

#[test]
fn parsing_an_old_config_does_not_change_what_it_serializes_for_the_other_keys() {
    let old = stripped_default_config_yaml();
    let parsed: OrchestratorConfig = serde_yaml_ng::from_str(&old).expect("parses");
    let again = serde_yaml_ng::to_string(&parsed).expect("serializes");
    // Writing back adds exactly the one documented key and nothing else.
    let without_new_key: String = again
        .lines()
        .filter(|l| !l.trim_start().starts_with("prompt_mode:"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(without_new_key, old);
}

#[test]
fn inherit_is_a_parse_error_in_agent_yaml_because_it_is_a_command_not_data() {
    assert!(serde_yaml_ng::from_str::<AgentConfig>("prompt_mode: inherit\n").is_err());
}

#[test]
fn inherit_is_a_parse_error_in_project_config_because_it_is_a_command_not_data() {
    let yaml = stripped_default_config_yaml().replace(
        "    workspace_mode: worktree\n",
        "    workspace_mode: worktree\n    prompt_mode: inherit\n",
    );
    assert!(serde_yaml_ng::from_str::<OrchestratorConfig>(&yaml).is_err());
}

#[test]
fn default_is_a_parse_error_for_effort_because_it_is_a_command_not_data() {
    assert!(serde_yaml_ng::from_str::<AgentConfig>("effort: default\n").is_err());
}

#[test]
fn null_prompt_mode_means_inherit() {
    let config: AgentConfig = serde_yaml_ng::from_str("prompt_mode: null\n").expect("parses");
    assert_eq!(config.prompt_mode, None);
}

#[test]
fn a_mode_spelling_outside_normal_and_eco_is_rejected_in_agent_yaml() {
    for bad in ["Eco", "NORMAL", "economy", "''", "0"] {
        assert!(
            serde_yaml_ng::from_str::<AgentConfig>(&format!("prompt_mode: {bad}\n")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn an_unset_override_is_omitted_from_yaml_not_written_as_inherit_or_null() {
    let yaml = serde_yaml_ng::to_string(&AgentConfig::default()).expect("serializes");
    assert!(!yaml.contains("prompt_mode"), "{yaml}");
    assert!(!yaml.contains("inherit"), "{yaml}");
    assert!(!yaml.contains("role_summary"), "{yaml}");
}

#[test]
fn clearing_the_mode_override_through_the_selection_command_stores_nothing() {
    let cleared = "inherit"
        .parse::<PromptModeSelection>()
        .expect("inherit is a command")
        .into_override();
    let config = AgentConfig {
        prompt_mode: cleared,
        ..AgentConfig::default()
    };
    assert!(
        !serde_yaml_ng::to_string(&config)
            .expect("serializes")
            .contains("prompt_mode")
    );
}

#[test]
fn setting_the_mode_override_through_the_selection_command_stores_the_spelling() {
    let set = "eco"
        .parse::<PromptModeSelection>()
        .expect("eco parses")
        .into_override();
    let config = AgentConfig {
        prompt_mode: set,
        ..AgentConfig::default()
    };
    assert_eq!(
        serde_yaml_ng::to_string(&config).expect("serializes"),
        "prompt_mode: eco\n"
    );
}

#[test]
fn the_default_effort_command_clears_the_generic_value_and_stores_nothing() {
    let cleared = "default"
        .parse::<EffortSelection>()
        .expect("default is a command")
        .into_effort();
    assert_eq!(cleared, None);
    let config = AgentConfig {
        model: Some("openrouter/anthropic/claude-opus-5.5#high".into()),
        effort: cleared,
        ..AgentConfig::default()
    };
    let yaml = serde_yaml_ng::to_string(&config).expect("serializes");
    assert!(!yaml.contains("effort"), "{yaml}");
    assert!(!yaml.contains("default"), "{yaml}");
}

#[test]
fn clearing_effort_keeps_a_native_model_suffix_byte_for_byte() {
    // The command only changes the generic field; the model string is data the
    // user wrote and is never rewritten.
    let model = "openrouter/anthropic/claude-opus-5.5#high";
    let mut config = AgentConfig {
        model: Some(model.into()),
        effort: Some(ReasoningEffort::High),
        ..AgentConfig::default()
    };
    config.effort = "default"
        .parse::<EffortSelection>()
        .expect("command")
        .into_effort();
    assert_eq!(config.model.as_deref(), Some(model));
    assert_eq!(config.effort, None);
}

#[test]
fn only_default_and_levels_parse_as_effort_selections() {
    for bad in ["", "inherit", "High", "ultracode", "auto"] {
        assert!(bad.parse::<EffortSelection>().is_err(), "{bad}");
    }
    for level in ReasoningEffort::ALL {
        assert_eq!(
            level.as_str().parse::<EffortSelection>(),
            Ok(EffortSelection::Set(level))
        );
    }
}

#[test]
fn every_new_effort_spelling_parses_from_yaml_so_listing_never_fails() {
    for level in ReasoningEffort::ALL {
        let config: AgentConfig =
            serde_yaml_ng::from_str(&format!("effort: {}\n", level.as_str())).expect("parses");
        assert_eq!(config.effort, Some(level), "{level}");
    }
}

#[test]
fn the_three_legacy_scalars_are_the_only_legacy_spellings() {
    let legacy: Vec<&str> = ReasoningEffort::ALL
        .into_iter()
        .filter(ReasoningEffort::is_legacy)
        .map(|e| e.as_str())
        .collect();
    assert_eq!(legacy, ["low", "medium", "high"]);
}

// ── Resolution (D1-3, domain half) ──────────────────────────────────────────

fn defaults(mode: PromptMode) -> AgentDefaults {
    AgentDefaults {
        prompt_mode: mode,
        ..default_config().defaults.agent
    }
}

fn agent_with(mode: Option<PromptMode>) -> AgentConfig {
    AgentConfig {
        prompt_mode: mode,
        ..AgentConfig::default()
    }
}

#[test]
fn an_agent_without_an_override_inherits_the_project_default_live() {
    assert_eq!(
        effective_prompt_mode(&agent_with(None), &defaults(PromptMode::Eco)),
        (PromptMode::Eco, PromptModeSource::Project)
    );
    assert_eq!(
        effective_prompt_mode(&agent_with(None), &defaults(PromptMode::Normal)),
        (PromptMode::Normal, PromptModeSource::Project)
    );
}

#[test]
fn a_normal_override_survives_a_project_switch_to_eco() {
    assert_eq!(
        effective_prompt_mode(
            &agent_with(Some(PromptMode::Normal)),
            &defaults(PromptMode::Eco)
        ),
        (PromptMode::Normal, PromptModeSource::Agent)
    );
}

#[test]
fn an_eco_override_survives_the_project_default_being_normal() {
    assert_eq!(
        effective_prompt_mode(
            &agent_with(Some(PromptMode::Eco)),
            &defaults(PromptMode::Normal)
        ),
        (PromptMode::Eco, PromptModeSource::Agent)
    );
}

#[test]
fn creating_an_agent_never_copies_the_project_mode_into_it() {
    let input: CreateAgentInput = serde_json::from_value(json!({
        "name": "n",
        "adapter": "claude"
    }))
    .expect("old create input parses");
    assert_eq!(input.prompt_mode, None);
    assert_eq!(input.role_summary, None);
    let back = serde_json::to_value(&input).expect("serializes");
    assert!(back.get("prompt_mode").is_none(), "{back}");
    assert!(back.get("role_summary").is_none(), "{back}");
}

// ── Raw edit path: unknown keys survive, enum validated (D1-2 domain half) ──

fn raw_config_with_extension_keys() -> Value {
    let mut raw = serde_json::to_value(default_config()).expect("serializes");
    set_by_path(&mut raw, "x_extension", json!({"keep": [1, 2, 3]}));
    set_by_path(&mut raw, "defaults.agent.x_hint", json!("hand-written"));
    raw
}

#[test]
fn setting_the_project_mode_through_the_raw_value_keeps_unknown_keys() {
    let mut raw = raw_config_with_extension_keys();
    set_by_path(&mut raw, "defaults.agent.prompt_mode", json!("eco"));
    assert_eq!(
        get_by_path(&raw, "x_extension.keep"),
        Some(&json!([1, 2, 3]))
    );
    assert_eq!(
        get_by_path(&raw, "defaults.agent.x_hint"),
        Some(&json!("hand-written"))
    );
    let typed: OrchestratorConfig =
        serde_json::from_value(raw).expect("typed read tolerates extras");
    assert_eq!(typed.defaults.agent.prompt_mode, PromptMode::Eco);
}

#[test]
fn a_merge_patch_of_the_mode_key_keeps_unknown_keys_and_sibling_defaults() {
    let raw = raw_config_with_extension_keys();
    let merged = deep_merge(
        &raw,
        &json!({"defaults": {"agent": {"prompt_mode": "eco"}}}),
    );
    assert_eq!(
        get_by_path(&merged, "x_extension.keep"),
        Some(&json!([1, 2, 3]))
    );
    assert_eq!(
        get_by_path(&merged, "defaults.agent.max_turns"),
        Some(&json!(50))
    );
    assert_eq!(
        get_by_path(&merged, "defaults.agent.prompt_mode"),
        Some(&json!("eco"))
    );
}

#[test]
fn an_invalid_mode_written_through_the_raw_value_fails_the_typed_validation() {
    for bad in [json!("inherit"), json!("turbo"), json!(7), json!(true)] {
        let mut raw = raw_config_with_extension_keys();
        set_by_path(&mut raw, "defaults.agent.prompt_mode", bad.clone());
        assert!(
            serde_json::from_value::<OrchestratorConfig>(raw).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn unknown_keys_in_an_agent_file_are_tolerated_on_read() {
    let yaml = OLD_FULL_AGENT_YAML.replace(
        "  skills:\n  - review\n",
        "  skills:\n  - review\n  x_future: kept-by-raw-editors\n",
    );
    let agent: Agent =
        serde_yaml_ng::from_str(&yaml).expect("an unknown key does not block reading");
    assert_eq!(agent.config.effort, Some(ReasoningEffort::Medium));
}
