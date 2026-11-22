//! Content-addressed byte storage, independent of the entity transaction.
//!
//! Publish bytes first, then register their hash in the caller's unit of work.
//! A rolled-back reference may leave an unreferenced blob for maintenance.

pub trait BlobStore: Send + Sync {
    /// Only a validated content hash may select bytes; never a path or a name.
    type Hash: Clone + Eq + std::fmt::Display + std::str::FromStr + Send + Sync;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Return only after the bytes and their name are durable. Equal bytes deduplicate.
    fn put(&self, bytes: &[u8]) -> Result<Self::Hash, Self::Error>;
    /// Verify the bytes against the hash, failing on missing or corrupt data.
    fn get(&self, hash: &Self::Hash) -> Result<Vec<u8>, Self::Error>;
    /// Idempotent removal. The caller must establish that no live reference remains.
    fn remove(&self, hash: &Self::Hash) -> Result<bool, Self::Error>;
}
