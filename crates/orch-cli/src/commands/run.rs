//! `orch run` — ported from `src/cli/commands/run.ts`
//! (`docs/parity/cli.md` §15).
//!
//! # Why this waits explicitly
//!
//! The original `await`s `runTask`/`runAll`, which return as soon as the
//! dispatch is done, and then simply *returns* — node keeps the process alive
//! because the agent's child process and the reactive-dispatch timer are open
//! handles. The run therefore finishes, its collector writes the task and run
//! records, and `process.exitCode` set from the `agent:completed` handler is
//! what the shell sees.
//!
//! Rust has no such implicit lifetime: returning here would drop the runtime
//! and kill the agent mid-run. So the wait is explicit and reproduces exactly
//! those two handles — the in-flight runs, and (for `--all` only) the 500 ms
//! debounce in which a completion can start the next task. `run <id>` needs no
//! such window: the orchestrator records it as a single-task run and suppresses
//! reactive dispatch.

use std::collections::HashSet;
use std::process::ExitCode;
use std::time::Duration;

use orch_core::error::Result;
use orch_domain::OrchestratorEvent;
use orch_engine::{EventSubscription, FullEngine};

use crate::output::{Color, Icon, Styles, print_error, print_success};

/// The reactive-dispatch debounce (500 ms) plus room for the dispatch itself.
/// Only `--all` waits it out; see the module docs.
const REACTIVE_DISPATCH_WINDOW: Duration = Duration::from_millis(1_500);

