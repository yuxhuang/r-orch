//! Prompt-mode policy shared by every mutation surface and by dispatch.
//!
//! Three rules live here so the CLI, the TUI and the orchestrator apply them
//! identically:
//!
//! - **Custom templates and effective eco are incompatible** (design D1). A
//!   project `prompt.template`, `prompt.system_template` or
//!   `prompt.user_template` is an authored rendering contract that eco would
//!   silently replace, so any agent resolving to eco while one is set is
//!   rejected before a write, with every offender listed, and again at
//!   dispatch for hand-edited files. There is no silent normal fallback.
//! - **The project default is written key-only under the definition lock**
//!   ([`set_project_prompt_mode`]): the raw file is re-read inside the lock,
//!   one key changes, unknown extension keys survive, and the offender check
//!   runs on the final candidate before anything is written.
//! - **Effort diagnostics are read-only** ([`agent_effort_diagnostics`]):
//!   watch startup, show and list report what dispatch will do with a stored
//!   effort without spawning anything.
//!
//! Remote agents are excluded throughout: remote execution bypasses eco.

use std::path::Path;

use orch_core::error::{Error, Result};
use orch_core::storage::AgentStore;
use orch_domain::{
    Agent, AgentConfig, EffortDispatch, OrchestratorConfig, PromptConfig, PromptMode, deep_merge,
    default_config, effective_prompt_mode, plan_effort_dispatch, set_by_path,
};
use serde_json::Value;

/// The project prompt fields that make eco unavailable, in display order.
pub const CUSTOM_TEMPLATE_FIELDS: [&str; 3] = [
    "prompt.template",
    "prompt.system_template",
    "prompt.user_template",
];

/// The custom template fields set in `prompt`, as dot paths.
pub fn custom_template_fields(prompt: Option<&PromptConfig>) -> Vec<&'static str> {
    let Some(prompt) = prompt else {
        return Vec::new();
    };
    [
        prompt.template.is_some(),
        prompt.system_template.is_some(),
        prompt.user_template.is_some(),
    ]
    .into_iter()
    .zip(CUSTOM_TEMPLATE_FIELDS)
    .filter_map(|(set, field)| set.then_some(field))
    .collect()
}

/// One agent that would render in eco while custom templates are configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcoTemplateOffender {
    pub agent_id: String,
    pub agent_name: String,
    /// Whether eco comes from the agent's own override or the project default.
    pub inherited: bool,
}

/// Every local agent in `agents` that resolves to eco under `config` while a
/// custom template is set. Empty when no custom template is set.
pub fn eco_template_offenders(
    config: &OrchestratorConfig,
    agents: &[Agent],
) -> Vec<EcoTemplateOffender> {
    if custom_template_fields(config.prompt.as_ref()).is_empty() {
        return Vec::new();
    }
    agents
        .iter()
        .filter(|agent| agent.remote.is_none())
        .filter_map(|agent| {
            let (mode, source) = effective_prompt_mode(&agent.config, &config.defaults.agent);
            (mode == PromptMode::Eco).then(|| EcoTemplateOffender {
                agent_id: agent.id.clone(),
                agent_name: agent.name.clone(),
                inherited: source == orch_domain::PromptModeSource::Project,
            })
        })
        .collect()
}

