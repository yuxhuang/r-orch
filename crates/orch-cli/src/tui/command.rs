//! `orch tui` — build the engine, hand it to the dashboard, clean up after.
//!
//! The port of `tui.ts`'s `.action(...)` tail: render, `waitUntilExit()`, then
//! stop watch mode or the observer and clear the event bus. [`first_run`] is
//! the bare-`orch` path from `src/bin/cli.ts` on top of it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orch_core::error::{Error, Result};
use orch_infra::storage::{ORCHESTRY_DIR, find_project_root};

use super::backend::EngineBackend;
use crate::commands::init;
use crate::output::Styles;

/// Bare `orch`: the first-run experience (PARITY.md §2.3, `src/bin/cli.ts`
/// `isBareOrch`).
///
/// With no `.orchestry/` in this directory, initialize one and launch the
/// dashboard on it; in an initialized project, launch straight into it.
///
/// The probe is deliberately **`cwd`-local**, and project-root resolution
/// happens **after** the init, for the same reason the reference spells out at
/// `cli.ts:112`: `find_project_root` walks up, so checking or resolving first
/// would silently adopt an ancestor project and leave the user's new directory
/// un-initialized. A developer running `orch` in a fresh subdirectory of a
/// project they already have is the common case, not an edge one.
pub async fn first_run(
    styles: &Styles,
    cwd: &Path,
    init_args: &init::InitArgs,
    version: &str,
) -> Result<ExitCode> {
    if !cwd.join(ORCHESTRY_DIR).is_dir() {
        // No `Next:` line here: the reference prints that from the `init`
        // command's own action rather than from `runInit`
        // (`src/cli/commands/init.ts:262-271`), so the first-run path does not
        // suggest a command the dashboard is about to make unnecessary.
        init::run(styles, cwd, init_args).await?;
    }
    run(find_project_root(cwd), version, false).await
}

/// Start a [`DiskObserver`](crate::serve::disk_observer::DiskObserver) feeding
/// `sink`, which is what observer mode reads its live events from.
///
/// Public so the test can wire it the way [`run`] does rather than the way it
/// imagines [`run`] does; dropping the handle stops the poll loop.
pub fn attach_observer(
    engine: &orch_engine::FullEngine,
    sink: Arc<dyn Fn(orch_domain::OrchestratorEvent) + Send + Sync>,
) -> crate::serve::disk_observer::ObserverHandle {
    crate::serve::disk_observer::DiskObserver::new(
        engine.paths.clone(),
        Arc::clone(&engine.state),
        Arc::clone(&engine.runs),
        Arc::clone(&engine.clock),
    )
    .observe(move |event| sink(event))
}

