//! The stored form of one retry diagnostic (`retry_queue.error`), R97.
//!
//! The column holds one JSON document, so no diagnostic, whatever its bytes,
//! can be read as a reference. Text of at most [`INLINE_TEXT_MAX_BYTES`] UTF-8
//! bytes is the document's `inline` member. Longer text is published to the
//! blob store first and the document's `blob` member names it: the SHA-256 of
//! the whole value, its length and one entry per ordered object. Objects are
//! at most [`MAX_BLOB_BYTES`] each, so text up to one object is a single entry
//! whose hash is the whole hash, and longer text is split by bytes: a
//! character may straddle a boundary, and the joined bytes are decoded only
//! after the whole length and hash match. Every occurrence keeps its position,
//! so identical objects are listed once for each. The whole hash of a split
//! value names no object.
//!
//! This mapping shares no table or tag with the task and entity formats; it
//! uses the same two thresholds and the same split.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::blob::{BlobError, BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use crate::data_dir::confined::ConfinedError;

pub(super) const FORMAT: u8 = 1;
/// The most bytes one stored document may hold. A manifest entry is about 110
/// bytes, so this admits text of more than two TiB; it exists so that a damaged
/// column is refused before it is parsed or allocated.
pub(super) const MAX_STORED_BYTES: usize = 4 * 1024 * 1024;

/// The blob store as this mapping uses it. A test wraps the disk adapter to fail
/// one publication; production is the adapter itself.
pub(super) trait Objects {
    fn put(&self, bytes: &[u8]) -> std::result::Result<BlobHash, BlobError>;
    fn get(&self, hash: &BlobHash) -> std::result::Result<Vec<u8>, BlobError>;
}

impl Objects for DiskBlobStore {
    fn put(&self, bytes: &[u8]) -> std::result::Result<BlobHash, BlobError> {
        DiskBlobStore::put(self, bytes)
    }
    fn get(&self, hash: &BlobHash) -> std::result::Result<Vec<u8>, BlobError> {
        DiskBlobStore::get(self, hash)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    v: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blob: Option<Blob>,
}

/// Where the bytes of one indirect text are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sqlite) struct Blob {
    pub sha256: String,
    pub bytes: u64,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sqlite) struct Chunk {
    pub sha256: String,
    pub size: u64,
}

/// A stored document after its structure was checked, before any object access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stored {
    Inline(String),
    Blob(Blob),
}

#[derive(Debug)]
pub(in crate::sqlite) enum TextError {
    /// Publishing an object failed; nothing refers to it.
    Publish(BlobError),
    /// The text cannot be stored in the schema's bounds.
    Unrepresentable,
    /// A transient failure to open or read an object; it proves no damage.
    Access(std::io::ErrorKind),
    /// Stored data that cannot be a document this mapping wrote.
    Corrupt(&'static str),
}

type Result<T> = std::result::Result<T, TextError>;

/// A published object that the row will name, once for each distinct hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Object {
    pub hash: BlobHash,
    pub size: u64,
}

pub(super) struct Prepared {
    pub document: String,
    pub objects: Vec<Object>,
}

fn expected_size(bytes: u64, ordinal: u64) -> u64 {
    (bytes - ordinal * MAX_BLOB_BYTES).min(MAX_BLOB_BYTES)
}

