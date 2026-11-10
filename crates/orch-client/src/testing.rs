//! Private in-memory peer for frontend tests, available under `cfg(test)` or
//! `cfg(debug_assertions)` (including downstream debug test builds). No sockets,
//! process creation, environment changes or daemon startup are involved.
//!
//! Exact signatures (all methods belong to `FakeServer`):
//!
//! ```text
//! fn fake_transport() -> (DuplexStream, FakeServer)
//! async fn accept(&mut self, instance: Id) -> Result<HelloFrame, CodecError>
//! async fn recv(&mut self) -> Result<Option<RequestFrame>, CodecError>
//! async fn send<T: Serialize>(&mut self, frame: &T) -> Result<(), CodecError>
//! async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), CodecError>
//! ```
//!
//! Ack, gap, changed instance and resync are scripted with ordinary proto DTOs.
//! `send_raw` can split a frame; dropping the server scripts EOF.
//!
//! ```no_run
//! # #[cfg(debug_assertions)]
//! # async fn example(project: orch_client::Project) -> Result<(), Box<dyn std::error::Error>> {
//! use orch_client::{Client, SubscriptionEvent, testing::fake_transport};
//! use orch_proto::{bounded::Id, frame::{OkFrame, EventFrame, Event, EventBody, ResyncReason},
//!     handshake::{Caller, CallerKind, SessionMode}, ops::{args::SubscribeArgs, results::SubscribeAck}};
//! let (stream, mut peer) = fake_transport();
//! let server = tokio::spawn(async move {
//!     peer.accept(Id::new("fake-instance").unwrap()).await.unwrap();
//!     let request = peer.recv().await.unwrap().unwrap();
//!     peer.send(&OkFrame { id: request.id, commit_seq: 9000,
//!         ok: serde_json::value::to_raw_value(&SubscribeAck {
//!             subscription: 1, instance: "fake-instance".into(), seq: 0 }).unwrap()
//!     }).await.unwrap();
//!     peer.send(&EventFrame { ev: Event { subscription: 1,
//!         instance: Id::new("fake-instance").unwrap(), seq: 1,
//!         body: EventBody::Resync { reason: ResyncReason::Lagged }
//!     }}).await.unwrap();
//! });
//! let client = Client::from_stream(project, stream,
//!     Caller { kind: CallerKind::Tui, agent_id: None, pid: None }, SessionMode::Normal).await?;
//! let mut observation = client.into_subscription(SubscribeArgs {
//!     stores: orch_proto::bounded::BoundedVec::new(vec![orch_proto::frame::Store::Tasks])?, filter: None, bus: true
//! }).await?;
//! assert_eq!(observation.baseline_commit_seq(), 9000);
//! assert!(matches!(observation.next().await?, SubscriptionEvent::ResyncRequired { .. }));
//! server.await?;
//! # Ok(()) }
//! ```
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::frame::{MAX_FRAME_BYTES, RequestFrame};
use orch_proto::handshake::{HandshakeReply, HelloFrame, MAX_HANDSHAKE_BYTES, Welcome};
use orch_proto::ndjson::{self, CodecError, FrameReader};
use serde::Serialize;
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

/// Bounded full-duplex transport and its scriptable peer (64 KiB per direction).
pub fn fake_transport() -> (DuplexStream, FakeServer) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(server);
    (
        client,
        FakeServer {
            read: FrameReader::new(BufReader::new(read)),
            write,
        },
    )
}

pub struct FakeServer {
    read: FrameReader<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
}

impl FakeServer {
    /// Read the real bounded hello and reply with matching versions and project.
    pub async fn accept(&mut self, instance: Id) -> Result<HelloFrame, CodecError> {
        let hello: HelloFrame = self
            .read
            .read(MAX_HANDSHAKE_BYTES)
            .await?
            .ok_or(CodecError::Unterminated)?;
        let welcome = Welcome {
            protocol: orch_proto::LOCAL_PROTOCOL,
            binary: BoundedString::new(orch_proto::BINARY_VERSION)
                .map_err(|_| CodecError::Unterminated)?,
            project_id: hello.hello.project_id,
            project_root: hello.hello.project_root.clone(),
            instance,
            pid: 1,
            compatible: true,
            stop_state: None,
        };
        ndjson::write_frame(
            &mut self.write,
            &HandshakeReply::welcome(welcome),
            MAX_HANDSHAKE_BYTES,
        )
        .await?;
        Ok(hello)
    }
    pub async fn recv(&mut self) -> Result<Option<RequestFrame>, CodecError> {
        self.read.read(MAX_FRAME_BYTES).await
    }
    pub async fn send<T: Serialize>(&mut self, frame: &T) -> Result<(), CodecError> {
        ndjson::write_frame(&mut self.write, frame, MAX_FRAME_BYTES).await
    }
    /// Deliberately bypass encode validation for malformed/partial/oversize tests.
    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), CodecError> {
        self.write.write_all(bytes).await?;
        self.write.flush().await?;
        Ok(())
    }
}