/// Launch the dashboard and block until the user quits.
///
/// Must be called from the runtime — `dispatch` already is.
///
/// `observe` is `--observe`: never start the orchestrator, only watch the
/// project as an observer (see [`EngineBackend::launch_with`]).
pub async fn run(project_root: PathBuf, version: &str, observe: bool) -> Result<ExitCode> {
    // SIGHUP (the terminal went away) and SIGTERM end the draw loop the same
    // way `q` does, so the cleanup below runs: agents get their stop grace and
    // the lock is released, instead of the process dying with them orphaned.
    // Installed before anything slow, so a signal during startup or recovery
    // is also deferred to the cleanup rather than hitting the default action.
    let stop = Arc::new(AtomicBool::new(false));
    watch_termination(Arc::clone(&stop));

    let engine = Arc::new(orch_engine::build_full(project_root).await?);
    let backend = EngineBackend::launch_with(Arc::clone(&engine), version, observe).await;

    // Read before the move: `orch_tui::run` consumes the backend, and the
    // cleanup below needs to know whether *this* process started watch mode.
    let watch_active = backend.is_watch_active();

    // Observer mode: another process holds the lock, so there is no in-process
    // bus traffic to subscribe to and the live events have to come off disk.
    // The TS does the same thing at the same point —
    // `onSubscribeEvents = handler => diskObserver.subscribe(handler)`
    // (`tui.ts:261-267`) — and constructs the observer with no interval, so
    // both poll at the default 1 s.
    //
    // Dropped in the cleanup below, which is the `unsubscribe` the TS
    // `subscribe` returns.
    //
    // Three visible differences remain in observer mode, all TS-faithful: the
    // `● OBSERVING` header chip, a 3 s dashboard refresh instead of 5 s, and
    // **stale teams** — the periodic refresh passes `include_teams = false`
    // (`App.tsx:826`), so a team another process creates or disbands does not
    // appear until something team-related happens locally. Changing that would
    // need a PARITY §14 entry: which collections the poll re-reads is
    // observable.
    let observer = (!watch_active).then(|| attach_observer(&engine, backend.observer_sink()));

    // The draw loop must not run on a runtime thread: `TuiBackend` is
    // synchronous and its methods call `Handle::block_on`, which panics inside
    // a runtime context. A plain `std::thread` has no such context, so the
    // handle the backend carries resolves against the runtime this task is
    // running on. See the module docs and `orch-engine/tests/sync_bridge.rs`.
    let outcome = if stop.load(Ordering::Relaxed) {
        // Signalled while starting up: no dashboard, straight to cleanup.
        drop(backend);
        Ok(Ok(()))
    } else {
        let draw = std::thread::Builder::new()
            .name("orch-tui".to_owned())
            .spawn(move || orch_tui::run_until(backend, &stop))
            .map_err(|err| Error::other(format!("cannot start the dashboard thread: {err}")))?;

        // `join` blocks, so it goes to the blocking pool rather than a worker:
        // awaiting here keeps the runtime free to serve the draw thread's calls.
        tokio::task::spawn_blocking(move || draw.join())
            .await
            .map_err(|err| Error::other(format!("the dashboard thread was cancelled: {err}")))?
    };

    // Cleanup runs whether the dashboard exited cleanly or panicked — a panic
    // there must not leave the lock held, an agent running or a poll loop
    // reading the project after the window is gone.
    drop(observer);
    if watch_active {
        // `stop()` already reports its own failures on the bus; the TS
        // swallows this rejection outright (`.catch(() => {})`).
        let _ = engine.orchestrator.stop().await;
    }
    engine.events.clear();

    match outcome {
        // The guard in `orch_tui::run` restored the terminal before unwinding
        // past it, so it is safe to print.
        Err(panic) => Err(Error::other(format!(
            "the dashboard panicked: {}",
            panic_message(&panic)
        ))),
        Ok(Err(err)) => Err(Error::other(format!("dashboard: {err}"))),
        Ok(Ok(())) => Ok(ExitCode::SUCCESS),
    }
}

/// Set `stop` when SIGHUP or SIGTERM arrives; a second one exits at once.
#[cfg(unix)]
fn watch_termination(stop: Arc<AtomicBool>) {
    use tokio::signal::unix::{SignalKind, signal};

    let (mut hangup, mut terminate) = match (
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
    ) {
        (Ok(hangup), Ok(terminate)) => (hangup, terminate),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!(%err, "cannot install SIGHUP/SIGTERM handlers; they will kill the dashboard without cleanup");
            return;
        }
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = hangup.recv() => tracing::info!("SIGHUP: shutting the dashboard down"),
            _ = terminate.recv() => tracing::info!("SIGTERM: shutting the dashboard down"),
        }
        stop.store(true, Ordering::Relaxed);

        // The graceful path can take a while (agents get a stop grace). A
        // second signal means "now".
        tokio::select! {
            _ = hangup.recv() => {}
            _ = terminate.recv() => {}
        }
        eprintln!("orch tui: second signal, exiting without cleanup");
        std::process::exit(130);
    });
}

#[cfg(not(unix))]
fn watch_termination(_stop: Arc<AtomicBool>) {}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = panic.downcast_ref::<&str>() {
        (*msg).to_owned()
    } else if let Some(msg) = panic.downcast_ref::<String>() {
        msg.clone()
    } else {
        "unknown cause".to_owned()
    }
}
