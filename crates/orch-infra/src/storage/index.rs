//! Generic `_index.json` manager — ported from
//! `src/infrastructure/storage/index-manager.ts`.
//!
//! Each store keeps individual entity files plus an `_index.json` cache of the
//! **complete** entity objects (not a summary) so `list()` is one read.
//!
//! Two properties are contract, not implementation detail:
//!
//! - `save` is remove-by-id then **push to the end**, so index order is
//!   last-write order. That is observable through `findBestAgent`'s tie
//!   breaking and r-orch replicates it (PARITY.md §6.10, §14.4).
//! - A missing, non-array or corrupt index triggers a **rebuild** from the
//!   individual files rather than an error.

use std::path::PathBuf;
use std::sync::Arc;

use orch_core::error::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use super::fs_utils::{ensure_dir, list_files, read_json, read_yaml, write_json};

/// How an index reads one entity file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemFormat {
    Yaml,
    Json,
}

impl ItemFormat {
    fn ext(self) -> &'static str {
        match self {
            ItemFormat::Yaml => ".yml",
            ItemFormat::Json => ".json",
        }
    }
}

/// Serializes all index read-modify-write cycles within the process, replacing
/// the TS promise-chain mutex.
///
/// `tokio::sync::Mutex` rather than `std::sync::Mutex` because the critical
/// section awaits file I/O — holding it across the await is the point
/// (ARCHITECTURE §6.1).
pub struct IndexManager<T> {
    dir: PathBuf,
    format: ItemFormat,
    mutex: Arc<Mutex<()>>,
    _item: std::marker::PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for IndexManager<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexManager")
            .field("dir", &self.dir)
            .field("format", &self.format)
            .finish()
    }
}

impl<T> IndexManager<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    pub fn new(dir: PathBuf, format: ItemFormat) -> Self {
        IndexManager {
            dir,
            format,
            mutex: Arc::new(Mutex::new(())),
            _item: std::marker::PhantomData,
        }
    }

    pub fn index_path(&self) -> PathBuf {
        self.dir.join("_index.json")
    }

    /// Read the index, rebuilding from the individual files when it is
    /// missing, not an array, or corrupt.
    pub async fn read_index(&self) -> Result<Vec<T>> {
        if let Ok(Some(items)) = read_json::<Vec<T>>(&self.index_path()).await {
            return Ok(items);
        }
        self.rebuild_index().await
    }

    /// Rebuild from the individual entity files and write the result back.
    ///
    /// Order comes from directory enumeration. Entity filenames embed random
    /// nanoids, so two trees can enumerate the same logical set differently —
    /// the one tie-order case that stays unverifiable, reachable only when the
    /// index is missing **and** ≥2 entities exist (PARITY.md §14.4).
    pub async fn rebuild_index(&self) -> Result<Vec<T>> {
        let items = self.scan_items().await?;
        let _guard = self.mutex.lock().await;
        self.write_index_unlocked(&items).await?;
        Ok(items)
    }

    async fn scan_items(&self) -> Result<Vec<T>> {
        ensure_dir(&self.dir).await?;
        let ext = self.format.ext();
        let mut names = list_files(&self.dir, Some(ext)).await?;
        // `.json`-backed stores must not pull `_index.json` into the rebuild.
        names.retain(|name| name != "_index.json");

        let mut items = Vec::with_capacity(names.len());
        for name in names {
            let path = self.dir.join(&name);
            let item = match self.format {
                ItemFormat::Yaml => read_yaml::<T>(&path).await,
                ItemFormat::Json => read_json::<T>(&path).await,
            };
            // Only a **missing** file is skipped. A parse error propagates.
            //
            // This used to be `if let Ok(Some(item))`, which swallowed `Err`
            // as well — so one corrupt entity file produced a *shortened*
            // index that `rebuild_index`'s caller then persisted, silently
            // dropping every entity in that file from `list()` with nothing
            // logged. The comment claimed the TS "drops nulls the same way",
            // which is true of nulls and untrue of errors: `readYaml` returns
            // `null` **only** on ENOENT and rethrows everything else, and
            // `rebuildIndex`'s `Promise.all` then rejects the whole rebuild.
            // Propagating is both safer and the faithful behaviour.
            match item? {
                Some(item) => items.push(item),
                None => continue,
            }
        }
        Ok(items)
    }

    /// Replace the index wholesale, serialized against every other index
    /// mutation.
    ///
    /// Used by the context store's lazy expiry purge, which deletes a batch of
    /// files and then rewrites the index **once** rather than per entry.
    pub async fn write_index(&self, items: &[T]) -> Result<()> {
        let _guard = self.mutex.lock().await;
        self.write_index_unlocked(items).await
    }

    /// Apply a mutation to the index and write it back, serialized against
    /// every other index mutation in this process.
    pub async fn update_index(&self, mutate: impl FnOnce(Vec<T>) -> Vec<T> + Send) -> Result<()> {
        // The read must happen under the lock, or two concurrent `task add`
        // calls lose one of the two writes.
        let _guard = self.mutex.lock().await;
        let current = match read_json::<Vec<T>>(&self.index_path()).await {
            Ok(Some(items)) => items,
            // Rebuild inline — taking the lock again would deadlock.
            _ => self.scan_items().await?,
        };
        let updated = mutate(current);
        self.write_index_unlocked(&updated).await
    }

    async fn write_index_unlocked(&self, items: &[T]) -> Result<()> {
        ensure_dir(&self.dir).await?;
        write_json(&self.index_path(), &items).await
    }
}

