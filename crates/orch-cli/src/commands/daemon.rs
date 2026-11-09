//! Explicit owner lifecycle and socket-only control commands (PR 1).
use clap::{Args, Subcommand, ValueEnum};
use orch_client::{Client, ClientConfig};
use orch_proto::ops::args::DispatchSwitch;
use std::process::ExitCode;

#[derive(Debug, Args)]
pub struct DaemonCmd {
    #[command(subcommand)]
    pub command: DaemonSub,
}

#[derive(Debug, Subcommand)]
pub enum DaemonSub {
    /// Start the project daemon explicitly
    Start(StartArgs),
    /// Show live daemon status
    Status,
    /// Stop and reconcile the project daemon, including across version skew
    Stop,
    /// Enable or disable future automatic dispatch
    Dispatch {
        #[arg(value_enum)]
        mode: Switch,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    #[arg(long)]
    pub detach: bool,
    #[arg(long, value_enum, default_value = "off")]
    pub dispatch: Switch,
    #[arg(long)]
    pub from_agent: bool,
    #[command(flatten)]
    pub peer: crate::tree::PeerFlags,
}

/// `orch serve` / `orch run --watch`: an explicit foreground start with
/// dispatch ON. `from_agent` is the same override `daemon start` takes (R31).
pub fn foreground(
    cli: &crate::tree::Cli,
    peer: &crate::tree::PeerFlags,
    from_agent: bool,
    cwd: &std::path::Path,
) -> ExitCode {
    let result = run(cli, &foreground_command(peer, from_agent), cwd);
    if result == ExitCode::from(4) {
        eprintln!("    Use: orch daemon dispatch on");
    }
    result
}

fn foreground_command(peer: &crate::tree::PeerFlags, from_agent: bool) -> DaemonCmd {
    DaemonCmd {
        command: DaemonSub::Start(StartArgs {
            detach: false,
            dispatch: Switch::On,
            from_agent,
            peer: crate::tree::PeerFlags {
                listen: peer.listen.clone(),
                peer_name: peer.peer_name.clone(),
            },
        }),
    }
}

/// An explicit start from an agent's environment needs `--from-agent` (R31).
fn refused_in_agent_context(agent_context: bool, args: &StartArgs) -> bool {
    agent_context && !args.from_agent
}

pub fn run(cli: &crate::tree::Cli, args: &DaemonCmd, cwd: &std::path::Path) -> ExitCode {
    let config = match ClientConfig::from_env(cwd.to_path_buf())
        .and_then(|cfg| cfg.with_agent(cli.agent.as_deref()))
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{}", error.render(false));
            return code(error.exit_code());
        }
    };
    // The lifecycle owns sanitization. Prepare before constructing Tokio so
    // every daemon child uses the owner's fixed environment snapshot (R21).
    let prepared = if let DaemonSub::Start(args) = &args.command {
        if refused_in_agent_context(config.agent_context, args) {
            eprintln!("  ✕ {}", orch_proto::error::START_REFUSED_IN_AGENT_CONTEXT);
            return ExitCode::from(2);
        }
        let project_root =
            match orch_daemon::select_project_root(config.project_root.as_deref(), cwd) {
                Ok(root) => root,
                Err(error) => return lifecycle_result(Err(error)),
            };
        let result = orch_daemon::prepare_start(orch_daemon::StartOptions {
            project_root,
            socket: config.socket.clone(),
            detach: args.detach,
            dispatch: match args.dispatch {
                Switch::On => orch_daemon::DispatchMode::On,
                Switch::Off => orch_daemon::DispatchMode::Off,
            },
            from_agent: args.from_agent,
            listen: args.peer.listen.clone(),
            peer_name: args.peer.peer_name.clone(),
        });
        match result {
            Ok(prepared) => {
                // A detached daemon's own warning goes to daemon.log; repeat
                // an explicit --listen here, where the owner sees it.
                if args.detach
                    && let Some(warning) = args
                        .peer
                        .listen
                        .as_deref()
                        .and_then(crate::commands::remote::exposure_warning)
                {
                    eprintln!("  ⚠ {warning}");
                }
                Some(prepared)
            }
            Err(error) => return lifecycle_result(Err(error)),
        }
    } else {
        None
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("  ✕ failed to start the async runtime: {error}");
            return ExitCode::from(1);
        }
    };
    if let Some(prepared) = prepared {
        return lifecycle_result(runtime.block_on(prepared.run()));
    }
    let result = runtime.block_on(control(cli, &args.command, &config));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.render(config.agent_context));
            code(error.exit_code())
        }
    }
}

