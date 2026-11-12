//! Own entered diagnostic work independently of the observer and its deadline.
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

const JOBS: usize = 32;
#[derive(Debug, Default)]
struct State {
    sealed: bool,
    live: usize,
}
#[derive(Debug)]
pub struct SpawnWork {
    state: Mutex<State>,
    live: watch::Sender<usize>,
    slots: Arc<Semaphore>,
    cancelled: tokio_util::sync::CancellationToken,
}
impl Default for SpawnWork {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            live: watch::channel(0).0,
            slots: Arc::new(Semaphore::new(JOBS)),
            cancelled: tokio_util::sync::CancellationToken::new(),
        }
    }
}
impl SpawnWork {
    pub(crate) async fn enter(self: &Arc<Self>) -> Option<WorkLease> {
        let slot = Arc::clone(&self.slots).acquire_owned().await.ok()?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sealed {
            return None;
        }
        state.live += 1;
        self.live.send_replace(state.live);
        Some(WorkLease {
            owner: Arc::clone(self),
            _slot: Some(slot),
        })
    }
    /// Synchronous manager work already has the caller's bounded admission.
    /// Its exit observer retains ownership, including after capture is cancelled.
    pub(crate) fn track(self: &Arc<Self>) -> Option<WorkLease> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sealed {
            return None;
        }
        state.live += 1;
        self.live.send_replace(state.live);
        Some(WorkLease {
            owner: Arc::clone(self),
            _slot: None,
        })
    }
    pub fn is_sealed(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sealed
    }
    pub fn seal(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sealed = true;
        self.slots.close();
        self.cancelled.cancel();
    }
    pub async fn cancelled(&self) {
        self.cancelled.cancelled().await;
    }
    pub async fn wait_idle(&self) {
        let mut live = self.live.subscribe();
        let _ = live.wait_for(|n| *n == 0).await;
    }
    pub fn is_idle(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).live == 0
    }
}
pub(crate) struct WorkLease {
    owner: Arc<SpawnWork>,
    _slot: Option<OwnedSemaphorePermit>,
}
impl WorkLease {
    pub(crate) fn belongs_to(&self, work: &Arc<SpawnWork>) -> bool {
        Arc::ptr_eq(&self.owner, work)
    }
}
impl Drop for WorkLease {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.live -= 1;
        self.owner.live.send_replace(state.live);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn owned_jobs_are_bounded_and_sealing_excludes_waiting_submission() {
        let work = Arc::new(SpawnWork::default());
        let mut leases = Vec::new();
        for _ in 0..JOBS {
            leases.push(work.enter().await.unwrap());
        }
        let queued = tokio::spawn({
            let work = work.clone();
            async move { work.enter().await }
        });
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        work.seal();
        assert!(queued.await.unwrap().is_none());
        assert!(!work.is_idle());
        drop(leases);
        work.wait_idle().await;
        assert!(work.is_idle());
        assert!(work.enter().await.is_none());
    }
}
