//! The clap command tree, output rendering and the single error boundary.
//!
//! WP-0b wired the walking-skeleton slice — `init`, `agent add`, `task add`,
//! `task list` — end to end. WP-5 extends the tree in place: [`tree`] now
//! carries all seventeen commands, and [`routing`] holds the three routing
//! properties (container-before-parse, the help fast path, the usage exit
//! codes).
//!
//! Commands whose behaviour needs WP-4's services land with those services;
//! until then they parse (so the surface check is honest) and report that
//! they are not wired rather than pretending to work. [`not_wired`] is the one
//! place that happens, and `crates/orch-cli/TEST-MAP.md` lists them.

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap inside `#[cfg(test)]`). This crate renders to the terminal, so printing is
// allowed here. See AGENTS.md, "Rust coding standards".
#![deny(unreachable_pub, clippy::unwrap_used)]

pub mod commands;
pub mod context;
pub mod doctor;
pub mod document_input;
pub mod editor;
pub mod logs;
mod mcp;
pub mod output;
pub mod routing;
pub mod serve;
pub mod shop_picker;
pub mod template_shop;
pub mod tree;
pub mod tui;
pub mod update;
pub mod update_check;
pub mod web;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use orch_core::error::{Error, Result};
use orch_domain::{SUPPORTED_ADAPTERS, TaskStatus};

use crate::context::{CliContext, EnvFlags};
use crate::output::{Styles, print_error};
use crate::routing::ArgvShape;
use crate::tree::{
    AgentCmd, AgentSub, Cli, Command, ConfigCmd, ConfigGlobalCmd, ConfigGlobalSub, ConfigSub,
    ContextCmd, ContextSub, GoalCmd, GoalSub, InitCmd, MsgCmd, MsgSub, RunCmd, TaskCmd, TaskSub,
    TeamCmd, TeamSub, WebCmd,
};

