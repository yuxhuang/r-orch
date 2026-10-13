//! The clap command tree, output rendering and the single error boundary.
//!
//! WP-0b wired the walking-skeleton slice — `init`, `agent add`, `task add`,
//! `task list` — end to end. WP-5 extends the tree in place: [`tree`] now
//! carries all sixteen commands, and [`routing`] holds the three L0 routing
//! properties (container-before-parse, the help fast path, Commander's exit
//! codes) that the parity harness exercises from its first scenario.
//!
//! Commands whose behaviour needs WP-4's services land with those services;
//! until then they parse (so V1's surface check is honest) and report that
//! they are not wired rather than pretending to work. [`not_wired`] is the one
//! place that happens, and `crates/orch-cli/TEST-MAP.md` lists them.

pub mod commands;
pub mod context;
pub mod doctor;
pub mod editor;
pub mod logs;
pub mod output;
pub mod routing;
pub mod serve;
pub mod shop_picker;
pub mod tree;
pub mod update;

use std::process::ExitCode;
use std::sync::Arc;

use clap::{CommandFactory, Parser};
use orch_core::error::{Error, Result};
use orch_core::storage::RunStore;
use orch_domain::{SUPPORTED_ADAPTERS, TaskStatus};
use orch_infra::storage::FileRunStore;

use crate::context::{CliContext, EnvFlags};
use crate::output::{Styles, print_error};
use crate::routing::ArgvShape;
use crate::tree::{
    AgentCmd, AgentSub, Cli, Command, ConfigCmd, ConfigSub, ContextCmd, GoalCmd, InitCmd, MsgCmd,
    OrgCmd, RunCmd, ServeCmd, TaskCmd, TaskSub, TeamCmd,
};

/// A command that parses but whose service is not wired yet.
///
/// Deliberately an error rather than a no-op: a silent success would make a
/// scenario pass for the wrong reason, and the parity harness has no scenario
/// for these until they land.
fn not_wired(command: &str) -> Error {
    Error::other(format!(
        "{command} is not wired yet in this build (see crates/orch-cli/TEST-MAP.md)"
    ))
}

/// The single entry point behind the `orch`, `orchestry` and `ao` binaries.
pub fn run() -> ExitCode {
    let env = EnvFlags::from_env();
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let shape = ArgvShape::of(&argv);

    // `--help` / `--version` with no real subcommand: answer without touching
    // the filesystem, so `orch --help` works anywhere.
    if shape.is_help_fast_path() {
        return match Cli::try_parse_from(&argv) {
            // `orch --json` alone parses: fall through to the normal path.
            Ok(cli) => dispatch_parsed(cli, env, shape),
            Err(err) => ExitCode::from(routing::report_parse_error(&err)),
        };
    }

    // The container is built **before** parsing, so an uninitialized project
    // beats a usage error, an unknown command and `--help` on a subcommand.
    if shape.requires_initialized_project() {
        let styles = Styles {
            color: env.stdout_is_tty && !env.no_color_env,
            ascii: env.dumb_term,
        };
        match std::env::current_dir() {
            Ok(cwd) if !is_initialized(&cwd) => {
                let err = Error::from(orch_domain::DomainError::NotInitialized);
                print_error(&styles, &err.to_string(), err.hint().as_deref());
                return ExitCode::from(3);
            }
            Ok(_) => {}
            Err(err) => {
                print_error(
                    &styles,
                    &format!("cannot read the working directory: {err}"),
                    None,
                );
                return ExitCode::from(1);
            }
        }
    }

    match Cli::try_parse_from(&argv) {
        Ok(cli) => dispatch_parsed(cli, env, shape),
        Err(err) => ExitCode::from(routing::report_parse_error(&err)),
    }
}

/// `findProjectRoot` + `paths.isInitialized()`, synchronously: this runs
/// before the runtime exists.
fn is_initialized(cwd: &std::path::Path) -> bool {
    let root = orch_infra::storage::find_project_root(cwd);
    root.join(orch_infra::storage::ORCHESTRY_DIR).is_dir()
}

