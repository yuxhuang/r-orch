//! File-reference encoding for R97. The port continues to carry whole typed values.
//! Old inline rows remain readable. Hash-backed rows are reference-only fixtures;
//! the production v1 reader is never pointed at these documents.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orch_core::Error;
use orch_core::store::{Record, Result, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob::{BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use crate::data_dir::confined::ConfinedDir;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum StoredRecord {
    Inline(Record),
    External { row: Value, texts: Vec<TextRef> },
}

#[derive(Serialize, Deserialize)]
pub(super) struct TextRef {
    pointer: String,
    sha256: String,
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(Error::other(format!("file reference text: {error}")))
}

fn blobs(dir: &Path) -> Result<DiskBlobStore> {
    let root = ConfinedDir::open(dir).map_err(backend)?;
    DiskBlobStore::open(&root).map_err(backend)
}

impl StoredRecord {
    pub(super) fn encode(dir: &Path, record: &Record) -> Result<Self> {
        let mut row = serde_json::to_value(record).map_err(backend)?;
        let mut texts = Vec::new();
        externalize(dir, &mut row, String::new(), &mut texts)?;
        if texts.is_empty() {
            Ok(Self::Inline(record.clone()))
        } else {
            Ok(Self::External { row, texts })
        }
    }

    pub(super) fn resolve(self, dir: &Path) -> Result<Record> {
        match self {
            Self::Inline(record) => Ok(record),
            Self::External { mut row, texts } => {
                if texts.is_empty() {
                    return Err(backend("external row has no text references"));
                }
                let store = blobs(dir)?;
                for text in texts {
                    let hash = text.sha256.parse::<BlobHash>().map_err(backend)?;
                    let slot = row
                        .pointer_mut(&text.pointer)
                        .ok_or_else(|| backend("missing text slot"))?;
                    if !slot.is_null() {
                        return Err(backend("text slot is not null or appears twice"));
                    }
                    let bytes = store.get(&hash).map_err(backend)?;
                    *slot = Value::String(String::from_utf8(bytes).map_err(backend)?);
                }
                serde_json::from_value(row).map_err(backend)
            }
        }
    }
}

fn externalize(
    dir: &Path,
    value: &mut Value,
    pointer: String,
    texts: &mut Vec<TextRef>,
) -> Result<()> {
    match value {
        Value::String(text) if text.len() > INLINE_TEXT_MAX_BYTES => {
            let sha256 = blobs(dir)?
                .put(text.as_bytes())
                .map_err(backend)?
                .to_string();
            texts.push(TextRef { pointer, sha256 });
            *value = Value::Null;
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                externalize(dir, item, format!("{pointer}/{index}"), texts)?;
            }
        }
        Value::Object(fields) => {
            for (name, item) in fields {
                let escaped = name.replace('~', "~0").replace('/', "~1");
                externalize(dir, item, format!("{pointer}/{escaped}"), texts)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Retry diagnostics keep the old string encoding for inline rows.
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum StoredText {
    Inline(String),
    Chunked { sha256: String, chunks: Vec<String> },
    Blob { sha256: String },
}

pub(super) type RetryBlobStore =
    dyn orch_core::store::blob::BlobStore<Hash = BlobHash, Error = crate::blob::BlobError>;

#[derive(Clone)]
pub(super) struct RetryText {
    pub(super) dir: PathBuf,
    // A disk-adapter seam for reference-only publication fault proofs.
    pub(super) store: Option<Arc<RetryBlobStore>>,
}

impl RetryText {
    pub(super) fn encode(&self, text: &str) -> Result<StoredText> {
        let dir = &self.dir;
        if text.len() <= INLINE_TEXT_MAX_BYTES {
            return Ok(StoredText::Inline(text.into()));
        }
        std::fs::create_dir_all(dir).map_err(backend)?;
        let opened;
        let store: &RetryBlobStore = match &self.store {
            Some(store) => store.as_ref(),
            None => {
                opened = blobs(dir)?;
                &opened
            }
        };
        let mut chunks = Vec::new();
        for chunk in text.as_bytes().chunks(MAX_BLOB_BYTES as usize) {
            let hash = store.put(chunk).map_err(backend)?;
            // Verify every publication before even a staged row may refer to it.
            if store.get(&hash).map_err(backend)? != chunk {
                return Err(backend("published retry bytes differ"));
            }
            chunks.push(hash.to_string());
        }
        let sha256 = BlobHash::of(text.as_bytes()).to_string();
        if text.len() as u64 <= MAX_BLOB_BYTES {
            Ok(StoredText::Blob { sha256 })
        } else {
            Ok(StoredText::Chunked { sha256, chunks })
        }
    }

    fn with_store<T>(&self, read: impl FnOnce(&RetryBlobStore) -> Result<T>) -> Result<T> {
        match &self.store {
            Some(store) => read(store.as_ref()),
            None => read(&blobs(&self.dir)?),
        }
    }

    pub(super) fn resolve(&self, text: &StoredText) -> Result<String> {
        match text {
            StoredText::Inline(text) => Ok(text.clone()),
            StoredText::Blob { sha256 } => {
                let hash = sha256.parse::<BlobHash>().map_err(backend)?;
                self.with_store(|store| {
                    String::from_utf8(store.get(&hash).map_err(backend)?).map_err(backend)
                })
            }
            StoredText::Chunked { sha256, chunks } => {
                let hash = sha256.parse::<BlobHash>().map_err(backend)?;
                if chunks.is_empty() {
                    return Err(backend("retry has no chunks"));
                }
                self.with_store(|store| {
                    let mut bytes = Vec::new();
                    for chunk in chunks {
                        let chunk = chunk.parse::<BlobHash>().map_err(backend)?;
                        bytes.extend(store.get(&chunk).map_err(backend)?);
                    }
                    if BlobHash::of(&bytes) != hash {
                        return Err(backend("reconstructed retry does not hash to its name"));
                    }
                    String::from_utf8(bytes).map_err(backend)
                })
            }
        }
    }
}
