//! `orch run`.
//!
//! # Why this waits explicitly
//!
//! The dispatch calls return as soon as the dispatch is done, while the
//! agent's child process and the reactive-dispatch timer keep working. The run
//! has to finish, its collector has to write the task and run records, and
//! the exit code set from the `agent:completed` handler is what the shell
//! sees.
//!
//! Rust has no such implicit lifetime: returning here would drop the runtime
//! and kill the agent mid-run. So the wait is explicit and reproduces exactly
//! those two handles — the in-flight runs, and (for `--all` only) the 500 ms
//! debounce in which a completion can start the next task. `run <id>` needs no
//! such window: the orchestrator records it as a single-task run and suppresses
//! reactive dispatch.
//!
//! # Why `run <id>` and `run --all` catch SIGINT/SIGTERM
//!
//! Watch, serve and the TUI install termination handlers; these two did not,
//! so a signal killed orch outright. During a dispatch that orphaned the
//! OpenCode effort probe's process group (gate-resilience F1): its deadline
//! and kill guard live in this runtime, and the probe pid is deliberately not
//! in the model-pid slot startup recovery reads. Both paths now latch the
//! signal ([`StandaloneSignals`]), let the bounded dispatch critical section
//! finish — the orchestrator cancels an in-flight preparation, which kills and
//! reaps its group, and starts no model child — then stop this process's runs
//! and settle their records before exiting `128 + signal`.

use std::collections::HashSet;
use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use orch_core::error::Result;
use orch_domain::OrchestratorEvent;
use orch_engine::{EventSubscription, FullEngine, Orchestrator};

use crate::output::{Color, Icon, Styles, print_error, print_success};

/// The reactive-dispatch debounce (500 ms) plus room for the dispatch itself.
/// Only `--all` waits it out; see the module docs.
const REACTIVE_DISPATCH_WINDOW: Duration = Duration::from_millis(1_500);

/// How long `run <id>` sits on a silent bus before asking the run record
/// whether it missed the completion.
///
/// Deliberately far longer than any agent's gap between events: the check is a
/// backstop for a *lost* completion, not a poll loop, and ending the wait
/// early would truncate the stream.
const LOST_COMPLETION_POLL: Duration = Duration::from_secs(60);

/// Print the engine's read-only startup diagnostics to **stderr**: one line
/// per affected agent for effort requests dispatch will ignore, clamp or
/// cannot confirm, the Codex managed-model coupling, and agents that would
/// render eco against a custom template. Nothing is spawned or written; a
/// store read failure is itself reported and does not stop the run, whose own
/// preflight decides.
pub async fn report_startup_diagnostics(engine: &FullEngine, styles: &Styles) {
    let agents = match engine.agent_service.list().await {
        Ok(agents) => agents,
        Err(err) => {
            print_error(
                styles,
                &format!("Could not check agent diagnostics: {err}"),
                None,
            );
            return;
        }
    };
    let glyph = styles.paint(Color::Yellow, styles.icon(Icon::Warning));
    for diagnostic in orch_engine::prompt_mode::startup_diagnostics(&engine.config, &agents) {
        eprintln!(
            "  {glyph} {} ({}): {}",
            diagnostic.agent_name, diagnostic.agent_id, diagnostic.message
        );
    }
}

