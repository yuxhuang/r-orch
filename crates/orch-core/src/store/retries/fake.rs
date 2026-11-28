//! Test-only retry rows: the reference for the table's rules, per project.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::Error;
use crate::store::StoreError;
use crate::store::blob::BlobStore;

use super::*;

use super::stream::{
    self, RETRY_STREAM_WINDOW_BYTES, RetryStreamAdmission, RetryStreamError, RetryTextPage,
    RetryTextRow, RetryTextSink, check_cancel, copy_text, select_retry_keys, verify_retry_objects,
};
use tokio_util::sync::CancellationToken;

trait RetryBlobs: Send + Sync {
    fn publish(&self, bytes: &[u8]) -> Result<Backing>;
    fn resolve(&self, backing: &Backing) -> Result<Vec<u8>>;
    fn preflight(&self, backing: &Backing) -> stream::Result<()>;
    fn stream(
        &self,
        backing: &Backing,
        window: &mut [u8],
        cancel: &CancellationToken,
        consume: &mut dyn FnMut(&[u8]) -> stream::Result<()>,
    ) -> stream::Result<()>;
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
    sizes: Vec<u64>,
    bytes: u64,
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(Error::other(format!(
        "memory reference retry text: {error}"
    )))
}

impl<B: BlobStore> RetryBlobs for Blobs<B> {
    fn publish(&self, bytes: &[u8]) -> Result<Backing> {
        let mut chunks = Vec::new();
        let mut sizes = Vec::new();
        for chunk in bytes.chunks(self.chunk_bytes) {
            let hash = self.store.put(chunk).map_err(backend)?;
            if self.store.get(&hash).map_err(backend)? != chunk {
                return Err(backend("published bytes differ"));
            }
            chunks.push(hash.to_string());
            sizes.push(chunk.len() as u64);
        }
        Ok(Backing {
            hash: (self.hash)(bytes).to_string(),
            chunks,
            sizes,
            bytes: bytes.len() as u64,
        })
    }

    fn preflight(&self, backing: &Backing) -> stream::Result<()> {
        backing
            .hash
            .parse::<B::Hash>()
            .map_err(|_| RetryStreamError::Corrupt)?;
        if backing.chunks.is_empty() || backing.chunks.len() != backing.sizes.len() {
            return Err(RetryStreamError::Corrupt);
        }
        let mut total = 0u64;
        for (ordinal, (name, size)) in backing.chunks.iter().zip(&backing.sizes).enumerate() {
            name.parse::<B::Hash>()
                .map_err(|_| RetryStreamError::Corrupt)?;
            if *size == 0
                || *size > self.chunk_bytes as u64
                || (ordinal + 1 < backing.chunks.len() && *size != self.chunk_bytes as u64)
            {
                return Err(RetryStreamError::Corrupt);
            }
            total = total.checked_add(*size).ok_or(RetryStreamError::Resource)?;
        }
        if total != backing.bytes || total <= MAX_RETRY_ERROR_BYTES as u64 {
            return Err(RetryStreamError::Corrupt);
        }
        Ok(())
    }

