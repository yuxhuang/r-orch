//! `orch tui` — build the engine, hand it to the dashboard, clean up after.
//!
//! The port of `tui.ts`'s `.action(...)` tail: render, `waitUntilExit()`, then
//! stop watch mode or the observer and clear the event bus. [`first_run`] is
//! the bare-`orch` path from `src/bin/cli.ts` on top of it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use orch_core::error::{Error, Result};
use orch_infra::storage::{ORCHESTRY_DIR, find_project_root};

use super::backend::EngineBackend;
use crate::commands::init;
use crate::output::Styles;

/// How long the wind-down gets when the terminal is already gone.
///
/// `Orchestrator::stop()` is unbounded, and nothing enforces a run timeout
/// except `stall_timeout_ms`, which fires only from the watch loop (PARITY.md
/// §6.5) — so one hung agent can hold the shutdown, and the lock, for as long
/// as it likes. With a terminal there that is the right trade: the user is
/// watching and the agents get their full grace. With the terminal gone nobody
/// is watching, and a process that will not exit is worse than a lock released
/// a moment late — `acquire_lock` deletes a lock whose PID is not alive
/// (`orch-infra/src/storage/lock.rs:51-62`), so the next `orch` reclaims it as
/// soon as this process is gone. See PARITY.md §14.40.
const DEAD_TTY_WIND_DOWN: Duration = Duration::from_secs(5);

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
    run_with_peer(
        project_root,
        version,
        observe,
        orch_domain::PeerConfig::default(),
        false,
    )
    .await
}

pub async fn run_with_peer(
    project_root: PathBuf,
    version: &str,
    observe: bool,
    peer: orch_domain::PeerConfig,
    explicit_listen: bool,
) -> Result<ExitCode> {
    if explicit_listen && observe {
        return Err(Error::invalid_arguments(
            "--listen requires the engine owner; --observe cannot listen",
        ));
    }
    // SIGHUP (the terminal went away) and SIGTERM end the draw loop the same
    // way `q` does, so the cleanup below runs: agents get their stop grace and
    // the lock is released, instead of the process dying with them orphaned.
    // Installed before anything slow, so a signal during startup or recovery
    // is also deferred to the cleanup rather than hitting the default action.
    let stop = Arc::new(AtomicBool::new(false));
    let hungup = Arc::new(AtomicBool::new(false));
    watch_termination(Arc::clone(&stop), Arc::clone(&hungup));

    let engine = Arc::new(orch_engine::build_full_with_peer(project_root, peer).await?);
    let backend = EngineBackend::launch_with(Arc::clone(&engine), version, observe).await;

    // Read before the move: `orch_tui::run` consumes the backend, and the
    // cleanup below needs to know whether *this* process started watch mode.
    let watch_active = backend.is_watch_active();
    let peer_runtime = backend.peer_runtime_guard();
    if let Some(error) = backend.startup_error() {
        return Err(Error::other(format!("cannot start the engine: {error}")));
    }
    if explicit_listen && !watch_active {
        return Err(Error::invalid_arguments(
            "--listen requires the engine owner; another orchestrator holds the lock",
        ));
    }

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
        Ok(Ok(orch_tui::Exit::Requested))
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
    if engine.orchestrator.is_owner() {
        // `stop()` already reports its own failures on the bus; the TS
        // swallows this rejection outright (`.catch(() => {})`).
        let stopping = engine.orchestrator.stop();
        if bound_wind_down(
            outcome.as_ref().ok().and_then(|r| r.as_ref().ok().copied()),
            hungup.load(Ordering::Relaxed),
        ) {
            if tokio::time::timeout(DEAD_TTY_WIND_DOWN, stopping)
                .await
                .is_err()
            {
                tracing::warn!(
                    grace_ms = DEAD_TTY_WIND_DOWN.as_millis(),
                    "the terminal is gone and the orchestrator did not stop in time; \
                     releasing the lock and exiting with agents possibly still running"
                );
            }
        } else {
            let _ = stopping.await;
        }
    }
    peer_runtime
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    engine.events.clear();

    match outcome {
        // The guard in `orch_tui::run` restored the terminal before unwinding
        // past it, so it is safe to print.
        Err(panic) => Err(Error::other(format!(
            "the dashboard panicked: {}",
            panic_message(&panic)
        ))),
        Ok(Err(err)) => Err(Error::other(format!("dashboard: {err}"))),
        Ok(Ok(_)) => Ok(ExitCode::SUCCESS),
    }
}

