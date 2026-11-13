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

    /// Stop subscriptions and release their reservations while accepted RPC
    /// replies drain through the separate response queue. Overflow still uses
    /// the closed signal to terminate an unhealthy session immediately.
    pub(crate) fn begin_drain(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.subscriptions.clear();
        self.ready.notify_one();
    }

    pub(crate) fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.subscriptions.clear();
        self.closed.send_replace(true);
        self.ready.notify_one();
    }

    /// Called by the session writer after the acknowledgement is flushed.
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
            output.begin_drain();
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

    /// Pins the listener's start on a project without a task index, driven
    /// directly by the hub fixture rather than the daemon's startup recovery.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_listener_start_declares_task_index_repair_with_remote_status() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let mut engine = orch_engine::build_full_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(temp.path().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        engine.engine.config.peer.listen = Some("127.0.0.1:0".into());
        engine.orchestrator.enable_daemon_mode();
        let engine = Arc::new(engine);
        let mut runtime = orch_peer::PeerRuntime::prepare(engine.clone())
            .await
            .unwrap();
        let index = engine.paths.tasks_dir().join("_index.json");
        assert!(!tokio::fs::try_exists(&index).await.unwrap());
        let boundary = engine.events.operation_boundary();
        let baseline = boundary.current_commit_seq();
        let records = Arc::new(Mutex::new(Vec::new()));
        let captured = records.clone();
        let published = Arc::new(tokio::sync::Notify::new());
        let ready = published.clone();
        boundary.set_publisher(Some(Arc::new(move |p| {
            if let Publication::Commit(record) = p {
                lock(&captured).push(record.clone());
                ready.notify_one();
            }
        })));
        runtime.start_listener();
        tokio::time::timeout(std::time::Duration::from_secs(5), published.notified())
            .await
            .expect("listener start committed");
        let start_records = lock(&records).clone();
        runtime.shutdown().await;
        assert_eq!(start_records.len(), 1, "one listener-start commit");
        let record = &start_records[0];
        assert_eq!(record.source, CommitSource::Peer);
        assert!(record.commit_seq > baseline);
        assert!(!record.store_set_uncertain);
        let stores: Vec<_> = record.changes.iter().map(|change| change.store).collect();
        assert!(
            stores.contains(&CommitStore::Tasks),
            "task-index repair declared"
        );
        assert!(
            stores.contains(&CommitStore::Remote),
            "listener status declared"
        );
        assert_eq!(stores.len(), 2, "only Tasks and Remote declared");
        let rebuilt: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&index).await.unwrap()).unwrap();
        assert_eq!(rebuilt, serde_json::json!([]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_status_publish_is_remote_store_wide_without_tasks_or_resync() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let mut engine = orch_engine::build_full_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(temp.path().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        engine.engine.config.peer.listen = Some("127.0.0.1:0".into());
        engine.orchestrator.enable_daemon_mode();
        let engine = Arc::new(engine);
        let mut runtime = orch_peer::PeerRuntime::prepare(engine.clone())
            .await
            .unwrap();
        let boundary = engine.events.operation_boundary();
        let output = output();
        let baseline = boundary.current_commit_seq();
        for (id, stores) in [
            (1, r#"{"stores":["remote"]}"#),
            (2, r#"{"stores":["tasks"]}"#),
        ] {
            assert!(output.register(id, Id::new("daemon_test").unwrap(), baseline, args(stores)));
            output.activate(id);
        }
        let sink = output.clone();
        let records = Arc::new(Mutex::new(Vec::new()));
        let captured = records.clone();
        boundary.set_publisher(Some(Arc::new(move |p| {
            if let Publication::Commit(record) = p {
                lock(&captured).push(record.clone());
            }
            sink.publish(p);
        })));
        // Keep task-index repair separate from the listener's status write.
        tokio::fs::create_dir_all(engine.paths.tasks_dir())
            .await
            .unwrap();
        tokio::fs::write(engine.paths.tasks_dir().join("_index.json"), b"[]")
            .await
            .unwrap();
        runtime.start_listener();
        let path = engine.paths.remotes_dir().join("_listener.json");
        let addr = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    if let Some(addr) = value["listen_addr"].as_str() {
                        break addr.to_owned();
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("listener status published");
        let socket = tokio::net::TcpStream::connect(&addr).await.unwrap();
        // The listener publication itself is a real peer persistence phase.
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(event) = output.pop() {
                    break serde_json::from_slice::<EventFrame>(&event.bytes)
                        .unwrap()
                        .ev;
                }
                output.ready.notified().await;
            }
        })
        .await
        .expect("remote invalidation");
        assert_eq!((event.subscription, event.seq), (1, 1));
        let EventBody::Change {
            store: Store::Remote,
            id: None,
            kind: ChangeKind::Updated,
            commit_seq,
        } = event.body
        else {
            panic!("expected remote store-wide change, got {:?}", event.body);
        };
        assert!(commit_seq > baseline);
        assert!(lock(&records).iter().any(|r| r.commit_seq == commit_seq
            && r.source == CommitSource::Peer
            && !r.store_set_uncertain));
        assert!(
            output.pop().is_none(),
            "tasks-only subscriber gets no remote event"
        );
        // Measure the unchanged idle worker cadence (fixture ruling section 26).
        use orch_peer::codec::{read_frame, write_frame};
        use orch_peer::protocol::{Frame, PROTOCOL};
        let mut peer = tokio::io::BufReader::new(socket);
        write_frame(
            peer.get_mut(),
            &Frame::Hello {
                proto: PROTOCOL,
                instance_id: "controller_test".into(),
                name: "controller".into(),
                os: "test".into(),
                agents: vec![],
                version: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame(&mut peer).await.unwrap(),
            Some(Frame::Welcome { .. })
        ));
        write_frame(peer.get_mut(), &Frame::Sync { active: vec![] })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match read_frame(&mut peer).await.unwrap().unwrap() {
                    Frame::SyncState { .. } => break,
                    Frame::Agents { .. } => {}
                    Frame::Ping => write_frame(peer.get_mut(), &Frame::Pong).await.unwrap(),
                    frame => panic!("unexpected peer reply: {frame:?}"),
                }
            }
        })
        .await
        .expect("initial sync");
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        output.remove(1);
        output.remove(2);
        let baseline = boundary.current_commit_seq();
        for (id, stores) in [
            (1, r#"{"stores":["messages"]}"#),
            (
                2,
                r#"{"stores":["tasks","goals","agents","messages","context","teams","runs","config","state","remote"]}"#,
            ),
        ] {
            assert!(output.register(id, Id::new("daemon_test").unwrap(), baseline, args(stores)));
            output.activate(id);
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let mut counts = [0; 2];
        let mut resyncs = 0;
        while let Some(frame) = output.pop() {
            let event = serde_json::from_slice::<EventFrame>(&frame.bytes)
                .unwrap()
                .ev;
            counts[(event.subscription - 1) as usize] += 1;
            resyncs += usize::from(matches!(event.body, EventBody::Resync { .. }));
        }
        eprintln!(
            "idle linked peer over 5s: messages={} all={} resyncs={resyncs}",
            counts[0], counts[1]
        );
        assert_eq!(resyncs, 0, "known peer writes do not require resync");
        assert_eq!(counts, [0, 0], "idle worker produces no invalidations");
        assert_eq!(boundary.current_commit_seq(), baseline);

        // A message expiring on the idle link must still invalidate Messages.
        let mut message = orch_domain::Message {
            id: "msg_idle_expiry".into(),
            channel: orch_domain::MessageChannel::Direct,
            from_agent_id: "sender".into(),
            to_agent_id: Some("recipient@controller".into()),
            subject: String::new(),
            body: String::new(),
            reply_to: None,
            created_at: engine.clock.now_iso(),
            expires_at: Some(orch_core::clock::iso_from_epoch_ms(
                engine.clock.now_ms() + 1200,
            )),
            status: orch_domain::MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
        };
        engine.messages.save(&message).await.unwrap();
        reset_peer_subscriptions(&output, boundary.current_commit_seq());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if engine.messages.get(&message.id).await.unwrap().is_none() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("outbox tick purges expired message");
        // Entity rename/removal precedes index commit and permit release.
        // Join the ordered path before asserting after-release publication.
        boundary.run(false, async {}).await;
        assert_peer_message_change(&output, &records, false);

        // Read the pending frame from the real session, then send a rejecting
        // MsgAck. The raw rejection store must reach a Messages subscriber.
        message.id = "msg_ack_reject".into();
        message.expires_at = None;
        engine.messages.save(&message).await.unwrap();
        reset_peer_subscriptions(&output, boundary.current_commit_seq());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match read_frame(&mut peer).await.unwrap().unwrap() {
                    Frame::Msg { id, .. } if id == message.id => break,
                    Frame::Ping => write_frame(peer.get_mut(), &Frame::Pong).await.unwrap(),
                    Frame::Msg { .. } | Frame::Agents { .. } => {}
                    frame => panic!("unexpected outbox frame: {frame:?}"),
                }
            }
        })
        .await
        .expect("pending outbox delivered on next tick");
        let remote_before = remote_store_files(&engine.paths.remotes_dir()).await;
        write_frame(
            peer.get_mut(),
            &Frame::MsgAck {
                id: message.id.clone(),
                reason: Some("unknown recipient".into()),
            },
        )
        .await
        .unwrap();
        wait_for_rejection(&engine, &message.id).await;
        assert_peer_message_change(&output, &records, false);
        assert_eq!(
            remote_store_files(&engine.paths.remotes_dir()).await,
            remote_before,
            "message rejection writes no remote files"
        );

        let remote_before = remote_store_files(&engine.paths.remotes_dir()).await;
        message.id = "msg_local_oversize".into();
        message.body = "x".repeat(orch_peer::codec::MAX_FRAME_BYTES + 1);
        engine.messages.save(&message).await.unwrap();
        reset_peer_subscriptions(&output, boundary.current_commit_seq());
        wait_for_rejection(&engine, &message.id).await;
        assert_peer_message_change(&output, &records, false);
        assert_eq!(
            remote_store_files(&engine.paths.remotes_dir()).await,
            remote_before,
            "oversize rejection writes no remote files"
        );
        drop(peer);
        runtime.shutdown().await;
    }

    async fn remote_store_files(
        root: &std::path::Path,
    ) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut directories = vec![root.to_owned()];
        while let Some(directory) = directories.pop() {
            let mut entries = tokio::fs::read_dir(directory).await.unwrap();
            while let Some(entry) = entries.next_entry().await.unwrap() {
                let path = entry.path();
                if entry.file_type().await.unwrap().is_dir() {
                    directories.push(path);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        tokio::fs::read(path).await.unwrap(),
                    );
                }
            }
        }
        files
    }

    fn reset_peer_subscriptions(output: &EventOutput, baseline: u64) {
        for (id, stores) in [
            (1, r#"{"stores":["messages"]}"#),
            (2, r#"{"stores":["remote"]}"#),
            (3, r#"{"stores":["tasks"]}"#),
        ] {
            output.remove(id);
            assert!(output.register(id, Id::new("daemon_test").unwrap(), baseline, args(stores)));
            output.activate(id);
        }
    }

    async fn wait_for_rejection(engine: &orch_engine::FullEngine, id: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if engine
                    .messages
                    .get(id)
                    .await
                    .unwrap()
                    .is_some_and(|m| m.subject.starts_with("[rejected]"))
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("message rejected durably");
        engine
            .events
            .operation_boundary()
            .run(false, async {})
            .await;
    }

    fn assert_peer_message_change(
        output: &EventOutput,
        records: &Mutex<Vec<CommitRecord>>,
        remote: bool,
    ) {
        let mut stores = Vec::new();
        while let Some(frame) = output.pop() {
            let event = serde_json::from_slice::<EventFrame>(&frame.bytes)
                .unwrap()
                .ev;
            let EventBody::Change {
                store, commit_seq, ..
            } = event.body
            else {
                panic!("unexpected resync or bus event");
            };
            assert_ne!(store, Store::Tasks);
            let records = lock(records);
            let record = records.iter().find(|r| r.commit_seq == commit_seq).unwrap();
            assert_eq!(record.source, CommitSource::Peer);
            assert!(!record.store_set_uncertain);
            stores.push(store);
        }
        assert!(
            stores.contains(&Store::Messages),
            "message write missed its subscriber"
        );
        assert_eq!(stores.contains(&Store::Remote), remote);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_controller_publishes_only_remote_heartbeats_over_five_seconds() {
        async fn engine_at(
            root: &std::path::Path,
            name: &str,
            listen: bool,
        ) -> Arc<orch_engine::FullEngine> {
            std::fs::create_dir(root.join(".orchestry")).unwrap();
            let mut engine = orch_engine::build_full_with(
                root,
                orch_engine::EngineSeams {
                    global_config_path: Some(root.join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            engine.engine.config.peer.name = Some(name.into());
            engine.engine.config.peer.listen = listen.then(|| "127.0.0.1:0".into());
            engine.orchestrator.enable_daemon_mode();
            Arc::new(engine)
        }
        let worker_root = tempfile::tempdir().unwrap();
        let worker = engine_at(worker_root.path(), "worker_idle", true).await;
        let worker_runtime = orch_peer::PeerRuntime::start(worker.clone()).await.unwrap();
        let addr = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) =
                    tokio::fs::read(worker.paths.remotes_dir().join("_listener.json")).await
                {
                    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    if let Some(addr) = value["listen_addr"].as_str() {
                        break addr.to_owned();
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let controller_root = tempfile::tempdir().unwrap();
        let controller = engine_at(controller_root.path(), "controller_idle", false).await;
        let store = orch_infra::storage::remote::FileRemoteStore::new(controller.paths.clone());
        store
            .add(orch_domain::RemoteConfig {
                alias: "worker".into(),
                addr,
                added_at: controller.clock.now_iso(),
            })
            .await
            .unwrap();
        let runtime = orch_peer::PeerRuntime::start(controller.clone())
            .await
            .unwrap();
        let status_path = controller.paths.remotes_dir().join("worker.json");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = tokio::fs::read(&status_path).await {
                    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    if value["online"] == true {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("controller linked");
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let boundary = controller.events.operation_boundary();
        let output = output();
        let baseline = boundary.current_commit_seq();
        for (id, stores) in [
            (1, r#"{"stores":["messages"]}"#),
            (2, r#"{"stores":["tasks"]}"#),
            (3, r#"{"stores":["remote"]}"#),
            (
                4,
                r#"{"stores":["tasks","goals","agents","messages","context","teams","runs","config","state","remote"]}"#,
            ),
        ] {
            assert!(output.register(id, Id::new("daemon_test").unwrap(), baseline, args(stores)));
            output.activate(id);
        }
        let sink = output.clone();
        boundary.set_publisher(Some(Arc::new(move |p| sink.publish(p))));
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let mut counts = [0; 4];
        let mut resyncs = 0;
        while let Some(frame) = output.pop() {
            let event = serde_json::from_slice::<EventFrame>(&frame.bytes)
                .unwrap()
                .ev;
            counts[(event.subscription - 1) as usize] += 1;
            resyncs += usize::from(matches!(event.body, EventBody::Resync { .. }));
            assert!(
                matches!(
                    event.body,
                    EventBody::Change {
                        store: Store::Remote,
                        ..
                    }
                ),
                "idle controller changed another store"
            );
        }
        eprintln!(
            "idle controller over 5s: messages={} tasks={} remote={} all={} resyncs={resyncs}",
            counts[0], counts[1], counts[2], counts[3]
        );
        assert_eq!((counts[0], counts[1], resyncs), (0, 0, 0));
        assert!(
            (4..=6).contains(&counts[2]),
            "expected about 1Hz Remote heartbeat"
        );
        assert_eq!(counts[2], counts[3]);
        runtime.shutdown().await;
        worker_runtime.shutdown().await;
    }

    #[tokio::test]
    async fn message_sweep_publishes_expiry_messages_without_tasks_or_resync() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let engine = orch_engine::build_full_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(temp.path().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        engine.orchestrator.enable_daemon_mode();
        engine
            .messages
            .save(&orch_domain::Message {
                id: "msg_startup_expired".into(),
                channel: orch_domain::MessageChannel::Direct,
                from_agent_id: "sender".into(),
                to_agent_id: Some("recipient@worker".into()),
                subject: String::new(),
                body: String::new(),
                reply_to: None,
                created_at: engine.clock.now_iso(),
                expires_at: Some("2000-01-01T00:00:00.000Z".into()),
                status: orch_domain::MessageStatus::Pending,
                delivered_at: None,
                team_id: None,
            })
            .await
            .unwrap();
        let engine = Arc::new(engine);
        let boundary = engine.events.operation_boundary();
        let output = output();
        for (id, stores) in [
            (1, r#"{"stores":["messages"]}"#),
            (2, r#"{"stores":["tasks"]}"#),
        ] {
            assert!(output.register(
                id,
                Id::new("daemon_test").unwrap(),
                boundary.current_commit_seq(),
                args(stores)
            ));
            output.activate(id);
        }
        let records = Arc::new(Mutex::new(Vec::new()));
        let (sink, captured) = (output.clone(), records.clone());
        boundary.set_publisher(Some(Arc::new(move |p| {
            if let Publication::Commit(record) = p {
                lock(&captured).push(record.clone());
            }
            sink.publish(p);
        })));
        assert_eq!(
            boundary
                .run_from(CommitSource::Expiry, false, engine.messages.purge_expired())
                .await
                .value
                .unwrap(),
            1
        );
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(frame) = output.pop() {
                    break frame;
                }
                output.ready.notified().await;
            }
        })
        .await
        .expect("startup sweep invalidation");
        let event = serde_json::from_slice::<EventFrame>(&frame.bytes)
            .unwrap()
            .ev;
        assert_eq!(event.subscription, 1);
        let EventBody::Change {
            store: Store::Messages,
            id: None,
            commit_seq,
            ..
        } = event.body
        else {
            panic!("expected known Messages invalidation");
        };
        assert!(lock(&records).iter().any(|r| r.commit_seq == commit_seq
            && r.source == CommitSource::Expiry
            && !r.store_set_uncertain));
        assert!(output.pop().is_none(), "Tasks subscriber remains silent");
        let before = boundary.current_commit_seq();
        assert_eq!(
            boundary
                .run_from(CommitSource::Expiry, false, engine.messages.purge_expired())
                .await
                .value
                .unwrap(),
            0
        );
        assert_eq!(boundary.current_commit_seq(), before);
        assert!(output.pop().is_none(), "zero sweep stays silent");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_thousand_buffered_events_preserve_listeners_and_lag_without_blocking_control() {
        use crate::ops::Handler;
        use orch_proto::ops::args::DaemonStatusArgs;
        let (_temp, root) = crate::dispatch::tests::project().await;
        let running =
            crate::lifecycle::Running::bring_up(crate::dispatch::tests::bring_up(&root, None))
                .await
                .unwrap();
        let boundary = running.engine.operation_boundary();
        let output = output();
        assert!(running.handler.hub.register(
            999,
            Some(output.clone()),
            1,
            Id::new("daemon_test").unwrap(),
            boundary.current_commit_seq(),
            args(r#"{"stores":[],"bus":true}"#)
        ));
        output.activate(1);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = observed.clone();
        let _listener = running.engine.events.on_any(move |event| {
            if let OrchestratorEvent::AgentOutput { data, .. } = event {
                lock(&sink).push(data.clone());
            }
        });
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let producer = {
            let (engine, entered, release) =
                (running.engine.clone(), entered.clone(), release.clone());
            tokio::spawn(async move {
                engine
                    .operation_boundary()
                    .run(false, async {
                        for n in 0..1000 {
                            engine.events.emit(OrchestratorEvent::AgentOutput {
                                run_id: "run_many".into(),
                                agent_id: "agent_many".into(),
                                data: n.to_string(),
                            });
                        }
                        entered.notify_one();
                        release.notified().await;
                    })
                    .await;
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert!(
            lock(&observed).is_empty(),
            "no listener delivery under permit"
        );
        assert!(output.pop().is_none(), "no hub delivery under permit");
        let ctx = crate::lifecycle::internal_context();
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), producer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            *lock(&observed),
            (0..1000).map(|n| n.to_string()).collect::<Vec<_>>(),
            "exactly once in emit order"
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            Handler::daemon_status(&running.handler, &ctx, DaemonStatusArgs {}),
        )
        .await
        .expect("control while subscriber is stalled")
        .unwrap();
        let mut queued = 0;
        let mut lagged = false;
        while let Some(frame) = output.pop() {
            queued += 1;
            let event = serde_json::from_slice::<EventFrame>(&frame.bytes)
                .unwrap()
                .ev;
            match event.body {
                EventBody::Resync {
                    reason: ResyncReason::Lagged,
                } => lagged = true,
                EventBody::Bus(_) => {}
                other => panic!("unexpected body: {other:?}"),
            }
        }
        assert!(lagged, "256-cap subscriber requires lag resync");
        assert!(queued <= limits::MAX_QUEUED_EVENTS_PER_SUBSCRIPTION);
        running.handler.hub.disconnect(999);
        assert!(running.shutdown().await.clean);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tick_spawned_completion_publishes_collector_task_change() {
        use crate::ops::Handler;
        use orch_proto::ops::args::*;
        let (_temp, root) = crate::dispatch::tests::project().await;
        let running =
            crate::lifecycle::Running::bring_up(crate::dispatch::tests::bring_up(&root, None))
                .await
                .unwrap();
        let handler = running.handler.clone();
        let ctx = crate::lifecycle::internal_context();
        let agent = Handler::agent_create(&handler, &ctx, serde_json::from_value(serde_json::json!({
            "name": "completion", "adapter": "shell", "command": "exit 0", "workspace_mode": "shared",
            "env": [{"name":"HOME","value":root.to_str().unwrap()}, {"name":"PATH","value":"/usr/bin:/bin"}]
        })).unwrap()).await.unwrap().agent;
        let task = Handler::task_create(
            &handler,
            &ctx,
            serde_json::from_value(serde_json::json!({
                "title":"completion", "assignee": agent.id
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let output = output();
        let boundary = running.engine.operation_boundary();
        let baseline = boundary.current_commit_seq();
        assert!(handler.hub.register(
            999,
            Some(output.clone()),
            1,
            Id::new("daemon_test").unwrap(),
            baseline,
            args(r#"{"stores":["tasks"]}"#)
        ));
        output.activate(1);
        let records = Arc::new(Mutex::new(Vec::new()));
        let sink = records.clone();
        let hub = handler.hub.clone();
        boundary.set_publisher(Some(Arc::new(move |p| {
            if let Publication::Commit(record) = p {
                lock(&sink).push(record.clone());
            }
            hub.publish(p);
        })));
        Handler::dispatch_tick(&handler, &ctx, DispatchTickArgs {})
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = running
                    .engine
                    .tasks
                    .get(&task.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status;
                if matches!(state, TaskStatus::Done | TaskStatus::Review) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake shell completion");
        // Storage returns before synchronous post-release publication finishes
        // on the collector task, so wait for that publication explicitly.
        let commit_seq = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(seq) = lock(&records)
                    .iter()
                    .find(|r| {
                        r.source == CommitSource::Collector
                            && r.changes.iter().any(|c| {
                                c.store == CommitStore::Tasks
                                    && c.id.as_deref() == Some(task.id.as_str())
                            })
                    })
                    .map(|r| r.commit_seq)
                {
                    break seq;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("collector publication, distinct from tick provenance");
        let mut matched = false;
        while let Some(frame) = output.pop() {
            let event = serde_json::from_slice::<EventFrame>(&frame.bytes)
                .unwrap()
                .ev;
            matched |= matches!(event.body, EventBody::Change { store: Store::Tasks, id: Some(ref id), commit_seq: seq, .. }
                if id.as_str() == task.id && seq == commit_seq);
        }
        assert!(
            matched,
            "real internal completion must reach task subscribers"
        );
        assert!(running.shutdown().await.clean);
    }

    #[test]
    fn every_store_create_update_delete_preserves_matching_sequence() {
        let output = output();
        assert!(register(
            &output,
            1,
            args(
                r#"{"stores":["tasks","goals","agents","messages","context","teams","runs","config","state","remote"]}"#
            )
        ));
        output.activate(1);
        let mut seq = 0;
        for store in CommitStore::ALL {
            for (action, expected) in [
                (CommitAction::Created, ChangeKind::Created),
                (CommitAction::Updated, ChangeKind::Updated),
                (CommitAction::Deleted, ChangeKind::Deleted),
            ] {
                output.publish(&publication(store, Some("entity_test"), action));
                let event = next(&output);
                seq += 1;
                assert_eq!(event.seq, seq);
                assert!(
                    matches!(event.body, EventBody::Change { store: actual, id: Some(ref id), kind, .. }
                    if actual == wire_store(store) && id.as_str() == "entity_test" && kind == expected)
                );
            }
        }
        assert!(output.pop().is_none());
    }

    #[tokio::test]
    async fn subscribe_mutation_race_captures_baseline_and_buffers_until_ack() {
        use crate::coordinator::{Lane, ProjectCoordinator};
        let boundary = Arc::new(orch_engine::container::OperationBoundary::new());
        let coordinator = Arc::new(ProjectCoordinator::new(boundary.clone()));
        let hub = Arc::new(Hub::default());
        let output = output();
        let sink = hub.clone();
        boundary.set_publisher(Some(Arc::new(move |p| sink.publish(p))));
        boundary
            .run(false, async {
                boundary.note_store(CommitStore::Tasks);
            })
            .await;
        let (registered, registration) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let registrar = {
            let (coordinator, boundary, hub, output) = (
                coordinator.clone(),
                boundary.clone(),
                hub.clone(),
                output.clone(),
            );
            tokio::spawn(async move {
                coordinator
                    .run_op(Lane::Ordinary, move |_| {
                        Box::pin(async move {
                            let baseline = boundary.current_commit_seq();
                            assert!(hub.register(
                                1,
                                Some(output),
                                1,
                                Id::new("daemon_test").unwrap(),
                                baseline,
                                args(r#"{"stores":["tasks"]}"#)
                            ));
                            registered.send(baseline).unwrap();
                            released.await.unwrap();
                            Ok(())
                        })
                    })
                    .await
                    .unwrap()
            })
        };
        let baseline = registration.await.unwrap();
        assert_eq!(baseline, 1);
        let mutation = {
            let boundary = boundary.clone();
            tokio::spawn(async move {
                boundary
                    .run(false, async {
                        boundary.note_store(CommitStore::Tasks);
                    })
                    .await
                    .commit_seq
            })
        };
        assert!(output.pop().is_none());
        release.send(()).unwrap();
        let ack = registrar.await.unwrap();
        assert_eq!(
            ack.commit_seq, baseline,
            "subscribe baseline excludes queued mutation"
        );
        let committed = mutation.await.unwrap();
        assert!(committed > baseline);
        assert!(
            output.pop().is_none(),
            "commit is buffered while terminal ack is not yet flushed"
        );
        output.activate(1);
        let event = next(&output);
        assert_eq!(event.seq, 1);
        assert!(
            matches!(event.body, EventBody::Change { store: Store::Tasks, commit_seq, .. } if commit_seq == committed)
        );
        assert!(output.pop().is_none(), "no baseline event replay");
        hub.disconnect(1);
        assert_eq!(
            output.budget.available_permits(),
            limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION
        );
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
