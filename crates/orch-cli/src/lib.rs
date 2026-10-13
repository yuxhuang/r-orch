//! The clap command tree, output rendering and the single error boundary.
//!
//! WP-0b wired the walking-skeleton slice — `init`, `agent add`, `task add`,
//! `task list` — end to end. WP-5 extends the tree in place.
//!
//! Three routing properties are L0 and are implemented here rather than left
//! to WP-5, because the parity harness exercises them from its first scenario:
//!
//! - **Any command in an uninitialized directory exits `3`**, including
//!   `orch <sub> --help` and an unknown command: the container is built before
//!   argument parsing, so `NotInitializedError` wins over help and over usage
//!   errors. Only `init`, `doctor` and `update` are exempt (PARITY.md §3).
//! - **A command group with no subcommand** prints its help to **stdout** and
//!   exits **`1`**.
//! - Usage errors exit **`1`**, not clap's default `2`.

pub mod commands;
pub mod context;
pub mod editor;
pub mod output;
pub mod serve;
pub mod shop_picker;
pub mod tui;
pub mod update_check;

use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use orch_core::error::{Error, Result};
use orch_domain::{SUPPORTED_ADAPTERS, TaskStatus};

use crate::context::{CliContext, EnvFlags};
use crate::output::{Styles, print_error};

/// The clap program name stays `orchestry` so `--help` matches, whichever of
/// the three binary names was invoked.
#[derive(Debug, Parser)]
#[command(
    name = "orchestry",
    about = "Agents Organizations — CLI orchestrator for AI agents",
    version = "1.0.34",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[arg(long, global = true, help = "Output as JSON")]
    pub json: bool,
    #[arg(long, global = true, help = "Minimal output (IDs only)")]
    pub quiet: bool,
    #[arg(long = "no-color", global = true, help = "Disable colors")]
    pub no_color: bool,
    #[arg(long, global = true, help = "ASCII-only output (no Unicode)")]
    pub ascii: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize .orchestry/ in the current directory
    Init(InitCmd),
    /// Manage tasks
    Task(TaskCmd),
    /// Manage agents
    Agent(AgentCmd),
    /// Launch interactive TUI dashboard
    Tui,
}

#[derive(Debug, Args)]
pub struct InitCmd {
    #[arg(long, help = "Project name")]
    pub name: Option<String>,
    #[arg(
        long,
        help = "Default agent adapter (claude, opencode, codex, cursor, pi, grok, antigravity, shell)"
    )]
    pub adapter: Option<String>,
}

#[derive(Debug, Args)]
pub struct TaskCmd {
    #[command(subcommand)]
    pub command: Option<TaskSub>,
}

#[derive(Debug, Subcommand)]
pub enum TaskSub {
    /// Create a new task
    ///
    /// The flags live in their own `Args` struct rather than inline on the
    /// variant: there are eleven of them, and an inline variant makes the enum
    /// large enough that every `TaskSub` costs that much to move.
    Add(Box<TaskAddFlags>),
    /// List all tasks
    List {
        #[arg(long, help = "Filter by status")]
        status: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct TaskAddFlags {
    pub title: String,
    #[arg(short, long, help = "Task description")]
    pub description: Option<String>,
    #[arg(short, long, default_value = "3", help = "Priority (1-4)")]
    pub priority: String,
    #[arg(short, long, help = "Comma-separated labels")]
    pub labels: Option<String>,
    #[arg(long = "depends-on", help = "Comma-separated dependency task IDs")]
    pub depends_on: Option<String>,
    #[arg(long = "max-attempts", help = "Max retry attempts")]
    pub max_attempts: Option<String>,
    #[arg(
        long = "workspace-mode",
        help = "Workspace mode: shared|worktree|isolated"
    )]
    pub workspace_mode: Option<String>,
    #[arg(long, help = "Assign to agent")]
    pub assignee: Option<String>,
    #[arg(
        long = "review-criteria",
        help = "Comma-separated auto-review criteria: test_pass,typecheck,lint"
    )]
    pub review_criteria: Option<String>,
    #[arg(long, help = "Comma-separated glob patterns for file scope")]
    pub scope: Option<String>,
    #[arg(long = "goal-id", help = "Associate task with a goal")]
    pub goal_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentCmd {
    #[command(subcommand)]
    pub command: Option<AgentSub>,
}

