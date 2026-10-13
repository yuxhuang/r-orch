//! `orch agent` — ported from `src/cli/commands/agent.ts`.
//!
//! WP-0b covers `add`. The remaining subcommands (`list`, `show`, `shop`,
//! `edit`, `remove`, `disable`, `enable`, `autonomous`) are WP-5's.

use orch_core::error::Result;
use orch_domain::CreateAgentInput;
use orch_engine::Engine;

use crate::context::CliContext;
use crate::output::{Styles, print_json, print_success};

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
