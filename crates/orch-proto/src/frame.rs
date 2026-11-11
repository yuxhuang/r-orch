//! Session frames after a compatible handshake (design D6, spec R7).
//!
//! Every frame here rejects unknown fields. Request ids are nonzero, scoped to
//! one connection and never reused while outstanding. Each accepted id gets
//! exactly one terminal frame ([`OkFrame`] or [`ErrFrame`]), preceded by any
//! number of [`PartFrame`]s with increasing `n`. Pushes ([`EventFrame`]) carry
//! no request id.

use crate::bounded::Id;
use crate::error::ErrorBody;
use crate::ops::Request;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::num::NonZeroU64;
use std::time::Duration;

/// Largest session frame in bytes, excluding its LF (the orch-peer bound).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Deepest JSON nesting accepted, checked by a pre-parse scan
/// ([`crate::ndjson::check_depth`]), not by serde_json's recursion limit.
pub const MAX_DEPTH: usize = 64;
/// Writing one frame may take at most this long.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connection and admission limits (design D6, D11; owner-approved defaults).
pub mod limits {
    use std::time::Duration;

    /// Connections still in the handshake; separate from live sessions.
    pub const PRE_HANDSHAKE_SLOTS: usize = 16;
    /// Live ordinary sessions per daemon. The stop intent needs no slot.
    pub const MAX_SESSIONS: usize = 64;
    /// Outstanding request ids per connection.
    pub const MAX_INFLIGHT_PER_SESSION: usize = 16;
    /// Ordinary state operations queued per project.
    pub const MAX_QUEUED_OPERATIONS: usize = 128;
    /// Encoded bytes of queued requests per project.
    pub const MAX_QUEUED_REQUEST_BYTES: usize = 64 * 1024 * 1024;
    /// An ordinary operation not admitted within this is `not_executed`.
    pub const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
    pub const MAX_SUBSCRIPTIONS_PER_SESSION: usize = 8;
    pub const MAX_QUEUED_EVENTS_PER_SUBSCRIPTION: usize = 256;
    /// Queued outbound bytes per connection across all subscriptions.
    pub const MAX_QUEUED_OUTPUT_BYTES_PER_SESSION: usize = 4 * 1024 * 1024;
    /// Raw bytes per transfer or result chunk, before base64.
    pub const MAX_CHUNK_BYTES: usize = 256 * 1024;
    /// Assembled bytes of one inbound transfer.
    pub const MAX_TRANSFER_BYTES: usize = 16 * 1024 * 1024;
    /// Staged transfer bytes across the daemon.
    pub const MAX_STAGED_BYTES: usize = 64 * 1024 * 1024;
    /// A transfer with no chunk for this long is discarded.
    pub const TRANSFER_INACTIVITY: Duration = Duration::from_secs(10);
}

/// A nonzero request id, scoped to one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub NonZeroU64);

/// `{"id":1,"req":{"op":"task.get","args":{...}}}`, client to daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    pub id: RequestId,
    pub req: Request,
}

/// `{"id":1,"ok":<result>,"commit_seq":42}`. `ok` is the operation's typed
/// result, serialized once; the client decodes it with the result type of the
/// operation it sent under `id`. A void result is `null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OkFrame {
    pub id: RequestId,
    pub ok: Box<RawValue>,
    /// The daemon-instance-scoped commit ordinal: the one assigned to this
    /// mutation, or the one observed by this read. Not a stored revision.
    pub commit_seq: u64,
}

/// `{"id":1,"err":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrFrame {
    pub id: RequestId,
    pub err: ErrorBody,
}

/// `{"id":1,"part":{...}}`: one item of a streamed response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartFrame {
    pub id: RequestId,
    pub part: Part,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// Starts at 0 and increases by one per part of the same request.
    pub n: u64,
    pub kind: PartKind,
    /// `log`: a log line DTO; `event`: an orchestrator event; `result_chunk`:
    /// `{"data_base64": "..."}` of at most [`limits::MAX_CHUNK_BYTES`] raw.
    pub data: Box<RawValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Log,
    Event,
    ResultChunk,
}

/// The terminal `ok` of a streamed large result: the client concatenates the
/// `result_chunk` parts, checks length and digest, then decodes the bytes as
/// the operation's result. An incomplete stream is an error, never success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamedResult {
    /// Always `true`; distinguishes this from an inline result.
    pub streamed: bool,
    pub encoding: StreamEncoding,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the reassembled bytes.
    pub sha256: Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEncoding {
    JsonUtf8,
}

/// `{"ev":{...}}`: a subscription push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFrame {
    pub ev: Event,
}

/// One push on one subscription. `seq` starts at 1 per subscription and
/// increments for every delivered-or-dropped matching event, so a hole is a
/// gap; `instance` changing means a new daemon lifetime. Neither is durable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "EventWire", into = "EventWire")]
pub struct Event {
    pub subscription: u64,
    pub instance: Id,
    pub seq: u64,
    pub body: EventBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventBody {
    /// A committed store change. Identity and action only: never entity
    /// contents, which may carry secrets. Coarsely invalidates a cache.
    Change {
        store: Store,
        id: Id,
        kind: ChangeKind,
        commit_seq: u64,
    },
    /// A domain event from the orchestrator's event bus.
    Bus(Box<orch_domain::OrchestratorEvent>),
    /// Events were lost; refetch every subscribed view.
    Resync { reason: ResyncReason },
}

/// Stores a subscription can watch (design D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Store {
    Tasks,
    Goals,
    Agents,
    Messages,
    Context,
    Teams,
    Runs,
    Config,
    State,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Updated,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    /// The subscriber's queue overflowed.
    Lagged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResyncWire {
    reason: ResyncReason,
}

/// Wire shape of [`Event`]: exactly one of a change, `bus` or `resync`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    subscription: u64,
    instance: Id,
    seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store: Option<Store>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<ChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bus: Option<Box<orch_domain::OrchestratorEvent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resync: Option<ResyncWire>,
}

