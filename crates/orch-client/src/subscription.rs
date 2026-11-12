//! A dedicated, bounded observation lane. Ordinary request lanes never enter it.
use crate::{Client, ClientError};
use orch_proto::bounded::Id;
use orch_proto::error::{ErrorBody, NoDaemonDetail};
use orch_proto::frame::{
    ChangeKind, Event, EventBody, MAX_FRAME_BYTES, RequestFrame, RequestId, ResyncReason,
    ServerFrame, Store, limits,
};
use orch_proto::handshake;
use orch_proto::ndjson;
use orch_proto::ops::{
    Request,
    args::{SubscribeArgs, UnsubscribeArgs},
    results::SubscribeAck,
};
use std::num::NonZeroU64;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::WriteHalf;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinHandle;

/// Observation failures never classify a mutation's outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscriptionError {
    #[error("{}", orch_proto::path::escape_controls(.0.message.as_str()))]
    Remote(Box<ErrorBody>),
    #[error("subscription transport closed; reconnect and refetch subscribed views")]
    TransportClosed,
    #[error("invalid subscription frame; reconnect and refetch subscribed views")]
    InvalidFrame,
    #[error("invalid subscription acknowledgement")]
    InvalidAck,
    #[error("subscription completion timed out")]
    CompletionTimeout,
    #[error("subscription buffer overflow; reconnect and refetch subscribed views")]
    BufferOverflow,
    #[error("subscription ended; reconnect and refetch subscribed views")]
    Ended,
}

/// Every reason invalidates all subscribed caches. A handle ends after emitting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionResyncReason {
    Server(ResyncReason),
    SequenceGap { expected: u64, received: u64 },
    InstanceChanged { expected: Id, received: Id },
    BufferOverflow,
}

/// Ordered observations, never entity contents in a change notification.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionEvent {
    Change {
        seq: u64,
        store: Store,
        id: Option<Id>,
        kind: ChangeKind,
        commit_seq: u64,
    },
    Bus {
        seq: u64,
        event: Box<orch_domain::OrchestratorEvent>,
    },
    ResyncRequired {
        reason: SubscriptionResyncReason,
    },
}

type SharedWriter = Arc<Mutex<Option<WriteHalf<Box<dyn crate::Transport>>>>>;

struct QueuedFrame {
    frame: ServerFrame,
    _bytes: OwnedSemaphorePermit,
}

/// Owns one observation session. Dropping it closes only observation; it cannot
/// cancel agents or replay mutations. The reader starts before this is returned.
/// It holds at most 256 queued frames and 4 MiB encoded queued bytes, plus one
/// bounded frame being read. Overflow closes the transport and requires resync.
pub struct Subscription {
    subscription: u64,
    instance: Id,
    baseline_commit_seq: u64,
    next_seq: u64,
    unsubscribe_id: RequestId,
    writer: SharedWriter,
    frames: mpsc::Receiver<QueuedFrame>,
    reader: JoinHandle<Result<(), SubscriptionError>>,
    overflow: Arc<AtomicBool>,
    ended: bool,
}

