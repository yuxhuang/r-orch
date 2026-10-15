//! `orch agent` — ported from `src/cli/commands/agent.ts`.
//!
//! WP-0b covers `add`; WP-5a adds `shop`. The remaining subcommands
//! (`list`, `status`, `edit`, `remove`, `disable`, `enable`, `autonomous`)
//! need WP-4's service methods.

use orch_core::error::Result;
use orch_domain::{AGENT_SHOP_TEMPLATES, CreateAgentInput};
use orch_engine::Engine;
use orch_engine::agent_factory::template_to_agent_input;

use crate::context::CliContext;
use crate::output::{
    Styles, format_tokens, print_json, print_key_value, print_success, print_table,
};
use crate::shop_picker;

#[derive(Debug, Clone, Default)]
pub struct AgentAddArgs {
    pub name: String,
    pub adapter: String,
    pub role: Option<String>,
    pub command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<orch_domain::ReasoningEffort>,
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
    let agent = engine
        .agent_service
        .create(CreateAgentInput {
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
        })
        .await?;

    if ctx.json {
        print_json(&agent)?;
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
    Ok(())
}

/// `orch agent list`.
///
/// The TIME column is **hard-coded** to a dim em dash: the reference never
/// computes it (`docs/parity/cli.md` §22.7).
pub async fn list(engine: &Engine, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let agents = orch_infra::storage::remote::RemoteAgentDirectory::new(engine.paths.clone())
        .list(engine.agent_service.list().await?)
        .await?;

    if ctx.json {
        return print_json(&agents);
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
pub async fn status(engine: &Engine, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let agent = orch_infra::storage::remote::RemoteAgentDirectory::new(engine.paths.clone())
        .resolve(id, engine.agent_service.list().await?)
        .await?
        .ok_or_else(|| orch_core::Error::invalid_arguments(format!("Agent not found: {id}")))?;

    if ctx.json {
        return print_json(&agent);
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
                .map(|effort| effort.as_str().to_owned())
                .unwrap_or_else(|| "default".to_owned()),
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

    if ctx.json {
        print_json(&updated)?;
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

/// `orch agent remove`. Ignores `--json` and `--quiet`.
pub async fn remove(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    engine.agent_service.remove(id).await?;
    print_success(styles, &format!("Removed agent {id}"));
    Ok(())
}

/// `orch agent autonomous` — `--on`, `--off`, or neither to toggle.
///
/// `--on` wins when both are given, as the TS ternary chain does.
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

    if ctx.json {
        print_json(&agent)?;
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
/// still exit 0. No `--json`, and `--quiet` is ignored
/// (`docs/parity/cli.md` §5.4, §21.6).
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
    let agent = engine.agent_service.create(input).await?;
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
            false,
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