/// Publish every object, verify each, and only then return the document. A
/// failure returns before any row can exist; objects already published stay
/// unreferenced for the normal sweep.
pub(super) fn encode(objects: &dyn Objects, text: &str) -> Result<Prepared> {
    let bytes = text.as_bytes();
    if bytes.len() <= INLINE_TEXT_MAX_BYTES {
        let document = Document {
            v: FORMAT,
            inline: Some(text.to_owned()),
            blob: None,
        };
        return Ok(Prepared {
            document: serialize(&document)?,
            objects: Vec::new(),
        });
    }
    let mut chunks = Vec::new();
    let mut distinct = Vec::new();
    let mut seen = HashSet::new();
    for part in bytes.chunks(MAX_BLOB_BYTES as usize) {
        let hash = objects.put(part).map_err(TextError::Publish)?;
        // The row may not name bytes that cannot be read back, so each
        // publication is verified, not assumed. The adapter checks the bytes
        // against the name it reads, so equal bytes also prove the name.
        if objects.get(&hash).map_err(TextError::Publish)? != part {
            return Err(TextError::Corrupt("published bytes"));
        }
        if seen.insert(hash) {
            distinct.push(Object {
                hash,
                size: part.len() as u64,
            });
        }
        chunks.push(Chunk {
            sha256: hash.to_string(),
            size: part.len() as u64,
        });
    }
    let document = Document {
        v: FORMAT,
        inline: None,
        blob: Some(Blob {
            sha256: BlobHash::of(bytes).to_string(),
            bytes: bytes.len() as u64,
            chunks,
        }),
    };
    Ok(Prepared {
        document: serialize(&document)?,
        objects: distinct,
    })
}

fn serialize(document: &Document) -> Result<String> {
    let text = serde_json::to_string(document).map_err(|_| TextError::Unrepresentable)?;
    if text.len() > MAX_STORED_BYTES {
        return Err(TextError::Unrepresentable);
    }
    Ok(text)
}

/// Check a stored document without opening an object. The checks cover the
/// version, the exactly-one member rule, the sizes and the manifest's shape, so
/// proven damage is never hidden behind an object access error.
pub(super) fn parse(stored: &str) -> Result<Stored> {
    if stored.len() > MAX_STORED_BYTES {
        return Err(TextError::Corrupt("document size"));
    }
    let document: Document =
        serde_json::from_str(stored).map_err(|_| TextError::Corrupt("document syntax"))?;
    if document.v != FORMAT {
        return Err(TextError::Corrupt("format version"));
    }
    match (document.inline, document.blob) {
        (Some(text), None) => {
            if text.len() > INLINE_TEXT_MAX_BYTES {
                return Err(TextError::Corrupt("inline size"));
            }
            Ok(Stored::Inline(text))
        }
        (None, Some(blob)) => {
            check(&blob)?;
            Ok(Stored::Blob(blob))
        }
        _ => Err(TextError::Corrupt("document members")),
    }
}

fn check(blob: &Blob) -> Result<()> {
    blob.sha256
        .parse::<BlobHash>()
        .map_err(|_| TextError::Corrupt("whole hash syntax"))?;
    if blob.bytes <= INLINE_TEXT_MAX_BYTES as u64 {
        return Err(TextError::Corrupt("indirect size"));
    }
    if blob.chunks.len() as u64 != blob.bytes.div_ceil(MAX_BLOB_BYTES) {
        return Err(TextError::Corrupt("chunk count"));
    }
    for (ordinal, chunk) in blob.chunks.iter().enumerate() {
        if chunk.size != expected_size(blob.bytes, ordinal as u64) {
            return Err(TextError::Corrupt("chunk size"));
        }
        chunk
            .sha256
            .parse::<BlobHash>()
            .map_err(|_| TextError::Corrupt("chunk hash syntax"))?;
    }
    Ok(())
}

/// Join the objects in order and verify the whole length and hash before the
/// bytes are decoded. Nothing is allocated from a stored length: the buffer
/// grows only from objects the adapter has verified.
pub(super) fn reconstruct(objects: &dyn Objects, blob: &Blob) -> Result<String> {
    check(blob)?;
    let whole: BlobHash = blob
        .sha256
        .parse()
        .map_err(|_| TextError::Corrupt("whole hash syntax"))?;
    let mut bytes = Vec::new();
    for chunk in &blob.chunks {
        let hash: BlobHash = chunk
            .sha256
            .parse()
            .map_err(|_| TextError::Corrupt("chunk hash syntax"))?;
        let part = objects.get(&hash).map_err(|error| match error {
            BlobError::Io(error) | BlobError::Confined(ConfinedError::Io(error))
                if error.kind() != std::io::ErrorKind::NotFound =>
            {
                TextError::Access(error.kind())
            }
            _ => TextError::Corrupt("chunk object"),
        })?;
        if part.len() as u64 != chunk.size {
            return Err(TextError::Corrupt("chunk object size"));
        }
        bytes.extend_from_slice(&part);
    }
    if bytes.len() as u64 != blob.bytes || BlobHash::of(&bytes) != whole {
        return Err(TextError::Corrupt("whole length or hash"));
    }
    String::from_utf8(bytes).map_err(|_| TextError::Corrupt("text encoding"))
}