impl TryFrom<EventWire> for Event {
    type Error = &'static str;
    fn try_from(w: EventWire) -> Result<Self, Self::Error> {
        let body = match (w.store, w.id, w.kind, w.commit_seq, w.bus, w.resync) {
            (Some(store), Some(id), Some(kind), Some(commit_seq), None, None) => {
                EventBody::Change {
                    store,
                    id,
                    kind,
                    commit_seq,
                }
            }
            (None, None, None, None, Some(bus), None) => EventBody::Bus(bus),
            (None, None, None, None, None, Some(resync)) => EventBody::Resync {
                reason: resync.reason,
            },
            _ => return Err("event must be exactly one of a store change, bus or resync"),
        };
        Ok(Event {
            subscription: w.subscription,
            instance: w.instance,
            seq: w.seq,
            body,
        })
    }
}

impl From<Event> for EventWire {
    fn from(e: Event) -> Self {
        let mut w = EventWire {
            subscription: e.subscription,
            instance: e.instance,
            seq: e.seq,
            store: None,
            id: None,
            kind: None,
            commit_seq: None,
            bus: None,
            resync: None,
        };
        match e.body {
            EventBody::Change {
                store,
                id,
                kind,
                commit_seq,
            } => {
                w.store = Some(store);
                w.id = Some(id);
                w.kind = Some(kind);
                w.commit_seq = Some(commit_seq);
            }
            EventBody::Bus(bus) => w.bus = Some(bus),
            EventBody::Resync { reason } => w.resync = Some(ResyncWire { reason }),
        }
        w
    }
}

/// Any daemon-to-client frame after the handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerFrame {
    Ok(OkFrame),
    Err(ErrFrame),
    Part(PartFrame),
    Event(EventFrame),
}

impl ServerFrame {
    /// Decode one server frame from a line produced by
    /// [`crate::ndjson::FrameReader`]. Dispatches on the discriminating key
    /// instead of serde's untagged trial-and-error, so the error names the
    /// real problem and `RawValue` payloads are not buffered.
    pub fn decode(line: &[u8]) -> Result<Self, serde_json::Error> {
        // `present` rather than `Option<IgnoredAny>`: a void result is
        // `"ok":null`, which an `Option` would read as absent.
        fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
            serde::de::IgnoredAny::deserialize(d).map(|_| true)
        }
        #[derive(Deserialize)]
        struct Keys {
            #[serde(default, deserialize_with = "present")]
            ok: bool,
            #[serde(default, deserialize_with = "present")]
            err: bool,
            #[serde(default, deserialize_with = "present")]
            part: bool,
        }
        let keys: Keys = serde_json::from_slice(line)?;
        Ok(if keys.ok {
            ServerFrame::Ok(serde_json::from_slice(line)?)
        } else if keys.err {
            ServerFrame::Err(serde_json::from_slice(line)?)
        } else if keys.part {
            ServerFrame::Part(serde_json::from_slice(line)?)
        } else {
            ServerFrame::Event(serde_json::from_slice(line)?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_frames_decode_by_shape_and_reject_unknown_fields() {
        let ok = br#"{"id":1,"ok":null,"commit_seq":4}"#;
        assert!(
            matches!(ServerFrame::decode(ok).unwrap(), ServerFrame::Ok(f) if f.commit_seq == 4)
        );
        let err = br#"{"id":2,"err":{"code":"invalid_arguments","message":"m","exit_code":2,"execution":"not_executed"}}"#;
        assert!(matches!(
            ServerFrame::decode(err).unwrap(),
            ServerFrame::Err(_)
        ));
        let ev = br#"{"ev":{"subscription":3,"instance":"i","seq":1,"store":"tasks","id":"tsk_1","kind":"updated","commit_seq":42}}"#;
        assert!(matches!(
            ServerFrame::decode(ev).unwrap(),
            ServerFrame::Event(_)
        ));
        let resync =
            br#"{"ev":{"subscription":3,"instance":"i","seq":4,"resync":{"reason":"lagged"}}}"#;
        assert!(ServerFrame::decode(resync).is_ok());

        assert!(ServerFrame::decode(br#"{"id":1,"ok":null,"commit_seq":4,"x":1}"#).is_err());
        assert!(ServerFrame::decode(br#"{"id":0,"ok":null,"commit_seq":4}"#).is_err());
        // A change event missing its commit ordinal is not a valid shape.
        assert!(
            ServerFrame::decode(br#"{"ev":{"subscription":3,"instance":"i","seq":1,"store":"tasks","id":"t","kind":"updated"}}"#)
                .is_err()
        );
    }

    #[test]
    fn event_round_trips_through_its_wire_shape() {
        let event = Event {
            subscription: 3,
            instance: Id::new("i").unwrap(),
            seq: 1,
            body: EventBody::Change {
                store: Store::Tasks,
                id: Id::new("tsk_1").unwrap(),
                kind: ChangeKind::Created,
                commit_seq: 9,
            },
        };
        let json = serde_json::to_string(&EventFrame { ev: event.clone() }).unwrap();
        let back: EventFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ev, event);
    }
}
