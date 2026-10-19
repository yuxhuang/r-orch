//! The stdout line splitters: the shared reader and pi's private reader.
//!
//! These are byte-level newline splitters, **not** text readers, and
//! deliberately not `AsyncBufReadExt::lines()`:
//!
//! 1. the delimiter is `0x0A` only — a trailing `\r` is **kept**;
//! 2. zero-length lines are **dropped**;
//! 3. a trailing fragment with no final newline **is** yielded;
//! 4. each line is capped at [`MAX_LINE_LEN`] UTF-16 code units.
//!
//! Pi opts out of (1) and (4).

use std::io;

use futures::Stream;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Max stdout line length before truncation (16 KB).
///
/// First layer of a three-layer cap: `read_lines` (16 KB) →
/// `serialize_event_data` (8 KB) → bus emit (4 KB). A truncated line produces
/// invalid JSON, which every adapter's parse fallback turns into a raw
/// `output` event.
pub const MAX_LINE_LEN: usize = 16384;

/// Read size per `read` call. Invisible to the caller: the splitter buffers
/// across chunk boundaries, so no line depends on how the kernel chunks the
/// pipe.
const READ_CHUNK: usize = 16384;

/// Truncate to [`MAX_LINE_LEN`] **UTF-16 code units**, because the cap counts
/// code units rather than bytes or chars.
///
/// One unavoidable divergence: JS can slice *inside* a surrogate pair and keep
/// the lone high surrogate, which a Rust `String` cannot hold, so a character
/// straddling the boundary is dropped whole. It takes a >16 KiB line with a
/// non-BMP character at exactly the cut point to observe, and the line is
/// invalid JSON either way.
fn cap_line(line: String) -> String {
    if line.len() <= MAX_LINE_LEN {
        // A byte length within the cap bounds the UTF-16 length too: UTF-8
        // never uses fewer bytes than the UTF-16 units it encodes.
        return line;
    }

    let mut units = 0usize;
    for (offset, ch) in line.char_indices() {
        let width = ch.len_utf16();
        if units + width > MAX_LINE_LEN {
            let mut capped = line;
            capped.truncate(offset);
            return capped;
        }
        units += width;
    }
    line
}

/// The shared reader used by every adapter except pi.
pub fn read_lines<R>(reader: R) -> impl Stream<Item = io::Result<String>> + Send
where
    R: AsyncRead + Unpin + Send + 'static,
{
    read_framed(reader, true, false)
}

/// Pi's reader: no 16 KB cap (its `agent_end` frame carries the whole
/// transcript and truncating it would corrupt the terminal event) and a
/// trailing `\r` is stripped.
pub fn read_pi_lines<R>(reader: R) -> impl Stream<Item = io::Result<String>> + Send
where
    R: AsyncRead + Unpin + Send + 'static,
{
    read_framed(reader, false, true)
}

fn read_framed<R>(
    mut reader: R,
    cap: bool,
    strip_cr: bool,
) -> impl Stream<Item = io::Result<String>> + Send
where
    R: AsyncRead + Unpin + Send + 'static,
{
    async_stream::try_stream! {
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; READ_CHUNK];

        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            pending.extend_from_slice(&chunk[..read]);

            let mut offset = 0;
            while let Some(found) = pending[offset..].iter().position(|b| *b == b'\n') {
                let newline = offset + found;
                // `newline > offset` drops empty lines.
                if newline > offset {
                    yield decode(&pending[offset..newline], cap, strip_cr);
                }
                offset = newline + 1;
            }
            pending.drain(..offset);
        }

        // Flush the last line when the stream ended without a newline.
        if !pending.is_empty() {
            let line = decode(&pending, cap, strip_cr);
            // pi guards the flush with `if (line)`, which only matters for a
            // remainder that decodes to nothing; the shared reader does not.
            if !strip_cr || !line.is_empty() {
                yield line;
            }
        }
    }
}

