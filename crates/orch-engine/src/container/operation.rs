//! The project-wide operation boundary shared by daemon requests and engine work.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use orch_domain::OrchestratorEvent;
use tokio::sync::{OwnedMutexGuard, broadcast};

use crate::events::EventBus;

tokio::task_local! {
    static OPERATION: Arc<OperationContext>;
    static TRACKER: (usize, Arc<AtomicU64>);
}

struct OperationContext {
    boundary: usize,
    changed: AtomicBool,
    events: Mutex<Vec<(EventBus, OrchestratorEvent)>>,
}

/// Complete operations acquire this boundary once; nested service calls reuse
/// the task-local context. A spawned task starts with no inherited context.
#[derive(Default)]
pub struct OperationBoundary {
    gate: Arc<tokio::sync::Mutex<()>>,
    commit_seq: AtomicU64,
    commits: Mutex<Option<broadcast::Sender<u64>>>,
    publication: Mutex<()>,
}

/// Value returned by an operation and the instance-local ordinal it observed.
pub struct OperationOutcome<T> {
    pub value: T,
    pub commit_seq: u64,
}

struct OperationGuard<'a> {
    boundary: &'a OperationBoundary,
    context: Arc<OperationContext>,
    permit: Option<OwnedMutexGuard<()>>,
    assigned_seq: u64,
}

impl OperationGuard<'_> {
    fn finish(&mut self) -> u64 {
        if self.permit.is_none() {
            return self.assigned_seq;
        }
        let publication = self
            .boundary
            .publication
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let changed = self.context.changed.load(Ordering::SeqCst);
        let seq = if changed {
            self.boundary.commit_seq.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.boundary.current_commit_seq()
        };
        let events = std::mem::take(
            &mut *self
                .context
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        // Synchronous publication immediately after release preserves commit
        // order without running callbacks under the project permit.
        self.permit.take();
        if changed
            && let Some(commits) = self
                .boundary
                .commits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
        {
            let _ = commits.send(seq);
        }
        self.assigned_seq = seq;
        let _ = TRACKER.try_with(|(identity, latest)| {
            if *identity == self.boundary.identity() {
                latest.store(seq, Ordering::SeqCst);
            }
        });
        drop(publication);
        for (bus, event) in events {
            bus.emit_immediate(event);
        }
        seq
    }
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

impl OperationBoundary {
    pub fn new() -> Self {
        Self::default()
    }

    fn identity(&self) -> usize {
        std::ptr::from_ref(self) as usize
    }

    /// Run a whole operation. Mark potential mutations before their first
    /// write, so failed partial writes and cancelled engine tasks invalidate.
    pub async fn run<T, F>(&self, mutation: bool, future: F) -> OperationOutcome<T>
    where
        F: Future<Output = T>,
    {
        if OPERATION
            .try_with(|context| context.boundary == self.identity())
            .unwrap_or(false)
        {
            if mutation {
                self.mark_changed();
            }
            return OperationOutcome {
                value: future.await,
                commit_seq: self.current_commit_seq(),
            };
        }

        let permit = Arc::clone(&self.gate).lock_owned().await;
        let context = Arc::new(OperationContext {
            boundary: self.identity(),
            changed: AtomicBool::new(mutation),
            events: Mutex::new(Vec::new()),
        });
        let mut guard = OperationGuard {
            boundary: self,
            context: Arc::clone(&context),
            permit: Some(permit),
            assigned_seq: 0,
        };
        let value = OPERATION.scope(context, future).await;
        let commit_seq = guard.finish();
        OperationOutcome { value, commit_seq }
    }

    /// Track the exact last phase committed by this task without keeping a
    /// permit across external work. Spawned collectors do not inherit it.
    pub async fn track<T, F>(&self, future: F) -> OperationOutcome<T>
    where
        F: Future<Output = T>,
    {
        let latest = Arc::new(AtomicU64::new(self.current_commit_seq()));
        let value = TRACKER
            .scope((self.identity(), Arc::clone(&latest)), future)
            .await;
        OperationOutcome {
            value,
            commit_seq: latest.load(Ordering::SeqCst),
        }
    }

    /// Called by operation-aware adapters before a potentially partial write.
    pub fn mark_changed(&self) {
        let _ = OPERATION.try_with(|context| {
            if context.boundary == self.identity() {
                context.changed.store(true, Ordering::SeqCst);
            }
        });
    }

    pub fn current_commit_seq(&self) -> u64 {
        self.commit_seq.load(Ordering::SeqCst)
    }

    /// Coarse invalidations, including writes made by internal engine work.
    /// A lagged subscriber must refetch its views.
    pub fn subscribe_commits(&self) -> broadcast::Receiver<u64> {
        self.commits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(|| broadcast::channel(4096).0)
            .subscribe()
    }
}

pub(crate) fn buffer_event(bus: &EventBus, event: OrchestratorEvent) -> Option<OrchestratorEvent> {
    let mut event = Some(event);
    let _ = OPERATION.try_with(|context| {
        context
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((bus.clone(), event.take().expect("event buffered once")));
    });
    event
}
