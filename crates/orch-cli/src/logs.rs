//! `orch logs`.
//!
//! Five routes with different fetch sizes, four different `--json` payloads,
//! and a display slice applied *after* the fetch. The traps, in the order they
//! bite:
//!
//! - `--follow` wins over everything, and `--since` is still **parsed** (and
//!   may fail) in follow mode before being ignored;
//! - only a positional run-id emits `RunEvent[]`; `--task`, `--agent` and
//!   `--since`-only emit **`Run[]`** — run metadata, not events;
//! - the fetch sizes differ per route *and* per `--since`, and the slice that
//!   is printed is narrower than the fetch;
//! - no route honours `--quiet`.

use orch_core::error::{Error, Result};
use orch_domain::{OrchestratorEvent, Run, RunEvent, RunEventType};

use crate::output::{Icon, Styles};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogsArgs {
    pub run_id: Option<String>,
    pub task: Option<String>,
    pub agent: Option<String>,
    pub follow: bool,
    pub since: Option<String>,
}

/// `^(\d+)(s|m|h|d)$`, in milliseconds.
pub fn parse_duration(input: &str) -> Result<i64> {
    let invalid = || {
        Error::invalid_arguments(format!(
            "Invalid duration: \"{input}\". Use format: 5m, 1h, 30s, 1d"
        ))
    };

    let (digits, unit) = input.split_at(input.len().saturating_sub(1));
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let value: i64 = digits.parse().map_err(|_| invalid())?;
    match unit {
        "s" => Ok(value * 1_000),
        "m" => Ok(value * 60_000),
        "h" => Ok(value * 3_600_000),
        "d" => Ok(value * 86_400_000),
        _ => Err(invalid()),
    }
}

/// The eight rendered event shapes; everything else is silently dropped.
pub fn format_live_event(styles: &Styles, event: &OrchestratorEvent, time: &str) -> Option<String> {
    let dim_time = styles.dim(time);
    let action = styles.icon(Icon::AgentAction);
    let failed = styles.icon(Icon::Failed);
    let done = styles.icon(Icon::Done);
    let line = match event {
        OrchestratorEvent::AgentOutput { data, .. } => {
            format!("  {dim_time}  {action} {}", slice_80(data))
        }
        OrchestratorEvent::AgentFileChanged { path, .. } => {
            format!("  {dim_time}  {action} Modified {path}")
        }
        OrchestratorEvent::AgentError { error, .. } => {
            format!("  {dim_time}  {failed} {error}")
        }
        OrchestratorEvent::AgentStarted {
            run_id, agent_id, ..
        } => format!(
            "  {dim_time}  {} Started {run_id} (agent: {agent_id})",
            styles.icon(Icon::OrchestratorEvent)
        ),
        OrchestratorEvent::AgentCompleted {
            run_id, success, ..
        } => {
            if *success {
                format!("  {dim_time}  {done} DONE  {run_id}")
            } else {
                format!("  {dim_time}  {failed} FAIL  {run_id}")
            }
        }
        OrchestratorEvent::RunRetry {
            attempt, delay_ms, ..
        } => format!(
            "  {dim_time}  {} RETRY  attempt {attempt} · next in {}s",
            styles.icon(Icon::Retrying),
            // `Math.round(delay_ms / 1000)`.
            (*delay_ms as f64 / 1000.0).round() as u64
        ),
        OrchestratorEvent::OrchestratorStallDetected { run_id } => format!(
            "  {dim_time}  {} STALL  {run_id}",
            styles.icon(Icon::Warning)
        ),
        OrchestratorEvent::OrchestratorError { error, context, .. } => {
            format_orchestrator_error(styles, &dim_time, error, context)
        }
        _ => return None,
    };
    Some(line)
}

/// `  {time}  ⚠ {context}: {error}`, untruncated: the diagnostic is the
/// point (e.g. which peer and execution a shutdown could not confirm).
pub(crate) fn format_orchestrator_error(
    styles: &Styles,
    time: &str,
    error: &str,
    context: &str,
) -> String {
    format!(
        "  {time}  {} {context}: {error}",
        styles.icon(Icon::Warning)
    )
}

/// A run's persisted terminal error, untruncated, stamped with its finish
/// time. `None` for a run without one.
pub fn format_run_error(styles: &Styles, run: &Run) -> Option<String> {
    let error = run.error.as_deref()?;
    let icon = if run.status == orch_domain::RunStatus::Failed {
        Icon::Failed
    } else {
        Icon::Warning
    };
    let at = run.finished_at.as_deref().unwrap_or(&run.started_at);
    Some(format!(
        "  {}  {} {error}",
        styles.dim(&local_time(at)),
        styles.icon(icon)
    ))
}

