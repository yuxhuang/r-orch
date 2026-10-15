//! Bounded NDJSON. A bad frame is a connection error, never a skipped line.
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

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(frame).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "peer frame exceeds 1 MiB",
        ));
    }
    bytes.push(b'\n');
    // A stalled reader must not stop heartbeat and registry removal forever.
    tokio::time::timeout(std::time::Duration::from_secs(5), writer.write_all(&bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "peer write timed out"))??;
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
        assert!(
            write_frame(
                &mut Vec::new(),
                &Frame::Error {
                    msg: "x".repeat(MAX_FRAME_BYTES)
                }
            )
            .await
            .is_err()
        );
    }
}
