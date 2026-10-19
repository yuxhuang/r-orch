//! The `serve` structured logger.
//!
//! Every record is `{ ts, level, event, …fields }` in that order, which is part of the
//! contract in JSON mode. Four things are easy to get wrong:
//!
//! - **`ts` is minted at transform time**, not taken from the event;
//! - **idle ticks are throttled**: one in six, unless `--verbose`;
//! - `agent:output` is dropped entirely unless `--verbose`;
//! - a long list of event types is **never logged at all** (team, goal,
//!   message, `task:assigned`, `agent:autonomous_toggled`), and the
//!   `task:auto_reviewed` payload drops its `results` array.
//!
//! Text mode is `HH:MM:SS.mmm LEVEL event  k=v k=v` — UTC, the level padded
//! to five, and **two** spaces before the fields.

use std::io::Write;
use std::sync::{Arc, Mutex};

use orch_core::clock::Clock;
use orch_domain::OrchestratorEvent;
use serde_json::{Map, Value};

/// One in six idle ticks is logged (`IDLE_LOG_EVERY_N_TICKS`).
pub const IDLE_LOG_EVERY_N_TICKS: u64 = 6;

/// This process's resident set in MiB, to one decimal — the `heap_mb` field of
/// an `orchestrator:tick` record.
///
/// `VmRSS` is reported in kB already, which avoids the page-size arithmetic
/// `/proc/self/statm` would need (pages are not 4 KiB everywhere). On a system
/// without `/proc` the field reads `0.0`, which is also what it did before
/// this was measured at all.
fn resident_mb() -> f64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0.0;
    };
    let kb = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|number| number.parse::<f64>().ok())
        .unwrap_or(0.0);
    // One decimal, rounding half away from zero — as `f64::round` does.
    (kb / 1024.0 * 10.0).round() / 10.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Text,
}

impl LogFormat {
    /// `VALID_LOG_FORMATS` — anything else is exit 2 at the call site.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "json" => Some(LogFormat::Json),
            "text" => Some(LogFormat::Text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// Where records go. `stdout` is never closed by `flush`; a file is.
pub enum LogSink {
    Stdout,
    Writer(Box<dyn Write + Send>),
}

impl std::fmt::Debug for LogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogSink::Stdout => f.write_str("LogSink::Stdout"),
            LogSink::Writer(_) => f.write_str("LogSink::Writer"),
        }
    }
}

impl LogSink {
    fn write_all(&mut self, line: &str) {
        let result = match self {
            LogSink::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(line.as_bytes()).and_then(|()| out.flush())
            }
            LogSink::Writer(writer) => writer
                .write_all(line.as_bytes())
                .and_then(|()| writer.flush()),
        };
        if let Err(err) = result {
            // Write `Log file error: <msg>` to stderr and keep
            // going, because losing the daemon over a full disk is worse.
            eprintln!("Log file error: {err}");
        }
    }
}

pub struct StructuredLogger {
    format: LogFormat,
    verbose: bool,
    idle_log_interval: u64,
    clock: Arc<dyn Clock>,
    sinks: Mutex<Vec<LogSink>>,
    tick_counter: Mutex<u64>,
}

impl StructuredLogger {
    pub fn new(
        format: LogFormat,
        verbose: bool,
        clock: Arc<dyn Clock>,
        sinks: Vec<LogSink>,
    ) -> Self {
        StructuredLogger {
            format,
            verbose,
            idle_log_interval: IDLE_LOG_EVERY_N_TICKS,
            clock,
            sinks: Mutex::new(sinks),
            tick_counter: Mutex::new(0),
        }
    }

    /// Override the idle throttle, which the tests drive at 3.
    pub fn with_idle_interval(mut self, interval: u64) -> Self {
        self.idle_log_interval = interval;
        self
    }

