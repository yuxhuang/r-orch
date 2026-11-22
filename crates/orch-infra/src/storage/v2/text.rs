//! File-reference encoding for R97. The port continues to carry whole typed values.
//! Old inline rows remain readable. Hash-backed rows are reference-only fixtures;
//! the production v1 reader is never pointed at these documents.
use std::path::Path;

use orch_core::Error;
use orch_core::store::{Record, Result, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob::{BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES};
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
