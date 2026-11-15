//! Tier wiring remains a seam; F2/F3 provide lease and direct-command primitives.

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code, reason = "F2 lifecycle awaits whole-gate integration")]
mod lease;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod signals;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code, reason = "F3 lifecycle awaits whole-gate integration")]
mod steps;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Target mutation awaits a stable native namespace implementation and tier wiring"
)]
mod target;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use crate::gate_selection::{self, SelectArgs};

#[derive(Debug, Args)]
#[command(
    about = "Tiered verification: full (default), pr (merge condition), fast (iteration)",
    after_help = "full is the goal and release gate. All tiers are not available yet in this seam."
)]
pub struct GateArgs {
    #[command(subcommand)]
    pub command: Option<GateCommand>,
    #[arg(value_enum, default_value = "full")]
    pub tier: Tier,
    #[arg(long)]
    pub plan: bool,
    #[arg(long, value_enum)]
    pub part: Option<Part>,
    #[arg(long)]
    pub base: Option<String>,
    #[arg(long)]
    pub evidence: Option<PathBuf>,
    #[arg(long)]
    pub host_label: Option<String>,
    #[arg(long, default_value_t = 30)]
    pub lease_wait: u64,
    #[arg(long)]
    pub no_lease: Option<String>,
    #[arg(long)]
    pub keep: bool,
    #[arg(long)]
    pub warm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Tier {
    Full,
    Pr,
    Fast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Part {
    Suite,
    Static,
}

#[derive(Debug, Subcommand)]
pub enum GateCommand {
    Select(SelectArgs),
    Inventory(InventoryArgs),
    Evidence {
        #[command(subcommand)]
        command: EvidenceCommand,
    },
    Lease {
        #[command(subcommand)]
        command: LeaseCommand,
    },
}

#[derive(Debug, Args)]
pub struct InventoryArgs {
    #[arg(long)]
    pub log: PathBuf,
    #[arg(long)]
    pub executables: PathBuf,
    /// Suite process status; omitted means success.
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    pub exit: i32,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum EvidenceCommand {
    Verify {
        dir: PathBuf,
        #[arg(long)]
        commit: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum LeaseCommand {
    Status,
    Exec {
        #[arg(long)]
        purpose: String,
        #[arg(long, default_value_t = 30)]
        lease_wait: u64,
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<std::ffi::OsString>,
    },
}

pub fn run(repo: &Path, args: GateArgs) -> Result<i32> {
    match args.command {
        Some(GateCommand::Select(select)) => {
            gate_selection::run_cli(repo, select)?;
            Ok(0)
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Some(GateCommand::Lease {
            command:
                LeaseCommand::Exec {
                    command,
                    lease_wait,
                    ..
                },
        }) => lease::exec(&command, std::time::Duration::from_secs(lease_wait)),
        _ => {
            if args.no_lease.is_some() {
                eprintln!(
                    "gate: REFUSED: configured lease required before target admission; qualifying: no"
                );
                return Ok(1);
            }
            eprintln!("gate: not available yet (registry seam only; nothing ran)");
            Ok(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Task};
    use clap::Parser;

    fn parse(args: &[&str]) -> GateArgs {
        let cli =
            Cli::try_parse_from(["xtask", "gate"].into_iter().chain(args.iter().copied())).unwrap();
        let Task::Gate(args) = cli.command else {
            panic!("expected gate");
        };
        args
    }

    #[test]
    fn tier_defaults_and_refuses_execution() {
        let args = parse(&[]);
        assert_eq!(args.tier, Tier::Full);
        assert_eq!(run(Path::new("."), args).unwrap(), 2);
        for tier in ["full", "pr", "fast"] {
            assert_eq!(run(Path::new("."), parse(&[tier, "--plan"])).unwrap(), 2);
        }
    }

    #[test]
    fn inventory_default_exit_and_helper_shapes() {
        let args = parse(&["inventory", "--log", "log", "--executables", "map"]);
        let Some(GateCommand::Inventory(args)) = args.command else {
            panic!("expected inventory");
        };
        assert_eq!(args.exit, 0);
        parse(&["evidence", "verify", "bundle", "--commit", "abc"]);
        parse(&["lease", "status"]);
        let args = parse(&[
            "lease",
            "exec",
            "--purpose",
            "probe",
            "--",
            "-command",
            "a b",
        ]);
        let Some(GateCommand::Lease {
            command: LeaseCommand::Exec { command, .. },
        }) = args.command
        else {
            panic!("expected lease exec");
        };
        assert_eq!(command, ["-command", "a b"]);
    }

    #[test]
    fn usage_errors_stay_usage_errors() {
        for args in [
            vec!["xtask", "gate", "unknown"],
            vec!["xtask", "gate", "inventory"],
            vec!["xtask", "gate", "lease", "exec", "--purpose", "x"],
            vec!["xtask", "gate", "select", "--changed-from", "changes"],
        ] {
            assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn no_lease_refuses_without_entering_target_lifecycle() {
        assert_eq!(
            run(Path::new("."), parse(&["pr", "--no-lease", "fixture"])).unwrap(),
            1
        );
    }
}
