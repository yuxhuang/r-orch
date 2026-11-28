//! Retry rows of the file reference, written from the port's rules
//! (`orch_core::store::retries`) and not from the test double: one row per
//! task, replaced whole, and read by deadline then task id.

use std::collections::BTreeMap;

use super::text::{RetryText, StoredText};

use orch_core::store::StoreError;
use orch_core::store::retries::{Retry, RetryCursor, RetryPage, check_limit};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Serialize, Deserialize)]
struct Row {
    attempt: u32,
    due_at: i64,
    error: StoredText,
}

/// The retry table of one project, by task id.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Retries {
    #[serde(
        serialize_with = "serialize_rows",
        deserialize_with = "deserialize_rows"
    )]
    rows: std::sync::Arc<BTreeMap<String, Row>>,
}

fn retry(text: &RetryText, task: &str, row: &Row) -> Result<Retry> {
    Ok(Retry {
        task: task.to_owned(),
        attempt: row.attempt,
        due_at: row.due_at,
        error: text.resolve(&row.error).map_err(|error| {
            StoreError::Backend(orch_core::Error::other(format!(
                "retry task {task}: {error}"
            )))
        })?,
    })
}

impl Retries {
    pub(super) fn put(&mut self, text: &RetryText, new: &Retry) -> Result<()> {
        new.validate()?;
        let error = text.encode(&new.error)?;
        std::sync::Arc::make_mut(&mut self.rows).insert(
            new.task.clone(),
            Row {
                attempt: new.attempt,
                due_at: new.due_at,
                error,
            },
        );
        Ok(())
    }

    pub(super) fn delete(&mut self, task: &str) -> bool {
        std::sync::Arc::make_mut(&mut self.rows)
            .remove(task)
            .is_some()
    }

    pub(super) fn get(&self, text: &RetryText, task: &str) -> Result<Option<Retry>> {
        self.rows
            .get_key_value(task)
            .map(|(t, r)| retry(text, t, r))
            .transpose()
    }

    /// Earliest deadline first, ties by task id.
    pub(super) fn due(&self, text: &RetryText, now_ms: i64, limit: usize) -> Result<Vec<Retry>> {
        check_limit(limit)?;
        let mut due: Vec<_> = self
            .rows
            .iter()
            .filter(|(_, r)| r.due_at <= now_ms)
            .collect();
        due.sort_by(|(ta, a), (tb, b)| (a.due_at, ta).cmp(&(b.due_at, tb)));
        due.into_iter()
            .take(limit)
            .map(|(t, r)| retry(text, t, r))
            .collect()
    }

