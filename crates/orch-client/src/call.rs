//! One request's frames after it is written (design D6): `part`s in order,
//! a streamed large `ok`, and an explicit `request.cancel`.
//!
//! The lane stays poisoned from the write until this request's terminal frame
//! decodes. Dropping a [`Call`] early therefore closes the lane for later
//! calls rather than letting them read this request's reply.
use crate::{Client, ClientError};
use base64::Engine as _;
use orch_proto::error::NoDaemonDetail;
use orch_proto::frame::{
    MAX_FRAME_BYTES, OkFrame, Part, PartKind, RequestFrame, RequestId, ServerFrame, StreamEncoding,
    StreamedResult,
};
use orch_proto::handshake;
use orch_proto::ndjson;
use orch_proto::ops::{Request, args};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::marker::PhantomData;

/// Largest streamed `ok` reassembled; equal to the daemon's send bound.
pub const MAX_STREAMED_RESULT_BYTES: usize = 96 * 1024 * 1024;

/// One step of an in-flight request.
#[derive(Debug)]
pub enum Progress<T> {
    /// An orchestrator event streamed by `logs.follow` or a dispatch request.
    Event(Box<orch_domain::OrchestratorEvent>),
    /// The terminal result. The lane is free again.
    Done(T),
}

/// An in-flight request on the client's single lane. Read it to completion
/// with [`Call::next`] or [`Call::finish`], or end it with [`Call::cancel`].
pub struct Call<'c, T> {
    client: &'c mut Client,
    id: RequestId,
    next_part: u64,
    chunks: Option<Vec<u8>>,
    done: bool,
    out: PhantomData<fn() -> T>,
}

impl Client {
    /// Reserve an id and poison the lane before any await: a cancelled future
    /// must never let a later call consume this request's unknown reply.
    fn reserve(&mut self) -> Result<RequestId, ClientError> {
        if self.request_may_have_been_sent {
            return Err(self.unknown());
        }
        let id = self.next_request_id()?;
        self.request_may_have_been_sent = true;
        Ok(id)
    }

    fn next_request_id(&mut self) -> Result<RequestId, ClientError> {
        let id = std::num::NonZeroU64::new(self.next_id).ok_or_else(|| self.unknown())?;
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        Ok(RequestId(id))
    }

    pub(crate) fn unknown(&self) -> ClientError {
        self.project
            .failure(NoDaemonDetail::RequestOutcomeUnknown)
            .into()
    }

    async fn write(&mut self, id: RequestId, req: Request) -> Result<(), ClientError> {
        ndjson::write_frame(&mut self.writer, &RequestFrame { id, req }, MAX_FRAME_BYTES)
            .await
            .map_err(|_| self.unknown())
    }

    /// The next frame addressed to a request. Subscription pushes carry no
    /// request id and have no consumer before PR 3, so they are skipped.
    async fn read_frame(&mut self) -> Result<ServerFrame, ClientError> {
        loop {
            let bytes = self
                .io
                .read_line(MAX_FRAME_BYTES)
                .await
                .map_err(|_| self.unknown())?
                .ok_or_else(|| self.unknown())?;
            ndjson::check_depth(&bytes, orch_proto::frame::MAX_DEPTH)
                .map_err(|_| self.unknown())?;
            match ServerFrame::decode(&bytes).map_err(|_| self.unknown())? {
                ServerFrame::Event(_) => continue,
                frame => return Ok(frame),
            }
        }
    }

    /// Write `req` and return its in-flight handle.
    pub(crate) async fn begin<T: DeserializeOwned>(
        &mut self,
        req: Request,
    ) -> Result<Call<'_, T>, ClientError> {
        let id = self.reserve()?;
        self.write(id, req).await?;
        Ok(Call {
            client: self,
            id,
            next_part: 0,
            chunks: None,
            done: false,
            out: PhantomData,
        })
    }
}