/// `run <task-id>` — dispatch one task and stream its events.
pub async fn single(engine: &FullEngine, styles: &Styles, task_id: &str) -> Result<ExitCode> {
    let task = engine.task_service.get(task_id).await?;
    println!();
    println!(
        "  {} · running {task_id} \"{}\"",
        styles.paint(Color::Amber, "orch"),
        task.title
    );

    // Installed before the dispatch: from here a signal is latched, never
    // the default immediate death (module docs).
    let mut signals = StandaloneSignals::install();
    // Subscribed before the dispatch, so `agent:started` cannot be missed.
    let mut events = engine.events.subscribe();
    let mut target_run: Option<String> = None;
    let mut failed = false;

    let (dispatched, interrupted) = settle_dispatch(
        &engine.orchestrator,
        engine.orchestrator.run_task(task_id),
        &mut signals,
    )
    .await;
    if let Some(signal) = interrupted {
        return interrupted_exit(engine, styles, signal, dispatched.err()).await;
    }
    dispatched?;

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
        let mut dropped = 0;
        loop {
            let next = tokio::select! {
                signal = signals.next() => {
                    return interrupted_exit(engine, styles, signal, None).await;
                }
                next = tokio::time::timeout(LOST_COMPLETION_POLL, events.recv()) => next,
            };
            match next {
                Ok(Some(event)) => {
                    let ended =
                        handle_single(styles, &event, task_id, &mut target_run, &mut failed);
                    let lost = crate::logs::report_dropped_events(&mut dropped, &events);
                    // Only the completion ends the wait — and a dropped event
                    // may *be* the completion, so check the record too.
                    if ended
                        || (lost
                            && finish_from_record(engine, styles, &target_run, &mut failed).await)
                    {
                        break;
                    }
                }
                // The bus outlives this call, so `None` means shutdown.
                Ok(None) => break,
                Err(_elapsed) => {
                    // Long silence. An agent can be quiet for minutes, so this
                    // ends the wait only if the run is already finished on
                    // disk — in which case its `agent:completed` is never
                    // coming and waiting is waiting forever.
                    if finish_from_record(engine, styles, &target_run, &mut failed).await {
                        break;
                    }
                }
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

/// Finish the wait from the run record when its `agent:completed` never
/// arrived, printing the same `Done`/`Failed` the event handler would.
///
/// Returns whether the run is over. `false` keeps the caller waiting, which is
/// the right answer while the run is still in flight.
async fn finish_from_record(
    engine: &FullEngine,
    styles: &Styles,
    target_run: &Option<String>,
    failed: &mut bool,
) -> bool {
    let Some(run_id) = target_run.as_deref() else {
        return false;
    };
    let Ok(Some(run)) = engine.run_service.get(run_id).await else {
        // A record we cannot read is not evidence the run ended.
        return false;
    };
    use orch_domain::RunStatus;
    match run.status {
        RunStatus::Succeeded => print_success(styles, "Done"),
        RunStatus::Failed | RunStatus::Cancelled | RunStatus::TimedOut => {
            print_error(styles, "Failed", None);
            *failed = true;
        }
        // Still in flight; keep waiting for the event.
        RunStatus::Preparing | RunStatus::Running => return false,
    }
    true
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

/// `run --all` — dispatch every todo task and wait for the queue to drain.
pub async fn all(engine: &FullEngine, styles: &Styles) -> Result<ExitCode> {
    println!();
    println!(
        "  {} · running all todo tasks",
        styles.paint(Color::Amber, "orch")
    );
    println!();

    let mut signals = StandaloneSignals::install();
    let mut events = engine.events.subscribe();
    let (dispatched, interrupted) = settle_dispatch(
        &engine.orchestrator,
        engine.orchestrator.run_all(),
        &mut signals,
    )
    .await;
    if let Some(signal) = interrupted {
        return interrupted_exit(engine, styles, signal, dispatched.err()).await;
    }
    dispatched?;
    tokio::select! {
        () = drain_runs(&mut events) => {}
        signal = signals.next() => {
            return interrupted_exit(engine, styles, signal, None).await;
        }
    }
    // The last completion event arrives from inside a collector; this waits
    // for the work that follows it.
    engine.orchestrator.wait_for_collectors().await;
    Ok(ExitCode::SUCCESS)
}

/// The `SIGINT`/`SIGTERM` arms of the standalone paths.
///
/// Creating an arm replaces the default disposition for the rest of the
/// process, so they are installed once per command and kept until it returns.
/// An arm that cannot be installed is reported and leaves that signal's
/// default behaviour in place.
struct StandaloneSignals {
    interrupt: Option<tokio::signal::unix::Signal>,
    terminate: Option<tokio::signal::unix::Signal>,
}

impl StandaloneSignals {
    fn install() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        let install = |kind: SignalKind, name: &str| {
            signal(kind)
                .inspect_err(|err| tracing::warn!(%err, "could not install the {name} handler"))
                .ok()
        };
        StandaloneSignals {
            interrupt: install(SignalKind::interrupt(), "SIGINT"),
            terminate: install(SignalKind::terminate(), "SIGTERM"),
        }
    }

    /// The next signal. Pending forever when neither arm exists.
    async fn next(&mut self) -> Signal {
        match (self.interrupt.as_mut(), self.terminate.as_mut()) {
            (Some(interrupt), Some(terminate)) => tokio::select! {
                _ = interrupt.recv() => Signal::Interrupt,
                _ = terminate.recv() => Signal::Terminate,
            },
            (Some(interrupt), None) => {
                interrupt.recv().await;
                Signal::Interrupt
            }
            (None, Some(terminate)) => {
                terminate.recv().await;
                Signal::Terminate
            }
            (None, None) => std::future::pending().await,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Interrupt,
    Terminate,
}

impl Signal {
    fn name(self) -> &'static str {
        match self {
            Signal::Interrupt => "SIGINT",
            Signal::Terminate => "SIGTERM",
        }
    }

    /// The shell convention for death by this signal.
    fn exit_code(self) -> ExitCode {
        ExitCode::from(match self {
            Signal::Interrupt => 130,
            Signal::Terminate => 143,
        })
    }
}

/// Await a dispatch to completion even when a signal arrives meanwhile.
///
/// The dispatch is never dropped: it may be between two persisted writes, and
/// abandoning it there is exactly the half-written critical section the
/// orchestrator's own stop path refuses to produce. A signal instead latches
/// the orchestrator's interrupt, which bounds what is left (an effort probe in
/// flight is killed and reaped, no model child starts), and the dispatch's own
/// result is still returned.
async fn settle_dispatch(
    orchestrator: &Orchestrator,
    dispatch: impl Future<Output = Result<()>>,
    signals: &mut StandaloneSignals,
) -> (Result<()>, Option<Signal>) {
    tokio::pin!(dispatch);
    tokio::select! {
        biased;
        result = &mut dispatch => (result, None),
        signal = signals.next() => {
            orchestrator.interrupt();
            (dispatch.await, Some(signal))
        }
    }
}

/// Stop this process's runs after a signal, report it, and pick the exit code.
async fn interrupted_exit(
    engine: &FullEngine,
    styles: &Styles,
    signal: Signal,
    dispatch_error: Option<orch_core::Error>,
) -> Result<ExitCode> {
    let stopped = engine
        .orchestrator
        .stop_interrupted_runs(signal.name())
        .await;
    println!();
    print_error(
        styles,
        &format!(
            "Interrupted by {}: stopped the runs this command started",
            signal.name()
        ),
        None,
    );
    for err in dispatch_error.iter().chain(stopped.as_ref().err()) {
        print_error(styles, &err.to_string(), None);
    }
    Ok(signal.exit_code())
}

/// Wait until no run is in flight and no reactive dispatch starts another.
///
/// `--all` prints nothing per event — no listener is registered in
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
            // drained, so there is nothing left to wait for.
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
pub async fn watch(
    engine: std::sync::Arc<FullEngine>,
    styles: &Styles,
    verbose: bool,
) -> Result<ExitCode> {
    println!(
        "{} · watching · poll interval {}s",
        styles.paint(Color::Amber, "orch"),
        // Integer division: `/ 1000` on a round number.
        engine.config.scheduling.poll_interval_ms / 1000
    );
    println!("{}", "━".repeat(43));
    println!();

    let events = engine.events.subscribe();
    // Ended by a token rather than `abort()`, and drained afterwards. The
    // shutdown emits a burst — `orchestrator:shutdown`, then a run, task and
    // agent event per entry the wind-down finishes — and aborting the pump
    // discards whatever it has not printed yet, including the last line
    // (`Shutting down...`).
    let done = tokio_util::sync::CancellationToken::new();
    let pump = tokio::spawn({
        let styles = *styles;
        let done = done.clone();
        async move {
            crate::logs::pump_until_cancelled(events, done, |event| {
                print_watch_event(&styles, event, verbose);
            })
            .await;
        }
    });

    let peer_runtime = crate::serve::peer::start_watch(
        std::sync::Arc::clone(&engine),
        orch_engine::WatchOptions::watching(),
    )
    .await?;
    engine.orchestrator.wait_for_stop().await;
    drop(peer_runtime);

    done.cancel();
    let _ = pump.await;
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
        // Includes shutdown's unacknowledged remote cancels, which name the
        // peer and execution that may still be running.
        OrchestratorEvent::OrchestratorError { error, context, .. } => {
            println!(
                "{}",
                crate::logs::format_orchestrator_error(styles, &time, error, context)
            );
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

    /// A project with one finished run, so the backstop has a record to read.
    async fn engine_with_run(
        status: orch_domain::RunStatus,
    ) -> (tempfile::TempDir, FullEngine, String) {
        let temp = tempfile::tempdir().expect("tempdir");
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        let paths = orch_infra::storage::Paths::new(temp.path());
        std::fs::create_dir_all(paths.root()).expect("create .orchestry");
        orch_core::storage::ConfigStore::write(
            &orch_infra::storage::FileConfigStore::new(paths.clone()),
            &orch_domain::default_config(),
        )
        .await
        .expect("write config");

        let engine = orch_engine::build_full_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(paths.root().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .expect("build_full");

        let run = engine
            .run_service
            .create(orch_engine::CreateRunInput {
                task_id: "tsk_1".to_owned(),
                agent_id: "agt_1".to_owned(),
                attempt: 1,
                prompt: "p".to_owned(),
                workspace_path: "/ws".to_owned(),
            })
            .await
            .expect("create run");
        if status != orch_domain::RunStatus::Preparing {
            engine
                .run_service
                .finish(&run.id, status, None, None)
                .await
                .expect("finish run");
        }
        (temp, engine, run.id)
    }

    /// The backstop for an `agent:completed` that never arrives — dropped by a
    /// lagging bus, or lost outright. Without it, `run <id>` waits forever on
    /// an event that is not coming.
    #[tokio::test]
    async fn a_finished_run_record_ends_the_wait_and_sets_the_exit_code() {
        use orch_domain::RunStatus;
        let styles = Styles::plain();

        for (status, expect_failed) in [
            (RunStatus::Succeeded, false),
            (RunStatus::Failed, true),
            (RunStatus::Cancelled, true),
            (RunStatus::TimedOut, true),
        ] {
            let (_temp, engine, run_id) = engine_with_run(status).await;
            let mut failed = false;
            assert!(
                finish_from_record(&engine, &styles, &Some(run_id), &mut failed).await,
                "{status:?} is terminal and must end the wait"
            );
            assert_eq!(failed, expect_failed, "{status:?} exit code");
        }
    }

    #[tokio::test]
    async fn an_unfinished_run_keeps_the_wait_open() {
        let styles = Styles::plain();
        let (_temp, engine, run_id) = engine_with_run(orch_domain::RunStatus::Preparing).await;
        let mut failed = false;
        // Still in flight: ending here would truncate the stream.
        assert!(!finish_from_record(&engine, &styles, &Some(run_id), &mut failed).await);
        // And an id with no record at all is not evidence of anything.
        assert!(
            !finish_from_record(
                &engine,
                &styles,
                &Some("run_missing".to_owned()),
                &mut failed
            )
            .await
        );
        assert!(!finish_from_record(&engine, &styles, &None, &mut failed).await);
        assert!(!failed);
    }

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
