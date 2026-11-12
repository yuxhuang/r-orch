//! Resource subscriptions over one daemon observation session (design D7,
//! D8; ruling 14.3).
//!
//! **One session per bridge.** However many URIs the client subscribes (at
//! most [`MAX_SUBSCRIBED_URIS`], distinct and canonical — `resources::parse`
//! accepts exactly one spelling per resource), the bridge holds a single
//! observation session, in its own identity and mode, subscribed by store.
//! Each change is mapped to the subscribed URIs locally ([`invalidates`]),
//! from what the projections in `resources.rs` read.
//!
//! **Never a missed invalidation.** A change carries a store and at most an
//! entity id, never membership, so a URI class the change may touch is
//! updated conservatively: every goal view on any task change (a deleted or
//! moved task may have belonged to any goal), every inbox on any message
//! change. `resources/updated` is a refetch hint; precision is a cost
//! question, completeness is not.
//!
//! **Bounded output.** Updates are coalesced per URI: a pending set of at
//! most [`MAX_SUBSCRIBED_URIS`] entries, written in URI order by one task. A
//! write storm marks each URI once until it is written. The task reading the
//! daemon never waits for stdout, so a client that stops reading cannot back
//! up into the daemon; the session's own bounded queue is drained at once.
//!
//! **Loss.** A sequence gap, a daemon resync, a new daemon instance or a lost
//! connection updates every subscribed URI, closes the session and keeps the
//! stdio session. Nothing reconnects on its own: the next explicit call
//! (a tool call, a read, a subscribe) opens a fresh session, which updates
//! every URI again because changes in between went unseen. No mutation is
//! ever replayed: an observation session only subscribes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use orch_client::{ClientError, Subscription, SubscriptionEvent};
use orch_proto::bounded::BoundedVec;
use orch_proto::frame::Store;
use orch_proto::ops::args::SubscribeArgs;
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::{ErrorData, Peer, RoleServer};
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use super::{Connector, NOT_EXECUTED_CODE, unavailable};
use crate::mcp::registry::ToolError;
use crate::mcp::resources::Target;

/// Distinct URIs one bridge tracks (ruling 14.3).
pub(crate) const MAX_SUBSCRIBED_URIS: usize = 256;
/// The longest supported spelling: `orch://agents/<id>@<alias>`.
const MAX_SUBSCRIBED_URI_BYTES: usize =
    "orch://agents/".len() + 2 * orch_proto::bounded::MAX_ID_BYTES + 1;

/// Subscription-local bounds; ordinary transient resource reads keep their parser.
pub(super) fn parse(uri: &str) -> Result<Target, ErrorData> {
    if uri.len() > MAX_SUBSCRIBED_URI_BYTES {
        return Err(ErrorData::invalid_params(
            "resource subscription URI is too long",
            None,
        ));
    }
    let target = crate::mcp::resources::parse(uri)?;
    let id = match &target {
        Target::Task(id) | Target::Goal(id) | Target::Agent(id) | Target::Inbox(id) => Some(id),
        _ => None,
    };
    if id.is_some_and(|id| {
        id.split('@')
            .any(|part| part.len() > orch_proto::bounded::MAX_ID_BYTES)
    }) {
        return Err(ErrorData::invalid_params(
            "resource subscription id and alias must each be at most 256 bytes",
            None,
        ));
    }
    Ok(target)
}

/// The stores any resource reads; teams are not among them.
const OBSERVED: [Store; 9] = [
    Store::Tasks,
    Store::Goals,
    Store::Agents,
    Store::Messages,
    Store::Context,
    Store::Runs,
    Store::Config,
    Store::State,
    Store::Remote,
];

/// Whether a change in `store` (to entity `id`, or store-wide when `None`)
/// may change what `target` reads. Must never answer `false` for a change
/// the projection can see.
pub(crate) fn invalidates(store: Store, id: Option<&str>, target: &Target) -> bool {
    let item = |of: &str| id.is_none_or(|id| id == of);
    match (store, target) {
        // A task belongs to at most one goal, but the change does not say
        // which one, before or after it.
        (Store::Tasks, Target::Status | Target::Tasks | Target::Goal(_)) => true,
        (Store::Tasks, Target::Task(task)) => item(task),
        (Store::Goals, Target::Goals) => true,
        (Store::Goals, Target::Goal(goal)) => item(goal),
        (Store::Agents, Target::Status | Target::Agents) => true,
        (Store::Agents, Target::Agent(agent)) => !agent.contains('@') && item(agent),
        // Remote snapshots are listed with the agents and read as id@alias;
        // their change ids are not agent ids.
        (Store::Remote, Target::Agents) => true,
        (Store::Remote, Target::Agent(agent)) => agent.contains('@'),
        // Recipient, read state and broadcasts are not on the change.
        (Store::Messages, Target::Inbox(_)) => true,
        (Store::Context, Target::Context) => true,
        // A goal's progress report is its `<goal>-progress` context entry.
        (Store::Context, Target::Goal(goal)) => {
            id.is_none_or(|key| key.strip_suffix("-progress") == Some(goal.as_str()))
        }
        (Store::Runs | Store::State | Store::Config, Target::Status) => true,
        _ => false,
    }
}

/// The bridge's subscriptions. Dropping it stops both tasks and closes the
/// observation session.
pub(super) struct Subscriptions {
    shared: Arc<Shared>,
}

struct Shared {
    observer: Connector,
    state: Mutex<State>,
    /// Signals the writer that updates are pending.
    pending: Notify,
    /// Held by the writer from taking a URI until its update is written, and
    /// by unsubscribe while it removes one: once unsubscribe has returned,
    /// no update for that URI follows.
    write: tokio::sync::Mutex<()>,
    /// One session being opened at a time.
    connect: tokio::sync::Mutex<()>,
    stop: CancellationToken,
    /// stdin ended: the writer drops its peer.
    input_closed: CancellationToken,
}

#[derive(Default)]
struct State {
    uris: BTreeMap<String, Target>,
    /// Always a subset of `uris`.
    dirty: BTreeSet<String>,
    link: Option<Link>,
    generation: u64,
    writer: Option<AbortHandle>,
    recovery: Option<Recovery>,
    /// Includes completed readers until the next operation reaps them. Dropping
    /// the set aborts all remaining tasks; async shutdown joins them as well.
    tasks: JoinSet<()>,
}

/// The open observation session, owned by its reader task.
struct Link {
    generation: u64,
    close: CancellationToken,
}

struct Recovery {
    close: CancellationToken,
    task: AbortHandle,
}

impl State {
    fn reap(&mut self) {
        while self.tasks.try_join_next().is_some() {}
    }