/// `run <task-id>` — dispatch one task and stream its events.
pub async fn single(engine: &FullEngine, styles: &Styles, task_id: &str) -> Result<ExitCode> {
    let task = engine.task_service.get(task_id).await?;
    println!();
    println!(
        "  {} · running {task_id} \"{}\"",
        styles.paint(Color::Amber, "orch"),
        task.title
    );

    // Subscribed before the dispatch, so `agent:started` cannot be missed.
    let mut events = engine.events.subscribe();
    let mut target_run: Option<String> = None;
    let mut failed = false;

    engine.orchestrator.run_task(task_id).await?;

    // Everything the dispatch emitted is already buffered, so this decides
    // correctly whether a run started at all — the `if (!targetRunId)` of the
    // original, which is reached for a terminal or non-dispatchable task.
    let mut completed = false;
    for event in events.try_drain() {
        completed |= handle_single(styles, &event, task_id, &mut target_run, &mut failed);
    }

    // The original's closing `console.log()` runs here, right after
    // `await runTask(...)` — *before* the agent's output arrives, since the
    // dispatch returns as soon as the child is spawned. So the blank line
    // sits under the header rather than at the end of the stream.
    println!();

    // `!completed` because the completion can already be in the batch above:
    // without it this waits for a second one that never comes, and `run <id>`
    // hangs after printing its result.
    if target_run.is_some() && !completed {
        while let Some(event) = events.recv().await {
            let completed = handle_single(styles, &event, task_id, &mut target_run, &mut failed);
            if completed {
                break;
            }
        }
    }
    if target_run.is_some() {
        // `agent:completed` is emitted when the *run* is finished, from the
        // middle of the collector — the task's status change, the agent's
        // stats and the workspace cleanup all come after it. Returning here
        // would drop the runtime mid-collector. Outside the branch above
        // because it is owed whenever a run started, including when its
        // completion arrived before the wait.
        engine.orchestrator.wait_for_collectors().await;
    }

    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Print what this event shows and report whether it ended the target run.
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
        // Printed for every run, not just this task's — as in the original.
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

/// `run --all` — dispatch every todo task and wait for the queue to drain.
pub async fn all(engine: &FullEngine, styles: &Styles) -> Result<ExitCode> {
    println!();
    println!(
        "  {} · running all todo tasks",
        styles.paint(Color::Amber, "orch")
    );
    println!();

    let mut events = engine.events.subscribe();
    engine.orchestrator.run_all().await?;
    drain_runs(&mut events).await;
    // The last completion event arrives from inside a collector; this waits
    // for the work that follows it.
    engine.orchestrator.wait_for_collectors().await;
    Ok(ExitCode::SUCCESS)
}

/// Wait until no run is in flight and no reactive dispatch starts another.
///
/// `--all` prints nothing per event — the original registers no listener in
/// this mode — so this only tracks the set of live runs.
async fn drain_runs(events: &mut EventSubscription) {
    let mut in_flight: HashSet<String> = HashSet::new();

    for event in events.try_drain() {
        track(&event, &mut in_flight);
    }

    loop {
        match tokio::time::timeout(REACTIVE_DISPATCH_WINDOW, events.recv()).await {
            Ok(Some(event)) => track(&event, &mut in_flight),
            // The bus outlives this call, so `None` only happens at shutdown.
            Ok(None) => return,
            // Nothing happened for a whole debounce window. With a run still
            // in flight that is just a quiet agent; with none, the queue has
            // drained and node would have exited here too.
            Err(_elapsed) => {
                if in_flight.is_empty() {
                    return;
                }
            }
        }
    }
}

fn track(event: &OrchestratorEvent, in_flight: &mut HashSet<String>) {
    match event {
        OrchestratorEvent::AgentStarted { run_id, .. } => {
            in_flight.insert(run_id.clone());
        }
        OrchestratorEvent::AgentCompleted { run_id, .. } => {
            in_flight.remove(run_id);
        }
        _ => {}
    }
}

/// `run --watch` — the continuous orchestration loop, with a live log stream.
pub async fn watch(engine: &FullEngine, styles: &Styles, verbose: bool) -> Result<ExitCode> {
    println!(
        "{} · watching · poll interval {}s",
        styles.paint(Color::Amber, "orch"),
        // Integer division in the original too: `/ 1000` on a round number.
        engine.config.scheduling.poll_interval_ms / 1000
    );
    println!("{}", "━".repeat(43));
    println!();

    let mut events = engine.events.subscribe();
    let pump = tokio::spawn({
        let styles = *styles;
        async move {
            while let Some(event) = events.recv().await {
                print_watch_event(&styles, &event, verbose);
            }
        }
    });

    engine
        .orchestrator
        .start_watch(orch_engine::WatchOptions::watching())
        .await?;
    engine.orchestrator.wait_for_stop().await;

    pump.abort();
    Ok(ExitCode::SUCCESS)
}

fn print_watch_event(styles: &Styles, event: &OrchestratorEvent, verbose: bool) {
    // Minutes, not seconds: watch mode's clock is coarser than `run <id>`'s.
    let time = styles.dim(&crate::logs::now_time_minutes());
    match event {
        OrchestratorEvent::AgentOutput { data, .. } => {
            if verbose {
                println!(
                    "{time}  {} {}",
                    styles.icon(Icon::AgentAction),
                    slice_60(data)
                );
            }
        }
        OrchestratorEvent::AgentCompleted {
            run_id, success, ..
        } => {
            if *success {
                println!("{time}  {} DONE  {run_id}", styles.icon(Icon::Done));
            } else {
                println!("{time}  {} FAIL  {run_id}", styles.icon(Icon::Failed));
            }
        }
        OrchestratorEvent::RunRetry {
            attempt, delay_ms, ..
        } => {
            println!(
                "{time}  {} RETRY  attempt {attempt} · next in {}s",
                styles.icon(Icon::Retrying),
                // `Math.round`, so 1500 ms is 2s.
                (*delay_ms as f64 / 1000.0).round() as u64
            );
        }
        OrchestratorEvent::OrchestratorTick { running, queued } => {
            // Overwrites itself with `\r`, and keeps the trailing spaces that
            // clear a longer previous line.
            print!(
                "\r{} · watching · {running} running · {queued} queued    ",
                styles.paint(Color::Amber, "orch")
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        OrchestratorEvent::OrchestratorStallDetected { run_id } => {
            println!("{time}  {} STALL  {run_id}", styles.icon(Icon::Warning));
        }
        OrchestratorEvent::OrchestratorShutdown { .. } => {
            println!("\n{}", styles.dim("Shutting down..."));
        }
        _ => {}
    }
}

/// `String.prototype.slice(0, 80)` over UTF-16 units, dropping a character
/// that would straddle the cut.
fn slice_80(data: &str) -> String {
    slice_utf16(data, 80)
}

fn slice_60(data: &str) -> String {
    slice_utf16(data, 60)
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

    fn started(run_id: &str, task_id: &str) -> OrchestratorEvent {
        OrchestratorEvent::AgentStarted {
            agent_id: "agt_1".to_owned(),
            task_id: task_id.to_owned(),
            run_id: run_id.to_owned(),
        }
    }

    fn completed(run_id: &str, success: bool) -> OrchestratorEvent {
        OrchestratorEvent::AgentCompleted {
            run_id: run_id.to_owned(),
            agent_id: "agt_1".to_owned(),
            success,
        }
    }

    #[test]
    fn only_the_targets_completion_ends_the_wait() {
        let styles = Styles::plain();
        let mut target = None;
        let mut failed = false;

        // Another task's run neither becomes the target nor ends the wait.
        handle_single(
            &styles,
            &started("run_other", "tsk_other"),
            "tsk_1",
            &mut target,
            &mut failed,
        );
        assert_eq!(target, None);
        assert!(!handle_single(
            &styles,
            &completed("run_other", true),
            "tsk_1",
            &mut target,
            &mut failed
        ));

        handle_single(
            &styles,
            &started("run_1", "tsk_1"),
            "tsk_1",
            &mut target,
            &mut failed,
        );
        assert_eq!(target.as_deref(), Some("run_1"));
        assert!(handle_single(
            &styles,
            &completed("run_1", false),
            "tsk_1",
            &mut target,
            &mut failed
        ));
        assert!(failed, "a failed run exits 1");
    }

    #[test]
    fn in_flight_tracking_pairs_starts_with_completions() {
        let mut in_flight = HashSet::new();
        track(&started("run_1", "tsk_1"), &mut in_flight);
        track(&started("run_2", "tsk_2"), &mut in_flight);
        assert_eq!(in_flight.len(), 2);
        track(&completed("run_1", true), &mut in_flight);
        assert_eq!(in_flight.len(), 1);
        track(&completed("run_2", false), &mut in_flight);
        assert!(in_flight.is_empty(), "a failed run is finished too");
    }

    #[test]
    fn output_is_cut_at_the_slice_width_in_utf16_units() {
        assert_eq!(slice_80("short"), "short");
        assert_eq!(slice_80(&"x".repeat(100)).len(), 80);
        assert_eq!(slice_60(&"x".repeat(100)).len(), 60);
        // 41 astral characters are 82 units, so 40 whole ones survive.
        assert_eq!(slice_80(&"\u{1f600}".repeat(41)).chars().count(), 40);
    }
}
