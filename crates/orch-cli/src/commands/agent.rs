//! `orch agent`.
//!
//! WP-0b covers `add`; WP-5a adds `shop`. The remaining subcommands
//! (`list`, `status`, `edit`, `remove`, `disable`, `enable`, `autonomous`)
//! need WP-4's service methods.

use orch_core::error::Result;
use orch_domain::{
    AGENT_SHOP_TEMPLATES, Agent, CreateAgentInput, EFFORT_REQUEST_ONLY_NOTE, EffortSelection,
    PromptModeSelection, PromptModeSource, effective_prompt_mode,
};
use orch_engine::Engine;
use orch_engine::agent_factory::template_to_agent_input;
use orch_engine::prompt_mode::{AgentDiagnostic, DefinitionLock, agent_effort_diagnostics};
use serde::Serialize;

use crate::context::CliContext;
use crate::output::{
    Styles, format_tokens, print_key_value, print_success, print_table, print_warning,
};
use crate::shop_picker;

/// Where read-only effort diagnostics go. Human output keeps them inline as
/// `⚠` lines; with `--json`/`--yaml` or `--quiet` they go to **stderr**, so a
/// script parsing stdout never sees them (design D5.4).
pub(crate) fn report_diagnostics(
    ctx: &CliContext,
    styles: &Styles,
    diagnostics: &[AgentDiagnostic],
) {
    for diagnostic in diagnostics {
        let line = format!(
            "{} ({}): {}",
            diagnostic.agent_name, diagnostic.agent_id, diagnostic.message
        );
        if ctx.output.is_some() || ctx.quiet {
            eprintln!("warning: {line}");
        } else {
            print_warning(styles, &line);
        }
    }
}

/// The resolved prompt mode of an agent for `agent show --effective`.
#[derive(Debug, Serialize)]
struct EffectiveConfig {
    prompt_mode: orch_domain::PromptMode,
    prompt_mode_source: PromptModeSource,
}

/// `agent show --effective` with `--json`/`--yaml`: the ordinary agent shape,
/// unchanged, under `agent`, plus the resolution under `effective_config`.
#[derive(Debug, Serialize)]
struct EffectiveAgent<'a> {
    agent: &'a Agent,
    effective_config: EffectiveConfig,
}

