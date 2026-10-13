//! File-based shared context store — ported from
//! `src/infrastructure/storage/context-store.ts`.
//!
//! Entries are individual JSON files plus an `_index.json`. TTL is optional
//! and expiry is **lazy**: `get` deletes the one entry it was asked for, and
//! `list` purges every expired entry in a batch with a single index rewrite
//! (PARITY.md §7.17).
//!
//! The clock is injected rather than read from the wall, because expiry is
//! otherwise untestable without sleeping (ARCHITECTURE §5.6).

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::storage::{ContextEntry, ContextStore};

use super::fs_utils::{ensure_dir, read_json, write_json};
use super::index::{IndexManager, ItemFormat, replace_at_end};
use super::paths::Paths;

/// Max TTL: 30 days.
pub const MAX_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;

pub struct FileContextStore {
    paths: Paths,
    index: IndexManager<ContextEntry>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FileContextStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileContextStore")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl FileContextStore {
    pub fn new(paths: Paths, clock: Arc<dyn Clock>) -> Self {
        let index = IndexManager::new(paths.context_dir(), ItemFormat::Json);
        FileContextStore {
            paths,
            index,
            clock,
        }
    }

    fn is_expired(&self, entry: &ContextEntry) -> bool {
        let Some(expires_at) = &entry.expires_at else {
            return false;
        };
        // `new Date(expires_at).getTime() < Date.now()`. Comparing the ISO
        // strings is equivalent for the fixed-width UTC form the clock writes,
        // and avoids a date parser on a hot path.
        expires_at.as_str() < self.clock.now_iso().as_str()
    }

    /// Delete only the file, leaving the index to the caller. Used by the
    /// batched expiry purge, which rewrites the index once.
    async fn delete_file(&self, key: &str) -> Result<()> {
        let path = self.paths.context_path(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::io(format!("delete {}", path.display()), &path, err)),
        }
    }
}

#[async_trait]
impl ContextStore for FileContextStore {
    async fn get(&self, key: &str) -> Result<Option<ContextEntry>> {
        let Some(entry) = read_json::<ContextEntry>(&self.paths.context_path(key)?).await? else {
            return Ok(None);
        };
        if self.is_expired(&entry) {
            self.delete(key).await?;
            return Ok(None);
        }
        Ok(Some(entry))
    }

    async fn set(&self, key: &str, value: &str, ttl_ms: Option<u64>) -> Result<()> {
        if let Some(ttl) = ttl_ms
            && (ttl == 0 || ttl > MAX_TTL_MS)
        {
            // A plain `Error` in the original, so exit 1 rather than 2.
            return Err(Error::other(format!(
                "TTL must be a positive number up to {MAX_TTL_MS}ms (30 days)"
            )));
        }

        ensure_dir(&self.paths.context_dir()).await?;
        let path = self.paths.context_path(key)?;
        let now = self.clock.now_iso();
        let existing = read_json::<ContextEntry>(&path).await?;

        let entry = ContextEntry {
            key: key.to_owned(),
            value: value.to_owned(),
            // `created_at` survives an overwrite; `updated_at` is refreshed.
            created_at: existing
                .map(|e| e.created_at)
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
            ttl_ms,
            expires_at: ttl_ms
                .map(|ttl| orch_core::clock::iso_from_epoch_ms(self.clock.now_ms() + ttl as i64)),
        };

        write_json(&path, &entry).await?;
        self.index
            .update_index(move |items| replace_at_end(items, entry, |e| &e.key))
            .await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.delete_file(key).await?;
        let key = key.to_owned();
        self.index
            .update_index(move |items| items.into_iter().filter(|e| e.key != key).collect())
            .await
    }

    /// Sorted by key ascending, with every expired entry purged first.
    async fn list(&self) -> Result<Vec<ContextEntry>> {
        let entries = self.index.read_index().await?;

        let (expired, mut valid): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|entry| self.is_expired(entry));

        if !expired.is_empty() {
            for entry in &expired {
                self.delete_file(&entry.key).await?;
            }
            // One rewrite for the whole batch, not one per entry.
            let keep = valid.clone();
            self.index.write_index(&keep).await?;
        }

