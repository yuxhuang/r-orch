//! Dev tooling for r-orch. `cargo xtask <command>`.

mod crate_graph;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "r-orch development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Assert the crate layering from ARCHITECTURE §3.1.
    CrateGraph,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = repo_root()?;

    match cli.command {
        Task::CrateGraph => crate_graph::run(&repo_root),
    }
}

fn repo_root() -> Result<PathBuf> {
    // The working directory's repo wins over the compile-time path.
    //
    // `CARGO_MANIFEST_DIR` is baked in at build time, and every worktree here
    // shares one `CARGO_TARGET_DIR` — so `cargo xtask` run from a worktree
    // gets a binary compiled in *another* checkout and would inspect that
    // checkout's crates.
    if let Ok(cwd) = std::env::current_dir() {
        for dir in cwd.ancestors() {
            if dir.join("crates").is_dir() && dir.join("xtask/Cargo.toml").is_file() {
                return Ok(dir.to_path_buf());
            }
        }
    }

    // `CARGO_MANIFEST_DIR` is `<repo>/xtask`.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest has no parent directory")
}
