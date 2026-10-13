//! `orch serve` — headless daemon mode (`docs/parity/cli.md` §16).
//!
//! The three pieces are independent and all three are built: the structured
//! [`logger`], the `--once` terminal rule in [`once`], and the
//! [`disk_observer`] (which `serve` itself never uses — `tui` falls back to it
//! when another process holds the lock).
//!
//! What is **not** here is the orchestrator: [`run_with`] takes one through
//! [`once::ServeOrchestrator`], and [`run`] reports that WP-4 has not wired it
//! yet. The `--log-format` validation and its exit 2 happen before any of
//! that, in the caller, because the reference validates before it builds
//! anything either.

pub mod disk_observer;
pub mod logger;
pub mod once;

use std::process::ExitCode;
use std::sync::Arc;

use orch_core::error::{Error, Result};
use orch_engine::Engine;
use serde_json::Value;

pub use logger::{LogFormat, LogLevel, LogSink, StructuredLogger};
pub use once::{ONCE_POLL_INTERVAL, OnceResult, ServeOrchestrator};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeArgs {
    pub once: bool,
    pub tick_interval: Option<String>,
    pub log_file: Option<String>,
    pub format: LogFormat,
    pub verbose: bool,
}

/// `parseInt(opts.tickInterval, 10)`, applied only when it is a positive
/// number — `--tick-interval abc` is **silently ignored**.
pub fn resolved_poll_interval(raw: Option<&str>, configured: u64) -> u64 {
    raw.and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(configured, |ms| ms as u64)
}

/// Open the sinks: stdout always, plus the `--log-file` in append mode.
pub fn open_sinks(log_file: Option<&str>) -> Result<Vec<LogSink>> {
    let mut sinks = vec![LogSink::Stdout];
    if let Some(path) = log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| Error::io("open the serve log file", path, err))?;
        sinks.push(LogSink::Writer(Box::new(file)));
    }
    Ok(sinks)
}

/// The full command, given an orchestrator.
pub async fn run_with(
    orchestrator: &dyn ServeOrchestrator,
    engine: &Engine,
    args: ServeArgs,
) -> Result<ExitCode> {
    let poll_interval_ms = resolved_poll_interval(
        args.tick_interval.as_deref(),
        engine.config.scheduling.poll_interval_ms,
    );

    let logger = Arc::new(StructuredLogger::new(
        args.format,
        args.verbose,
        Arc::clone(&engine.clock),
        open_sinks(args.log_file.as_deref())?,
    ));

    // Subscribed **before** `startWatch`, so nothing emitted during startup is
    // lost.
    let mut subscription = engine.events.subscribe();
    let pump = tokio::spawn({
        let logger = Arc::clone(&logger);
        async move {
            while let Some(event) = subscription.recv().await {
                logger.handle(&event);
            }
        }
    });

    logger.log(
        logger::LogLevel::Info,
        "serve:started",
        vec![
            (
                "mode",
                Value::String(if args.once { "once" } else { "watch" }.to_owned()),
            ),
            ("pid", Value::from(std::process::id())),
            ("poll_interval_ms", Value::from(poll_interval_ms)),
        ],
    );

    let code = if args.once {
        let result = once::run_once(
            orchestrator,
            engine.tasks.as_ref(),
            &engine.events,
            ONCE_POLL_INTERVAL,
        )
        .await?;
        logger.log(
            logger::LogLevel::Info,
            "serve:finished",
            vec![
                ("result", Value::String(result.as_str().to_owned())),
                ("exit_code", Value::from(result.exit_code())),
            ],
        );
        ExitCode::from(result.exit_code())
    } else {
        orchestrator.start_watch(false).await?;
        orchestrator.wait_for_stop().await;
        ExitCode::SUCCESS
    };

    pump.abort();
    logger.flush();
    Ok(code)
}

/// [`ServeOrchestrator`] over the real orchestrator.
///
/// The only translation is the watch options: `serve` chooses whether to seed
/// autonomous tasks, and always handles signals — it is a daemon, and
/// `SIGTERM` from systemd or pm2 is its normal way to stop.
struct EngineOrchestrator(Arc<orch_engine::Orchestrator>);

#[async_trait::async_trait]
impl ServeOrchestrator for EngineOrchestrator {
    async fn start_watch(&self, skip_autonomous_seeding: bool) -> Result<()> {
        self.0
            .start_watch(orch_engine::WatchOptions {
                skip_autonomous_seeding,
                handle_signals: true,
            })
            .await
    }

    async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }

    async fn wait_for_stop(&self) {
        self.0.wait_for_stop().await;
    }
}

/// The entry point the CLI calls, over the full container.
pub async fn run(engine: orch_engine::FullEngine, args: ServeArgs) -> Result<ExitCode> {
    let orchestrator = EngineOrchestrator(Arc::clone(&engine.orchestrator));
    run_with(&orchestrator, &engine.engine, args).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_positive_tick_interval_overrides_the_configured_one() {
        assert_eq!(resolved_poll_interval(Some("1000"), 10_000), 1_000);
        assert_eq!(resolved_poll_interval(Some(" 250 "), 10_000), 250);
    }

    #[test]
    fn a_junk_or_non_positive_tick_interval_is_silently_ignored() {
        for raw in ["abc", "", "0", "-5", "1.5"] {
            assert_eq!(
                resolved_poll_interval(Some(raw), 10_000),
                10_000,
                "--tick-interval {raw}"
            );
        }
        assert_eq!(resolved_poll_interval(None, 10_000), 10_000);
    }

    #[test]
    fn the_log_file_is_opened_in_append_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("serve.log");
        std::fs::write(&path, "existing\n").expect("seed");

        let sinks = open_sinks(Some(&path.to_string_lossy())).expect("opens");
        assert_eq!(sinks.len(), 2, "stdout plus the file");
        drop(sinks);

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "existing\n",
            "opening must not truncate"
        );
    }

    #[test]
    fn a_log_file_that_cannot_be_opened_is_an_error() {
        let err =
            open_sinks(Some("/orch-definitely-not-writable/serve.log")).expect_err("cannot open");
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn stdout_is_the_only_sink_without_a_log_file() {
        let sinks = open_sinks(None).expect("opens");
        assert_eq!(sinks.len(), 1);
        assert!(matches!(sinks[0], LogSink::Stdout));
    }
}
