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
use crate::output::{Styles, format_tokens, print_json, print_success, print_table};
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
    let agents = engine.agent_service.list().await?;

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
        &["STATUS", "AGENT", "ADAPTER", "TASK", "TIME"],
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