impl<T: DeserializeOwned> Call<'_, T> {
    /// The next event, or the terminal result. A malformed, out-of-order or
    /// misaddressed frame is an unknown outcome and leaves the lane closed.
    pub async fn next(&mut self) -> Result<Progress<T>, ClientError> {
        if self.done {
            return Err(ClientError::InvalidArguments(
                "request already finished".into(),
            ));
        }
        loop {
            match self.client.read_frame().await? {
                ServerFrame::Part(frame) if frame.id == self.id => {
                    if let Some(event) = self.part(frame.part)? {
                        return Ok(Progress::Event(event));
                    }
                }
                ServerFrame::Ok(frame) if frame.id == self.id => {
                    self.done = true;
                    let value = self.decode_ok(frame)?;
                    self.client.request_may_have_been_sent = false;
                    return Ok(Progress::Done(value));
                }
                ServerFrame::Err(frame) if frame.id == self.id => {
                    self.done = true;
                    self.client.request_may_have_been_sent = false;
                    return Err(ClientError::Remote(frame.err));
                }
                _ => return Err(self.client.unknown()),
            }
        }
    }

    /// Read to the terminal result, discarding streamed events.
    pub async fn finish(mut self) -> Result<T, ClientError> {
        loop {
            if let Progress::Done(value) = self.next().await? {
                return Ok(value);
            }
        }
    }

    /// Ask the daemon to end this request (`request.cancel`) and wait, within
    /// the bounded completion deadline, for both terminal frames. Events that
    /// arrive meanwhile are discarded. Cancelling a dispatch observer ends the
    /// observation only; it never kills agents (design D8).
    pub async fn cancel(mut self) -> Result<T, ClientError> {
        if self.done {
            return Err(ClientError::InvalidArguments(
                "request already finished".into(),
            ));
        }
        let cancel_id = self.client.next_request_id()?;
        let req = Request::RequestCancel(args::RequestCancelArgs { id: self.id });
        self.client.write(cancel_id, req).await?;
        let unknown = self.client.unknown();
        tokio::time::timeout(handshake::STOP_COMPLETION_TIMEOUT, async {
            let mut result = None;
            let mut acknowledged = false;
            while result.is_none() || !acknowledged {
                match self.client.read_frame().await? {
                    ServerFrame::Part(frame) if frame.id == self.id => {
                        self.part(frame.part)?;
                    }
                    ServerFrame::Ok(frame) if frame.id == self.id && result.is_none() => {
                        result = Some(Ok(self.decode_ok(frame)?));
                    }
                    ServerFrame::Err(frame) if frame.id == self.id && result.is_none() => {
                        result = Some(Err(ClientError::Remote(frame.err)));
                    }
                    ServerFrame::Ok(frame) if frame.id == cancel_id && !acknowledged => {
                        acknowledged = true;
                    }
                    // A refused cancel leaves the original running: the lane
                    // stays poisoned and the refusal is the answer.
                    ServerFrame::Err(frame) if frame.id == cancel_id => {
                        return Err(ClientError::Remote(frame.err));
                    }
                    _ => return Err(self.client.unknown()),
                }
            }
            self.done = true;
            self.client.request_may_have_been_sent = false;
            result.unwrap_or_else(|| Err(self.client.unknown()))
        })
        .await
        .map_err(|_| unknown)?
    }

    fn part(
        &mut self,
        part: Part,
    ) -> Result<Option<Box<orch_domain::OrchestratorEvent>>, ClientError> {
        // Increasing per request (D6); the digest catches a lost result chunk.
        if part.n < self.next_part {
            return Err(self.client.unknown());
        }
        self.next_part = part.n.saturating_add(1);
        match part.kind {
            PartKind::Event => ndjson::decode(part.data.get().as_bytes())
                .map(Some)
                .map_err(|_| self.client.unknown()),
            // No operation of this protocol revision emits log parts.
            PartKind::Log => Ok(None),
            PartKind::ResultChunk => {
                let encoded: String = ndjson::decode(part.data.get().as_bytes())
                    .map_err(|_| self.client.unknown())?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| self.client.unknown())?;
                let chunks = self.chunks.get_or_insert_with(Vec::new);
                if chunks.len().saturating_add(bytes.len()) > MAX_STREAMED_RESULT_BYTES {
                    return Err(self.client.unknown());
                }
                chunks.extend_from_slice(&bytes);
                Ok(None)
            }
        }
    }

    /// An inline result, or the reassembled bytes checked against the
    /// streamed summary. An incomplete stream is never success.
    fn decode_ok(&mut self, frame: OkFrame) -> Result<T, ClientError> {
        let value = match self.chunks.take() {
            None => ndjson::decode(frame.ok.get().as_bytes()),
            Some(bytes) => {
                let summary: StreamedResult =
                    ndjson::decode(frame.ok.get().as_bytes()).map_err(|_| self.client.unknown())?;
                if !summary.streamed
                    || summary.encoding != StreamEncoding::JsonUtf8
                    || summary.bytes != bytes.len() as u64
                    || summary.sha256.as_str() != hex_sha256(&bytes)
                {
                    return Err(self.client.unknown());
                }
                ndjson::decode(&bytes)
            }
        }
        .map_err(|_| self.client.unknown())?;
        self.client.last_commit_seq = Some(frame.commit_seq);
        Ok(value)
    }
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
