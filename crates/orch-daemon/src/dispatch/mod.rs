//! Automatic dispatch hosted in the daemon (design D3, D8; spec R1, R13).
//!
//! This is `orch serve`'s watch loop as a daemon mode: one coordinated tick
//! per configured poll interval while the global dispatch switch is on. The
//! switch itself, the manual entrances (`dispatch.tick`, `run_task`,
//! `run_all`, `until_terminal`) and the `serve --once` batch policy are the
//! `dispatch.*` handlers in [`crate::ops`]; every entrance, this loop
//! included, ticks through the engine's operation boundary that the
//! [`crate::coordinator::ProjectCoordinator`] shares, so concurrent ticks
//! serialise and recheck eligibility before any launch.
//!
//! What does not live here, because it must not depend on the switch:
//! context expiry and transfer sweeps (`ServiceHandler::run_maintenance`),
//! the PID-lock refresh (`lifecycle::heartbeat`), run collectors (spawned per
//! run at launch) and the peer runtime ([`crate::peer`]).

use crate::coordinator::Lane;
use crate::ops::ServiceHandler;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Used when the configured interval cannot be read.
const DEFAULT_POLL: Duration = Duration::from_secs(10);
/// The floor for a configured `scheduling.poll_interval_ms`.
const MIN_POLL: Duration = Duration::from_millis(100);

/// Tick while dispatch is on until `quit` is set or the daemon stops. The
/// interval is read fresh each round so `config.set` applies to the next one
/// (R30). A tick with dispatch off, during a batch or while stopping is a
/// no-op decided by [`ServiceHandler::automatic_tick`].
pub(crate) async fn run_loop(handler: ServiceHandler, mut quit: watch::Receiver<bool>) {
    loop {
        let interval = poll_interval(&handler).await;
        tokio::select! {
            () = crate::session::until_set(&mut quit) => return,
            () = tokio::time::sleep(interval) => {}
        }
        if handler.coordinator().is_stopping() {
            return;
        }
        if let Err(error) = handler.automatic_tick().await {
            tracing::warn!(code = ?error.code, "dispatch tick failed");
        }
    }
}

async fn poll_interval(handler: &ServiceHandler) -> Duration {
    let engine = Arc::clone(&handler.engine);
    let read = handler
        .coordinator()
        .run_op(Lane::Internal, move |_| {
            Box::pin(async move {
                Ok(engine
                    .config_store
                    .read()
                    .await
                    .map(|config| config.scheduling.poll_interval_ms)
                    .ok())
            })
        })
        .await;
    match read {
        Ok(committed) => committed
            .value
            .map_or(DEFAULT_POLL, |ms| Duration::from_millis(ms).max(MIN_POLL)),
        Err(_) => DEFAULT_POLL,
    }
}

#[cfg(test)]
pub(crate) mod tests;
