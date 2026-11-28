//! Synthetic public-port bounded diagnostic recovery; no production caller.
use orch_core::store::retries::stream::*;
use orch_core::store::retries::{Retry, RetryCursor};
use orch_core::store::{Caller, Lane, Principal, ReadSnapshot, Storage, StorageExt};
use orch_infra::{
    blob::{BlobHash, DiskBlobStore, MAX_BLOB_BYTES},
    data_dir::{DataDir, Holder, LockKind, confined::ConfinedDir},
    sqlite::{SqliteStorage, StorageOptions},
    storage::v2::FileStorage,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn caller() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
struct Fixture {
    _tmp: tempfile::TempDir,
    storage: Arc<dyn Storage>,
    sqlite: Option<SqliteStorage>,
    objects: std::path::PathBuf,
}
impl Fixture {
    fn new(kind: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mut sqlite = None;
        let objects;
        let storage: Arc<dyn Storage> = match kind {
            "memory" => {
                objects = tmp.path().join("unused");
                Arc::new(orch_core::store::fake::FakeStorage::default())
            }
            "backed" => {
                objects = tmp.path().join("backed");
                std::fs::create_dir(&objects).unwrap();
                let disk = DiskBlobStore::open(&ConfinedDir::open(&objects).unwrap()).unwrap();
                Arc::new(orch_core::store::fake::FakeStorage::with_retry_blob_store(
                    disk,
                    BlobHash::of,
                    MAX_BLOB_BYTES as usize,
                ))
            }
            "file" => {
                objects = tmp.path().join("file/p-70");
                Arc::new(FileStorage::new(tmp.path().join("file")))
            }
            "sqlite" => {
                objects = tmp.path().join("data");
                let data = DataDir::acquire(
                    &objects,
                    &Holder {
                        kind: LockKind::Daemon,
                        pid: std::process::id(),
                        started_at: "synthetic".into(),
                        binary_version: "test".into(),
                        executable: "/synthetic/orch".into(),
                    },
                )
                .unwrap();
                let adapter =
                    SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
                sqlite = Some(adapter.clone());
                Arc::new(adapter)
            }
            _ => panic!("unknown fixture"),
        };
        Self {
            _tmp: tmp,
            storage,
            sqlite,
            objects,
        }
    }
    async fn put(&self, task: &str, due: i64, error: String) {
        let row = Retry {
            task: task.into(),
            attempt: 1,
            due_at: due,
            error,
        };
        self.storage
            .transaction(caller(), Lane::Entities, move |u| u.put_retry(&row))
            .await
            .unwrap();
    }
    async fn snapshot(&self) -> Box<dyn ReadSnapshot> {
        self.storage.read(caller()).await.unwrap()
    }
    async fn close(&self) {
        if let Some(sqlite) = &self.sqlite {
            sqlite.close().await.unwrap();
        }
    }
    fn object(&self, bytes: &[u8]) -> std::path::PathBuf {
        let hash = BlobHash::of(bytes).to_string();
        self.objects
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(hash)
    }
}
#[derive(Default)]
struct Seen {
    rows: usize,
    bytes: usize,
    calls: usize,
    declared: u64,
    max: usize,
}
struct Sink {
    seen: Arc<Mutex<Seen>>,
    expected: Arc<Vec<u8>>,
}
impl RetryTextSink for Sink {
    fn begin(&mut self, row: RetryTextRow<'_>, _: &RetryStreamAdmission) -> Result<()> {
        let mut seen = self.seen.lock().unwrap();
        seen.rows += 1;
        seen.declared += row.bytes;
        Ok(())
    }
    fn write(&mut self, bytes: &[u8], _: &RetryStreamAdmission) -> Result<()> {
        let mut seen = self.seen.lock().unwrap();
        assert!(bytes.len() <= RETRY_STREAM_WINDOW_BYTES);
        assert_eq!(bytes, &self.expected[seen.bytes..seen.bytes + bytes.len()]);
        seen.bytes += bytes.len();
        seen.calls += 1;
        seen.max = seen.max.max(bytes.len());
        Ok(())
    }
}
fn sink(expected: Vec<u8>) -> (Box<dyn RetryTextSink>, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    (
        Box::new(Sink {
            seen: seen.clone(),
            expected: Arc::new(expected),
        }),
        seen,
    )
}
#[tokio::test]
async fn all_adapters_stream_inline_single_objects_order_and_committed_snapshot() {
    for kind in ["memory", "backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        let text = "é🙂".repeat(12_000);
        rig.put("b", 1, "tail".into()).await;
        rig.put("a", 1, text.clone()).await;
        let snapshot = rig.snapshot().await;
        rig.put("a", 9, "replacement".into()).await;
        let (out, seen) = sink(text.as_bytes().to_vec());
        let page = snapshot
            .read_retry_text(None, 1, out, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!((page.rows, page.bytes), (1, 72_000), "{kind}");
        assert_eq!(
            page.next,
            Some(RetryCursor {
                task: "a".into(),
                due_at: 1
            })
        );
        assert_eq!(seen.lock().unwrap().bytes, 72_000);
        let (out, seen) = sink(b"tail".to_vec());
        let last = snapshot
            .read_retry_text(page.next.as_ref(), 1, out, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(last.bytes, 4);
        assert!(last.next.is_none());
        assert_eq!(seen.lock().unwrap().rows, 1);
        let (out, _) = sink(Vec::new());
        let empty = snapshot
            .read_retry_text(
                Some(&RetryCursor {
                    task: "z".into(),
                    due_at: 99,
                }),
                1,
                out,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!((empty.rows, empty.bytes, empty.next), (0, 0, None));
        let (out, _) = sink(Vec::new());
        assert!(matches!(
            snapshot
                .read_retry_text(None, 0, out, CancellationToken::new())
                .await,
            Err(RetryStreamError::Store(_))
        ));
        drop(snapshot);
        rig.close().await;
    }
}
#[tokio::test]
async fn selected_missing_or_corrupt_objects_fail_but_unselected_damage_does_not() {
    for kind in ["backed", "file", "sqlite"] {
        for missing in [false, true] {
            let rig = Fixture::new(kind);
            let text = "x".repeat(65_537);
            rig.put("a", 1, "good".into()).await;
            rig.put("z", 2, text.clone()).await;
            let path = rig.object(text.as_bytes());
            if missing {
                std::fs::remove_file(path).unwrap();
            } else {
                std::fs::write(path, "y".repeat(65_537)).unwrap();
            }
            let snapshot = rig.snapshot().await;
            let (out, _) = sink(b"good".to_vec());
            let page = snapshot
                .read_retry_text(None, 1, out, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(page.next.as_ref().unwrap().task, "a");
            // Corrupt bytes are provisional; this sink discards them.
            let error = snapshot
                .read_retry_text(
                    page.next.as_ref(),
                    1,
                    Box::new(Discard),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, RetryStreamError::Corrupt),
                "{kind}: {error}"
            );
            drop(snapshot);
            rig.close().await;
        }
    }
}
struct Discard;
impl RetryTextSink for Discard {
    fn begin(&mut self, _: RetryTextRow<'_>, _: &RetryStreamAdmission) -> Result<()> {
        Ok(())
    }
    fn write(&mut self, _: &[u8], _: &RetryStreamAdmission) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn all_adapters_bound_shared_admission_and_propagate_sink_failure() {
    struct Exhaust;
    impl RetryTextSink for Exhaust {
        fn begin(&mut self, _: RetryTextRow<'_>, admission: &RetryStreamAdmission) -> Result<()> {
            let _permit = admission.reserve(usize::MAX)?;
            Ok(())
        }
        fn write(&mut self, _: &[u8], _: &RetryStreamAdmission) -> Result<()> {
            panic!("resource error must stop callbacks")
        }
    }
    for kind in ["memory", "backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        rig.put("a", 1, "x".repeat(65_537)).await;
        let snapshot = rig.snapshot().await;
        assert!(matches!(
            snapshot
                .read_retry_text(None, 1, Box::new(Exhaust), CancellationToken::new())
                .await,
            Err(RetryStreamError::Resource)
        ));
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            snapshot
                .read_retry_text(None, 1, Box::new(Exhaust), token)
                .await,
            Err(RetryStreamError::Cancelled)
        ));
        let (out, seen) = sink(vec![b'x'; 65_537]);
        snapshot
            .read_retry_text(None, 1, out, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().bytes, 65_537);
        drop(snapshot);
        rig.close().await;
    }
}
#[tokio::test]
async fn backed_adapters_stream_every_repeated_object_and_split_utf8_boundary() {
    for kind in ["backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        let mut repeated = "x".repeat(2 * MAX_BLOB_BYTES as usize);
        repeated.push_str("é🙂");
        // Two equal full objects plus a tail: every occurrence is counted.
        let expected = repeated.as_bytes().to_vec();
        rig.put("a", 1, repeated).await;
        let snapshot = rig.snapshot().await;
        let (out, seen) = sink(expected);
        let page = snapshot
            .read_retry_text(None, 1, out, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.bytes, 2 * MAX_BLOB_BYTES + 6);
        assert_eq!(seen.lock().unwrap().max, 65_536);
        drop(snapshot);
        let mut split = "x".repeat(MAX_BLOB_BYTES as usize - 1);
        split.push_str("🙂tail");
        let expected = split.as_bytes().to_vec();
        rig.put("a", 1, split).await;
        let snapshot = rig.snapshot().await;
        let (out, seen) = sink(expected);
        let page = snapshot
            .read_retry_text(None, 1, out, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.bytes, MAX_BLOB_BYTES + 7);
        assert_eq!(seen.lock().unwrap().bytes as u64, page.bytes);
        drop(snapshot);
        rig.close().await;
    }
}

#[tokio::test]
async fn all_adapters_return_resource_for_injected_buffer_allocation_and_release_charges() {
    struct Capture(Arc<Mutex<Option<RetryStreamAdmission>>>);
    impl RetryTextSink for Capture {
        fn begin(&mut self, _: RetryTextRow<'_>, admission: &RetryStreamAdmission) -> Result<()> {
            *self.0.lock().unwrap() = Some(admission.clone());
            Ok(())
        }
        fn write(&mut self, _: &[u8], _: &RetryStreamAdmission) -> Result<()> {
            Ok(())
        }
    }
    for kind in ["memory", "backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        rig.put("a", 1, "x".repeat(65_537)).await;
        let snapshot = rig.snapshot().await;
        let captured = Arc::new(Mutex::new(None));
        snapshot
            .read_retry_text(
                None,
                1,
                Box::new(Capture(captured.clone())),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let admission = captured.lock().unwrap().take().unwrap();
        assert_eq!(admission.used_bytes(), 0, "{kind}");
        admission.fail_next_buffer_allocation();
        let (out, seen) = sink(vec![b'x'; 65_537]);
        let error = snapshot
            .read_retry_text(None, 1, out, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(error, RetryStreamError::Resource),
            "{kind}: {error}"
        );
        assert_eq!(seen.lock().unwrap().rows, 0);
        assert_eq!(admission.used_bytes(), 0);
        let (out, _) = sink(vec![b'x'; 65_537]);
        snapshot
            .read_retry_text(None, 1, out, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(admission.used_bytes(), 0);
        drop(snapshot);
        rig.close().await;
    }
}

#[tokio::test]
async fn all_adapters_hold_actual_worker_memory_after_future_drop_and_cancel_without_success() {
    struct Block {
        entered: Option<tokio::sync::oneshot::Sender<RetryStreamAdmission>>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        permit: Option<RetryStreamPermit>,
        callbacks: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl RetryTextSink for Block {
        fn begin(&mut self, _: RetryTextRow<'_>, _: &RetryStreamAdmission) -> Result<()> {
            Ok(())
        }
        fn write(&mut self, _: &[u8], admission: &RetryStreamAdmission) -> Result<()> {
            self.callbacks
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Saturate the adapter's actual counter while the blocking callback
            // remains live. Cancellation of the waiter must not release it.
            self.permit = Some(admission.reserve(64 * 1024 * 1024 - admission.used_bytes())?);
            self.entered.take().unwrap().send(admission.clone()).ok();
            let (lock, condition) = &*self.release;
            let ready = lock.lock().unwrap();
            let (_held, timeout) = condition
                .wait_timeout_while(ready, std::time::Duration::from_secs(10), |ready| !*ready)
                .unwrap();
            assert!(!timeout.timed_out(), "fixture release deadline");
            Ok(())
        }
    }
    for kind in ["memory", "backed", "file", "sqlite"] {
        for drop_waiter in [false, true] {
            let rig = Fixture::new(kind);
            rig.put("a", 1, "x".repeat(131_073)).await;
            let snapshot: Arc<dyn ReadSnapshot> = Arc::from(rig.snapshot().await);
            let worker = snapshot.clone();
            let token = CancellationToken::new();
            let cancellation = token.clone();
            let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
            let released = release.clone();
            let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let calls = callbacks.clone();
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let mut task = Some(tokio::spawn(async move {
                worker
                    .read_retry_text(
                        None,
                        1,
                        Box::new(Block {
                            entered: Some(entered),
                            release: released,
                            permit: None,
                            callbacks: calls,
                        }),
                        cancellation,
                    )
                    .await
            }));
            let admission = tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
                .await
                .unwrap()
                .unwrap();
            if drop_waiter {
                let waiter = task.take().unwrap();
                waiter.abort();
                // abort requests cancellation; awaiting the join proves the
                // future and its child-token drop guard have actually dropped.
                assert!(waiter.await.unwrap_err().is_cancelled());
                assert!(!token.is_cancelled());
            } else {
                token.cancel();
            }
            assert_eq!(admission.used_bytes(), 64 * 1024 * 1024, "{kind}");
            let sibling = rig.snapshot().await;
            assert!(matches!(
                sibling
                    .read_retry_text(None, 1, Box::new(Discard), CancellationToken::new())
                    .await,
                Err(RetryStreamError::Resource)
            ));
            {
                let (lock, condition) = &*release;
                *lock.lock().unwrap() = true;
                condition.notify_all();
            }
            if let Some(waiter) = task {
                assert!(matches!(
                    waiter.await.unwrap(),
                    Err(RetryStreamError::Cancelled)
                ));
            }
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while admission.used_bytes() != 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(callbacks.load(std::sync::atomic::Ordering::SeqCst), 1);
            let (out, seen) = sink(vec![b'x'; 131_073]);
            snapshot
                .read_retry_text(None, 1, out, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(seen.lock().unwrap().bytes, 131_073);
            assert_eq!(admission.used_bytes(), 0);
            drop(sibling);
            drop(snapshot);
            rig.close().await;
        }
    }
}

#[tokio::test]
async fn all_adapters_bound_live_work_across_a_page() {
    struct Track {
        high: Arc<std::sync::atomic::AtomicUsize>,
        bytes: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl RetryTextSink for Track {
        fn begin(&mut self, _: RetryTextRow<'_>, admission: &RetryStreamAdmission) -> Result<()> {
            self.high
                .fetch_max(admission.used_bytes(), std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn write(&mut self, part: &[u8], admission: &RetryStreamAdmission) -> Result<()> {
            self.high
                .fetch_max(admission.used_bytes(), std::sync::atomic::Ordering::SeqCst);
            self.bytes
                .fetch_add(part.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }
    for kind in ["memory", "backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        for n in 0..5 {
            rig.put(&format!("task-{n}"), n, "x".repeat(2 * 1024 * 1024))
                .await;
        }
        let snapshot = rig.snapshot().await;
        let high = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let page = snapshot
            .read_retry_text(
                None,
                5,
                Box::new(Track {
                    high: high.clone(),
                    bytes: bytes.clone(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!((page.rows, page.bytes), (5, 10 * 1024 * 1024));
        assert_eq!(
            bytes.load(std::sync::atomic::Ordering::SeqCst),
            10 * 1024 * 1024
        );
        let peak = high.load(std::sync::atomic::Ordering::SeqCst);
        assert!(peak <= 128 * 1024, "{kind}: {peak}");
        eprintln!(
            "synthetic {kind}: observed live charged bytes {peak}, logical page {}",
            page.bytes
        );
        drop(snapshot);
        rig.close().await;
    }
}

#[tokio::test]
async fn sqlite_preflights_selected_json_before_any_callback_and_keeps_lookahead_opaque() {
    let rig = Fixture::new("sqlite");
    rig.put("a", 1, "a".repeat(65_537)).await;
    rig.put("z", 2, "last".into()).await;
    let conn = rusqlite::Connection::open(rig.objects.join("orch.db")).unwrap();
    conn.execute(
        "UPDATE retry_queue SET error='not-json' WHERE task_id='z'",
        [],
    )
    .unwrap();
    drop(conn);
    let snapshot = rig.snapshot().await;
    let (out, seen) = sink(vec![b'a'; 65_537]);
    assert!(matches!(
        snapshot
            .read_retry_text(None, 2, out, CancellationToken::new())
            .await,
        Err(RetryStreamError::Corrupt)
    ));
    assert_eq!(seen.lock().unwrap().rows, 0);
    let (out, _) = sink(vec![b'a'; 65_537]);
    let page = snapshot
        .read_retry_text(None, 1, out, CancellationToken::new())
        .await
        .unwrap();
    assert!(page.next.is_some());
    drop(snapshot);
    rig.close().await;
}

#[tokio::test]
async fn sqlite_stream_preserves_existing_decoder_acceptance_of_optional_null_members() {
    let rig = Fixture::new("sqlite");
    rig.put("a", 1, "a".into()).await;
    let conn = rusqlite::Connection::open(rig.objects.join("orch.db")).unwrap();
    conn.execute(
        "UPDATE retry_queue SET error=?1 WHERE task_id='a'",
        [r#"{"v":1,"inline":"a","blob":null}"#],
    )
    .unwrap();
    drop(conn);
    let snapshot = rig.snapshot().await;
    assert_eq!(snapshot.read_retry("a").await.unwrap().unwrap().error, "a");
    let (out, seen) = sink(b"a".to_vec());
    snapshot
        .read_retry_text(None, 1, out, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().bytes, 1);
    drop(snapshot);
    rig.close().await;
}

#[tokio::test]
async fn valid_objects_still_require_the_whole_hash_and_utf8_to_verify() {
    for defect in 0..3 {
        let kind = if defect == 2 { "file" } else { "sqlite" };
        let rig = Fixture::new(kind);
        rig.put("a", 1, "x".repeat(65_537)).await;
        if kind == "sqlite" {
            let conn = rusqlite::Connection::open(rig.objects.join("orch.db")).unwrap();
            let encoded: String = conn
                .query_row("SELECT error FROM retry_queue WHERE task_id='a'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let mut doc: serde_json::Value = serde_json::from_str(&encoded).unwrap();
            if defect == 0 {
                doc["blob"]["sha256"] = BlobHash::of(b"different whole value").to_string().into();
            } else {
                let disk = DiskBlobStore::open(&ConfinedDir::open(&rig.objects).unwrap()).unwrap();
                let mut invalid = vec![b'x'; 65_537];
                invalid[0] = 0xff;
                let hash = disk.put(&invalid).unwrap().to_string();
                conn.execute("INSERT INTO blobs(project_id,sha256,size,storage_location,created_at) VALUES('p',?1,65537,'disk',0)",[&hash]).unwrap();
                doc["blob"]["sha256"] = hash.clone().into();
                doc["blob"]["chunks"][0]["sha256"] = hash.into();
            }
            conn.execute(
                "UPDATE retry_queue SET error=?1 WHERE task_id='a'",
                [serde_json::to_string(&doc).unwrap()],
            )
            .unwrap();
        } else {
            let disk = DiskBlobStore::open(&ConfinedDir::open(&rig.objects).unwrap()).unwrap();
            let mut invalid = vec![b'x'; 65_537];
            invalid[0] = 0xff;
            let hash = disk.put(&invalid).unwrap().to_string();
            let path = rig.objects.join("state.json");
            let mut doc: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            doc["retries"]["rows"]["a"]["error"]["sha256"] = hash.into();
            std::fs::write(path, serde_json::to_vec(&doc).unwrap()).unwrap();
        }
        let snapshot = rig.snapshot().await;
        assert!(
            matches!(
                snapshot
                    .read_retry_text(None, 1, Box::new(Discard), CancellationToken::new())
                    .await,
                Err(RetryStreamError::Corrupt)
            ),
            "defect {defect}"
        );
        assert!(
            snapshot.read_retry("a").await.is_err(),
            "existing whole reader"
        );
        drop(snapshot);
        rig.close().await;
    }
}

// A synthetic consumer example, not the T12 production implementation. It keeps
// only counters: diagnostic payload is checked as it arrives, never accumulated.
#[derive(Default)]
struct LargeInlineSeen {
    rows: usize,
    row_bytes: usize,
    bytes: u64,
    admission: Option<RetryStreamAdmission>,
}
struct LargeInlineSink {
    first: usize,
    seen: Arc<Mutex<LargeInlineSeen>>,
    refuse: bool,
}
impl RetryTextSink for LargeInlineSink {
    fn begin(&mut self, row: RetryTextRow<'_>, admission: &RetryStreamAdmission) -> Result<()> {
        let mut seen = self.seen.lock().unwrap();
        if seen.rows > 0 {
            assert_eq!(seen.row_bytes, 65_536);
        }
        let index = self.first + seen.rows;
        assert_eq!(row.task, format!("inline-{index:04}"));
        assert_eq!(row.attempt, (index % 7 + 1) as u32);
        assert_eq!(row.due_at, (index / 3) as i64);
        assert_eq!(row.bytes, 65_536);
        assert!(admission.used_bytes() <= 64 * 1024 * 1024);
        seen.admission = Some(admission.clone());
        seen.rows += 1;
        seen.row_bytes = 0;
        Ok(())
    }
    fn write(&mut self, bytes: &[u8], admission: &RetryStreamAdmission) -> Result<()> {
        let mut seen = self.seen.lock().unwrap();
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= RETRY_STREAM_WINDOW_BYTES);
        for (offset, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, [0xc3, 0xa9][(seen.row_bytes + offset) % 2]);
        }
        seen.row_bytes += bytes.len();
        seen.bytes += bytes.len() as u64;
        assert!(admission.used_bytes() <= 64 * 1024 * 1024);
        if self.refuse {
            return Err(RetryStreamError::Resource);
        }
        Ok(())
    }
}

#[derive(Default)]
struct RecoveryTrace {
    limits: Vec<usize>,
    discarded_bytes: u64,
}
// Resource alone reduces the limit; the snapshot and committed cursor never
// change on failure. Every attempt owns fresh provisional state.
async fn recover_inline_page(
    snapshot: &dyn ReadSnapshot,
    after: Option<&RetryCursor>,
    first: usize,
    mut limit: usize,
    mut refused_attempts: usize,
    trace: &mut RecoveryTrace,
) -> Result<RetryTextPage> {
    loop {
        trace.limits.push(limit);
        let seen = Arc::new(Mutex::new(LargeInlineSeen::default()));
        let result = snapshot
            .read_retry_text(
                after,
                limit,
                Box::new(LargeInlineSink {
                    first,
                    seen: seen.clone(),
                    refuse: refused_attempts > 0,
                }),
                CancellationToken::new(),
            )
            .await;
        refused_attempts = refused_attempts.saturating_sub(1);
        let provisional = seen.lock().unwrap();
        if let Some(admission) = &provisional.admission {
            assert_eq!(admission.used_bytes(), 0);
        }
        match result {
            Ok(page) => {
                assert_eq!(page.rows, provisional.rows);
                assert_eq!(page.bytes, provisional.bytes);
                assert_eq!(provisional.row_bytes, 65_536);
                return Ok(page);
            }
            Err(RetryStreamError::Resource) => {
                trace.discarded_bytes += provisional.bytes;
                if limit == 1 {
                    return Err(RetryStreamError::Resource);
                }
                limit = (limit / 2).max(1);
            }
            Err(error) => return Err(error),
        }
        // No provisional counters or payload survive into the next attempt.
    }
}
async fn put_large_inline(rig: &Fixture, count: usize) {
    rig.storage
        .transaction(caller(), Lane::Entities, move |u| {
            for index in 0..count {
                u.put_retry(&Retry {
                    task: format!("inline-{index:04}"),
                    attempt: (index % 7 + 1) as u32,
                    due_at: (index / 3) as i64,
                    error: "é".repeat(32_768),
                })?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn all_adapters_limit_1000_large_inline_bounded_resource_recovery() {
    for kind in ["memory", "backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        // One extra row forces a continuation even on adapters admitting 1000.
        put_large_inline(&rig, 1001).await;
        let snapshot = rig.snapshot().await;
        let mut cursor = None;
        let mut accepted = 0;
        let mut total = 0;
        let mut limit = 1000;
        let mut trace = RecoveryTrace::default();
        loop {
            let page = recover_inline_page(
                snapshot.as_ref(),
                cursor.as_ref(),
                accepted,
                limit,
                0,
                &mut trace,
            )
            .await
            .unwrap();
            limit = *trace.limits.last().unwrap();
            assert!(page.rows > 0);
            accepted += page.rows;
            total += page.bytes;
            assert_eq!(
                page.next,
                (accepted < 1001).then(|| RetryCursor {
                    task: format!("inline-{:04}", accepted - 1),
                    due_at: ((accepted - 1) / 3) as i64,
                })
            );
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!((accepted, total), (1001, 1001 * 65_536));
        assert_eq!(trace.limits[0], 1000);
        // Retaining the reduced limit bounds the complete traversal by at
        // most 1001 nonempty pages plus nine failed reductions. Admission may
        // improve in any adapter without changing this recovery contract.
        assert!(trace.limits.len() <= 1010, "finite recovery and traversal");
        for limits in trace.limits.windows(2) {
            assert!(limits[1] == limits[0] || limits[1] == (limits[0] / 2).max(1));
        }
        eprintln!(
            "{kind}: limit1000 traversal rows={accepted} bytes={total} limits={:?}",
            trace.limits
        );
        drop(snapshot);
        rig.close().await;
    }
}

#[tokio::test]
async fn all_adapters_resource_recovery_discards_provisional_page_and_stops_at_one() {
    for kind in ["memory", "backed", "file", "sqlite"] {
        let rig = Fixture::new(kind);
        put_large_inline(&rig, 9).await;
        let snapshot = rig.snapshot().await;
        let cursor = RetryCursor {
            task: "inline-0000".into(),
            due_at: 0,
        };
        let mut trace = RecoveryTrace::default();
        let page = recover_inline_page(snapshot.as_ref(), Some(&cursor), 1, 8, 1, &mut trace)
            .await
            .unwrap();
        assert_eq!(trace.limits, [8, 4]);
        assert!(trace.discarded_bytes > 0);
        assert_eq!((page.rows, page.bytes), (4, 4 * 65_536));
        assert_eq!(
            page.next,
            Some(RetryCursor {
                task: "inline-0004".into(),
                due_at: 1
            })
        );
        let mut terminal = RecoveryTrace::default();
        assert!(matches!(
            recover_inline_page(
                snapshot.as_ref(),
                Some(&cursor),
                1,
                1000,
                usize::MAX,
                &mut terminal,
            )
            .await,
            Err(RetryStreamError::Resource)
        ));
        assert_eq!(terminal.limits, [1000, 500, 250, 125, 62, 31, 15, 7, 3, 1]);
        assert!(terminal.discarded_bytes > 0);
        // The failed page did not advance the cursor or poison the snapshot.
        let mut recovered = RecoveryTrace::default();
        let page = recover_inline_page(snapshot.as_ref(), Some(&cursor), 1, 1, 0, &mut recovered)
            .await
            .unwrap();
        assert_eq!(
            page.next,
            Some(RetryCursor {
                task: "inline-0001".into(),
                due_at: 0
            })
        );
        assert_eq!((page.rows, page.bytes), (1, 65_536));
        drop(snapshot);
        rig.close().await;
    }
}