/// The single entry point behind the `orch`, `orchestry` and `ao` binaries.
pub fn run() -> ExitCode {
    init_tracing();
    let env = EnvFlags::from_env();
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let mut routing_argv = Vec::new();
    let mut skip_identity = false;
    let mut before_command = true;
    for (index, arg) in argv.iter().enumerate() {
        if index == 0 {
            routing_argv.push(arg);
            continue;
        }
        if skip_identity {
            skip_identity = false;
            continue;
        }
        if before_command && arg == "--agent" {
            skip_identity = true;
            continue;
        }
        if before_command && arg.to_string_lossy().starts_with("--agent=") {
            continue;
        }
        if !arg.to_string_lossy().starts_with('-') {
            before_command = false;
        }
        routing_argv.push(arg);
    }
    let shape = ArgvShape::of(&routing_argv);

    // `--version` prints the bare version string, where clap would prefix it
    // with the program name.
    if shape.is_help_fast_path() && shape.wants_version {
        println!("{}", update::VERSION);
        return ExitCode::SUCCESS;
    }

    // `--help` with no real subcommand: answer without touching the
    // filesystem, so `orch --help` works anywhere.
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

/// Whether the project root is initialized, checked synchronously: this runs
/// before the runtime exists.
fn is_initialized(cwd: &std::path::Path) -> bool {
    let root = context::project_root(cwd);
    root.join(".orchestry").is_dir()
}

fn dispatch_parsed(mut cli: Cli, env: EnvFlags, shape: ArgvShape) -> ExitCode {
    let styles = styles_for(&cli, env);

    // `orch mcp` owns its runtime: its shutdown must be bounded, and nothing
    // of the ordinary command path may write to stdout.
    inherit_root_agent(&mut cli);
    if let Some(Command::Mcp(args)) = &cli.command {
        return mcp::run(args, &styles);
    }

    // Owner starts capture the environment before Tokio creates workers.
    let owner_command = match &cli.command {
        Some(Command::Daemon(args)) => Some(args),
        _ => None,
    };
    // Refused before any connect or lifecycle call, never silently ignored.
    let rejected = match &cli.command {
        Some(Command::Serve(args)) => args.rejected_flag(),
        Some(Command::Run(args)) => args.rejected_flag(),
        _ => None,
    };
    if let Some(message) = rejected {
        print_error(&styles, message, None);
        return ExitCode::from(2);
    }
    let foreground = foreground_start(cli.command.as_ref());
    if owner_command.is_some() || foreground.is_some() {
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                print_error(
                    &styles,
                    &format!("cannot read the working directory: {error}"),
                    None,
                );
                return ExitCode::from(1);
            }
        };
        if let Some(args) = owner_command {
            return commands::daemon::run(&cli, args, &cwd);
        }
        if let Some((peer, from_agent)) = foreground {
            return commands::daemon::foreground(&cli, peer, from_agent, &cwd);
        }
    }

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
            if let Some(error) = commands::rpc::recover_client_error(&err) {
                let agent_context = std::env::var_os("ORCH_TASK_ID").is_some()
                    || std::env::var_os("ORCH_AGENT_ID").is_some();
                eprintln!("{}", error.render(agent_context));
                return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1));
            }
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
        // Container-less: the version check reads no project state, so it
        // needs neither a project nor a daemon.
        Some(Command::Update(args)) => {
            update::run(&styles_for(cli, env), args.check);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Doctor) => {
            let styles = styles_for(cli, env);
            commands::run::doctor(cli, &styles, &cwd).await
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
                            priority: parse_priority(&flags.priority),
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
                            attach: flags.attach.clone(),
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
                TaskSub::Show { id } => commands::task::show(&engine, &ctx, &styles, id).await?,
                TaskSub::Update(args) => {
                    commands::task::mutate(
                        &engine,
                        &ctx,
                        &styles,
                        args,
                        document_input::MutationKind::Update,
                    )
                    .await?
                }
                TaskSub::Patch(args) => {
                    commands::task::mutate(
                        &engine,
                        &ctx,
                        &styles,
                        args,
                        document_input::MutationKind::Patch,
                    )
                    .await?
                }
                TaskSub::Edit { id } => commands::task::edit(&engine, &styles, id).await?,
                TaskSub::Cancel { id } => commands::task::cancel(&engine, &styles, id).await?,
                TaskSub::Approve { id } => commands::task::approve(&engine, &styles, id).await?,
                TaskSub::Reject { id, reason } => {
                    commands::task::reject(&engine, &styles, id, reason.as_deref()).await?;
                }
                TaskSub::Retry { id } => commands::task::retry(&engine, &styles, id).await?,
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
                            prompt_mode: parse_enum(flags.prompt_mode.as_deref(), "prompt-mode")?,
                            role_summary: flags.role_summary.clone(),
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
                AgentSub::Shop(args) => {
                    template_shop::run(
                        &engine,
                        &ctx,
                        &styles,
                        args,
                        env.stdout_is_tty,
                        env.stdin_is_tty,
                    )
                    .await?;
                }
                AgentSub::List => commands::agent::list(&engine, &ctx, &styles).await?,
                AgentSub::Status { id, effective } => {
                    commands::agent::status(&engine, &ctx, &styles, id, *effective).await?;
                }
                AgentSub::Update(args) => match &args.file {
                    // The file form is the unchanged full replacement.
                    Some(file) => {
                        commands::agent::mutate(
                            &engine,
                            &ctx,
                            &styles,
                            &crate::tree::EntityDefinitionInput {
                                id: args.id.clone(),
                                input: crate::tree::DefinitionInput {
                                    file: file.clone(),
                                    input_format: args.input_format,
                                },
                            },
                            document_input::MutationKind::Update,
                        )
                        .await?
                    }
                    None => {
                        commands::agent::update_flags(
                            &engine,
                            &ctx,
                            &styles,
                            &args.id,
                            commands::agent::AgentFlagUpdate {
                                prompt_mode: parse_enum(
                                    args.prompt_mode.as_deref(),
                                    "prompt-mode",
                                )?,
                                effort: parse_enum(args.effort.as_deref(), "effort")?,
                                role_summary: args.role_summary.clone(),
                            },
                        )
                        .await?
                    }
                },
                AgentSub::Patch(args) => {
                    commands::agent::mutate(
                        &engine,
                        &ctx,
                        &styles,
                        args,
                        document_input::MutationKind::Patch,
                    )
                    .await?
                }
                AgentSub::Edit { id } => {
                    commands::agent::edit(&engine, &ctx, &styles, id).await?;
                }
                AgentSub::Remove { id } => commands::agent::remove(&engine, &styles, id).await?,
                AgentSub::Disable { id } => {
                    commands::agent::set_enabled(&engine, &styles, id, false).await?;
                }
                AgentSub::Enable { id } => {
                    commands::agent::set_enabled(&engine, &styles, id, true).await?;
                }
                AgentSub::Autonomous { id, on, off } => {
                    commands::agent::autonomous(&engine, &ctx, &styles, id, *on, *off).await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Logs(args)) => {
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            commands::run::logs::run(
                &engine,
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

        Some(Command::Serve(args)) => {
            let (_, _, client) = build(cli, env, &cwd).await?;
            commands::run::serve_once(&client, args).await
        }

        Some(Command::Web(args)) => run_web(cli, env, &cwd, args).await,

        // Dispatched before the runtime is built, in `dispatch_parsed`.
        Some(Command::Daemon(_)) => unreachable!("daemon dispatch precedes the runtime"),
        Some(Command::Mcp(_)) => unreachable!("orch mcp is dispatched before the runtime"),

        Some(Command::Remote(args)) => {
            let Some(sub) = &args.command else {
                return Ok(print_group_help(&["remote"]));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            commands::remote::run(&engine, &ctx, &styles, sub).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Status) => {
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            commands::status::run(&engine, &ctx, &styles).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Config(ConfigCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["config"]));
            };
            if let ConfigSub::Global(ConfigGlobalCmd { command: None }) = sub {
                return Ok(print_group_help(&["config", "global"]));
            }
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                ConfigSub::Get { key } => {
                    commands::config::get(&engine, &ctx, &styles, key).await?;
                }
                ConfigSub::Set { key, value } => {
                    commands::config::set(&engine, &ctx, &styles, key, value).await?;
                }
                ConfigSub::Update(args) => {
                    commands::config::mutate(
                        &engine,
                        &ctx,
                        &styles,
                        args,
                        document_input::MutationKind::Update,
                    )
                    .await?
                }
                ConfigSub::Patch(args) => {
                    commands::config::mutate(
                        &engine,
                        &ctx,
                        &styles,
                        args,
                        document_input::MutationKind::Patch,
                    )
                    .await?
                }
                ConfigSub::Edit => commands::config::edit(&engine).await?,
                ConfigSub::Global(ConfigGlobalCmd { command }) => match command {
                    Some(ConfigGlobalSub::Get { key }) => {
                        commands::config::global_get(&engine, &ctx, &styles, key).await?;
                    }
                    Some(ConfigGlobalSub::Set { key, value }) => {
                        commands::config::global_set(&engine, &ctx, &styles, key, value).await?;
                    }
                    Some(ConfigGlobalSub::Show) => {
                        commands::config::global_show(&engine, &ctx, &styles).await?;
                    }
                    // Handled above, before the container is built.
                    None => unreachable!("the empty group prints help"),
                },
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Context(ContextCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["context"]));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                ContextSub::Set { key, value, ttl } => {
                    let ttl_ms = match ttl {
                        Some(raw) => Some(parse_int(raw, "ttl")? as u64),
                        None => None,
                    };
                    commands::context::set(&engine, &ctx, &styles, key, value, ttl_ms).await?;
                }
                ContextSub::Get { key } => {
                    commands::context::get(&engine, &ctx, &styles, key).await?;
                }
                ContextSub::List => commands::context::list(&engine, &ctx, &styles).await?,
                ContextSub::Delete { key } => {
                    commands::context::delete(&engine, &ctx, &styles, key).await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Msg(MsgCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["msg"]));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                MsgSub::Send {
                    to_agent_id,
                    body,
                    subject,
                    from,
                    ttl,
                    reply_to,
                } => {
                    commands::msg::send(
                        &engine,
                        &ctx,
                        &styles,
                        commands::msg::MsgSendArgs {
                            to_agent_id,
                            body,
                            subject: subject.as_deref(),
                            from: from.as_deref(),
                            ttl_ms: parse_ttl(ttl.as_deref())?,
                            reply_to: reply_to.as_deref(),
                        },
                    )
                    .await?;
                }
                MsgSub::Broadcast {
                    body,
                    subject,
                    from,
                    team,
                    ttl,
                } => {
                    commands::msg::broadcast(
                        &engine,
                        &ctx,
                        &styles,
                        commands::msg::MsgBroadcastArgs {
                            body,
                            subject: subject.as_deref(),
                            from: from.as_deref(),
                            team: team.as_deref(),
                            ttl_ms: parse_ttl(ttl.as_deref())?,
                        },
                    )
                    .await?;
                }
                MsgSub::Inbox { agent_id } => {
                    commands::msg::inbox(&engine, &ctx, &styles, agent_id).await?;
                }
                MsgSub::List { agent } => {
                    commands::msg::list(&engine, &ctx, &styles, agent.as_deref()).await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Goal(GoalCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["goal"]));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                GoalSub::Add {
                    title,
                    description,
                    assignee,
                } => {
                    commands::goal::add(
                        &engine,
                        &ctx,
                        &styles,
                        title,
                        description.as_deref(),
                        assignee.as_deref(),
                    )
                    .await?;
                }
                GoalSub::List { status } => {
                    commands::goal::list(&engine, &ctx, &styles, status.as_deref()).await?;
                }
                GoalSub::Show { id } => commands::goal::show(&engine, &ctx, &styles, id).await?,
                GoalSub::Status { id, status, force } => {
                    // The only subcommand that reports a failure without
                    // raising: an unknown status name exits 1 from here.
                    return commands::goal::status(&engine, &ctx, &styles, id, status, *force)
                        .await;
                }
                GoalSub::Update {
                    id,
                    title,
                    description,
                    assignee,
                } => {
                    commands::goal::update(
                        &engine,
                        &ctx,
                        &styles,
                        id,
                        orch_engine::goal_service::UpdateGoalFields {
                            title: title.clone(),
                            description: description.clone(),
                            assignee: assignee.clone(),
                        },
                    )
                    .await?;
                }
                GoalSub::Delete { id } => {
                    commands::goal::delete(&engine, &ctx, &styles, id).await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Team(TeamCmd { command })) => {
            let Some(sub) = command else {
                return Ok(print_group_help(&["team"]));
            };
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            match sub {
                TeamSub::Create {
                    name,
                    lead,
                    members,
                    description,
                    no_auto_claim,
                } => {
                    commands::team::create(
                        &engine,
                        &ctx,
                        &styles,
                        commands::team::TeamCreateArgs {
                            name,
                            lead,
                            members: members.as_deref(),
                            description: description.as_deref(),
                            no_auto_claim: *no_auto_claim,
                        },
                    )
                    .await?;
                }
                TeamSub::List => commands::team::list(&engine, &ctx, &styles).await?,
                TeamSub::Show { id } => commands::team::show(&engine, &ctx, &styles, id).await?,
                TeamSub::Join { team_id, agent_id } => {
                    commands::team::join(&engine, &styles, team_id, agent_id).await?;
                }
                TeamSub::Leave { team_id, agent_id } => {
                    commands::team::leave(&engine, &styles, team_id, agent_id).await?;
                }
                TeamSub::AddTask { team_id, task_id } => {
                    commands::team::add_task(&engine, &styles, team_id, task_id).await?;
                }
                TeamSub::SetLead { team_id, agent_id } => {
                    commands::team::set_lead(&engine, &styles, team_id, agent_id).await?;
                }
                TeamSub::Disband { id } => {
                    commands::team::disband(&engine, &styles, id).await?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Org(args)) => commands::org::dispatch(cli, env, &cwd, args).await,
        Some(Command::Run(RunCmd {
            task_id,
            all,
            once,
            watch: _,
            from_agent: _,
            verbose: _,
            tick_interval: _,
            peer,
        })) => {
            if !*all && !*once && task_id.is_none() {
                return Err(Error::invalid_arguments(
                    "Specify a task ID, --all, --once, or --watch",
                ));
            }
            if peer.listen.is_some() || peer.peer_name.is_some() {
                return Err(Error::invalid_arguments(
                    "--listen and --peer-name require run --watch",
                ));
            }
            let (ctx, styles, engine) = build(cli, env, &cwd).await?;
            commands::run::report_startup_diagnostics(&engine, &styles).await?;
            if *all {
                commands::run::all(&engine, &styles).await
            } else if *once {
                commands::run::once(&engine, &ctx, &styles).await
            } else {
                commands::run::single(&engine, &styles, task_id.as_deref().expect("checked above"))
                    .await
            }
        }

        Some(Command::Tui(_args)) => {
            let ctx =
                CliContext::resolve(&cwd, cli.output(), cli.quiet, cli.no_color, cli.ascii, env);
            // The dashboard is another socket client, as the root `--agent`.
            tui::run_as_agent(ctx.project_root, version(), cli.agent.as_deref()).await
        }

        // A bare invocation is another dashboard client. Initialization is
        // explicit, so this path cannot create a project behind the owner.
        None if shape.is_bare => {
            let ctx =
                CliContext::resolve(&cwd, cli.output(), cli.quiet, cli.no_color, cli.ascii, env);
            tui::run_as_agent(ctx.project_root, version(), cli.agent.as_deref()).await
        }

        // Flags but no subcommand (`orch --json`): `tui` is appended
        // only for a bare argv, so this prints the root help instead.
        None => Ok(print_group_help_root()),
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
        return Ok(ExitCode::from(2));
    }
    let prompt_mode = match args.prompt_mode.as_deref() {
        None => None,
        Some(raw) => match raw.parse::<orch_domain::PromptMode>() {
            Ok(mode) => Some(mode),
            Err(()) => {
                print_error(
                    &styles,
                    &format!("Unknown prompt mode \"{raw}\""),
                    Some("Supported: normal, eco"),
                );
                return Ok(ExitCode::from(2));
            }
        },
    };

    commands::init::run(
        &styles,
        cwd,
        &commands::init::InitArgs {
            name: args.name.clone(),
            adapter: args.adapter.clone(),
            prompt_mode,
            interactive: env.stdout_is_tty && env.stdin_is_tty,
        },
    )
    .await?;
    // Unconditional: a re-init prints `Already initialized` *and* the hint.
    println!(
        "  Next: {}",
        styles.dim(commands::init::next_hint(args.adapter.as_deref()))
    );
    println!();
    Ok(ExitCode::SUCCESS)
}

async fn run_web(
    cli: &Cli,
    env: EnvFlags,
    cwd: &std::path::Path,
    args: &WebCmd,
) -> Result<ExitCode> {
    let web_args = match web::WebArgs::parse(web::WebOptions {
        bind: &args.bind,
        password: args.password.clone(),
        env_password: std::env::var(web::PASSWORD_ENV).ok(),
        no_auth: args.no_auth,
        allow_hosts: &args.allow_host,
        observe: args.observe,
    }) {
        Ok(web_args) => web_args,
        Err(message) => {
            print_error(&styles_for(cli, env), &message, None);
            return Ok(ExitCode::from(2));
        }
    };
    web::run_as_agent(web_args, cwd.to_path_buf(), cli.agent.as_deref()).await
}

/// `orch serve` (without `--once`) and `orch run --watch` are explicit
/// foreground daemon starts: their peer flags and `--from-agent` override.
fn foreground_start(command: Option<&Command>) -> Option<(&tree::PeerFlags, bool)> {
    match command {
        Some(Command::Serve(args)) if !args.once => Some((&args.peer, args.from_agent)),
        Some(Command::Run(args)) if args.watch => Some((&args.peer, args.from_agent)),
        _ => None,
    }
}

/// Root `--agent` is the caller identity for every client (over
/// ORCH_AGENT_ID); `mcp --agent` is the more specific spelling and wins.
fn inherit_root_agent(cli: &mut Cli) {
    if let Some(Command::Mcp(args)) = &mut cli.command
        && args.agent.is_none()
    {
        args.agent.clone_from(&cli.agent);
    }
}

/// The version the dashboard reports, which is the one `--version` prints.
fn version() -> &'static str {
    tree::VERSION
}

/// Send `tracing` output to **stderr**, and only when asked for.
///
/// Until now nothing installed a subscriber, so every `tracing::warn!` in the
/// engine, the adapters and the stores went nowhere: the swallowed-error paths
/// that are deliberate decisions (a corrupt lock, an unreadable entity
/// file, a dropped event) left no trace at all, and the first question in any
/// incident — "what did it decide to ignore?" — had no answer.
///
/// Three rules, in this order of importance:
///
/// 1. **stderr, never stdout.** Stdout is the contract: it is compared
///    byte for byte and `--json` output has to stay parseable, so a diagnostic there would corrupt both.
/// 2. **Silent by default**, so no invocation's output changes.
/// 3. `RUST_LOG` takes an `EnvFilter` for anyone who knows what they want;
///    `ORCHESTRY_DEBUG` — which already exists, as the flag that prints an
///    error's cause chain — turns on `debug` without one.
///
/// One cap sits above all three: the MCP SDK's `debug` and `trace` events are
/// never recorded, whatever `RUST_LOG` says (see [`sdk_event_carries_no_payload`]).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.trim().is_empty() => EnvFilter::new(spec),
        _ if std::env::var_os("ORCHESTRY_DEBUG").is_some() => EnvFilter::new("debug"),
        // No targets enabled: the subscriber is installed but records nothing,
        // which costs a filter check per call site and nothing else.
        _ => EnvFilter::new("off"),
    };

    // `try_init` rather than `init`: a test binary may install its own, and a
    // second attempt must not abort the process over logging.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .without_time()
        .finish()
        .with(tracing_subscriber::filter::filter_fn(
            sdk_event_carries_no_payload,
        ))
        .try_init();
}