/// `  {dim(time)}  {icon} {data.slice(0, 80)}`, with the error icon for an
/// `error` event.
pub fn format_event(styles: &Styles, event: &RunEvent) -> String {
    let icon = if event.event_type == RunEventType::Error {
        styles.icon(Icon::Failed)
    } else {
        styles.icon(Icon::AgentAction)
    };
    let data = match &event.data {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    format!(
        "  {}  {icon} {}",
        styles.dim(&local_time(&event.timestamp)),
        slice_80(&data)
    )
}

/// `String.prototype.slice(0, 80)` — UTF-16 code units.
///
/// A character straddling the boundary is dropped whole rather than split into
/// a lone surrogate, which a Rust `String` cannot hold.
fn slice_80(text: &str) -> String {
    let mut units = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let width = ch.len_utf16();
        if units + width > 80 {
            break;
        }
        units += width;
        out.push(ch);
    }
    out
}

/// `toLocaleTimeString('en-US', { hour12: false, hour: '2-digit',
/// minute: '2-digit', second: '2-digit' })` — **local** time, zero-padded.
fn local_time(timestamp: &str) -> String {
    match timestamp.parse::<jiff::Timestamp>() {
        Ok(ts) => format_zoned(ts),
        // An unparseable stamp renders as-is rather than as `Invalid Date`,
        // which differs from JavaScript's `Invalid Date`; a malformed stamp is a
        // corrupt JSONL line either way.
        Err(_) => timestamp.to_owned(),
    }
}

pub(crate) fn now_time() -> String {
    format_zoned(jiff::Timestamp::now())
}

