//! [`EventBuffer`].
//!
//! A bounded FIFO queue with backpressure, used by the shell adapter to merge
//! its two concurrently-read streams. What matters is the *concurrency*, not
//! the data structure, so it is a `VecDeque` rather than a hand-rolled ring
//! with explicit head/tail indices; every observable is preserved:
//!
//! - capacity 1024 by default;
//! - `push` waits for space rather than dropping or growing;
//! - `push` after `close()` **silently drops** the event and returns;
//! - `take` returns `None` only when the buffer is empty **and** closed, so
//!   buffered events still drain after `close()`;
//! - `close()` unblocks a waiting producer and a waiting consumer.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::Stream;
use orch_core::adapter::AgentEvent;
use tokio::sync::Notify;

const DEFAULT_CAPACITY: usize = 1024;

#[derive(Debug)]
struct State {
    queue: VecDeque<AgentEvent>,
    closed: bool,
}

#[derive(Debug)]
pub struct EventBuffer {
    state: Mutex<State>,
    capacity: usize,
    /// Woken when an event arrives or the buffer closes.
    data_ready: Notify,
    /// Woken when space frees up or the buffer closes.
    space_ready: Notify,
}

impl Default for EventBuffer {
    fn default() -> Self {
        EventBuffer::new()
    }
}