/// Whether a span or event may be recorded at all, before any `RUST_LOG`
/// directive is consulted.
///
/// `rmcp` logs every request, every result and every line it cannot parse, in
/// full, at `debug`: with diagnostics on, `orch mcp` would copy tool arguments
/// and results — task bodies, context values, message text — to stderr, which
/// MCP clients commonly keep in a log file (design D13). This is a separate
/// layer rather than a directive appended to the `EnvFilter` because a filter
/// lets the most specific directive win, so `rmcp::service=trace` would
/// re-enable what `rmcp=info` turned off; a layer that says no is final.
/// The SDK's `info` and above stay: they carry lifecycle and failures, not
/// payloads. Only `orch mcp` links the SDK, so every other command is
/// unaffected.
fn sdk_event_carries_no_payload(metadata: &tracing::Metadata<'_>) -> bool {
    let target = metadata.target();
    let from_sdk = target == "rmcp" || target.starts_with("rmcp::");
    !from_sdk || *metadata.level() <= tracing::Level::INFO
}

fn styles_for(cli: &Cli, env: EnvFlags) -> Styles {
    Styles {
        color: !cli.no_color && !env.no_color_env && env.stdout_is_tty,
        ascii: cli.ascii || env.dumb_term,
    }
}

/// Resolve presentation flags and connect one serialized daemon session.
async fn build(
    cli: &Cli,
    env: EnvFlags,
    cwd: &std::path::Path,
) -> Result<(CliContext, Styles, commands::rpc::Rpc)> {
    let ctx = CliContext::resolve(cwd, cli.output(), cli.quiet, cli.no_color, cli.ascii, env);
    let styles = ctx.styles(env);
    let engine = commands::rpc::Rpc::connect(cli, cwd).await?;
    Ok((ctx, styles, engine))
}

