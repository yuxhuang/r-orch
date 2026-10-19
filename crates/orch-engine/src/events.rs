//! The event bus.
//!
//! The bus is **synchronous**: `emit()` runs every handler inline and
//! swallows their exceptions. Two layers serve that contract here, because two
//! different consumers need two different things:
//!
//! 1. **Synchronous handlers** ([`EventBus::on`], [`EventBus::once`],
//!    [`EventBus::on_any`]) — used by the orchestrator's own
//!    reactive-dispatch subscription and by every test that asserts on event
//!    payloads. Handler panics are caught so one bad subscriber cannot break
//!    the chain, as a `catch` around each handler would.
//! 2. **A `broadcast` channel** ([`EventBus::subscribe`]) — cheap fan-out to
//!    the TUI, the structured logger and the run-store writer, which consume on
//!    their own tasks (ARCHITECTURE §6.5).
//!
//! Two behaviour differences, stated plainly:
//!
//! - `broadcast` **drops** messages for a lagging receiver where synchronous
//!   emission cannot lose an event. The channel is sized generously and
//!   `Lagged` is surfaced as a `tracing::warn`, never silently ignored. A
//!   subscriber that must not miss events consumes on a dedicated task.
//! - `emit` snapshots the handler list before dispatching, so a handler that
//!   subscribes during dispatch does not receive the in-flight event. JS `Set`
//!   iteration *would* visit it. Snapshotting is what makes [`EventBus::once`]
//!   able to unsubscribe itself from inside its own call, and nothing in the
//!   suite depends on the other reading.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard};

use orch_domain::{OrchestratorEvent, OrchestratorEventType};
use tokio::sync::broadcast;

/// Channel capacity. Generous because the lossy case is the thing to avoid
/// (ARCHITECTURE §6.5).
pub const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// The default `max_listeners` — the threshold above which a repeated
/// subscription is reported as a probable leak.
pub const DEFAULT_MAX_LISTENERS: usize = 10;

/// A synchronous event handler.
///
/// `Fn`, not `FnMut`: handlers are shared and run from `&self`, so any state
/// they need lives behind their own interior mutability — which is also how
/// closures capture it.
pub type EventHandler = Arc<dyn Fn(&OrchestratorEvent) + Send + Sync>;

#[derive(Default)]
struct Registry {
    typed: HashMap<OrchestratorEventType, Vec<(u64, EventHandler)>>,
    wildcard: Vec<(u64, EventHandler)>,
    next_id: u64,
    max_listeners: usize,
    /// Which event types have already produced a leak warning. `"*"` stands
    /// for the wildcard bucket.
    warned: HashSet<String>,
}

impl Registry {
    fn new() -> Self {
        Registry {
            max_listeners: DEFAULT_MAX_LISTENERS,
            ..Registry::default()
        }
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Warn at most once per bucket, and never when `max_listeners` is 0 —
    /// which means "unlimited".
    fn maybe_warn(&mut self, bucket: &str, count: usize) -> Option<String> {
        if self.max_listeners == 0 || count <= self.max_listeners {
            return None;
        }
        if !self.warned.insert(bucket.to_owned()) {
            return None;
        }
        Some(format!(
            "EventBus: possible memory leak detected. {count} listeners added for \"{bucket}\". \
             Use set_max_listeners() to increase limit if this is intentional."
        ))
    }
}

/// The single communication channel between all layers.
///
/// `Clone` shares one registry and one channel, so cloning behaves like
/// passing one shared bus reference around. The container nonetheless hands out
/// `Arc<EventBus>`, which is what every service holds.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Arc<OrchestratorEvent>>,
    registry: Arc<Mutex<Registry>>,
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registry = lock(&self.registry);
        f.debug_struct("EventBus")
            .field("typed_listeners", &registry.typed.len())
            .field("wildcard_listeners", &registry.wildcard.len())
            .field("stream_subscribers", &self.tx.receiver_count())
            .finish()
    }
}