#[derive(Debug, Subcommand)]
pub enum AgentSub {
    /// Add a new agent
    Add(Box<AgentAddFlags>),
}

#[derive(Debug, Args)]
pub struct AgentAddFlags {
    pub name: String,
    #[arg(
        long,
        required = true,
        help = "Adapter type: claude, opencode, codex, cursor, pi, grok, antigravity, shell"
    )]
    pub adapter: String,
    #[arg(long, help = "Agent role description")]
    pub role: Option<String>,
    #[arg(long, help = "Shell command (for shell adapter)")]
    pub command: Option<String>,
    #[arg(long, help = "Model name (for AI adapters)")]
    pub model: Option<String>,
    #[arg(long, help = "Reasoning effort: low, medium, high")]
    pub effort: Option<String>,
    #[arg(long = "max-turns", help = "Max turns per run")]
    pub max_turns: Option<String>,
    #[arg(long, help = "Timeout in ms")]
    pub timeout: Option<String>,
    #[arg(long = "approval-policy", help = "suggest|auto|manual")]
    pub approval_policy: Option<String>,
    #[arg(long = "workspace-mode", help = "shared|worktree|isolated")]
    pub workspace_mode: Option<String>,
    #[arg(long, help = "Comma-separated list of agent skills")]
    pub skills: Option<String>,
}

/// The single entry point behind the `orch`, `orchestry` and `ao` binaries.
pub fn run() -> ExitCode {
    let env = EnvFlags::from_env();

    // Parse first so the global flags are available to the error printer, but
    // map clap's own exit code: a usage error is exit 1 in ORCH, not 2.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let is_help_or_version = matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayVersion
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            let _ = err.print();
            return if is_help_or_version {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            };
        }
    };

    let styles = styles_for(&cli, env);

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            print_error(
                &styles,
                &format!("failed to start the async runtime: {err}"),
                None,
            );
            return ExitCode::from(1);
        }
    };

    match runtime.block_on(dispatch(&cli, env)) {
        Ok(code) => code,
        Err(err) => {
            print_error(&styles, &err.to_string(), err.hint().as_deref());
            if std::env::var_os("ORCHESTRY_DEBUG").is_some() {
                eprintln!("{err:?}");
                let mut source = std::error::Error::source(&err);
                while let Some(cause) = source {
                    eprintln!("  caused by: {cause}");
                    source = std::error::Error::source(cause);
                }
            }
            ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1))
        }
    }
}

