//! Legacy shop forwarding and directory-template installation.
use crate::commands::rpc::{Rpc, arguments};
use crate::context::CliContext;
use crate::output::{Styles, print_table};
use crate::shop_picker::{ShopEntryView, pick_index};
use clap::Args;
use orch_core::Result;
use orch_domain::CreateAgentInput;
use orch_proto::ops::args;
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
    engine: &Rpc,
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
    // Relative to the caller's cwd, never the daemon's, and lossless on the
    // wire (the protocol's tagged path, not a display string).
    let template_dir = args
        .template_dir
        .as_deref()
        .map(|path| {
            std::path::absolute(path)
                .map_err(crate::commands::rpc::argument_error)
                .and_then(|path| {
                    orch_proto::path::WirePath::new(path)
                        .map_err(crate::commands::rpc::argument_error)
                })
        })
        .transpose()?;
    let loaded = engine
        .call::<args::OrgTemplateShowArgs>(serde_json::json!({
            "selector": { "name":args.template, "template_dir":template_dir },
            "resolved":true,
        }))
        .await?;
    let inputs = loaded
        .resolved_agents
        .clone()
        .ok_or_else(|| orch_core::Error::other("daemon omitted resolved template agents"))?;
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    input: CreateAgentInput,
) -> Result<()> {
    let agent = engine
        .request(arguments::<args::AgentCreateArgs>(input)?)
        .await?
        .agent;
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