/// Human description of a resolved mode: `eco (agent override)`,
/// `normal (project default)`.
fn describe_mode(engine: &Engine, agent: &Agent) -> String {
    let (mode, source) = effective_prompt_mode(&agent.config, &engine.config.defaults.agent);
    let origin = match source {
        PromptModeSource::Agent => "agent override",
        PromptModeSource::Project => "project default",
    };
    if agent.remote.is_some() {
        // Remote execution bypasses eco processing entirely.
        format!("{mode} ({origin}; not applied to remote runs)")
    } else {
        format!("{mode} ({origin})")
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentAddArgs {
    pub name: String,
    pub adapter: String,
    pub role: Option<String>,
    pub command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<orch_domain::ReasoningEffort>,
    /// `--prompt-mode`; `inherit` and omission store no override.
    pub prompt_mode: Option<PromptModeSelection>,
    pub role_summary: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub approval_policy: Option<orch_domain::ApprovalPolicy>,
    pub workspace_mode: Option<orch_domain::WorkspaceMode>,
    pub skills: Option<String>,
}

pub async fn add(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: AgentAddArgs,
) -> Result<()> {
    // The final candidate is checked against the project config read inside
    // the definition lock, not the engine's construction snapshot.
    let lock = DefinitionLock::acquire(engine.paths.project_root()).await?;
    let authored = engine
        .agent_service
        .create_locked(
            &lock,
            CreateAgentInput {
                name: args.name,
                adapter: args.adapter,
                role: args.role,
                command: args.command,
                model: args.model,
                effort: args.effort,
                approval_policy: args.approval_policy,
                max_turns: args.max_turns,
                timeout_ms: args.timeout_ms,
                // `agent add` has no `--stall-timeout` flag; the default applies.
                stall_timeout_ms: None,
                env: None,
                system_prompt: None,
                workspace_mode: args.workspace_mode,
                skills: args
                    .skills
                    .as_deref()
                    .map(|raw| raw.split(',').map(|part| part.trim().to_owned()).collect()),
                // Absent means live inheritance of the project default: the
                // default is never copied into the agent (design D1).
                prompt_mode: args
                    .prompt_mode
                    .and_then(PromptModeSelection::into_override),
                role_summary: args.role_summary,
            },
        )
        .await?;
    drop(lock);
    let agent = authored.agent;

    if let Some(format) = ctx.output {
        format.print(&agent)?;
    } else if ctx.quiet {
        println!("{}", agent.id);
    } else {
        print_success(
            styles,
            &format!(
                "Added agent {} ({}) → {}",
                styles.agent_name(&agent.name),
                agent.adapter,
                agent.id
            ),
        );
    }
    report_diagnostics(ctx, styles, &authored.diagnostics);
    Ok(())
}

/// `orch agent list`.
///
/// The TIME column is **hard-coded** to a dim em dash: it is never
/// computed.
pub async fn list(engine: &Engine, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let agents = orch_infra::storage::remote::RemoteAgentDirectory::new(engine.paths.clone())
        .list(engine.agent_service.list().await?)
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&agents);
    }
    if ctx.quiet {
        for agent in &agents {
            println!("{}", agent.id);
        }
        return Ok(());
    }
    if agents.is_empty() {
        println!(
            "\n  No agents. Add one: {}\n",
            styles.dim("orch agent add <name> --adapter <adapter>")
        );
        return Ok(());
    }

    println!();
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|agent| agent_row(styles, agent))
        .collect();
    print_table(
        styles,
        &[
            "STATUS", "AGENT", "ADAPTER", "CAPS", "WHERE", "TASK", "TIME",
        ],
        &rows,
        2,
    );
    println!("\n  {}\n", summary_line(styles, &agents));
    let diagnostics = agent_effort_diagnostics(&agents);
    if !diagnostics.is_empty() {
        report_diagnostics(ctx, styles, &diagnostics);
        println!();
    }
    Ok(())
}

fn agent_row(styles: &Styles, agent: &orch_domain::Agent) -> Vec<String> {
    vec![
        format!(
            "{} {}",
            styles.status_icon(agent.status.as_str()),
            agent.status
        ),
        styles.agent_name(&agent.name),
        agent.adapter.clone(),
        agent.capabilities.join(","),
        agent
            .remote
            .as_ref()
            .map(|remote| {
                format!(
                    "remote {} {}",
                    remote.alias,
                    if remote.online { "online" } else { "offline" }
                )
            })
            .unwrap_or_else(|| "local".into()),
        agent
            .current_task
            .clone()
            .unwrap_or_else(|| styles.dim("—")),
        // Never computed.
        styles.dim("—"),
    ]
}

/// `<n> agents · <r> running · <t> tokens total` — every clause always
/// present, unlike `task list`'s.
fn summary_line(_styles: &Styles, agents: &[orch_domain::Agent]) -> String {
    let running = agents
        .iter()
        .filter(|agent| agent.status == orch_domain::AgentStatus::Running)
        .count();
    let tokens: u64 = agents
        .iter()
        .map(|agent| agent.stats.tokens_used.unwrap_or(0))
        .sum();
    format!(
        "{} agents · {running} running · {} tokens total",
        agents.len(),
        format_tokens(tokens)
    )
}

/// `orch agent disable` / `orch agent enable`. Neither honours `--quiet`.
pub async fn set_enabled(engine: &Engine, styles: &Styles, id: &str, enabled: bool) -> Result<()> {
    if enabled {
        engine.agent_service.enable(id).await?;
        print_success(styles, &format!("Enabled agent {id}"));
    } else {
        engine.agent_service.disable(id).await?;
        print_success(styles, &format!("Disabled agent {id}"));
    }
    Ok(())
}

