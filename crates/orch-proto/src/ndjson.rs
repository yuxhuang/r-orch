//! Bounded, transport-neutral newline-delimited JSON (design D6, D10).
//!
//! The line algorithm is the one in `orch-peer`'s codec (bounded `fill_buf`
//! scan, the allocation never grows past the limit), made generic over the
//! frame type and limit. Policies stay with their owners: peer-side event
//! truncation and `SyncState` splitting remain in `orch-peer`, which switches
//! to this codec in S1d. Here an oversized outbound frame is an error, never a
//! silent drop.
//!
//! Framing rules: UTF-8 JSON objects, at most `max` bytes excluding the LF,
//! exactly one LF terminator. Empty lines, unterminated input at EOF,
//! oversized lines and nesting deeper than [`MAX_DEPTH`] are errors; decoding
//! errors are reported as a fixed category and byte offset, never serde text.

use crate::frame::{MAX_DEPTH, WRITE_TIMEOUT};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// A framing or decoding failure. Any of these closes the connection, except
/// [`CodecError::Decode`] after a valid frame boundary, which the daemon may
/// answer with an `invalid_arguments` error for a well-formed request id.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("transport error: {0}")]
    Io(#[from] io::Error),
    #[error("frame exceeds {max} bytes")]
    Oversize { max: usize },
    #[error("unterminated frame at end of input")]
    Unterminated,
    #[error("empty frame")]
    Empty,
    #[error("frame nests deeper than {MAX_DEPTH}")]
    TooDeep,
    #[error("frame is not valid {category} at byte {offset}")]
    Decode {
        category: DecodeCategory,
        offset: usize,
    },
    #[error("frame write timed out")]
    WriteTimeout,
}

/// serde_json's category, without its message (which can echo input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeCategory {
    Syntax,
    Schema,
    Eof,
    Io,
}

impl std::fmt::Display for DecodeCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DecodeCategory::Syntax => "JSON",
            DecodeCategory::Schema => "frame schema",
            DecodeCategory::Eof => "complete JSON",
            DecodeCategory::Io => "readable JSON",
        })
    }
}

/// Map a serde_json error to a value-free [`CodecError::Decode`]. The offset
/// is the column on the (single) line, which is the byte offset in a frame.
pub fn decode_error(line: &[u8], error: &serde_json::Error) -> CodecError {
    use serde_json::error::Category;
    let category = match error.classify() {
        Category::Syntax => DecodeCategory::Syntax,
        Category::Data => DecodeCategory::Schema,
        Category::Eof => DecodeCategory::Eof,
        Category::Io => DecodeCategory::Io,
    };
    CodecError::Decode {
        category,
        offset: error.column().min(line.len()),
    }
}

/// Reject JSON nested deeper than `max_depth` without parsing it. Brackets
/// inside strings (including escaped quotes) are ignored.
pub fn check_depth(line: &[u8], max_depth: usize) -> Result<(), CodecError> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &b in line {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max_depth {
                    return Err(CodecError::TooDeep);
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Owns the partial-line state of one inbound stream. Call from one dedicated
/// reader task: dropping a partially read frame means closing the connection.
pub struct FrameReader<R> {
    inner: R,
}

impl<R: AsyncBufRead + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        FrameReader { inner }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// The next line without its LF, at most `max` bytes. `Ok(None)` on a
    /// clean EOF between frames.
    pub async fn read_line(&mut self, max: usize) -> Result<Option<Vec<u8>>, CodecError> {
        let mut line = Vec::new();
        loop {
            let bytes = self.inner.fill_buf().await?;
            if bytes.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Err(CodecError::Unterminated)
                };
            }
            let end = bytes.iter().position(|b| *b == b'\n');
            let n = end.map_or(bytes.len(), |i| i + 1);
            if line.len() + n > max + 1 {
                return Err(CodecError::Oversize { max });
            }
            line.extend_from_slice(&bytes[..n]);
            self.inner.consume(n);
            if end.is_some() {
                line.pop();
                if line.is_empty() {
                    return Err(CodecError::Empty);
                }
                return Ok(Some(line));
            }
        }
    }

    /// Read, depth-check and decode one frame of type `T`.
    pub async fn read<T: DeserializeOwned>(&mut self, max: usize) -> Result<Option<T>, CodecError> {
        let Some(line) = self.read_line(max).await? else {
            return Ok(None);
        };
        decode(&line).map(Some)
    }
}

/// Depth-check and decode one complete line.
pub fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, CodecError> {
    check_depth(line, MAX_DEPTH)?;
    serde_json::from_slice(line).map_err(|e| decode_error(line, &e))
}

/// Encode `frame` as one line including its LF. Fails rather than truncating
/// when the encoding exceeds `max`.
pub fn encode<T: Serialize>(frame: &T, max: usize) -> Result<Vec<u8>, CodecError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|e| CodecError::Io(io::Error::other(e)))?;
    if bytes.len() > max {
        return Err(CodecError::Oversize { max });
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Write one frame within [`WRITE_TIMEOUT`].
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    frame: &T,
    max: usize,
) -> Result<(), CodecError> {
    let bytes = encode(frame, max)?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| CodecError::WriteTimeout)??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn lines_are_bounded_terminated_and_nonempty() {
        let mut r = FrameReader::new(BufReader::new(&b"{\"a\":1}\n{}\n"[..]));
        assert_eq!(r.read_line(16).await.unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(r.read_line(16).await.unwrap().unwrap(), b"{}");
        assert!(r.read_line(16).await.unwrap().is_none());

        let exact = [b'x'; 8];
        let mut input = exact.to_vec();
        input.push(b'\n');
        assert!(
            FrameReader::new(&input[..])
                .read_line(8)
                .await
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            FrameReader::new(&input[..]).read_line(7).await,
            Err(CodecError::Oversize { max: 7 })
        ));
        assert!(matches!(
            FrameReader::new(&b"{}"[..]).read_line(8).await,
            Err(CodecError::Unterminated)
        ));
        assert!(matches!(
            FrameReader::new(&b"\n"[..]).read_line(8).await,
            Err(CodecError::Empty)
        ));
    }

    #[test]
    fn depth_is_checked_before_parsing() {
        let ok = format!("{}{}", "[".repeat(64), "]".repeat(64));
        assert!(check_depth(ok.as_bytes(), 64).is_ok());
        let deep = format!("{}{}", "[".repeat(65), "]".repeat(65));
        assert!(matches!(
            check_depth(deep.as_bytes(), 64),
            Err(CodecError::TooDeep)
        ));
        // Brackets in strings, including after an escaped quote, do not count.
        let quoted = format!("[\"\\\"{}\"]", "[".repeat(100));
        assert!(check_depth(quoted.as_bytes(), 64).is_ok());
    }

    #[test]
    fn decode_errors_carry_no_input_text() {
        let line = br#"{"secret-canary": }"#;
        let err = decode::<serde_json::Value>(line).unwrap_err();
        assert!(!err.to_string().contains("secret-canary"), "{err}");
    }

    #[test]
    fn oversized_outbound_frames_fail_instead_of_vanishing() {
        assert!(matches!(
            encode(&"x".repeat(10), 8),
            Err(CodecError::Oversize { .. })
        ));
    }
}
