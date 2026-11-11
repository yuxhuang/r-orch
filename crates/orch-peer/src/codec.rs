//! Bounded NDJSON. Malformed incoming frames close the socket; local oversize
//! data is bounded or rejected before it can interrupt the transport.
//!
//! The line framing itself (bounded `fill_buf` scan, one LF, the 1 MiB cap)
//! is `orch_proto::ndjson`, shared with the local daemon protocol (design
//! D10). Everything peer-specific stays here: the `Frame` schema, event and
//! outcome truncation, `SyncState` splitting, and the rule that an oversize
//! local frame is skipped rather than failing the session.
use crate::protocol::Frame;
use orch_proto::ndjson::{self, CodecError, FrameReader};
use std::io;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = orch_proto::frame::MAX_FRAME_BYTES;

/// Call from a dedicated reader task: dropping a partially read frame is a
/// connection close. The allocation never grows beyond the frame limit.
///
/// Decoding is plain `serde_json` on purpose: the local protocol's depth
/// check is not applied to peers, whose frames 0.5.x builds already send.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Frame>> {
    let line = match FrameReader::new(reader).read_line(MAX_FRAME_BYTES).await {
        Ok(Some(line)) => line,
        Ok(None) => return Ok(None),
        Err(CodecError::Io(error)) => return Err(error),
        Err(CodecError::Unterminated) => {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unterminated peer frame",
            ));
        }
        Err(CodecError::Oversize { .. }) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "peer frame exceeds 1 MiB",
            ));
        }
        // An empty line is not JSON, exactly as before the lift.
        Err(_) => Vec::new(),
    };
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Check the encoded size, including JSON escaping, before enqueueing data.
pub(crate) fn fits(frame: &Frame) -> bool {
    serde_json::to_vec(frame).is_ok_and(|bytes| bytes.len() <= MAX_FRAME_BYTES)
}

/// Event progress is best effort. Keep the marker in the ring as well as on
/// the live stream so an attachment cannot replay the original oversize data.
pub(crate) fn bounded_event(
    exec_key: &str,
    mut event: orch_core::adapter::AgentEvent,
) -> orch_core::adapter::AgentEvent {
    if !fits(&Frame::Event {
        exec_key: exec_key.into(),
        seq: u64::MAX,
        event: event.clone(),
    }) {
        event.data =
            serde_json::json!({"truncated": true, "message": "event too large for remote frame"});
    }
    event
}

fn bounded_outcome(outcome: &orch_core::remote::RemoteOutcome) -> orch_core::remote::RemoteOutcome {
    // Preserve the outcome type (especially review/integration failures) while
    // bounding diagnostic text, file lists and review output independently.
    let mut value = serde_json::to_value(outcome).expect("serializable outcome");
    fn bound(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) if text.len() > 512 => {
                let mut end = 512;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str(" [truncated for remote frame]");
            }
            serde_json::Value::Array(items) => {
                items.truncate(100);
                for item in items {
                    bound(item);
                }
            }
            serde_json::Value::Object(fields) => {
                for value in fields.values_mut() {
                    bound(value);
                }
            }
            _ => {}
        }
    }
    bound(&mut value);
    for field in ["summary", "error", "reason"] {
        if let Some(serde_json::Value::String(text)) = value.get_mut(field) {
            text.push_str(" [remote outcome truncated]");
            break;
        }
    }
    serde_json::from_value(value).expect("bounded outcome retains its schema")
}

