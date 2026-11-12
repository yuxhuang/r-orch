//! A single coalesced wake slot, published synchronously after permit release.
use orch_engine::container::commit::{CommitSource, Publication};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

const DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Dirty {
    generation: u64,
    serviced: u64,
    first: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct Wake {
    dirty: Mutex<Dirty>,
    pub(crate) ready: Notify,
}
impl Wake {
    pub(crate) fn publish(&self, publication: &Publication) {
        let Publication::Commit(record) = publication else {
            return;
        };
        if record.source == CommitSource::Tick {
            return;
        }
        let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        dirty.generation = dirty.generation.wrapping_add(1);
        dirty.first.get_or_insert_with(Instant::now);
        drop(dirty);
        self.ready.notify_one();
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        let dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        dirty.first.map(|first| first + DEBOUNCE)
    }

    pub(crate) fn take(&self) {
        let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        dirty.serviced = dirty.generation;
        dirty.first = None;
    }

    pub(crate) fn after_tick(&self) {
        let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        if dirty.generation != dirty.serviced {
            // A commit during external preparation survives. Give it one
            // fixed follow-up window; later commits cannot move this deadline.
            dirty.first = Some(Instant::now());
        }
    }
}