/// Test-only oracle for the original isolated verification leaf. The public
/// adapter uses the shared core verifier and its resource/cancellation errors.
#[derive(Debug)]
#[cfg(test)]
pub(in crate::sqlite) enum StreamError {
    Text(TextError),
    Resource,
    Cancelled,
}

#[cfg(test)]
pub(in crate::sqlite) fn checked_page_bytes(
    total: u64,
    bytes: u64,
) -> std::result::Result<u64, StreamError> {
    total.checked_add(bytes).ok_or(StreamError::Resource)
}

// Incremental UTF-8 validation needs at most three pending bytes. Use the
// standard decoder so overlong forms, surrogates and out-of-range scalars
// receive exactly the same validation as a complete String.
#[cfg(test)]
#[derive(Default)]
struct Utf8Carry {
    bytes: [u8; 4],
    len: usize,
}

#[cfg(test)]
impl Utf8Carry {
    fn feed(&mut self, mut bytes: &[u8]) -> std::result::Result<(), StreamError> {
        let invalid = || StreamError::Text(TextError::Corrupt("text encoding"));
        while self.len != 0 && !bytes.is_empty() {
            self.bytes[self.len] = bytes[0];
            self.len += 1;
            bytes = &bytes[1..];
            match std::str::from_utf8(&self.bytes[..self.len]) {
                Ok(_) => self.len = 0,
                Err(error) if error.error_len().is_none() && self.len < 4 => {}
                Err(_) => return Err(invalid()),
            }
        }
        match std::str::from_utf8(bytes) {
            Ok(_) => Ok(()),
            Err(error) if error.error_len().is_none() => {
                let tail = &bytes[error.valid_up_to()..];
                self.bytes[..tail.len()].copy_from_slice(tail);
                self.len = tail.len();
                Ok(())
            }
            Err(_) => Err(invalid()),
        }
    }
}