fn outbound_frames(frame: &Frame) -> Vec<Frame> {
    if fits(frame) {
        return vec![frame.clone()];
    }
    match frame {
        Frame::Event {
            exec_key,
            seq,
            event,
        } => vec![Frame::Event {
            exec_key: exec_key.clone(),
            seq: *seq,
            event: bounded_event(exec_key, event.clone()),
        }],
        Frame::SyncState { items } => {
            // Each item is independent in protocol v1; no end-of-batch marker
            // is required. Old and new controllers accept repeated sync_state.
            items
                .iter()
                .map(|item| {
                    let mut item = item.clone();
                    let single = Frame::SyncState {
                        items: vec![item.clone()],
                    };
                    if !fits(&single) {
                        item.outcome = item.outcome.as_ref().map(bounded_outcome);
                    }
                    Frame::SyncState { items: vec![item] }
                })
                .collect()
        }
        Frame::Finished { exec_key, outcome } => vec![Frame::Finished {
            exec_key: exec_key.clone(),
            outcome: bounded_outcome(outcome),
        }],
        // Submit and Msg must be rejected by their producer before writing.
        // A locally produced invalid frame is never a socket failure.
        _ => {
            tracing::warn!("Skip locally produced outbound frame exceeding 1 MiB");
            vec![]
        }
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    for frame in outbound_frames(frame) {
        let bytes = match ndjson::encode(&frame, MAX_FRAME_BYTES) {
            Ok(bytes) => bytes,
            Err(CodecError::Oversize { .. }) => {
                tracing::warn!("Skip locally produced outbound frame with oversized envelope");
                continue;
            }
            Err(CodecError::Io(error)) => return Err(error),
            Err(error) => return Err(io::Error::other(error)),
        };
        // Only transport errors terminate a session, never local frame size.
        tokio::time::timeout(std::time::Duration::from_secs(5), writer.write_all(&bytes))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "peer write timed out"))??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{BufReader, duplex};

    #[tokio::test]
    async fn round_trip_multiple_frames() {
        let (mut a, b) = duplex(128);
        write_frame(&mut a, &Frame::Ping).await.unwrap();
        write_frame(&mut a, &Frame::Pong).await.unwrap();
        drop(a);
        let mut b = BufReader::new(b);
        assert!(matches!(
            read_frame(&mut b).await.unwrap(),
            Some(Frame::Ping)
        ));
        assert!(matches!(
            read_frame(&mut b).await.unwrap(),
            Some(Frame::Pong)
        ));
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    /// D10: the lift changes no byte on the wire in either direction. These
    /// are the exact lines a 0.5.x peer writes and expects.
    #[tokio::test]
    async fn wire_bytes_are_unchanged_by_the_framing_lift() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &Frame::Ping).await.unwrap();
        write_frame(
            &mut bytes,
            &Frame::Error {
                msg: "bad \"frame\"".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            bytes,
            b"{\"t\":\"ping\"}\n{\"t\":\"error\",\"msg\":\"bad \\\"frame\\\"\"}\n"
        );

        // A 0.5.x hello omits every later additive field.
        let old_hello = b"{\"t\":\"hello\",\"proto\":1,\"instance_id\":\"i\",\"name\":\"n\",\"os\":\"linux\",\"agents\":[]}\n";
        assert!(matches!(
            read_frame(&mut BufReader::new(&old_hello[..]))
                .await
                .unwrap(),
            Some(Frame::Hello { .. })
        ));

        // Exactly 1 MiB of JSON plus its LF is accepted; one byte more is not.
        let base = serde_json::to_vec(&Frame::Error { msg: String::new() })
            .unwrap()
            .len();
        let mut line = serde_json::to_vec(&Frame::Error {
            msg: "x".repeat(MAX_FRAME_BYTES - base + 1),
        })
        .unwrap();
        line.push(b'\n');
        let error = read_frame(&mut BufReader::new(line.as_slice()))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "peer frame exceeds 1 MiB");
        let error = read_frame(&mut BufReader::new(&b"{\"t\":\"ping\"}"[..]))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        // Peers keep serde_json's own nesting limit, not the local protocol's.
        let deep = format!(
            "{{\"t\":\"error\",\"msg\":\"m\",\"x\":{}{}}}\n",
            "[".repeat(80),
            "]".repeat(80)
        );
        assert!(matches!(
            read_frame(&mut BufReader::new(deep.as_bytes()))
                .await
                .unwrap(),
            Some(Frame::Error { .. })
        ));
    }

    #[tokio::test]
    async fn malformed_and_unterminated_lines_are_errors() {
        for bytes in [
            &b"not json\n"[..],
            &b"{}\n"[..],
            &b"{\"t\":\"ping\"}"[..],
            &b"\n"[..],
        ] {
            assert!(read_frame(&mut BufReader::new(bytes)).await.is_err());
        }
    }

    #[tokio::test]
    async fn oversize_input_is_rejected_without_reading_the_whole_line() {
        let bytes = vec![b'x'; MAX_FRAME_BYTES + 20];
        assert!(
            read_frame(&mut BufReader::with_capacity(1024, bytes.as_slice()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn outbound_size_cap_and_exact_boundary() {
        let base = serde_json::to_vec(&Frame::Error { msg: String::new() })
            .unwrap()
            .len();
        let frame = Frame::Error {
            msg: "x".repeat(MAX_FRAME_BYTES - base),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).await.unwrap();
        assert_eq!(bytes.len(), MAX_FRAME_BYTES + 1);
        assert!(
            read_frame(&mut BufReader::new(bytes.as_slice()))
                .await
                .unwrap()
                .is_some()
        );
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &Frame::Error {
                msg: "x".repeat(MAX_FRAME_BYTES),
            },
        )
        .await
        .unwrap();
        write_frame(&mut bytes, &Frame::Ping).await.unwrap();
        assert!(matches!(
            read_frame(&mut BufReader::new(bytes.as_slice()))
                .await
                .unwrap(),
            Some(Frame::Ping)
        ));
    }
    #[tokio::test]
    async fn oversized_event_is_a_marker_and_next_frame_is_delivered() {
        let frame = Frame::Event {
            exec_key: "key".into(),
            seq: 1,
            event: orch_core::adapter::AgentEvent::new(
                orch_core::adapter::AgentEventKind::Output,
                "now",
                serde_json::json!("x".repeat(MAX_FRAME_BYTES)),
            ),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).await.unwrap();
        write_frame(&mut bytes, &Frame::Ping).await.unwrap();
        let mut read = BufReader::new(bytes.as_slice());
        assert!(
            matches!(read_frame(&mut read).await.unwrap(), Some(Frame::Event { event, .. }) if event.data["truncated"] == true)
        );
        assert!(matches!(
            read_frame(&mut read).await.unwrap(),
            Some(Frame::Ping)
        ));
    }

    #[tokio::test]
    async fn oversized_sync_state_chunks_and_bounds_outcomes_then_delivers_next_frame() {
        use crate::protocol::SyncItem;
        use orch_core::remote::RemoteOutcome;
        let outcome = RemoteOutcome::Succeeded {
            summary: "summary".into(),
            files_changed: vec!["file".repeat(1024); 300],
            branch: None,
            review_results: None,
            tokens: None,
        };
        let frame = Frame::SyncState {
            items: (0..3)
                .map(|n| SyncItem {
                    outcome: Some(outcome.clone()),
                    ..SyncItem::new(&format!("key{n}"), "finished")
                })
                .collect(),
        };
        assert!(!fits(&frame));
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).await.unwrap();
        write_frame(&mut bytes, &Frame::Ping).await.unwrap();
        let mut read = BufReader::new(bytes.as_slice());
        for n in 0..3 {
            let Some(Frame::SyncState { items }) = read_frame(&mut read).await.unwrap() else {
                panic!("sync item missing")
            };
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].exec_key, format!("key{n}"));
            assert!(
                matches!(&items[0].outcome, Some(RemoteOutcome::Succeeded { files_changed, .. }) if files_changed.len() == 100)
            );
        }
        assert!(matches!(
            read_frame(&mut read).await.unwrap(),
            Some(Frame::Ping)
        ));
    }
}
