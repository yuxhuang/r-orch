//! Active-reader retirement from an actual passive-checkpoint observation (R127).
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub(super) const MAX_AGE: Duration = Duration::from_secs(30);
pub(super) const MAX_BACKLOG: u64 = 256 * 1024 * 1024;

/// A passive checkpoint's exact frame counts, and the reader action they caused.
/// The WAL file may remain large even when `backlog_bytes` is zero.
#[derive(Debug, Clone)]
pub struct CheckpointObservation {
    pub log_frames: i32,
    pub checkpointed_frames: i32,
    pub backlog_bytes: u64,
    pub oldest_read: Option<Duration>,
    pub interrupted: usize,
}

struct Active {
    started: Mutex<Instant>,
    expired: Arc<AtomicBool>,
    interrupt: rusqlite::InterruptHandle,
}

#[derive(Default)]
pub(in crate::sqlite) struct ActiveReads(Mutex<Vec<Arc<Active>>>);

impl ActiveReads {
    pub(super) fn register(
        &self,
        tx: &Transaction<'_>,
        expired: Arc<AtomicBool>,
    ) -> Registration<'_> {
        let active = Arc::new(Active {
            started: Mutex::new(Instant::now()),
            expired,
            interrupt: tx.get_interrupt_handle(),
        });
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(active.clone());
        Registration {
            reads: self,
            active,
        }
    }

    pub(in crate::sqlite) fn checkpoint(
        &self,
        data: &DataDir,
        sync: Synchronous,
        state: &FailureState,
    ) -> Result<CheckpointObservation> {
        state.ensure_available()?;
        let result = (|| {
            // A checkpoint never runs on the writer or a connection held by a read.
            let conn = open_connection(data, ConnectionRole::Checkpoint, sync)?;
            let page_size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
            let (log, copied) =
                conn.wal_checkpoint_v2(Some("main"), rusqlite::wal::CheckpointMode::PASSIVE)?;
            // SQLite returns -1 for a non-WAL database. No backlog is inferred from it.
            let backlog = if log >= 0 && copied >= 0 {
                (log.saturating_sub(copied) as u64).saturating_mul(page_size.max(0) as u64)
            } else {
                0
            };
            Ok(self.apply(log, copied, backlog, Instant::now()))
        })();
        if let Err(error) = &result {
            state.observe(FailureSource::Checkpoint, error);
        }
        result
    }

    fn apply(
        &self,
        log_frames: i32,
        checkpointed_frames: i32,
        backlog_bytes: u64,
        now: Instant,
    ) -> CheckpointObservation {
        let reads = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut observation = CheckpointObservation {
            log_frames,
            checkpointed_frames,
            backlog_bytes,
            oldest_read: None,
            interrupted: 0,
        };
        for read in reads.iter() {
            let age = now.saturating_duration_since(
                *read
                    .started
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            observation.oldest_read = Some(observation.oldest_read.map_or(age, |old| old.max(age)));
            if backlog_bytes > MAX_BACKLOG
                && age > MAX_AGE
                && !read.expired.swap(true, Ordering::SeqCst)
            {
                read.interrupt.interrupt();
                observation.interrupted += 1;
            }
        }
        observation
    }

    #[cfg(test)]
    pub(in crate::sqlite) fn age_for_test(&self, age: Duration) {
        for read in self.0.lock().unwrap().iter() {
            *read.started.lock().unwrap() = Instant::now() - age;
        }
    }
}

pub(super) struct Registration<'a> {
    reads: &'a ActiveReads,
    active: Arc<Active>,
}
impl Registration<'_> {
    pub(super) fn finish(self) -> bool {
        let mut reads = self
            .reads
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reads.retain(|read| !Arc::ptr_eq(&read.expired, &self.active.expired));
        self.active.expired.load(Ordering::SeqCst)
    }
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        // Removal and interruption share a lock, so an expired registration can
        // never interrupt a later transaction on a reused connection.
        self.reads
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|read| !Arc::ptr_eq(&read.expired, &self.active.expired));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_limits_do_not_expire_and_interruption_counts_once() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let tx = conn.transaction().unwrap();
        let reads = ActiveReads::default();
        let expired = Arc::new(AtomicBool::new(false));
        let registration = reads.register(&tx, expired.clone());
        let started = *registration.active.started.lock().unwrap();
        assert_eq!(
            reads
                .apply(1, 0, MAX_BACKLOG + 1, started + MAX_AGE)
                .interrupted,
            0
        );
        assert_eq!(
            reads
                .apply(
                    1,
                    0,
                    MAX_BACKLOG,
                    started + MAX_AGE + Duration::from_nanos(1)
                )
                .interrupted,
            0
        );
        assert_eq!(
            reads
                .apply(
                    1,
                    0,
                    MAX_BACKLOG + 1,
                    started + MAX_AGE + Duration::from_nanos(1)
                )
                .interrupted,
            1
        );
        assert!(expired.load(Ordering::SeqCst));
        assert_eq!(
            reads
                .apply(
                    1,
                    0,
                    MAX_BACKLOG + 1,
                    started + MAX_AGE + Duration::from_secs(1)
                )
                .interrupted,
            0
        );
        assert!(registration.finish());
        assert!(reads.0.lock().unwrap().is_empty());
    }
}
