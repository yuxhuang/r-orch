//! One client connection, on any byte stream (design D5/D6, spec R6/R7/R20/R36).
//!
//! ```text
//! kernel peer uid == euid (before reading a byte) → pre-handshake slot
//!  → hello ≤16 KiB within 2 s → project dev/ino
//!     stop    → welcome accepted → lifecycle stop → welcome completed | reject stop_failed
//!     session → exact protocol + binary → session slot (busy otherwise) → welcome
//!  → requests: bounded NDJSON (1 MiB, depth 64) → typed decode → ServiceHandler
//! ```
//!
//! The listener and the in-process harness both mount connections through
//! [`Server::admit`], so a duplex test runs exactly this code. Accepted
//! operations are daemon-owned tasks (`ServiceHandler::execute_request`); a
//! disconnect cancels observation only and never aborts a started write.
//!
//! Logs carry the op name, request id, error code, execution class and timing
//! — never a message, a payload or serde error text (R20).

use crate::ops::{OpContext, ServiceHandler};
use base64::Engine as _;
use orch_proto::bounded::BoundedString;
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::frame::{
    ErrFrame, MAX_FRAME_BYTES, OkFrame, Part, PartFrame, PartKind, RequestFrame, RequestId,
    StreamEncoding, StreamedResult, WRITE_TIMEOUT, limits,
};
use orch_proto::handshake::{
    HANDSHAKE_TIMEOUT, HandshakeReply, HelloFrame, Intent, MAX_HANDSHAKE_BYTES, ProjectId, Reject,
    RejectCode, Session, StopState, Welcome,
};
use orch_proto::ndjson::{self, CodecError, FrameReader};
use orch_proto::ops::{CATALOGUE, OpClass, Request};
use orch_proto::path::WirePath;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

/// Largest raw result streamed as `result_chunk` parts. Above the 1 MiB frame
/// bound a result is chunked; above this it is an explicit error, never a
/// truncation (D6). Sized for the largest typed result, a 64 MiB attachment
/// read, after its base64 expansion.
pub(crate) const MAX_STREAMED_RESULT_BYTES: usize = 96 * 1024 * 1024;
/// How long a closing session waits for its accepted requests to answer.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Overflow writers that tell a connection beyond the pre-handshake budget
/// it is `busy`; beyond these the connection is closed without bytes.
const OVERFLOW_SLOTS: usize = 16;
const OVERFLOW_TIMEOUT: Duration = Duration::from_secs(1);

/// The kernel's view of the peer. `uid: None` (unavailable) fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Peer {
    pub(crate) uid: Option<u32>,
    pub(crate) pid: Option<u32>,
}

/// What this daemon answers in every welcome.
#[derive(Debug, Clone)]
pub(crate) struct ServerConfig {
    pub(crate) project_root: PathBuf,
    pub(crate) project_id: ProjectId,
    pub(crate) instance: String,
    pub(crate) pid: u32,
    pub(crate) protocol: u32,
    pub(crate) binary: String,
    pub(crate) euid: u32,
}

/// The lifecycle's stop hand-off: a stop intent asks; the lifecycle publishes
/// whether cleanup was complete.
#[derive(Debug)]
pub(crate) struct StopControl {
    requested: watch::Sender<bool>,
    outcome: watch::Sender<Option<bool>>,
}

impl StopControl {
    pub(crate) fn new() -> Self {
        StopControl {
            requested: watch::channel(false).0,
            outcome: watch::channel(None).0,
        }
    }

    pub(crate) fn request(&self) {
        self.requested.send_replace(true);
    }

    pub(crate) fn requested(&self) -> watch::Receiver<bool> {
        self.requested.subscribe()
    }

    pub(crate) fn finish(&self, clean: bool) {
        self.outcome.send_replace(Some(clean));
    }

    async fn wait(&self) -> bool {
        let mut outcome = self.outcome.subscribe();
        match outcome.wait_for(Option::is_some).await {
            Ok(value) => value.unwrap_or(false),
            Err(_) => false,
        }
    }
}

/// Live connection tasks, so shutdown can wait for terminal frames.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    live: AtomicUsize,
}

struct TrackerToken(Arc<Server>);