impl Client {
    /// Consume a fresh observation connection. Mutations and reads use a separate
    /// `Client`; this is deliberately not a generic RPC multiplexer.
    pub async fn into_subscription(self, args: SubscribeArgs) -> Result<Subscription, ClientError> {
        if self.request_may_have_been_sent {
            return Err(self.unknown());
        }
        let mut client = self;
        let id = RequestId(
            NonZeroU64::new(client.next_id)
                .ok_or_else(|| client.project.failure(NoDaemonDetail::HandshakeFailed))?,
        );
        let unsubscribe_id = RequestId(
            id.0.checked_add(1)
                .ok_or_else(|| client.project.failure(NoDaemonDetail::HandshakeFailed))?,
        );
        let (ack, baseline) = tokio::time::timeout(handshake::STOP_COMPLETION_TIMEOUT, async {
            ndjson::write_frame(
                &mut client.writer,
                &RequestFrame {
                    id,
                    req: Request::Subscribe(args),
                },
                MAX_FRAME_BYTES,
            )
            .await
            .map_err(codec_error)?;
            match read_frame(&mut client.io).await? {
                ServerFrame::Ok(frame) if frame.id == id => {
                    let ack: SubscribeAck = ndjson::decode(frame.ok.get().as_bytes())
                        .map_err(|_| SubscriptionError::InvalidAck)?;
                    Ok((ack, frame.commit_seq))
                }
                ServerFrame::Err(frame) if frame.id == id => {
                    Err(SubscriptionError::Remote(Box::new(frame.err)))
                }
                _ => Err(SubscriptionError::InvalidAck),
            }
        })
        .await
        .map_err(|_| client.project.failure(NoDaemonDetail::Timeout))?
        .map_err(|error| error.connect_error(&client.project))?;
        let instance = Id::new(ack.instance)
            .map_err(|_| client.project.failure(NoDaemonDetail::HandshakeFailed))?;
        if ack.seq != 0 || ack.subscription == 0 || instance != client.welcome.instance {
            return Err(client
                .project
                .failure(NoDaemonDetail::HandshakeFailed)
                .into());
        }
        let (send, frames) = mpsc::channel(limits::MAX_QUEUED_EVENTS_PER_SUBSCRIPTION);
        let overflow = Arc::new(AtomicBool::new(false));
        let over = overflow.clone();
        let writer = Arc::new(Mutex::new(Some(client.writer)));
        let reader_writer = writer.clone();
        let reader = tokio::spawn(async move {
            let result = receive_frames(client.io, send, over).await;
            // Both generic split halves must be dropped: dropping only the read
            // half leaves the shared transport alive and retains the daemon slot.
            reader_writer.lock().await.take();
            result
        });
        Ok(Subscription {
            subscription: ack.subscription,
            instance,
            baseline_commit_seq: baseline,
            next_seq: 1,
            unsubscribe_id,
            writer,
            frames,
            reader,
            overflow,
            ended: false,
        })
    }
}

impl Subscription {
    pub fn subscription(&self) -> u64 {
        self.subscription
    }
    pub fn instance(&self) -> &Id {
        &self.instance
    }
    /// The subscribe terminal's `ok.commit_seq`, independent of event sequence.
    pub fn baseline_commit_seq(&self) -> u64 {
        self.baseline_commit_seq
    }

    /// Cancellation-safe, including mid-frame: the owned reader retains the
    /// partial bytes while a cancelled `next` only drops a channel receive.
    /// A resync item ends this handle; open a new observation and refetch.
    pub async fn next(&mut self) -> Result<SubscriptionEvent, SubscriptionError> {
        if self.ended {
            return Err(SubscriptionError::Ended);
        }
        if self.overflow.load(Ordering::Acquire) {
            return Ok(self.resync(SubscriptionResyncReason::BufferOverflow));
        }
        let frame = self.receive().await?;
        if self.overflow.load(Ordering::Acquire) {
            return Ok(self.resync(SubscriptionResyncReason::BufferOverflow));
        }
        match frame {
            ServerFrame::Event(frame) => self.event(frame.ev),
            _ => {
                self.end();
                Err(SubscriptionError::InvalidFrame)
            }
        }
    }

    /// Consume this observation, discard already-in-flight events and wait for
    /// the correlated null acknowledgement. No ordinary request lane is used.
    pub async fn unsubscribe(mut self) -> Result<(), SubscriptionError> {
        if self.ended {
            return Err(SubscriptionError::Ended);
        }
        let result = tokio::time::timeout(handshake::STOP_COMPLETION_TIMEOUT, async {
            {
                let mut guard = self.writer.lock().await;
                let writer = guard.as_mut().ok_or(SubscriptionError::TransportClosed)?;
                ndjson::write_frame(
                    writer,
                    &RequestFrame {
                        id: self.unsubscribe_id,
                        req: Request::Unsubscribe(UnsubscribeArgs {
                            subscription: self.subscription,
                        }),
                    },
                    MAX_FRAME_BYTES,
                )
                .await
                .map_err(codec_error)?;
            }
            loop {
                match self.receive().await? {
                    ServerFrame::Event(frame) if frame.ev.subscription == self.subscription => {}
                    ServerFrame::Ok(frame)
                        if frame.id == self.unsubscribe_id && frame.ok.get() == "null" =>
                    {
                        return Ok(());
                    }
                    ServerFrame::Err(frame) if frame.id == self.unsubscribe_id => {
                        return Err(SubscriptionError::Remote(Box::new(frame.err)));
                    }
                    _ => return Err(SubscriptionError::InvalidFrame),
                }
            }
        })
        .await
        .map_err(|_| SubscriptionError::CompletionTimeout)?;
        // `receive` already joined the reader when its channel closed.
        if !self.ended {
            self.reader.abort();
            let _ = (&mut self.reader).await;
        }
        result
    }

