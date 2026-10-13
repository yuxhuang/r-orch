//! The event bus — ported from `src/application/event-bus.ts`.
//!
//! The TS bus is **synchronous**: `emit()` runs every handler inline and
//! swallows their exceptions. The Rust equivalent is a `broadcast` channel
//! with `Arc` payloads, which gives cheap fan-out to the TUI, the structured
//! logger and the run-store writer (ARCHITECTURE §6.5).
//!
//! One behaviour difference, stated plainly: `broadcast` **drops** messages
//! for a lagging receiver where synchronous emission cannot lose an event.
//! The channel is sized generously and `Lagged` is surfaced as a
//! `tracing::warn`, never silently ignored. A subscriber that must not miss
//! events consumes on a dedicated task that does nothing else.

use std::sync::Arc;

use orch_domain::OrchestratorEvent;
use tokio::sync::broadcast;

/// Channel capacity. Generous because the lossy case is the thing to avoid
/// (ARCHITECTURE §6.5).
pub const EVENT_CHANNEL_CAPACITY: usize = 4096;

#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Arc<OrchestratorEvent>>,
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        EventBus::with_capacity(EVENT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        EventBus { tx }
    }

    /// Non-blocking. An event emitted with no subscribers is dropped, exactly
    /// as the TS bus drops one with no handlers.
    pub fn emit(&self, event: OrchestratorEvent) {
        let _ = self.tx.send(Arc::new(event));
    }

    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            rx: self.tx.subscribe(),
        }
    }

    /// Live subscriber count.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// A receiving end of the bus.
#[derive(Debug)]
pub struct EventSubscription {
    rx: broadcast::Receiver<Arc<OrchestratorEvent>>,
}

impl EventSubscription {
    /// The next event, or `None` once the bus is dropped.
    ///
    /// A lag is logged and the stream continues from the oldest retained
    /// event, which is what a lossy fan-out means in practice.
    pub async fn recv(&mut self) -> Option<Arc<OrchestratorEvent>> {
        loop {
            match self.rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "event bus subscriber lagged; events were dropped");
                }
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
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "event bus subscriber lagged; events were dropped");
                }
                Err(_) => return events,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{OrchestratorEventType, TaskStatus};

    fn status_changed(task_id: &str) -> OrchestratorEvent {
        OrchestratorEvent::TaskStatusChanged {
            task_id: task_id.into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        }
    }

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
}
