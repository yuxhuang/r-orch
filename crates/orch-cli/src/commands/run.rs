//! Explicit scheduling over the daemon socket. Disconnecting an observer
//! leaves its runs owned by the daemon; task cancel is the only kill command.
pub mod logs;

use super::rpc::{Rpc, client_error};
use crate::context::CliContext;
use crate::output::{Color, Icon, Styles, print_error, print_success};
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
    let mut target_run = None;
    let mut failed = false;
    let (outcome, signal) = stream(engine, args, |event| {
        handle_single(styles, event, task_id, &mut target_run, &mut failed);
    })
    .await?;
    Ok(code(outcome.exit_code, signal))
}

pub async fn all(engine: &Rpc, styles: &Styles) -> Result<ExitCode> {
    println!(
        "\n  {} · running all todo tasks\n",
        styles.paint(Color::Amber, "orch")
    );
    let (outcome, signal) = stream(engine, args::DispatchRunAllArgs {}, |_| {}).await?;
    Ok(code(outcome.exit_code, signal))
}

pub async fn once(engine: &Rpc, ctx: &CliContext, styles: &Styles) -> Result<ExitCode> {
    let (outcome, signal) = stream(engine, args::DispatchTickArgs {}, |event| {
        if ctx.output.is_none() && !ctx.quiet {
            print_event(styles, event);
        }
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
    log_startup_diagnostics(engine, &logger).await;
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

/// The daemon's startup diagnostics as `warn` records, one `serve:diagnostic`
/// per warning, before the batch starts any work: the set `run` prints to
/// stderr, in the chosen `--log-format` so they reach the `--log-file` and
/// never break the JSON stream. A failed read is logged and does not stop the
/// batch; dispatch's own preflight decides.
async fn log_startup_diagnostics(engine: &Rpc, logger: &crate::serve::StructuredLogger) {
    let warnings = match engine.request(args::UiBootstrapArgs {}).await {
        Ok(bootstrap) => bootstrap.startup_warnings,
        Err(error) => {
            logger.log(
                crate::serve::LogLevel::Warn,
                "serve:diagnostics_unavailable",
                vec![("error", serde_json::json!(error.to_string()))],
            );
            return;
        }
    };
    if warnings.is_empty() {
        return;
    }
    // Only for the agentId/agentName split; without it each record still
    // carries the whole warning as its message.
    let agents = engine
        .request(args::AgentListArgs {})
        .await
        .unwrap_or_default();
    for warning in &warnings {
        logger.log(
            crate::serve::LogLevel::Warn,
            "serve:diagnostic",
            diagnostic_fields(warning, &agents),
        );
    }
}

/// Split a `ui.bootstrap` startup warning, `"{agent_name} ({agent_id}):
/// {message}"`, back into the 0.13.0 record fields. The agent is matched by
/// its exact prefix, never guessed from the text; the longest match wins.
fn diagnostic_fields(
    warning: &str,
    agents: &[orch_domain::Agent],
) -> Vec<(&'static str, serde_json::Value)> {
    let matched = agents
        .iter()
        .filter_map(|agent| {
            warning
                .strip_prefix(&format!("{} ({}): ", agent.name, agent.id))
                .map(|message| (agent, message))
        })
        .min_by_key(|(_, message)| message.len());
    match matched {
        Some((agent, message)) => vec![
            ("agentId", serde_json::json!(agent.id)),
            ("agentName", serde_json::json!(agent.name)),
            ("message", serde_json::json!(message)),
        ],
        None => vec![("message", serde_json::json!(warning))],
    }
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

fn handle_single(
    styles: &Styles,
    event: &OrchestratorEvent,
    task_id: &str,
    target_run: &mut Option<String>,
    failed: &mut bool,
) -> bool {
    let time = styles.dim(&crate::logs::now_time());
    match event {
        OrchestratorEvent::AgentStarted {
            task_id: started,
            run_id,
            ..
        } => {
            if started == task_id {
                *target_run = Some(run_id.clone());
            }
        }
        // Printed for every run, not just this task's.
        OrchestratorEvent::AgentOutput { data, .. } => {
            println!(
                "  {time}  {} {}",
                styles.icon(Icon::AgentAction),
                slice_80(data)
            );
        }
        OrchestratorEvent::AgentFileChanged { path, .. } => {
            println!(
                "  {time}  {} Modified {path}",
                styles.icon(Icon::AgentAction)
            );
        }
        OrchestratorEvent::AgentError { error, .. } => {
            println!("  {time}  {} {error}", styles.icon(Icon::Failed));
        }
        OrchestratorEvent::AgentCompleted {
            run_id, success, ..
        } => {
            if target_run.as_deref() != Some(run_id.as_str()) {
                return false;
            }
            if *success {
                print_success(styles, "Done");
            } else {
                print_error(styles, "Failed", None);
                *failed = true;
            }
            return true;
        }
        _ => {}
    }
    false
}

fn slice_80(data: &str) -> String {
    slice_utf16(data, 80)
}

fn slice_utf16(data: &str, max_units: usize) -> String {
    if data.len() <= max_units {
        return data.to_owned();
    }
    let mut units = 0;
    let mut out = String::new();
    for ch in data.chars() {
        let width = ch.len_utf16();
        if units + width > max_units {
            break;
        }
        units += width;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_daemon::testing::DaemonHarness;
    use orch_proto::handshake::{Caller, CallerKind, SessionMode};
    use serde_json::Value;

    fn agent(id: &str, name: &str) -> orch_domain::Agent {
        let mut agent = orch_core::test_support::make_agent();
        agent.id = id.into();
        agent.name = name.into();
        agent
    }

    /// Runs `serve --once`'s startup step against an in-process daemon
    /// holding `agents` and returns the JSON records it wrote.
    async fn startup_records(agents: Vec<orch_domain::Agent>) -> Vec<Value> {
        let daemon = DaemonHarness::builder()
            .maintenance_timer(false)
            .start()
            .await
            .expect("harness daemon");
        for agent in &agents {
            daemon
                .engine()
                .agents
                .save(agent)
                .await
                .expect("save agent");
        }
        let endpoint = daemon.connect().await.expect("connect");
        let project = orch_client::Project::discover(daemon.project_root(), None).expect("project");
        let caller = Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        };
        let client = orch_client::Client::from_stream(
            project,
            endpoint.into_stream(),
            caller,
            SessionMode::Normal,
        )
        .await
        .expect("handshake");
        let engine = Rpc::from_client(daemon.project_root().to_path_buf(), client);

        let log = tempfile::tempdir().expect("tempdir");
        let path = log.path().join("serve.log");
        let logger = crate::serve::StructuredLogger::new(
            crate::serve::LogFormat::Json,
            false,
            std::sync::Arc::new(orch_infra::clock::SystemClock),
            crate::serve::open_sinks(Some(&path.to_string_lossy())).expect("sink"),
        );
        log_startup_diagnostics(&engine, &logger).await;
        logger.flush();
        std::fs::read_to_string(&path)
            .expect("read log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every record is JSON"))
            .collect()
    }

    /// W2b B2: `serve --once` logs the 0.13.0 `serve:diagnostic` record per
    /// startup warning, with the agent split back out of the bootstrap line.
    #[tokio::test]
    async fn serve_once_logs_one_diagnostic_record_per_startup_warning() {
        let mut cursor = agent("agt_cur2", "cur2");
        cursor.adapter = "cursor".into();
        cursor.config.effort = Some(orch_domain::ReasoningEffort::High);
        let records = startup_records(vec![cursor, agent("agt_clean", "clean")]).await;

        assert_eq!(records.len(), 1, "one warning, for cur2 only: {records:#?}");
        let record = &records[0];
        assert_eq!(record["event"], "serve:diagnostic");
        assert_eq!(record["level"], "warn");
        assert_eq!(record["agentId"], "agt_cur2");
        assert_eq!(record["agentName"], "cur2");
        let message = record["message"].as_str().expect("message");
        assert!(message.starts_with("legacy-ignored"), "{record}");
    }

    #[tokio::test]
    async fn serve_once_logs_no_diagnostic_when_no_agent_is_affected() {
        let records = startup_records(vec![agent("agt_clean", "clean")]).await;
        assert!(records.is_empty(), "{records:#?}");
    }

    #[test]
    fn a_warning_is_split_only_at_an_exact_agent_prefix() {
        let agents = [agent("agt_a", "a"), agent("agt_b", "a (agt_a): x")];
        let fields = |warning| -> Vec<(&str, Value)> { diagnostic_fields(warning, &agents) };
        assert_eq!(
            fields("a (agt_a): legacy-ignored: m"),
            vec![
                ("agentId", Value::from("agt_a")),
                ("agentName", Value::from("a")),
                ("message", Value::from("legacy-ignored: m")),
            ]
        );
        // The longest (most specific) prefix wins over a shorter one.
        assert_eq!(
            fields("a (agt_a): x (agt_b): m"),
            vec![
                ("agentId", Value::from("agt_b")),
                ("agentName", Value::from("a (agt_a): x")),
                ("message", Value::from("m")),
            ]
        );
        // No known agent: the whole line is the message, nothing is guessed.
        assert_eq!(
            fields("gone (agt_gone): m"),
            vec![("message", Value::from("gone (agt_gone): m"))]
        );
    }
}