    /// A synthetic record: `serve:started`, `serve:finished`.
    pub fn log(&self, level: LogLevel, event: &str, fields: Vec<(&str, Value)>) {
        let mut record = Map::new();
        record.insert("ts".to_owned(), Value::String(self.clock.now_iso()));
        record.insert("level".to_owned(), Value::String(level.as_str().to_owned()));
        record.insert("event".to_owned(), Value::String(event.to_owned()));
        for (key, value) in fields {
            record.insert(key.to_owned(), value);
        }
        self.write(&record);
    }

    /// Transform a bus event into a record, or `None` when it is not logged.
    pub fn transform(&self, event: &OrchestratorEvent) -> Option<Map<String, Value>> {
        let (level, name, fields) = match event {
            OrchestratorEvent::OrchestratorTick { running, queued } => {
                let mut counter = self.tick_counter.lock().unwrap_or_else(|e| e.into_inner());
                *counter += 1;
                let is_idle = *running == 0 && *queued == 0;
                if !self.verbose && is_idle && !(*counter).is_multiple_of(self.idle_log_interval) {
                    return None;
                }
                (
                    LogLevel::Info,
                    "orchestrator:tick",
                    vec![
                        ("running", Value::from(*running)),
                        ("queued", Value::from(*queued)),
                        // There is no V8 heap here, so this reports the
                        // process's own resident set, in MiB, to one decimal.
                        // The field has to be a real number, because an
                        // operator watching `heap_mb` for a leak is the reason
                        // it exists, and a hard 0.0 would quietly tell them
                        // everything is fine forever.
                        ("heap_mb", Value::from(resident_mb())),
                    ],
                )
            }
            OrchestratorEvent::OrchestratorShutdown { reason } => (
                LogLevel::Info,
                "orchestrator:shutdown",
                vec![("reason", Value::String(reason.clone()))],
            ),
            OrchestratorEvent::OrchestratorError {
                error,
                context,
                fatal,
            } => (
                if *fatal {
                    LogLevel::Error
                } else {
                    LogLevel::Warn
                },
                "orchestrator:error",
                vec![
                    ("error", Value::String(error.clone())),
                    ("context", Value::String(context.clone())),
                    ("fatal", Value::Bool(*fatal)),
                ],
            ),
            OrchestratorEvent::OrchestratorStallDetected { run_id } => (
                LogLevel::Warn,
                "orchestrator:stall_detected",
                vec![("runId", Value::String(run_id.clone()))],
            ),
            OrchestratorEvent::AgentStarted {
                agent_id,
                task_id,
                run_id,
            } => (
                LogLevel::Info,
                "agent:started",
                vec![
                    ("agentId", Value::String(agent_id.clone())),
                    ("taskId", Value::String(task_id.clone())),
                    ("runId", Value::String(run_id.clone())),
                ],
            ),
            OrchestratorEvent::AgentCompleted {
                run_id,
                agent_id,
                success,
            } => (
                if *success {
                    LogLevel::Info
                } else {
                    LogLevel::Warn
                },
                "agent:completed",
                vec![
                    ("runId", Value::String(run_id.clone())),
                    ("agentId", Value::String(agent_id.clone())),
                    ("success", Value::Bool(*success)),
                ],
            ),
            OrchestratorEvent::AgentError {
                run_id,
                agent_id,
                error,
                error_kind,
            } => (
                LogLevel::Error,
                "agent:error",
                vec![
                    ("runId", Value::String(run_id.clone())),
                    ("agentId", Value::String(agent_id.clone())),
                    ("error", Value::String(error.clone())),
                    (
                        "errorKind",
                        error_kind
                            .map(|kind| Value::String(kind.as_str().to_owned()))
                            .unwrap_or(Value::Null),
                    ),
                ],
            ),
            OrchestratorEvent::AgentOutput {
                run_id,
                agent_id,
                data,
            } => {
                // High-frequency: dropped unless --verbose.
                if !self.verbose {
                    return None;
                }
                (
                    LogLevel::Debug,
                    "agent:output",
                    vec![
                        ("runId", Value::String(run_id.clone())),
                        ("agentId", Value::String(agent_id.clone())),
                        ("data", Value::String(slice_200(data))),
                    ],
                )
            }
            OrchestratorEvent::AgentFileChanged {
                run_id,
                agent_id,
                path,
            } => (
                LogLevel::Info,
                "agent:file_changed",
                vec![
                    ("runId", Value::String(run_id.clone())),
                    ("agentId", Value::String(agent_id.clone())),
                    ("path", Value::String(path.clone())),
                ],
            ),
            OrchestratorEvent::RunRetry {
                run_id,
                attempt,
                delay_ms,
            } => (
                LogLevel::Warn,
                "run:retry",
                vec![
                    ("runId", Value::String(run_id.clone())),
                    ("attempt", Value::from(*attempt)),
                    ("delay_ms", Value::from(*delay_ms)),
                ],
            ),
            OrchestratorEvent::TaskCreated { task } => (
                LogLevel::Info,
                "task:created",
                vec![
                    ("taskId", Value::String(task.id.clone())),
                    ("title", Value::String(task.title.clone())),
                ],
            ),
            OrchestratorEvent::TaskStatusChanged { task_id, from, to } => (
                LogLevel::Info,
                "task:status_changed",
                vec![
                    ("taskId", Value::String(task_id.clone())),
                    ("from", Value::String(from.as_str().to_owned())),
                    ("to", Value::String(to.as_str().to_owned())),
                ],
            ),
            // The `results` array is deliberately dropped.
            OrchestratorEvent::TaskAutoReviewed {
                task_id, passed, ..
            } => (
                LogLevel::Info,
                "task:auto_reviewed",
                vec![
                    ("taskId", Value::String(task_id.clone())),
                    ("passed", Value::Bool(*passed)),
                ],
            ),
            OrchestratorEvent::WorkspaceMergeSucceeded { task_id, branch } => (
                LogLevel::Info,
                "workspace:merge_succeeded",
                vec![
                    ("taskId", Value::String(task_id.clone())),
                    ("branch", Value::String(branch.clone())),
                ],
            ),
            OrchestratorEvent::WorkspaceMergeConflict {
                task_id,
                branch,
                conflict_info,
            } => (
                LogLevel::Warn,
                "workspace:merge_conflict",
                vec![
                    ("taskId", Value::String(task_id.clone())),
                    ("branch", Value::String(branch.clone())),
                    ("conflictInfo", Value::String(conflict_info.clone())),
                ],
            ),
            OrchestratorEvent::TaskOrphaned { task_id } => (
                LogLevel::Warn,
                "task:orphaned",
                vec![("taskId", Value::String(task_id.clone()))],
            ),
            OrchestratorEvent::TaskScopeOverlap {
                task_id,
                overlapping_task_id,
                patterns,
            } => (
                LogLevel::Warn,
                "task:scope_overlap",
                vec![
                    ("taskId", Value::String(task_id.clone())),
                    (
                        "overlappingTaskId",
                        Value::String(overlapping_task_id.clone()),
                    ),
                    (
                        "patterns",
                        Value::Array(
                            patterns
                                .iter()
                                .map(|pattern| Value::String(pattern.clone()))
                                .collect(),
                        ),
                    ),
                ],
            ),
            OrchestratorEvent::TaskCascadeFailed {
                task_id,
                failed_dependency_id,
                reason,
            } => (
                LogLevel::Warn,
                "task:cascade_failed",
                vec![
                    ("taskId", Value::String(task_id.clone())),
                    (
                        "failedDependencyId",
                        Value::String(failed_dependency_id.clone()),
                    ),
                    ("reason", Value::String(reason.clone())),
                ],
            ),
            // Everything else hits `default: return null`.
            _ => return None,
        };

        let mut record = Map::new();
        record.insert("ts".to_owned(), Value::String(self.clock.now_iso()));
        record.insert("level".to_owned(), Value::String(level.as_str().to_owned()));
        record.insert("event".to_owned(), Value::String(name.to_owned()));
        for (key, value) in fields {
            record.insert(key.to_owned(), value);
        }
        Some(record)
    }

