//! `orch agent`.
//!
//! State and lifecycle operations run through the daemon. Pure rendering and
//! shop-template expansion remain in this presentation layer.

use crate::commands::rpc::{Rpc, argument_error, arguments, client_error};
use orch_core::agent_factory::template_to_agent_input;
use orch_core::error::Result;
use orch_domain::{
    AGENT_SHOP_TEMPLATES, Agent, CreateAgentInput, EFFORT_REQUEST_ONLY_NOTE, EffortSelection,
    PromptModeSelection, PromptModeSource,
};
use orch_proto::ops::results::AgentDiagnostic;
use orch_proto::ops::{args, results};
use serde_json::json;

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

fn effort_diagnostics(agents: &[Agent]) -> Vec<AgentDiagnostic> {
    orch_domain::agent_effort_diagnostics(agents)
        .into_iter()
        .map(|d| AgentDiagnostic {
            agent_id: d.agent_id,
            agent_name: d.agent_name,
            message: d.message,
        })
        .collect()
}

/// Human description of the mode resolved by the daemon.
fn describe_mode(config: results::EffectiveConfig, agent: &Agent) -> String {
    let mode = config.prompt_mode;
    let source = config.prompt_mode_source;
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    args: AgentAddArgs,
) -> Result<()> {
    let authored = create_authored(engine, args).await?;
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

/// Create through the daemon's definition-validation lane, without printing.
pub(crate) async fn create_authored(
    engine: &Rpc,
    args: AgentAddArgs,
) -> Result<results::AuthoredAgent> {
    engine
        .call::<args::AgentCreateArgs>(agent_input(args))
        .await
}

fn agent_input(args: AgentAddArgs) -> CreateAgentInput {
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
    }
}