    /// Every row from just after `after`, in the order of [`Self::due`].
    pub(super) fn page(
        &self,
        text: &RetryText,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<RetryPage> {
        check_limit(limit)?;
        if let Some(cursor) = after {
            cursor.validate()?;
        }
        let mut rest: Vec<_> = self
            .rows
            .iter()
            .filter(|(t, r)| {
                after.is_none_or(|c| (r.due_at, t.as_str()) > (c.due_at, c.task.as_str()))
            })
            .collect();
        rest.sort_by(|(ta, a), (tb, b)| (a.due_at, ta).cmp(&(b.due_at, tb)));
        let more = rest.len() > limit;
        let rows: Vec<Retry> = rest
            .into_iter()
            .take(limit)
            .map(|(t, r)| retry(text, t, r))
            .collect::<Result<_>>()?;
        let next = rows.last().filter(|_| more).map(RetryCursor::after);
        Ok(RetryPage { rows, next })
    }
}

fn serialize_rows<S: serde::Serializer>(
    rows: &std::sync::Arc<BTreeMap<String, Row>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    rows.as_ref().serialize(serializer)
}
fn deserialize_rows<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<std::sync::Arc<BTreeMap<String, Row>>, D::Error> {
    Ok(std::sync::Arc::new(BTreeMap::deserialize(deserializer)?))
}

impl Retries {
    pub(super) fn stream_page(
        &self,
        text: &RetryText,
        after: Option<&RetryCursor>,
        limit: usize,
        admission: &orch_core::store::retries::stream::RetryStreamAdmission,
        sink: &mut dyn orch_core::store::retries::stream::RetryTextSink,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> orch_core::store::retries::stream::Result<orch_core::store::retries::stream::RetryTextPage>
    {
        use orch_core::store::retries::stream::*;
        let selected = select_retry_keys(
            self.rows
                .iter()
                .map(|(task, row)| (task.as_str(), row.due_at)),
            after,
            limit,
            admission,
            cancel,
        )?;
        let _plans = admission.reserve(
            selected
                .keys
                .len()
                .checked_mul(std::mem::size_of::<super::text::RetryTextPlan<'_>>())
                .ok_or(RetryStreamError::Resource)?,
        )?;
        let mut plans = Vec::new();
        plans
            .try_reserve_exact(selected.keys.len())
            .map_err(|_| RetryStreamError::Resource)?;
        let mut bytes = 0u64;
        for (task, _) in &selected.keys {
            check_cancel(cancel)?;
            let row = &self.rows[*task];
            if task.is_empty() || row.attempt == 0 || row.due_at < 0 {
                return Err(RetryStreamError::Corrupt);
            }
            let plan = text.preflight(&row.error, admission, cancel)?;
            bytes = bytes
                .checked_add(plan.bytes())
                .ok_or(RetryStreamError::Resource)?;
            plans.push(plan);
        }
        let mut window = admission.buffer(RETRY_STREAM_WINDOW_BYTES)?;
        let _cursor = admission.reserve(selected.keys.last().map_or(0, |(task, _)| task.len()))?;
        for ((task, due), plan) in selected.keys.iter().zip(&plans) {
            check_cancel(cancel)?;
            sink.begin(
                RetryTextRow {
                    task,
                    attempt: self.rows[*task].attempt,
                    due_at: *due,
                    bytes: plan.bytes(),
                },
                admission,
            )?;
            text.stream(plan, window.as_mut_slice(), cancel, &mut |part| {
                check_cancel(cancel)?;
                sink.write(part, admission)
            })?;
        }
        let next = if selected.more {
            selected
                .keys
                .last()
                .map(|(task, due)| {
                    Ok::<_, RetryStreamError>(RetryCursor {
                        task: copy_text(task)?,
                        due_at: *due,
                    })
                })
                .transpose()?
        } else {
            None
        };
        check_cancel(cancel)?;
        Ok(RetryTextPage {
            rows: selected.keys.len(),
            bytes,
            next,
        })
    }
}

#[cfg(test)]
mod stream_preflight_tests {
    use super::super::FileStorage;
    use crate::{
        blob::{BlobError, BlobHash, DiskBlobStore},
        data_dir::confined::ConfinedDir,
    };
    use orch_core::store::blob::{BlobStore, BlobStreamHasher};
    use orch_core::store::retries::Retry;
    use orch_core::store::retries::stream::*;
    use orch_core::store::{Caller, Lane, Principal, Storage, StorageExt};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio_util::sync::CancellationToken;
    struct Probe {
        disk: DiskBlobStore,
        final_hash: BlobHash,
        reads: Arc<AtomicUsize>,
        mutate: Option<std::path::PathBuf>,
        shorten: bool,
    }
    impl BlobStore for Probe {
        type Hash = BlobHash;
        type Error = BlobError;
        fn put(&self, bytes: &[u8]) -> std::result::Result<BlobHash, BlobError> {
            self.disk.put(bytes)
        }
        fn get(&self, hash: &BlobHash) -> std::result::Result<Vec<u8>, BlobError> {
            self.disk.get(hash)
        }
        fn remove(&self, hash: &BlobHash) -> std::result::Result<bool, BlobError> {
            self.disk.remove(hash)
        }
        fn stream_len(&self, hash: &BlobHash) -> Result<u64> {
            let length = self.disk.stream_len(hash)?;
            if *hash == self.final_hash {
                if let Some(path) = &self.mutate {
                    std::fs::write(path, vec![b'y'; if self.shorten { 65_536 } else { 65_537 }])
                        .unwrap();
                } else {
                    return Err(RetryStreamError::Corrupt);
                }
            }
            Ok(length)
        }
        fn read_windowed(
            &self,
            hash: &BlobHash,
            window: &mut [u8],
            cancel: &CancellationToken,
            consume: &mut dyn FnMut(&[u8]) -> Result<()>,
        ) -> Result<()> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.disk.read_windowed(hash, window, cancel, consume)
        }
        fn with_stream_hasher(
            &self,
            work: &mut dyn FnMut(&mut dyn BlobStreamHasher) -> Result<()>,
        ) -> Result<()> {
            self.disk.with_stream_hasher(work)
        }
    }
    struct Count(Arc<AtomicUsize>);
    impl RetryTextSink for Count {
        fn begin(&mut self, _: RetryTextRow<'_>, _: &RetryStreamAdmission) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn write(&mut self, _: &[u8], _: &RetryStreamAdmission) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn file_page_preflight_reads_no_payload_and_stream_rechecks_metadata_and_same_length_substitution()
     {
        for defect in 0..3 {
            let tmp = tempfile::tempdir().unwrap();
            let root = ConfinedDir::open(tmp.path()).unwrap();
            let disk = DiskBlobStore::open(&root).unwrap();
            let first = "x".repeat(65_537);
            let last = "z".repeat(65_537);
            let hash = BlobHash::of(first.as_bytes()).to_string();
            let object = tmp
                .path()
                .join("blobs/sha256")
                .join(&hash[..2])
                .join(&hash[2..4])
                .join(&hash);
            let reads = Arc::new(AtomicUsize::new(0));
            let probe = Probe {
                disk,
                final_hash: BlobHash::of(last.as_bytes()),
                reads: reads.clone(),
                mutate: (defect != 0).then_some(object),
                shorten: defect == 2,
            };
            let mut storage = FileStorage::new(tmp.path().join("reference"));
            storage.retry_blobs = Some(Arc::new(probe));
            let caller = Caller {
                project: "p".into(),
                principal: Principal::Owner {
                    identity: "synthetic".into(),
                },
            };
            storage
                .transaction(caller.clone(), Lane::Entities, move |u| {
                    for (task, text) in [("a", first), ("z", last)] {
                        u.put_retry(&Retry {
                            task: task.into(),
                            attempt: 1,
                            due_at: 1,
                            error: text,
                        })?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
            let snapshot = storage.read(caller).await.unwrap();
            let begins = Arc::new(AtomicUsize::new(0));
            let result = snapshot
                .read_retry_text(
                    None,
                    2,
                    Box::new(Count(begins.clone())),
                    CancellationToken::new(),
                )
                .await;
            assert!(matches!(result, Err(RetryStreamError::Corrupt)));
            if defect == 0 {
                assert_eq!(reads.load(Ordering::SeqCst), 0);
                assert_eq!(begins.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(reads.load(Ordering::SeqCst), 1);
                assert_eq!(begins.load(Ordering::SeqCst), 1);
            }
        }
    }
}