/// Should the wind-down be bounded by [`DEAD_TTY_WIND_DOWN`]?
///
/// Yes exactly when the terminal is gone, which happens two ways the draw
/// loop cannot tell apart: it returns `TerminalGone` when it detected the
/// hang-up itself, but a **SIGHUP** reaches it as the generic `stop` flag and
/// comes back as `Requested` — indistinguishable from `q`. So the signal
/// watcher records the hang-up separately and it is or-ed in here.
///
/// SIGTERM and SIGINT are *not* included: those come from a service manager
/// or the user, who are waiting for the shutdown, so the agents keep their
/// full grace. Nobody is waiting on a terminal that has gone away.
fn bound_wind_down(exit: Option<orch_tui::Exit>, hungup: bool) -> bool {
    // Only an orderly `Exit::Requested` — `q`, SIGTERM, SIGINT — waits
    // indefinitely. `None` is a dashboard that **panicked or errored**, and it
    // belongs with the gone-terminal cases: the guard has already restored the
    // terminal on its way out, so there is nothing left to show a shutdown on
    // and nobody waiting for the agents' grace. Leaving that path unbounded is
    // how a panicked dashboard could sit forever inside `stop()` — alive,
    // re-parented, and unable to act on a single SIGTERM because the signal
    // task is waiting for a *second* one (QA B4).
    !matches!(exit, Some(orch_tui::Exit::Requested)) || hungup
}

/// Set `stop` when SIGHUP or SIGTERM arrives; a second one exits at once.
///
/// `hungup` additionally records that it was **SIGHUP**, which is a different
/// situation from SIGTERM however similar the shutdown looks: SIGHUP means the
/// terminal went away, so nobody is watching and nobody is waiting out an
/// agent's stop grace — exactly the condition that bounds the wind-down for a
/// dead tty (PARITY.md §14.40). SIGTERM and SIGINT come from a service manager
/// or the user, who *are* waiting, and keep the unbounded wait.
#[cfg(unix)]
fn watch_termination(stop: Arc<AtomicBool>, hungup: Arc<AtomicBool>) {
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
            _ = hangup.recv() => {
                tracing::info!("SIGHUP: shutting the dashboard down");
                hungup.store(true, Ordering::Relaxed);
            }
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
fn watch_termination(_stop: Arc<AtomicBool>, _hungup: Arc<AtomicBool>) {}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = panic.downcast_ref::<&str>() {
        (*msg).to_owned()
    } else if let Some(msg) = panic.downcast_ref::<String>() {
        msg.clone()
    } else {
        "unknown cause".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_tui::Exit;

    /// PARITY.md §14.40 bounds the wind-down only when the terminal is gone.
    /// A SIGHUP *is* that, but it arrives as `Requested`, so the flag is what
    /// carries it — and getting this wrong means a hung agent holds the lock
    /// after the terminal has closed, which is the case §14.40 exists for.
    #[test]
    fn only_a_vanished_terminal_bounds_the_wind_down() {
        assert!(
            bound_wind_down(Some(Exit::TerminalGone), false),
            "the draw loop detected the hang-up itself"
        );
        assert!(
            bound_wind_down(Some(Exit::Requested), true),
            "SIGHUP arrives as `Requested`; the flag is the only signal of it"
        );
        assert!(
            !bound_wind_down(Some(Exit::Requested), false),
            "`q`, SIGTERM and SIGINT keep the unbounded wait: someone is watching"
        );
        assert!(
            bound_wind_down(None, true),
            "a panicked dashboard after SIGHUP is still a gone terminal"
        );
        assert!(
            bound_wind_down(None, false),
            "a panic or an I/O error is bounded too: the guard restored the \
             terminal on the way out, so nobody is watching the shutdown, and \
             an unbounded `stop()` there can hang the process for good"
        );
    }
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    #[tokio::test]
    async fn explicit_listen_refuses_observer_mode_before_starting_engine() {
        let err = run_with_peer(
            PathBuf::from("/unused"),
            "test",
            true,
            orch_domain::PeerConfig {
                name: None,
                listen: Some("127.0.0.1:0".into()),
            },
            true,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--observe cannot listen"));
    }
}
