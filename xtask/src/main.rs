//! Dev tooling for r-orch. `cargo xtask <command>`.

mod changelog;
mod crate_graph;
mod fixture_gen;
mod gate;
mod gate_selection;
mod linux;

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
    /// Validate fragments or assemble release notes.
    Changelog(changelog::ChangelogArgs),
    /// Generate the synthetic 1.0 import corpus, or with --check compare it with the checked-in tree.
    FixtureGen(fixture_gen::FixtureGenArgs),
    /// Run the Linux checks from macOS in a linux/amd64 Docker container (AGENTS.md).
    Linux(linux::LinuxArgs),
    /// Tiered checks: full is the goal/release gate; pr is the merge tier after policy transition; fast is staged.
    Gate(gate::GateArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = repo_root()?;

    match cli.command {
        Task::Changelog(args) => changelog::run(&std::env::current_dir()?, args),
        Task::CrateGraph => crate_graph::run(&repo_root),
        Task::FixtureGen(args) => fixture_gen::run(&repo_root, args),
        Task::Linux(args) => linux::run(&repo_root, args),
        Task::Gate(args) => {
            // Helpers operate on the caller's repository, including disposable fixtures.
            let code = gate::run(&std::env::current_dir()?, args)?;
            std::process::exit(code);
        }
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
