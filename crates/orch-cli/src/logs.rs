//! `orch logs` — ported from `src/cli/commands/logs.ts`
//! (`docs/parity/cli.md` §7).
//!
//! Five routes with different fetch sizes, four different `--json` payloads,
//! and a display slice applied *after* the fetch. The traps, in the order they
//! bite:
//!
//! - `--follow` wins over everything, and `--since` is still **parsed** (and
//!   may fail) in follow mode before being ignored;
//! - only a positional run-id emits `RunEvent[]`; `--task`, `--agent` and
//!   `--since`-only emit **`Run[]`** — run metadata, not events (§22.5);
//! - the fetch sizes differ per route *and* per `--since`, and the slice that
//!   is printed is narrower than the fetch;
//! - no route honours `--quiet`.

use std::process::ExitCode;
use std::sync::Arc;

use orch_core::error::{Error, Result};
use orch_core::storage::RunStore;
use orch_domain::{OrchestratorEvent, Run, RunEvent, RunEventType};
use orch_engine::EventBus;

use crate::context::CliContext;
use crate::output::{Icon, Styles, print_json};

/// What the command needs: runs to read and a bus to follow.
pub struct LogsDeps {
    pub runs: Arc<dyn RunStore>,
    pub events: Arc<EventBus>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogsArgs {
    pub run_id: Option<String>,
    pub task: Option<String>,
    pub agent: Option<String>,
    pub follow: bool,
    pub since: Option<String>,
}

/// `parseDuration` — `^(\d+)(s|m|h|d)$`, in milliseconds.
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

pub async fn run(
    deps: &LogsDeps,
    ctx: &CliContext,
    styles: &Styles,
    args: &LogsArgs,
) -> Result<ExitCode> {
    // Parsed before the routing decision, so a bad duration fails even in
    // follow mode — where the value is then ignored.
    let since_ms = match &args.since {
        Some(raw) => Some(parse_duration(raw)?),
        None => None,
    };

    if args.follow {
        return follow_live(deps, styles, args).await;
    }
    if let Some(run_id) = &args.run_id {
        return show_run_logs(deps, ctx, styles, run_id, since_ms).await;
    }
    if let Some(task_id) = &args.task {
        return show_task_logs(deps, ctx, styles, task_id, since_ms).await;
    }
    if let Some(agent_id) = &args.agent {
        return show_agent_logs(deps, ctx, styles, agent_id, since_ms).await;
    }
    if let Some(since_ms) = since_ms {
        return show_all_recent_logs(deps, ctx, styles, since_ms).await;
    }

    crate::output::print_error(
        styles,
        "Specify a run ID, --task, --agent, or --since <duration>",
        None,
    );
    Ok(ExitCode::from(2))
}

/// Positional run-id: `RunEvent[]` in JSON, 50 events without `--since` and
/// 500-then-filtered with it.
async fn show_run_logs(
    deps: &LogsDeps,
    ctx: &CliContext,
    styles: &Styles,
    run_id: &str,
    since_ms: Option<i64>,
) -> Result<ExitCode> {
    let count = if since_ms.is_some() { 500 } else { 50 };
    let events = deps.runs.read_events_tail(run_id, count).await?;
    let events = filter_by_since(events, since_ms);

    if ctx.json {
        print_json(&events)?;
        return Ok(ExitCode::SUCCESS);
    }

    if events.is_empty() {
        println!("\n  No events for run {run_id}\n");
        return Ok(ExitCode::SUCCESS);
    }

    println!();
    for event in &events {
        println!("{}", format_event(styles, event));
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--task`: every run without `--since`, the last 20 with it. JSON is
/// `Run[]`.
async fn show_task_logs(
    deps: &LogsDeps,
    ctx: &CliContext,
    styles: &Styles,
    task_id: &str,
    since_ms: Option<i64>,
) -> Result<ExitCode> {
    let mut runs = deps.runs.list_for_task(task_id).await?;
    if since_ms.is_some() {
        runs = last(runs, 20);
    }

    if ctx.json {
        print_json(&runs)?;
        return Ok(ExitCode::SUCCESS);
    }

    if runs.is_empty() {
        println!("\n  No runs for task {task_id}\n");
        return Ok(ExitCode::SUCCESS);
    }

    let per_run = if since_ms.is_some() { 500 } else { 10 };
    for run in &runs {
        let events = deps.runs.read_events_tail(&run.id, per_run).await?;
        let events = filter_by_since(events, since_ms);
        println!(
            "\n  Run {} · attempt {} · {}",
            run.id, run.attempt, run.status
        );
        for event in last(events, 10) {
            println!("{}", format_event(styles, &event));
        }
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--agent`: the last 5 runs either way, 5 events each without `--since`.
async fn show_agent_logs(
    deps: &LogsDeps,
    ctx: &CliContext,
    styles: &Styles,
    agent_id: &str,
    since_ms: Option<i64>,
) -> Result<ExitCode> {
    let runs = last(deps.runs.list_for_agent(agent_id).await?, 5);

    if ctx.json {
        print_json(&runs)?;
        return Ok(ExitCode::SUCCESS);
    }

    if runs.is_empty() {
        println!("\n  No runs for agent {agent_id}\n");
        return Ok(ExitCode::SUCCESS);
    }

    let per_run = if since_ms.is_some() { 500 } else { 5 };
    for run in &runs {
        let events = deps.runs.read_events_tail(&run.id, per_run).await?;
        let events = filter_by_since(events, since_ms);
        println!("\n  Run {} · task {} · {}", run.id, run.task_id, run.status);
        for event in last(events, 5) {
            println!("{}", format_event(styles, &event));
        }
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--since` with nothing else: every run whose window overlaps, first 20.
async fn show_all_recent_logs(
    deps: &LogsDeps,
    ctx: &CliContext,
    styles: &Styles,
    since_ms: i64,
) -> Result<ExitCode> {
    let now = now_ms();
    let cutoff = now - since_ms;
    let all = deps.runs.list_all().await?;
    let matching: Vec<Run> = all
        .into_iter()
        .filter(|run| run_in_window(run, cutoff, now))
        .collect();
    let total = matching.len();
    // `slice(0, 20)` — the **first** twenty, unlike every other route.
    let recent: Vec<Run> = matching.into_iter().take(20).collect();

    if ctx.json {
        print_json(&recent)?;
        return Ok(ExitCode::SUCCESS);
    }

    if recent.is_empty() {
        println!("\n  No runs in the specified time window\n");
        return Ok(ExitCode::SUCCESS);
    }

    for run in &recent {
        let events = deps.runs.read_events_tail(&run.id, 500).await?;
        let events = filter_by_since(events, Some(since_ms));
        // A run with no matching events is skipped entirely.
        if events.is_empty() {
            continue;
        }
        println!(
            "\n  Run {} · task {} · agent {} · {}",
            run.id, run.task_id, run.agent_id, run.status
        );
        for event in last(events, 10) {
            println!("{}", format_event(styles, &event));
        }
    }

    if total > 20 {
        println!(
            "\n  {}",
            styles.dim(&format!("(showing 20 of {total} matching runs)"))
        );
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--follow`: render the eight event shapes until SIGINT/SIGTERM.
async fn follow_live(deps: &LogsDeps, styles: &Styles, args: &LogsArgs) -> Result<ExitCode> {
    let mut run_ids: Vec<String> = Vec::new();
    if let Some(run_id) = &args.run_id {
        run_ids.push(run_id.clone());
    }
    if let Some(task_id) = &args.task {
        for run in deps.runs.list_for_task(task_id).await? {
            run_ids.push(run.id);
        }
    }
    let agent_ids: Vec<String> = args.agent.clone().into_iter().collect();
    let has_filter = !run_ids.is_empty() || !agent_ids.is_empty();

    println!(
        "\n  {} {}\n",
        styles.dim("Following live events..."),
        styles.dim("(Ctrl+C to stop)")
    );

    let mut subscription = deps.events.subscribe();
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|err| Error::other(format!("cannot listen for SIGINT: {err}")))?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| Error::other(format!("cannot listen for SIGTERM: {err}")))?;

    loop {
        tokio::select! {
            event = subscription.recv() => {
                let Some(event) = event else { break };
                if has_filter && !matches_filter(&event, &run_ids, &agent_ids) {
                    continue;
                }
                if let Some(line) = format_live_event(styles, &event, &now_time()) {
                    println!("{line}");
                }
            }
            // `process.once` for both signals: the first one ends the stream.
            _ = interrupt.recv() => break,
            _ = terminate.recv() => break,
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// `if ('runId' in event && runIds.size > 0 && !runIds.has(event.runId)) return;`
/// — an event with neither field always passes.
fn matches_filter(event: &OrchestratorEvent, run_ids: &[String], agent_ids: &[String]) -> bool {
    if let Some(run_id) = event_run_id(event)
        && !run_ids.is_empty()
        && !run_ids.iter().any(|id| id == run_id)
    {
        return false;
    }
    if let Some(agent_id) = event_agent_id(event)
        && !agent_ids.is_empty()
        && !agent_ids.iter().any(|id| id == agent_id)
    {
        return false;
    }
    true
}

fn event_run_id(event: &OrchestratorEvent) -> Option<&str> {
    match event {
        OrchestratorEvent::AgentStarted { run_id, .. }
        | OrchestratorEvent::AgentOutput { run_id, .. }
        | OrchestratorEvent::AgentFileChanged { run_id, .. }
        | OrchestratorEvent::AgentCompleted { run_id, .. }
        | OrchestratorEvent::AgentError { run_id, .. }
        | OrchestratorEvent::RunRetry { run_id, .. }
        | OrchestratorEvent::OrchestratorStallDetected { run_id } => Some(run_id),
        _ => None,
    }
}

fn event_agent_id(event: &OrchestratorEvent) -> Option<&str> {
    match event {
        OrchestratorEvent::AgentStarted { agent_id, .. }
        | OrchestratorEvent::AgentOutput { agent_id, .. }
        | OrchestratorEvent::AgentFileChanged { agent_id, .. }
        | OrchestratorEvent::AgentCompleted { agent_id, .. }
        | OrchestratorEvent::AgentError { agent_id, .. }
        | OrchestratorEvent::TaskAssigned { agent_id, .. }
        | OrchestratorEvent::AgentAutonomousToggled { agent_id, .. }
        | OrchestratorEvent::TeamMemberJoined { agent_id, .. }
        | OrchestratorEvent::TeamMemberLeft { agent_id, .. } => Some(agent_id),
        _ => None,
    }
}

/// The seven rendered event shapes; everything else is silently dropped.
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
        _ => return None,
    };
    Some(line)
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

/// `if (!sinceMs) return events;` — **0 also returns unfiltered**.
fn filter_by_since(events: Vec<RunEvent>, since_ms: Option<i64>) -> Vec<RunEvent> {
    let Some(since_ms) = since_ms.filter(|ms| *ms != 0) else {
        return events;
    };
    let cutoff = now_ms() - since_ms;
    events
        .into_iter()
        .filter(|event| parse_iso_ms(&event.timestamp).is_none_or(|ms| ms >= cutoff))
        .collect()
}

/// `finishedAt >= cutoff || startedAt >= cutoff`, where an unfinished run uses
/// `Date.now()` — so it always matches.
fn run_in_window(run: &Run, cutoff: i64, now: i64) -> bool {
    let started_at = parse_iso_ms(&run.started_at).unwrap_or(now);
    let finished_at = run
        .finished_at
        .as_deref()
        .and_then(parse_iso_ms)
        .unwrap_or(now);
    finished_at >= cutoff || started_at >= cutoff
}

/// `arr.slice(-n)`.
fn last<T>(items: Vec<T>, n: usize) -> Vec<T> {
    let skip = items.len().saturating_sub(n);
    items.into_iter().skip(skip).collect()
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

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

/// `new Date(ts).getTime()`, or `None` for an unparseable stamp.
fn parse_iso_ms(timestamp: &str) -> Option<i64> {
    timestamp
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|ts| ts.as_millisecond())
}

/// `toLocaleTimeString('en-US', { hour12: false, hour: '2-digit',
/// minute: '2-digit', second: '2-digit' })` — **local** time, zero-padded.
fn local_time(timestamp: &str) -> String {
    match timestamp.parse::<jiff::Timestamp>() {
        Ok(ts) => format_zoned(ts),
        // An unparseable stamp renders as-is rather than as `Invalid Date`,
        // which is the one place this differs from node; a malformed stamp is
        // a corrupt JSONL line either way.
        Err(_) => timestamp.to_owned(),
    }
}

pub(crate) fn now_time() -> String {
    format_zoned(jiff::Timestamp::now())
}

/// The same clock at minute precision, which is what `run --watch` prints —
/// `toLocaleTimeString` there omits `second`.
pub(crate) fn now_time_minutes() -> String {
    let zoned = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
    format!("{:02}:{:02}", zoned.hour(), zoned.minute())
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
    fn a_bad_duration_is_exit_two_with_the_reference_message() {
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
    fn a_zero_since_returns_events_unfiltered() {
        let events = vec![event(
            "1970-01-01T00:00:00.000Z",
            RunEventType::Done,
            json!(1),
        )];
        assert_eq!(filter_by_since(events.clone(), Some(0)).len(), 1);
        assert_eq!(filter_by_since(events.clone(), None).len(), 1);
        // A real window drops the 1970 event.
        assert_eq!(filter_by_since(events, Some(60_000)).len(), 0);
    }

    #[test]
    fn filtering_keeps_events_inside_the_window() {
        let now = jiff::Timestamp::now();
        let recent = now.to_string();
        let old = (now - std::time::Duration::from_secs(3600)).to_string();
        let events = vec![
            event(&old, RunEventType::AgentOutput, json!("old")),
            event(&recent, RunEventType::AgentOutput, json!("new")),
        ];
        let kept = filter_by_since(events, Some(60_000));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].data, json!("new"));
    }

    #[test]
    fn an_unfinished_run_always_matches_the_window() {
        let now = now_ms();
        let cutoff = now - 60_000;
        // Started long ago and never finished.
        assert!(run_in_window(
            &run("run_1", "2020-01-01T00:00:00.000Z", None),
            cutoff,
            now
        ));
        // Finished long ago: outside.
        assert!(!run_in_window(
            &run(
                "run_2",
                "2020-01-01T00:00:00.000Z",
                Some("2020-01-01T00:01:00.000Z")
            ),
            cutoff,
            now
        ));
    }

    #[test]
    fn slice_from_the_end_matches_the_js_negative_slice() {
        assert_eq!(last(vec![1, 2, 3, 4, 5], 2), vec![4, 5]);
        assert_eq!(last(vec![1, 2], 5), vec![1, 2]);
        assert_eq!(last(Vec::<u8>::new(), 5), Vec::<u8>::new());
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

    #[test]
    fn the_follow_filter_passes_events_without_the_filtered_field() {
        let run_ids = vec!["run_1".to_owned()];
        let agent_ids: Vec<String> = Vec::new();

        let matching = OrchestratorEvent::AgentOutput {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            data: "x".into(),
        };
        assert!(matches_filter(&matching, &run_ids, &agent_ids));

        let other_run = OrchestratorEvent::AgentOutput {
            run_id: "run_2".into(),
            agent_id: "agt_1".into(),
            data: "x".into(),
        };
        assert!(!matches_filter(&other_run, &run_ids, &agent_ids));

        // No `runId` field at all: passes, as `'runId' in event` is false.
        let tick = OrchestratorEvent::OrchestratorTick {
            running: 0,
            queued: 0,
        };
        assert!(matches_filter(&tick, &run_ids, &agent_ids));
    }

    #[test]
    fn the_agent_filter_is_independent_of_the_run_filter() {
        let agent_ids = vec!["agt_1".to_owned()];
        let wrong_agent = OrchestratorEvent::AgentStarted {
            run_id: "run_9".into(),
            agent_id: "agt_2".into(),
            task_id: "tsk_1".into(),
        };
        assert!(!matches_filter(&wrong_agent, &[], &agent_ids));
    }
}