fn decode(bytes: &[u8], cap: bool, strip_cr: bool) -> String {
    // `Buffer.toString('utf-8')` substitutes U+FFFD for invalid sequences,
    // exactly as `from_utf8_lossy` does.
    let mut line = String::from_utf8_lossy(bytes).into_owned();
    if strip_cr && line.ends_with('\r') {
        line.pop();
    }
    if cap {
        line = cap_line(line);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    async fn lines(input: &'static [u8]) -> Vec<String> {
        read_lines(std::io::Cursor::new(input))
            .map(|line| line.expect("cursor never fails"))
            .collect()
            .await
    }

    async fn pi_lines(input: &'static [u8]) -> Vec<String> {
        read_pi_lines(std::io::Cursor::new(input))
            .map(|line| line.expect("cursor never fails"))
            .collect()
            .await
    }

    #[tokio::test]
    async fn splits_on_newlines() {
        assert_eq!(lines(b"a\nb\nc\n").await, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn drops_empty_lines() {
        assert_eq!(lines(b"\n\na\n\n\nb\n").await, vec!["a", "b"]);
        assert!(lines(b"\n\n\n").await.is_empty());
    }

    #[tokio::test]
    async fn yields_a_trailing_fragment_without_a_newline() {
        assert_eq!(lines(b"a\nb").await, vec!["a", "b"]);
        assert_eq!(lines(b"only").await, vec!["only"]);
    }

    #[tokio::test]
    async fn keeps_carriage_returns() {
        assert_eq!(lines(b"a\r\nb\r").await, vec!["a\r", "b\r"]);
    }

    #[tokio::test]
    async fn pi_strips_a_trailing_carriage_return() {
        assert_eq!(pi_lines(b"a\r\nb\r").await, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn reassembles_lines_split_across_reads() {
        // Longer than READ_CHUNK so the splitter has to buffer across reads.
        let payload: &'static [u8] = Box::leak(
            format!("{}\n{}\n", "a".repeat(20_000), "b".repeat(5))
                .into_bytes()
                .into_boxed_slice(),
        );
        let got = lines(payload).await;
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), MAX_LINE_LEN);
        assert_eq!(got[1], "bbbbb");
    }

    #[tokio::test]
    async fn caps_long_lines_but_pi_does_not() {
        let payload: &'static [u8] = Box::leak(
            format!("{}\n", "x".repeat(20_000))
                .into_bytes()
                .into_boxed_slice(),
        );
        assert_eq!(lines(payload).await[0].len(), MAX_LINE_LEN);
        assert_eq!(pi_lines(payload).await[0].len(), 20_000);
    }

    #[tokio::test]
    async fn decodes_utf8_per_line_and_replaces_invalid_bytes() {
        assert_eq!(lines("héllo\n".as_bytes()).await, vec!["héllo"]);
        assert_eq!(lines(b"a\xffb\n").await, vec!["a\u{fffd}b"]);
    }

    #[test]
    fn the_cap_counts_utf16_code_units_not_bytes() {
        // 8 192 astral characters = 16 384 UTF-16 units = exactly the cap,
        // but 32 768 bytes.
        let at_cap = "\u{1f600}".repeat(8_192);
        assert_eq!(cap_line(at_cap.clone()), at_cap);

        let over_cap = format!("{at_cap}\u{1f600}");
        assert_eq!(cap_line(over_cap), at_cap);

        // A character straddling the boundary is dropped whole.
        let straddling = format!("{}\u{1f600}", "x".repeat(MAX_LINE_LEN - 1));
        assert_eq!(cap_line(straddling), "x".repeat(MAX_LINE_LEN - 1));
    }

    #[test]
    fn the_cap_leaves_short_lines_alone() {
        assert_eq!(cap_line("short".to_owned()), "short");
        let exact = "y".repeat(MAX_LINE_LEN);
        assert_eq!(cap_line(exact.clone()), exact);
    }
}
