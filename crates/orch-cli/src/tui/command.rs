//! `orch tui` — build the engine, hand it to the dashboard, clean up after.
//!
//! The port of `tui.ts`'s `.action(...)` tail: render, `waitUntilExit()`, then
//! stop watch mode or the observer and clear the event bus.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use orch_core::error::{Error, Result};

use super::backend::EngineBackend;

/// Launch the dashboard and block until the user quits.
///
/// Must be called from the runtime — `dispatch` already is.
pub async fn run(project_root: PathBuf, version: &str) -> Result<ExitCode> {
    let engine = Arc::new(orch_engine::build_full(project_root).await?);
    let backend = EngineBackend::launch(Arc::clone(&engine), version).await;

    // Read before the move: `orch_tui::run` consumes the backend, and the
    // cleanup below needs to know whether *this* process started watch mode.
    let watch_active = backend.is_watch_active();
    // TODO(wp5): in observer mode, attach `serve::disk_observer` to
    // `backend.observer_sink()` here and stop it in the cleanup below. The
    // observer is on the WP-5a branch; until it lands, an observer-mode
    // dashboard renders from the stores on every refresh but sees no live
    // events.

    // The draw loop must not run on a runtime thread: `TuiBackend` is
    // synchronous and its methods call `Handle::block_on`, which panics inside
    // a runtime context. A plain `std::thread` has no such context, so the
    // handle the backend carries resolves against the runtime this task is
    // running on. See the module docs and `orch-engine/tests/sync_bridge.rs`.
    let draw = std::thread::Builder::new()
        .name("orch-tui".to_owned())
        .spawn(move || orch_tui::run(backend))
        .map_err(|err| Error::other(format!("cannot start the dashboard thread: {err}")))?;

    // `join` blocks, so it goes to the blocking pool rather than a worker:
    // awaiting here keeps the runtime free to serve the draw thread's calls.
    let outcome = tokio::task::spawn_blocking(move || draw.join())
        .await
        .map_err(|err| Error::other(format!("the dashboard thread was cancelled: {err}")))?;

    // Cleanup runs whether the dashboard exited cleanly or panicked — a panic
    // there must not leave the lock held or an agent running.
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

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = panic.downcast_ref::<&str>() {
        (*msg).to_owned()
    } else if let Some(msg) = panic.downcast_ref::<String>() {
        msg.clone()
    } else {
        "unknown cause".to_owned()
    }
}