/// Verify a value without joining objects or collecting its output. `read`
/// must stream the selected object from confined storage, fallibly allocating
/// at most one 64-KiB window; it must not call the whole-object `get` API.
/// `consume` is synchronous backpressure and receives PROVISIONAL bytes. Its
/// owner must discard all provisional output (including earlier rows) on any
/// error or cancellation, and publish only after the enclosing page succeeds.
/// Neither callback may retain windows without charging shared admission.
/// This leaf deliberately does not call the existing infallible disk reader.
#[cfg(test)]
pub(in crate::sqlite) fn verify_stream(
    blob: &Blob,
    mut read: impl FnMut(
        &BlobHash,
        &mut dyn FnMut(&[u8]) -> std::result::Result<(), StreamError>,
    ) -> std::result::Result<(), StreamError>,
    mut consume: impl FnMut(&[u8]) -> std::result::Result<(), StreamError>,
) -> std::result::Result<(), StreamError> {
    use sha2::{Digest, Sha256};
    check(blob).map_err(StreamError::Text)?;
    let mut whole = Sha256::new();
    let mut total = 0u64;
    let mut utf8 = Utf8Carry::default();
    for chunk in &blob.chunks {
        let hash = chunk
            .sha256
            .parse::<BlobHash>()
            .map_err(|_| StreamError::Text(TextError::Corrupt("chunk hash syntax")))?;
        let mut object = Sha256::new();
        let mut seen = 0u64;
        // Latch a callback failure: a faulty read implementation cannot hide
        // sink cancellation or validation failure by catching the error.
        let mut failure = None;
        let result = read(&hash, &mut |part| {
            let result = (|| {
                if failure.is_some() {
                    return Err(StreamError::Cancelled);
                }
                if part.len() > 64 * 1024 {
                    return Err(StreamError::Resource);
                }
                seen = checked_page_bytes(seen, part.len() as u64)?;
                if seen > chunk.size {
                    return Err(StreamError::Text(TextError::Corrupt("chunk object size")));
                }
                utf8.feed(part)?;
                object.update(part);
                whole.update(part);
                consume(part)
            })();
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                    Err(StreamError::Cancelled)
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        result?;
        if seen != chunk.size || BlobHash::from_digest(object.finalize().into()) != hash {
            return Err(StreamError::Text(TextError::Corrupt(
                "chunk length or hash",
            )));
        }
        total = checked_page_bytes(total, seen)?;
    }
    let hash = blob
        .sha256
        .parse::<BlobHash>()
        .map_err(|_| StreamError::Text(TextError::Corrupt("whole hash syntax")))?;
    if total != blob.bytes || BlobHash::from_digest(whole.finalize().into()) != hash {
        return Err(StreamError::Text(TextError::Corrupt(
            "whole length or hash",
        )));
    }
    if utf8.len != 0 {
        return Err(StreamError::Text(TextError::Corrupt("text encoding")));
    }
    Ok(())
}

// serde_json's owned String/Vec decoding uses infallible allocation. This
// bounded reader needs fallible allocation instead; no maintained parser in
// the current dependency set provides that contract. This small reader accepts
// the existing document grammar (including escaped strings/keys), not a new
// format. All output capacities are admitted before this function is entered.
pub(super) fn parse_stream(stored: &str) -> orch_core::store::retries::stream::Result<Stored> {
    use orch_core::store::retries::stream::RetryStreamError as E;
    if stored.len() > MAX_STORED_BYTES {
        return Err(E::Corrupt);
    }
    let mut json = StreamJson {
        bytes: stored.as_bytes(),
        at: 0,
    };
    json.expect(b'{')?;
    let mut version = None;
    let mut inline = None;
    let mut blob = None;
    let mut fields = 0u8;
    while !json.take(b'}') {
        let key = json.string()?;
        json.expect(b':')?;
        match key.as_str() {
            "v" if version.is_none() => version = Some(json.number()?),
            "inline" if fields & 1 == 0 => {
                fields |= 1;
                if !json.null() {
                    inline = Some(json.string()?);
                }
            }
            "blob" if fields & 2 == 0 => {
                fields |= 2;
                if !json.null() {
                    blob = Some(json.blob()?);
                }
            }
            _ => return Err(E::Corrupt),
        }
        if json.take(b'}') {
            break;
        }
        json.expect(b',')?;
        if json.peek() == Some(b'}') {
            return Err(E::Corrupt);
        }
    }
    if version != Some(FORMAT as u64) || json.peek().is_some() {
        return Err(E::Corrupt);
    }
    match (inline, blob) {
        (Some(text), None) if text.len() <= INLINE_TEXT_MAX_BYTES => Ok(Stored::Inline(text)),
        (None, Some(blob)) => {
            check(&blob).map_err(|_| E::Corrupt)?;
            Ok(Stored::Blob(blob))
        }
        _ => Err(E::Corrupt),
    }
}
struct StreamJson<'a> {
    bytes: &'a [u8],
    at: usize,
}
type StreamResult<T> = orch_core::store::retries::stream::Result<T>;
use orch_core::store::retries::stream::RetryStreamError as StreamFailure;
impl StreamJson<'_> {
    fn null(&mut self) -> bool {
        self.peek();
        if self.bytes.get(self.at..self.at.saturating_add(4)) == Some(b"null") {
            self.at += 4;
            true
        } else {
            false
        }
    }
    fn peek(&mut self) -> Option<u8> {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.at += 1;
        }
        self.bytes.get(self.at).copied()
    }
    fn take(&mut self, token: u8) -> bool {
        if self.peek() == Some(token) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, token: u8) -> StreamResult<()> {
        if self.take(token) {
            Ok(())
        } else {
            Err(StreamFailure::Corrupt)
        }
    }
    fn number(&mut self) -> StreamResult<u64> {
        self.peek();
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        let bytes = &self.bytes[start..self.at];
        if bytes.is_empty() || (bytes.len() > 1 && bytes[0] == b'0') {
            return Err(StreamFailure::Corrupt);
        }
        std::str::from_utf8(bytes)
            .map_err(|_| StreamFailure::Corrupt)?
            .parse()
            .map_err(|_| StreamFailure::Corrupt)
    }
    fn string(&mut self) -> StreamResult<String> {
        self.expect(b'"')?;
        let start = self.at;
        let mut end = start;
        loop {
            match self.bytes.get(end) {
                Some(b'"') => break,
                Some(b'\\') => end = end.checked_add(2).ok_or(StreamFailure::Resource)?,
                Some(0..=31) | None => return Err(StreamFailure::Corrupt),
                _ => end += 1,
            }
        }
        let mut out = String::new();
        out.try_reserve_exact(end - start)
            .map_err(|_| StreamFailure::Resource)?;
        while self.at < end {
            let begin = self.at;
            while self.at < end && self.bytes[self.at] != b'\\' {
                self.at += 1;
            }
            out.push_str(
                std::str::from_utf8(&self.bytes[begin..self.at])
                    .map_err(|_| StreamFailure::Corrupt)?,
            );
            if self.at == end {
                break;
            }
            self.at += 1;
            let escape = *self.bytes.get(self.at).ok_or(StreamFailure::Corrupt)?;
            self.at += 1;
            match escape {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let mut scalar = self.hex()?;
                    if (0xd800..=0xdbff).contains(&scalar) {
                        if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
                            return Err(StreamFailure::Corrupt);
                        }
                        self.at += 2;
                        let low = self.hex()?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err(StreamFailure::Corrupt);
                        }
                        scalar = 0x10000 + ((scalar - 0xd800) << 10) + low - 0xdc00;
                    }
                    out.push(char::from_u32(scalar).ok_or(StreamFailure::Corrupt)?);
                }
                _ => return Err(StreamFailure::Corrupt),
            }
        }
        if self.at != end {
            return Err(StreamFailure::Corrupt);
        }
        self.at += 1;
        Ok(out)
    }
    fn hex(&mut self) -> StreamResult<u32> {
        let mut scalar = 0;
        for _ in 0..4 {
            let byte = *self.bytes.get(self.at).ok_or(StreamFailure::Corrupt)?;
            self.at += 1;
            scalar = scalar * 16
                + match byte {
                    b'0'..=b'9' => (byte - b'0') as u32,
                    b'a'..=b'f' => (byte - b'a' + 10) as u32,
                    b'A'..=b'F' => (byte - b'A' + 10) as u32,
                    _ => return Err(StreamFailure::Corrupt),
                };
        }
        Ok(scalar)
    }
    fn blob(&mut self) -> StreamResult<Blob> {
        self.expect(b'{')?;
        let mut hash = None;
        let mut bytes = None;
        let mut chunks = None;
        while !self.take(b'}') {
            let key = self.string()?;
            self.expect(b':')?;
            match key.as_str() {
                "sha256" if hash.is_none() => hash = Some(self.string()?),
                "bytes" if bytes.is_none() => bytes = Some(self.number()?),
                "chunks" if chunks.is_none() => {
                    self.expect(b'[')?;
                    let mut values = Vec::new();
                    while !self.take(b']') {
                        values.try_reserve(1).map_err(|_| StreamFailure::Resource)?;
                        values.push(self.chunk()?);
                        if self.take(b']') {
                            break;
                        }
                        self.expect(b',')?;
                        if self.peek() == Some(b']') {
                            return Err(StreamFailure::Corrupt);
                        }
                    }
                    chunks = Some(values);
                }
                _ => return Err(StreamFailure::Corrupt),
            }
            if self.take(b'}') {
                break;
            }
            self.expect(b',')?;
            if self.peek() == Some(b'}') {
                return Err(StreamFailure::Corrupt);
            }
        }
        Ok(Blob {
            sha256: hash.ok_or(StreamFailure::Corrupt)?,
            bytes: bytes.ok_or(StreamFailure::Corrupt)?,
            chunks: chunks.ok_or(StreamFailure::Corrupt)?,
        })
    }
    fn chunk(&mut self) -> StreamResult<Chunk> {
        self.expect(b'{')?;
        let mut hash = None;
        let mut size = None;
        while !self.take(b'}') {
            let key = self.string()?;
            self.expect(b':')?;
            match key.as_str() {
                "sha256" if hash.is_none() => hash = Some(self.string()?),
                "size" if size.is_none() => size = Some(self.number()?),
                _ => return Err(StreamFailure::Corrupt),
            }
            if self.take(b'}') {
                break;
            }
            self.expect(b',')?;
            if self.peek() == Some(b'}') {
                return Err(StreamFailure::Corrupt);
            }
        }
        Ok(Chunk {
            sha256: hash.ok_or(StreamFailure::Corrupt)?,
            size: size.ok_or(StreamFailure::Corrupt)?,
        })
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;

    fn manifest(parts: &[&[u8]]) -> Blob {
        let all: Vec<_> = parts.iter().flat_map(|part| part.iter().copied()).collect();
        Blob {
            sha256: BlobHash::of(&all).to_string(),
            bytes: all.len() as u64,
            chunks: parts
                .iter()
                .map(|part| Chunk {
                    sha256: BlobHash::of(part).to_string(),
                    size: part.len() as u64,
                })
                .collect(),
        }
    }

    #[test]
    fn streaming_verifier_preserves_occurrences_and_split_utf8() {
        // Feed windows across character boundaries, independently of object shape.
        let bytes = "é🙂".repeat(20_000).into_bytes();
        let blob = manifest(&[&bytes]);
        let mut seen = 0;
        verify_stream(
            &blob,
            |_, consume| {
                for part in bytes.chunks(3) {
                    consume(part)?;
                }
                Ok(())
            },
            |part| {
                assert_eq!(part, &bytes[seen..seen + part.len()]);
                seen += part.len();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, bytes.len());
    }

    #[test]
    fn streaming_verifier_refuses_object_hash_length_whole_hash_and_encoding() {
        let bytes = vec![b'a'; INLINE_TEXT_MAX_BYTES + 1];
        for defect in 0..4 {
            let mut blob = manifest(&[&bytes]);
            let mut supplied = bytes.clone();
            match defect {
                0 => supplied[0] = b'b',
                1 => {
                    supplied.pop();
                }
                2 => blob.sha256 = BlobHash::of(b"wrong").to_string(),
                _ => {
                    supplied[0] = 0xff;
                    blob = manifest(&[&supplied]);
                }
            }
            assert!(
                verify_stream(
                    &blob,
                    |_, consume| supplied.chunks(65536).try_for_each(consume),
                    |_| Ok(())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn streaming_verifier_propagates_cancellation_without_success() {
        let bytes = vec![b'a'; INLINE_TEXT_MAX_BYTES + 1];
        let blob = manifest(&[&bytes]);
        let result = verify_stream(
            &blob,
            |_, consume| bytes.chunks(65536).try_for_each(consume),
            |_| Err(StreamError::Cancelled),
        );
        assert!(matches!(result, Err(StreamError::Cancelled)));
    }

    #[test]
    fn streaming_verifier_cannot_swallow_resource_or_sink_failure() {
        let bytes = vec![b'a'; INLINE_TEXT_MAX_BYTES + 1];
        let blob = manifest(&[&bytes]);
        let result = verify_stream(
            &blob,
            |_, consume| {
                let _ = consume(&bytes); // An oversized window must latch Resource.
                let _ = consume(&bytes[..1]); // Later callbacks must not overwrite it.
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(matches!(result, Err(StreamError::Resource)));
        let result = verify_stream(
            &blob,
            |_, consume| {
                let _ = consume(&bytes[..65536]);
                Ok(())
            },
            |_| Err(StreamError::Cancelled),
        );
        assert!(matches!(result, Err(StreamError::Cancelled)));
    }

    #[test]
    fn streaming_verifier_reads_every_repeated_object_occurrence() {
        use sha2::{Digest, Sha256};
        let window = [b'x'; 65536];
        let mut object = Sha256::new();
        let mut whole = Sha256::new();
        for _ in 0..MAX_BLOB_BYTES / window.len() as u64 {
            object.update(window);
        }
        for _ in 0..2 * MAX_BLOB_BYTES / window.len() as u64 {
            whole.update(window);
        }
        let hash = BlobHash::from_digest(object.finalize().into());
        let blob = Blob {
            sha256: BlobHash::from_digest(whole.finalize().into()).to_string(),
            bytes: 2 * MAX_BLOB_BYTES,
            chunks: vec![
                Chunk {
                    sha256: hash.to_string(),
                    size: MAX_BLOB_BYTES
                };
                2
            ],
        };
        let mut reads = 0;
        let mut emitted = 0u64;
        verify_stream(
            &blob,
            |selected, consume| {
                assert_eq!(*selected, hash);
                reads += 1;
                for _ in 0..MAX_BLOB_BYTES / window.len() as u64 {
                    consume(&window)?;
                }
                Ok(())
            },
            |part| {
                emitted += part.len() as u64;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(reads, 2);
        assert_eq!(emitted, 2 * MAX_BLOB_BYTES);
    }

    #[test]
    fn streaming_admission_is_shared_between_threads() {
        let budget = orch_core::store::retries::stream::RetryStreamAdmission::new(8);
        std::thread::scope(|scope| {
            let _permit = budget.reserve(8).unwrap();
            scope
                .spawn(|| assert!(budget.reserve(1).is_err()))
                .join()
                .unwrap();
        });
        assert!(budget.reserve(8).is_ok());
    }

    #[test]
    fn streaming_admission_is_checked_shared_and_released() {
        let budget = orch_core::store::retries::stream::RetryStreamAdmission::new(8);
        let permit = budget.reserve(5).unwrap();
        assert!(budget.reserve(4).is_err());
        assert!(budget.reserve(usize::MAX).is_err());
        let other = budget.reserve(3).unwrap();
        drop(permit);
        assert!(budget.reserve(5).is_ok());
        drop(other);
        assert!(budget.reserve(8).is_ok());
        assert!(checked_page_bytes(u64::MAX, 1).is_err());
    }
}

#[cfg(test)]
mod fallible_document_tests {
    use super::*;
    #[test]
    fn fallible_reader_matches_existing_writer_and_reader_for_escaped_text_and_manifests() {
        // Other side's real reader, not a re-read using the new decoder.
        for text in [
            String::new(),
            "plain é🙂".into(),
            "\0\u{1}\u{8}\u{c}\n\r\t\"\\/".into(),
            "é".repeat(32_768),
        ] {
            let document = Document {
                v: FORMAT,
                inline: Some(text),
                blob: None,
            };
            let encoded = serialize(&document).unwrap();
            assert_eq!(parse_stream(&encoded).unwrap(), parse(&encoded).unwrap());
        }
        let encoded = r#"{"inline":"\uD83D\uDE42\u00e9","v":1}"#;
        assert_eq!(parse_stream(encoded).unwrap(), parse(encoded).unwrap());
        let bytes = MAX_BLOB_BYTES + 1;
        let blob = Blob {
            sha256: BlobHash::of(b"whole").to_string(),
            bytes,
            chunks: vec![
                Chunk {
                    sha256: BlobHash::of(b"one").to_string(),
                    size: MAX_BLOB_BYTES,
                },
                Chunk {
                    sha256: BlobHash::of(b"two").to_string(),
                    size: 1,
                },
            ],
        };
        let encoded = serialize(&Document {
            v: FORMAT,
            inline: None,
            blob: Some(blob),
        })
        .unwrap();
        assert_eq!(parse_stream(&encoded).unwrap(), parse(&encoded).unwrap());
        for bad in [
            r#"{"v":1,"inline":"x",}"#,
            r#"{"v":1,"inline":"\uD800"}"#,
            r#"{"v":1,"inline":"\q"}"#,
            r#"{"v":01,"inline":"x"}"#,
            r#"{"v":1,"v":1,"inline":"x"}"#,
        ] {
            assert!(parse_stream(bad).is_err());
            assert!(parse(bad).is_err());
        }
    }
}
