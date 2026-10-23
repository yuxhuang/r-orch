//! Project configuration — the shape of `.orchestry/config.yml`,
//! plus the pure value operations `ConfigStore` builds on.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// How the default prompt templates present a dispatch.
///
/// `normal` is the historical renderer, byte-for-byte. `eco` is the opt-in
/// compact presentation of the same task, context and obligations. The mode
/// changes presentation only; no scheduling, retry, review or selection logic
/// reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PromptMode {
    #[default]
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "eco")]
    Eco,
}

impl PromptMode {
    pub const ALL: [PromptMode; 2] = [PromptMode::Normal, PromptMode::Eco];

    pub fn as_str(&self) -> &'static str {
        match self {
            PromptMode::Normal => "normal",
            PromptMode::Eco => "eco",
        }
    }

    /// The other mode: what the TUI project toggle switches to.
    pub fn toggled(self) -> PromptMode {
        match self {
            PromptMode::Normal => PromptMode::Eco,
            PromptMode::Eco => PromptMode::Normal,
        }
    }
}

impl std::fmt::Display for PromptMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PromptMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PromptMode::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// A per-agent prompt-mode choice as a command argument: `inherit` clears the
/// override, `normal`/`eco` set it. `inherit` is a command, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptModeSelection {
    Inherit,
    Set(PromptMode),
}

impl PromptModeSelection {
    /// The spelling that clears an agent's override.
    pub const INHERIT: &'static str = "inherit";

    /// The value to store in `AgentConfig::prompt_mode`.
    pub fn into_override(self) -> Option<PromptMode> {
        match self {
            PromptModeSelection::Inherit => None,
            PromptModeSelection::Set(mode) => Some(mode),
        }
    }
}

impl std::fmt::Display for PromptModeSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptModeSelection::Inherit => f.write_str(Self::INHERIT),
            PromptModeSelection::Set(mode) => mode.fmt(f),
        }
    }
}

impl std::str::FromStr for PromptModeSelection {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == Self::INHERIT {
            Ok(PromptModeSelection::Inherit)
        } else {
            s.parse().map(PromptModeSelection::Set)
        }
    }
}

/// Where an agent's effective prompt mode came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PromptModeSource {
    /// The agent's own `config.prompt_mode` override.
    #[serde(rename = "agent")]
    Agent,
    /// Inherited from `defaults.agent.prompt_mode` (normal when the project
    /// file predates the setting).
    #[serde(rename = "project")]
    Project,
}

impl PromptModeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PromptModeSource::Agent => "agent",
            PromptModeSource::Project => "project",
        }
    }
}