impl Drop for TrackerToken {
    fn drop(&mut self) {
        self.0.tracker.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Everything a connection needs. One per daemon.
pub(crate) struct Server {
    pub(crate) config: ServerConfig,
    pub(crate) handler: ServiceHandler,
    pub(crate) stop: StopControl,
    pre_handshake: Arc<Semaphore>,
    overflow: Arc<Semaphore>,
    sessions: Arc<Semaphore>,
    session_cap: u32,
    live_sessions: AtomicU64,
    busy_rejections: AtomicU64,
    drain: watch::Sender<bool>,
    tracker: Tracker,
}

impl Server {
    pub(crate) fn new(config: ServerConfig, handler: ServiceHandler) -> Arc<Self> {
        Self::with_limits(
            config,
            handler,
            limits::PRE_HANDSHAKE_SLOTS,
            limits::MAX_SESSIONS,
        )
    }

    pub(crate) fn with_limits(
        config: ServerConfig,
        handler: ServiceHandler,
        pre_handshake: usize,
        sessions: usize,
    ) -> Arc<Self> {
        Arc::new(Server {
            config,
            handler,
            stop: StopControl::new(),
            pre_handshake: Arc::new(Semaphore::new(pre_handshake)),
            overflow: Arc::new(Semaphore::new(OVERFLOW_SLOTS)),
            sessions: Arc::new(Semaphore::new(sessions)),
            session_cap: u32::try_from(sessions).unwrap_or(u32::MAX),
            live_sessions: AtomicU64::new(0),
            busy_rejections: AtomicU64::new(0),
            drain: watch::channel(false).0,
            tracker: Tracker::default(),
        })
    }

    fn live(&self) -> u64 {
        self.live_sessions.load(Ordering::SeqCst)
    }

    fn publish_counts(&self) {
        self.handler.update_session_counts(
            u32::try_from(self.live()).unwrap_or(u32::MAX),
            self.busy_rejections.load(Ordering::SeqCst),
        );
    }

    /// Stop accepting and tell every session to finish.
    pub(crate) fn begin_drain(&self) {
        self.drain.send_replace(true);
    }

    pub(crate) fn drain_signal(&self) -> watch::Receiver<bool> {
        self.drain.subscribe()
    }

    /// Wait until every connection task has ended, or `timeout`.
    pub(crate) async fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.tracker.live.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    /// Mount one connection. The kernel peer uid is checked before any byte
    /// is read or written: a different or unavailable uid is closed silently.
    pub(crate) fn admit<S>(self: &Arc<Self>, stream: S, peer: Peer)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        if peer.uid != Some(self.config.euid) {
            tracing::debug!(peer_uid = ?peer.uid, peer_pid = ?peer.pid, "peer credential rejected");
            return;
        }
        if *self.drain.borrow() {
            return;
        }
        match Arc::clone(&self.pre_handshake).try_acquire_owned() {
            Ok(slot) => {
                let token = self.enter();
                let server = Arc::clone(self);
                tokio::spawn(async move {
                    let _token = token;
                    server.run(stream, peer, slot).await;
                });
            }
            Err(_) => {
                // Pre-handshake saturation is capacity, not absence (SE-6):
                // say `busy` within a bounded budget of overflow writers.
                let Ok(permit) = Arc::clone(&self.overflow).try_acquire_owned() else {
                    return;
                };
                self.busy_rejections.fetch_add(1, Ordering::SeqCst);
                self.publish_counts();
                let reply = HandshakeReply::reject(self.busy_reject("handshake capacity is full"));
                tokio::spawn(async move {
                    let _permit = permit;
                    let mut stream = stream;
                    let _ = tokio::time::timeout(
                        OVERFLOW_TIMEOUT,
                        ndjson::write_frame(&mut stream, &reply, MAX_HANDSHAKE_BYTES),
                    )
                    .await;
                });
            }
        }
    }

    fn enter(self: &Arc<Self>) -> TrackerToken {
        self.tracker.live.fetch_add(1, Ordering::SeqCst);
        TrackerToken(Arc::clone(self))
    }

    fn busy_reject(&self, message: &str) -> Reject {
        Reject {
            code: RejectCode::Busy,
            message: bounded(message),
            sessions: Some(u32::try_from(self.live()).unwrap_or(u32::MAX)),
            cap: Some(self.session_cap),
            project_root: None,
        }
    }

    fn welcome(&self, compatible: bool, stop_state: Option<StopState>) -> HandshakeReply {
        HandshakeReply::welcome(Welcome {
            protocol: self.config.protocol,
            binary: bounded(&self.config.binary),
            project_root: wire_path(&self.config.project_root),
            project_id: self.config.project_id,
            instance: bounded(&self.config.instance),
            pid: self.config.pid,
            compatible,
            stop_state,
        })
    }

    async fn run<S>(self: Arc<Self>, stream: S, peer: Peer, slot: OwnedSemaphorePermit)
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = FrameReader::new(BufReader::new(read));
        let reject = |code, message: &str| {
            HandshakeReply::reject(Reject {
                code,
                message: bounded(message),
                sessions: None,
                cap: None,
                project_root: None,
            })
        };

        let line =
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, reader.read_line(MAX_HANDSHAKE_BYTES))
                .await
            {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    tracing::debug!(error = %codec_category(&error), "handshake frame rejected");
                    let reply = reject(RejectCode::HandshakeFailed, &error.to_string());
                    let _ = reply_frame(&mut write, &reply).await;
                    return;
                }
                Err(_) => {
                    let reply = reject(RejectCode::HandshakeFailed, "handshake deadline exceeded");
                    let _ = reply_frame(&mut write, &reply).await;
                    return;
                }
            };
        let hello = match ndjson::decode::<HelloFrame>(&line) {
            Ok(frame) => frame.hello,
            Err(error) => {
                tracing::debug!(error = %codec_category(&error), "hello rejected");
                let reply = reject(RejectCode::HandshakeFailed, &error.to_string());
                let _ = reply_frame(&mut write, &reply).await;
                return;
            }
        };
        if hello.project_id != self.config.project_id {
            let reply = HandshakeReply::reject(Reject {
                code: RejectCode::WrongProject,
                message: bounded("this daemon serves a different project"),
                sessions: None,
                cap: None,
                project_root: Some(wire_path(&self.config.project_root)),
            });
            let _ = reply_frame(&mut write, &reply).await;
            return;
        }
        let compatible =
            hello.protocol == self.config.protocol && hello.binary.as_str() == self.config.binary;

        match hello.intent {
            // RT2: after the uid and project checks, stop is honoured whatever
            // the versions; nothing but this envelope is decoded.
            Intent::Stop => {
                if reply_frame(
                    &mut write,
                    &self.welcome(compatible, Some(StopState::Accepted)),
                )
                .await
                .is_err()
                {
                    tracing::debug!("stop intent acknowledgement could not be written");
                }
                drop(slot);
                tracing::info!(peer_pid = ?peer.pid, "stop intent accepted");
                self.stop.request();
                let reply = if self.stop.wait().await {
                    self.welcome(compatible, Some(StopState::Completed))
                } else {
                    reject(
                        RejectCode::StopFailed,
                        "daemon cleanup was incomplete; recovery evidence is kept",
                    )
                };
                let _ = reply_frame(&mut write, &reply).await;
                let _ = write.shutdown().await;
                return;
            }
            Intent::Unknown if compatible => {
                let reply = reject(RejectCode::HandshakeFailed, "unsupported hello intent");
                let _ = reply_frame(&mut write, &reply).await;
                return;
            }
            Intent::Unknown | Intent::Session => {}
        }
        if !compatible {
            tracing::debug!(
                client_protocol = hello.protocol,
                "version skew; closing before any request"
            );
            let _ = reply_frame(&mut write, &self.welcome(false, None)).await;
            return;
        }
        let Ok(session) = hello.session_identity() else {
            let reply = reject(
                RejectCode::HandshakeFailed,
                "session hello has an invalid caller or mode",
            );
            let _ = reply_frame(&mut write, &reply).await;
            return;
        };
        let Ok(session_slot) = Arc::clone(&self.sessions).try_acquire_owned() else {
            self.busy_rejections.fetch_add(1, Ordering::SeqCst);
            self.publish_counts();
            let reply = HandshakeReply::reject(self.busy_reject("session capacity is full"));
            let _ = reply_frame(&mut write, &reply).await;
            return;
        };
        self.live_sessions.fetch_add(1, Ordering::SeqCst);
        self.publish_counts();
        if reply_frame(&mut write, &self.welcome(true, None))
            .await
            .is_ok()
        {
            drop(slot);
            self.serve_requests(reader, write, session, peer).await;
        }
        self.live_sessions.fetch_sub(1, Ordering::SeqCst);
        self.publish_counts();
        drop(session_slot);
    }

    async fn serve_requests<R, W>(
        self: &Arc<Self>,
        reader: FrameReader<BufReader<R>>,
        write: W,
        session: Session,
        peer: Peer,
    ) where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let mut base = OpContext::new(session);
        base.peer_pid = peer.pid;
        let session_id = base.session_id;

        let (queue, queued) = mpsc::channel(64);
        let outbox = Outbox {
            queue,
            budget: Arc::new(Semaphore::new(limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION)),
        };
        let events = crate::hub::EventOutput::new(Arc::clone(&outbox.budget));
        base.event_output = Some(Arc::clone(&events));
        let mut event_closed = events.closed();
        let writer = tokio::spawn(write_loop(write, queued, Arc::clone(&events)));
        // The reader task owns partial-line state, so cancelling a select
        // never loses half a frame.
        let (lines, mut incoming) = mpsc::channel(1);
        let reading = tokio::spawn(read_loop(reader, lines));

        let outstanding: Arc<Mutex<HashSet<RequestId>>> = Arc::default();
        let mut inflight = tokio::task::JoinSet::new();
        let mut drain = self.drain.subscribe();
        loop {
            let next = tokio::select! {
                biased;
                () = crate::session::until_set(&mut drain) => break,
                () = outbox.queue.closed() => break,
                () = until_set(&mut event_closed) => break,
                next = incoming.recv() => next,
            };
            while inflight.try_join_next().is_some() {}
            let line = match next {
                None | Some(Ok(None)) => break,
                Some(Err(error)) => {
                    tracing::debug!(session_id, error = %codec_category(&error), "frame rejected; closing session");
                    break;
                }
                Some(Ok(Some(line))) => line,
            };
            let frame = match ndjson::decode::<RequestFrame>(&line) {
                Ok(frame) => frame,
                Err(error) => {
                    let Some(id) = probe_id(&line, &error) else {
                        tracing::debug!(session_id, error = %codec_category(&error), "unattributable frame; closing session");
                        break;
                    };
                    if lock(&outstanding).contains(&id) {
                        tracing::debug!(
                            session_id,
                            id = id.0.get(),
                            "duplicate request id; closing session"
                        );
                        break;
                    }
                    let err = invalid_request(&line, &error);
                    tracing::debug!(session_id, id = id.0.get(), code = ?err.code, "request rejected before decode");
                    if !outbox.frame(&ErrFrame { id, err }).await {
                        break;
                    }
                    continue;
                }
            };
            if !lock(&outstanding).insert(frame.id) {
                tracing::debug!(
                    session_id,
                    id = frame.id.0.get(),
                    "duplicate request id; closing session"
                );
                break;
            }
            let cap = if matches!(frame.req, Request::RequestCancel(_)) {
                2 * limits::MAX_INFLIGHT_PER_SESSION
            } else {
                limits::MAX_INFLIGHT_PER_SESSION
            };
            if inflight.len() >= cap {
                lock(&outstanding).remove(&frame.id);
                let err = ErrorBody::new(
                    ErrorCode::Overloaded,
                    "too many requests in flight on this connection",
                    Execution::NotExecuted,
                );
                if !outbox.frame(&ErrFrame { id: frame.id, err }).await {
                    break;
                }
                continue;
            }
            inflight.spawn(run_request(
                self.handler.clone(),
                base.clone(),
                frame,
                outbox.clone(),
                Arc::clone(&outstanding),
            ));
        }
        reading.abort();
        let _ = reading.await;
        events.close();
        // Cancel observations first so streams end; started writes finish and
        // their cleanup is ordered behind them by the handler.
        if let Err(error) = self.handler.disconnect(session_id).await {
            tracing::debug!(session_id, code = ?error.code, "disconnect cleanup failed");
        }
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
            while inflight.join_next().await.is_some() {}
        })
        .await;
        // Requests still running are daemon-owned; detach them.
        inflight.detach_all();
        drop(outbox);
        let mut writer = writer;
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut writer)
            .await
            .is_err()
        {
            writer.abort();
            let _ = writer.await;
        }
    }
}