    fn invalidate_all(&mut self) {
        self.dirty.extend(self.uris.keys().cloned());
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Open the observation session unless one is open. A reconnect
    /// (`for_subscribe` false) keeps no session once nothing is subscribed.
    async fn connect(
        self: &Arc<Shared>,
        for_subscribe: bool,
        close: &CancellationToken,
    ) -> Result<(), ClientError> {
        // Cancellation covers both mutex acquisition and connect/subscribe. A
        // cancelled connect future drops the client rather than installing it.
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Ok(()),
            () = self.input_closed.cancelled() => Ok(()),
            () = close.cancelled() => Ok(()),
            result = self.open(for_subscribe, close) => result,
        }
    }

    async fn open(
        self: &Arc<Shared>,
        for_subscribe: bool,
        close: &CancellationToken,
    ) -> Result<(), ClientError> {
        let _connecting = self.connect.lock().await;
        if self.lock().link.is_some() {
            return Ok(());
        }
        let stores = BoundedVec::new(OBSERVED.to_vec())
            .map_err(|err| ClientError::InvalidArguments(err.to_string()))?;
        let client = (self.observer)().await?;
        let observation = client
            .into_subscription(SubscribeArgs {
                stores,
                filter: None,
                bus: false,
            })
            .await?;
        let mut state = self.lock();
        state.reap();
        if self.stop.is_cancelled()
            || self.input_closed.is_cancelled()
            || close.is_cancelled()
            || (!for_subscribe && state.uris.is_empty())
        {
            // Everything was unsubscribed meanwhile; dropping closes it.
            return Ok(());
        }
        state.generation += 1;
        let generation = state.generation;
        let close = CancellationToken::new();
        state.tasks.spawn(read(
            Arc::clone(self),
            observation,
            generation,
            close.clone(),
        ));
        state.link = Some(Link { generation, close });
        // Whatever changed before this session went unseen.
        if !state.uris.is_empty() {
            state.invalidate_all();
            self.pending.notify_one();
        }
        Ok(())
    }
}

impl Subscriptions {
    pub(super) fn new(observer: Connector, input_closed: CancellationToken) -> Subscriptions {
        Subscriptions {
            shared: Arc::new(Shared {
                observer,
                state: Mutex::new(State::default()),
                pending: Notify::new(),
                write: tokio::sync::Mutex::new(()),
                connect: tokio::sync::Mutex::new(()),
                stop: CancellationToken::new(),
                input_closed,
            }),
        }
    }

    /// `resources/subscribe` for a parsed `uri`. A duplicate is free; a new
    /// URI past the cap is refused; the session is open before this returns,
    /// so a read after it misses no later change.
    pub(super) async fn subscribe(
        &self,
        uri: &str,
        target: Target,
        peer: &Peer<RoleServer>,
        agent_context: bool,
    ) -> Result<(), ErrorData> {
        {
            let state = self.shared.lock();
            if state.uris.contains_key(uri) {
                return Ok(());
            }
            if state.uris.len() >= MAX_SUBSCRIBED_URIS {
                return Err(limit_reached());
            }
        }
        self.shared
            .connect(true, &self.shared.stop)
            .await
            .map_err(|err| {
                let err = ToolError::Client(err);
                if err.exit_code() == orch_proto::error::exit::NO_DAEMON {
                    unavailable(&err, agent_context)
                } else {
                    ErrorData::internal_error(err.text(agent_context), None)
                }
            })?;
        let mut state = self.shared.lock();
        state.reap();
        if self.shared.stop.is_cancelled() || self.shared.input_closed.is_cancelled() {
            return Err(ErrorData::new(
                NOT_EXECUTED_CODE,
                "Not executed: the server is shutting down",
                None,
            ));
        }
        if !state.uris.contains_key(uri) {
            if state.uris.len() >= MAX_SUBSCRIBED_URIS {
                return Err(limit_reached());
            }
            state.uris.insert(uri.to_owned(), target);
        }
        if state.writer.is_none() {
            state.writer = Some(
                state
                    .tasks
                    .spawn(write(Arc::clone(&self.shared), peer.clone())),
            );
        }
        Ok(())
    }

    /// `resources/unsubscribe`: idempotent and local. The last one closes
    /// the observation session.
    pub(super) async fn unsubscribe(&self, uri: &str) {
        let _write = self.shared.write.lock().await;
        let mut state = self.shared.lock();
        state.uris.remove(uri);
        state.dirty.remove(uri);
        state.reap();
        if state.uris.is_empty() {
            if let Some(link) = state.link.take() {
                link.close.cancel();
            }
            if let Some(recovery) = &state.recovery {
                recovery.close.cancel();
            }
        }
    }

    /// An explicit call: at most one owned recovery, including mutex waiters.
    /// A completed failure may be retried by a later call, never autonomously.
    pub(super) fn revive(&self) {
        let mut state = self.shared.lock();
        state.reap();
        if self.shared.stop.is_cancelled()
            || self.shared.input_closed.is_cancelled()
            || state.link.is_some()
            || state.uris.is_empty()
            || state
                .recovery
                .as_ref()
                .is_some_and(|r| !r.task.is_finished())
        {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let close = CancellationToken::new();
        let cancelled = close.clone();
        let task = state.tasks.spawn(async move {
            if let Err(err) = shared.connect(false, &cancelled).await {
                tracing::debug!(target: "orch::mcp", error = %err, "resource subscriptions not restored");
            }
        });
        state.recovery = Some(Recovery { close, task });
    }

    /// Capture before moving the handler into the SDK; join after its drain.
    pub(super) fn shutdown(&self) -> futures::future::BoxFuture<'static, ()> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            shared.stop.cancel();
            let mut tasks = {
                let mut state = shared.lock();
                state.link = None;
                state.recovery = None;
                state.writer = None;
                std::mem::take(&mut state.tasks)
            };
            while tasks.join_next().await.is_some() {}
        })
    }
}

impl Drop for Subscriptions {
    fn drop(&mut self) {
        self.shared.stop.cancel();
        let mut state = self.shared.lock();
        state.tasks.abort_all();
    }
}

fn limit_reached() -> ErrorData {
    ErrorData::new(
        NOT_EXECUTED_CODE,
        format!(
            "Not executed: at most {MAX_SUBSCRIBED_URIS} resources can be subscribed at once; \
             unsubscribe one first"
        ),
        None,
    )
}

/// Drain the observation session into the pending set. Never waits for
/// stdout. Ends, closing the session, on loss or when asked to.
async fn read(
    shared: Arc<Shared>,
    mut observation: Subscription,
    generation: u64,
    close: CancellationToken,
) {
    loop {
        let event = tokio::select! {
            () = close.cancelled() => return,
            () = shared.stop.cancelled() => return,
            event = observation.next() => event,
        };
        let mut state = shared.lock();
        match event {
            Ok(SubscriptionEvent::Change { store, id, .. }) => {
                let State { uris, dirty, .. } = &mut *state;
                let before = dirty.len();
                for (uri, target) in uris.iter() {
                    if invalidates(store, id.as_deref(), target) {
                        dirty.insert(uri.clone());
                    }
                }
                if dirty.len() != before {
                    shared.pending.notify_one();
                }
            }
            Ok(SubscriptionEvent::Bus { .. }) => {}
            Ok(SubscriptionEvent::ResyncRequired { reason }) => {
                tracing::debug!(target: "orch::mcp", ?reason, "resource subscriptions resynchronize");
                lost(&shared, &mut state, generation);
                return;
            }
            Err(err) => {
                tracing::debug!(target: "orch::mcp", error = %err, "resource subscriptions lost");
                lost(&shared, &mut state, generation);
                return;
            }
        }
    }
}

