//! Dev tooling for r-orch. `cargo xtask <command>`.

mod crate_graph;
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
        /// Skip scenarios whose name contains this substring.
        ///
        /// For the mutation probes, which run the suite once per probe and
        /// need none of the agent-driven scenarios: `--exclude o7-` turns an
        /// hour back into a few minutes.
        #[arg(long)]
        exclude: Option<String>,
        /// Print the id mapping and per-step output even when a scenario passes.
        #[arg(long)]
        verbose: bool,
    },
    /// Assert the crate layering from ARCHITECTURE §3.1.
    CrateGraph,
    /// Replay every scenario against the oracle alone, checking that the
    /// scenarios themselves are well-formed.
    ///
    /// The corpus is written ahead of the commands it exercises, so most of it
    /// is skipped on any given day — and a skipped scenario is never parsed
    /// past its `requires`, so a misspelled flag or a `${task.3}` that does
    /// not exist hides until the command lands, months later. The oracle has
    /// every command today, so running the steps through it alone tells us now
    /// whether each scenario *says* what its author meant.
    LintScenarios {
        /// Only scenarios whose name contains this substring.
        #[arg(long)]
        filter: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = repo_root()?;

    match cli.command {
        Task::Parity {
            filter,
            exclude,
            verbose,
        } => parity(&repo_root, filter.as_deref(), exclude.as_deref(), verbose),
        Task::LintScenarios { filter } => lint_scenarios(&repo_root, filter.as_deref()),
        Task::CrateGraph => crate_graph::run(&repo_root),
    }
}

fn lint_scenarios(repo_root: &Path, filter: Option<&str>) -> Result<()> {
    let bins = harness::Binaries::discover(repo_root)?;
    let scenarios_dir = repo_root.join("fixtures/scenarios");
    let selected: Vec<_> = scenario::load_all(&scenarios_dir)?
        .into_iter()
        .filter(|(_, s)| filter.is_none_or(|f| s.name.contains(f)))
        .collect();

    if selected.is_empty() {
        bail!("no scenarios matched in {}", scenarios_dir.display());
    }

    println!(
        "linting {} scenario(s) against the oracle\n",
        selected.len()
    );
    let mut bad = 0usize;

    for (path, scenario) in &selected {
        print!("  {} ... ", scenario.name);
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let problems = harness::lint_against_oracle(&bins, scenario)
            .with_context(|| format!("lint scenario {}", path.display()))?;
        if problems.is_empty() {
            println!("ok");
        } else {
            bad += 1;
            println!("{} problem(s)", problems.len());
            for problem in &problems {
                println!("    {}", problem.replace('\n', "\n    "));
            }
        }
    }

    println!();
    if bad > 0 {
        bail!("{bad} of {} scenario(s) are malformed", selected.len());
    }
    println!("all {} scenario(s) well-formed", selected.len());
    Ok(())
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

fn parity(
    repo_root: &Path,
    filter: Option<&str>,
    exclude: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let bins = harness::Binaries::discover(repo_root)?;
    let scenarios_dir = repo_root.join("fixtures/scenarios");
    let scenarios = scenario::load_all(&scenarios_dir)?;

    let selected: Vec<_> = scenarios
        .into_iter()
        .filter(|(_, s)| filter.is_none_or(|f| s.name.contains(f)))
        .filter(|(_, s)| !exclude.is_some_and(|e| s.name.contains(e)))
        .collect();

    if selected.is_empty() {
        bail!(
            "no scenarios matched{} in {}",
            filter.map(|f| format!(" `{f}`")).unwrap_or_default(),
            scenarios_dir.display()
        );
    }

    println!("running {} parity scenario(s)\n", selected.len());

    let capabilities = harness::Capabilities::new();
    if let Some(note) = capabilities.isolation_note() {
        // Loud, because the consequence is writing into the developer's own
        // home rather than a failure anyone would notice.
        println!("WARNING: {note}\n");
    }
    let (mut failed, mut skipped) = (0usize, 0usize);
    let mut skipped_names: Vec<(String, String)> = Vec::new();

    for (path, scenario) in &selected {
        print!("  {} ... ", scenario.name);
        // Flush so a hang is attributable to the scenario that caused it.
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let result = harness::run_scenario(&bins, &capabilities, scenario)
            .with_context(|| format!("run scenario {}", path.display()))?;

        match (&result.skipped, result.failures.is_empty()) {
            (Some(reason), _) => {
                skipped += 1;
                println!("skipped — {reason}");
                skipped_names.push((scenario.name.clone(), reason.clone()));
            }
            (None, true) => println!("ok  {}", result.headline()),
            (None, false) => {
                failed += 1;
                println!("FAILED ({} difference(s))", result.failures.len());
            }
        }

        let show_detail = verbose || (result.ran() && !result.passed());
        if show_detail {
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

    let total = selected.len();
    let runnable = total - skipped;
    let green = runnable - failed;

    println!();
    println!(
        "{total} scenario(s): {runnable} runnable, {green} green, {failed} failed, {skipped} skipped"
    );
    if skipped > 0 && verbose {
        println!("\nskipped, and why:");
        for (name, reason) in &skipped_names {
            println!("  {name}: {reason}");
        }
    }
    if skipped > 0 {
        // A skip is a gap in coverage, not a pass. Say so every run so the
        // number cannot quietly grow.
        println!(
            "\n{skipped} scenario(s) are waiting on commands the Rust binary does not have \
             yet — they go live on their own as those land (`--verbose` lists them)."
        );
    }
    if failed > 0 {
        bail!("{failed} of {runnable} runnable parity scenario(s) failed");
    }
    println!("\nall {runnable} runnable parity scenario(s) green");
    Ok(())
}
