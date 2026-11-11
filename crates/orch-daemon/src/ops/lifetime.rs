//! Accepted operations retain write/spawn authority until their task ends.
//!
//! Socket observation may detach, but lifecycle cannot release project ownership
//! until this registry is sealed and empty. No deadline can revoke a partial write.

use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Default)]
struct State {
    sealed: bool,
    live: usize,
}

pub(super) struct Requests {
    state: Mutex<State>,
    live: watch::Sender<usize>,
}

impl Default for Requests {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            live: watch::channel(0).0,
        }
    }
}

impl Requests {
    pub(super) fn enter(self: &Arc<Self>) -> Option<RequestLease> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sealed {
            return None;
        }
        state.live += 1;
        self.live.send_replace(state.live);
        Some(RequestLease(Arc::clone(self)))
    }

    pub(super) fn seal(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sealed = true;
    }

    pub(super) async fn wait_idle(&self) {
        let mut live = self.live.subscribe();
        let _ = live.wait_for(|count| *count == 0).await;
    }
}

pub(super) struct RequestLease(Arc<Requests>);

impl Drop for RequestLease {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.live -= 1;
        self.0.live.send_replace(state.live);
    }
}
