//! Ordered, nonblocking subscription delivery. Every staged byte owns the
//! session's shared output budget, including activation and writer handoff.

use orch_engine::container::commit::{CommitAction, CommitChange, CommitStore, Publication};
use orch_proto::bounded::Id;
use orch_proto::frame::{
    ChangeKind, Event, EventBody, EventFrame, MAX_FRAME_BYTES, ResyncReason, Store, limits,
};
use orch_proto::ndjson;
use orch_proto::ops::args::{SubscribeArgs, SubscriptionFilter};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, watch};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug)]
struct EventCount(Arc<AtomicUsize>);
impl Drop for EventCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Retains reservations until the socket finishes writing this frame.
#[derive(Debug)]
pub(crate) struct QueuedEvent {
    pub(crate) bytes: Vec<u8>,
    _budget: OwnedSemaphorePermit,
    _count: EventCount,
}

#[derive(Debug)]
struct Subscription {
    args: SubscribeArgs,
    instance: Id,
    baseline: u64,
    active: bool,
    seq: u64,
    queue: VecDeque<QueuedEvent>,
    count: Arc<AtomicUsize>,
}

#[derive(Debug, Default)]
struct OutputState {
    subscriptions: HashMap<u64, Subscription>,
    closed: bool,
    last_subscription: u64,
}

/// Event queues share the response outbox's semaphore. No second event
/// channel or activation buffer escapes the session's byte and count caps.
#[derive(Debug)]
pub(crate) struct EventOutput {
    state: Mutex<OutputState>,
    budget: Arc<Semaphore>,
    pub(crate) ready: Notify,
    closed: watch::Sender<bool>,
}

impl EventOutput {
    pub(crate) fn new(budget: Arc<Semaphore>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            budget,
            ready: Notify::new(),
            closed: watch::channel(false).0,
        })
    }

    pub(crate) fn closed(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    pub(crate) fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.subscriptions.clear();
        self.closed.send_replace(true);
        self.ready.notify_one();
    }

    pub(crate) fn activate(&self, id: u64) {
        if let Some(subscription) = lock(&self.state).subscriptions.get_mut(&id) {
            subscription.active = true;
            self.ready.notify_one();
        }
    }

    pub(crate) fn pop(&self) -> Option<QueuedEvent> {
        let mut state = lock(&self.state);
        let ready =
            |subscription: &&Subscription| subscription.active && !subscription.queue.is_empty();
        let id = state
            .subscriptions
            .iter()
            .filter(|(_, s)| ready(s))
            .map(|(&id, _)| id)
            .filter(|id| *id > state.last_subscription)
            .min()
            .or_else(|| {
                state
                    .subscriptions
                    .iter()
                    .filter(|(_, s)| ready(s))
                    .map(|(&id, _)| id)
                    .min()
            })?;
        state.last_subscription = id;
        state.subscriptions.get_mut(&id)?.queue.pop_front()
    }

    fn register(&self, id: u64, instance: Id, baseline: u64, args: SubscribeArgs) -> bool {
        let mut state = lock(&self.state);
        if state.closed || state.subscriptions.len() >= limits::MAX_SUBSCRIPTIONS_PER_SESSION {
            return false;
        }
        state.subscriptions.insert(
            id,
            Subscription {
                args,
                instance,
                baseline,
                active: false,
                seq: 0,
                queue: VecDeque::new(),
                count: Arc::new(AtomicUsize::new(0)),
            },
        );
        true
    }

    fn remove(&self, id: u64) {
        lock(&self.state).subscriptions.remove(&id);
    }

    fn publish(&self, publication: &Publication) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        let mut failed = false;
        for (&id, subscription) in &mut state.subscriptions {
            match publication {
                Publication::Commit(record) if record.commit_seq > subscription.baseline => {
                    if record.store_set_uncertain {
                        failed |= !enqueue(
                            &self.budget,
                            id,
                            subscription,
                            EventBody::Resync {
                                reason: ResyncReason::StoreSetUncertain,
                            },
                        );
                    } else {
                        for change in &record.changes {
                            if matches_change(&subscription.args, change) {
                                let entity = change.id.as_ref().and_then(|id| Id::new(id).ok());
                                let kind = if entity.is_none() {
                                    ChangeKind::Updated
                                } else {
                                    match change.action {
                                        CommitAction::Created => ChangeKind::Created,
                                        CommitAction::Updated => ChangeKind::Updated,
                                        CommitAction::Deleted => ChangeKind::Deleted,
                                    }
                                };
                                failed |= !enqueue(
                                    &self.budget,
                                    id,
                                    subscription,
                                    EventBody::Change {
                                        store: wire_store(change.store),
                                        id: entity,
                                        kind,
                                        commit_seq: record.commit_seq,
                                    },
                                );
                            }
                        }
                    }
                }
                Publication::Bus(event) if subscription.args.bus => {
                    failed |= !enqueue(
                        &self.budget,
                        id,
                        subscription,
                        EventBody::Bus(Box::new((**event).clone())),
                    );
                }
                _ => {}
            }
        }
        if failed {
            state.closed = true;
            state.subscriptions.clear();
            self.closed.send_replace(true);
        }
        self.ready.notify_one();
    }
}

