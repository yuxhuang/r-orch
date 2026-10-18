//! Agent shop arguments and dispatch, shared by legacy and directory templates.

use clap::Args;
use orch_core::error::Result;
use orch_engine::Engine;

use crate::context::CliContext;
use crate::output::Styles;

#[derive(Debug, Args)]
pub struct ShopArgs {
    #[arg(long, help = "Print all templates (non-interactive)")]
    pub list: bool,
}

pub async fn run(
    engine: &Engine,
    _ctx: &CliContext,
    styles: &Styles,
    args: &ShopArgs,
    stdout_is_tty: bool,
    stdin_is_tty: bool,
) -> Result<()> {
    crate::commands::agent::shop(engine, styles, args.list, stdout_is_tty, stdin_is_tty).await
}
