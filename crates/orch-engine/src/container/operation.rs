//! The project-wide operation boundary shared by daemon requests and engine work.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use orch_domain::OrchestratorEvent;
use tokio::sync::{OwnedMutexGuard, broadcast};

use super::commit::*;
use crate::events::EventBus;

tokio::task_local! {
    static OPERATION: Arc<OperationContext>;
    static SOURCE: (usize, CommitSource);
    static TRACKER: (usize, Arc<AtomicU64>);
}

struct OperationContext {
    boundary: usize,
    changed: AtomicBool,
    unattributed: AtomicBool,
    source: CommitSource,
    metadata: Mutex<PendingChanges>,
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
    publisher: Mutex<Option<Publisher>>,
    store_operations: AtomicBool,
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
        if changed {
            let metadata = std::mem::take(
                &mut *self
                    .context
                    .metadata
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            );
            self.boundary.publish(&Publication::Commit(
                metadata.finish(seq, self.context.source),
            ));
        }
        for (_, event) in &events {
            self.boundary
                .publish(&Publication::Bus(Arc::new(event.clone())));
        }
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

    /// Install the daemon's nonblocking publication adapter.
    pub fn set_publisher(&self, publisher: Option<Publisher>) {
        self.enable_store_operations();
        *self.publisher.lock().unwrap_or_else(|e| e.into_inner()) = publisher;
    }

    /// Daemon writers all use this boundary. Legacy in-process watch mode
    /// retains its existing state-lock ordering until it is removed in PR 4.
    pub fn enable_store_operations(&self) {
        self.store_operations.store(true, Ordering::SeqCst);
    }

    pub(super) async fn store_write<T>(
        &self,
        future: impl Future<Output = T>,
    ) -> OperationOutcome<T> {
        let nested = OPERATION
            .try_with(|context| context.boundary == self.identity())
            .unwrap_or(false);
        if nested || self.store_operations.load(Ordering::SeqCst) {
            self.run(false, future).await
        } else {
            OperationOutcome {
                value: future.await,
                commit_seq: self.current_commit_seq(),
            }
        }
    }

    /// Record identity-only metadata before a potentially partial write.
    pub fn record_change(&self, change: CommitChange) {
        let _ = OPERATION.try_with(|context| {
            if context.boundary == self.identity() {
                context.changed.store(true, Ordering::SeqCst);
                context
                    .metadata
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record(change);
            }
        });
    }

    /// Record a known store when exact entities are unavailable.
    pub fn note_store(&self, store: CommitStore) {
        let _ = OPERATION.try_with(|context| {
            if context.boundary == self.identity() {
                context.changed.store(true, Ordering::SeqCst);
                context
                    .metadata
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .note(store);
            }
        });
    }

    /// Only explicit maintenance that proved it performed no writes may
    /// retract its conservative note. Other writes and uncertainty survive.
    pub(super) fn discard_noop_store(&self, store: CommitStore) {
        let _ = OPERATION.try_with(|context| {
            if context.boundary == self.identity() {
                let remaining = context
                    .metadata
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .discard_noop_store(store);
                context.changed.store(
                    remaining || context.unattributed.load(Ordering::SeqCst),
                    Ordering::SeqCst,
                );
            }
        });
    }

    pub async fn with_source<T>(&self, source: CommitSource, future: impl Future<Output = T>) -> T {
        SOURCE.scope((self.identity(), source), future).await
    }

    /// Scope provenance across a phase without inheriting it in spawned tasks.
    pub async fn run_from<T>(
        &self,
        source: CommitSource,
        mutation: bool,
        future: impl Future<Output = T>,
    ) -> OperationOutcome<T> {
        self.with_source(source, self.run(mutation, future)).await
    }

    fn publish(&self, publication: &Publication) {
        let publisher = self
            .publisher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(publisher) = publisher {
            publisher(publication);
        }
    }

