//! Explicit scheduling over the daemon socket. Disconnecting an observer
//! leaves its runs owned by the daemon; task cancel is the only kill command.
pub mod logs;

use super::rpc::{Rpc, client_error};
use crate::context::CliContext;
use crate::output::{Color, Styles};
use orch_client::Progress;
use orch_core::error::{Error, Result};
use orch_domain::OrchestratorEvent;
use orch_proto::ops::{Op, args};
use std::process::ExitCode;

pub async fn report_startup_diagnostics(engine: &Rpc, styles: &Styles) -> Result<()> {
    let bootstrap = engine.request(args::UiBootstrapArgs {}).await?;
    for warning in bootstrap.startup_warnings {
        eprintln!("  {} {warning}", styles.paint(Color::Yellow, "⚠"));
    }
    Ok(())
}

pub async fn single(engine: &Rpc, styles: &Styles, task_id: &str) -> Result<ExitCode> {
    let task = engine
        .call::<args::TaskGetArgs>(serde_json::json!({"id":task_id}))
        .await?;
    println!(
        "\n  {} · running {task_id} \"{}\"\n",
        styles.paint(Color::Amber, "orch"),
        task.title
    );
    let args =
        super::rpc::arguments::<args::DispatchRunTaskArgs>(serde_json::json!({"task_id":task_id}))?;
    let (outcome, signal) = stream(engine, args, |event| print_event(styles, event)).await?;
    Ok(code(outcome.exit_code, signal))
}

pub async fn all(engine: &Rpc, styles: &Styles) -> Result<ExitCode> {
    println!(
        "\n  {} · running all todo tasks\n",
        styles.paint(Color::Amber, "orch")
    );
    let (outcome, signal) = stream(engine, args::DispatchRunAllArgs {}, |event| {
        print_event(styles, event)
    })
    .await?;
    Ok(code(outcome.exit_code, signal))
}

pub async fn once(engine: &Rpc, ctx: &CliContext, styles: &Styles) -> Result<ExitCode> {
    let (outcome, signal) = stream(engine, args::DispatchTickArgs {}, |event| {
        print_event(styles, event)
    })
    .await?;
    if let Some(format) = ctx.output {
        format.print(&outcome)?;
    }
    Ok(code(0, signal))
}

pub async fn serve_once(engine: &Rpc, flags: &crate::tree::ServeCmd) -> Result<ExitCode> {
    let format = crate::serve::LogFormat::parse(&flags.log_format).ok_or_else(|| {
        Error::invalid_arguments(format!(
            "Unknown --log-format \"{}\". Valid: json, text",
            flags.log_format
        ))
    })?;
    let logger = crate::serve::StructuredLogger::new(
        format,
        flags.verbose,
        std::sync::Arc::new(orch_infra::clock::SystemClock),
        crate::serve::open_sinks(flags.log_file.as_deref())?,
    );
    logger.log(
        crate::serve::LogLevel::Info,
        "serve:started",
        vec![("mode", serde_json::json!("once"))],
    );
    let (result, signal) = stream(engine, args::DispatchUntilTerminalArgs {}, |event| {
        logger.handle(event)
    })
    .await?;
    let exit = match result {
        orch_proto::ops::results::BatchResult::AllDone => 0,
        _ => 1,
    };
    logger.log(
        crate::serve::LogLevel::Info,
        "serve:finished",
        vec![
            ("result", serde_json::json!(result)),
            ("exit_code", serde_json::json!(exit)),
        ],
    );
    logger.flush();
    Ok(code(exit, signal))
}

fn code(exit: i32, signal: Option<u8>) -> ExitCode {
    ExitCode::from(signal.unwrap_or_else(|| u8::try_from(exit).unwrap_or(1)))
}

fn print_event(styles: &Styles, event: &OrchestratorEvent) {
    if let Some(line) = crate::logs::format_live_event(styles, event, &crate::logs::now_time()) {
        println!("{line}");
    }
}

/// Install all observer signal handlers before dispatch. Cancellation is a
/// protocol request and bounded by orch-client; it never kills an agent.
async fn stream<A: Op>(
    engine: &Rpc,
    args: A,
    mut render: impl FnMut(&OrchestratorEvent),
) -> Result<(A::Output, Option<u8>)> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())
        .map_err(|e| Error::other(format!("cannot listen for SIGINT: {e}")))?;
    let mut terminate = signal(SignalKind::terminate())
        .map_err(|e| Error::other(format!("cannot listen for SIGTERM: {e}")))?;
    let mut hangup = signal(SignalKind::hangup())
        .map_err(|e| Error::other(format!("cannot listen for SIGHUP: {e}")))?;
    let mut lane = engine.lane().await;
    let mut call = lane.start(args).await.map_err(client_error)?;
    loop {
        let signal = tokio::select! {
            result = call.next() => {
                match result.map_err(client_error)? {
                    Progress::Event(event) => render(&event),
                    Progress::Done(value) => return Ok((value,None)),
                }
                continue;
            }
            _ = interrupt.recv() => 130,
            _ = terminate.recv() => 143,
            _ = hangup.recv() => 129,
        };
        return call
            .cancel()
            .await
            .map(|value| (value, Some(signal)))
            .map_err(client_error);
    }
}

/// Local executable/dependency checks remain available before owner startup;
/// project checks run only inside the daemon.
pub async fn doctor(
    cli: &crate::tree::Cli,
    styles: &Styles,
    cwd: &std::path::Path,
) -> Result<ExitCode> {
    let client = match Rpc::connect(cli, cwd).await {
        Ok(client) => client,
        Err(error) => {
            crate::doctor::run(styles, cwd, cli.output(), None).await?;
            if super::rpc::recover_client_error(&error).is_some_and(|e| {
                matches!(
                    e,
                    orch_client::ClientError::Connection(orch_client::NoDaemon::NotInitialized)
                )
            }) {
                return Ok(ExitCode::SUCCESS);
            }
            return Err(error);
        }
    };
    let report = client.request(args::DoctorRunArgs {}).await?;
    let report: orch_engine::DoctorReport = super::rpc::arguments(report)?;
    print!("{}", crate::doctor::render_header(styles));
    if let Some(format) = cli.output() {
        format.print(&crate::doctor::DoctorJson::from(&report))?;
    } else {
        let status = client.request(args::StatusGetArgs {}).await?;
        print!(
            "{}",
            crate::doctor::render_report(
                styles,
                &report,
                &crate::doctor::ProjectLine::Initialized {
                    agents: status.agents.len(),
                    tasks: status.tasks.len()
                }
            )
        );
    }
    Ok(ExitCode::SUCCESS)
}