fn encode(id: u64, subscription: &Subscription, body: EventBody) -> Option<Vec<u8>> {
    ndjson::encode(
        &EventFrame {
            ev: Event {
                subscription: id,
                instance: subscription.instance.clone(),
                seq: subscription.seq,
                body,
            },
        },
        MAX_FRAME_BYTES,
    )
    .ok()
}

fn reserve(
    budget: &Arc<Semaphore>,
    subscription: &Subscription,
    bytes: Vec<u8>,
) -> Option<QueuedEvent> {
    let count = u32::try_from(bytes.len()).ok()?;
    // Leave room for all eight activation acknowledgements and small
    // control replies even when inactive event queues fill the session.
    if budget.available_permits() < bytes.len().saturating_add(4096) {
        return None;
    }
    let permit = Arc::clone(budget).try_acquire_many_owned(count).ok()?;
    subscription.count.fetch_add(1, Ordering::SeqCst);
    Some(QueuedEvent {
        bytes,
        _budget: permit,
        _count: EventCount(subscription.count.clone()),
    })
}

fn enqueue(
    budget: &Arc<Semaphore>,
    id: u64,
    subscription: &mut Subscription,
    body: EventBody,
) -> bool {
    subscription.seq = match subscription.seq.checked_add(1) {
        Some(seq) => seq,
        None => return false,
    };
    if subscription.count.load(Ordering::SeqCst) < limits::MAX_QUEUED_EVENTS_PER_SUBSCRIPTION
        && let Some(queued) =
            encode(id, subscription, body).and_then(|bytes| reserve(budget, subscription, bytes))
    {
        subscription.queue.push_back(queued);
        return true;
    }
    // In-flight bytes stay reserved; obsolete queued events release their
    // reservations before the recovery marker tries to claim capacity.
    subscription.queue.clear();
    let Some(queued) = encode(
        id,
        subscription,
        EventBody::Resync {
            reason: ResyncReason::Lagged,
        },
    )
    .and_then(|bytes| reserve(budget, subscription, bytes)) else {
        return false;
    };
    subscription.queue.push_back(queued);
    true
}

pub(crate) fn wire_store(store: CommitStore) -> Store {
    match store {
        CommitStore::Tasks => Store::Tasks,
        CommitStore::Goals => Store::Goals,
        CommitStore::Agents => Store::Agents,
        CommitStore::Messages => Store::Messages,
        CommitStore::Context => Store::Context,
        CommitStore::Teams => Store::Teams,
        CommitStore::Runs => Store::Runs,
        CommitStore::Config => Store::Config,
        CommitStore::State => Store::State,
        CommitStore::Remote => Store::Remote,
    }
}

pub(crate) fn engine_store(store: Store) -> CommitStore {
    match store {
        Store::Tasks => CommitStore::Tasks,
        Store::Goals => CommitStore::Goals,
        Store::Agents => CommitStore::Agents,
        Store::Messages => CommitStore::Messages,
        Store::Context => CommitStore::Context,
        Store::Teams => CommitStore::Teams,
        Store::Runs => CommitStore::Runs,
        Store::Config => CommitStore::Config,
        Store::State => CommitStore::State,
        Store::Remote => CommitStore::Remote,
    }
}

