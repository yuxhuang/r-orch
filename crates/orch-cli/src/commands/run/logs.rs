//! Socket-backed log reads, retaining the established rendering and limits.
use super::super::rpc::Rpc;
use crate::context::CliContext;
use crate::logs::{LogsArgs, format_event, format_run_error, parse_duration};
use crate::output::Styles;
use orch_core::error::Result;
use orch_domain::{Run, RunEvent};
use orch_proto::ops::args;
use serde_json::json;
use std::process::ExitCode;

struct LogsDeps<'a> {
    runs: RunReads<'a>,
}
struct RunReads<'a>(&'a Rpc);
impl RunReads<'_> {
    async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.0.call::<args::RunGetArgs>(json!({"id":id})).await
    }
    async fn list_all(&self) -> Result<Vec<Run>> {
        self.0
            .request(args::RunListArgs {
                task_id: None,
                agent_id: None,
                since_ms: None,
                limit: None,
            })
            .await
    }
    async fn list_for_task(&self, id: &str) -> Result<Vec<Run>> {
        self.0
            .call::<args::RunListArgs>(json!({"task_id":id}))
            .await
    }
    async fn list_for_agent(&self, id: &str) -> Result<Vec<Run>> {
        self.0
            .call::<args::RunListArgs>(json!({"agent_id":id}))
            .await
    }
    async fn read_events_tail(&self, id: &str, count: usize) -> Result<Vec<RunEvent>> {
        self.0
            .call::<args::RunEventsTailArgs>(json!({"run_id":id,"count":count}))
            .await
    }
}

pub async fn run(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    args: &LogsArgs,
) -> Result<ExitCode> {
    let deps = LogsDeps {
        runs: RunReads(engine),
    };
    // Parsed before the routing decision, so a bad duration fails even in
    // follow mode — where the value is then ignored.
    let since_ms = match &args.since {
        Some(raw) => Some(parse_duration(raw)?),
        None => None,
    };

    if args.follow {
        return follow_live(engine, styles, args).await;
    }
    if let Some(run_id) = &args.run_id {
        return show_run_logs(&deps, ctx, styles, run_id, since_ms).await;
    }
    if let Some(task_id) = &args.task {
        return show_task_logs(&deps, ctx, styles, task_id, since_ms).await;
    }
    if let Some(agent_id) = &args.agent {
        return show_agent_logs(&deps, ctx, styles, agent_id, since_ms).await;
    }
    if let Some(since_ms) = since_ms {
        return show_all_recent_logs(&deps, ctx, styles, since_ms).await;
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
    deps: &LogsDeps<'_>,
    ctx: &CliContext,
    styles: &Styles,
    run_id: &str,
    since_ms: Option<i64>,
) -> Result<ExitCode> {
    let count = if since_ms.is_some() { 500 } else { 50 };
    let events = deps.runs.read_events_tail(run_id, count).await?;
    let events = filter_by_since(events, since_ms);

    if let Some(format) = ctx.output {
        format.print(&events)?;
        return Ok(ExitCode::SUCCESS);
    }

    // The run's terminal error lives on the record, not in its event log:
    // e.g. a remote cancel the worker never acknowledged before shutdown.
    let error = deps
        .runs
        .get(run_id)
        .await?
        .and_then(|run| format_run_error(styles, &run));
    if events.is_empty() && error.is_none() {
        println!("\n  No events for run {run_id}\n");
        return Ok(ExitCode::SUCCESS);
    }

    println!();
    for event in &events {
        println!("{}", format_event(styles, event));
    }
    if let Some(error) = error {
        println!("{error}");
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--task`: every run without `--since`, the last 20 with it. JSON is
/// `Run[]`.
async fn show_task_logs(
    deps: &LogsDeps<'_>,
    ctx: &CliContext,
    styles: &Styles,
    task_id: &str,
    since_ms: Option<i64>,
) -> Result<ExitCode> {
    let mut runs = deps.runs.list_for_task(task_id).await?;
    if since_ms.is_some() {
        runs = last(runs, 20);
    }

    if let Some(format) = ctx.output {
        format.print(&runs)?;
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
        if let Some(error) = format_run_error(styles, run) {
            println!("{error}");
        }
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--agent`: the last 5 runs either way, 5 events each without `--since`.
async fn show_agent_logs(
    deps: &LogsDeps<'_>,
    ctx: &CliContext,
    styles: &Styles,
    agent_id: &str,
    since_ms: Option<i64>,
) -> Result<ExitCode> {
    let runs = last(deps.runs.list_for_agent(agent_id).await?, 5);

    if let Some(format) = ctx.output {
        format.print(&runs)?;
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
        if let Some(error) = format_run_error(styles, run) {
            println!("{error}");
        }
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

/// `--since` with nothing else: every run whose window overlaps, first 20.
async fn show_all_recent_logs(
    deps: &LogsDeps<'_>,
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

    if let Some(format) = ctx.output {
        format.print(&recent)?;
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

async fn follow_live(engine: &Rpc, styles: &Styles, flags: &LogsArgs) -> Result<ExitCode> {
    let args: args::LogsFollowArgs = super::super::rpc::arguments(json!({
        "run_id":flags.run_id,"task_id":flags.task,"agent_id":flags.agent
    }))?;
    println!(
        "\n  {} {}\n",
        styles.dim("Following live events..."),
        styles.dim("(Ctrl+C to stop)")
    );
    super::stream(engine, args, |event| {
        if let Some(line) = crate::logs::format_live_event(styles, event, &crate::logs::now_time())
        {
            println!("{line}");
        }
    })
    .await
    .map(|(_, signal)| signal.map_or(ExitCode::SUCCESS, ExitCode::from))
}
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