    pub(crate) fn publish_bus(&self, event: &OrchestratorEvent) {
        let _publication = self.publication.lock().unwrap_or_else(|e| e.into_inner());
        self.publish(&Publication::Bus(Arc::new(event.clone())));
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
            unattributed: AtomicBool::new(mutation),
            source: SOURCE
                .try_with(|(boundary, source)| {
                    if *boundary == self.identity() {
                        *source
                    } else {
                        CommitSource::Internal
                    }
                })
                .unwrap_or_default(),
            metadata: Mutex::new(PendingChanges::default()),
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
                context.unattributed.store(true, Ordering::SeqCst);
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

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn capture(boundary: &OperationBoundary) -> Arc<Mutex<Vec<CommitRecord>>> {
        let records = Arc::new(Mutex::new(Vec::new()));
        let sink = records.clone();
        boundary.set_publisher(Some(Arc::new(move |publication| {
            if let Publication::Commit(record) = publication {
                sink.lock().unwrap().push(record.clone());
            }
        })));
        records
    }

    #[tokio::test]
    async fn nested_metadata_is_published_once_after_release() {
        let boundary = Arc::new(OperationBoundary::new());
        let records = capture(&boundary);
        let sink = records.clone();
        let weak = Arc::downgrade(&boundary);
        boundary.set_publisher(Some(Arc::new(move |publication| {
            if let Publication::Commit(record) = publication {
                assert!(
                    weak.upgrade().unwrap().gate.try_lock().is_ok(),
                    "callback ran under project permit"
                );
                sink.lock().unwrap().push(record.clone());
            }
        })));
        boundary
            .run(false, async {
                boundary
                    .run(true, async {
                        boundary.record_change(CommitChange {
                            store: CommitStore::Tasks,
                            id: Some("task_one".into()),
                            action: CommitAction::Deleted,
                            old: Some(Membership {
                                goal_id: Some("goal_old".into()),
                                ..Default::default()
                            }),
                            new: None,
                        });
                        assert!(records.lock().unwrap().is_empty());
                    })
                    .await;
            })
            .await;
        let records = records.lock().unwrap();
        assert_eq!(
            records.len(),
            1,
            "one outer operation must publish one typed record"
        );
        assert_eq!(records[0].commit_seq, 1);
        assert!(!records[0].store_set_uncertain);
        assert_eq!(
            records[0].changes[0]
                .old
                .as_ref()
                .unwrap()
                .goal_id
                .as_deref(),
            Some("goal_old")
        );
        assert_eq!(records[0].changes[0].action, CommitAction::Deleted);
        assert!(
            boundary.gate.try_lock().is_ok(),
            "publication never retains the permit"
        );
    }

    #[tokio::test]
    async fn partial_and_cancelled_operations_retain_metadata() {
        let boundary = Arc::new(OperationBoundary::new());
        let records = capture(&boundary);
        boundary
            .run(true, async { Err::<(), _>("possible partial write") })
            .await;
        boundary
            .run(true, async {
                boundary.note_store(CommitStore::Config);
                Err::<(), _>("known partial write")
            })
            .await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn({
            let boundary = boundary.clone();
            let entered = entered.clone();
            async move {
                boundary
                    .run(true, async {
                        boundary.note_store(CommitStore::State);
                        entered.notify_one();
                        std::future::pending::<()>().await;
                    })
                    .await
            }
        });
        entered.notified().await;
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        let records = records.lock().unwrap();
        assert_eq!(
            records.len(),
            3,
            "errors and cancellation must publish possible writes"
        );
        assert!(records[0].store_set_uncertain);
        assert!(!records[1].store_set_uncertain);
        assert_eq!(
            records[1].changes,
            vec![CommitChange::store_wide(CommitStore::Config)]
        );
        assert_eq!(
            records[2].changes,
            vec![CommitChange::store_wide(CommitStore::State)]
        );
    }

    #[tokio::test]
    async fn metadata_overflow_keeps_known_stores_without_unbounded_id_history() {
        let boundary = OperationBoundary::new();
        let records = capture(&boundary);
        boundary
            .run(false, async {
                for n in 0..1000 {
                    boundary.record_change(CommitChange {
                        store: CommitStore::Tasks,
                        id: Some(format!("task_{n}")),
                        action: CommitAction::Created,
                        old: None,
                        new: Some(Membership::default()),
                    });
                }
                boundary.note_store(CommitStore::Agents);
            })
            .await;
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].store_set_uncertain);
        assert_eq!(
            records[0].changes,
            vec![
                CommitChange::store_wide(CommitStore::Tasks),
                CommitChange::store_wide(CommitStore::Agents)
            ]
        );
    }

    #[tokio::test]
    async fn metadata_byte_overflow_degrades_to_store_wide() {
        let boundary = OperationBoundary::new();
        let records = capture(&boundary);
        boundary
            .run(false, async {
                boundary.record_change(CommitChange {
                    store: CommitStore::Context,
                    id: Some("x".repeat(128 * 1024)),
                    action: CommitAction::Updated,
                    old: None,
                    new: None,
                });
            })
            .await;
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].changes,
            vec![CommitChange::store_wide(CommitStore::Context)]
        );
    }

    #[tokio::test]
    async fn source_is_boundary_specific_and_not_inherited_by_spawned_collectors() {
        let boundary = Arc::new(OperationBoundary::new());
        let records = capture(&boundary);
        boundary
            .with_source(CommitSource::Tick, async {
                boundary.run(true, async {}).await;
                tokio::spawn({
                    let boundary = boundary.clone();
                    async move { boundary.run(true, async {}).await }
                })
                .await
                .unwrap();
                tokio::spawn({
                    let boundary = boundary.clone();
                    async move {
                        boundary
                            .with_source(CommitSource::Collector, boundary.run(true, async {}))
                            .await
                    }
                })
                .await
                .unwrap();
                boundary.run(true, async {}).await;
            })
            .await;
        boundary.run(true, async {}).await;
        let other = OperationBoundary::new();
        let other_records = capture(&other);
        boundary
            .with_source(CommitSource::Peer, other.run(true, async {}))
            .await;
        assert_eq!(
            other_records.lock().unwrap()[0].source,
            CommitSource::Internal
        );
        let records = records.lock().unwrap();
        assert_eq!(
            records.iter().map(|r| r.source).collect::<Vec<_>>(),
            vec![
                CommitSource::Tick,
                CommitSource::Internal,
                CommitSource::Collector,
                CommitSource::Tick,
                CommitSource::Internal
            ]
        );
    }
}
