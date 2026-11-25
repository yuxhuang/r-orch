//! Tier dispatch and independently tested folded-run components. Refusals and
//! failed test rows never become passing tier evidence.

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Low-level lease and simulation controls are also exercised by private fixtures"
)]
mod lease;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod signals;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Private fixture lifecycle helpers also exercise direct-child behavior"
)]
mod steps;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Independent model and native limitation controls remain for qualified recovery"
)]
mod target;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod contention;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod files;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Private live fixtures exercise the same folded suite and environment checks"
)]
mod hermetic;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod inventory;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod judge;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Framing and listing helpers are also exercised by private fixtures"
)]
mod nextest;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Private metadata fixture controls retain independent snapshot coverage"
)]
mod real_home;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod receipt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Private verifier/retention fixture controls retain independent coverage"
)]
mod report;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(
    dead_code,
    reason = "Private live fixtures exercise the same folded suite and environment checks"
)]
mod suite;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tier;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use crate::gate_selection::{self, SelectArgs};

#[derive(Debug, Args)]
#[command(
    about = "Tiered verification: full (default), pr (merge condition), fast (iteration)",
    after_help = "full is the goal and release gate; pr is the merge condition after the policy transition. full uses a cold target; pr allows content-checked warm reuse. --warm is developer-only and not a gate. fast is for local iteration, never merge evidence."
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
    #[arg(long, conflicts_with = "receipt")]
    pub evidence: Option<PathBuf>,
    /// Redacted diagnostic receipt; only pr --part suite, never full evidence.
    #[arg(long)]
    pub receipt: Option<PathBuf>,
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
    /// Verify diagnostic integrity/outcome; exit 0 does not mean suite success.
    VerifyReceipt {
        dir: PathBuf,
        #[arg(long)]
        commit: String,
    },
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
    // Argument misuse is rejected before filesystem, environment or lease work,
    // including --plan. The legacy warm mode belongs to full only.
    if args.command.is_none() && args.warm && args.tier != Tier::Full {
        eprintln!("gate: --warm is only valid for full");
        return Ok(2);
    }
    if args.command.is_none() && args.tier == Tier::Fast && args.part.is_some() {
        eprintln!("gate: --part is only valid for pr/full");
        return Ok(2);
    }
    if args.receipt.is_some()
        && (args.command.is_some() || args.tier != Tier::Pr || args.part != Some(Part::Suite))
    {
        eprintln!("gate: --receipt is only valid for pr --part suite");
        return Ok(2);
    }
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
                    purpose,
                },
        }) => {
            let _ = writeln!(
                std::io::stderr(),
                "gate lease purpose (unvalidated): {}",
                report::diagnostic(purpose.as_bytes())
            );
            lease::exec_purpose(
                &command,
                std::time::Duration::from_secs(
                    lease_wait
                        .checked_mul(60)
                        .ok_or_else(|| anyhow::anyhow!("lease wait overflow"))?,
                ),
                &purpose,
            )
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Some(GateCommand::Inventory(args)) => {
            let result = (|| -> Result<()> {
                let log = files::text(&args.log, inventory::MAX_LOG)?;
                let map = files::text(&args.executables, inventory::MAX_LOG)?;
                let rows = inventory::parse(&log, &map, args.exit, inventory::Scope::All)?;
                let text = inventory::tsv(&rows);
                if let Some(out) = args.out {
                    files::write(&out, text.as_bytes())?;
                } else {
                    print!("{text}");
                }
                // This helper judges completeness/consistency. Suite execution
                // and evidence verification separately reject failed tests.
                Ok(())
            })();
            match result {
                Ok(()) => Ok(0),
                Err(e) => {
                    eprintln!("gate inventory: {e:#}");
                    Ok(1)
                }
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Some(GateCommand::Evidence {
            command: EvidenceCommand::VerifyReceipt { dir, commit },
        }) => match receipt::verify(repo, &dir, &commit) {
            Ok(summary) => {
                println!("{}", summary.text());
                Ok(0)
            }
            Err(e) => {
                eprintln!(
                    "receipt rejected: {}",
                    report::diagnostic(format!("{e:#}").as_bytes())
                );
                Ok(1)
            }
        },
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Some(GateCommand::Evidence {
            command: EvidenceCommand::Verify { dir, commit },
        }) => match report::verify(repo, &dir, &commit) {
            Ok(()) => {
                println!("evidence verified");
                Ok(0)
            }
            Err(e) => {
                eprintln!("evidence rejected: {e:#}");
                Ok(1)
            }
        },
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Some(GateCommand::Lease {
            command: LeaseCommand::Status,
        }) => lease::status(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        None => tier::run(repo, &args),
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        _ => {
            eprintln!("gate: unsupported native platform; nothing ran");
            Ok(1)
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
        assert!(args.command.is_none());
        for tier in ["full", "pr"] {
            assert_eq!(run(Path::new("."), parse(&[tier, "--plan"])).unwrap(), 0);
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
            // There is one runner. The suite step has no runner choice, so
            // asking for one is a usage error, never a silent fallback.
            vec!["xtask", "gate", "pr", "--runner", "cargo"],
            vec!["xtask", "gate", "--runner", "cargo", "pr"],
            vec!["xtask", "gate", "unknown"],
            vec!["xtask", "gate", "inventory"],
            vec!["xtask", "gate", "lease", "exec", "--purpose", "x"],
            vec!["xtask", "gate", "select", "--changed-from", "changes"],
        ] {
            assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn pr_warm_is_usage_before_any_runtime_or_plan() {
        let scratch = steps::tests::Scratch::new();
        for flags in [vec!["pr", "--warm"], vec!["pr", "--warm", "--plan"]] {
            assert_eq!(run(&scratch.0.join("missing"), parse(&flags)).unwrap(), 2);
            assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
        }
    }

    #[test]
    fn inventory_helper_accepts_consistent_failed_rows_without_passing_the_suite() {
        let scratch = steps::tests::Scratch::new();
        let log = scratch.0.join("test.log");
        let map = scratch.0.join("map.jsonl");
        let out = scratch.0.join("inventory.tsv");
        std::fs::write(&log, "Running unittests src/lib.rs (/fixture/test)\nrunning 1 test\ntest actual ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n").unwrap();
        std::fs::write(&map, serde_json::json!({"reason":"compiler-artifact","package_id":"path+file:///fixture#fixture@0.1.0","profile":{"test":true},"executable":"/fixture/test","target":{"kind":["lib"],"name":"fixture"}}).to_string()).unwrap();
        let args = parse(&[
            "inventory",
            "--log",
            log.to_str().unwrap(),
            "--executables",
            map.to_str().unwrap(),
            "--exit",
            "101",
            "--out",
            out.to_str().unwrap(),
        ]);
        assert_eq!(run(&scratch.0, args).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "lib\tfixture\tfixture\tactual\tfailed\n"
        );
        assert_eq!(
            report::exit(
                &[report::Step {
                    name: "test",
                    verdict: report::Verdict::Fail,
                    exit: Some(101),
                    elapsed: std::time::Duration::ZERO,
                    detail: "failed suite; inventory parsed".into()
                }],
                false
            ),
            1
        );
    }

    #[test]
    fn no_lease_refuses_without_entering_target_lifecycle() {
        assert_eq!(
            {
                let scratch = steps::tests::Scratch::new();
                run(&scratch.0, parse(&["pr", "--no-lease", "fixture"])).unwrap()
            },
            1
        );
    }
}
