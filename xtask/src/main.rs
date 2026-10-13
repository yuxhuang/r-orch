//! Dev tooling for r-orch. `cargo xtask <command>`.

mod harness;
mod normalize;
mod scenario;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "r-orch development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Run the differential parity harness against the vendored TS oracle.
    Parity {
        /// Only scenarios whose name contains this substring.
        #[arg(long)]
        filter: Option<String>,
        /// Print the id mapping and per-step output even when a scenario passes.
        #[arg(long)]
        verbose: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = repo_root()?;

    match cli.command {
        Task::Parity { filter, verbose } => parity(&repo_root, filter.as_deref(), verbose),
    }
}

fn repo_root() -> Result<PathBuf> {
    // The working directory's repo wins over the compile-time path.
    //
    // `CARGO_MANIFEST_DIR` is baked in at build time, and every worktree here
    // shares one `CARGO_TARGET_DIR` — so `cargo xtask parity` run from a
    // worktree gets a binary compiled in *another* checkout and would load
    // that checkout's scenarios. Measured, not hypothetical.
    if let Ok(cwd) = std::env::current_dir() {
        for dir in cwd.ancestors() {
            if dir.join("fixtures/scenarios").is_dir() && dir.join("xtask/Cargo.toml").is_file() {
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

fn parity(repo_root: &Path, filter: Option<&str>, verbose: bool) -> Result<()> {
    let bins = harness::Binaries::discover(repo_root)?;
    let scenarios_dir = repo_root.join("fixtures/scenarios");
    let scenarios = scenario::load_all(&scenarios_dir)?;

    let selected: Vec<_> = scenarios
        .into_iter()
        .filter(|(_, s)| filter.is_none_or(|f| s.name.contains(f)))
        .collect();

    if selected.is_empty() {
        bail!(
            "no scenarios matched{} in {}",
            filter.map(|f| format!(" `{f}`")).unwrap_or_default(),
            scenarios_dir.display()
        );
    }

    println!("running {} parity scenario(s)\n", selected.len());

    let mut failed = 0usize;
    for (path, scenario) in &selected {
        print!("  {} ... ", scenario.name);
        // Flush so a hang is attributable to the scenario that caused it.
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let result = harness::run_scenario(&bins, scenario)
            .with_context(|| format!("run scenario {}", path.display()))?;

        if result.passed() {
            println!("ok  {}", result.headline());
        } else {
            failed += 1;
            println!("FAILED ({} difference(s))", result.failures.len());
        }

        if verbose || !result.passed() {
            if let Some(description) = &scenario.description {
                println!("      {}", description.trim().replace('\n', "\n      "));
            }
            if !result.id_mapping.is_empty() {
                println!("      id mapping:");
                for (id, placeholder) in &result.id_mapping {
                    println!("        {id} → {placeholder}");
                }
            }
        }
        for failure in &result.failures {
            println!("\n    {}\n", failure.replace('\n', "\n    "));
        }
    }

    println!();
    if failed > 0 {
        bail!("{failed} of {} parity scenario(s) failed", selected.len());
    }
    println!("all {} parity scenario(s) green", selected.len());
    Ok(())
}