/// A command group invoked with no subcommand prints its help to **stdout**
/// and exits **1**.
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

/// Leading whitespace, an optional sign, then as many digits as there are —
/// `"3abc"` is `3` and `"abc"` is `None`.
pub(crate) fn js_parse_int(raw: &str) -> Option<i64> {
    let text = raw.trim_start();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    digits[..end].parse::<i64>().ok().map(|value| sign * value)
}

/// A non-numeric value is rejected for `--max-attempts`, `--max-turns` and
/// `--ttl` rather than stored as `NaN` in the entity. `--priority` is
/// different: see [`parse_priority`].
pub(crate) fn parse_int(raw: &str, flag: &str) -> Result<i64> {
    js_parse_int(raw)
        .ok_or_else(|| Error::invalid_arguments(format!("--{flag} must be an integer")))
}

/// A `NaN` priority stands in as an out-of-range number.
///
/// `--priority notanumber` must reach the service and be rejected *there*,
/// with the service's own message and in the service's own order: an empty
/// title is reported before a bad priority, so rejecting it here would print
/// the wrong error for `task add '' --priority nope`. Every value outside
/// `1..=4` fails that check identically, so any one of them reproduces `NaN`
/// exactly.
pub(crate) fn parse_priority(raw: &str) -> i64 {
    js_parse_int(raw).unwrap_or(i64::MIN)
}