impl std::fmt::Display for PromptModeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An agent's effective prompt mode: its override, else the current project
/// default. Pure; the caller supplies the defaults it read for this dispatch.
pub fn effective_prompt_mode(
    agent: &crate::agent::AgentConfig,
    defaults: &AgentDefaults,
) -> (PromptMode, PromptModeSource) {
    match agent.prompt_mode {
        Some(mode) => (mode, PromptModeSource::Agent),
        None => (defaults.prompt_mode, PromptModeSource::Project),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefaults {
    pub adapter: String,
    pub approval_policy: crate::agent::ApprovalPolicy,
    pub max_turns: u32,
    pub timeout_ms: u64,
    pub stall_timeout_ms: u64,
    pub workspace_mode: crate::task::WorkspaceMode,
    /// The project prompt mode agents without an override inherit. Files that
    /// predate it read as `normal`; nothing writes it back on read.
    #[serde(default)]
    pub prompt_mode: PromptMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDefaults {
    pub max_attempts: u32,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    pub agent: AgentDefaults,
    pub task: TaskDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulingConfig {
    pub poll_interval_ms: u64,
    pub max_concurrent_agents: usize,
    pub retry_base_delay_ms: u64,
    pub retry_max_delay_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptConfig {
    /// Legacy single combined template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_template: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
}

impl PeerConfig {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.listen.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    pub project: ProjectConfig,
    pub defaults: Defaults,
    pub scheduling: SchedulingConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptConfig>,
    #[serde(default, skip_serializing_if = "PeerConfig::is_empty")]
    pub peer: PeerConfig,
}

/// The default project configuration.
pub fn default_config() -> OrchestratorConfig {
    OrchestratorConfig {
        project: ProjectConfig {
            name: "my-project".into(),
            description: None,
        },
        defaults: Defaults {
            agent: AgentDefaults {
                adapter: "claude".into(),
                approval_policy: crate::agent::ApprovalPolicy::Auto,
                max_turns: 50,
                timeout_ms: 3_600_000,
                stall_timeout_ms: 600_000,
                workspace_mode: crate::task::WorkspaceMode::Worktree,
                prompt_mode: PromptMode::Normal,
            },
            task: TaskDefaults {
                max_attempts: 3,
                priority: 3,
            },
        },
        scheduling: SchedulingConfig {
            poll_interval_ms: 10_000,
            max_concurrent_agents: 6,
            retry_base_delay_ms: 10_000,
            retry_max_delay_ms: 300_000,
        },
        prompt: None,
        peer: PeerConfig::default(),
    }
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        default_config()
    }
}

/// Recursive deep merge: objects merge key-wise, everything else (arrays,
/// scalars, `null`) replaces.
///
/// This is `ConfigStore`'s `deepMerge`. It operates on a value tree rather
/// than on [`OrchestratorConfig`] because the store merges the raw parsed
/// YAML over `DEFAULT_CONFIG` as plain objects — which means **unknown keys in
/// `config.yml` survive a `config set` round trip**. `ConfigStore` must
/// therefore keep the merged value for write-back and deserialize a typed
/// [`OrchestratorConfig`] only for consumers.
pub fn deep_merge(target: &Value, source: &Value) -> Value {
    let (Value::Object(target), Value::Object(source)) = (target, source) else {
        return source.clone();
    };

    let mut result = target.clone();
    for (key, source_val) in source {
        let merged = match (result.get(key), source_val) {
            // Both sides a non-null, non-array object → recurse.
            (Some(target_val @ Value::Object(_)), Value::Object(_)) => {
                deep_merge(target_val, source_val)
            }
            _ => source_val.clone(),
        };
        result.insert(key.clone(), merged);
    }
    Value::Object(result)
}

/// Read a `.`-separated dot path. Returns `None` for a missing key or when the
/// path runs through a non-object.
pub fn get_by_path<'v>(obj: &'v Value, key_path: &str) -> Option<&'v Value> {
    let mut current = obj;
    for key in key_path.split('.') {
        current = current.as_object()?.get(key)?;
    }
    Some(current)
}

/// Write a `.`-separated dot path, creating missing intermediate objects and
/// **replacing** any non-object value found along the way.
pub fn set_by_path(obj: &mut Value, key_path: &str, value: Value) {
    let keys: Vec<&str> = key_path.split('.').collect();
    if !obj.is_object() {
        *obj = Value::Object(Map::new());
    }

    let mut current = obj;
    for key in &keys[..keys.len() - 1] {
        // `current` is an object here: the loop below re-establishes it.
        let map = current.as_object_mut().expect("current is an object");
        let entry = map
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = entry;
    }

    current
        .as_object_mut()
        .expect("current is an object")
        .insert(keys[keys.len() - 1].to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_config_matches_the_init_file() {
        // As written by `init` (with the default project name).
        let value = serde_json::to_value(default_config()).unwrap();
        assert_eq!(
            value,
            json!({
                "project": { "name": "my-project" },
                "defaults": {
                    "agent": {
                        "adapter": "claude",
                        "approval_policy": "auto",
                        "max_turns": 50,
                        "timeout_ms": 3_600_000,
                        "stall_timeout_ms": 600_000,
                        "workspace_mode": "worktree",
                        "prompt_mode": "normal",
                    },
                    "task": { "max_attempts": 3, "priority": 3 },
                },
                "scheduling": {
                    "poll_interval_ms": 10_000,
                    "max_concurrent_agents": 6,
                    "retry_base_delay_ms": 10_000,
                    "retry_max_delay_ms": 300_000,
                },
            })
        );
    }

    #[test]
    fn empty_source_yields_the_defaults() {
        let defaults = serde_json::to_value(default_config()).unwrap();
        assert_eq!(deep_merge(&defaults, &json!({})), defaults);
    }

    #[test]
    fn nested_objects_merge_key_wise() {
        let merged = deep_merge(
            &json!({ "a": { "b": 1, "c": 2 }, "d": 3 }),
            &json!({ "a": { "c": 9 } }),
        );
        assert_eq!(merged, json!({ "a": { "b": 1, "c": 9 }, "d": 3 }));
    }

    #[test]
    fn arrays_and_scalars_replace_rather_than_merge() {
        assert_eq!(
            deep_merge(&json!({ "a": [1, 2, 3] }), &json!({ "a": [9] })),
            json!({ "a": [9] })
        );
        assert_eq!(
            deep_merge(&json!({ "a": { "b": 1 } }), &json!({ "a": 5 })),
            json!({ "a": 5 })
        );
    }

    #[test]
    fn null_in_the_source_replaces() {
        // A null source is not an object, so it is assigned.
        assert_eq!(
            deep_merge(&json!({ "a": { "b": 1 } }), &json!({ "a": null })),
            json!({ "a": null })
        );
    }

    #[test]
    fn unknown_keys_in_the_source_survive_the_merge() {
        let defaults = serde_json::to_value(default_config()).unwrap();
        let merged = deep_merge(&defaults, &json!({ "my_extension": { "k": "v" } }));
        assert_eq!(merged["my_extension"]["k"], "v");
        assert_eq!(merged["defaults"]["agent"]["max_turns"], 50);
    }

    #[test]
    fn partial_config_file_merges_over_defaults() {
        let defaults = serde_json::to_value(default_config()).unwrap();
        let merged = deep_merge(
            &defaults,
            &json!({ "project": { "name": "proj" }, "scheduling": { "max_concurrent_agents": 2 } }),
        );
        let config: OrchestratorConfig = serde_json::from_value(merged).unwrap();
        assert_eq!(config.project.name, "proj");
        assert_eq!(config.scheduling.max_concurrent_agents, 2);
        assert_eq!(config.scheduling.poll_interval_ms, 10_000);
    }

    #[test]
    fn get_by_path_walks_dot_notation() {
        let value = serde_json::to_value(default_config()).unwrap();
        assert_eq!(
            get_by_path(&value, "defaults.agent.max_turns"),
            Some(&json!(50))
        );
        assert_eq!(
            get_by_path(&value, "project.name"),
            Some(&json!("my-project"))
        );
        assert_eq!(get_by_path(&value, "nope"), None);
        assert_eq!(get_by_path(&value, "defaults.agent.max_turns.deeper"), None);
        assert_eq!(get_by_path(&value, "project"), value.get("project"));
    }

    #[test]
    fn set_by_path_creates_missing_intermediates() {
        let mut value = json!({});
        set_by_path(&mut value, "a.b.c", json!(1));
        assert_eq!(value, json!({ "a": { "b": { "c": 1 } } }));
    }

    #[test]
    fn set_by_path_replaces_a_non_object_intermediate() {
        let mut value = json!({ "a": 5 });
        set_by_path(&mut value, "a.b", json!(1));
        assert_eq!(value, json!({ "a": { "b": 1 } }));
    }

    #[test]
    fn set_by_path_overwrites_a_leaf() {
        let mut value = serde_json::to_value(default_config()).unwrap();
        set_by_path(&mut value, "defaults.agent.workspace_mode", json!("shared"));
        let config: OrchestratorConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            config.defaults.agent.workspace_mode,
            crate::task::WorkspaceMode::Shared
        );
    }

    #[test]
    fn set_by_path_on_a_single_segment() {
        let mut value = json!({ "a": 1 });
        set_by_path(&mut value, "b", json!(2));
        assert_eq!(value, json!({ "a": 1, "b": 2 }));
    }
    #[test]
    fn peer_configuration_is_optional_and_round_trips_name_and_listen() {
        let mut config = default_config();
        assert!(config.peer.listen.is_none());
        assert!(config.peer.name.is_none());
        assert!(serde_json::to_value(&config).unwrap().get("peer").is_none());
        config.peer = PeerConfig {
            name: Some("controller".into()),
            listen: Some("0.0.0.0:7690".into()),
        };
        let read: OrchestratorConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(read.peer, config.peer);
    }

    #[test]
    fn a_config_file_without_prompt_mode_reads_as_normal() {
        let mut value = serde_json::to_value(default_config()).unwrap();
        value["defaults"]["agent"]
            .as_object_mut()
            .unwrap()
            .remove("prompt_mode");
        let config: OrchestratorConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.defaults.agent.prompt_mode, PromptMode::Normal);
    }

    #[test]
    fn prompt_mode_spellings_are_normal_and_eco_only() {
        for mode in PromptMode::ALL {
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.as_str())
            );
            assert_eq!(mode.as_str().parse::<PromptMode>(), Ok(mode));
        }
        assert_eq!(PromptMode::default(), PromptMode::Normal);
        assert_eq!(PromptMode::Normal.toggled(), PromptMode::Eco);
        assert_eq!(PromptMode::Eco.toggled(), PromptMode::Normal);
        for bad in ["inherit", "Eco", "default", ""] {
            assert_eq!(bad.parse::<PromptMode>(), Err(()), "{bad}");
            assert!(serde_json::from_value::<PromptMode>(json!(bad)).is_err());
        }
    }

    #[test]
    fn prompt_mode_selection_distinguishes_inherit_from_a_mode() {
        assert_eq!(
            "inherit".parse::<PromptModeSelection>(),
            Ok(PromptModeSelection::Inherit)
        );
        assert_eq!(
            "eco".parse::<PromptModeSelection>(),
            Ok(PromptModeSelection::Set(PromptMode::Eco))
        );
        assert_eq!("default".parse::<PromptModeSelection>(), Err(()));
        assert_eq!(PromptModeSelection::Inherit.into_override(), None);
        assert_eq!(
            PromptModeSelection::Set(PromptMode::Normal).into_override(),
            Some(PromptMode::Normal)
        );
        for selection in [
            PromptModeSelection::Inherit,
            PromptModeSelection::Set(PromptMode::Eco),
        ] {
            assert_eq!(
                selection.to_string().parse::<PromptModeSelection>(),
                Ok(selection)
            );
        }
    }

    #[test]
    fn effective_mode_is_the_override_else_the_live_project_default() {
        let mut defaults = default_config().defaults.agent;
        let inherited = crate::agent::AgentConfig::default();
        let pinned = crate::agent::AgentConfig {
            prompt_mode: Some(PromptMode::Normal),
            ..crate::agent::AgentConfig::default()
        };
        assert_eq!(
            effective_prompt_mode(&inherited, &defaults),
            (PromptMode::Normal, PromptModeSource::Project)
        );
        defaults.prompt_mode = PromptMode::Eco;
        assert_eq!(
            effective_prompt_mode(&inherited, &defaults),
            (PromptMode::Eco, PromptModeSource::Project)
        );
        assert_eq!(
            effective_prompt_mode(&pinned, &defaults),
            (PromptMode::Normal, PromptModeSource::Agent)
        );
        assert_eq!(
            serde_json::to_value(PromptModeSource::Project).unwrap(),
            json!("project")
        );
        assert_eq!(
            serde_json::to_value(PromptModeSource::Agent).unwrap(),
            json!("agent")
        );
    }

    #[test]
    fn config_set_can_switch_the_project_mode() {
        let mut value = serde_json::to_value(default_config()).unwrap();
        set_by_path(&mut value, "defaults.agent.prompt_mode", json!("eco"));
        let config: OrchestratorConfig = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(config.defaults.agent.prompt_mode, PromptMode::Eco);
        set_by_path(&mut value, "defaults.agent.prompt_mode", json!("inherit"));
        assert!(serde_json::from_value::<OrchestratorConfig>(value).is_err());
    }
}
