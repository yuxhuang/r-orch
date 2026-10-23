//! Legacy shop forwarding and directory-template installation.
use crate::context::CliContext;
use crate::output::{Styles, print_table};
use crate::shop_picker::{ShopEntryView, pick_index};
use clap::Args;
use orch_core::Result;
use orch_domain::CreateAgentInput;
use orch_engine::Engine;
use orch_engine::prompt_mode::DefinitionLock;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ShopArgs {
    #[arg(long, help = "Print all templates (non-interactive)")]
    pub list: bool,
    #[arg(long, conflicts_with = "template_dir")]
    pub template: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub template_dir: Option<PathBuf>,
}

pub async fn run(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: &ShopArgs,
    stdout_is_tty: bool,
    stdin_is_tty: bool,
) -> Result<()> {
    if args.template.is_none() && args.template_dir.is_none() {
        return crate::commands::agent::shop(
            engine,
            styles,
            args.list,
            stdout_is_tty,
            stdin_is_tty,
        )
        .await;
    }
    let loaded = orch_infra::storage::org_templates::resolve(
        args.template.as_deref(),
        args.template_dir.as_deref(),
    )
    .await?;
    let inputs = orch_engine::org_templates::resolve_inputs(
        &loaded.bundle,
        &engine.config,
        None,
        None,
        orch_infra::storage::org_templates::validate_record_size,
    )?;
    if let Some(format) = ctx.output {
        return format.print(&loaded.bundle.agents);
    }
    let summaries = inputs
        .iter()
        .map(|i| {
            format!(
                "{}{}",
                i.adapter,
                i.model
                    .as_ref()
                    .map(|m| format!("/{m}"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    if args.list || !stdout_is_tty || !stdin_is_tty {
        let rows = loaded
            .bundle
            .agents
            .iter()
            .zip(&summaries)
            .map(|(a, summary)| {
                vec![
                    a.name.clone(),
                    a.description.clone(),
                    summary.clone(),
                    a.skills.as_ref().map(|s| s.join(", ")).unwrap_or_default(),
                ]
            })
            .map(|row| {
                row.into_iter()
                    .map(|cell| crate::shop_picker::sanitize(&cell))
                    .collect()
            })
            .collect::<Vec<_>>();
        print_table(
            styles,
            &["NAME", "DESCRIPTION", "ADAPTER/MODEL", "SKILLS"],
            &rows,
            2,
        );
        return Ok(());
    }
    let entries = loaded
        .bundle
        .agents
        .iter()
        .zip(&summaries)
        .map(|(a, summary)| ShopEntryView {
            name: &a.name,
            description: &a.description,
            summary,
        })
        .collect::<Vec<_>>();
    let Some(index) = pick_index(&entries, stdin_is_tty) else {
        return Ok(());
    };
    let input = inputs.into_iter().nth(index).expect("picker index");
    install(engine, ctx, styles, input).await
}

/// Create the picked template agent.
///
/// The definition lock is taken here, after the interactive picker, and held
/// only around the preflight and the create: the candidate is checked
/// against the project config read inside the lock, not the engine's
/// construction snapshot taken before the (human-scale) pick.
pub async fn install(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    input: CreateAgentInput,
) -> Result<()> {
    let lock = DefinitionLock::acquire(engine.paths.project_root()).await?;
    orch_engine::org_templates::preflight(engine, std::slice::from_ref(&input)).await?;
    let agent = engine
        .agent_service
        .create_locked(&lock, input)
        .await?
        .agent;
    drop(lock);
    if ctx.quiet {
        println!("{}", agent.id);
    } else {
        crate::output::print_success(
            styles,
            &format!("Added agent {} → {}", agent.name, agent.id),
        );
    }
    Ok(())
}