/// Every subscribed URI may have changed unseen; the session is gone.
fn lost(shared: &Shared, state: &mut State, generation: u64) {
    state.invalidate_all();
    if state
        .link
        .as_ref()
        .is_some_and(|link| link.generation == generation)
    {
        state.link = None;
    }
    shared.pending.notify_one();
}

/// Write pending updates, one URI at a time, in URI order. A failed write
/// means stdout is gone, which already stops the bridge.
async fn write(shared: Arc<Shared>, peer: Peer<RoleServer>) {
    loop {
        tokio::select! {
            () = shared.stop.cancelled() => return,
            () = shared.input_closed.cancelled() => return,
            () = shared.pending.notified() => {}
        }
        loop {
            let _write = shared.write.lock().await;
            let Some(uri) = shared.lock().dirty.pop_first() else {
                break;
            };
            let update = ResourceUpdatedNotificationParam::new(uri);
            tokio::select! {
                biased;
                () = shared.stop.cancelled() => return,
                () = shared.input_closed.cancelled() => return,
                result = peer.notify_resource_updated(update) => {
                    if result.is_err() { return; }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use orch_client::testing::{FakeServer, fake_transport};
    use orch_client::{Client, Project};
    use orch_proto::bounded::Id;
    use orch_proto::frame::{
        ChangeKind, Event, EventBody, EventFrame, OkFrame, ResyncReason, Store,
    };
    use orch_proto::handshake::{Caller, CallerKind, Session, SessionMode};
    use orch_proto::ops::Request;
    use orch_proto::ops::args::SubscribeArgs;
    use orch_proto::ops::results::SubscribeAck;
    use serde_json::{Value, json};
    use tokio::io::{
        AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
    };
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use crate::mcp::identity::Identity;
    use crate::mcp::mode::Mode;
    use crate::mcp::server::{Connector, McpServer, Seams, Settings, serve_transport};
    use crate::mcp::testing::{Fixture, no_daemon};

    /// A failure guard only: every wait below normally ends on an event.
    const WAIT: Duration = Duration::from_secs(20);
    /// Lexicographically after every other URI these tests subscribe, and
    /// touched by nothing but its own task id: once its update arrives, every
    /// update caused by an earlier event has been written (the bridge flushes
    /// pending URIs in order).
    const SENTINEL: &str = "orch://tasks/tsk_zz";

    /// One observation connection the bridge opened to the scripted daemon.
    struct Observed {
        session: Session,
        args: SubscribeArgs,
        instance: Id,
        subscription: u64,
        peer: FakeServer,
        next_seq: u64,
    }

    impl Observed {
        async fn change(&mut self, store: Store, id: Option<&str>, kind: ChangeKind) {
            let seq = self.next_seq;
            self.next_seq += 1;
            let body = EventBody::Change {
                store,
                id: id.map(|id| Id::new(id).unwrap()),
                kind,
                commit_seq: 9000 + seq,
            };
            self.event(seq, self.instance.clone(), body).await;
        }

        async fn event(&mut self, seq: u64, instance: Id, body: EventBody) {
            let ev = Event {
                subscription: self.subscription,
                instance,
                seq,
                body,
            };
            self.peer.send(&EventFrame { ev }).await.unwrap();
        }

        /// The bridge closed this observation: EOF on the daemon side, and
        /// no request (in particular no replayed mutation) before it.
        async fn closed(&mut self) {
            match tokio::time::timeout(WAIT, self.peer.recv()).await {
                Ok(Ok(None)) | Ok(Err(_)) => {}
                Ok(Ok(Some(frame))) => panic!("unexpected request {:?}", frame.req),
                Err(_) => panic!("the bridge kept the observation open"),
            }
        }
    }

    /// A scripted daemon for observation sessions only: each connection is
    /// handshaken, its subscribe acknowledged (ack.seq 0, baseline 9000),
    /// then handed to the test.
    struct Scripted {
        sessions: mpsc::UnboundedReceiver<Observed>,
        opened: Arc<AtomicUsize>,
    }

    impl Scripted {
        async fn session(&mut self) -> Observed {
            tokio::time::timeout(WAIT, self.sessions.recv())
                .await
                .expect("an observation session in time")
                .expect("the scripted daemon is alive")
        }

        fn opened(&self) -> usize {
            self.opened.load(Ordering::SeqCst)
        }
    }

    fn scripted(root: &Path, mode: Mode, agent: &str) -> (Connector, Scripted) {
        let (sessions_tx, sessions) = mpsc::unbounded_channel();
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&opened);
        let root = root.to_path_buf();
        let caller = Caller {
            kind: CallerKind::Mcp,
            agent_id: Some(Id::new(agent).unwrap()),
            pid: None,
        };
        let connector: Connector = Arc::new(move || {
            let (sessions_tx, root, caller) = (sessions_tx.clone(), root.clone(), caller.clone());
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                let (stream, mut peer) = fake_transport();
                let instance = Id::new(format!("instance-{n}")).unwrap();
                tokio::spawn(async move {
                    let hello = peer.accept(instance.clone()).await.unwrap();
                    let session = hello.hello.session_identity().unwrap();
                    let request = peer.recv().await.unwrap().unwrap();
                    let Request::Subscribe(args) = request.req else {
                        panic!("an observation session only subscribes: {:?}", request.req);
                    };
                    let subscription = n as u64 * 10;
                    let ack = SubscribeAck {
                        subscription,
                        instance: instance.as_str().to_owned(),
                        seq: 0,
                    };
                    peer.send(&OkFrame {
                        id: request.id,
                        ok: serde_json::value::to_raw_value(&ack).unwrap(),
                        commit_seq: 9000,
                    })
                    .await
                    .unwrap();
                    let _ = sessions_tx.send(Observed {
                        session,
                        args,
                        instance,
                        subscription,
                        peer,
                        next_seq: 1,
                    });
                });
                let project = Project::discover(&root, None)?;
                Client::from_stream(project, stream, caller, mode.session()).await
            })
        });
        (connector, Scripted { sessions, opened })
    }

    /// A bridge over in-memory stdio: ordinary calls reach the harness
    /// daemon, observation sessions reach the scripted one.
    struct Bridge {
        /// Keeps the harness daemon alive.
        _fixture: Fixture,
        daemon: Scripted,
        writer: Option<WriteHalf<DuplexStream>>,
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        /// Notifications read while waiting for a response.
        pending: VecDeque<Value>,
        /// The `initialize` result.
        init: Value,
        next_id: u64,
        output_stalls: Arc<AtomicUsize>,
        lifetime: std::sync::Weak<super::Shared>,
        output_lifetime: std::sync::Weak<()>,
        server: tokio::task::JoinHandle<orch_core::error::Result<()>>,
    }

    struct PendingOutput<W> {
        inner: W,
        stalls: Arc<AtomicUsize>,
        _lifetime: Arc<()>,
    }

    impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for PendingOutput<W> {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let poll = std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes);
            if poll.is_pending() {
                self.stalls.fetch_add(1, Ordering::SeqCst);
            }
            poll
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    async fn bridge(mode: Mode, stdio_buffer: usize) -> Bridge {
        let fixture = Fixture::start().await;
        let (observer, daemon) = scripted(fixture.root(), mode, "agt_me");
        bridge_with(fixture, observer, daemon, mode, stdio_buffer).await
    }

    async fn bridge_with(
        fixture: Fixture,
        observer: Connector,
        daemon: Scripted,
        mode: Mode,
        stdio_buffer: usize,
    ) -> Bridge {
        let settings = Settings {
            project_root: fixture.root().to_path_buf(),
            identity: Identity::of(Some("agt_me")),
            mode,
            agent_context: false,
        };
        let seams = Seams {
            connector: fixture.connector(mode, Some("agt_me")),
            observer,
            timer: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
        };
        let registry = crate::mcp::build_registry(mode).unwrap();
        let server = McpServer::new(settings, seams, registry);
        let lifetime = Arc::downgrade(&server.subscriptions.shared);
        let (client, server_io) = tokio::io::duplex(stdio_buffer);
        let (input, output) = tokio::io::split(server_io);
        let output_stalls = Arc::new(AtomicUsize::new(0));
        let output_owner = Arc::new(());
        let output_lifetime = Arc::downgrade(&output_owner);
        let output = PendingOutput {
            inner: output,
            stalls: output_stalls.clone(),
            _lifetime: output_owner,
        };
        let server = tokio::spawn(serve_transport(
            server,
            input,
            output,
            CancellationToken::new(),
        ));
        let (reader, writer) = tokio::io::split(client);
        let mut bridge = Bridge {
            _fixture: fixture,
            daemon,
            writer: Some(writer),
            lines: BufReader::new(reader).lines(),
            pending: VecDeque::new(),
            init: Value::Null,
            next_id: 1,
            output_stalls,
            lifetime,
            output_lifetime,
            server,
        };
        let init = bridge
            .call(
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "claude-code", "version": "0"},
                }),
            )
            .await;
        assert!(init.get("result").is_some(), "{init}");
        bridge.init = init["result"].clone();
        bridge
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        bridge
    }

    impl Bridge {
        async fn send(&mut self, frame: Value) {
            let mut line = serde_json::to_string(&frame).unwrap();
            line.push('\n');
            let writer = self.writer.as_mut().expect("stdin open");
            writer.write_all(line.as_bytes()).await.unwrap();
        }

        async fn frame(&mut self) -> Value {
            let line = tokio::time::timeout(WAIT, self.lines.next_line())
                .await
                .expect("a frame in time")
                .unwrap()
                .expect("stdout open");
            serde_json::from_str(&line).unwrap()
        }

        async fn call(&mut self, method: &str, params: Value) -> Value {
            let id = self.next_id;
            self.next_id += 1;
            self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
                .await;
            loop {
                let frame = self.frame().await;
                if frame["id"] == json!(id) {
                    return frame;
                }
                assert!(frame.get("id").is_none(), "a stray response: {frame}");
                self.pending.push_back(frame);
            }
        }

        async fn subscribe(&mut self, uri: &str) -> Value {
            self.call("resources/subscribe", json!({"uri": uri})).await
        }

        async fn subscribed(&mut self, uri: &str) {
            let reply = self.subscribe(uri).await;
            assert_eq!(reply["result"], json!({}), "{uri}: {reply}");
        }

        async fn unsubscribe(&mut self, uri: &str) {
            let reply = self
                .call("resources/unsubscribe", json!({"uri": uri}))
                .await;
            assert_eq!(reply["result"], json!({}), "{uri}: {reply}");
        }

        /// The URI of the next `notifications/resources/updated`.
        async fn updated(&mut self) -> String {
            let frame = match self.pending.pop_front() {
                Some(frame) => frame,
                None => self.frame().await,
            };
            assert_eq!(
                frame["method"], "notifications/resources/updated",
                "{frame}"
            );
            assert!(frame.get("id").is_none(), "{frame}");
            frame["params"]["uri"].as_str().unwrap().to_owned()
        }

        /// Updates until every URI of `expected` was seen at least once.
        async fn updated_all(&mut self, expected: &[&str]) -> Vec<String> {
            let mut missing: BTreeSet<&str> = expected.iter().copied().collect();
            let mut seen = Vec::new();
            while !missing.is_empty() {
                let uri = self.updated().await;
                missing.remove(uri.as_str());
                seen.push(uri);
            }
            seen
        }

        /// Push the sentinel's own change, then return the distinct URIs
        /// updated before it, sentinel excluded.
        async fn updates_before_sentinel(&mut self, observed: &mut Observed) -> BTreeSet<String> {
            observed
                .change(Store::Tasks, Some("tsk_zz"), ChangeKind::Updated)
                .await;
            let mut seen = BTreeSet::new();
            loop {
                let uri = self.updated().await;
                if uri == SENTINEL {
                    return seen;
                }
                seen.insert(uri);
            }
        }

        /// An explicit call through the ordinary lane.
        async fn status(&mut self) {
            let status = self.call("tools/call", json!({"name": "status"})).await;
            assert_eq!(status["result"]["isError"], false, "{status}");
        }
    }

    fn set(uris: &[&str]) -> BTreeSet<String> {
        uris.iter().map(|uri| (*uri).to_owned()).collect()
    }

    // The connector stays pending so Arc ownership includes every scheduled
    // recovery, even tasks waiting for the connection mutex rather than dialing.
    #[tokio::test]
    async fn recovery_is_coalesced_before_spawn_including_refused_calls() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let observer: Connector = Arc::new(move || {
            let notify = notify.clone();
            Box::pin(async move {
                notify.notify_one();
                std::future::pending().await
            })
        });
        let subscriptions = super::Subscriptions::new(observer, CancellationToken::new());
        subscriptions.shared.lock().uris.insert(
            "orch://status".into(),
            crate::mcp::resources::Target::Status,
        );
        subscriptions.revive();
        tokio::time::timeout(WAIT, entered.notified())
            .await
            .unwrap();
        let mut server = McpServer::new(
            Settings {
                project_root: "/unused".into(),
                identity: Identity::of(None),
                mode: Mode::ReadOnly,
                agent_context: false,
            },
            Seams {
                connector: no_daemon(Path::new("/unused")),
                observer: no_daemon(Path::new("/unused")),
                timer: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
            },
            crate::mcp::build_registry(Mode::ReadOnly).unwrap(),
        );
        server.subscriptions = subscriptions;
        let _permits = server
            .store
            .permits
            .clone()
            .acquire_many_owned(16)
            .await
            .unwrap();
        let server = Arc::new(server);
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let server = server.clone();
            calls.spawn(async move {
                for _ in 0..512 {
                    server.subscriptions.revive();
                    let result = server
                        .run_call(
                            crate::mcp::registry::Execution::Store,
                            &CancellationToken::new(),
                            |_| async { panic!("refused request executed") },
                        )
                        .await;
                    assert!(result.is_err());
                    tokio::task::yield_now().await;
                }
            });
        }
        while let Some(result) = calls.join_next().await {
            result.unwrap();
        }
        // One owner and one scheduled worker; a connection counter alone would
        // miss the old implementation's thousands of mutex waiters.
        assert_eq!(Arc::strong_count(&server.subscriptions.shared), 2);
    }

    #[tokio::test]
    async fn dropping_subscriptions_releases_a_pending_recovery() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let observer: Connector = Arc::new(move || {
            let notify = notify.clone();
            Box::pin(async move {
                notify.notify_one();
                std::future::pending().await
            })
        });
        let subscriptions = super::Subscriptions::new(observer, CancellationToken::new());
        subscriptions.shared.lock().uris.insert(
            "orch://status".into(),
            crate::mcp::resources::Target::Status,
        );
        let weak = Arc::downgrade(&subscriptions.shared);
        subscriptions.revive();
        tokio::time::timeout(WAIT, entered.notified())
            .await
            .unwrap();
        drop(subscriptions);
        let ended = tokio::time::timeout(Duration::from_secs(1), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(ended.is_ok(), "pending recovery outlived its owner");
    }

    #[tokio::test]
    async fn subscription_identifier_bytes_are_bounded_before_retention() {
        let mut b = bridge(Mode::ReadOnly, 1 << 20).await;
        let exact = format!("orch://tasks/{}", "x".repeat(256));
        b.subscribed(&exact).await;
        let oversized = format!("{exact}x");
        let reply = b.subscribe(&oversized).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        let qualified = format!("orch://agents/{}@{}", "a".repeat(256), "b".repeat(256));
        b.subscribed(&qualified).await;
        for uri in [
            format!("{qualified}b"),
            format!("orch://inbox/{}@peer", "a".repeat(257)),
            format!("orch://tasks/{}", "é".repeat(129)),
        ] {
            let reply = b.subscribe(&uri).await;
            assert_eq!(reply["error"]["code"], -32602, "{reply}");
        }
        b.unsubscribe(&exact).await;
        b.subscribed(&exact).await;
        b.server.abort();
        let _ = b.server.await;
    }

    #[tokio::test]
    async fn unread_stdout_does_not_keep_eof_teardown_alive() {
        let mut b = bridge(Mode::ReadOnly, 64).await;
        b.subscribed("orch://status").await;
        let mut o = b.daemon.session().await;
        let before = b.output_stalls.load(Ordering::SeqCst);
        o.change(Store::State, None, ChangeKind::Updated).await;
        tokio::time::timeout(WAIT, async {
            while b.output_stalls.load(Ordering::SeqCst) == before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("notification reached a pending transport write");
        b.writer.take().unwrap().shutdown().await.unwrap();
        // Keep b.lines alive and unread throughout, including server shutdown.
        let ended = tokio::time::timeout(Duration::from_secs(8), &mut b.server).await;
        if ended.is_err() {
            b.server.abort();
        }
        assert!(ended.is_ok(), "EOF teardown waited for stdout progress");
        assert!(
            ended.unwrap().unwrap().is_err(),
            "output deadline must be reported"
        );
        assert!(
            b.lifetime.upgrade().is_none(),
            "subscription tasks still own bridge"
        );
        assert!(
            b.output_lifetime.upgrade().is_none(),
            "SDK still owns output"
        );
        o.closed().await;
    }

    #[tokio::test]
    async fn pending_recovery_is_cancelled_on_eof_last_unsubscribe_and_mutex_wait() {
        for cause in ["eof", "unsubscribe", "mutex"] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let notify = entered.clone();
            let observer: Connector = Arc::new(move || {
                let notify = notify.clone();
                Box::pin(async move {
                    notify.notify_one();
                    std::future::pending().await
                })
            });
            let closed = CancellationToken::new();
            let subscriptions = super::Subscriptions::new(observer, closed.clone());
            subscriptions.shared.lock().uris.insert(
                "orch://status".into(),
                crate::mcp::resources::Target::Status,
            );
            let mutex = if cause == "mutex" {
                Some(subscriptions.shared.connect.lock().await)
            } else {
                None
            };
            subscriptions.revive();
            if cause != "mutex" {
                tokio::time::timeout(WAIT, entered.notified())
                    .await
                    .unwrap();
            } else {
                tokio::task::yield_now().await;
            }
            if cause == "unsubscribe" {
                subscriptions.unsubscribe("orch://status").await;
            } else {
                closed.cancel();
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let finished = subscriptions
                        .shared
                        .lock()
                        .recovery
                        .as_ref()
                        .unwrap()
                        .task
                        .is_finished();
                    if finished {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("recovery cancellation covers acquisition and active connector");
            for _ in 0..1024 {
                subscriptions.revive();
            }
            assert!(
                subscriptions
                    .shared
                    .lock()
                    .recovery
                    .as_ref()
                    .unwrap()
                    .task
                    .is_finished()
            );
            tokio::time::timeout(WAIT, subscriptions.shutdown())
                .await
                .unwrap();
            assert!(subscriptions.shared.lock().tasks.is_empty());
            drop(mutex);
        }
    }

    #[tokio::test]
    async fn recovery_cancellation_closes_an_unacknowledged_subscribe() {
        for cause in ["eof", "unsubscribe", "drop"] {
            let fixture = Fixture::start().await;
            let (stream, mut peer) = fake_transport();
            let (send, mut receive) = mpsc::unbounded_channel();
            let daemon = tokio::spawn(async move {
                peer.accept(Id::new("held-subscribe").unwrap())
                    .await
                    .unwrap();
                let request = peer.recv().await.unwrap().unwrap();
                assert!(matches!(request.req, Request::Subscribe(_)));
                send.send(peer).unwrap();
            });
            let stream = Arc::new(std::sync::Mutex::new(Some(stream)));
            let root = fixture.root().to_path_buf();
            let observer: Connector = Arc::new(move || {
                let stream = stream.lock().unwrap().take().expect("one recovery");
                let root = root.clone();
                Box::pin(async move {
                    Client::from_stream(
                        Project::discover(&root, None)?,
                        stream,
                        Caller {
                            kind: CallerKind::Mcp,
                            agent_id: None,
                            pid: None,
                        },
                        SessionMode::ReadOnly,
                    )
                    .await
                })
            });
            let closed = CancellationToken::new();
            let subscriptions = super::Subscriptions::new(observer, closed.clone());
            subscriptions.shared.lock().uris.insert(
                "orch://status".into(),
                crate::mcp::resources::Target::Status,
            );
            subscriptions.revive();
            let mut peer = tokio::time::timeout(WAIT, receive.recv())
                .await
                .unwrap()
                .unwrap();
            daemon.await.unwrap();
            // Handshake finished and subscribe is waiting for its acknowledgement.
            match cause {
                "eof" => {
                    closed.cancel();
                    tokio::time::timeout(WAIT, subscriptions.shutdown())
                        .await
                        .unwrap();
                }
                "unsubscribe" => {
                    subscriptions.unsubscribe("orch://status").await;
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while !subscriptions
                            .shared
                            .lock()
                            .recovery
                            .as_ref()
                            .unwrap()
                            .task
                            .is_finished()
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("last unsubscribe cancelled the pending subscribe");
                    tokio::time::timeout(WAIT, subscriptions.shutdown())
                        .await
                        .unwrap();
                }
                _ => drop(subscriptions),
            }
            let frame = tokio::time::timeout(Duration::from_secs(1), peer.recv())
                .await
                .expect("cancelled subscribe retained the daemon connection");
            assert!(
                matches!(frame, Ok(None) | Err(_)),
                "no replay or other request"
            );
        }
    }

    #[tokio::test]
    async fn recovery_failure_waits_for_an_explicit_call_before_retry() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let observer: Connector = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(orch_client::ClientError::InvalidArguments(
                    "fixture unavailable".into(),
                ))
            })
        });
        let subscriptions = super::Subscriptions::new(observer, CancellationToken::new());
        subscriptions.shared.lock().uris.insert(
            "orch://status".into(),
            crate::mcp::resources::Target::Status,
        );
        for attempt in 1..=2 {
            subscriptions.revive();
            tokio::time::timeout(WAIT, async {
                while !subscriptions
                    .shared
                    .lock()
                    .recovery
                    .as_ref()
                    .unwrap()
                    .task
                    .is_finished()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            for _ in 0..100 {
                tokio::task::yield_now().await;
            }
            assert_eq!(attempts.load(Ordering::SeqCst), attempt);
        }
        subscriptions.shutdown().await;
    }

    #[tokio::test]
    async fn healthy_eof_drains_an_already_received_response() {
        let mut b = bridge(Mode::ReadOnly, 1 << 20).await;
        b.subscribed("orch://status").await;
        let mut o = b.daemon.session().await;
        b.send(json!({"jsonrpc":"2.0","id":99,"method":"tools/call", "params":{"name":"status"}}))
            .await;
        b.writer.take().unwrap().shutdown().await.unwrap();
        let response = b.frame().await;
        assert_eq!(response["id"], 99);
        assert_eq!(response["result"]["isError"], false, "{response}");
        tokio::time::timeout(WAIT, &mut b.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(b.lifetime.upgrade().is_none());
        assert!(b.output_lifetime.upgrade().is_none());
        o.closed().await;
    }

    #[tokio::test]
    async fn unread_stdout_with_unsubscribe_ends_without_draining() {
        let mut b = bridge(Mode::ReadOnly, 64).await;
        b.subscribed("orch://status").await;
        let mut o = b.daemon.session().await;
        let before = b.output_stalls.load(Ordering::SeqCst);
        o.change(Store::State, None, ChangeKind::Updated).await;
        tokio::time::timeout(WAIT, async {
            while b.output_stalls.load(Ordering::SeqCst) == before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        b.send(json!({"jsonrpc":"2.0","id":99,"method":"resources/unsubscribe", "params":{"uri":"orch://status"}})).await;
        let ended = tokio::time::timeout(Duration::from_secs(8), &mut b.server).await;
        if ended.is_err() {
            b.server.abort();
        }
        assert!(ended.is_ok(), "unsubscribe remained behind unread stdout");
        assert!(ended.unwrap().unwrap().is_err());
        assert!(b.lifetime.upgrade().is_none());
        assert!(b.output_lifetime.upgrade().is_none());
        o.closed().await;
        // b.writer and b.lines stay open: output failure alone ends the bridge.
    }

    #[tokio::test]
    async fn initialize_advertises_resource_subscriptions() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        assert_eq!(
            b.init["capabilities"]["resources"]["subscribe"],
            json!(true),
            "{}",
            b.init
        );
        // Subscribing alone opens exactly one observation session.
        assert_eq!(b.daemon.opened(), 0);
        b.subscribed("orch://status").await;
        let observed = b.daemon.session().await;
        assert_eq!(b.daemon.opened(), 1);
        assert!(!observed.args.bus);
        assert!(observed.args.filter.is_none());
        let stores: BTreeSet<String> = observed
            .args
            .stores
            .iter()
            .map(|store| format!("{store:?}"))
            .collect();
        for store in [
            "Tasks", "Goals", "Agents", "Messages", "Context", "Runs", "Config", "State", "Remote",
        ] {
            assert!(stores.contains(store), "{store} missing from {stores:?}");
        }
        b.server.abort();
    }

    #[tokio::test]
    async fn relevant_writes_notify_unrelated_ones_do_not_and_unsubscribe_stops_them() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        for uri in [
            "orch://tasks/tsk_a",
            "orch://agents/agt_a",
            "orch://context",
            "orch://inbox/agt_me",
            SENTINEL,
        ] {
            b.subscribed(uri).await;
        }
        let mut o = b.daemon.session().await;

        // Unrelated: another task, a goal, another agent, a team.
        o.change(Store::Tasks, Some("tsk_b"), ChangeKind::Updated)
            .await;
        o.change(Store::Goals, Some("goal_x"), ChangeKind::Created)
            .await;
        o.change(Store::Agents, Some("agt_b"), ChangeKind::Deleted)
            .await;
        o.change(Store::Teams, Some("team_1"), ChangeKind::Updated)
            .await;
        assert_eq!(b.updates_before_sentinel(&mut o).await, set(&[]));

        o.change(Store::Tasks, Some("tsk_a"), ChangeKind::Updated)
            .await;
        assert_eq!(
            b.updates_before_sentinel(&mut o).await,
            set(&["orch://tasks/tsk_a"])
        );
        o.change(Store::Context, Some("k"), ChangeKind::Created)
            .await;
        o.change(Store::Messages, Some("msg_1"), ChangeKind::Created)
            .await;
        o.change(Store::Agents, Some("agt_a"), ChangeKind::Updated)
            .await;
        assert_eq!(
            b.updates_before_sentinel(&mut o).await,
            set(&[
                "orch://agents/agt_a",
                "orch://context",
                "orch://inbox/agt_me"
            ])
        );

        // A burst for one URI is coalesced, never one line per write.
        for _ in 0..50 {
            o.change(Store::Tasks, Some("tsk_a"), ChangeKind::Updated)
                .await;
        }
        o.change(Store::Tasks, Some("tsk_zz"), ChangeKind::Updated)
            .await;
        let mut count = 0;
        loop {
            match b.updated().await.as_str() {
                SENTINEL => break,
                "orch://tasks/tsk_a" => count += 1,
                other => panic!("unexpected update {other}"),
            }
        }
        assert!((1..50).contains(&count), "{count} updates for 50 writes");

        b.unsubscribe("orch://tasks/tsk_a").await;
        o.change(Store::Tasks, Some("tsk_a"), ChangeKind::Updated)
            .await;
        assert_eq!(b.updates_before_sentinel(&mut o).await, set(&[]));
        assert_eq!(b.daemon.opened(), 1, "one session for every URI");
        b.server.abort();
    }

    #[tokio::test]
    async fn deletion_and_old_goal_membership_invalidate_every_goal_view() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        for uri in [
            "orch://goals/goal_a",
            "orch://goals/goal_b",
            "orch://tasks/tsk_x",
            "orch://tasks",
        ] {
            b.subscribed(uri).await;
        }
        let mut o = b.daemon.session().await;
        // The change carries no membership: a deleted or moved task may have
        // belonged to any goal, so every goal view is a refetch candidate.
        o.change(Store::Tasks, Some("tsk_x"), ChangeKind::Deleted)
            .await;
        b.updated_all(&[
            "orch://goals/goal_a",
            "orch://goals/goal_b",
            "orch://tasks/tsk_x",
            "orch://tasks",
        ])
        .await;
        o.change(Store::Tasks, Some("tsk_new"), ChangeKind::Created)
            .await;
        b.updated_all(&["orch://goals/goal_a", "orch://goals/goal_b", "orch://tasks"])
            .await;
        b.server.abort();
    }

    #[tokio::test]
    async fn context_expiry_and_progress_reach_context_and_goal_views() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        for uri in ["orch://context", SENTINEL] {
            b.subscribed(uri).await;
        }
        let mut o = b.daemon.session().await;
        // Expiry is a daemon-internal delete of the key.
        o.change(Store::Context, Some("lease"), ChangeKind::Deleted)
            .await;
        assert_eq!(
            b.updates_before_sentinel(&mut o).await,
            set(&["orch://context"])
        );
        // The goal's progress report is its `<goal>-progress` context key.
        b.subscribed("orch://goals/goal_a").await;
        o.change(Store::Context, Some("goal_a-progress"), ChangeKind::Updated)
            .await;
        b.updated_all(&["orch://context", "orch://goals/goal_a"])
            .await;
        // A store-wide context change (no id) reaches both.
        o.change(Store::Context, None, ChangeKind::Updated).await;
        b.updated_all(&["orch://context", "orch://goals/goal_a"])
            .await;
        b.server.abort();
    }

    #[tokio::test]
    async fn status_follows_tasks_agents_runs_state_and_config() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        b.subscribed("orch://status").await;
        let mut o = b.daemon.session().await;
        for (store, id) in [
            (Store::Tasks, Some("tsk_1")),
            (Store::Agents, Some("agt_1")),
            (Store::Runs, Some("run_1")),
            (Store::State, None),
            (Store::Config, None),
        ] {
            o.change(store, id, ChangeKind::Updated).await;
            assert_eq!(b.updated().await, "orch://status", "{store:?}");
        }
        b.server.abort();
    }

    #[tokio::test]
    async fn gap_resync_new_instance_and_loss_update_every_uri_and_resubscribe_on_the_next_call() {
        let all = ["orch://context", "orch://tasks/tsk_a"];
        let mut b = bridge(Mode::Default, 1 << 20).await;
        for uri in all {
            b.subscribed(uri).await;
        }

        // A sequence gap.
        let mut o = b.daemon.session().await;
        let instance = o.instance.clone();
        o.event(
            2,
            instance,
            EventBody::Change {
                store: Store::Teams,
                id: None,
                kind: ChangeKind::Updated,
                commit_seq: 9002,
            },
        )
        .await;
        b.updated_all(&all).await;
        o.closed().await;
        // Nothing reconnects on its own; the next explicit call does, and
        // the fresh session invalidates everything again.
        assert_eq!(b.daemon.opened(), 1);
        b.status().await;
        let mut o = b.daemon.session().await;
        assert_eq!(o.instance.as_str(), "instance-2");
        b.updated_all(&all).await;
        o.change(Store::Tasks, Some("tsk_a"), ChangeKind::Updated)
            .await;
        assert_eq!(b.updated().await, "orch://tasks/tsk_a");

        // A changed instance.
        let seq = o.next_seq;
        o.event(
            seq,
            Id::new("instance-other").unwrap(),
            EventBody::Change {
                store: Store::Teams,
                id: None,
                kind: ChangeKind::Updated,
                commit_seq: 1,
            },
        )
        .await;
        b.updated_all(&all).await;
        o.closed().await;
        let read = b
            .call("resources/read", json!({"uri": "orch://context"}))
            .await;
        assert!(read.get("result").is_some(), "{read}");
        let mut o = b.daemon.session().await;
        b.updated_all(&all).await;

        // An explicit daemon resync.
        let seq = o.next_seq;
        let instance = o.instance.clone();
        o.event(
            seq,
            instance,
            EventBody::Resync {
                reason: ResyncReason::Lagged,
            },
        )
        .await;
        b.updated_all(&all).await;
        o.closed().await;
        b.status().await;
        let o = b.daemon.session().await;
        b.updated_all(&all).await;

        // Connection loss: the daemon side goes away.
        drop(o);
        b.updated_all(&all).await;
        // The stdio session stays up and keeps serving.
        b.status().await;
        let _o = b.daemon.session().await;
        b.updated_all(&all).await;
        assert_eq!(b.daemon.opened(), 5);
        b.server.abort();
    }

    #[tokio::test]
    async fn read_only_bridges_subscribe_in_their_own_identity_and_mode() {
        let mut b = bridge(Mode::ReadOnly, 1 << 20).await;
        b.subscribed("orch://tasks/tsk_a").await;
        let mut o = b.daemon.session().await;
        assert_eq!(o.session.mode, SessionMode::ReadOnly);
        assert_eq!(o.session.caller.kind, CallerKind::Mcp);
        assert_eq!(
            o.session.caller.agent_id.as_ref().map(|id| id.as_str()),
            Some("agt_me")
        );
        o.change(Store::Tasks, Some("tsk_a"), ChangeKind::Updated)
            .await;
        assert_eq!(b.updated().await, "orch://tasks/tsk_a");
        b.server.abort();
    }

    #[tokio::test]
    async fn at_most_256_distinct_uris_duplicates_are_free_and_bad_uris_are_explicit() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        for n in 0..256 {
            b.subscribed(&format!("orch://tasks/tsk_{n}")).await;
        }
        let refused = b.subscribe("orch://tasks/tsk_256").await;
        assert_eq!(refused["error"]["code"], -32000, "{refused}");
        let message = refused["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("Not executed:"), "{message}");
        assert!(message.contains("256"), "{message}");
        // A duplicate is free, at the cap too.
        b.subscribed("orch://tasks/tsk_0").await;

        let invalid = b.subscribe("orch://tasks/a b").await;
        assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
        let unknown = b.subscribe("orch://nope").await;
        assert_eq!(unknown["error"]["code"], -32002, "{unknown}");

        let mut o = b.daemon.session().await;
        // The refused URI is not tracked.
        o.change(Store::Tasks, Some("tsk_256"), ChangeKind::Updated)
            .await;
        o.change(Store::Tasks, Some("tsk_255"), ChangeKind::Updated)
            .await;
        assert_eq!(b.updated().await, "orch://tasks/tsk_255");

        b.unsubscribe("orch://tasks/tsk_0").await;
        b.subscribed("orch://tasks/tsk_256").await;
        assert_eq!(b.daemon.opened(), 1);

        // Unsubscribing everything closes the observation session.
        b.unsubscribe("orch://tasks/tsk_256").await;
        for n in 1..256 {
            b.unsubscribe(&format!("orch://tasks/tsk_{n}")).await;
        }
        o.closed().await;
        b.server.abort();
    }

    #[tokio::test]
    async fn subscribing_without_a_daemon_is_the_uniform_error_and_tracks_nothing() {
        let fixture = Fixture::start().await;
        let root = fixture.root().to_path_buf();
        let (_, daemon) = scripted(&root, Mode::Default, "agt_me");
        let mut b = bridge_with(fixture, no_daemon(&root), daemon, Mode::Default, 1 << 20).await;
        let reply = b.subscribe("orch://status").await;
        assert_eq!(reply["error"]["code"], -32001, "{reply}");
        assert_eq!(reply["error"]["data"], json!({"exit_code": 7}), "{reply}");
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(&format!("No orch daemon is running for {}", root.display())),
            "{message}"
        );
        b.server.abort();
    }

    #[tokio::test]
    async fn a_stalled_stdout_never_stalls_the_observation() {
        // 64 bytes of stdout buffer: once the test stops reading, the bridge
        // cannot write a second notification.
        let mut b = bridge(Mode::Default, 64).await;
        for uri in ["orch://context", "orch://tasks/tsk_a", SENTINEL] {
            b.subscribed(uri).await;
        }
        let mut o = b.daemon.session().await;
        // Far more frames than the client's 256-frame observation queue.
        for _ in 0..2000 {
            o.change(Store::Tasks, Some("tsk_a"), ChangeKind::Updated)
                .await;
        }
        // No overflow resync (which would update orch://context too), and the
        // burst coalesced.
        let mut count = 0;
        o.change(Store::Tasks, Some("tsk_zz"), ChangeKind::Updated)
            .await;
        loop {
            match b.updated().await.as_str() {
                SENTINEL => break,
                "orch://tasks/tsk_a" => count += 1,
                other => panic!("unexpected update {other}"),
            }
        }
        assert!((1..=3).contains(&count), "{count} updates for 2000 writes");
        assert_eq!(b.daemon.opened(), 1);
        b.server.abort();
    }

    #[tokio::test]
    async fn stdin_eof_ends_the_bridge_and_closes_the_observation() {
        let mut b = bridge(Mode::Default, 1 << 20).await;
        b.subscribed("orch://status").await;
        let mut o = b.daemon.session().await;
        let mut stdin = b.writer.take().unwrap();
        stdin.shutdown().await.unwrap();
        let started = std::time::Instant::now();
        tokio::time::timeout(WAIT, &mut b.server)
            .await
            .expect("the bridge stops at EOF")
            .unwrap()
            .unwrap();
        // Not held up by the SDK's 5 s shutdown drain waiting for the writer.
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        o.closed().await;
    }

    #[test]
    fn every_store_maps_to_the_views_whose_projection_reads_it() {
        use super::invalidates;
        use crate::mcp::resources::Target::*;
        let task = || Task("tsk_1".into());
        let goal = || Goal("goal_1".into());
        let agent = || Agent("agt_1".into());
        let remote = || Agent("agt_1@peer".into());
        let inbox = || Inbox("agt_1".into());
        let all = || {
            vec![
                Status,
                Tasks,
                task(),
                Goals,
                goal(),
                Agents,
                agent(),
                remote(),
                Context,
                inbox(),
            ]
        };
        // (store, id, the targets invalidated; every other one is not)
        let table: Vec<(Store, Option<&str>, Vec<crate::mcp::resources::Target>)> = vec![
            (
                Store::Tasks,
                Some("tsk_1"),
                vec![Status, Tasks, task(), goal()],
            ),
            (Store::Tasks, Some("tsk_2"), vec![Status, Tasks, goal()]),
            (Store::Tasks, None, vec![Status, Tasks, task(), goal()]),
            (Store::Goals, Some("goal_1"), vec![Goals, goal()]),
            (Store::Goals, Some("goal_2"), vec![Goals]),
            (Store::Goals, None, vec![Goals, goal()]),
            (Store::Agents, Some("agt_1"), vec![Status, Agents, agent()]),
            (Store::Agents, Some("agt_2"), vec![Status, Agents]),
            (Store::Agents, None, vec![Status, Agents, agent()]),
            (Store::Remote, Some("peer"), vec![Agents, remote()]),
            (Store::Remote, None, vec![Agents, remote()]),
            (Store::Messages, Some("msg_1"), vec![inbox()]),
            (Store::Messages, None, vec![inbox()]),
            (Store::Context, Some("k"), vec![Context]),
            (
                Store::Context,
                Some("goal_1-progress"),
                vec![Context, goal()],
            ),
            (Store::Context, Some("goal_2-progress"), vec![Context]),
            (Store::Context, None, vec![Context, goal()]),
            (Store::Runs, Some("run_1"), vec![Status]),
            (Store::State, None, vec![Status]),
            (Store::Config, None, vec![Status]),
            (Store::Teams, Some("team_1"), vec![]),
            (Store::Teams, None, vec![]),
        ];
        for (store, id, expected) in table {
            for target in all() {
                assert_eq!(
                    invalidates(store, id, &target),
                    expected.contains(&target),
                    "{store:?} {id:?} -> {target:?}"
                );
            }
        }
    }
}
