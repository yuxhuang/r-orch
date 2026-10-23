//! `orch serve` — headless daemon mode.
//!
//! The three pieces are independent and all three are built: the structured
//! [`logger`], the `--once` terminal rule in [`once`], and the
//! [`disk_observer`] (which `serve` itself never uses — `tui` falls back to it
//! when another process holds the lock).
//!
//! What is **not** here is the orchestrator: [`run_with`] takes one through
//! [`once::ServeOrchestrator`], and [`run`] reports that WP-4 has not wired it
//! yet. The `--log-format` validation and its exit 2 happen before any of
//! that, in the caller, because validation comes before building
//! anything.

pub mod disk_observer;
pub mod logger;
pub mod once;
pub(crate) mod peer;

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

/// The tick interval, applied only when it is a positive
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
    let subscription = engine.events.subscribe();
    // Ended by a token rather than `abort()`, and drained afterwards: the
    // wind-down inside `stop()` emits a run, task and agent event per running
    // entry, plus an `orchestrator:error` record per sub-failure, and aborting
    // the pump discards whatever it has not logged yet. A synchronous
    // logger could not lose a record, and `serve --json`'s record stream has
    // to be complete.
    let done = tokio_util::sync::CancellationToken::new();
    let pump = tokio::spawn({
        let logger = Arc::clone(&logger);
        let done = done.clone();
        async move {
            crate::logs::pump_until_cancelled(subscription, done, |event| logger.handle(event))
                .await;
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

    log_startup_diagnostics(&logger, engine).await;

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

    // Drain, *then* flush: the final records have to reach the sinks before
    // the file is closed.
    done.cancel();
    let _ = pump.await;
    logger.flush();
    Ok(code)
}

/// Log the engine's read-only startup diagnostics as `warn` records, one
/// `serve:diagnostic` per affected agent, before any unattended work: effort
/// requests dispatch will ignore, clamp or cannot confirm, the Codex
/// managed-model coupling, and eco against a custom template — the same set
/// `run --watch` prints to stderr. They are records in the chosen
/// `--log-format`, so they reach the `--log-file` and never break the JSON
/// stream. A store read failure is logged and does not stop the daemon;
/// dispatch's own preflight decides.
async fn log_startup_diagnostics(logger: &StructuredLogger, engine: &Engine) {
    let agents = match engine.agent_service.list().await {
        Ok(agents) => agents,
        Err(err) => {
            logger.log(
                LogLevel::Warn,
                "serve:diagnostics_unavailable",
                vec![("error", Value::String(err.to_string()))],
            );
            return;
        }
    };
    for diagnostic in orch_engine::prompt_mode::startup_diagnostics(&engine.config, &agents) {
        logger.log(
            LogLevel::Warn,
            "serve:diagnostic",
            vec![
                ("agent_id", Value::String(diagnostic.agent_id)),
                ("agent_name", Value::String(diagnostic.agent_name)),
                ("message", Value::String(diagnostic.message)),
            ],
        );
    }
}

/// [`ServeOrchestrator`] over the real orchestrator.
///
/// The only translation is the watch options: `serve` chooses whether to seed
/// autonomous tasks, and always handles signals — it is a daemon, and
/// `SIGTERM` from systemd or pm2 is its normal way to stop.
struct EngineOrchestrator {
    engine: Arc<orch_engine::FullEngine>,
    peer_runtime: tokio::sync::Mutex<Option<orch_peer::PeerRuntime>>,
}

#[async_trait::async_trait]
impl ServeOrchestrator for EngineOrchestrator {
    async fn start_watch(&self, skip_autonomous_seeding: bool) -> Result<()> {
        let runtime = peer::start_watch(
            Arc::clone(&self.engine),
            orch_engine::WatchOptions {
                skip_autonomous_seeding,
                handle_signals: true,
            },
        )
        .await?;
        *self.peer_runtime.lock().await = Some(runtime);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let result = self.engine.orchestrator.stop().await;
        self.peer_runtime.lock().await.take();
        result
    }

    async fn wait_for_stop(&self) {
        self.engine.orchestrator.wait_for_stop().await;
        self.peer_runtime.lock().await.take();
    }
}

/// The entry point the CLI calls, over the full container.
pub async fn run(engine: orch_engine::FullEngine, args: ServeArgs) -> Result<ExitCode> {
    let engine = Arc::new(engine);
    let orchestrator = EngineOrchestrator {
        engine: Arc::clone(&engine),
        peer_runtime: tokio::sync::Mutex::new(None),
    };
    run_with(&orchestrator, &engine.engine, args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::OrchestratorEvent;
    use std::sync::Mutex;

    /// Emits the shutdown burst from `wait_for_stop`, the way the real
    /// orchestrator emits it from inside `stop()`.
    struct BurstingOrchestrator {
        events: Arc<orch_engine::EventBus>,
        stops: Mutex<usize>,
    }

    #[async_trait::async_trait]
    impl ServeOrchestrator for BurstingOrchestrator {
        async fn start_watch(&self, _skip_autonomous_seeding: bool) -> Result<()> {
            Ok(())
        }
        async fn stop(&self) -> Result<()> {
            *self.stops.lock().unwrap() += 1;
            Ok(())
        }
        async fn wait_for_stop(&self) {
            // Synchronous and with no await afterwards, so the pump has had no
            // chance to run when `run_with` reaches its shutdown: exactly the
            // window `pump.abort()` used to discard.
            for n in 0..5 {
                self.events.emit(OrchestratorEvent::OrchestratorError {
                    error: format!("wind-down {n}"),
                    context: "stop".to_owned(),
                    fatal: false,
                });
            }
            self.events.emit(OrchestratorEvent::OrchestratorShutdown {
                reason: "Received SIGTERM".to_owned(),
            });
        }
    }

    #[tokio::test]
    async fn the_shutdown_burst_reaches_the_log_before_the_sinks_close() {
        let temp = tempfile::tempdir().expect("tempdir");
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        std::fs::create_dir_all(orch_infra::storage::Paths::new(temp.path()).root())
            .expect("create .orchestry");
        let engine = orch_engine::build_light_with(
            temp.path(),
            orch_engine::EngineSeams {
                clock: Arc::new(orch_core::test_support::FixedClock::new()),
                ids: Arc::new(orch_core::test_support::SeqIdGenerator::new()),
                global_config_path: None,
            },
        )
        .await
        .expect("build_light");

        let log_path = temp.path().join("serve.log");
        let orchestrator = BurstingOrchestrator {
            events: Arc::clone(&engine.events),
            stops: Mutex::new(0),
        };

        run_with(
            &orchestrator,
            &engine,
            ServeArgs {
                once: false,
                tick_interval: None,
                log_file: Some(log_path.to_string_lossy().into_owned()),
                format: LogFormat::Json,
                verbose: false,
            },
        )
        .await
        .expect("serve");

        let written = std::fs::read_to_string(&log_path).expect("read log");
        // A synchronous logger could not lose any of these;
        // aborting the pump instead of draining it loses all six.
        for n in 0..5 {
            assert!(
                written.contains(&format!("wind-down {n}")),
                "lost wind-down record {n}:\n{written}"
            );
        }
        assert!(
            written.contains("Received SIGTERM"),
            "lost the shutdown record:\n{written}"
        );
    }

    /// Records, at `start_watch`, what the log file already held: the
    /// diagnostics have to be written before any unattended work begins.
    struct WatchProbe {
        log_path: std::path::PathBuf,
        log_at_start: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl ServeOrchestrator for WatchProbe {
        async fn start_watch(&self, _skip_autonomous_seeding: bool) -> Result<()> {
            let seen = std::fs::read_to_string(&self.log_path).unwrap_or_default();
            *self.log_at_start.lock().unwrap() = Some(seen);
            Ok(())
        }
        async fn stop(&self) -> Result<()> {
            Ok(())
        }
        async fn wait_for_stop(&self) {}
    }

    async fn serve_with_agents(agents: Vec<orch_domain::Agent>) -> (Vec<Value>, Vec<Value>) {
        let temp = tempfile::tempdir().expect("tempdir");
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        std::fs::create_dir_all(orch_infra::storage::Paths::new(temp.path()).root())
            .expect("create .orchestry");
        let engine = orch_engine::build_light_with(
            temp.path(),
            orch_engine::EngineSeams {
                clock: Arc::new(orch_core::test_support::FixedClock::new()),
                ids: Arc::new(orch_core::test_support::SeqIdGenerator::new()),
                global_config_path: None,
            },
        )
        .await
        .expect("build_light");
        for agent in &agents {
            engine.agents.save(agent).await.expect("save agent");
        }

        let log_path = temp.path().join("serve.log");
        let orchestrator = WatchProbe {
            log_path: log_path.clone(),
            log_at_start: Mutex::new(None),
        };
        run_with(
            &orchestrator,
            &engine,
            ServeArgs {
                once: false,
                tick_interval: None,
                log_file: Some(log_path.to_string_lossy().into_owned()),
                format: LogFormat::Json,
                verbose: false,
            },
        )
        .await
        .expect("serve");

        let parse = |text: &str| -> Vec<Value> {
            text.lines()
                .map(|line| serde_json::from_str(line).expect("every record is JSON"))
                .collect()
        };
        let at_start = orchestrator
            .log_at_start
            .lock()
            .unwrap()
            .clone()
            .expect("start_watch was called");
        let written = std::fs::read_to_string(&log_path).expect("read log");
        (parse(&at_start), parse(&written))
    }

    fn diagnostics(records: &[Value]) -> Vec<&Value> {
        records
            .iter()
            .filter(|record| record["event"] == "serve:diagnostic")
            .collect()
    }

    #[tokio::test]
    async fn watch_startup_logs_one_warning_per_affected_agent_before_unattended_work() {
        let mut cursor = orch_core::test_support::make_agent();
        cursor.id = "agt_cur2".into();
        cursor.name = "cur2".into();
        cursor.adapter = "cursor".into();
        cursor.config.effort = Some(orch_domain::ReasoningEffort::High);
        let mut clean = orch_core::test_support::make_agent();
        clean.id = "agt_clean".into();
        clean.name = "clean".into();

        let (at_start, written) = serve_with_agents(vec![cursor, clean]).await;

        let found = diagnostics(&written);
        assert_eq!(found.len(), 1, "one warning, for cur2 only: {written:#?}");
        let record = found[0];
        assert_eq!(record["level"], "warn");
        assert_eq!(record["agent_id"], "agt_cur2");
        assert_eq!(record["agent_name"], "cur2");
        assert!(
            record["message"]
                .as_str()
                .is_some_and(|m| m.contains("legacy-ignored")),
            "names the disposition: {record}"
        );
        assert_eq!(
            diagnostics(&at_start).len(),
            1,
            "the warning is logged before start_watch: {at_start:#?}"
        );
        let started = written
            .iter()
            .position(|r| r["event"] == "serve:started")
            .expect("serve:started");
        let warned = written
            .iter()
            .position(|r| r["event"] == "serve:diagnostic")
            .expect("serve:diagnostic");
        assert!(started < warned, "serve:started stays the first record");
    }

    #[tokio::test]
    async fn watch_startup_logs_no_diagnostic_when_no_agent_is_affected() {
        let (_, written) = serve_with_agents(vec![orch_core::test_support::make_agent()]).await;
        assert!(diagnostics(&written).is_empty(), "{written:#?}");
    }

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