    /// Transform and write, if the event is logged at all.
    pub fn handle(&self, event: &OrchestratorEvent) {
        if let Some(record) = self.transform(event) {
            self.write(&record);
        }
    }

    fn write(&self, record: &Map<String, Value>) {
        let line = match self.format {
            LogFormat::Json => format!(
                "{}\n",
                serde_json::to_string(record).unwrap_or_else(|_| "{}".to_owned())
            ),
            LogFormat::Text => format_text(record),
        };
        let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        for sink in sinks.iter_mut() {
            sink.write_all(&line);
        }
    }

    /// End every sink that is not stdout, as `flush()` does.
    pub fn flush(&self) {
        let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        sinks.retain(|sink| matches!(sink, LogSink::Stdout));
    }
}

/// `${time} ${level} ${event}  ${fields}` — note the two spaces.
fn format_text(record: &Map<String, Value>) -> String {
    let ts = record.get("ts").and_then(Value::as_str).unwrap_or_default();
    // Characters 11..23 of the timestamp — HH:MM:SS.mmm, still UTC.
    let time: String = ts.chars().skip(11).take(12).collect();
    let level = record
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase();
    let event = record
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let fields: Vec<String> = record
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "ts" | "level" | "event"))
        .map(|(key, value)| match value {
            Value::String(text) => format!("{key}={text}"),
            other => format!("{key}={other}"),
        })
        .collect();

    format!("{time} {level:<5} {event}  {}\n", fields.join(" "))
}

