//! Bounded NDJSON. Malformed incoming frames close the socket; local oversize
//! data is bounded or rejected before it can interrupt the transport.
use crate::protocol::Frame;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Call from a dedicated reader task: dropping a partially read frame is a
/// connection close. The allocation never grows beyond the frame limit.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Frame>> {
    let mut line = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated peer frame",
                ))
            };
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let n = end.map_or(bytes.len(), |i| i + 1);
        if line.len() + n > MAX_FRAME_BYTES + 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "peer frame exceeds 1 MiB",
            ));
        }
        line.extend_from_slice(&bytes[..n]);
        reader.consume(n);
        if end.is_some() {
            line.pop();
            return serde_json::from_slice(&line)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
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
        let mut bytes = serde_json::to_vec(&frame).map_err(io::Error::other)?;
        if bytes.len() > MAX_FRAME_BYTES {
            tracing::warn!("Skip locally produced outbound frame with oversized envelope");
            continue;
        }
        bytes.push(b'\n');
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