impl EventBuffer {
    pub fn new() -> Self {
        EventBuffer::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "an EventBuffer needs room for one event");
        EventBuffer {
            state: Mutex::new(State {
                queue: VecDeque::with_capacity(capacity),
                closed: false,
            }),
            capacity,
            data_ready: Notify::new(),
            space_ready: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Number of buffered events.
    pub fn size(&self) -> usize {
        self.lock().queue.len()
    }

    pub fn is_full(&self) -> bool {
        self.lock().queue.len() >= self.capacity
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Push an event, waiting for space if the buffer is full.
    pub async fn push(&self, event: AgentEvent) {
        loop {
            // Subscribe before looking, so a `take` between the check and the
            // await cannot be missed.
            let space = self.space_ready.notified();
            let mut space = std::pin::pin!(space);
            space.as_mut().enable();

            {
                let mut state = self.lock();
                if state.closed {
                    return;
                }
                if state.queue.len() < self.capacity {
                    state.queue.push_back(event);
                    drop(state);
                    self.data_ready.notify_waiters();
                    return;
                }
            }

            space.await;
        }
    }

    /// Dequeue the next event, waiting for one to arrive.
    ///
    /// `None` means the buffer is closed **and** drained.
    pub async fn take(&self) -> Option<AgentEvent> {
        loop {
            let data = self.data_ready.notified();
            let mut data = std::pin::pin!(data);
            data.as_mut().enable();

            {
                let mut state = self.lock();
                if let Some(event) = state.queue.pop_front() {
                    drop(state);
                    self.space_ready.notify_waiters();
                    return Some(event);
                }
                if state.closed {
                    return None;
                }
            }

            data.await;
        }
    }

    /// Signal that no more events will be pushed, waking every waiter.
    pub fn close(&self) {
        self.lock().closed = true;
        self.data_ready.notify_waiters();
        self.space_ready.notify_waiters();
    }
}

/// Drain the buffer until it is closed and empty.
pub fn drain(buffer: Arc<EventBuffer>) -> impl Stream<Item = AgentEvent> + Send {
    async_stream::stream! {
        while let Some(event) = buffer.take().await {
            yield event;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use orch_core::adapter::AgentEventKind;
    use serde_json::json;
    use std::time::Duration;

    fn event(data: &str) -> AgentEvent {
        AgentEvent::new(
            AgentEventKind::Output,
            "2026-01-01T00:00:00.000Z",
            json!(data),
        )
    }

    fn data_of(event: &AgentEvent) -> String {
        event.data.as_str().expect("string data").to_owned()
    }

    #[tokio::test]
    async fn pushes_and_takes_in_fifo_order() {
        let buffer = EventBuffer::new();
        buffer.push(event("a")).await;
        buffer.push(event("b")).await;
        assert_eq!(data_of(&buffer.take().await.expect("a")), "a");
        assert_eq!(data_of(&buffer.take().await.expect("b")), "b");
    }

    #[tokio::test]
    async fn take_waits_for_a_push() {
        let buffer = Arc::new(EventBuffer::new());
        let waiting = tokio::spawn({
            let buffer = Arc::clone(&buffer);
            async move { buffer.take().await }
        });
        tokio::task::yield_now().await;
        buffer.push(event("delayed")).await;
        assert_eq!(
            data_of(&waiting.await.expect("join").expect("event")),
            "delayed"
        );
    }

    #[tokio::test]
    async fn returns_none_after_close_on_an_empty_buffer() {
        let buffer = EventBuffer::new();
        buffer.close();
        assert!(buffer.take().await.is_none());
        assert!(buffer.is_closed());
    }

    #[tokio::test]
    async fn drains_remaining_events_after_close() {
        let buffer = EventBuffer::new();
        buffer.push(event("x")).await;
        buffer.close();
        assert_eq!(data_of(&buffer.take().await.expect("x")), "x");
        assert!(buffer.take().await.is_none());
    }

    #[tokio::test]
    async fn an_event_pushed_after_close_is_silently_dropped() {
        let buffer = EventBuffer::new();
        buffer.close();
        buffer.push(event("late")).await;
        assert_eq!(buffer.size(), 0);
        assert!(buffer.take().await.is_none());
    }

    #[tokio::test]
    async fn tracks_size() {
        let buffer = EventBuffer::with_capacity(4);
        assert_eq!(buffer.size(), 0);
        buffer.push(event("1")).await;
        buffer.push(event("2")).await;
        assert_eq!(buffer.size(), 2);
        buffer.take().await;
        assert_eq!(buffer.size(), 1);
    }

    #[tokio::test]
    async fn keeps_order_across_a_full_cycle() {
        // The observable is that order survives draining and refilling.
        let buffer = EventBuffer::with_capacity(3);
        buffer.push(event("a")).await;
        buffer.push(event("b")).await;
        buffer.take().await;
        buffer.take().await;
        buffer.push(event("c")).await;
        buffer.push(event("d")).await;
        buffer.push(event("e")).await;
        assert_eq!(data_of(&buffer.take().await.expect("c")), "c");
        assert_eq!(data_of(&buffer.take().await.expect("d")), "d");
        assert_eq!(data_of(&buffer.take().await.expect("e")), "e");
    }

    #[tokio::test]
    async fn applies_backpressure_when_full() {
        let buffer = Arc::new(EventBuffer::with_capacity(2));
        buffer.push(event("1")).await;
        buffer.push(event("2")).await;
        assert!(buffer.is_full());

        let blocked = tokio::spawn({
            let buffer = Arc::clone(&buffer);
            async move { buffer.push(event("3")).await }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!blocked.is_finished(), "push must wait for space");

        buffer.take().await;
        blocked.await.expect("join");
        assert_eq!(buffer.size(), 2);
    }

    #[tokio::test]
    async fn close_unblocks_a_waiting_push() {
        let buffer = Arc::new(EventBuffer::with_capacity(1));
        buffer.push(event("1")).await;
        let blocked = tokio::spawn({
            let buffer = Arc::clone(&buffer);
            async move { buffer.push(event("2")).await }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        buffer.close();
        blocked.await.expect("the push resolves instead of hanging");
    }

    #[tokio::test]
    async fn the_drain_stream_yields_everything_and_stops_on_close() {
        let buffer = Arc::new(EventBuffer::new());
        buffer.push(event("x")).await;
        buffer.push(event("y")).await;
        buffer.close();

        let collected: Vec<String> = drain(Arc::clone(&buffer))
            .map(|event| data_of(&event))
            .collect()
            .await;
        assert_eq!(collected, ["x", "y"]);
    }

    #[tokio::test]
    async fn the_drain_stream_waits_for_events_then_stops_on_close() {
        let buffer = Arc::new(EventBuffer::new());
        let consumer = tokio::spawn({
            let buffer = Arc::clone(&buffer);
            async move {
                drain(buffer)
                    .map(|event| data_of(&event))
                    .collect::<Vec<_>>()
                    .await
            }
        });

        buffer.push(event("a")).await;
        buffer.push(event("b")).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        buffer.close();
        assert_eq!(consumer.await.expect("join"), ["a", "b"]);
    }

    #[tokio::test]
    async fn two_producers_and_one_consumer_lose_nothing() {
        let buffer = Arc::new(EventBuffer::with_capacity(2));
        let producers: Vec<_> = (0..2)
            .map(|producer| {
                let buffer = Arc::clone(&buffer);
                tokio::spawn(async move {
                    for index in 0..50 {
                        buffer.push(event(&format!("{producer}:{index}"))).await;
                    }
                })
            })
            .collect();

        let consumer = tokio::spawn({
            let buffer = Arc::clone(&buffer);
            async move {
                let mut seen = Vec::new();
                while let Some(event) = buffer.take().await {
                    seen.push(data_of(&event));
                }
                seen
            }
        });

        for producer in producers {
            producer.await.expect("join");
        }
        buffer.close();
        assert_eq!(consumer.await.expect("join").len(), 100);
    }
}
