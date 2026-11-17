//! Safe signal registration, driven by the synchronous command owner's poll loop.
//! Tokio installs process-global handlers; a gate is a standalone CLI invocation.
//! Do not embed this lifecycle in a long-lived service expecting default handlers
//! to be restored when a Runner is dropped.

use std::future::{pending, poll_fn};
use std::task::Poll;
use std::time::Duration;

use anyhow::Result;
use tokio::runtime::{Builder, Runtime};
use tokio::signal::unix::{Signal, SignalKind, signal};

pub(super) struct Signals {
    runtime: Runtime,
    streams: [(i32, Signal); 3],
    first: Option<i32>,
}

impl Signals {
    pub(super) fn new() -> Result<Self> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let streams = {
            let _entered = runtime.enter();
            [
                (nix::libc::SIGINT, signal(SignalKind::interrupt())?),
                (nix::libc::SIGTERM, signal(SignalKind::terminate())?),
                (nix::libc::SIGHUP, signal(SignalKind::hangup())?),
            ]
        };
        Ok(Self {
            runtime,
            streams,
            first: None,
        })
    }

    /// Drive the reactor even for a nonblocking observation. timeout polls the
    /// signal future first. A handled signal stays sticky through publication.
    pub(super) fn wait(&mut self, delay: Duration) -> Option<i32> {
        let streams = &mut self.streams;
        let observed = self.runtime.block_on(async {
            tokio::time::timeout(
                delay,
                poll_fn(|cx| {
                    for (number, stream) in streams.iter_mut() {
                        if let Poll::Ready(Some(())) = stream.poll_recv(cx) {
                            return Poll::Ready(*number);
                        }
                    }
                    Poll::Pending
                }),
            )
            .await
            .ok()
        });
        if self.first.is_none() {
            self.first = observed;
        }
        self.first
    }

    pub(super) fn pause(&self, delay: Duration) {
        self.runtime.block_on(async {
            let _ = tokio::time::timeout(delay, pending::<()>()).await;
        });
    }
}