fn matches_change(args: &SubscribeArgs, change: &CommitChange) -> bool {
    if !args
        .stores
        .iter()
        .any(|store| *store == wire_store(change.store))
    {
        return false;
    }
    let Some(filter) = &args.filter else {
        return true;
    };
    let Some(id) = &change.id else {
        return true;
    };
    if filter
        .ids
        .as_ref()
        .is_some_and(|ids| !ids.iter().any(|wanted| wanted.as_str() == id))
    {
        return false;
    }
    membership_matches(filter, change)
}

fn membership_matches(filter: &SubscriptionFilter, change: &CommitChange) -> bool {
    // These stores have no complete goal/agent projection. Never interpret
    // absent metadata as proof that an entity cannot affect a filtered view.
    if matches!(
        change.store,
        CommitStore::Teams
            | CommitStore::Remote
            | CommitStore::Context
            | CommitStore::Config
            | CommitStore::State
    ) {
        return true;
    }
    let memberships: Vec<_> = change.old.iter().chain(change.new.iter()).collect();
    if memberships.is_empty() {
        return true;
    }
    let goal_matches = filter.goal_id.as_ref().is_none_or(|goal| {
        change.store != CommitStore::Tasks && change.store != CommitStore::Goals
            || memberships
                .iter()
                .any(|m| m.goal_id.as_deref() == Some(goal.as_str()))
    });
    let agent_matches = filter.agent_id.as_ref().is_none_or(|agent| {
        memberships.iter().any(|m| {
            m.agent_id.as_deref() == Some(agent.as_str())
                || m.recipient_id.as_deref() == Some(agent.as_str())
                || change.store == CommitStore::Messages && m.recipient_id.is_none()
        })
    });
    goal_matches && agent_matches
}

#[derive(Default)]
pub(crate) struct Hub {
    sessions: Mutex<HashMap<u64, Arc<EventOutput>>>,
}
impl Hub {
    pub(crate) fn register(
        &self,
        session: u64,
        output: Option<Arc<EventOutput>>,
        id: u64,
        instance: Id,
        baseline: u64,
        args: SubscribeArgs,
    ) -> bool {
        let mut sessions = lock(&self.sessions);
        let output = sessions.get(&session).cloned().unwrap_or_else(|| {
            output.unwrap_or_else(|| {
                EventOutput::new(Arc::new(Semaphore::new(
                    limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION,
                )))
            })
        });
        if !output.register(id, instance, baseline, args) {
            return false;
        }
        sessions.entry(session).or_insert(output);
        true
    }