/// `orch agent status` — one agent's details and stats.
///
/// The pair list is built by **splicing**: a shell agent gets `Command` in
/// slot 1 and no `Effort`, everything else gets `Effort` in slot 2, so the
/// order is adapter/command/status/policy for shell and
/// adapter/status/effort/policy otherwise.
pub async fn status(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    effective: bool,
) -> Result<()> {
    let agent = orch_infra::storage::remote::RemoteAgentDirectory::new(engine.paths.clone())
        .resolve(id, engine.agent_service.list().await?)
        .await?
        .ok_or_else(|| orch_core::Error::invalid_arguments(format!("Agent not found: {id}")))?;
    let diagnostics = agent_effort_diagnostics(std::slice::from_ref(&agent));

    if let Some(format) = ctx.output {
        if effective {
            let (prompt_mode, prompt_mode_source) =
                effective_prompt_mode(&agent.config, &engine.config.defaults.agent);
            format.print(&EffectiveAgent {
                agent: &agent,
                effective_config: EffectiveConfig {
                    prompt_mode,
                    prompt_mode_source,
                },
            })?;
        } else {
            format.print(&agent)?;
        }
        report_diagnostics(ctx, styles, &diagnostics);
        return Ok(());
    }

    println!("\n  {}", agent.name);
    println!("  {}", "═".repeat(42));
    println!();

    let is_shell = agent.adapter == "shell";
    let adapter = match agent.config.model.as_deref() {
        Some(model) if !is_shell => format!("{} ({model})", agent.adapter),
        _ => agent.adapter.clone(),
    };
    let status_line = format!(
        "{} {}",
        styles.status_icon(agent.status.as_str()),
        agent.status
    );
    let policy = agent
        .config
        .approval_policy
        .map(|policy| policy.as_str().to_owned())
        .unwrap_or_else(|| "auto".to_owned());

    let mut pairs = vec![("Adapter".to_owned(), adapter)];
    if is_shell {
        pairs.push((
            "Command".to_owned(),
            agent
                .config
                .command
                .clone()
                .unwrap_or_else(|| "—".to_owned()),
        ));
        pairs.push(("Status".to_owned(), status_line));
    } else {
        pairs.push(("Status".to_owned(), status_line));
        pairs.push((
            "Effort".to_owned(),
            agent
                .config
                .effort
                .map(|effort| format!("{} (requested)", effort.as_str()))
                .unwrap_or_else(|| "default".to_owned()),
        ));
    }
    pairs.push(("Prompt mode".to_owned(), describe_mode(engine, &agent)));
    if !is_shell {
        pairs.push((
            "Role hint".to_owned(),
            match agent.config.role_summary.as_deref() {
                Some(summary) => summary.to_owned(),
                None => format!(
                    "{} {}",
                    orch_domain::role_summary_for(&agent),
                    styles.dim("(derived)")
                ),
            },
        ));
    }
    pairs.push(("CAPS".to_owned(), agent.capabilities.join(", ")));
    pairs.push((
        "WHERE".to_owned(),
        agent
            .remote
            .as_ref()
            .map(|remote| {
                format!(
                    "remote {} {}",
                    remote.alias,
                    if remote.online { "online" } else { "offline" }
                )
            })
            .unwrap_or_else(|| "local".into()),
    ));
    pairs.push(("Policy".to_owned(), policy));
    if let Some(task) = &agent.current_task {
        pairs.push(("Task".to_owned(), task.clone()));
    }
    if let Some(role) = &agent.role {
        pairs.push(("Role".to_owned(), role.clone()));
    }
    if let Some(skills) = agent
        .config
        .skills
        .as_ref()
        .filter(|skills| !skills.is_empty())
    {
        pairs.push(("Skills".to_owned(), skills.join(", ")));
    }
    print_key_value(styles, &pairs);
    if agent.config.effort.is_some() && !is_shell {
        println!("\n  {}", styles.dim(EFFORT_REQUEST_ONLY_NOTE));
    }
    if !diagnostics.is_empty() {
        println!();
        report_diagnostics(ctx, styles, &diagnostics);
    }

    println!("\n  Stats\n  {}", "─".repeat(42));
    print_key_value(
        styles,
        &[
            (
                "Tasks completed".to_owned(),
                agent.stats.tasks_completed.to_string(),
            ),
            (
                "Tasks failed".to_owned(),
                agent.stats.tasks_failed.to_string(),
            ),
            ("Total runs".to_owned(), agent.stats.total_runs.to_string()),
            (
                "Tokens used".to_owned(),
                format_tokens(agent.stats.tokens_used.unwrap_or(0)),
            ),
        ],
    );

    println!();
    Ok(())
}

