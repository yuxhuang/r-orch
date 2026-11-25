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
pub(super) struct Blob {
    pub sha256: String,
    pub bytes: u64,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Chunk {
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
pub(super) enum TextError {
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