/// `save` semantics: drop any entry with this id, then append.
pub fn replace_at_end<T>(mut items: Vec<T>, entity: T, id_of: impl Fn(&T) -> &str) -> Vec<T> {
    let id = id_of(&entity).to_owned();
    items.retain(|item| id_of(item) != id);
    items.push(entity);
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{atomic_write, clear_ensured_dirs, write_yaml};
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Item {
        id: String,
        value: u32,
    }

    fn item(id: &str, value: u32) -> Item {
        Item {
            id: id.into(),
            value,
        }
    }

    fn manager(dir: &std::path::Path) -> IndexManager<Item> {
        clear_ensured_dirs();
        IndexManager::new(dir.to_path_buf(), ItemFormat::Yaml)
    }

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[tokio::test]
    async fn an_empty_directory_yields_an_empty_index_and_writes_it() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        assert!(index.read_index().await.unwrap().is_empty());
        // The rebuild wrote the file back, as a fresh `task list` does.
        assert_eq!(
            tokio::fs::read_to_string(index.index_path()).await.unwrap(),
            "[]\n"
        );
    }

    #[tokio::test]
    async fn a_missing_index_is_rebuilt_from_the_entity_files() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        write_yaml(&temp.path().join("a.yml"), &item("a", 1))
            .await
            .unwrap();
        write_yaml(&temp.path().join("b.yml"), &item("b", 2))
            .await
            .unwrap();

        let mut rebuilt = index.read_index().await.unwrap();
        rebuilt.sort_by(|x, y| x.id.cmp(&y.id));
        assert_eq!(rebuilt, vec![item("a", 1), item("b", 2)]);
        // And it is now cached on disk.
        assert!(
            read_json::<Vec<Item>>(&index.index_path())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_corrupt_index_is_rebuilt_rather_than_failing() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        write_yaml(&temp.path().join("a.yml"), &item("a", 1))
            .await
            .unwrap();
        atomic_write(&index.index_path(), "{not an array")
            .await
            .unwrap();

        assert_eq!(index.read_index().await.unwrap(), vec![item("a", 1)]);
    }

    #[tokio::test]
    async fn a_non_array_index_is_rebuilt() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        write_yaml(&temp.path().join("a.yml"), &item("a", 1))
            .await
            .unwrap();
        atomic_write(&index.index_path(), "{\"not\": \"an array\"}\n")
            .await
            .unwrap();

        assert_eq!(index.read_index().await.unwrap(), vec![item("a", 1)]);
    }

    #[tokio::test]
    async fn the_index_holds_complete_entities_not_summaries() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        index
            .update_index(|items| replace_at_end(items, item("a", 42), |i| &i.id))
            .await
            .unwrap();

        let raw = tokio::fs::read_to_string(index.index_path()).await.unwrap();
        assert_eq!(
            raw,
            "[\n  {\n    \"id\": \"a\",\n    \"value\": 42\n  }\n]\n"
        );
    }

    #[tokio::test]
    async fn save_pushes_to_the_end_so_index_order_is_last_write_order() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        for id in ["a", "b", "c"] {
            index
                .update_index(|items| replace_at_end(items, item(id, 0), |i| &i.id))
                .await
                .unwrap();
        }
        assert_eq!(ids(&index.read_index().await.unwrap()), vec!["a", "b", "c"]);

        // Re-saving `a` moves it to the end.
        index
            .update_index(|items| replace_at_end(items, item("a", 9), |i| &i.id))
            .await
            .unwrap();
        let after = index.read_index().await.unwrap();
        assert_eq!(ids(&after), vec!["b", "c", "a"]);
        assert_eq!(after.last().unwrap().value, 9);
        assert_eq!(after.len(), 3);
    }

    #[tokio::test]
    async fn concurrent_updates_do_not_lose_a_write() {
        let temp = tempfile::tempdir().unwrap();
        let index = Arc::new(manager(temp.path()));

        let mut handles = Vec::new();
        for n in 0..16u32 {
            let index = Arc::clone(&index);
            handles.push(tokio::spawn(async move {
                let id = format!("item-{n}");
                index
                    .update_index(move |items| replace_at_end(items, item(&id, n), |i| &i.id))
                    .await
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(index.read_index().await.unwrap().len(), 16);
    }

    #[tokio::test]
    async fn an_unreadable_entity_file_fails_the_rebuild_rather_than_shortening_it() {
        // Inverted deliberately (sre B1). Skipping the bad file produced an
        // index missing that entity, and `rebuild_index`'s callers *persist*
        // what it returns — so a single corrupt file silently removed an
        // entity from every `list()` thereafter, with nothing logged. The TS
        // propagates here: `readYaml` returns null only on ENOENT and
        // rethrows a parse error, which rejects `rebuildIndex`'s `Promise.all`.
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        write_yaml(&temp.path().join("a.yml"), &item("a", 1))
            .await
            .unwrap();
        atomic_write(&temp.path().join("bad.yml"), "this: is: not: an: item\n")
            .await
            .unwrap();

        let err = index
            .rebuild_index()
            .await
            .expect_err("a corrupt entity file must not be silently dropped");
        assert!(
            format!("{err}").contains("bad.yml"),
            "the error must name the file: {err}"
        );
    }

    #[tokio::test]
    async fn a_missing_entity_file_is_still_skipped() {
        // The other half of B1: `Ok(None)` — a file listed but gone by the
        // time it is read — stays non-fatal, which is what the TS `null`
        // branch does.
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        write_yaml(&temp.path().join("a.yml"), &item("a", 1))
            .await
            .unwrap();
        assert_eq!(index.rebuild_index().await.unwrap(), vec![item("a", 1)]);
    }

    #[tokio::test]
    async fn a_json_backed_index_excludes_itself_from_the_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let index: IndexManager<Item> =
            IndexManager::new(temp.path().to_path_buf(), ItemFormat::Json);
        write_json(&temp.path().join("a.json"), &item("a", 1))
            .await
            .unwrap();
        write_json(&index.index_path(), &vec![item("stale", 0)])
            .await
            .unwrap();

        assert_eq!(index.rebuild_index().await.unwrap(), vec![item("a", 1)]);
    }

    #[tokio::test]
    async fn write_index_replaces_the_whole_index() {
        let temp = tempfile::tempdir().unwrap();
        let index = manager(temp.path());
        index
            .update_index(|items| replace_at_end(items, item("a", 1), |i| &i.id))
            .await
            .unwrap();
        index
            .update_index(|items| replace_at_end(items, item("b", 2), |i| &i.id))
            .await
            .unwrap();

        index.write_index(&[item("c", 3)]).await.unwrap();
        assert_eq!(index.read_index().await.unwrap(), vec![item("c", 3)]);

        index.write_index(&[]).await.unwrap();
        assert!(index.read_index().await.unwrap().is_empty());
    }

    #[test]
    fn replace_at_end_is_an_upsert() {
        let items = vec![item("a", 1), item("b", 2)];
        let after = replace_at_end(items, item("a", 9), |i| &i.id);
        assert_eq!(ids(&after), vec!["b", "a"]);

        let after = replace_at_end(after, item("c", 3), |i| &i.id);
        assert_eq!(ids(&after), vec!["b", "a", "c"]);
    }
}