/// `orch agent edit` — round-trip name, command, model and role through
/// `$EDITOR`.
///
/// Unlike `task edit` this does **not** compare against the existing agent: it
/// passes whatever came back straight to the service, where `None` means
/// "unchanged" and `Some("")` clears the field.
pub async fn edit(engine: &Engine, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let agent = engine.agent_service.get(id).await?;
    let initial = crate::editor::agent_buffer(
        &agent.name,
        agent.config.command.as_deref().unwrap_or_default(),
        agent.config.model.as_deref().unwrap_or_default(),
        agent.role.as_deref().unwrap_or_default(),
    );

    let edited = crate::editor::open_in_editor(&initial, ".yml").await?;
    let parsed = crate::editor::parse_agent_content(&edited);

    let updated = engine
        .agent_service
        .update(
            id,
            orch_engine::agent_service::UpdateAgentFields {
                name: parsed.name,
                role: parsed.role,
                command: parsed.command,
                model: parsed.model,
                ..Default::default()
            },
        )
        .await?;

    if let Some(format) = ctx.output {
        format.print(&updated)?;
    } else if ctx.quiet {
        println!("{}", updated.id);
    } else {
        print_success(
            styles,
            &format!(
                "Updated agent {} ({})",
                styles.agent_name(&updated.name),
                updated.id
            ),
        );
    }
    Ok(())
}

const WRITABLE: &[&str] = &["name", "adapter", "role", "config"];
const READONLY: &[&str] = &[
    "id",
    "status",
    "current_task",
    "autonomous",
    "stats",
    "last_error",
    "capabilities",
    "remote",
];
const CONFIG_FIELDS: &[&str] = &[
    "command",
    "model",
    "effort",
    "approval_policy",
    "max_turns",
    "timeout_ms",
    "stall_timeout_ms",
    "env",
    "system_prompt",
    "workspace_mode",
    "skills",
    "prompt_mode",
    "role_summary",
];