    async fn receive(&mut self) -> Result<ServerFrame, SubscriptionError> {
        if let Some(queued) = self.frames.recv().await {
            return Ok(queued.frame);
        }
        self.ended = true;
        match (&mut self.reader).await {
            Ok(Err(error)) => Err(error),
            _ => Err(SubscriptionError::TransportClosed),
        }
    }

    fn event(&mut self, event: Event) -> Result<SubscriptionEvent, SubscriptionError> {
        if event.subscription != self.subscription {
            self.end();
            return Err(SubscriptionError::InvalidFrame);
        }
        if event.instance != self.instance {
            return Ok(self.resync(SubscriptionResyncReason::InstanceChanged {
                expected: self.instance.clone(),
                received: event.instance,
            }));
        }
        if let EventBody::Resync { reason } = event.body {
            return Ok(self.resync(SubscriptionResyncReason::Server(reason)));
        }
        if event.seq != self.next_seq {
            return Ok(self.resync(SubscriptionResyncReason::SequenceGap {
                expected: self.next_seq,
                received: event.seq,
            }));
        }
        let Some(next) = self.next_seq.checked_add(1) else {
            self.end();
            return Err(SubscriptionError::InvalidFrame);
        };
        self.next_seq = next;
        Ok(match event.body {
            EventBody::Change {
                store,
                id,
                kind,
                commit_seq,
            } => SubscriptionEvent::Change {
                seq: event.seq,
                store,
                id,
                kind,
                commit_seq,
            },
            EventBody::Bus(event_body) => SubscriptionEvent::Bus {
                seq: event.seq,
                event: event_body,
            },
            EventBody::Resync { .. } => unreachable!(),
        })
    }

    fn resync(&mut self, reason: SubscriptionResyncReason) -> SubscriptionEvent {
        self.end();
        SubscriptionEvent::ResyncRequired { reason }
    }
    fn end(&mut self) {
        self.ended = true;
        self.reader.abort();
        if let Ok(mut writer) = self.writer.try_lock() {
            writer.take();
        }
        self.frames.close();
        while self.frames.try_recv().is_ok() {}
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn codec_error(error: ndjson::CodecError) -> SubscriptionError {
    match error {
        ndjson::CodecError::Io(_) | ndjson::CodecError::WriteTimeout => {
            SubscriptionError::TransportClosed
        }
        _ => SubscriptionError::InvalidFrame,
    }
}

async fn read_frame(io: &mut crate::Reader) -> Result<ServerFrame, SubscriptionError> {
    let bytes = io
        .read_line(MAX_FRAME_BYTES)
        .await
        .map_err(codec_error)?
        .ok_or(SubscriptionError::TransportClosed)?;
    ndjson::check_depth(&bytes, orch_proto::frame::MAX_DEPTH).map_err(codec_error)?;
    ServerFrame::decode(&bytes).map_err(|_| SubscriptionError::InvalidFrame)
}

async fn receive_frames(
    mut io: crate::Reader,
    send: mpsc::Sender<QueuedFrame>,
    overflow: Arc<AtomicBool>,
) -> Result<(), SubscriptionError> {
    let budget = Arc::new(Semaphore::new(limits::MAX_QUEUED_OUTPUT_BYTES_PER_SESSION));
    loop {
        let bytes = io
            .read_line(MAX_FRAME_BYTES)
            .await
            .map_err(codec_error)?
            .ok_or(SubscriptionError::TransportClosed)?;
        let permit = match budget
            .clone()
            .try_acquire_many_owned((bytes.len() + 1) as u32)
        {
            Ok(permit) => permit,
            Err(_) => {
                overflow.store(true, Ordering::Release);
                return Err(SubscriptionError::BufferOverflow);
            }
        };
        ndjson::check_depth(&bytes, orch_proto::frame::MAX_DEPTH).map_err(codec_error)?;
        let frame = ServerFrame::decode(&bytes).map_err(|_| SubscriptionError::InvalidFrame)?;
        match send.try_send(QueuedFrame {
            frame,
            _bytes: permit,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                overflow.store(true, Ordering::Release);
                return Err(SubscriptionError::BufferOverflow);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
        }
    }
}

impl SubscriptionError {
    fn connect_error(self, project: &crate::Project) -> ClientError {
        match self {
            Self::Remote(body) => ClientError::Remote(*body),
            Self::TransportClosed => project.failure(NoDaemonDetail::ConnectionLost).into(),
            Self::CompletionTimeout => project.failure(NoDaemonDetail::Timeout).into(),
            _ => project.failure(NoDaemonDetail::HandshakeFailed).into(),
        }
    }
}