fn lifecycle_result(result: Result<orch_daemon::DaemonExit, orch_daemon::DaemonError>) -> ExitCode {
    match result {
        Ok(exit) => code(exit.code),
        Err(error) => {
            eprintln!(
                "  ✕ {}",
                orch_proto::path::escape_controls(&error.to_string())
            );
            code(error.exit_code())
        }
    }
}

async fn control(
    cli: &crate::tree::Cli,
    command: &DaemonSub,
    config: &ClientConfig,
) -> Result<(), orch_client::ClientError> {
    match command {
        DaemonSub::Stop => {
            let outcome = orch_client::stop(config).await?;
            render(cli, &outcome, "Daemon stopped cleanly")?;
        }
        DaemonSub::Status => {
            let status = Client::connect(config).await?.status().await?;
            let text = format!(
                "orch daemon PID {} · dispatch {}\n  executable: {}\n  started: {}\n  version: orch {} protocol {}\n  sessions: {}/{}\n  ownership: {}\n  socket: {}\n  instance: {}\n  running tasks: {}",
                status.pid,
                if status.dispatch.dispatch_on {
                    "ON"
                } else {
                    "OFF"
                },
                status.executable.display_escaped(),
                orch_proto::path::escape_controls(&status.started_at),
                orch_proto::path::escape_controls(&status.binary),
                status.protocol,
                status.sessions,
                status.session_cap,
                match status.ownership {
                    orch_proto::ops::results::OwnershipStatus::Held => "held",
                    orch_proto::ops::results::OwnershipStatus::Lost =>
                        "LOST — stop and restart the daemon",
                },
                status.socket.display_escaped(),
                orch_proto::path::escape_controls(&status.instance),
                status.running_tasks.len()
            );
            render(cli, &status, &text)?;
        }
        DaemonSub::Dispatch { mode } => {
            let state = Client::connect(config)
                .await?
                .dispatch(match mode {
                    Switch::On => DispatchSwitch::On,
                    Switch::Off => DispatchSwitch::Off,
                })
                .await?;
            render(
                cli,
                &state,
                if state.dispatch_on {
                    "Dispatch ON"
                } else {
                    "Dispatch OFF"
                },
            )?;
        }
        DaemonSub::Start(_) => unreachable!("start is handled before client connection"),
    }
    Ok(())
}

fn render(
    cli: &crate::tree::Cli,
    value: &impl serde::Serialize,
    text: &str,
) -> Result<(), orch_client::ClientError> {
    let result = if cli.json {
        serde_json::to_string_pretty(value)
            .map(Some)
            .map_err(|_| ())
    } else if cli.yaml {
        serde_yaml_ng::to_string(value).map(Some).map_err(|_| ())
    } else {
        Ok(None)
    };
    match result {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) if !cli.quiet => println!("{text}"),
        Ok(None) => {}
        Err(()) => {
            return Err(orch_client::ClientError::InvalidArguments(
                "cannot serialize daemon response".into(),
            ));
        }
    }
    Ok(())
}
fn code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::PeerFlags;

    fn start(command: &DaemonCmd) -> &StartArgs {
        match &command.command {
            DaemonSub::Start(args) => args,
            other => panic!("expected a start, got {other:?}"),
        }
    }

    /// The foreground aliases start with dispatch ON, attached, and carry the
    /// override and peer flags through to the start the guard checks.
    #[test]
    fn foreground_command_forwards_the_override_and_peer_flags() {
        let peer = PeerFlags {
            listen: Some("127.0.0.1:7690".into()),
            peer_name: Some("n".into()),
        };
        for from_agent in [false, true] {
            let command = foreground_command(&peer, from_agent);
            let args = start(&command);
            assert_eq!(args.from_agent, from_agent);
            assert!(!args.detach);
            assert!(matches!(args.dispatch, Switch::On));
            assert_eq!(args.peer.listen, peer.listen);
            assert_eq!(args.peer.peer_name, peer.peer_name);
        }
    }

    #[test]
    fn an_agent_context_start_is_refused_unless_from_agent() {
        let peer = PeerFlags {
            listen: None,
            peer_name: None,
        };
        let plain = foreground_command(&peer, false);
        let overridden = foreground_command(&peer, true);
        assert!(refused_in_agent_context(true, start(&plain)));
        assert!(!refused_in_agent_context(true, start(&overridden)));
        assert!(!refused_in_agent_context(false, start(&plain)));
        assert!(!refused_in_agent_context(false, start(&overridden)));
    }
}