fn definition_candidate(
    current: &serde_json::Value,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
) -> Result<serde_json::Value> {
    use crate::document_input::{MutationKind, apply_mutation, reject_nulls, writable_input};
    use orch_core::Error;
    let input = writable_input(input, current, WRITABLE, READONLY, kind)?;
    // Check keys before merge: an unknown null would otherwise disappear.
    if let Some(config) = input.get("config").and_then(|v| v.as_object()) {
        for key in config.keys() {
            if !CONFIG_FIELDS.contains(&key.as_str()) {
                return Err(Error::invalid_arguments(format!(
                    "config.{key}: unknown field"
                )));
            }
        }
    }
    let base = serde_json::Value::Object(
        current
            .as_object()
            .expect("serialized agent")
            .iter()
            .filter(|(key, _)| WRITABLE.contains(&key.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    if kind == MutationKind::Update {
        reject_nulls(&input, "agent")?;
    }
    let candidate = apply_mutation(&base, &input, kind)?;
    reject_nulls(&candidate, "agent")?;
    for field in ["name", "adapter"] {
        if !candidate
            .get(field)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
        {
            return Err(Error::invalid_arguments(format!(
                "{field}: required nonblank string"
            )));
        }
    }
    if candidate.get("role").is_some_and(|v| !v.is_string()) {
        return Err(Error::invalid_arguments("role: expected a string"));
    }
    let config = candidate
        .get("config")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::invalid_arguments("config: required object"))?;
    for (field, value) in config {
        let mut single = serde_json::Map::new();
        single.insert(field.clone(), value.clone());
        serde_json::from_value::<orch_domain::AgentConfig>(single.into()).map_err(|_| {
            Error::invalid_arguments(format!("config.{field}: invalid field type or enum"))
        })?;
    }
    Ok(candidate)
}

pub async fn mutate(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: &crate::tree::EntityDefinitionInput,
    kind: crate::document_input::MutationKind,
) -> Result<()> {
    let input =
        crate::document_input::read_document(&args.input.file, args.input.input_format).await?;
    let file = args.input.file.clone();
    let source = move |err| crate::document_input::at_source(&file, err);
    let updated = apply_definition(engine, &args.id, &input, kind, &source).await?;
    print_updated(ctx, styles, &updated.agent)?;
    report_diagnostics(ctx, styles, &updated.diagnostics);
    Ok(())
}

/// `agent update ID` with flags only. Each flag compiles to one key of an
/// RFC 7396 merge patch on `config`, applied through the same locked
/// final-candidate path as `agent patch`:
///
/// - `--prompt-mode inherit` → `prompt_mode: null` (the override is removed);
/// - `--effort default` → `effort: null` (generic effort cleared; a native
///   suffix in the model string is part of `model` and stays as authored);
/// - `--role-summary TEXT` → `role_summary: TEXT`, normalized by the service.
///
/// Everything else in the definition, including fields this binary does not
/// model, is left as stored.
#[derive(Debug, Clone, Default)]
pub struct AgentFlagUpdate {
    pub prompt_mode: Option<PromptModeSelection>,
    pub effort: Option<EffortSelection>,
    pub role_summary: Option<String>,
}

impl AgentFlagUpdate {
    /// The merge patch these flags stand for.
    pub fn to_patch(&self) -> serde_json::Value {
        let mut config = serde_json::Map::new();
        if let Some(mode) = self.prompt_mode {
            config.insert(
                "prompt_mode".into(),
                mode.into_override()
                    .map(|mode| serde_json::Value::String(mode.as_str().to_owned()))
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(effort) = self.effort {
            config.insert(
                "effort".into(),
                effort
                    .into_effort()
                    .map(|effort| serde_json::Value::String(effort.as_str().to_owned()))
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(summary) = &self.role_summary {
            config.insert(
                "role_summary".into(),
                serde_json::Value::String(summary.clone()),
            );
        }
        serde_json::json!({ "config": config })
    }
}

pub async fn update_flags(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    flags: AgentFlagUpdate,
) -> Result<()> {
    let updated = apply_definition(
        engine,
        id,
        &flags.to_patch(),
        crate::document_input::MutationKind::Patch,
        &|err| err,
    )
    .await?;
    print_updated(ctx, styles, &updated.agent)?;
    report_diagnostics(ctx, styles, &updated.diagnostics);
    Ok(())
}

fn print_updated(ctx: &CliContext, styles: &Styles, updated: &Agent) -> Result<()> {
    if let Some(format) = ctx.output {
        format.print(updated)?;
    } else if ctx.quiet {
        println!("{}", updated.id);
    } else {
        print_success(
            styles,
            &format!(
                "Updated agent {} ({})",
                styles.agent_name(&updated.name),
                updated.id
            ),
        );
    }
    Ok(())
}

/// The locked read → candidate → validate → single save path shared by the
/// file and flag forms. `source` attributes errors to the input document.
/// The service's final-candidate validation (effort table, eco versus custom
/// templates) runs before the save, against the project config re-read
/// inside the lock, so a rejection writes nothing and a concurrent project
/// mode switch is never missed.
async fn apply_definition(
    engine: &Engine,
    id: &str,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
    source: &(dyn Fn(orch_core::Error) -> orch_core::Error + Sync),
) -> Result<orch_engine::agent_service::AuthoredAgent> {
    use orch_core::Error;
    if id.contains('@') {
        return Err(source(Error::invalid_arguments(
            "id: remote agents cannot be mutated",
        )));
    }
    // One stable project lock also serializes duplicate-name validation between
    // new agent mutations. Existing edit/runtime writes retain their old race.
    let lock = DefinitionLock::acquire(engine.paths.project_root())
        .await
        .map_err(source)?;
    let current = engine.agent_service.get(id).await?;
    let value = serde_json::to_value(&current).map_err(|_| Error::other("serialize agent"))?;
    let candidate = definition_candidate(&value, input, kind).map_err(source)?;
    let config: orch_domain::AgentConfig = serde_json::from_value(candidate["config"].clone())
        .map_err(|_| Error::invalid_arguments("config: invalid definition"))?;
    let mut authored = current.clone();
    authored.name = candidate["name"].as_str().expect("validated").to_owned();
    authored.adapter = candidate["adapter"].as_str().expect("validated").to_owned();
    authored.role = candidate
        .get("role")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    authored.config = config.clone();
    // Serialization belongs to the CLI/infra boundary, not AgentService. The
    // service re-reads runtime state before saving; legacy writers may still race.
    orch_infra::storage::fs_utils::check_definition_write_size(&authored).map_err(source)?;
    engine
        .agent_service
        .replace_definition_locked(
            &lock,
            id,
            candidate["name"].as_str().expect("validated").to_owned(),
            candidate["adapter"].as_str().expect("validated").to_owned(),
            candidate
                .get("role")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            config,
        )
        .await
        .map_err(source)
}

/// `orch agent remove`. Ignores `--json` and `--quiet`.
pub async fn remove(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    engine.agent_service.remove(id).await?;
    print_success(styles, &format!("Removed agent {id}"));
    Ok(())
}

/// `orch agent autonomous` — `--on`, `--off`, or neither to toggle.
///
/// `--on` wins when both are given.
pub async fn autonomous(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    on: bool,
    off: bool,
) -> Result<()> {
    let current = engine.agent_service.get(id).await?;
    let enable = if on {
        true
    } else if off {
        false
    } else {
        !current.autonomous.unwrap_or(false)
    };

    let agent = engine.agent_service.set_autonomous(id, enable).await?;

    if let Some(format) = ctx.output {
        format.print(&agent)?;
    } else if ctx.quiet {
        println!("{}", agent.id);
    } else {
        print_success(
            styles,
            &format!(
                "Autonomous mode {} for agent {} ({})",
                if enable { "enabled" } else { "disabled" },
                styles.agent_name(&agent.name),
                agent.id
            ),
        );
    }
    Ok(())
}

/// `orch agent shop` — browse and install a pre-built template.
///
/// The table is printed when `--list` is given **or** stdout is not a TTY;
/// otherwise the picker runs. A cancelled pick prints `  Cancelled.` and is
/// still exit 0. No `--json`, and `--quiet` is ignored.
pub async fn shop(
    engine: &Engine,
    styles: &Styles,
    list: bool,
    stdout_is_tty: bool,
    stdin_is_tty: bool,
) -> Result<()> {
    if list || !stdout_is_tty {
        print!(
            "{}",
            shop_picker::render_list(styles, &AGENT_SHOP_TEMPLATES)
        );
        return Ok(());
    }

    let Some(template) = shop_picker::pick_from_shop(&AGENT_SHOP_TEMPLATES, stdin_is_tty) else {
        println!("  Cancelled.");
        return Ok(());
    };

    let input = template_to_agent_input(template, &engine.config.defaults.agent.adapter)?;
    let lock = DefinitionLock::acquire(engine.paths.project_root()).await?;
    let agent = engine
        .agent_service
        .create_locked(&lock, input)
        .await?
        .agent;
    drop(lock);
    print_success(
        styles,
        &format!(
            "Added agent {} ({}) → {}",
            styles.agent_name(&agent.name),
            agent.adapter,
            agent.id
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::EnvFlags;
    use orch_engine::{EngineSeams, build_light_with};
    use std::path::Path;
    use std::sync::Arc;

    async fn engine(root: &Path) -> Engine {
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        std::fs::create_dir_all(orch_infra::storage::Paths::new(root).root()).unwrap();
        build_light_with(
            root,
            EngineSeams {
                clock: Arc::new(orch_core::test_support::FixedClock::new()),
                ids: Arc::new(orch_core::test_support::SeqIdGenerator::new()),
                // Light tier only, so no global-config store is built.
                global_config_path: None,
            },
        )
        .await
        .unwrap()
    }

    fn ctx(root: &Path) -> CliContext {
        CliContext::new(
            root.to_path_buf(),
            None,
            false,
            true,
            false,
            EnvFlags::default(),
        )
    }

    fn args(name: &str, adapter: &str) -> AgentAddArgs {
        AgentAddArgs {
            name: name.into(),
            adapter: adapter.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn add_persists_a_shell_agent() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path()).await;
        add(
            &engine,
            &ctx(temp.path()),
            &Styles::plain(),
            AgentAddArgs {
                command: Some("echo hello".into()),
                ..args("shelly", "shell")
            },
        )
        .await
        .unwrap();

        let agent = engine.agent_service.get("agt_0000001").await.unwrap();
        assert_eq!(agent.name, "shelly");
        assert_eq!(agent.config.command.as_deref(), Some("echo hello"));
    }

    #[tokio::test]
    async fn skills_are_split_on_commas_and_trimmed() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path()).await;
        add(
            &engine,
            &ctx(temp.path()),
            &Styles::plain(),
            AgentAddArgs {
                skills: Some("review, feature-dev:code-explorer".into()),
                ..args("claudia", "claude")
            },
        )
        .await
        .unwrap();

        let agent = engine.agent_service.get("agt_0000001").await.unwrap();
        assert_eq!(
            agent.config.skills.as_deref(),
            Some(["review".to_owned(), "feature-dev:code-explorer".to_owned()].as_slice())
        );
    }

    #[tokio::test]
    async fn a_shell_agent_without_a_command_fails_with_exit_2() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path()).await;
        let err = add(
            &engine,
            &ctx(temp.path()),
            &Styles::plain(),
            args("shelly", "shell"),
        )
        .await
        .expect_err("rejected");
        assert_eq!(err.to_string(), "Shell agents require a command");
        assert_eq!(err.exit_code(), 2);
    }
}

#[cfg(test)]
mod definition_tests {
    use super::*;
    use crate::document_input::MutationKind;
    use serde_json::json;
    #[test]
    fn definition_projection_checks_unknown_null_and_equal_readonly_echoes() {
        let current = json!({"id":"agt_1", "status":"idle", "name":"old", "adapter":"codex", "config":{"model":"old"}, "role":"old"});
        let update = json!({"id":"agt_1", "name":"new", "adapter":"codex", "config":{}});
        let value = definition_candidate(&current, &update, MutationKind::Update).unwrap();
        assert!(value.get("role").is_none());
        assert!(value.get("id").is_none());
        for bad in [
            json!({"id":"agt_1"}),
            json!({"config":{"typo":null}}),
            json!({"typo":null}),
        ] {
            assert!(definition_candidate(&current, &bad, MutationKind::Patch).is_err());
        }
        assert!(
            definition_candidate(
                &current,
                &json!({"config":{"model":null}}),
                MutationKind::Patch
            )
            .unwrap()["config"]
                .get("model")
                .is_none()
        );
        assert!(
            definition_candidate(
                &current,
                &json!({"name":"n", "adapter":"a", "config":{"model":null}}),
                MutationKind::Update
            )
            .is_err()
        );
    }
}