/// `event.data.slice(0, 200)` in UTF-16 code units.
fn slice_200(text: &str) -> String {
    let mut units = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let width = ch.len_utf16();
        if units + width > 200 {
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
    use orch_core::test_support::{FIXED_NOW_ISO, FixedClock};
    use orch_domain::{AdapterErrorKind, TaskStatus};

    /// A sink that keeps what was written, so the records can be asserted
    /// without capturing stdout.
    #[derive(Debug, Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl Recorder {
        fn lines(&self) -> Vec<String> {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn records(&self) -> Vec<Value> {
            self.lines()
                .iter()
                .map(|line| serde_json::from_str(line).expect("a JSON record"))
                .collect()
        }
    }

    impl Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn logger(format: LogFormat, verbose: bool) -> (StructuredLogger, Recorder) {
        let recorder = Recorder::default();
        let logger = StructuredLogger::new(
            format,
            verbose,
            Arc::new(FixedClock::new()),
            vec![LogSink::Writer(Box::new(recorder.clone()))],
        );
        (logger, recorder)
    }

    #[test]
    fn a_synthetic_record_is_ts_level_event_then_its_fields() {
        let (logger, recorder) = logger(LogFormat::Json, false);
        logger.log(
            LogLevel::Info,
            "serve:started",
            vec![
                ("mode", Value::from("once")),
                ("pid", Value::from(4242)),
                ("poll_interval_ms", Value::from(1000)),
            ],
        );
        assert_eq!(
            recorder.lines(),
            [format!(
                "{{\"ts\":\"{FIXED_NOW_ISO}\",\"level\":\"info\",\"event\":\"serve:started\",\
                 \"mode\":\"once\",\"pid\":4242,\"poll_interval_ms\":1000}}"
            )]
        );
    }

    #[test]
    fn text_mode_pads_the_level_and_uses_two_spaces_before_the_fields() {
        let (logger, recorder) = logger(LogFormat::Text, false);
        logger.log(
            LogLevel::Info,
            "serve:finished",
            vec![
                ("result", Value::from("all_done")),
                ("exit_code", Value::from(0)),
            ],
        );
        assert_eq!(
            recorder.lines(),
            ["16:37:33.426 INFO  serve:finished  result=all_done exit_code=0"]
        );
    }

    #[test]
    fn text_mode_json_encodes_a_non_string_field() {
        let (logger, recorder) = logger(LogFormat::Text, false);
        logger.log(
            LogLevel::Warn,
            "task:scope_overlap",
            vec![("patterns", Value::from(vec!["a/**", "b/**"]))],
        );
        assert_eq!(
            recorder.lines(),
            [r#"16:37:33.426 WARN  task:scope_overlap  patterns=["a/**","b/**"]"#]
        );
    }

    // `resident_mb` reads `/proc/self/status`; elsewhere it reports 0.0 (see
    // its doc comment).
    #[test]
    #[cfg(target_os = "linux")]
    fn heap_mb_is_the_resident_set_in_mib_to_one_decimal() {
        let mb = resident_mb();
        // This process is linked against tokio and serde; it is not using a
        // kilobyte, and a hard 0.0 is what this field used to report.
        assert!(mb > 0.5, "implausible RSS: {mb}");
        assert!(mb < 100_000.0, "implausible RSS: {mb}");
        // At most one decimal place survives.
        assert_eq!(mb, (mb * 10.0).round() / 10.0, "more than one decimal");

        // And it reaches the record, rather than only existing as a helper.
        let recorder = Recorder::default();
        let logger = StructuredLogger::new(
            LogFormat::Json,
            false,
            Arc::new(FixedClock::new()),
            vec![LogSink::Writer(Box::new(recorder.clone()))],
        );
        logger.handle(&OrchestratorEvent::OrchestratorTick {
            running: 1,
            queued: 0,
        });
        let line = recorder.lines().join("");
        let record: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
        let logged = record["heap_mb"].as_f64().expect("heap_mb is a number");
        assert!(
            logged > 0.5,
            "the record still carries a placeholder: {line}"
        );
    }

    #[test]
    fn idle_ticks_are_throttled_and_busy_ticks_are_not() {
        let recorder = Recorder::default();
        let logger = StructuredLogger::new(
            LogFormat::Json,
            false,
            Arc::new(FixedClock::new()),
            vec![LogSink::Writer(Box::new(recorder.clone()))],
        )
        .with_idle_interval(3);

        // Six idle ticks produce two records: counters 3 and 6.
        for _ in 0..6 {
            logger.handle(&OrchestratorEvent::OrchestratorTick {
                running: 0,
                queued: 0,
            });
        }
        assert_eq!(recorder.lines().len(), 2);

        // A busy tick is always logged.
        logger.handle(&OrchestratorEvent::OrchestratorTick {
            running: 1,
            queued: 0,
        });
        assert_eq!(recorder.lines().len(), 3);
    }

    #[test]
    fn verbose_logs_every_tick() {
        let (logger, recorder) = logger(LogFormat::Json, true);
        for _ in 0..4 {
            logger.handle(&OrchestratorEvent::OrchestratorTick {
                running: 0,
                queued: 0,
            });
        }
        assert_eq!(recorder.lines().len(), 4);
    }

    #[test]
    fn agent_output_is_dropped_unless_verbose_and_then_truncated() {
        let event = OrchestratorEvent::AgentOutput {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            data: "x".repeat(300),
        };

        let (quiet, quiet_sink) = logger(LogFormat::Json, false);
        quiet.handle(&event);
        assert!(quiet_sink.lines().is_empty());

        let (loud, loud_sink) = logger(LogFormat::Json, true);
        loud.handle(&event);
        let records = loud_sink.records();
        assert_eq!(records[0]["level"], "debug");
        assert_eq!(records[0]["data"].as_str().expect("data").len(), 200);
    }

    #[test]
    fn the_level_depends_on_the_outcome_for_the_three_conditional_events() {
        let (logger, recorder) = logger(LogFormat::Json, false);
        logger.handle(&OrchestratorEvent::AgentCompleted {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            success: true,
        });
        logger.handle(&OrchestratorEvent::AgentCompleted {
            run_id: "run_2".into(),
            agent_id: "agt_1".into(),
            success: false,
        });
        logger.handle(&OrchestratorEvent::OrchestratorError {
            error: "boom".into(),
            context: "tick".into(),
            fatal: false,
        });
        logger.handle(&OrchestratorEvent::OrchestratorError {
            error: "boom".into(),
            context: "tick".into(),
            fatal: true,
        });

        let levels: Vec<String> = recorder
            .records()
            .iter()
            .map(|record| record["level"].as_str().expect("level").to_owned())
            .collect();
        assert_eq!(levels, ["info", "warn", "warn", "error"]);
    }

    #[test]
    fn auto_reviewed_drops_its_results_array() {
        let (logger, recorder) = logger(LogFormat::Json, false);
        logger.handle(&OrchestratorEvent::TaskAutoReviewed {
            task_id: "tsk_1".into(),
            passed: true,
            results: vec![],
        });
        let record = &recorder.records()[0];
        assert_eq!(record["taskId"], "tsk_1");
        assert_eq!(record["passed"], true);
        assert_eq!(record.get("results"), None);
    }

    #[test]
    fn task_created_reports_the_id_and_title_from_the_task() {
        let (logger, recorder) = logger(LogFormat::Json, false);
        let task = orch_core::test_support::make_task();
        let (id, title) = (task.id.clone(), task.title.clone());
        logger.handle(&OrchestratorEvent::TaskCreated {
            task: Box::new(task),
        });
        let record = &recorder.records()[0];
        assert_eq!(record["taskId"], Value::String(id));
        assert_eq!(record["title"], Value::String(title));
    }

    #[test]
    fn status_changes_render_their_statuses_as_strings() {
        let (logger, recorder) = logger(LogFormat::Json, false);
        logger.handle(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        });
        let record = &recorder.records()[0];
        assert_eq!(record["from"], "todo");
        assert_eq!(record["to"], "in_progress");
    }

    #[test]
    fn an_agent_error_carries_its_kind_or_an_explicit_null() {
        let (logger, recorder) = logger(LogFormat::Json, false);
        logger.handle(&OrchestratorEvent::AgentError {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            error: "boom".into(),
            error_kind: Some(AdapterErrorKind::RateLimit),
        });
        logger.handle(&OrchestratorEvent::AgentError {
            run_id: "run_2".into(),
            agent_id: "agt_1".into(),
            error: "boom".into(),
            error_kind: None,
        });
        let records = recorder.records();
        assert_eq!(records[0]["errorKind"], "rate_limit");
        assert_eq!(records[1]["errorKind"], Value::Null);
    }

    #[test]
    fn team_goal_and_message_events_are_never_logged() {
        let (logger, recorder) = logger(LogFormat::Json, true);
        for event in [
            OrchestratorEvent::TaskAssigned {
                task_id: "tsk_1".into(),
                agent_id: "agt_1".into(),
            },
            OrchestratorEvent::MessageSent {
                message_id: "msg_1".into(),
                from_agent_id: "agt_1".into(),
                to_agent_id: None,
                channel: orch_domain::MessageChannel::Broadcast,
            },
            OrchestratorEvent::MessageDelivered {
                message_id: "msg_1".into(),
                to_agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
            },
            OrchestratorEvent::TeamCreated {
                team_id: "tem_1".into(),
                name: "t".into(),
                lead_agent_id: "agt_1".into(),
            },
            OrchestratorEvent::TeamMemberJoined {
                team_id: "tem_1".into(),
                agent_id: "agt_1".into(),
            },
            OrchestratorEvent::TeamMemberLeft {
                team_id: "tem_1".into(),
                agent_id: "agt_1".into(),
            },
            OrchestratorEvent::TeamTaskClaimed {
                team_id: "tem_1".into(),
                task_id: "tsk_1".into(),
                agent_id: "agt_1".into(),
            },
            OrchestratorEvent::TeamDisbanded {
                team_id: "tem_1".into(),
            },
            OrchestratorEvent::TeamTaskAdded {
                team_id: "tem_1".into(),
                task_id: "tsk_1".into(),
            },
            OrchestratorEvent::AgentAutonomousToggled {
                agent_id: "agt_1".into(),
                autonomous: true,
            },
            OrchestratorEvent::GoalCreated {
                goal_id: "gol_1".into(),
                title: "g".into(),
            },
            OrchestratorEvent::GoalStatusChanged {
                goal_id: "gol_1".into(),
                from: orch_domain::GoalStatus::Active,
                to: orch_domain::GoalStatus::Achieved,
            },
            OrchestratorEvent::GoalUpdated {
                goal_id: "gol_1".into(),
            },
            OrchestratorEvent::GoalDeleted {
                goal_id: "gol_1".into(),
            },
        ] {
            logger.handle(&event);
        }
        assert!(
            recorder.lines().is_empty(),
            "logged: {:?}",
            recorder.lines()
        );
    }

    #[test]
    fn every_logged_event_name_equals_its_wire_type() {
        let (logger, recorder) = logger(LogFormat::Json, true);
        let events = vec![
            OrchestratorEvent::OrchestratorTick {
                running: 1,
                queued: 0,
            },
            OrchestratorEvent::OrchestratorShutdown { reason: "r".into() },
            OrchestratorEvent::OrchestratorStallDetected {
                run_id: "run_1".into(),
            },
            OrchestratorEvent::AgentStarted {
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                run_id: "run_1".into(),
            },
            OrchestratorEvent::AgentFileChanged {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                path: "a".into(),
            },
            OrchestratorEvent::RunRetry {
                run_id: "run_1".into(),
                attempt: 1,
                delay_ms: 10,
            },
            OrchestratorEvent::TaskOrphaned {
                task_id: "tsk_1".into(),
            },
            OrchestratorEvent::WorkspaceMergeSucceeded {
                task_id: "tsk_1".into(),
                branch: "b".into(),
            },
            OrchestratorEvent::WorkspaceMergeConflict {
                task_id: "tsk_1".into(),
                branch: "b".into(),
                conflict_info: "c".into(),
            },
            OrchestratorEvent::TaskCascadeFailed {
                task_id: "tsk_1".into(),
                failed_dependency_id: "tsk_0".into(),
                reason: "r".into(),
            },
        ];
        for event in &events {
            logger.handle(event);
        }

        let names: Vec<String> = recorder
            .records()
            .iter()
            .map(|record| record["event"].as_str().expect("event").to_owned())
            .collect();
        let expected: Vec<String> = events
            .iter()
            .map(|event| {
                serde_json::to_value(event).expect("serialize")["type"]
                    .as_str()
                    .expect("type")
                    .to_owned()
            })
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn flush_drops_every_sink_but_stdout() {
        let recorder = Recorder::default();
        let logger = StructuredLogger::new(
            LogFormat::Json,
            false,
            Arc::new(FixedClock::new()),
            vec![LogSink::Stdout, LogSink::Writer(Box::new(recorder.clone()))],
        );
        logger.flush();
        logger.log(LogLevel::Info, "after:flush", vec![]);
        assert!(
            recorder.lines().is_empty(),
            "the file sink was written after flush"
        );
    }

    #[test]
    fn the_format_parser_accepts_exactly_two_values() {
        assert_eq!(LogFormat::parse("json"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Text));
        for bad in ["JSON", "", "yaml", "jsonl"] {
            assert_eq!(LogFormat::parse(bad), None, "{bad}");
        }
    }
}