/// The actionable rejection for a non-empty offender list.
pub fn eco_template_error(config: &OrchestratorConfig, offenders: &[EcoTemplateOffender]) -> Error {
    let fields = custom_template_fields(config.prompt.as_ref()).join(", ");
    let listed = offenders
        .iter()
        .map(|offender| {
            format!(
                "{} ({}, {})",
                offender.agent_name,
                offender.agent_id,
                if offender.inherited {
                    "inherits project eco"
                } else {
                    "agent override eco"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Error::invalid_arguments(format!(
        "eco prompt mode is incompatible with custom templates ({fields}); affected agents: \
         {listed}. Set those agents to `--prompt-mode normal`, or remove the custom templates."
    ))
}

/// Reject `config` + `agents` when any local agent would render eco against a
/// custom template. Used on final candidates before any write and again at
/// dispatch.
pub fn check_eco_template_compatibility(
    config: &OrchestratorConfig,
    agents: &[Agent],
) -> Result<()> {
    let offenders = eco_template_offenders(config, agents);
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(eco_template_error(config, &offenders))
    }
}

/// The same check for one agent definition candidate (add, update, replace,
/// patch, editor, wizard): only that agent is evaluated.
pub fn check_agent_candidate(
    config: &OrchestratorConfig,
    agent_id: &str,
    agent_name: &str,
    candidate: &AgentConfig,
) -> Result<()> {
    if custom_template_fields(config.prompt.as_ref()).is_empty() {
        return Ok(());
    }
    let (mode, source) = effective_prompt_mode(candidate, &config.defaults.agent);
    if mode != PromptMode::Eco {
        return Ok(());
    }
    Err(eco_template_error(
        config,
        &[EcoTemplateOffender {
            agent_id: agent_id.to_owned(),
            agent_name: agent_name.to_owned(),
            inherited: source == orch_domain::PromptModeSource::Project,
        }],
    ))
}

/// Validate a complete project-config candidate against the current local
/// agents. The config CLI's set/patch/update paths call this on their final
/// candidate inside the definition lock, before writing.
pub fn validate_project_config_candidate(
    candidate: &OrchestratorConfig,
    agents: &[Agent],
) -> Result<()> {
    check_eco_template_compatibility(candidate, agents)
}

/// The effective typed config of a raw `config.yml` value (merged over the
/// defaults, as the store reads it).
pub fn effective_config_of_raw(raw: &Value) -> Result<OrchestratorConfig> {
    let defaults = serde_json::to_value(default_config())
        .map_err(|err| Error::other(format!("serialize default config: {err}")))?;
    serde_json::from_value(deep_merge(&defaults, raw)).map_err(|_| {
        Error::invalid_arguments("config: missing required field or invalid known field type/enum")
    })
}

/// The project's `.definition.lock`, held, with the project config re-read
/// inside it.
///
/// Agent definition mutations validate their final candidate against
/// [`DefinitionLock::config`], never against a config snapshot taken before
/// the lock (an engine's construction-time config, or a long-lived TUI
/// session's): a project mode switch that ran in between is otherwise
/// invisible, and an "inherit" edit can be accepted against a project that is
/// now eco with a custom template. The lock is the same one the project mode
/// writers and `config set/patch/update` take, so the two sides serialize.
///
/// The lock is per open file, not re-entrant: a holder must not try to take it
/// again (that waits out the lock's timeout). Released on drop.
pub struct DefinitionLock {
    _file: std::fs::File,
    config: OrchestratorConfig,
}

impl DefinitionLock {
    /// Take the lock (bounded wait), then read the project config the same
    /// way the engine does at construction.
    pub async fn acquire(project_root: &Path) -> Result<Self> {
        use orch_core::storage::ConfigStore;
        use orch_infra::storage::{FileConfigStore, Paths, fs_utils};

        let paths = Paths::new(project_root.to_path_buf());
        let file = fs_utils::mutation_lock(&paths.root().join(".definition.lock")).await?;
        let config = FileConfigStore::new(paths).read().await?;
        Ok(DefinitionLock {
            _file: file,
            config,
        })
    }

    /// The project config as read inside the lock.
    pub fn config(&self) -> &OrchestratorConfig {
        &self.config
    }
}

/// Set `defaults.agent.prompt_mode` key-only, under the project's definition
/// lock.
///
/// The raw file is re-read inside the lock, one key is replaced, the merged
/// known config is validated, every local agent is checked for a custom
/// template conflict, and only then is the raw candidate published with the
/// store's owner-only atomic write. Unknown extension keys survive. A failure
/// at any step leaves the file untouched. An unchanged value writes nothing.
///
/// Running children are unaffected; dispatch reads the new value on its next
/// prompt assembly.
pub async fn set_project_prompt_mode(
    project_root: &Path,
    agents: &dyn AgentStore,
    mode: PromptMode,
) -> Result<PromptMode> {
    update_project_prompt_mode(project_root, agents, |_| mode).await
}

/// Flip the project default normal ↔ eco (the TUI's F3), with the same
/// locking and validation as [`set_project_prompt_mode`]. Returns the new
/// mode.
pub async fn toggle_project_prompt_mode(
    project_root: &Path,
    agents: &dyn AgentStore,
) -> Result<PromptMode> {
    update_project_prompt_mode(project_root, agents, PromptMode::toggled).await
}

async fn update_project_prompt_mode(
    project_root: &Path,
    agents: &dyn AgentStore,
    next: impl FnOnce(PromptMode) -> PromptMode,
) -> Result<PromptMode> {
    use orch_infra::storage::{FileConfigStore, Paths, fs_utils};

    let paths = Paths::new(project_root.to_path_buf());
    let _lock = fs_utils::mutation_lock(&paths.root().join(".definition.lock")).await?;
    let store = FileConfigStore::new(paths);
    let current_raw = store.read_raw().await?;
    let current = effective_config_of_raw(&current_raw)?;
    let mode = next(current.defaults.agent.prompt_mode);

    let mut candidate_raw = current_raw.clone();
    set_by_path(
        &mut candidate_raw,
        "defaults.agent.prompt_mode",
        Value::String(mode.as_str().to_owned()),
    );
    let candidate = effective_config_of_raw(&candidate_raw)?;
    let local_agents = agents.list().await?;
    validate_project_config_candidate(&candidate, &local_agents)?;
    if candidate_raw != current_raw {
        store.write_raw(&candidate_raw, false).await?;
    }
    Ok(mode)
}

/// One read-only diagnostic for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDiagnostic {
    pub agent_id: String,
    pub agent_name: String,
    pub message: String,
}

/// What dispatch will do with each agent's stored effort, without spawning
/// anything: warnings for legacy-ignored, clamp/override/unknown statuses and
/// the Codex managed-model coupling, and the configuration error a new
/// invalid value will raise. The plain request-only note is not reported.
///
/// OpenCode's bounded wire check is **not** run here; its outcome is reported
/// at dispatch, where the probe runs immediately before the spawn.
pub fn agent_effort_diagnostics(agents: &[Agent]) -> Vec<AgentDiagnostic> {
    let mut out = Vec::new();
    for agent in agents.iter().filter(|agent| agent.remote.is_none()) {
        let push = |out: &mut Vec<AgentDiagnostic>, message: String| {
            out.push(AgentDiagnostic {
                agent_id: agent.id.clone(),
                agent_name: agent.name.clone(),
                message,
            });
        };
        match plan_effort_dispatch(
            &agent.adapter,
            agent.config.model.as_deref(),
            agent.config.effort,
        ) {
            EffortDispatch::Unset => {}
            EffortDispatch::Reject(error) => {
                push(&mut out, format!("configuration error: {error}"))
            }
            EffortDispatch::IgnoreLegacy { diagnostics }
            | EffortDispatch::Forward { diagnostics, .. } => {
                for diagnostic in diagnostics.into_iter().filter(|d| d.is_warning()) {
                    push(&mut out, diagnostic.to_string());
                }
            }
        }
    }
    out
}

/// Every read-only startup diagnostic: effort, plus the eco/custom-template
/// conflict dispatch will refuse.
pub fn startup_diagnostics(config: &OrchestratorConfig, agents: &[Agent]) -> Vec<AgentDiagnostic> {
    let mut out = agent_effort_diagnostics(agents);
    let offenders = eco_template_offenders(config, agents);
    if !offenders.is_empty() {
        let fields = custom_template_fields(config.prompt.as_ref()).join(", ");
        out.extend(offenders.into_iter().map(|offender| AgentDiagnostic {
            message: format!(
                "configuration error: eco prompt mode is incompatible with custom templates \
                 ({fields}); set this agent to `--prompt-mode normal` or remove the custom \
                 templates"
            ),
            agent_id: offender.agent_id,
            agent_name: offender.agent_name,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{FakeAgentStore, make_agent};
    use orch_domain::{ReasoningEffort, RemoteRef};

    fn agent(id: &str, mode: Option<PromptMode>) -> Agent {
        let mut agent = make_agent();
        agent.id = id.into();
        agent.name = format!("name-{id}");
        agent.config.prompt_mode = mode;
        agent
    }

    fn with_template(mut config: OrchestratorConfig) -> OrchestratorConfig {
        config.prompt = Some(PromptConfig {
            system_template: Some("custom".into()),
            ..PromptConfig::default()
        });
        config
    }

    #[test]
    fn no_custom_template_means_no_offenders() {
        let mut config = default_config();
        config.defaults.agent.prompt_mode = PromptMode::Eco;
        assert!(eco_template_offenders(&config, &[agent("a", None)]).is_empty());
    }

    #[test]
    fn every_eco_resolving_agent_is_listed_with_its_source() {
        let mut config = with_template(default_config());
        config.defaults.agent.prompt_mode = PromptMode::Eco;
        let agents = [
            agent("inherits", None),
            agent("override_normal", Some(PromptMode::Normal)),
            agent("override_eco", Some(PromptMode::Eco)),
        ];
        let offenders = eco_template_offenders(&config, &agents);
        assert_eq!(
            offenders
                .iter()
                .map(|o| (o.agent_id.as_str(), o.inherited))
                .collect::<Vec<_>>(),
            [("inherits", true), ("override_eco", false)]
        );
        let message = eco_template_error(&config, &offenders).to_string();
        assert!(message.contains("prompt.system_template"), "{message}");
        assert!(message.contains("name-inherits (inherits"), "{message}");
        assert!(
            message.contains("name-override_eco (override_eco"),
            "{message}"
        );
        assert!(message.contains("--prompt-mode normal"), "{message}");
    }

    #[test]
    fn remote_agents_bypass_the_eco_template_check() {
        let mut config = with_template(default_config());
        config.defaults.agent.prompt_mode = PromptMode::Eco;
        let mut remote = agent("r@peer", None);
        remote.remote = Some(RemoteRef {
            alias: "peer".into(),
            online: true,
        });
        assert!(check_eco_template_compatibility(&config, &[remote]).is_ok());
    }

    #[test]
    fn an_agent_candidate_is_checked_alone() {
        let config = with_template(default_config());
        let eco = AgentConfig {
            prompt_mode: Some(PromptMode::Eco),
            ..AgentConfig::default()
        };
        assert!(check_agent_candidate(&config, "a", "A", &eco).is_err());
        assert!(check_agent_candidate(&config, "a", "A", &AgentConfig::default()).is_ok());
        assert!(check_agent_candidate(&default_config(), "a", "A", &eco).is_ok());
    }

    #[test]
    fn effort_diagnostics_skip_unset_and_plain_request_notes() {
        let mut unset = make_agent();
        unset.adapter = "claude".into();
        let mut known = unset.clone();
        known.id = "known".into();
        known.config.model = Some("claude-opus-5-5".into());
        known.config.effort = Some(ReasoningEffort::High);
        assert!(agent_effort_diagnostics(&[unset, known]).is_empty());
    }

    #[test]
    fn effort_diagnostics_report_legacy_ignores_and_new_value_errors() {
        let mut cursor = make_agent();
        cursor.id = "cursor".into();
        cursor.adapter = "cursor".into();
        cursor.config.effort = Some(ReasoningEffort::High);
        let mut codex_ultra = make_agent();
        codex_ultra.id = "codex".into();
        codex_ultra.adapter = "codex".into();
        codex_ultra.config.effort = Some(ReasoningEffort::Ultra);
        let diagnostics = agent_effort_diagnostics(&[cursor, codex_ultra]);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert_eq!(diagnostics[0].agent_id, "cursor");
        assert!(
            diagnostics[0].message.starts_with("legacy-ignored"),
            "{diagnostics:?}"
        );
        assert_eq!(diagnostics[1].agent_id, "codex");
        assert!(
            diagnostics[1].message.starts_with("configuration error:"),
            "{diagnostics:?}"
        );
    }

    #[tokio::test]
    async fn the_project_mode_toggle_is_key_only_and_preserves_extensions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".orchestry")).unwrap();
        let paths = orch_infra::storage::Paths::new(root.to_path_buf());
        let original = "project:\n  name: proj\nmy_extension:\n  keep: [1, 2]\n";
        std::fs::write(paths.config_path(), original).unwrap();
        let agents = FakeAgentStore::with_agents([agent("a", None)]);

        let mode = toggle_project_prompt_mode(root, &agents).await.unwrap();
        assert_eq!(mode, PromptMode::Eco);
        let written = std::fs::read_to_string(paths.config_path()).unwrap();
        assert!(written.contains("my_extension"), "{written}");
        assert!(written.contains("prompt_mode: eco"), "{written}");
        // Defaults were not materialized: only the toggled key was added.
        assert!(!written.contains("max_turns"), "{written}");

        assert_eq!(
            toggle_project_prompt_mode(root, &agents).await.unwrap(),
            PromptMode::Normal
        );
        assert_eq!(
            set_project_prompt_mode(root, &agents, PromptMode::Normal)
                .await
                .unwrap(),
            PromptMode::Normal
        );
    }

    #[tokio::test]
    async fn a_conflicting_toggle_lists_offenders_and_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".orchestry")).unwrap();
        let paths = orch_infra::storage::Paths::new(root.to_path_buf());
        let original = "project:\n  name: proj\nprompt:\n  user_template: custom\n";
        std::fs::write(paths.config_path(), original).unwrap();
        let agents =
            FakeAgentStore::with_agents([agent("a", None), agent("b", Some(PromptMode::Normal))]);

        let err = toggle_project_prompt_mode(root, &agents)
            .await
            .expect_err("conflict");
        let message = err.to_string();
        assert!(
            message.contains("name-a (a, inherits project eco)"),
            "{message}"
        );
        assert!(!message.contains("name-b"), "{message}");
        assert_eq!(
            std::fs::read_to_string(paths.config_path()).unwrap(),
            original
        );
    }
}