/// `opts.ttl ? parseInt(opts.ttl, 10) : undefined` — a TTL of `0` is a real
/// value (the string `"0"` is truthy), so it reaches the service and fails its
/// range check rather than being treated as absent.
fn parse_ttl(raw: Option<&str>) -> Result<Option<u64>> {
    match raw {
        None | Some("") => Ok(None),
        Some(raw) => Ok(Some(parse_int(raw, "ttl")? as u64)),
    }
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

    fn mcp_agent(argv: &[&str]) -> Option<String> {
        let mut cli = Cli::try_parse_from(argv).expect("parses");
        inherit_root_agent(&mut cli);
        match cli.command {
            Some(Command::Mcp(args)) => args.agent,
            _ => panic!("mcp"),
        }
    }

    #[test]
    fn root_agent_reaches_mcp_and_the_mcp_flag_is_more_specific() {
        assert_eq!(
            mcp_agent(&["orch", "--agent", "agt_r", "mcp"]).as_deref(),
            Some("agt_r")
        );
        assert_eq!(
            mcp_agent(&["orch", "--agent", "agt_r", "mcp", "--agent", "agt_m"]).as_deref(),
            Some("agt_m")
        );
        assert_eq!(mcp_agent(&["orch", "mcp"]), None);
    }

    fn foreground(argv: &[&str]) -> Option<bool> {
        let cli = Cli::try_parse_from(argv).expect("parses");
        foreground_start(cli.command.as_ref()).map(|(_, from_agent)| from_agent)
    }

    /// W2b B1 / spec R31: `serve` and `run --watch` are explicit starts and
    /// take `daemon start`'s `--from-agent`; it reaches the start the
    /// agent-context guard checks. A batch or one-shot run is no start.
    #[test]
    fn foreground_starts_forward_from_agent_and_batches_refuse_it() {
        assert_eq!(foreground(&["orch", "serve"]), Some(false));
        assert_eq!(foreground(&["orch", "serve", "--from-agent"]), Some(true));
        assert_eq!(foreground(&["orch", "run", "--watch"]), Some(false));
        assert_eq!(
            foreground(&["orch", "run", "--watch", "--from-agent"]),
            Some(true)
        );
        assert_eq!(foreground(&["orch", "serve", "--once"]), None);
        assert_eq!(foreground(&["orch", "run", "--once"]), None);
        assert_eq!(foreground(&["orch", "run", "tsk_1"]), None);

        use clap::error::ErrorKind;
        for (refused, kind) in [
            (
                &["orch", "serve", "--once", "--from-agent"][..],
                ErrorKind::ArgumentConflict,
            ),
            (
                &["orch", "run", "--from-agent", "tsk_1"],
                ErrorKind::MissingRequiredArgument,
            ),
            (
                &["orch", "run", "--once", "--from-agent"],
                ErrorKind::MissingRequiredArgument,
            ),
        ] {
            let error = Cli::try_parse_from(refused).expect_err("refused");
            assert_eq!(error.kind(), kind, "{refused:?}");
        }
    }

    #[test]
    fn parse_int_rejects_a_non_numeric_value_with_exit_2() {
        let err = parse_int("abc", "max-attempts").expect_err("rejected");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(parse_int(" 4 ", "max-attempts").unwrap(), 4);
        assert_eq!(parse_int("-1", "max-attempts").unwrap(), -1);
    }

    #[test]
    fn js_parse_int_takes_the_leading_digits_and_nothing_else() {
        assert_eq!(js_parse_int("3abc"), Some(3));
        assert_eq!(js_parse_int("  -12 "), Some(-12));
        assert_eq!(js_parse_int("+7"), Some(7));
        // `parseInt` with radix 10 stops at the `x`.
        assert_eq!(js_parse_int("0x10"), Some(0));
        for nan in ["abc", "", "   ", "-", "+", ".5"] {
            assert_eq!(js_parse_int(nan), None, "{nan:?}");
        }
    }

    #[test]
    fn a_non_numeric_priority_reaches_the_service_as_out_of_range() {
        assert_eq!(parse_priority("2"), 2);
        assert_eq!(parse_priority("2abc"), 2);
        // Not 1..=4, so the service rejects it exactly as it rejects `NaN`.
        assert!(!(1..=4).contains(&parse_priority("notanumber")));
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
