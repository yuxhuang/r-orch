//! Content-addressed byte storage, independent of the entity transaction.
//!
//! Publish bytes first, then register their hash in the caller's unit of work.
//! A rolled-back reference may leave an unreferenced blob for maintenance.

use super::retries::stream::{Result as StreamResult, RetryStreamError};
use tokio_util::sync::CancellationToken;

/// Incremental content verification without allocating a whole logical value.
/// The adapter supplies this state on its stack through `with_stream_hasher`.
pub trait BlobStreamHasher {
    fn update(&mut self, bytes: &[u8]);
    fn matches(&self, expected: &str) -> bool;
}

pub trait BlobStore: Send + Sync {
    /// Only a validated content hash may select bytes; never a path or a name.
    type Hash: Clone + Eq + std::fmt::Display + std::str::FromStr + Send + Sync;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Return only after the bytes and their name are durable. Equal bytes deduplicate.
    fn put(&self, bytes: &[u8]) -> Result<Self::Hash, Self::Error>;
    /// Verify the bytes against the hash, failing on missing or corrupt data.
    fn get(&self, hash: &Self::Hash) -> Result<Vec<u8>, Self::Error>;
    /// Metadata-only preflight. Implementations must not fetch object payloads.
    /// Whole-only injected stores retain their old contract and refuse streaming.
    fn stream_len(&self, _hash: &Self::Hash) -> StreamResult<u64> {
        Err(stream_unavailable())
    }
    /// Stream into a caller-owned admitted window, verify the object hash and
    /// length, and check cancellation before reads, callbacks and success.
    /// Output remains provisional until this and the enclosing page succeed.
    fn read_windowed(
        &self,
        _hash: &Self::Hash,
        _window: &mut [u8],
        _cancel: &CancellationToken,
        _consume: &mut dyn FnMut(&[u8]) -> StreamResult<()>,
    ) -> StreamResult<()> {
        Err(stream_unavailable())
    }
    /// Supply stack-owned incremental state; no boxed hasher allocation or
    /// whole-value fallback. Unsupported injected stores refuse streaming.
    fn with_stream_hasher(
        &self,
        _work: &mut dyn FnMut(&mut dyn BlobStreamHasher) -> StreamResult<()>,
    ) -> StreamResult<()> {
        Err(stream_unavailable())
    }
    /// Idempotent removal. The caller must establish that no live reference remains.
    fn remove(&self, hash: &Self::Hash) -> Result<bool, Self::Error>;
}

fn stream_unavailable() -> RetryStreamError {
    RetryStreamError::Store(super::StoreError::Backend(crate::Error::other(
        "blob store does not support bounded streaming",
    )))
}