/// Resolve once the flag is set (or its sender is gone), holding no borrow
/// of the channel across the await.
pub(crate) async fn until_set(flag: &mut watch::Receiver<bool>) {
    let _ = flag.wait_for(|set| *set).await;
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn bounded<const MAX: usize>(text: &str) -> BoundedString<MAX> {
    let mut end = text.len().min(MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    BoundedString::new(&text[..end]).unwrap_or_else(|_| BoundedString::default())
}

fn wire_path(path: &std::path::Path) -> WirePath {
    WirePath::new(path).unwrap_or_else(|_| WirePath::new("/").expect("root is a valid path"))
}

async fn reply_frame<W: AsyncWrite + Unpin>(
    write: &mut W,
    reply: &HandshakeReply,
) -> Result<(), CodecError> {
    ndjson::write_frame(write, reply, MAX_HANDSHAKE_BYTES).await
}

/// A fixed category for logs: never serde text or payload bytes.
fn codec_category(error: &CodecError) -> &'static str {
    match error {
        CodecError::Io(_) => "transport",
        CodecError::Oversize { .. } => "oversize",
        CodecError::Unterminated => "unterminated",
        CodecError::Empty => "empty",
        CodecError::TooDeep => "too_deep",
        CodecError::Decode { .. } => "decode",
        CodecError::WriteTimeout => "write_timeout",
    }
}

/// The request id of a frame that failed typed decoding, when the frame is
/// otherwise a well-formed object with a nonzero `id`. Depth violations are
/// never probed.
fn probe_id(line: &[u8], error: &CodecError) -> Option<RequestId> {
    #[derive(serde::Deserialize)]
    struct IdOnly {
        id: RequestId,
    }
    match error {
        CodecError::Decode { .. } => serde_json::from_slice::<IdOnly>(line).ok().map(|f| f.id),
        _ => None,
    }
}

/// `invalid_arguments` naming the op when it is known, `unknown operation`
/// otherwise. The text is built from fixed strings and the catalogue only.
fn invalid_request(line: &[u8], error: &CodecError) -> ErrorBody {
    #[derive(serde::Deserialize)]
    struct OpOnly {
        req: OpName,
    }
    #[derive(serde::Deserialize)]
    struct OpName {
        op: String,
    }
    let op = serde_json::from_slice::<OpOnly>(line)
        .ok()
        .map(|frame| frame.req.op);
    let known = op
        .as_deref()
        .and_then(|op| CATALOGUE.iter().find(|(name, _)| *name == op))
        .map(|(name, _)| *name);
    let message = match (op, known) {
        (Some(_), None) => "unknown operation".to_owned(),
        (_, Some(name)) => format!("invalid request for `{name}`: {error}"),
        (None, None) => format!("invalid request: {error}"),
    };
    ErrorBody::new(
        ErrorCode::InvalidArguments,
        &message,
        Execution::NotExecuted,
    )
}

/// A frame queued for the writer, holding its share of the per-session
/// output budget until written.
struct Queued {
    bytes: Vec<u8>,
    _budget: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct Outbox {
    queue: mpsc::Sender<Queued>,
    budget: Arc<Semaphore>,
}

impl Outbox {
    async fn send(&self, bytes: Vec<u8>) -> bool {
        let Ok(count) = u32::try_from(bytes.len().min(limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION))
        else {
            return false;
        };
        let Ok(budget) = Arc::clone(&self.budget).acquire_many_owned(count).await else {
            return false;
        };
        self.queue
            .send(Queued {
                bytes,
                _budget: budget,
            })
            .await
            .is_ok()
    }

    async fn frame<T: Serialize>(&self, frame: &T) -> bool {
        match ndjson::encode(frame, MAX_FRAME_BYTES) {
            Ok(bytes) => self.send(bytes).await,
            Err(_) => false,
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: FrameReader<BufReader<R>>,
    lines: mpsc::Sender<Result<Option<Vec<u8>>, CodecError>>,
) {
    loop {
        let next = reader.read_line(MAX_FRAME_BYTES).await;
        let last = !matches!(next, Ok(Some(_)));
        if lines.send(next).await.is_err() || last {
            return;
        }
    }
}

enum Outbound {
    Reply(Queued),
    Event(crate::hub::QueuedEvent),
}
impl Outbound {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Reply(frame) => &frame.bytes,
            Self::Event(frame) => &frame.bytes,
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut write: W,
    mut queued: mpsc::Receiver<Queued>,
    events: Arc<crate::hub::EventOutput>,
) {
    let mut closed = events.closed();
    loop {
        if *closed.borrow() {
            return;
        }
        // Register before inspecting queues: a concurrent publication cannot
        // fall between the empty check and arming the wake.
        let notified = events.ready.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let frame = if let Ok(frame) = queued.try_recv() {
            Outbound::Reply(frame)
        } else if let Some(frame) = events.pop() {
            Outbound::Event(frame)
        } else {
            tokio::select! {
                biased;
                () = until_set(&mut closed) => return,
                frame = queued.recv() => match frame { Some(frame) => Outbound::Reply(frame), None => break },
                () = &mut notified => continue,
            }
        };
        let written = tokio::time::timeout(WRITE_TIMEOUT, async {
            write.write_all(frame.bytes()).await?;
            write.flush().await
        })
        .await;
        if !matches!(written, Ok(Ok(()))) {
            tracing::debug!("session write failed or timed out; closing");
            return;
        }
    }
    let _ = write.shutdown().await;
}

fn run_request(
    handler: ServiceHandler,
    base: OpContext,
    frame: RequestFrame,
    outbox: Outbox,
    outstanding: Arc<Mutex<HashSet<RequestId>>>,
) -> impl Future<Output = ()> + Send + 'static {
    let RequestFrame { id, req } = frame;
    let name = req.name();
    let class: OpClass = req.class();
    let started = Instant::now();
    let (parts, mut part_rx) = mpsc::channel::<Part>(16);
    let ctx = base.for_request(id, class, Some(parts));
    // Construct execution synchronously, before inflight.spawn can race with
    // disconnect. The handler registers observation when this future is made.
    let execution = handler.execute_request(&ctx, req);
    async move {
        let mut next_part = 0u64;
        let result = {
            tokio::pin!(execution);
            loop {
                tokio::select! {
                    biased;
                    Some(part) = part_rx.recv() => forward_part(&outbox, id, part, &mut next_part).await,
                    result = &mut execution => break result,
                }
            }
        };
        drop(ctx);
        part_rx.close();
        while let Ok(part) = part_rx.try_recv() {
            forward_part(&outbox, id, part, &mut next_part).await;
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(committed) => {
                tracing::debug!(op = name, id = id.0.get(), elapsed_ms, "ok");
                let subscription = if name == "subscribe" {
                    serde_json::from_str::<orch_proto::ops::results::SubscribeAck>(
                        committed.value.get(),
                    )
                    .ok()
                    .map(|ack| ack.subscription)
                } else {
                    None
                };
                send_ok(
                    &outbox,
                    id,
                    committed.value,
                    committed.commit_seq,
                    next_part,
                )
                .await;
                // The ack is in the writer queue before buffered events can
                // become visible. The writer always takes responses first.
                if let Some(subscription) = subscription
                    && let Some(events) = &base.event_output
                {
                    events.activate(subscription);
                }
            }
            Err(err) => {
                tracing::debug!(op = name, id = id.0.get(), code = ?err.code, execution = ?err.execution, elapsed_ms, "err");
                outbox.frame(&ErrFrame { id, err }).await;
            }
        }
        lock(&outstanding).remove(&id);
    }
}

async fn forward_part(outbox: &Outbox, id: RequestId, part: Part, next: &mut u64) {
    *next = (*next).max(part.n.saturating_add(1));
    if !outbox.frame(&PartFrame { id, part }).await {
        tracing::debug!(id = id.0.get(), "part not delivered");
    }
}

/// A terminal `ok`. Above the frame bound the encoded result is streamed once
/// as base64 `result_chunk` parts and closed by a [`StreamedResult`] (D6).
async fn send_ok(
    outbox: &Outbox,
    id: RequestId,
    value: Box<serde_json::value::RawValue>,
    commit_seq: u64,
    first_part: u64,
) {
    let frame = OkFrame {
        id,
        ok: value,
        commit_seq,
    };
    match ndjson::encode(&frame, MAX_FRAME_BYTES) {
        Ok(bytes) => {
            outbox.send(bytes).await;
            return;
        }
        Err(CodecError::Oversize { .. }) => {}
        Err(_) => {
            let err = ErrorBody::new(
                ErrorCode::General,
                "result could not be encoded",
                Execution::Completed,
            );
            outbox.frame(&ErrFrame { id, err }).await;
            return;
        }
    }
    let raw = frame.ok.get().as_bytes();
    if raw.len() > MAX_STREAMED_RESULT_BYTES {
        let err = ErrorBody::new(
            ErrorCode::General,
            &format!(
                "result is {} bytes, more than the {MAX_STREAMED_RESULT_BYTES}-byte local transport bound",
                raw.len()
            ),
            Execution::Completed,
        );
        outbox.frame(&ErrFrame { id, err }).await;
        return;
    }
    for (n, chunk) in (first_part..).zip(raw.chunks(limits::MAX_CHUNK_BYTES)) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let Ok(data) = serde_json::value::to_raw_value(&encoded) else {
            return;
        };
        let part = Part {
            n,
            kind: PartKind::ResultChunk,
            data,
        };
        if !outbox.frame(&PartFrame { id, part }).await {
            return;
        }
    }
    let digest: String = Sha256::digest(raw)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let summary = StreamedResult {
        streamed: true,
        encoding: StreamEncoding::JsonUtf8,
        bytes: raw.len() as u64,
        sha256: bounded(&digest),
    };
    let Ok(ok) = serde_json::value::to_raw_value(&summary) else {
        return;
    };
    outbox.frame(&OkFrame { id, ok, commit_seq }).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::test_support::{TestDaemon, start};
    use orch_proto::error::ConnectFailure;
    use orch_proto::handshake::{Caller, CallerKind, Hello, Reply, SessionMode, VersionSkew};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, DuplexStream};

    const EUID: fn() -> u32 = || nix::unistd::geteuid().as_raw();

    fn me() -> Peer {
        Peer {
            uid: Some(EUID()),
            pid: Some(std::process::id()),
        }
    }

    fn connect(server: &Arc<Server>, peer: Peer) -> BufReader<DuplexStream> {
        let (client, daemon) = tokio::io::duplex(4 * 1024 * 1024);
        server.admit(daemon, peer);
        BufReader::new(client)
    }

    fn hello(daemon: &TestDaemon) -> Hello {
        let caller = Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        };
        Hello::session(
            WirePath::new(daemon.root()).unwrap(),
            daemon.running.server.config.project_id,
            &caller,
            SessionMode::Normal,
        )
    }

    /// Write errors are ignored: a daemon that already closed is what
    /// several tests assert.
    async fn send(stream: &mut BufReader<DuplexStream>, line: &str) {
        let _ = stream.get_mut().write_all(line.as_bytes()).await;
        let _ = stream.get_mut().write_all(b"\n").await;
    }

    async fn send_hello(stream: &mut BufReader<DuplexStream>, hello: &Hello) {
        let frame = serde_json::to_string(&HelloFrame {
            hello: hello.clone(),
        })
        .unwrap();
        send(stream, &frame).await;
    }

    async fn line(stream: &mut BufReader<DuplexStream>) -> Option<String> {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read_line(&mut line))
            .await
            .expect("daemon answered in time")
            .unwrap();
        (n > 0).then_some(line)
    }

    async fn reply(stream: &mut BufReader<DuplexStream>) -> Reply {
        let line = line(stream).await.expect("a handshake reply");
        serde_json::from_str::<HandshakeReply>(&line)
            .unwrap()
            .into_reply()
            .expect("exactly one of welcome/reject")
    }

    async fn session(daemon: &TestDaemon) -> BufReader<DuplexStream> {
        let mut stream = connect(&daemon.running.server, me());
        send_hello(&mut stream, &hello(daemon)).await;
        match reply(&mut stream).await {
            Reply::Welcome(welcome) => assert!(welcome.compatible),
            other => panic!("expected welcome, got {other:?}"),
        }
        stream
    }

    /// The daemon closed the stream, having sent nothing more.
    async fn eof(stream: &mut BufReader<DuplexStream>) -> bool {
        matches!(closed(stream).await, Some(0))
    }

    /// The daemon closed the stream; how many bytes it sent before that.
    async fn closed(stream: &mut BufReader<DuplexStream>) -> Option<usize> {
        let mut rest = Vec::new();
        match tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut rest)).await {
            Ok(Ok(n)) => Some(n),
            _ => None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreign_or_unavailable_peer_uid_is_closed_before_any_byte() {
        let daemon = start(false, None).await;
        for peer in [
            Peer {
                uid: Some(EUID().wrapping_add(1)),
                pid: Some(1),
            },
            Peer {
                uid: None,
                pid: None,
            },
        ] {
            let mut stream = connect(&daemon.running.server, peer);
            send_hello(&mut stream, &hello(&daemon)).await;
            assert!(eof(&mut stream).await, "no byte may reach {peer:?}");
        }
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn push_ack_then_snapshot_then_matching_change() {
        let daemon = start(false, None).await;
        let mut stream = session(&daemon).await;
        send(
            &mut stream,
            r#"{"id":1,"req":{"op":"context.set","args":{"key":"before","value":"v"}}}"#,
        )
        .await;
        assert!(line(&mut stream).await.unwrap().contains("commit_seq"));
        send(
            &mut stream,
            r#"{"id":2,"req":{"op":"subscribe","args":{"stores":["context"]}}}"#,
        )
        .await;
        let ack: serde_json::Value =
            serde_json::from_str(&line(&mut stream).await.unwrap()).unwrap();
        assert_eq!(
            ack["ok"]["seq"], 0,
            "ack is cursor origin, not the baseline"
        );
        let baseline = ack["commit_seq"].as_u64().unwrap();
        assert!(baseline > 0);
        send(
            &mut stream,
            r#"{"id":3,"req":{"op":"context.list","args":{}}}"#,
        )
        .await;
        let snapshot: serde_json::Value =
            serde_json::from_str(&line(&mut stream).await.unwrap()).unwrap();
        assert_eq!(snapshot["id"], 3);
        assert!(snapshot["commit_seq"].as_u64().unwrap() >= baseline);
        send(
            &mut stream,
            r#"{"id":4,"req":{"op":"context.set","args":{"key":"after","value":"secret-canary"}}}"#,
        )
        .await;
        let mut pushed = None;
        for _ in 0..2 {
            let raw = line(&mut stream).await.unwrap();
            let frame: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if frame.get("ev").is_some() {
                assert!(
                    !raw.contains("secret-canary"),
                    "invalidations never carry entity contents"
                );
                pushed = Some(frame);
            }
        }
        let pushed = pushed.expect("write delivered an invalidation");
        assert_eq!(pushed["ev"]["seq"], 1);
        assert_eq!(pushed["ev"]["store"], "context");
        assert!(pushed["ev"]["commit_seq"].as_u64().unwrap() > baseline);
        drop(stream);
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_push_consumer_does_not_block_control() {
        let daemon = start(false, None).await;
        let (client, server_stream) = tokio::io::duplex(64);
        daemon.running.server.admit(server_stream, me());
        let mut stalled = BufReader::new(client);
        send_hello(&mut stalled, &hello(&daemon)).await;
        assert!(matches!(reply(&mut stalled).await, Reply::Welcome(_)));
        send(
            &mut stalled,
            r#"{"id":1,"req":{"op":"subscribe","args":{"stores":["state"]}}}"#,
        )
        .await;
        let ack: serde_json::Value =
            serde_json::from_str(&line(&mut stalled).await.unwrap()).unwrap();
        assert_eq!(ack["ok"]["seq"], 0);
        let boundary = daemon.running.handler.engine.operation_boundary();
        for _ in 0..600 {
            boundary
                .run(false, async {
                    boundary.note_store(orch_engine::container::commit::CommitStore::State);
                })
                .await;
        }
        let mut control = session(&daemon).await;
        send(
            &mut control,
            r#"{"id":2,"req":{"op":"dispatch.set","args":{"dispatch":"off"}}}"#,
        )
        .await;
        let response = tokio::time::timeout(Duration::from_secs(2), line(&mut control))
            .await
            .expect("control remains responsive")
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], 2);
        assert!(response.get("ok").is_some(), "{response}");
        drop(stalled);
        drop(control);
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn welcome_then_typed_request_and_commit_seq() {
        let daemon = start(false, None).await;
        let mut stream = connect(&daemon.running.server, me());
        send_hello(&mut stream, &hello(&daemon)).await;
        let Reply::Welcome(welcome) = reply(&mut stream).await else {
            panic!("expected welcome");
        };
        assert!(welcome.compatible);
        assert_eq!(welcome.protocol, orch_proto::LOCAL_PROTOCOL);
        assert_eq!(welcome.binary.as_str(), orch_proto::BINARY_VERSION);
        assert_eq!(welcome.pid, std::process::id());
        assert_eq!(
            welcome.instance.as_str(),
            daemon.running.server.config.instance
        );
        assert_eq!(welcome.stop_state, None);

        send(
            &mut stream,
            r#"{"id":7,"req":{"op":"context.set","args":{"key":"k","value":"v"}}}"#,
        )
        .await;
        let ok: serde_json::Value =
            serde_json::from_str(&line(&mut stream).await.unwrap()).unwrap();
        assert_eq!(ok["id"], 7);
        let seq = ok["commit_seq"].as_u64().unwrap();
        assert!(seq > 0, "a mutation is assigned an ordinal: {ok}");
        send(
            &mut stream,
            r#"{"id":8,"req":{"op":"context.get","args":{"key":"k"}}}"#,
        )
        .await;
        let ok: serde_json::Value =
            serde_json::from_str(&line(&mut stream).await.unwrap()).unwrap();
        assert_eq!(ok["id"], 8);
        assert!(ok["commit_seq"].as_u64().unwrap() >= seq, "{ok}");
        drop(stream);
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skew_in_either_field_and_direction_closes_before_any_request() {
        let daemon = start(false, None).await;
        let mut variants = Vec::new();
        let mut newer_protocol = hello(&daemon);
        newer_protocol.protocol += 1;
        variants.push(newer_protocol);
        let mut older_binary = hello(&daemon);
        older_binary.binary = BoundedString::new("0.0.1").unwrap();
        variants.push(older_binary);
        for client in variants {
            let mut stream = connect(&daemon.running.server, me());
            send_hello(&mut stream, &client).await;
            let Reply::Welcome(welcome) = reply(&mut stream).await else {
                panic!("skew is a welcome with compatible=false");
            };
            assert!(!welcome.compatible);
            // A request after the skew welcome is never decoded or answered.
            send(
                &mut stream,
                r#"{"id":1,"req":{"op":"daemon.status","args":{}}}"#,
            )
            .await;
            assert!(eof(&mut stream).await);
            let failure = ConnectFailure::VersionSkew {
                project_root: welcome.project_root.to_string(),
                skew: VersionSkew {
                    client_protocol: client.protocol,
                    client_binary: client.binary.to_string(),
                    daemon_protocol: welcome.protocol,
                    daemon_binary: welcome.binary.to_string(),
                },
            };
            let text = failure.render(false);
            assert!(text.contains(&format!(
                "daemon is orch {} protocol {}",
                orch_proto::BINARY_VERSION,
                orch_proto::LOCAL_PROTOCOL
            )));
            assert!(text.contains(&format!(
                "this client is orch {} protocol {}",
                client.binary.as_str(),
                client.protocol
            )));
            assert!(text.contains("Start it with: orch daemon stop, then orch daemon start"));
        }
        assert_eq!(daemon.running.shutdown().await.code, 0);

        // The other direction: an older daemon answers this build's client.
        let older = start(false, Some((1, "0.12.0".into()))).await;
        let mut stream = connect(&older.running.server, me());
        send_hello(&mut stream, &hello(&older)).await;
        let Reply::Welcome(welcome) = reply(&mut stream).await else {
            panic!("expected welcome");
        };
        assert!(!welcome.compatible);
        assert_eq!(welcome.binary.as_str(), "0.12.0");
        assert_eq!(older.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_intent_crosses_version_skew_and_reports_completion() {
        let daemon = start(true, Some((99, "9.9.9".into()))).await;
        let root = daemon.root();
        let state = root.join(".orchestry");
        let mut stream = connect(&daemon.running.server, me());
        let stop = Hello::stop(
            WirePath::new(&root).unwrap(),
            daemon.running.server.config.project_id,
        );
        send_hello(&mut stream, &stop).await;
        let Reply::Welcome(accepted) = reply(&mut stream).await else {
            panic!("stop is accepted");
        };
        assert!(!accepted.compatible, "versions are still reported");
        assert_eq!(accepted.stop_state, Some(StopState::Accepted));

        let TestDaemon {
            running, scratch, ..
        } = daemon;
        assert_eq!(running.wait_for_stop(None).await, "stop intent");
        let shutdown = tokio::spawn(running.shutdown());
        let Reply::Welcome(completed) = reply(&mut stream).await else {
            panic!("clean stop completes");
        };
        assert_eq!(completed.stop_state, Some(StopState::Completed));
        assert_eq!(shutdown.await.unwrap(), crate::lifecycle::DaemonExit::CLEAN);
        assert!(!scratch.path().join("s").exists());
        assert!(!state.join("daemon.json").exists());
        assert!(!state.join("orchestry.lock").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wrong_project_is_rejected_with_the_served_root() {
        let daemon = start(false, None).await;
        let mut other = hello(&daemon);
        other.project_id.ino = other.project_id.ino.wrapping_add(1);
        for intent in [Intent::Session, Intent::Stop] {
            other.intent = intent;
            let mut stream = connect(&daemon.running.server, me());
            send_hello(&mut stream, &other).await;
            let Reply::Reject(reject) = reply(&mut stream).await else {
                panic!("expected reject");
            };
            assert_eq!(reject.code, RejectCode::WrongProject);
            assert_eq!(reject.project_root.unwrap().as_path(), daemon.root());
        }
        assert!(
            !*daemon.running.server.stop.requested().borrow(),
            "no stop for another project"
        );
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_session_cap_is_busy_not_absent_and_idle_handshakes_are_separate() {
        let daemon = start(false, None).await;
        let server = Server::with_limits(
            daemon.running.server.config.clone(),
            daemon.running.handler.clone(),
            1,
            1,
        );
        let mut first = connect(&server, me());
        send_hello(&mut first, &hello(&daemon)).await;
        assert!(matches!(reply(&mut first).await, Reply::Welcome(w) if w.compatible));

        let mut second = connect(&server, me());
        send_hello(&mut second, &hello(&daemon)).await;
        let Reply::Reject(busy) = reply(&mut second).await else {
            panic!("expected busy");
        };
        assert_eq!(busy.code, RejectCode::Busy);
        assert_eq!((busy.sessions, busy.cap), (Some(1), Some(1)));
        let text = ConnectFailure::Busy {
            project_root: "/p".into(),
            pid: Some(42),
            sessions: 1,
            cap: 1,
        }
        .render(false);
        assert!(
            text.contains("daemon_busy") && !text.contains("No orch daemon"),
            "{text}"
        );
        assert!(!text.contains("orch daemon start"), "{text}");

        // An idle pre-handshake connection holds the only handshake slot; the
        // next connection is told `busy` at once, and the live session above
        // is unaffected.
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _idle = connect(&server, me());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut third = connect(&server, me());
        let Reply::Reject(busy) = reply(&mut third).await else {
            panic!("expected busy");
        };
        assert_eq!(busy.code, RejectCode::Busy);
        assert!(server.busy_rejections.load(Ordering::SeqCst) >= 2);
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_handshake_is_closed_after_the_deadline() {
        let daemon = start(false, None).await;
        let mut stream = connect(&daemon.running.server, me());
        let started = Instant::now();
        let Reply::Reject(reject) = reply(&mut stream).await else {
            panic!("expected handshake_failed");
        };
        assert_eq!(reject.code, RejectCode::HandshakeFailed);
        assert!(started.elapsed() >= HANDSHAKE_TIMEOUT);
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_requests_get_attributed_errors_and_protocol_violations_close() {
        let daemon = start(false, None).await;
        let mut stream = session(&daemon).await;

        send(
            &mut stream,
            r#"{"id":1,"req":{"op":"no.such.op","args":{}}}"#,
        )
        .await;
        let err: ErrFrame = serde_json::from_str(&line(&mut stream).await.unwrap()).unwrap();
        assert_eq!(err.id.0.get(), 1);
        assert_eq!(err.err.code, ErrorCode::InvalidArguments);
        assert_eq!(err.err.message.as_str(), "unknown operation");
        assert_eq!(err.err.execution, Execution::NotExecuted);

        send(
            &mut stream,
            r#"{"id":2,"req":{"op":"context.get","args":{"key":"k","extra":"secret-canary"}}}"#,
        )
        .await;
        let err: ErrFrame = serde_json::from_str(&line(&mut stream).await.unwrap()).unwrap();
        assert_eq!(err.id.0.get(), 2);
        assert!(
            err.err
                .message
                .as_str()
                .starts_with("invalid request for `context.get`")
        );
        assert!(
            !err.err.message.as_str().contains("secret-canary"),
            "no payload echo"
        );

        // The session survived both; then each violation closes it.
        send(
            &mut stream,
            r#"{"id":3,"req":{"op":"daemon.status","args":{}}}"#,
        )
        .await;
        assert!(line(&mut stream).await.unwrap().contains(r#""id":3"#));

        let deep = format!(
            r#"{{"id":4,"req":{{"op":"context.get","args":{{"key":{}1{}}}}}}}"#,
            "[".repeat(65),
            "]".repeat(65)
        );
        let violations = [
            deep,
            r#"{"req":{"op":"daemon.status","args":{}}}"#.to_owned(),
            r#"{"id":0,"req":{"op":"daemon.status","args":{}}}"#.to_owned(),
            "not json".to_owned(),
            String::new(),
            format!(r#"{{"id":5,"pad":"{}"}}"#, "x".repeat(MAX_FRAME_BYTES)),
        ];
        for violation in violations {
            let mut stream = session(&daemon).await;
            send(&mut stream, &violation).await;
            assert!(
                closed(&mut stream).await.is_some(),
                "closes on {:.40}",
                violation
            );
        }
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_outstanding_id_closes_the_session() {
        let daemon = start(false, None).await;
        let mut stream = session(&daemon).await;
        // logs.follow stays outstanding until cancelled.
        send(
            &mut stream,
            r#"{"id":9,"req":{"op":"logs.follow","args":{}}}"#,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        send(
            &mut stream,
            r#"{"id":9,"req":{"op":"daemon.status","args":{}}}"#,
        )
        .await;
        // Closed; at most the cancelled stream's own terminal precedes EOF,
        // never an answer for the duplicate.
        let mut rest = String::new();
        let read =
            tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut rest)).await;
        assert!(matches!(read, Ok(Ok(_))), "session must close");
        assert!(
            !rest.contains("\"pid\""),
            "the duplicate was answered: {rest}"
        );
        assert_eq!(daemon.running.shutdown().await.code, 0);
    }

    #[tokio::test]
    async fn oversized_results_stream_as_checksummed_chunks() {
        let (queue, mut queued) = mpsc::channel(64);
        let outbox = Outbox {
            queue,
            budget: Arc::new(Semaphore::new(limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION)),
        };
        let big = serde_json::to_string(&"x".repeat(3 * 1024 * 1024)).unwrap();
        let value = serde_json::value::RawValue::from_string(big.clone()).unwrap();
        let id = RequestId(std::num::NonZeroU64::new(5).unwrap());
        let sender = tokio::spawn(async move { send_ok(&outbox, id, value, 42, 3).await });
        let mut frames = Vec::new();
        while let Some(frame) = queued.recv().await {
            assert!(frame.bytes.len() <= MAX_FRAME_BYTES + 1);
            frames.push(frame.bytes);
        }
        sender.await.unwrap();
        let (last, parts) = frames.split_last().unwrap();
        let mut rebuilt = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let part: PartFrame = serde_json::from_slice(part).unwrap();
            assert_eq!(part.part.kind, PartKind::ResultChunk);
            assert_eq!(part.part.n, 3 + i as u64, "n continues after earlier parts");
            let chunk: String = serde_json::from_str(part.part.data.get()).unwrap();
            rebuilt.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(chunk)
                    .unwrap(),
            );
        }
        assert_eq!(rebuilt, big.as_bytes());
        let ok: OkFrame = serde_json::from_slice(last).unwrap();
        assert_eq!(ok.commit_seq, 42);
        let summary: StreamedResult = serde_json::from_str(ok.ok.get()).unwrap();
        assert!(summary.streamed);
        assert_eq!(summary.bytes, big.len() as u64);
        let digest: String = Sha256::digest(big.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(summary.sha256.as_str(), digest);
    }
}