/// Mutex poisoning carries no information here: the registry holds handler
/// pointers and counters, and a panicking handler is already caught. Recovering
/// the guard keeps a real failure visible instead of masking it with a poison
/// error.
fn lock(registry: &Arc<Mutex<Registry>>) -> MutexGuard<'_, Registry> {
    registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl EventBus {
    pub fn new() -> Self {
        EventBus::with_capacity(EVENT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        EventBus {
            tx,
            registry: Arc::new(Mutex::new(Registry::new())),
        }
    }

    /// Emit an event to every synchronous handler and to every stream
    /// subscriber.
    ///
    /// The channel send happens **first** so that the channel's order is emit
    /// order even when a handler emits a follow-up event; the typed handlers
    /// then run before the wildcard ones, which is the dispatch order.
    /// An event emitted with no subscribers is dropped.
    pub fn emit(&self, event: OrchestratorEvent) {
        let event = Arc::new(event);
        let _ = self.tx.send(Arc::clone(&event));

        let (typed, wildcard) = {
            let registry = lock(&self.registry);
            let typed: Vec<EventHandler> = registry
                .typed
                .get(&event.event_type())
                .map(|handlers| handlers.iter().map(|(_, h)| Arc::clone(h)).collect())
                .unwrap_or_default();
            let wildcard = registry
                .wildcard
                .iter()
                .map(|(_, h)| Arc::clone(h))
                .collect::<Vec<_>>();
            (typed, wildcard)
        };

        for handler in typed {
            dispatch(&handler, &event, "handler");
        }
        for handler in wildcard {
            dispatch(&handler, &event, "wildcard handler");
        }
    }

    /// Subscribe to one event type. Call [`Unsubscribe::unsubscribe`] on the
    /// returned handle to detach; dropping it leaves the handler attached,
    /// so the returned handle must be used explicitly.
    pub fn on(
        &self,
        event_type: OrchestratorEventType,
        handler: impl Fn(&OrchestratorEvent) + Send + Sync + 'static,
    ) -> Unsubscribe {
        let mut registry = lock(&self.registry);
        let id = registry.take_id();
        let handlers = registry.typed.entry(event_type).or_default();
        handlers.push((id, Arc::new(handler)));
        let count = handlers.len();
        let warning = registry.maybe_warn(event_type.as_str(), count);
        drop(registry);

        if let Some(warning) = warning {
            tracing::warn!("{warning}");
        }
        Unsubscribe {
            registry: Arc::clone(&self.registry),
            target: Target::Typed(event_type),
            id,
        }
    }

    /// Subscribe to one event type and detach after the first delivery.
    ///
    /// The wrapper unsubscribes **before** invoking the inner handler, so a
    /// handler that re-emits the same type does not re-enter itself.
    pub fn once(
        &self,
        event_type: OrchestratorEventType,
        handler: impl Fn(&OrchestratorEvent) + Send + Sync + 'static,
    ) -> Unsubscribe {
        let registry = Arc::clone(&self.registry);
        // The id is only known after registration, so the wrapper reads it
        // from a slot the registration fills in.
        let slot = Arc::new(Mutex::new(None::<u64>));
        let wrapper = {
            let slot = Arc::clone(&slot);
            let registry = Arc::clone(&registry);
            move |event: &OrchestratorEvent| {
                let id = slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                if let Some(id) = id {
                    remove(&registry, &Target::Typed(event_type), id);
                }
                handler(event);
            }
        };

        let handle = self.on(event_type, wrapper);
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle.id);
        handle
    }

    /// Subscribe to every event type.
    pub fn on_any(
        &self,
        handler: impl Fn(&OrchestratorEvent) + Send + Sync + 'static,
    ) -> Unsubscribe {
        let mut registry = lock(&self.registry);
        let id = registry.take_id();
        registry.wildcard.push((id, Arc::new(handler)));
        let count = registry.wildcard.len();
        let warning = registry.maybe_warn("*", count);
        drop(registry);

        if let Some(warning) = warning {
            tracing::warn!("{warning}");
        }
        Unsubscribe {
            registry: Arc::clone(&self.registry),
            target: Target::Wildcard,
            id,
        }
    }

    /// Detach a handler. Equivalent to [`Unsubscribe::unsubscribe`] and
    /// present as the conventional `off` spelling.
    pub fn off(&self, handle: Unsubscribe) {
        handle.unsubscribe();
    }

    /// Remove every handler and reset the leak-warning state, so a warning for
    /// a type that was previously warned about fires again.
    pub fn clear(&self) {
        let mut registry = lock(&self.registry);
        registry.typed.clear();
        registry.wildcard.clear();
        registry.warned.clear();
    }

    /// Listeners attached to one event type. Wildcard handlers are **not**
    /// counted.
    pub fn listener_count(&self, event_type: OrchestratorEventType) -> usize {
        lock(&self.registry)
            .typed
            .get(&event_type)
            .map_or(0, Vec::len)
    }

    pub fn wildcard_listener_count(&self) -> usize {
        lock(&self.registry).wildcard.len()
    }

    pub fn max_listeners(&self) -> usize {
        lock(&self.registry).max_listeners
    }

    /// Set the per-type listener threshold. `0` means unlimited.
    pub fn set_max_listeners(&self, n: usize) {
        lock(&self.registry).max_listeners = n;
    }

    /// How many distinct buckets have produced a leak warning.
    ///
    /// This is the size of the warned set, exposed because "warn
    /// once per type" is the asserted behaviour and a `tracing` subscriber is
    /// a heavier way to observe the same fact.
    pub fn warned_type_count(&self) -> usize {
        lock(&self.registry).warned.len()
    }

    /// A streaming receiver. Independent of the synchronous handlers.
    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            rx: self.tx.subscribe(),
            skipped: 0,
        }
    }

    /// Live stream-subscriber count.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// Run one handler, containing a panic so it cannot break the chain. The