    fn stream(
        &self,
        backing: &Backing,
        window: &mut [u8],
        cancel: &CancellationToken,
        consume: &mut dyn FnMut(&[u8]) -> stream::Result<()>,
    ) -> stream::Result<()> {
        verify_retry_objects(
            &self.store,
            &backing.hash,
            backing
                .chunks
                .iter()
                .map(String::as_str)
                .zip(backing.sizes.iter().copied()),
            backing.bytes,
            window,
            cancel,
            consume,
        )
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
    rows: Arc<BTreeMap<String, Row>>,
    admission: RetryStreamAdmission,
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
            row.retry.error = String::new();
        }
        Arc::make_mut(&mut self.rows).insert(retry.task.clone(), row);
        Ok(())
    }

    pub fn delete(&mut self, task: &str) -> bool {
        Arc::make_mut(&mut self.rows).remove(task).is_some()
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

impl RetryState {
    pub fn admission(&self) -> RetryStreamAdmission {
        self.admission.clone()
    }

    pub fn stream_page(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
        sink: &mut dyn RetryTextSink,
        cancel: &CancellationToken,
    ) -> stream::Result<RetryTextPage> {
        let admission = &self.admission;
        let selected = select_retry_keys(
            self.rows
                .values()
                .map(|r| (r.retry.task.as_str(), r.retry.due_at)),
            after,
            limit,
            admission,
            cancel,
        )?;
        let mut bytes = 0u64;
        for (task, _) in &selected.keys {
            let row = &self.rows[*task];
            row.retry
                .validate()
                .map_err(|_| RetryStreamError::Corrupt)?;
            let length = match &row.backing {
                None => row.retry.error.len() as u64,
                Some(backing) => {
                    self.blobs
                        .as_ref()
                        .ok_or(RetryStreamError::Corrupt)?
                        .preflight(backing)?;
                    if backing.chunks.is_empty() || backing.chunks.len() != backing.sizes.len() {
                        return Err(RetryStreamError::Corrupt);
                    }
                    let sum = backing.sizes.iter().try_fold(0u64, |a, b| {
                        a.checked_add(*b).ok_or(RetryStreamError::Resource)
                    })?;
                    if sum != backing.bytes {
                        return Err(RetryStreamError::Corrupt);
                    }
                    backing.bytes
                }
            };
            bytes = bytes
                .checked_add(length)
                .ok_or(RetryStreamError::Resource)?;
        }
        let mut window = admission.buffer(RETRY_STREAM_WINDOW_BYTES)?;
        let mut next = None;
        let _cursor = admission.reserve(selected.keys.last().map_or(0, |(task, _)| task.len()))?;
        for (task, due) in &selected.keys {
            check_cancel(cancel)?;
            let row = &self.rows[*task];
            let length = row
                .backing
                .as_ref()
                .map_or(row.retry.error.len() as u64, |b| b.bytes);
            sink.begin(
                RetryTextRow {
                    task,
                    attempt: row.retry.attempt,
                    due_at: *due,
                    bytes: length,
                },
                admission,
            )?;
            let mut consume = |part: &[u8]| {
                check_cancel(cancel)?;
                sink.write(part, admission)
            };
            match &row.backing {
                None => {
                    for part in row.retry.error.as_bytes().chunks(RETRY_STREAM_WINDOW_BYTES) {
                        consume(part)?;
                    }
                }
                Some(backing) => self
                    .blobs
                    .as_ref()
                    .ok_or(RetryStreamError::Corrupt)?
                    .stream(backing, window.as_mut_slice(), cancel, &mut consume)?,
            }
        }
        if selected.more
            && let Some((task, due)) = selected.keys.last()
        {
            next = Some(RetryCursor {
                task: copy_text(task)?,
                due_at: *due,
            });
        }
        check_cancel(cancel)?;
        Ok(RetryTextPage {
            rows: selected.keys.len(),
            bytes,
            next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // An exact, reversible content name is enough for this private order proof;
    // disk-backed port tests separately use real SHA-256 and its object bound.
    struct TextBlobs;
    impl BlobStore for TextBlobs {
        type Hash = String;
        type Error = std::io::Error;
        fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
            String::from_utf8(bytes.to_vec()).map_err(std::io::Error::other)
        }
        fn get(&self, hash: &String) -> std::io::Result<Vec<u8>> {
            Ok(hash.as_bytes().to_vec())
        }
        fn remove(&self, _: &String) -> std::io::Result<bool> {
            Ok(false)
        }
    }
    fn name(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn reordered_retry_chunks_fail_every_memory_retry_read() {
        let mut state = RetryState::with_blob_store(TextBlobs, name, 32_768);
        let row = Retry {
            task: "t".into(),
            attempt: 1,
            due_at: 0,
            error: format!("{}{}", "a".repeat(32_768), "b".repeat(32_769)),
        };
        state.put(&row).unwrap();
        assert_eq!(state.get("t").unwrap(), Some(row));
        Arc::make_mut(&mut state.rows)
            .get_mut("t")
            .unwrap()
            .backing
            .as_mut()
            .unwrap()
            .chunks
            .swap(0, 1);
        for error in [
            state.get("t").unwrap_err(),
            state.due(i64::MAX, 1).unwrap_err(),
            state.page(None, 1).unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("reconstructed retry does not hash"),
                "{error}"
            );
        }
    }
}