    pub(crate) fn remove(&self, session: u64, id: u64) {
        if let Some(output) = lock(&self.sessions).get(&session) {
            output.remove(id);
        }
    }
    pub(crate) fn disconnect(&self, session: u64) {
        if let Some(output) = lock(&self.sessions).remove(&session) {
            output.close();
        }
    }
    pub(crate) fn publish(&self, publication: &Publication) {
        for output in lock(&self.sessions).values() {
            output.publish(publication);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{OrchestratorEvent, TaskStatus};
    use orch_engine::container::commit::{CommitRecord, CommitSource, Membership};

    fn args(json: &str) -> SubscribeArgs {
        serde_json::from_str(json).unwrap()
    }
    fn output() -> Arc<EventOutput> {
        EventOutput::new(Arc::new(Semaphore::new(
            limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION,
        )))
    }
    fn register(output: &EventOutput, id: u64, args: SubscribeArgs) -> bool {
        output.register(id, Id::new("daemon_test").unwrap(), 42, args)
    }
    fn publication(store: CommitStore, id: Option<&str>, action: CommitAction) -> Publication {
        Publication::Commit(CommitRecord {
            commit_seq: 43,
            source: CommitSource::Rpc,
            changes: vec![CommitChange {
                store,
                id: id.map(str::to_owned),
                action,
                old: None,
                new: None,
            }],
            store_set_uncertain: false,
        })
    }
    fn next(output: &EventOutput) -> Event {
        serde_json::from_slice::<EventFrame>(&output.pop().expect("event queued").bytes)
            .unwrap()
            .ev
    }

    #[test]
    fn inactive_subscription_buffers_matching_events_until_ack_activation() {
        let output = output();
        assert!(register(&output, 1, args(r#"{"stores":["tasks"]}"#)));
        for store in [CommitStore::Goals, CommitStore::Tasks] {
            output.publish(&publication(store, Some("task_a"), CommitAction::Created));
        }
        assert!(output.pop().is_none(), "no event before ack activation");
        output.activate(1);
        let event = next(&output);
        assert_eq!(
            event.seq, 1,
            "unmatched writes never advance the subscription cursor"
        );
        assert!(matches!(
            event.body,
            EventBody::Change {
                store: Store::Tasks,
                kind: ChangeKind::Created,
                commit_seq: 43,
                ..
            }
        ));
        assert!(output.pop().is_none());
    }

    #[test]
    fn eight_subscriptions_ninth_refused_unsubscribe_releases_slot_and_bytes() {
        let output = output();
        for id in 1..=8 {
            assert!(register(&output, id, args(r#"{"stores":["tasks"]}"#)));
        }
        assert!(!register(&output, 9, args(r#"{"stores":["tasks"]}"#)));
        let budget = output.budget.available_permits();
        output.publish(&publication(
            CommitStore::Tasks,
            Some("task_a"),
            CommitAction::Updated,
        ));
        assert!(output.budget.available_permits() < budget);
        for id in 1..=8 {
            output.remove(id);
        }
        assert_eq!(output.budget.available_permits(), budget);
        assert!(register(&output, 9, args(r#"{"stores":["tasks"]}"#)));
    }

    #[test]
    fn count_256_is_accepted_257_replaces_pending_data_with_resync() {
        let output = output();
        assert!(register(&output, 1, args(r#"{"stores":["tasks"]}"#)));
        for _ in 0..256 {
            output.publish(&publication(
                CommitStore::Tasks,
                Some("task_a"),
                CommitAction::Updated,
            ));
        }
        {
            let state = lock(&output.state);
            assert_eq!(state.subscriptions[&1].queue.len(), 256);
        }
        output.publish(&publication(
            CommitStore::Tasks,
            Some("task_a"),
            CommitAction::Updated,
        ));
        output.activate(1);
        let event = next(&output);
        assert_eq!(event.seq, 257);
        assert!(matches!(
            event.body,
            EventBody::Resync {
                reason: ResyncReason::Lagged
            }
        ));
        assert!(output.pop().is_none());
    }

    #[test]
    fn byte_cap_applies_during_activation_before_event_count_cap() {
        let output = output();
        assert!(register(&output, 1, args(r#"{"stores":[],"bus":true}"#)));
        let bus = Publication::Bus(Arc::new(OrchestratorEvent::OrchestratorError {
            error: "x".repeat(600 * 1024),
            context: "test".into(),
            fatal: false,
        }));
        for _ in 0..7 {
            output.publish(&bus);
        }
        output.activate(1);
        let event = next(&output);
        assert!(matches!(
            event.body,
            EventBody::Resync {
                reason: ResyncReason::Lagged
            }
        ));
        assert_eq!(event.seq, 7);
        assert!(output.pop().is_none());
        assert_eq!(
            output.budget.available_permits(),
            limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION
        );
    }

    #[test]
    fn writer_handoff_retains_both_reservations_and_close_releases_pending() {
        let output = output();
        assert!(register(&output, 1, args(r#"{"stores":["tasks"]}"#)));
        output.activate(1);
        output.publish(&publication(
            CommitStore::Tasks,
            Some("task_a"),
            CommitAction::Updated,
        ));
        let frame = output.pop().unwrap();
        let count = lock(&output.state).subscriptions[&1].count.clone();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let in_flight_bytes = frame.bytes.len();
        for _ in 0..256 {
            output.publish(&publication(
                CommitStore::Tasks,
                Some("task_a"),
                CommitAction::Updated,
            ));
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "in-flight frame plus recovery marker"
        );
        output.close();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            output.budget.available_permits(),
            limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION - in_flight_bytes
        );
        drop(frame);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            output.budget.available_permits(),
            limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION
        );
    }

    #[test]
    fn shared_response_budget_and_oversized_bus_cannot_escape_session_bound() {
        let output = output();
        assert!(register(&output, 1, args(r#"{"stores":[],"bus":true}"#)));
        // Simulate encoded response frames queued or held by a blocked writer.
        let held = output
            .budget
            .clone()
            .try_acquire_many_owned(limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION as u32)
            .unwrap();
        output.publish(&Publication::Bus(Arc::new(
            OrchestratorEvent::TaskStatusChanged {
                task_id: "task_a".into(),
                from: TaskStatus::Todo,
                to: TaskStatus::Done,
            },
        )));
        assert!(
            *output.closed().borrow(),
            "close if the recovery marker cannot fit"
        );
        drop(held);
        assert_eq!(
            output.budget.available_permits(),
            limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION
        );

        let output = self::output();
        assert!(register(&output, 1, args(r#"{"stores":[],"bus":true}"#)));
        output.activate(1);
        output.publish(&Publication::Bus(Arc::new(
            OrchestratorEvent::OrchestratorError {
                error: "x".repeat(MAX_FRAME_BYTES),
                context: "test".into(),
                fatal: false,
            },
        )));
        assert!(matches!(
            next(&output).body,
            EventBody::Resync {
                reason: ResyncReason::Lagged
            }
        ));
    }

    #[test]
    fn reassign_and_delete_reach_old_goal_and_agent_filters() {
        let output = output();
        assert!(register(
            &output,
            1,
            args(r#"{"stores":["tasks"],"filter":{"goal_id":"goal_old","agent_id":"agent_old"}}"#)
        ));
        output.activate(1);
        let old = Membership {
            goal_id: Some("goal_old".into()),
            agent_id: Some("agent_old".into()),
            recipient_id: None,
        };
        let new = Membership {
            goal_id: Some("goal_new".into()),
            agent_id: Some("agent_new".into()),
            recipient_id: None,
        };
        for (action, old, new) in [
            (CommitAction::Updated, Some(old.clone()), Some(new)),
            (CommitAction::Deleted, Some(old), None),
        ] {
            output.publish(&Publication::Commit(CommitRecord {
                commit_seq: 43,
                source: CommitSource::Internal,
                changes: vec![CommitChange {
                    store: CommitStore::Tasks,
                    id: Some("task_a".into()),
                    action,
                    old,
                    new,
                }],
                store_set_uncertain: false,
            }));
        }
        assert_eq!(next(&output).seq, 1);
        assert_eq!(next(&output).seq, 2);
        output.publish(&publication(
            CommitStore::Tasks,
            None,
            CommitAction::Updated,
        ));
        assert_eq!(
            next(&output).seq,
            3,
            "store-wide changes reach every matching store filter"
        );
    }

    #[test]
    fn uncertain_store_set_reaches_every_subscription_and_known_stores_remain_precise() {
        let output = output();
        assert!(register(
            &output,
            1,
            args(r#"{"stores":["context"],"filter":{"ids":["unrelated"]}}"#)
        ));
        output.activate(1);
        output.publish(&publication(
            CommitStore::Config,
            None,
            CommitAction::Updated,
        ));
        assert!(output.pop().is_none());
        output.publish(&Publication::Commit(CommitRecord {
            commit_seq: 43,
            source: CommitSource::Peer,
            changes: vec![],
            store_set_uncertain: true,
        }));
        assert!(matches!(
            next(&output).body,
            EventBody::Resync {
                reason: ResyncReason::StoreSetUncertain
            }
        ));
    }
    #[test]
    fn late_subscribe_after_eof_does_not_resurrect_session_reservations() {
        let hub = Hub::default();
        let output = output();
        let weak = Arc::downgrade(&output);
        assert!(hub.register(
            7,
            Some(output.clone()),
            1,
            Id::new("daemon_test").unwrap(),
            42,
            args(r#"{"stores":["tasks"]}"#)
        ));
        hub.disconnect(7);
        assert!(!hub.register(
            7,
            Some(output.clone()),
            2,
            Id::new("daemon_test").unwrap(),
            42,
            args(r#"{"stores":["tasks"]}"#)
        ));
        drop(output);
        assert!(
            weak.upgrade().is_none(),
            "rejected late subscription must not retain a dead session"
        );
    }

    #[test]
    fn continuously_ready_subscription_does_not_starve_its_session_peer() {
        let output = output();
        for id in 1..=2 {
            assert!(register(&output, id, args(r#"{"stores":["tasks"]}"#)));
            output.activate(id);
        }
        let mut observed = Vec::new();
        for _ in 0..8 {
            output.publish(&publication(
                CommitStore::Tasks,
                Some("task_a"),
                CommitAction::Updated,
            ));
            observed.push(next(&output).subscription);
        }
        assert!(
            observed.contains(&1) && observed.contains(&2),
            "both subscriptions must make progress: {observed:?}"
        );
    }
}