/// panic is logged, because swallowing it silently would hide a real defect.
fn dispatch(handler: &EventHandler, event: &OrchestratorEvent, label: &str) {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| handler(event)));
    if result.is_err() {
        tracing::error!(
            event = %event.event_type(),
            "EventBus {label} panicked",
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Typed(OrchestratorEventType),
    Wildcard,
}

fn remove(registry: &Arc<Mutex<Registry>>, target: &Target, id: u64) {
    let mut registry = lock(registry);
    match target {
        Target::Typed(event_type) => {
            if let Some(handlers) = registry.typed.get_mut(event_type) {
                handlers.retain(|(handler_id, _)| *handler_id != id);
            }
        }
        Target::Wildcard => registry
            .wildcard
            .retain(|(handler_id, _)| *handler_id != id),
    }
}

/// A handle that detaches one synchronous handler.
///
/// Deliberately **not** a `Drop` guard: `let _ = bus.on(…)` would then
/// unsubscribe immediately; the handle's `unsubscribe` is called explicitly.
#[must_use = "the handler stays attached until `unsubscribe` is called"]
pub struct Unsubscribe {
    registry: Arc<Mutex<Registry>>,
    target: Target,
    id: u64,
}

impl std::fmt::Debug for Unsubscribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unsubscribe")
            .field("target", &self.target)
            .field("id", &self.id)
            .finish()
    }
}

impl Unsubscribe {
    /// Detach the handler. Calling it twice is harmless.
    pub fn unsubscribe(self) {
        remove(&self.registry, &self.target, self.id);
    }
}

/// A receiving end of the bus's broadcast channel.
#[derive(Debug)]
pub struct EventSubscription {
    rx: broadcast::Receiver<Arc<OrchestratorEvent>>,
    skipped: u64,
}