fn dispatch_parsed(cli: Cli, env: EnvFlags, shape: ArgvShape) -> ExitCode {
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

    match runtime.block_on(dispatch(&cli, env, &shape)) {
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

async fn dispatch(cli: &Cli, env: EnvFlags, shape: &ArgvShape) -> Result<ExitCode> {
    let cwd = std::env::current_dir()
        .map_err(|err| Error::other(format!("cannot read the working directory: {err}")))?;

    match &cli.command {
        Some(Command::Init(args)) => run_init(cli, env, &cwd, args).await,
        Some(Command::Update(args)) => {
            update::run(&styles_for(cli, env), args.check);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Doctor) => {
            let styles = styles_for(cli, env);
            // `doctor` runs in either tier: container-less it probes five
            // adapters, with a project it probes eight.
            let project = if is_initialized(&cwd) {
                let (ctx, _, engine) = build(cli, env, &cwd).await?;
                Some((ctx, engine))
            } else {
                None
            };
            doctor::run(&styles, &cwd, cli.json, project.as_ref()).await
        }

        Some(Command::Task(TaskCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["task"]));
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
                TaskSub::Assign { task_id, agent_id } => {
                    commands::task::assign(&engine, &ctx, &styles, task_id, agent_id).await?;
                }
                TaskSub::Show { .. } => return Err(not_wired("task show")),
                TaskSub::Edit { .. } => return Err(not_wired("task edit")),
                TaskSub::Cancel { .. } => return Err(not_wired("task cancel")),
                TaskSub::Approve { .. } => return Err(not_wired("task approve")),
                TaskSub::Reject { .. } => return Err(not_wired("task reject")),
                TaskSub::Retry { .. } => return Err(not_wired("task retry")),
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Agent(AgentCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["agent"]));
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
                AgentSub::Shop { list } => {
                    commands::agent::shop(
                        &engine,
                        &styles,
                        *list,
                        env.stdout_is_tty,
                        env.stdin_is_tty,
                    )
                    .await?;
                }
                AgentSub::List => return Err(not_wired("agent list")),
                AgentSub::Status { .. } => return Err(not_wired("agent status")),
                AgentSub::Edit { .. } => return Err(not_wired("agent edit")),
                AgentSub::Remove { .. } => return Err(not_wired("agent remove")),
                AgentSub::Disable { .. } => return Err(not_wired("agent disable")),
                AgentSub::Enable { .. } => return Err(not_wired("agent enable")),
                AgentSub::Autonomous { .. } => return Err(not_wired("agent autonomous")),
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Logs(args)) => {
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            let runs: Arc<dyn RunStore> = Arc::new(FileRunStore::new(engine.paths.clone()));
            logs::run(
                &logs::LogsDeps {
                    runs,
                    events: Arc::clone(&engine.events),
                },
                &ctx,
                &styles,
                &logs::LogsArgs {
                    run_id: args.run_id.clone(),
                    task: args.task.clone(),
                    agent: args.agent.clone(),
                    follow: args.follow,
                    since: args.since.clone(),
                },
            )
            .await
        }

        Some(Command::Serve(args)) => run_serve(cli, env, &cwd, args).await,

        Some(Command::Status) => Err(not_wired("status")),
        Some(Command::Config(ConfigCmd { command })) => match command {
            None => Ok(print_group_help(&["config"])),
            Some(ConfigSub::Global(global)) if global.command.is_none() => {
                Ok(print_group_help(&["config", "global"]))
            }
            Some(_) => Err(not_wired("config")),
        },
        Some(Command::Context(ContextCmd { command })) => match command {
            None => Ok(print_group_help(&["context"])),
            Some(_) => Err(not_wired("context")),
        },
        Some(Command::Msg(MsgCmd { command })) => match command {
            None => Ok(print_group_help(&["msg"])),
            Some(_) => Err(not_wired("msg")),
        },
        Some(Command::Goal(GoalCmd { command })) => match command {
            None => Ok(print_group_help(&["goal"])),
            Some(_) => Err(not_wired("goal")),
        },
        Some(Command::Team(TeamCmd { command })) => match command {
            None => Ok(print_group_help(&["team"])),
            Some(_) => Err(not_wired("team")),
        },
        Some(Command::Org(OrgCmd { command })) => match command {
            None => Ok(print_group_help(&["org"])),
            Some(_) => Err(not_wired("org")),
        },
        Some(Command::Run(RunCmd {
            task_id,
            all,
            watch,
            ..
        })) => {
            // The argument check is the command's own, and it exits 2 before
            // anything else happens (`docs/parity/cli.md` §13.1).
            if !*watch && !*all && task_id.is_none() {
                print_error(
                    &styles_for(cli, env),
                    "Specify a task ID, --all, or --watch",
                    None,
                );
                return Ok(ExitCode::from(2));
            }
            Err(not_wired("run"))
        }
        Some(Command::Tui) => Err(not_wired("tui")),

        // Bare `orch` is the first-run experience in the reference: it inits
        // and launches the TUI. Both halves of that are WP-6's; printing the
        // root help is the honest interim behaviour.
        None => {
            let _ = shape;
            Ok(print_group_help_root())
        }
    }
}