fn format_zoned(ts: jiff::Timestamp) -> String {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    format!(
        "{:02}:{:02}:{:02}",
        zoned.hour(),
        zoned.minute(),
        zoned.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::RunStatus;
    use serde_json::json;

    fn event(timestamp: &str, event_type: RunEventType, data: serde_json::Value) -> RunEvent {
        RunEvent {
            timestamp: timestamp.to_owned(),
            event_type,
            data,
        }
    }

    fn run(id: &str, started_at: &str, finished_at: Option<&str>) -> Run {
        Run {
            id: id.to_owned(),
            task_id: "tsk_1".to_owned(),
            agent_id: "agt_1".to_owned(),
            attempt: 1,
            status: RunStatus::Succeeded,
            started_at: started_at.to_owned(),
            finished_at: finished_at.map(str::to_owned),
            workspace_path: "/ws".to_owned(),
            prompt: "p".to_owned(),
            pid: None,
            error: None,
            error_kind: None,
            tokens: None,
        }
    }

    #[test]
    fn durations_parse_in_the_four_documented_units() {
        assert_eq!(parse_duration("30s").unwrap(), 30_000);
        assert_eq!(parse_duration("5m").unwrap(), 300_000);
        assert_eq!(parse_duration("1h").unwrap(), 3_600_000);
        assert_eq!(parse_duration("1d").unwrap(), 86_400_000);
        assert_eq!(parse_duration("0m").unwrap(), 0);
    }

    #[test]
    fn a_bad_duration_is_exit_two_with_the_fixed_message() {
        for bad in ["", "m", "5", "5x", "-5m", "5 m", "1.5h", "5M"] {
            let err = parse_duration(bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("Invalid duration: \"{bad}\". Use format: 5m, 1h, 30s, 1d")
            );
            assert_eq!(err.exit_code(), 2, "{bad}");
        }
    }

    #[test]
    fn an_event_line_is_time_icon_and_eighty_characters_of_data() {
        let styles = Styles::plain();
        let line = format_event(
            &styles,
            &event(
                "2026-10-13T16:03:45.086Z",
                RunEventType::AgentOutput,
                json!("done"),
            ),
        );
        // The time is local, so assert the shape and the tail.
        assert!(line.ends_with("  ▸ done"), "{line}");
        assert_eq!(line.len() - "  ▸ done".len(), 10, "two spaces + HH:MM:SS");

        let error = format_event(
            &styles,
            &event(
                "2026-10-13T16:03:45.086Z",
                RunEventType::Error,
                json!("boom"),
            ),
        );
        assert!(error.ends_with("  ✕ boom"), "{error}");
    }

    #[test]
    fn non_string_event_data_is_json_encoded() {
        let line = format_event(
            &Styles::plain(),
            &event(
                "2026-10-13T16:03:45.086Z",
                RunEventType::AgentOutput,
                json!({ "text": "hi" }),
            ),
        );
        assert!(line.ends_with(r#"  ▸ {"text":"hi"}"#), "{line}");
    }

    #[test]
    fn event_data_is_truncated_at_eighty_utf16_units() {
        assert_eq!(slice_80(&"x".repeat(100)).len(), 80);
        assert_eq!(slice_80("short"), "short");
        // 40 astral characters are exactly 80 units.
        let astral = "\u{1f600}".repeat(41);
        assert_eq!(slice_80(&astral).chars().count(), 40);
    }

    #[test]
    fn a_run_error_is_rendered_untruncated_and_absent_without_one() {
        let styles = Styles::plain();
        let mut cancelled = run("run_1", "2020-01-01T00:00:00.000Z", None);
        assert_eq!(format_run_error(&styles, &cancelled), None);

        let diagnostic = format!("Remote cancel outcome unknown: {}", "x".repeat(200));
        cancelled.status = RunStatus::Cancelled;
        cancelled.error = Some(diagnostic.clone());
        let line = format_run_error(&styles, &cancelled).expect("error line");
        assert!(line.ends_with(&format!("⚠ {diagnostic}")), "{line}");

        cancelled.status = RunStatus::Failed;
        let line = format_run_error(&styles, &cancelled).expect("error line");
        assert!(line.ends_with(&format!("✕ {diagnostic}")), "{line}");
    }

    #[test]
    fn the_live_renderer_covers_the_documented_shapes_and_drops_the_rest() {
        let styles = Styles::plain();
        let time = "12:03:45";
        let cases: Vec<(OrchestratorEvent, &str)> = vec![
            (
                OrchestratorEvent::AgentOutput {
                    run_id: "run_1".into(),
                    agent_id: "agt_1".into(),
                    data: "hello".into(),
                },
                "  12:03:45  ▸ hello",
            ),
            (
                OrchestratorEvent::AgentFileChanged {
                    run_id: "run_1".into(),
                    agent_id: "agt_1".into(),
                    path: "src/a.ts".into(),
                },
                "  12:03:45  ▸ Modified src/a.ts",
            ),
            (
                OrchestratorEvent::AgentError {
                    run_id: "run_1".into(),
                    agent_id: "agt_1".into(),
                    error: "boom".into(),
                    error_kind: None,
                },
                "  12:03:45  ✕ boom",
            ),
            (
                OrchestratorEvent::AgentStarted {
                    run_id: "run_1".into(),
                    agent_id: "agt_1".into(),
                    task_id: "tsk_1".into(),
                },
                "  12:03:45  → Started run_1 (agent: agt_1)",
            ),
            (
                OrchestratorEvent::AgentCompleted {
                    run_id: "run_1".into(),
                    agent_id: "agt_1".into(),
                    success: true,
                },
                "  12:03:45  ✓ DONE  run_1",
            ),
            (
                OrchestratorEvent::AgentCompleted {
                    run_id: "run_1".into(),
                    agent_id: "agt_1".into(),
                    success: false,
                },
                "  12:03:45  ✕ FAIL  run_1",
            ),
            (
                OrchestratorEvent::RunRetry {
                    run_id: "run_1".into(),
                    attempt: 2,
                    delay_ms: 10_500,
                },
                "  12:03:45  ↻ RETRY  attempt 2 · next in 11s",
            ),
            (
                OrchestratorEvent::OrchestratorStallDetected {
                    run_id: "run_1".into(),
                },
                "  12:03:45  ⚠ STALL  run_1",
            ),
            (
                OrchestratorEvent::OrchestratorError {
                    error: "Remote cancel outcome unknown".into(),
                    context: "stop: cancel remote w1:run_1".into(),
                    fatal: false,
                },
                "  12:03:45  ⚠ stop: cancel remote w1:run_1: Remote cancel outcome unknown",
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(
                format_live_event(&styles, &event, time).as_deref(),
                Some(expected)
            );
        }

        // Everything else is dropped.
        for dropped in [
            OrchestratorEvent::OrchestratorTick {
                running: 1,
                queued: 0,
            },
            OrchestratorEvent::GoalCreated {
                goal_id: "gol_1".into(),
                title: "t".into(),
            },
            OrchestratorEvent::TaskOrphaned {
                task_id: "tsk_1".into(),
            },
        ] {
            assert_eq!(format_live_event(&styles, &dropped, time), None);
        }
    }
}