impl EventSubscription {
    /// The next event, or `None` once the bus is dropped.
    ///
    /// A lag is logged and counted, and the stream continues from the oldest
    /// retained event, which is what a lossy fan-out means in practice.
    pub async fn recv(&mut self) -> Option<Arc<OrchestratorEvent>> {
        loop {
            match self.rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => self.note_lag(skipped),
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Drain everything buffered without waiting. Used by the CLI, which
    /// emits and then reports in one process.
    pub fn try_drain(&mut self) -> Vec<Arc<OrchestratorEvent>> {
        let mut events = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(event) => events.push(event),
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => self.note_lag(skipped),
                Err(_) => return events,
            }
        }
    }

    /// How many events this subscriber has missed, in total.
    ///
    /// A lossy fan-out is fine for a dashboard feed, but a caller that waits
    /// for *one specific* event — `run <id>` waiting for its own
    /// `agent:completed` — would wait forever if that event were among the
    /// dropped ones. The synchronous handlers cannot hit this because
    /// a handler cannot lag, but this channel can, so the count is exposed
    /// rather than only logged. A caller that must not miss an event compares this across calls and falls back to the store
    /// when it grows.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    fn note_lag(&mut self, skipped: u64) {
        self.skipped = self.skipped.saturating_add(skipped);
        tracing::warn!(
            skipped,
            total = self.skipped,
            "event bus subscriber lagged; events were dropped"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{Task, TaskStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn status_changed(task_id: &str) -> OrchestratorEvent {
        OrchestratorEvent::TaskStatusChanged {
            task_id: task_id.into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        }
    }

    fn task_created() -> OrchestratorEvent {
        OrchestratorEvent::TaskCreated {
            task: Box::new(Task {
                id: "tsk_1".into(),
                title: "t".into(),
                description: String::new(),
                status: TaskStatus::Todo,
                priority: 3,
                assignee: None,
                labels: vec![],
                depends_on: vec![],
                created_at: "t0".into(),
                updated_at: "t0".into(),
                attempts: 0,
                max_attempts: 3,
                workspace_mode: None,
                workspace: None,
                proof: None,
                review_criteria: None,
                review_results: None,
                scope: None,
                feedback: None,
                goal_id: None,
                attachments: None,
            }),
        }
    }

    /// A counter a handler can bump, so a test can assert delivery without a
    /// mocking framework.
    fn counter() -> (
        Arc<AtomicUsize>,
        impl Fn(&OrchestratorEvent) + Send + Sync + 'static,
    ) {
        let count = Arc::new(AtomicUsize::new(0));
        let handler = {
            let count = Arc::clone(&count);
            move |_: &OrchestratorEvent| {
                count.fetch_add(1, Ordering::SeqCst);
            }
        };
        (count, handler)
    }

    fn noop(_: &OrchestratorEvent) {}

    // ── on / emit ────────────────────────────────────────────────────────

    #[test]
    fn delivers_events_to_matching_handler() {
        let bus = EventBus::new();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let _unsub = bus.on(OrchestratorEventType::TaskStatusChanged, move |event| {
            if let OrchestratorEvent::TaskStatusChanged { task_id, .. } = event {
                sink.lock().unwrap().push(task_id.clone());
            }
        });

        bus.emit(status_changed("tsk_1"));
        assert_eq!(*seen.lock().unwrap(), vec!["tsk_1".to_owned()]);
    }

    #[test]
    fn does_not_deliver_events_to_non_matching_handlers() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let _unsub = bus.on(OrchestratorEventType::TaskCreated, handler);

        bus.emit(status_changed("tsk_1"));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn supports_multiple_handlers_for_the_same_event_type() {
        let bus = EventBus::new();
        let (first, handler_a) = counter();
        let (second, handler_b) = counter();
        let _a = bus.on(OrchestratorEventType::TaskCreated, handler_a);
        let _b = bus.on(OrchestratorEventType::TaskCreated, handler_b);

        bus.emit(task_created());
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn returns_an_unsubscribe_handle() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let unsub = bus.on(OrchestratorEventType::TaskCreated, handler);

        bus.emit(task_created());
        unsub.unsubscribe();
        bus.emit(task_created());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    // ── off ──────────────────────────────────────────────────────────────

    #[test]
    fn off_removes_a_specific_handler() {
        let bus = EventBus::new();
        let (kept, handler_a) = counter();
        let (removed, handler_b) = counter();
        let _a = bus.on(OrchestratorEventType::TaskCreated, handler_a);
        let b = bus.on(OrchestratorEventType::TaskCreated, handler_b);

        bus.off(b);
        bus.emit(task_created());
        assert_eq!(kept.load(Ordering::SeqCst), 1);
        assert_eq!(removed.load(Ordering::SeqCst), 0);
        assert_eq!(bus.listener_count(OrchestratorEventType::TaskCreated), 1);
    }

    // ── once ─────────────────────────────────────────────────────────────

    #[test]
    fn once_fires_only_once_then_auto_unsubscribes() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let _unsub = bus.once(OrchestratorEventType::TaskCreated, handler);

        bus.emit(task_created());
        bus.emit(task_created());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(bus.listener_count(OrchestratorEventType::TaskCreated), 0);
    }

    #[test]
    fn once_can_be_unsubscribed_before_the_first_call() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let unsub = bus.once(OrchestratorEventType::TaskCreated, handler);

        unsub.unsubscribe();
        bus.emit(task_created());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    // ── on_any ───────────────────────────────────────────────────────────

    #[test]
    fn on_any_receives_all_event_types() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let _unsub = bus.on_any(handler);

        bus.emit(task_created());
        bus.emit(status_changed("tsk_1"));
        bus.emit(OrchestratorEvent::OrchestratorTick {
            running: 0,
            queued: 0,
        });
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn on_any_unsubscribe_removes_from_all_event_types() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let unsub = bus.on_any(handler);

        bus.emit(task_created());
        unsub.unsubscribe();
        bus.emit(task_created());
        bus.emit(status_changed("tsk_1"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(bus.wildcard_listener_count(), 0);
    }

    #[test]
    fn on_any_subscribes_to_orchestrator_error_and_shutdown() {
        // Both reach a wildcard subscriber, because that is how `serve --json`
        // logs them.
        let bus = EventBus::new();
        let seen: Arc<Mutex<Vec<OrchestratorEventType>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let _unsub = bus.on_any(move |event| sink.lock().unwrap().push(event.event_type()));

        bus.emit(OrchestratorEvent::OrchestratorError {
            error: "boom".into(),
            context: "tick".into(),
            fatal: false,
        });
        bus.emit(OrchestratorEvent::OrchestratorShutdown {
            reason: "Received SIGINT".into(),
        });
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                OrchestratorEventType::OrchestratorError,
                OrchestratorEventType::OrchestratorShutdown,
            ]
        );
    }

    // ── error handling ───────────────────────────────────────────────────

    #[test]
    fn a_panicking_handler_does_not_break_the_event_chain() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let _bad = bus.on(OrchestratorEventType::TaskCreated, |_| {
            panic!("handler blew up");
        });
        let _good = bus.on(OrchestratorEventType::TaskCreated, handler);

        bus.emit(task_created());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // And the bus still works afterwards.
        bus.emit(task_created());
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    // ── task:orphaned ────────────────────────────────────────────────────

    #[test]
    fn delivers_task_orphaned_to_typed_and_wildcard_subscribers() {
        let bus = EventBus::new();
        let (typed, typed_handler) = counter();
        let (any, any_handler) = counter();
        let _a = bus.on(OrchestratorEventType::TaskOrphaned, typed_handler);
        let _b = bus.on_any(any_handler);

        bus.emit(OrchestratorEvent::TaskOrphaned {
            task_id: "tsk_orphan".into(),
        });
        assert_eq!(typed.load(Ordering::SeqCst), 1);
        assert_eq!(any.load(Ordering::SeqCst), 1);
    }

    // ── clear ────────────────────────────────────────────────────────────

    #[test]
    fn clear_removes_all_handlers() {
        let bus = EventBus::new();
        let (typed, typed_handler) = counter();
        let (any, any_handler) = counter();
        let _a = bus.on(OrchestratorEventType::TaskCreated, typed_handler);
        let _b = bus.on_any(any_handler);

        bus.clear();
        bus.emit(task_created());
        assert_eq!(typed.load(Ordering::SeqCst), 0);
        assert_eq!(any.load(Ordering::SeqCst), 0);
    }

    // ── maxListeners ─────────────────────────────────────────────────────

    #[test]
    fn max_listeners_defaults_to_10() {
        assert_eq!(EventBus::new().max_listeners(), 10);
    }

    #[test]
    fn max_listeners_can_be_changed() {
        let bus = EventBus::new();
        bus.set_max_listeners(20);
        assert_eq!(bus.max_listeners(), 20);
    }

    #[test]
    fn warns_when_the_listener_count_exceeds_max_listeners() {
        let bus = EventBus::new();
        bus.set_max_listeners(3);
        for _ in 0..4 {
            let _unsub = bus.on(OrchestratorEventType::TaskCreated, noop);
        }
        assert_eq!(bus.warned_type_count(), 1);
    }

    #[test]
    fn warns_only_once_per_event_type() {
        let bus = EventBus::new();
        bus.set_max_listeners(2);
        for _ in 0..5 {
            let _unsub = bus.on(OrchestratorEventType::TaskCreated, noop);
        }
        assert_eq!(bus.warned_type_count(), 1);

        // A second type warns on its own.
        for _ in 0..5 {
            let _unsub = bus.on(OrchestratorEventType::TaskAssigned, noop);
        }
        assert_eq!(bus.warned_type_count(), 2);
    }

    #[test]
    fn does_not_warn_when_max_listeners_is_zero() {
        let bus = EventBus::new();
        bus.set_max_listeners(0);
        for _ in 0..50 {
            let _unsub = bus.on(OrchestratorEventType::TaskCreated, noop);
        }
        assert_eq!(bus.warned_type_count(), 0);
    }

    #[test]
    fn listener_count_returns_the_current_count() {
        let bus = EventBus::new();
        assert_eq!(bus.listener_count(OrchestratorEventType::TaskCreated), 0);
        let _a = bus.on(OrchestratorEventType::TaskCreated, noop);
        let _b = bus.on(OrchestratorEventType::TaskCreated, noop);
        assert_eq!(bus.listener_count(OrchestratorEventType::TaskCreated), 2);
    }

    #[test]
    fn clear_resets_warned_types_so_warnings_fire_again() {
        let bus = EventBus::new();
        bus.set_max_listeners(1);
        let _a = bus.on(OrchestratorEventType::TaskCreated, noop);
        let _b = bus.on(OrchestratorEventType::TaskCreated, noop);
        assert_eq!(bus.warned_type_count(), 1);

        bus.clear();
        assert_eq!(bus.warned_type_count(), 0);
        let _c = bus.on(OrchestratorEventType::TaskCreated, noop);
        let _d = bus.on(OrchestratorEventType::TaskCreated, noop);
        assert_eq!(bus.warned_type_count(), 1);
    }

    // ── leak bounds ───────────────────────────────────────────────────────

    #[test]
    fn a_thousand_subscribe_unsubscribe_cycles_leave_no_listeners() {
        let bus = EventBus::new();
        bus.set_max_listeners(0);
        for _ in 0..1000 {
            bus.on(OrchestratorEventType::TaskCreated, noop)
                .unsubscribe();
        }
        assert_eq!(bus.listener_count(OrchestratorEventType::TaskCreated), 0);
    }

    #[test]
    fn a_thousand_wildcard_subscribe_unsubscribe_cycles_do_not_leak() {
        let bus = EventBus::new();
        bus.set_max_listeners(0);
        for _ in 0..1000 {
            bus.on_any(noop).unsubscribe();
        }
        assert_eq!(bus.wildcard_listener_count(), 0);
    }

    #[test]
    fn once_does_not_accumulate_listeners() {
        let bus = EventBus::new();
        bus.set_max_listeners(0);
        for _ in 0..100 {
            let _unsub = bus.once(OrchestratorEventType::TaskCreated, noop);
            bus.emit(task_created());
        }
        assert_eq!(bus.listener_count(OrchestratorEventType::TaskCreated), 0);
    }

    // ── the broadcast layer ──────────────────────────────────────────────

    #[tokio::test]
    async fn a_subscriber_receives_events_in_order() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        bus.emit(status_changed("tsk_1"));
        bus.emit(status_changed("tsk_2"));

        let drained = sub.try_drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(
            drained[0].event_type(),
            OrchestratorEventType::TaskStatusChanged
        );
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_event() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
        bus.emit(status_changed("tsk_1"));
        assert_eq!(a.try_drain().len(), 1);
        assert_eq!(b.try_drain().len(), 1);
    }

