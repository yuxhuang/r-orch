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
    /// Start the project daemon explicitly (developmental)
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

const GATE_ERROR: &str =
    "orch daemon is developmental in this build; set ORCH_DAEMON_DEVELOPMENTAL=1 to run it";

pub fn run(cli: &crate::tree::Cli, args: &DaemonCmd, cwd: &std::path::Path) -> ExitCode {
    // This check precedes discovery, connection and any lifecycle action, for
    // every daemon verb. Existing serve/watch entry points are unchanged.
    if std::env::var_os(orch_daemon::DEVELOPMENTAL_ENV).as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        eprintln!("  ✕ {GATE_ERROR}");
        return ExitCode::from(2);
    }
    let config = match ClientConfig::from_env(cwd.to_path_buf()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{}", error.render(false));
            return code(error.exit_code());
        }
    };
    // The lifecycle owns sanitization. Prepare before constructing Tokio so
    // every daemon child uses the owner's fixed environment snapshot (R21).
    let prepared = if let DaemonSub::Start(args) = &args.command {
        if config.agent_context && !args.from_agent {
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
            developmental: true,
        });
        match result {
            Ok(prepared) => Some(prepared),
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
                "orch daemon PID {} · dispatch {}\n  executable: {}\n  started: {}\n  version: orch {} protocol {}\n  sessions: {}/{}",
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
                status.session_cap
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
