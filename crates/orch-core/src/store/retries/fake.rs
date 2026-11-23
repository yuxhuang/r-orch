//! Test-only retry rows: the reference for the table's rules, per project.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::Error;
use crate::store::StoreError;
use crate::store::blob::BlobStore;

use super::*;

trait RetryBlobs: Send + Sync {
    fn publish(&self, bytes: &[u8]) -> Result<Backing>;
    fn resolve(&self, backing: &Backing) -> Result<Vec<u8>>;
}

struct Blobs<B: BlobStore> {
    store: B,
    hash: fn(&[u8]) -> B::Hash,
    chunk_bytes: usize,
}

#[derive(Clone)]
struct Backing {
    hash: String,
    chunks: Vec<String>,
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(Error::other(format!(
        "memory reference retry text: {error}"
    )))
}

impl<B: BlobStore> RetryBlobs for Blobs<B> {
    fn publish(&self, bytes: &[u8]) -> Result<Backing> {
        let mut chunks = Vec::new();
        for chunk in bytes.chunks(self.chunk_bytes) {
            let hash = self.store.put(chunk).map_err(backend)?;
            if self.store.get(&hash).map_err(backend)? != chunk {
                return Err(backend("published bytes differ"));
            }
            chunks.push(hash.to_string());
        }
        Ok(Backing {
            hash: (self.hash)(bytes).to_string(),
            chunks,
        })
    }

    fn resolve(&self, backing: &Backing) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        for chunk in &backing.chunks {
            let hash = chunk
                .parse::<B::Hash>()
                .map_err(|_| backend("invalid blob hash"))?;
            bytes.extend(self.store.get(&hash).map_err(backend)?);
        }
        if (self.hash)(&bytes).to_string() != backing.hash {
            return Err(backend("reconstructed retry does not hash to its name"));
        }
        Ok(bytes)
    }
}

#[derive(Clone)]
struct Row {
    retry: Retry,
    backing: Option<Backing>,
}

#[derive(Clone, Default)]
pub struct RetryState {
    /// By task id: one row per task.
    rows: BTreeMap<String, Row>,
    blobs: Option<Arc<dyn RetryBlobs>>,
}

impl RetryState {
    /// The default is a logical, non-durable mapping: whole strings in memory,
    /// with no claim of blob backing. Inject the real blob port for publication
    /// and damaged-reference proofs; snapshots retain references, not byte copies.
    pub fn with_blob_store<B: BlobStore + 'static>(
        store: B,
        hash: fn(&[u8]) -> B::Hash,
        chunk_bytes: usize,
    ) -> Self {
        assert!(chunk_bytes > 0, "blob chunk size must be positive");
        Self {
            blobs: Some(Arc::new(Blobs {
                store,
                hash,
                chunk_bytes,
            })),
            ..Self::default()
        }
    }

    fn resolve(&self, row: &Row) -> Result<Retry> {
        let mut retry = row.retry.clone();
        if let Some(backing) = &row.backing {
            let bytes = self
                .blobs
                .as_ref()
                .ok_or_else(|| backend("no blob store"))?
                .resolve(backing)
                .map_err(|e| backend(format!("task {}: {e}", retry.task)))?;
            retry.error = String::from_utf8(bytes)
                .map_err(|e| backend(format!("task {}: {e}", retry.task)))?;
        }
        Ok(retry)
    }

    pub fn put(&mut self, retry: &Retry) -> Result<()> {
        retry.validate()?;
        let mut row = Row {
            retry: retry.clone(),
            backing: None,
        };
        if retry.error.len() > MAX_RETRY_ERROR_BYTES
            && let Some(blobs) = &self.blobs
        {
            row.backing = Some(blobs.publish(retry.error.as_bytes())?);
            row.retry.error.clear();
        }
        self.rows.insert(retry.task.clone(), row);
        Ok(())
    }

    pub fn delete(&mut self, task: &str) -> bool {
        self.rows.remove(task).is_some()
    }

    pub fn get(&self, task: &str) -> Result<Option<Retry>> {
        self.rows.get(task).map(|r| self.resolve(r)).transpose()
    }

    pub fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>> {
        check_limit(limit)?;
        let mut due: Vec<_> = self
            .rows
            .values()
            .filter(|r| r.retry.due_at <= now_ms)
            .collect();
        due.sort_by(|a, b| (a.retry.due_at, &a.retry.task).cmp(&(b.retry.due_at, &b.retry.task)));
        due.into_iter()
            .take(limit)
            .map(|r| self.resolve(r))
            .collect()
    }

    pub fn page(&self, after: Option<&RetryCursor>, limit: usize) -> Result<RetryPage> {
        check_limit(limit)?;
        if let Some(cursor) = after {
            cursor.validate()?;
        }
        let mut rest: Vec<_> = self
            .rows
            .values()
            .filter(|r| {
                after.is_none_or(|c| {
                    (r.retry.due_at, r.retry.task.as_str()) > (c.due_at, c.task.as_str())
                })
            })
            .collect();
        rest.sort_by(|a, b| (a.retry.due_at, &a.retry.task).cmp(&(b.retry.due_at, &b.retry.task)));
        let more = rest.len() > limit;
        let rows: Vec<Retry> = rest
            .into_iter()
            .take(limit)
            .map(|r| self.resolve(r))
            .collect::<Result<_>>()?;
        let next = rows.last().filter(|_| more).map(RetryCursor::after);
        Ok(RetryPage { rows, next })
    }
}