async fn run_init(
    cli: &Cli,
    env: EnvFlags,
    cwd: &std::path::Path,
    args: &InitCmd,
) -> Result<ExitCode> {
    let styles = styles_for(cli, env);

    if let Some(adapter) = &args.adapter
        && !orch_domain::is_adapter_kind(adapter)
    {
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

    let outcome = commands::init::run(
        &styles,
        cwd,
        &commands::init::InitArgs {
            name: args.name.clone(),
            adapter: args.adapter.clone(),
            interactive: env.stdout_is_tty && env.stdin_is_tty,
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

async fn run_serve(
    cli: &Cli,
    env: EnvFlags,
    cwd: &std::path::Path,
    args: &ServeCmd,
) -> Result<ExitCode> {
    let styles = styles_for(cli, env);

    let Some(format) = serve::LogFormat::parse(&args.log_format) else {
        print_error(
            &styles,
            &format!(
                "Unknown --log-format \"{}\". Valid: json, text",
                args.log_format
            ),
            None,
        );
        return Ok(ExitCode::from(2));
    };

    let (_ctx, _styles, engine) = build(cli, env, cwd).await?;
    serve::run(
        engine,
        serve::ServeArgs {
            once: args.once,
            tick_interval: args.tick_interval.clone(),
            log_file: args.log_file.clone(),
            format,
            verbose: args.verbose,
        },
    )
    .await
}

fn styles_for(cli: &Cli, env: EnvFlags) -> Styles {
    Styles {
        color: !cli.no_color && !env.no_color_env && env.stdout_is_tty,
        ascii: cli.ascii || env.dumb_term,
    }
}

/// Resolve the context and build the light engine. The `require_init` inside
/// `build_light` is what produces exit 3 for an uninitialized directory, and
/// the pre-parse guard in [`run`] is what makes that happen before parsing.
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
/// and exits **1** (`docs/parity/cli.md` §3.6).
fn print_group_help(path: &[&str]) -> ExitCode {
    let mut command = Cli::command();
    for segment in path {
        match command.find_subcommand(segment) {
            Some(sub) => command = sub.clone(),
            None => return ExitCode::from(1),
        }
    }
    let _ = command.print_help();
    println!();
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
pub(crate) fn parse_int(raw: &str, flag: &str) -> Result<i64> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| Error::invalid_arguments(format!("--{flag} must be an integer")))
}

/// Parse an enum-valued flag, naming the accepted values on failure.
pub(crate) fn parse_enum<T>(raw: Option<&str>, flag: &str) -> Result<Option<T>>
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
}