    #[tokio::test]
    async fn emitting_without_subscribers_is_not_an_error() {
        let bus = EventBus::new();
        bus.emit(status_changed("tsk_1"));
        // A subscriber attached afterwards sees nothing earlier.
        let mut sub = bus.subscribe();
        assert!(sub.try_drain().is_empty());
    }

    #[tokio::test]
    async fn recv_resolves_when_an_event_arrives() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        let waiter = tokio::spawn(async move { sub.recv().await });
        tokio::task::yield_now().await;
        bus.emit(status_changed("tsk_1"));
        assert!(waiter.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn recv_returns_none_once_the_bus_is_dropped() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        drop(bus);
        assert!(sub.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_lagging_subscriber_keeps_going_rather_than_erroring_out() {
        let bus = EventBus::with_capacity(2);
        let mut sub = bus.subscribe();
        for n in 0..5 {
            bus.emit(status_changed(&format!("tsk_{n}")));
        }
        // Two retained, three dropped — and the subscriber still works.
        let drained = sub.try_drain();
        assert_eq!(drained.len(), 2);
        bus.emit(status_changed("tsk_after"));
        assert_eq!(sub.try_drain().len(), 1);
    }

    #[tokio::test]
    async fn a_lagging_subscriber_can_tell_how_much_it_missed() {
        let bus = EventBus::with_capacity(2);
        let mut sub = bus.subscribe();
        assert_eq!(sub.skipped(), 0);

        for n in 0..5 {
            bus.emit(status_changed(&format!("tsk_{n}")));
        }
        // Dropping events silently is right for a dashboard feed and wrong for
        // a caller waiting on one specific event, so the loss is countable.
        assert_eq!(sub.try_drain().len(), 2);
        assert_eq!(sub.skipped(), 3);

        // It accumulates rather than resetting, so a caller can compare it
        // across calls, and `recv` counts the same way `try_drain` does.
        for n in 0..5 {
            bus.emit(status_changed(&format!("tsk_second_{n}")));
        }
        assert!(sub.recv().await.is_some());
        assert_eq!(sub.skipped(), 6);
    }

    #[tokio::test]
    async fn a_nested_emit_keeps_channel_order() {
        // The channel send precedes handler dispatch, so a handler that emits
        // a follow-up event cannot make the follow-up arrive first.
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        let nested = bus.clone();
        let _unsub = bus.on(OrchestratorEventType::TaskCreated, move |_| {
            nested.emit(status_changed("tsk_nested"));
        });

        bus.emit(task_created());
        let drained = sub.try_drain();
        assert_eq!(
            drained.iter().map(|e| e.event_type()).collect::<Vec<_>>(),
            vec![
                OrchestratorEventType::TaskCreated,
                OrchestratorEventType::TaskStatusChanged,
            ]
        );
    }

    #[test]
    fn a_clone_shares_the_registry() {
        let bus = EventBus::new();
        let (count, handler) = counter();
        let _unsub = bus.on(OrchestratorEventType::TaskCreated, handler);

        bus.clone().emit(task_created());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            bus.clone()
                .listener_count(OrchestratorEventType::TaskCreated),
            1
        );
    }
}