async fn dispatch(cli: &Cli, env: EnvFlags) -> Result<ExitCode> {
    let cwd = std::env::current_dir()
        .map_err(|err| Error::other(format!("cannot read the working directory: {err}")))?;

    match &cli.command {
        // `init` is one of the three commands that work without a container.
        Some(Command::Init(args)) => {
            if let Some(adapter) = &args.adapter
                && !orch_domain::is_adapter_kind(adapter)
            {
                let styles = styles_for(cli, env);
                print_error(
                    &styles,
                    &format!("Unknown adapter \"{adapter}\""),
                    Some(&format!(
                        "Supported: {}",
                        SUPPORTED_ADAPTERS
                            .iter()
                            .map(|a| a.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                );
                // An explicit `process.exit(2)` guard in the original.
                return Ok(ExitCode::from(2));
            }

            let styles = styles_for(cli, env);
            let outcome = commands::init::run(
                &styles,
                &cwd,
                &commands::init::InitArgs {
                    name: args.name.clone(),
                    adapter: args.adapter.clone(),
                },
            )
            .await?;
            if !outcome.already_initialized {
                println!(
                    "  Next: {}",
                    styles.dim(commands::init::next_hint(&outcome.adapter))
                );
                println!();
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Task(TaskCmd { command })) => {
            let Some(sub) = command else {
                // A group with no subcommand prints help to stdout, exit 1.
                return Ok(print_group_help("task"));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                TaskSub::Add(flags) => {
                    commands::task::add(
                        &engine,
                        &ctx,
                        &styles,
                        commands::task::TaskAddArgs {
                            title: flags.title.clone(),
                            description: flags.description.clone(),
                            priority: parse_int(&flags.priority, "priority")?,
                            labels: flags.labels.clone(),
                            depends_on: flags.depends_on.clone(),
                            max_attempts: match &flags.max_attempts {
                                Some(raw) => Some(parse_int(raw, "max-attempts")? as u32),
                                None => None,
                            },
                            workspace_mode: parse_enum(
                                flags.workspace_mode.as_deref(),
                                "workspace-mode",
                            )?,
                            assignee: flags.assignee.clone(),
                            review_criteria: flags.review_criteria.clone(),
                            scope: flags.scope.clone(),
                            goal_id: flags.goal_id.clone(),
                        },
                    )
                    .await?;
                }
                TaskSub::List { status } => {
                    let status: Option<TaskStatus> = parse_enum(status.as_deref(), "status")?;
                    commands::task::list(&engine, &ctx, &styles, status).await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Agent(AgentCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help("agent"));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                AgentSub::Add(flags) => {
                    commands::agent::add(
                        &engine,
                        &ctx,
                        &styles,
                        commands::agent::AgentAddArgs {
                            name: flags.name.clone(),
                            adapter: flags.adapter.clone(),
                            role: flags.role.clone(),
                            command: flags.command.clone(),
                            model: flags.model.clone(),
                            effort: parse_enum(flags.effort.as_deref(), "effort")?,
                            max_turns: match &flags.max_turns {
                                Some(raw) => Some(parse_int(raw, "max-turns")? as u32),
                                None => None,
                            },
                            timeout_ms: match &flags.timeout {
                                Some(raw) => Some(parse_int(raw, "timeout")? as u64),
                                None => None,
                            },
                            approval_policy: parse_enum(
                                flags.approval_policy.as_deref(),
                                "approval-policy",
                            )?,
                            workspace_mode: parse_enum(
                                flags.workspace_mode.as_deref(),
                                "workspace-mode",
                            )?,
                            skills: flags.skills.clone(),
                        },
                    )
                    .await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Tui) => {
            let ctx = CliContext::resolve(&cwd, cli.json, cli.quiet, cli.no_color, cli.ascii, env);
            // The dashboard needs the orchestrator, so this is the one command
            // that builds the full container up front.
            tui::run(
                ctx.project_root,
                Cli::command().get_version().unwrap_or("0.0.0"),
            )
            .await
        }

        // Bare `orch` is the first-run experience (PARITY.md §2.3).
        //
        // `argv_is_bare` is this branch's stand-in for WP-5's
        // `ArgvShape::is_bare`; when the routing guard lands, this arm becomes
        // `None if shape.is_bare =>` and the helper goes away.
        None if argv_is_bare() => {
            tui::first_run(
                &styles_for(cli, env),
                &cwd,
                &commands::init::InitArgs {
                    name: None,
                    adapter: None,
                },
                Cli::command().get_version().unwrap_or("0.0.0"),
            )
            .await
        }

        // Flags but no subcommand (`orch --json`). The reference appends `tui`
        // only for a *bare* argv (`cli.ts:206`), so this prints the root help
        // rather than launching the dashboard.
        None => Ok(print_group_help_root()),
    }
}

/// `process.argv.length <= 2` — no arguments at all, which is the first-run
/// path rather than a flag-only invocation.
fn argv_is_bare() -> bool {
    std::env::args_os().count() <= 1
}

fn styles_for(cli: &Cli, env: EnvFlags) -> Styles {
    Styles {
        color: !cli.no_color && !env.no_color_env && env.stdout_is_tty,
        ascii: cli.ascii || env.dumb_term,
    }
}

/// Resolve the context and build the light engine. The `require_init` inside
/// `build_light` is what produces exit 3 for an uninitialized directory.
async fn build(
    cli: &Cli,
    env: EnvFlags,
    cwd: &std::path::Path,
) -> Result<(CliContext, Styles, orch_engine::Engine)> {
    let ctx = CliContext::resolve(cwd, cli.json, cli.quiet, cli.no_color, cli.ascii, env);
    let styles = ctx.styles(env);
    let engine = orch_engine::build_light(ctx.project_root.clone()).await?;
    Ok((ctx, styles, engine))
}

/// A command group invoked with no subcommand prints its help to **stdout**
/// and exits **1**.
fn print_group_help(group: &str) -> ExitCode {
    let mut command = Cli::command();
    if let Some(sub) = command.find_subcommand_mut(group) {
        let mut sub = sub.clone();
        let _ = sub.print_help();
        println!();
    }
    ExitCode::from(1)
}

fn print_group_help_root() -> ExitCode {
    let _ = Cli::command().print_help();
    println!();
    ExitCode::from(1)
}

/// `parseInt(value, 10)`. A non-numeric value is `NaN` in the original, which
/// then fails the domain's own range check — so the message a user sees comes
/// from the service, not from here.
fn parse_int(raw: &str, flag: &str) -> Result<i64> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| Error::invalid_arguments(format!("--{flag} must be an integer")))
}

/// Parse an enum-valued flag, naming the accepted values on failure.
fn parse_enum<T>(raw: Option<&str>, flag: &str) -> Result<Option<T>>
where
    T: std::str::FromStr<Err = ()>,
{
    match raw {
        None => Ok(None),
        Some(value) => value
            .parse::<T>()
            .map(Some)
            .map_err(|()| Error::invalid_arguments(format!("Invalid --{flag}: \"{value}\""))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_tree_parses_the_walking_skeleton_surface() {
        let cli = Cli::try_parse_from(["orch", "init", "--name", "p", "--adapter", "shell"])
            .expect("init parses");
        match cli.command {
            Some(Command::Init(args)) => {
                assert_eq!(args.name.as_deref(), Some("p"));
                assert_eq!(args.adapter.as_deref(), Some("shell"));
            }
            other => panic!("{other:?}"),
        }

        let cli = Cli::try_parse_from([
            "orch",
            "task",
            "add",
            "Do it",
            "--priority",
            "1",
            "--assignee",
            "agt_creator",
        ])
        .expect("task add parses");
        assert!(matches!(cli.command, Some(Command::Task(_))));

        let cli = Cli::try_parse_from(["orch", "--json", "task", "list"]).expect("parses");
        assert!(cli.json);

        // The global flags are accepted after the subcommand too, as
        // Commander accepts them.
        let cli = Cli::try_parse_from(["orch", "task", "list", "--json"]).expect("parses");
        assert!(cli.json);
    }

    #[test]
    fn agent_add_requires_the_adapter_flag() {
        let err = Cli::try_parse_from(["orch", "agent", "add", "shelly"]).expect_err("required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn task_add_requires_a_title() {
        let err = Cli::try_parse_from(["orch", "task", "add"]).expect_err("required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_group_with_no_subcommand_parses_so_the_handler_can_print_help() {
        let cli = Cli::try_parse_from(["orch", "task"]).expect("parses");
        assert!(matches!(
            cli.command,
            Some(Command::Task(TaskCmd { command: None }))
        ));
    }

    #[test]
    fn short_flags_match_the_reference() {
        let cli = Cli::try_parse_from([
            "orch", "task", "add", "t", "-d", "body", "-p", "2", "-l", "a,b",
        ])
        .expect("parses");
        match cli.command {
            Some(Command::Task(TaskCmd {
                command: Some(TaskSub::Add(flags)),
            })) => {
                assert_eq!(flags.description.as_deref(), Some("body"));
                assert_eq!(flags.priority, "2");
                assert_eq!(flags.labels.as_deref(), Some("a,b"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn priority_defaults_to_three() {
        let cli = Cli::try_parse_from(["orch", "task", "add", "t"]).expect("parses");
        match cli.command {
            Some(Command::Task(TaskCmd {
                command: Some(TaskSub::Add(flags)),
            })) => {
                assert_eq!(flags.priority, "3");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_int_rejects_a_non_numeric_value_with_exit_2() {
        let err = parse_int("abc", "priority").expect_err("rejected");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(parse_int(" 4 ", "priority").unwrap(), 4);
        assert_eq!(parse_int("-1", "priority").unwrap(), -1);
    }

    #[test]
    fn parse_enum_names_the_flag_it_rejected() {
        assert_eq!(parse_enum::<TaskStatus>(None, "status").unwrap(), None);
        assert_eq!(
            parse_enum::<TaskStatus>(Some("todo"), "status").unwrap(),
            Some(TaskStatus::Todo)
        );
        let err = parse_enum::<TaskStatus>(Some("nope"), "status").expect_err("rejected");
        assert_eq!(err.to_string(), "Invalid --status: \"nope\"");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn the_program_name_is_orchestry_whichever_binary_was_invoked() {
        assert_eq!(Cli::command().get_name(), "orchestry");
        assert_eq!(Cli::command().get_version(), Some("1.0.34"));
    }
}
