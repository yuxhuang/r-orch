//! The orch-peer TCP runtime hosted inside the daemon (design D10, spec R19).
//!
//! The daemon starts exactly what `orch serve` starts — link management to
//! configured remotes, inbound message expiry and, with `--listen`, the
//! inbound listener — with the same wire protocol, so mixed 0.5.x–0.14 peers
//! interoperate unchanged. Peer store writes enter the project's operation
//! boundary inside orch-peer; network I/O stays outside it.
//!
//! The runtime runs whatever the dispatch switch says (D3): submissions,
//! acknowledgements and messages are processed with dispatch OFF, and the
//! mirror tasks they create wait for dispatch like any other queued task.

use crate::DaemonError;
use orch_engine::container::FullEngine;
use orch_peer::PeerRuntime;
use std::sync::Arc;

/// A prepared runtime. The listener (if any) is already bound, so a busy
/// `--listen` address fails the start, but it admits nobody until
/// [`HostedPeer::open`].
pub(crate) struct HostedPeer {
    runtime: PeerRuntime,
}

impl HostedPeer {
    /// After startup recovery: re-registers remote runs before the first
    /// link and resets stale peer status. `--listen` / `--peer-name` were
    /// applied to the engine's peer config at build.
    pub(crate) async fn prepare(engine: Arc<FullEngine>) -> Result<HostedPeer, DaemonError> {
        Ok(HostedPeer {
            runtime: PeerRuntime::prepare(engine).await?,
        })
    }

    /// Drain peer state work while the daemon still owns the project.
    pub(crate) async fn shutdown(self) {
        self.runtime.shutdown().await;
    }

    /// Begin inbound admission. Called once the daemon is READY, so a
    /// connection flood cannot exhaust descriptors during startup.
    pub(crate) fn open(&mut self) {
        self.runtime.start_listener();
    }
}

#[cfg(test)]
mod tests;