        // `localeCompare` on the fixed key alphabet (`^[A-Za-z0-9._-]+$`,
        // enforced by sanitize_id) agrees with a byte comparison.
        valid.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(valid)
    }

    async fn get_all(&self) -> Result<IndexMap<String, String>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{clear_ensured_dirs, list_files, path_exists};
    use orch_core::test_support::FixedClock;
    use std::path::Path;

    fn store(root: &Path) -> (FileContextStore, Arc<FixedClock>) {
        clear_ensured_dirs();
        let clock = Arc::new(FixedClock::at(1_760_000_000_000));
        let store = FileContextStore::new(Paths::new(root), Arc::clone(&clock) as Arc<dyn Clock>);
        (store, clock)
    }

    #[tokio::test]
    async fn sets_gets_and_deletes_an_entry() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());

        assert!(store.get("mykey").await.unwrap().is_none());
        store.set("mykey", "myvalue", None).await.unwrap();

        let entry = store.get("mykey").await.unwrap().unwrap();
        assert_eq!(entry.key, "mykey");
        assert_eq!(entry.value, "myvalue");
        assert_eq!(entry.created_at, entry.updated_at);
        assert_eq!(entry.ttl_ms, None);
        assert_eq!(entry.expires_at, None);

        store.delete("mykey").await.unwrap();
        assert!(store.get("mykey").await.unwrap().is_none());
        // Deleting a missing key is a no-op.
        store.delete("mykey").await.unwrap();
    }

    #[tokio::test]
    async fn an_entry_without_a_ttl_omits_both_ttl_fields_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        store.set("mykey", "myvalue", None).await.unwrap();

        // PARITY.md §4.2 ContextEntry.
        let raw = tokio::fs::read_to_string(store.paths.context_path("mykey").unwrap())
            .await
            .unwrap();
        assert!(!raw.contains("ttl_ms"), "{raw}");
        assert!(!raw.contains("expires_at"), "{raw}");
        assert!(raw.ends_with("}\n"));
    }

    #[tokio::test]
    async fn created_at_survives_an_overwrite_and_updated_at_moves() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        store.set("k", "v1", None).await.unwrap();
        let first = store.get("k").await.unwrap().unwrap();

        clock.advance_ms(5_000);
        store.set("k", "v2", None).await.unwrap();
        let second = store.get("k").await.unwrap().unwrap();

        assert_eq!(second.value, "v2");
        assert_eq!(second.created_at, first.created_at);
        assert_ne!(second.updated_at, first.updated_at);
    }

    #[tokio::test]
    async fn a_ttl_sets_expires_at_to_now_plus_ttl() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        store.set("k", "v", Some(60_000)).await.unwrap();

        let entry = store.get("k").await.unwrap().unwrap();
        assert_eq!(entry.ttl_ms, Some(60_000));
        assert_eq!(
            entry.expires_at.as_deref(),
            Some(orch_core::clock::iso_from_epoch_ms(clock.now_ms() + 60_000).as_str())
        );
    }

    #[tokio::test]
    async fn get_lazily_deletes_an_expired_entry() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        store.set("k", "v", Some(1_000)).await.unwrap();
        assert!(store.get("k").await.unwrap().is_some());

        clock.advance_ms(1_001);
        assert!(store.get("k").await.unwrap().is_none());
        // And the file is gone, not merely hidden.
        assert!(!path_exists(&store.paths.context_path("k").unwrap()).await);
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_entry_exactly_at_its_expiry_is_still_valid() {
        // `expires_at < now` — strictly less, so the boundary is inclusive.
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        store.set("k", "v", Some(1_000)).await.unwrap();
        clock.advance_ms(1_000);
        assert!(store.get("k").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_purges_every_expired_entry_in_one_pass() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        store.set("keep", "v", None).await.unwrap();
        store.set("gone1", "v", Some(1_000)).await.unwrap();
        store.set("gone2", "v", Some(2_000)).await.unwrap();
        store.set("later", "v", Some(90_000)).await.unwrap();

        clock.advance_ms(3_000);
        let listed = store.list().await.unwrap();
        let keys: Vec<&str> = listed.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["keep", "later"]);

        for key in ["gone1", "gone2"] {
            assert!(
                !path_exists(&store.paths.context_path(key).unwrap()).await,
                "{key}"
            );
        }
        // The index was rewritten once, so a re-read agrees.
        assert_eq!(store.list().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_is_sorted_by_key_ascending() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        for key in ["zeta", "Alpha", "mid", "_under", "0digit"] {
            store.set(key, "v", None).await.unwrap();
        }
        let keys: Vec<String> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, vec!["0digit", "Alpha", "_under", "mid", "zeta"]);
    }

    #[tokio::test]
    async fn get_all_projects_list_in_the_same_order() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        store.set("b", "2", None).await.unwrap();
        store.set("a", "1", None).await.unwrap();

        let all = store.get_all().await.unwrap();
        assert_eq!(all.keys().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(all["a"], "1");
    }

    #[tokio::test]
    async fn an_out_of_range_ttl_is_rejected_with_the_contract_message() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());

        for ttl in [0, MAX_TTL_MS + 1, u64::MAX] {
            let err = store.set("k", "v", Some(ttl)).await.expect_err("rejected");
            assert_eq!(
                err.to_string(),
                "TTL must be a positive number up to 2592000000ms (30 days)"
            );
            // A plain `Error` in the original, so exit 1.
            assert_eq!(err.exit_code(), 1);
        }
        // Nothing was written.
        assert!(store.get("k").await.unwrap().is_none());

        // The bounds themselves are accepted.
        store.set("k", "v", Some(1)).await.unwrap();
        store.set("k", "v", Some(MAX_TTL_MS)).await.unwrap();
    }

    #[tokio::test]
    async fn the_index_excludes_itself_from_a_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        store.set("a", "1", None).await.unwrap();
        store.set("b", "2", None).await.unwrap();
        tokio::fs::remove_file(store.index.index_path())
            .await
            .unwrap();

        // A `.json`-backed index must not pull `_index.json` into the scan.
        assert_eq!(store.list().await.unwrap().len(), 2);
        let mut files = list_files(&store.paths.context_dir(), Some(".json"))
            .await
            .unwrap();
        files.sort();
        assert_eq!(files, vec!["_index.json", "a.json", "b.json"]);
    }

    #[tokio::test]
    async fn an_invalid_key_is_rejected_rather_than_sanitized() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        for key in ["a/b", "../escape", ""] {
            assert!(store.set(key, "v", None).await.is_err(), "{key}");
            assert!(store.get(key).await.is_err(), "{key}");
        }
    }
}
