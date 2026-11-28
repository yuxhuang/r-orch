//! The CLI passes typed offline inputs, signals cancellation and renders output.
use clap::Args;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Args)]
pub struct ImportCmd {
    #[arg(long, value_name = "PROJECT")]
    pub from: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub resume: bool,
    #[arg(long, value_name = "RUN_ID")]
    pub assume_stopped: Vec<String>,
}

pub async fn run(cli: &crate::tree::Cli, args: &ImportCmd, cwd: &Path) -> ExitCode {
    let home = std::env::var_os("ORCH_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".orchestry/daemons")));
    let Some(data_home) = home else {
        eprintln!("import needs HOME or ORCH_DATA_HOME");
        return ExitCode::from(2);
    };
    if cli.agent.is_some() {
        eprintln!("import is owner-only");
        return ExitCode::from(2);
    }
    let request = orch_daemon::ImportRequest {
        source: args.from.clone().unwrap_or_else(|| cwd.to_owned()),
        data_home,
        data_dir: std::env::var_os("ORCH_DATA_DIR").map(PathBuf::from),
        tmp_dir: std::env::temp_dir(),
        socket: std::env::var_os("ORCH_SOCKET").map(PathBuf::from),
        dry_run: args.dry_run,
        resume: args.resume,
        assume_stopped: args.assume_stopped.clone(),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let operation = orch_daemon::import_offline(request, cancel.clone());
    tokio::pin!(operation);
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(_) => {
                eprintln!("cannot install import termination handler");
                return ExitCode::from(2);
            }
        };
    let outcome: orch_daemon::ImportOutcome = tokio::select! {
        answer = &mut operation => answer,
        _ = tokio::signal::ctrl_c() => { cancel.store(true, Ordering::Relaxed); operation.await },
        _ = terminate.recv() => { cancel.store(true, Ordering::Relaxed); operation.await },
    };
    let report = serde_json::from_str::<serde_json::Value>(&outcome.report_json)
        .unwrap_or(serde_json::Value::Null);
    let rendered = serde_json::json!({"preflight": report, "imported": outcome.imported,
        "unfinished": outcome.unfinished, "procedural_exclusion": outcome.procedural_exclusion,
        "detail": outcome.detail, "exit_code": outcome.exit_code});
    if cli.json {
        println!("{rendered}");
    } else {
        // Source paths and names are untrusted terminal text.
        println!("{}", outcome.detail.escape_debug());
        println!("{rendered}");
    }
    ExitCode::from(outcome.exit_code)
}
