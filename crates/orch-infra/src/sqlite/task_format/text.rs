//! R97's byte-level format, shared by a task's text slots and root detail.
use crate::blob::{BlobError, BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use serde::{Deserialize, Serialize};

/// The tag is an object in a string slot; arbitrary user strings cannot alias it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tag {
    #[serde(rename = "$blob")]
    pub reference: Reference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    pub sha256: String,
    pub bytes: u64,
    pub chunks: u64,
}

/// One ordinal at one locator, including repeated references to the same hash.
/// The SQL owner adds project, task and RFC 6901 locator to each row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Chunk {
    pub ordinal: u64,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TextError {
    #[error("task text is not large enough for indirect storage")]
    Inline,
    #[error(transparent)]
    Publish(#[from] BlobError),
    /// The adapter adds the task id and its known locator at the R140 boundary.
    /// Never carry blob errors here: they may include filesystem diagnostics.
    #[error("stored task text failed {0}")]
    Corrupt(&'static str),
}

type Result<T> = std::result::Result<T, TextError>;

/// Publish complete immutable objects before the caller installs any rows.
/// On failure, objects already published remain unreachable for the normal sweep.
/// Split bytes, not characters: only reconstruction decodes UTF-8.
pub(super) fn publish(store: &DiskBlobStore, bytes: &[u8]) -> Result<(Reference, Vec<Chunk>)> {
    if bytes.len() <= INLINE_TEXT_MAX_BYTES {
        return Err(TextError::Inline);
    }
    let mut rows = Vec::new();
    for (ordinal, part) in bytes.chunks(MAX_BLOB_BYTES as usize).enumerate() {
        let expected = BlobHash::of(part);
        let actual = store.put(part)?;
        if actual != expected {
            return Err(TextError::Corrupt("published hash"));
        }
        rows.push(Chunk {
            ordinal: ordinal as u64,
            sha256: actual.to_string(),
            size: part.len() as u64,
        });
    }
    Ok((
        Reference {
            sha256: BlobHash::of(bytes).to_string(),
            bytes: bytes.len() as u64,
            chunks: rows.len() as u64,
        },
        rows,
    ))
}

/// Validate structure before opening any object. Lengths from stored rows never
/// determine an allocation: grow only from objects that the blob adapter verified.
/// The caller separately verifies the `blobs` rows in the selecting transaction.
pub(super) fn reconstruct(
    store: &DiskBlobStore,
    reference: &Reference,
    rows: &[Chunk],
) -> Result<Vec<u8>> {
    let whole = reference
        .sha256
        .parse::<BlobHash>()
        .map_err(|_| TextError::Corrupt("whole hash syntax"))?;
    if reference.bytes <= INLINE_TEXT_MAX_BYTES as u64 {
        return Err(TextError::Corrupt("indirect size"));
    }
    let count = reference.bytes.div_ceil(MAX_BLOB_BYTES);
    if reference.chunks != count || rows.len() as u64 != count {
        return Err(TextError::Corrupt("chunk count"));
    }
    // Check all metadata first, including rows after a broken object. Multiplication
    // is bounded by bytes: ordinal is strictly below ceil(bytes / chunk size).
    for (ordinal, row) in rows.iter().enumerate() {
        let ordinal = ordinal as u64;
        let size = (reference.bytes - ordinal * MAX_BLOB_BYTES).min(MAX_BLOB_BYTES);
        if row.ordinal != ordinal || row.size != size {
            return Err(TextError::Corrupt("chunk ordinal or size"));
        }
        row.sha256
            .parse::<BlobHash>()
            .map_err(|_| TextError::Corrupt("chunk hash syntax"))?;
    }
    let mut bytes = Vec::new();
    for row in rows {
        let hash = row
            .sha256
            .parse::<BlobHash>()
            .map_err(|_| TextError::Corrupt("chunk hash syntax"))?;
        let part = store
            .get(&hash)
            .map_err(|_| TextError::Corrupt("chunk object"))?;
        if part.len() as u64 != row.size {
            return Err(TextError::Corrupt("chunk object size"));
        }
        bytes.extend_from_slice(&part);
    }
    if bytes.len() as u64 != reference.bytes || BlobHash::of(&bytes) != whole {
        return Err(TextError::Corrupt("whole length or hash"));
    }
    Ok(bytes)
}

pub(super) fn reconstruct_text(
    store: &DiskBlobStore,
    reference: &Reference,
    rows: &[Chunk],
) -> Result<String> {
    String::from_utf8(reconstruct(store, reference, rows)?).map_err(|_| TextError::Corrupt("UTF-8"))
}

#[cfg(test)]
mod tests;