/// `orch agent list`.
///
/// The TIME column is **hard-coded** to a dim em dash: it is never
/// computed.
pub async fn list(engine: &Rpc, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let agents = engine.request(args::AgentListArgs {}).await?;

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
    let diagnostics = effort_diagnostics(&agents);
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
pub async fn set_enabled(engine: &Rpc, styles: &Styles, id: &str, enabled: bool) -> Result<()> {
    if enabled {
        engine
            .call::<args::AgentEnableArgs>(json!({"id": id}))
            .await?;
        print_success(styles, &format!("Enabled agent {id}"));
    } else {
        engine
            .call::<args::AgentDisableArgs>(json!({"id": id}))
            .await?;
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    effective: bool,
) -> Result<()> {
    // Human output always includes the resolved prompt mode.
    let view = engine
        .call::<args::AgentGetArgs>(json!({"id": id, "effective": true}))
        .await?;
    let agent = &view.agent;
    let diagnostics = effort_diagnostics(std::slice::from_ref(agent));

    if let Some(format) = ctx.output {
        if effective {
            format.print(&view)?;
        } else {
            format.print(agent)?;
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
    pairs.push((
        "Prompt mode".to_owned(),
        describe_mode(
            view.effective_config.expect("requested effective config"),
            agent,
        ),
    ));
    if !is_shell {
        pairs.push((
            "Role hint".to_owned(),
            match agent.config.role_summary.as_deref() {
                Some(summary) => summary.to_owned(),
                None => format!(
                    "{} {}",
                    orch_domain::role_summary_for(agent),
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
/// Omitted fields stay unchanged; empty optional fields are cleared. A digest
/// check rejects a save if another editor changed the authored definition.
pub async fn edit(engine: &Rpc, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let base = engine
        .call::<args::AgentDefinitionGetArgs>(json!({"id": id}))
        .await?;
    let value: serde_json::Value = serde_json::from_str(&base.document).map_err(argument_error)?;
    let initial = crate::editor::agent_buffer(
        value["name"].as_str().expect("agent definition name"),
        value["config"]["command"].as_str().unwrap_or_default(),
        value["config"]["model"].as_str().unwrap_or_default(),
        value["role"].as_str().unwrap_or_default(),
    );
    let edited = crate::editor::open_in_editor(&initial, ".yml").await?;
    let parsed = crate::editor::parse_agent_content(&edited);
    let patch = editor_patch(parsed);
    let updated = definition_request(
        engine,
        id,
        &patch,
        crate::document_input::MutationKind::Patch,
        Some(base.digest),
    )
    .await?
    .agent;

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

fn editor_patch(parsed: crate::editor::AgentEdit) -> serde_json::Value {
    let mut patch = serde_json::Map::new();
    let mut config = serde_json::Map::new();
    if let Some(name) = parsed.name {
        patch.insert("name".into(), json!(name.trim()));
    }
    if let Some(role) = parsed.role {
        patch.insert(
            "role".into(),
            if role.is_empty() {
                serde_json::Value::Null
            } else {
                json!(role)
            },
        );
    }
    if let Some(command) = parsed.command {
        config.insert(
            "command".into(),
            if command.trim().is_empty() {
                serde_json::Value::Null
            } else {
                json!(command.trim())
            },
        );
    }
    if let Some(model) = parsed.model {
        config.insert(
            "model".into(),
            if model.is_empty() {
                serde_json::Value::Null
            } else {
                json!(model)
            },
        );
    }
    patch.insert("config".into(), config.into());
    patch.into()
}

pub async fn mutate(
    engine: &Rpc,
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
    engine: &Rpc,
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

/// The daemon merges and validates the authored definition atomically.
/// `source` attributes input failures while retaining typed transport errors.
pub(crate) async fn apply_definition(
    engine: &Rpc,
    id: &str,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
    source: &(dyn Fn(orch_core::Error) -> orch_core::Error + Sync),
) -> Result<results::AuthoredAgent> {
    use orch_core::Error;
    if id.contains('@') {
        return Err(source(Error::invalid_arguments(
            "id: remote agents cannot be mutated",
        )));
    }
    definition_request(engine, id, input, kind, None)
        .await
        .map_err(source)
}

/// Keep large authored JSON out of the single-frame lane. Upload and apply
/// share one session lock because transfer tokens are connection-local.
async fn definition_request(
    engine: &Rpc,
    id: &str,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
    base_digest: Option<orch_proto::bounded::Id>,
) -> Result<results::AuthoredAgent> {
    let document = serde_json::to_string(input).map_err(argument_error)?;
    let large = document.len() > orch_proto::frame::MAX_FRAME_BYTES / 4;
    let mut request: args::AgentDefinitionApplyArgs = arguments(json!({
        "id": id,
        "definition": {
            "input": {"document": if large { None } else { Some(&document) }, "format": "json"},
            "kind": match kind { crate::document_input::MutationKind::Update => "update", crate::document_input::MutationKind::Patch => "patch" },
            "base_digest": base_digest,
        }
    }))?;
    if !large {
        return engine.request(request).await;
    }
    let mut lane = engine.lane().await;
    request.definition.input.input_transfer = Some(
        lane.upload(args::TransferPurpose::Document, document.as_bytes())
            .await
            .map_err(client_error)?,
    );
    lane.request(request).await.map_err(client_error)
}

/// `orch agent remove`. Ignores `--json` and `--quiet`.
pub async fn remove(engine: &Rpc, styles: &Styles, id: &str) -> Result<()> {
    engine
        .call::<args::AgentRemoveArgs>(json!({"id": id}))
        .await?;
    print_success(styles, &format!("Removed agent {id}"));
    Ok(())
}

/// `orch agent autonomous` — `--on`, `--off`, or neither to toggle.
///
/// `--on` wins when both are given.
pub async fn autonomous(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    on: bool,
    off: bool,
) -> Result<()> {
    let enabled = if on {
        Some(true)
    } else if off {
        Some(false)
    } else {
        None
    };
    let agent = engine
        .call::<args::AgentAutonomousArgs>(json!({"id": id, "enabled": enabled}))
        .await?;
    let enable = agent.autonomous.unwrap_or(false);

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
    engine: &Rpc,
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

    let config = engine
        .call::<args::ConfigGetArgs>(json!({"key": "defaults.agent.adapter"}))
        .await?;
    let adapter = config
        .value
        .as_ref()
        .and_then(|value| value.as_str())
        .ok_or_else(|| orch_core::Error::other("project default agent adapter is missing"))?;
    let input = template_to_agent_input(template, adapter)?;
    let agent = engine.call::<args::AgentCreateArgs>(input).await?.agent;
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
    use crate::commands::rpc::arguments;

    fn args(name: &str, adapter: &str) -> AgentAddArgs {
        AgentAddArgs {
            name: name.into(),
            adapter: adapter.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_shell_command_survives_bounded_rpc_conversion() {
        let input = agent_input(AgentAddArgs {
            command: Some("echo hello".into()),
            ..args("shelly", "shell")
        });
        let wire: args::AgentCreateArgs = arguments(input).unwrap();
        assert_eq!(wire.name.as_str(), "shelly");
        assert_eq!(wire.adapter.as_str(), "shell");
        assert_eq!(wire.command.as_deref(), Some("echo hello"));
    }

    #[test]
    fn skills_are_split_on_commas_and_trimmed() {
        let input = agent_input(AgentAddArgs {
            skills: Some("review, feature-dev:code-explorer".into()),
            ..args("claudia", "claude")
        });
        let wire: args::AgentCreateArgs = arguments(input).unwrap();
        let skills: Vec<_> = wire
            .skills
            .unwrap()
            .into_inner()
            .into_iter()
            .map(|s| s.into_inner())
            .collect();
        assert_eq!(skills, ["review", "feature-dev:code-explorer"]);
    }

    #[test]
    fn agent_input_is_bounded_before_the_daemon_request() {
        let input = agent_input(args(
            &"a".repeat(orch_proto::bounded::MAX_SHORT_TEXT_BYTES + 1),
            "codex",
        ));
        assert!(arguments::<args::AgentCreateArgs>(input).is_err());
    }

    #[test]
    fn editor_patch_clears_optional_fields_and_preserves_omissions() {
        let patch = editor_patch(crate::editor::parse_agent_content(
            "---\nname: renamed\ncommand:\nmodel:\n---\n",
        ));
        assert_eq!(
            patch,
            json!({"name":"renamed", "role":null, "config":{"command":null,"model":null}})
        );
        assert_eq!(
            editor_patch(crate::editor::AgentEdit::default()),
            json!({"config":{}})
        );
    }
}
