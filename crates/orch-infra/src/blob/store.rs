//! The disk adapter. Writes follow R58: temporary file, sync, rename, and only then may a row name the blob.
use super::{BlobHash, InvalidHash, MAX_BLOB_BYTES};
use crate::data_dir::confined::{ConfinedDir, ConfinedError, FileAccess};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error(transparent)]
    InvalidHash(#[from] InvalidHash),
    #[error("no blob {0}")]
    NotFound(BlobHash),
    #[error("blob {0} does not hash to its name")]
    Corrupt(BlobHash),
    #[error("blob is over the {limit} byte limit")]
    TooLarge { limit: u64 },
    #[error(transparent)]
    Confined(#[from] ConfinedError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub(super) type Result<T> = std::result::Result<T, BlobError>;

/// The steps of a write, in order. Each runs through [`Hooks`] so a test can see the order and fail one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    WriteTmp,
    SyncTmp,
    Publish,
    SyncDirs,
}

/// A seam for the order of a write and for failing one step; production runs the operation unchanged.
pub(crate) trait Hooks: Send + Sync {
    fn run(&self, step: Step, op: &mut dyn FnMut() -> Result<()>) -> Result<()>;
}

#[derive(Debug)]
struct Direct;

impl Hooks for Direct {
    fn run(&self, _step: Step, op: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        op()
    }
}

/// `blobs/sha256/` and `blobs/tmp/` of one data directory, held by descriptor.
pub struct DiskBlobStore {
    sha256: ConfinedDir,
    tmp: ConfinedDir,
    hooks: Box<dyn Hooks>,
    #[cfg(test)]
    fail_stream_read: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for DiskBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskBlobStore").finish_non_exhaustive()
    }
}

fn is_missing(e: &ConfinedError) -> bool {
    matches!(e, ConfinedError::Io(io) if io.kind() == io::ErrorKind::NotFound)
}

/// Opens or creates one directory; the flag says whether this call made it.
fn open_or_create(parent: &ConfinedDir, name: &str) -> Result<(ConfinedDir, bool)> {
    match parent.create_dir(name) {
        Ok(dir) => Ok((dir, true)),
        Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
            Ok((parent.open_dir(name)?, false))
        }
        Err(e) => Err(e.into()),
    }
}

impl DiskBlobStore {
    /// `data_root` is the data directory's confined root (`DataDir::root()`).
    pub fn open(data_root: &ConfinedDir) -> Result<Self> {
        Self::with_hooks(data_root, Box::new(Direct))
    }

    pub(crate) fn with_hooks(data_root: &ConfinedDir, hooks: Box<dyn Hooks>) -> Result<Self> {
        let blobs = data_root.ensure_dir("blobs")?;
        let sha256 = blobs.ensure_dir("sha256")?;
        let tmp = blobs.ensure_dir("tmp")?;
        blobs.sync()?;
        data_root.sync()?;
        Ok(Self {
            sha256,
            tmp,
            hooks,
            #[cfg(test)]
            fail_stream_read: std::sync::atomic::AtomicBool::new(false),
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_stream_read(&self) {
        self.fail_stream_read
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Stores the bytes and returns their hash; a blob already stored, and intact, is not written again.
    /// When this returns the bytes are durable under their name, so a row may name them.
    pub fn put(&self, bytes: &[u8]) -> Result<BlobHash> {
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                limit: MAX_BLOB_BYTES,
            });
        }
        let hash = BlobHash::of(bytes);
        let (ab, cd, name) = names(&hash);
        // Anything unusable at the final name (a link, a FIFO, other bytes) is replaced by the rename below.
        if self.is_intact(&hash, bytes.len() as u64, &ab, &cd, &name) {
            // A prior publication may have failed its directory sync, or its
            // transaction may have rolled back. Re-establish durability on retry.
            self.sync_name(&ab, &cd, &name)?;
            return Ok(hash);
        }
        let tmp_name = tmp_name();
        let result = self.publish(bytes, &tmp_name, &ab, &cd, &name);
        if result.is_err() {
            // The temporary file is ours; best effort, the sweep removes what this leaves.
            let _ = self.tmp.remove_file(&tmp_name);
        }
        result.map(|()| hash)
    }

    fn publish(&self, bytes: &[u8], tmp_name: &str, ab: &str, cd: &str, name: &str) -> Result<()> {
        let mut file = self.tmp.create_file(tmp_name)?;
        self.hooks
            .run(Step::WriteTmp, &mut || Ok(file.write_all(bytes)?))?;
        self.hooks
            .run(Step::SyncTmp, &mut || Ok(file.sync_all()?))?;
        let (ab_dir, _) = open_or_create(&self.sha256, ab)?;
        let (cd_dir, _) = open_or_create(&ab_dir, cd)?;
        self.hooks.run(Step::Publish, &mut || {
            Ok(self.tmp.rename(tmp_name, &cd_dir, name)?)
        })?;
        self.sync_name(ab, cd, name)
    }

    fn sync_name(&self, ab: &str, cd: &str, name: &str) -> Result<()> {
        self.hooks.run(Step::SyncDirs, &mut || {
            let ab_dir = self.sha256.open_dir(ab)?;
            let cd_dir = ab_dir.open_dir(cd)?;
            cd_dir.open_file(name, FileAccess::Read)?.sync_all()?;
            cd_dir.sync()?;
            // Sync ancestors even if created by a previous failed put.
            ab_dir.sync()?;
            self.sha256.sync()?;
            self.tmp.sync()?;
            Ok(())
        })
    }

    /// The whole blob, after its bytes have been hashed against its name.
    pub fn get(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        let (ab, cd, name) = names(hash);
        let mut file = self
            .shard(&ab, &cd)
            .and_then(|dir| dir.open_file(&name, FileAccess::Read))
            .map_err(|e| missing(hash, e))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_BLOB_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                limit: MAX_BLOB_BYTES,
            });
        }
        if BlobHash::of(&bytes) != *hash {
            return Err(BlobError::Corrupt(*hash));
        }
        Ok(bytes)
    }

    /// Deletes a blob's file; an absent blob is success (the `blob.delete` intent is repeated). Returns
    /// whether a file was removed. A link or special file at the name is refused and left in place.
    pub fn remove(&self, hash: &BlobHash) -> Result<bool> {
        let (ab, cd, name) = names(hash);
        match self.shard(&ab, &cd).and_then(|dir| dir.remove_file(&name)) {
            Ok(()) => Ok(true),
            Err(e) if is_missing(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn shard(&self, ab: &str, cd: &str) -> std::result::Result<ConfinedDir, ConfinedError> {
        self.sha256.open_dir(ab)?.open_dir(cd)
    }

    fn is_intact(&self, hash: &BlobHash, len: u64, ab: &str, cd: &str, name: &str) -> bool {
        let Ok(mut file) = self
            .shard(ab, cd)
            .and_then(|dir| dir.open_file(name, FileAccess::Read))
        else {
            return false;
        };
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut seen = 0u64;
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    seen += n as u64;
                    if seen > len {
                        return false;
                    }
                    hasher.update(&buf[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        seen == len && BlobHash::from_digest(hasher.finalize().into()) == *hash
    }
}

fn missing(hash: &BlobHash, e: ConfinedError) -> BlobError {
    if is_missing(&e) {
        BlobError::NotFound(*hash)
    } else {
        e.into()
    }
}

/// Shard directories and file name of a parsed hash: the first two and the next two hex digits.
fn names(hash: &BlobHash) -> (String, String, String) {
    let hex = hash.to_string();
    (hex[0..2].to_owned(), hex[2..4].to_owned(), hex)
}

fn tmp_name() -> String {
    const HEX: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];
    nanoid::nanoid!(32, &HEX)
}

use orch_core::store::blob::BlobStreamHasher;
use orch_core::store::retries::stream::{
    RETRY_STREAM_WINDOW_BYTES, Result as StreamResult, RetryStreamError,
};
use tokio_util::sync::CancellationToken;

fn stream_access(error: BlobError) -> RetryStreamError {
    match error {
        BlobError::Io(e) | BlobError::Confined(ConfinedError::Io(e))
            if e.kind() != io::ErrorKind::NotFound =>
        {
            RetryStreamError::Store(orch_core::store::StoreError::Backend(
                orch_core::Error::other("retry object access failed"),
            ))
        }
        _ => RetryStreamError::Corrupt,
    }
}

struct StreamHasher(Sha256);
impl BlobStreamHasher for StreamHasher {
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn matches(&self, expected: &str) -> bool {
        let hash = BlobHash::from_digest(self.0.clone().finalize().into());
        // Avoid allocating a formatted hash, including on malformed input.
        expected.len() == 64
            && hash
                .as_bytes()
                .iter()
                .zip(expected.as_bytes().as_chunks::<2>().0)
                .all(|(byte, pair)| {
                    const HEX: &[u8] = b"0123456789abcdef";
                    *pair == [HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]
                })
    }
}

impl DiskBlobStore {
    /// Preserve native disk-I/O origin privately. Consumer callback failures
    /// bypass this observation; the public blob/stream error stays unchanged.
    pub(crate) fn read_windowed_observed(
        &self,
        hash: &BlobHash,
        window: &mut [u8],
        cancel: &CancellationToken,
        consume: &mut dyn FnMut(&[u8]) -> StreamResult<()>,
        observed: &mut Option<io::ErrorKind>,
    ) -> StreamResult<()> {
        let mut access = |error: BlobError| {
            match &error {
                BlobError::Io(e) | BlobError::Confined(ConfinedError::Io(e))
                    if e.kind() != io::ErrorKind::NotFound =>
                {
                    *observed = Some(e.kind());
                }
                _ => {}
            }
            stream_access(error)
        };

        if cancel.is_cancelled() {
            return Err(RetryStreamError::Cancelled);
        }
        #[cfg(test)]
        if self
            .fail_stream_read
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(access(BlobError::Io(
                io::ErrorKind::PermissionDenied.into(),
            )));
        }
        if window.is_empty() || window.len() > RETRY_STREAM_WINDOW_BYTES {
            return Err(RetryStreamError::Resource);
        }
        let (ab, cd, name) = names(hash);
        let mut file = self
            .shard(&ab, &cd)
            .and_then(|dir| dir.open_file(&name, FileAccess::Read))
            .map_err(|e| access(missing(hash, e)))?;
        let length = file.metadata().map_err(|e| access(e.into()))?.len();
        if length > MAX_BLOB_BYTES {
            return Err(RetryStreamError::Corrupt);
        }
        let mut digest = Sha256::new();
        let mut seen = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(RetryStreamError::Cancelled);
            }
            let n = match file.read(window) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(access(e.into())),
            };
            if n == 0 {
                break;
            }
            seen = seen
                .checked_add(n as u64)
                .ok_or(RetryStreamError::Resource)?;
            if seen > length {
                return Err(RetryStreamError::Corrupt);
            }
            digest.update(&window[..n]);
            if cancel.is_cancelled() {
                return Err(RetryStreamError::Cancelled);
            }
            consume(&window[..n])?;
        }
        if seen != length || BlobHash::from_digest(digest.finalize().into()) != *hash {
            return Err(RetryStreamError::Corrupt);
        }
        if cancel.is_cancelled() {
            return Err(RetryStreamError::Cancelled);
        }
        Ok(())
    }
}

impl orch_core::store::blob::BlobStore for DiskBlobStore {
    type Hash = BlobHash;
    type Error = BlobError;
    fn put(&self, bytes: &[u8]) -> Result<BlobHash> {
        DiskBlobStore::put(self, bytes)
    }
    fn get(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        DiskBlobStore::get(self, hash)
    }
    fn remove(&self, hash: &BlobHash) -> Result<bool> {
        DiskBlobStore::remove(self, hash)
    }
    fn stream_len(&self, hash: &BlobHash) -> StreamResult<u64> {
        let (ab, cd, name) = names(hash);
        let file = self
            .shard(&ab, &cd)
            .and_then(|dir| dir.open_file(&name, FileAccess::Read))
            .map_err(|e| stream_access(missing(hash, e)))?;
        let length = file.metadata().map_err(|e| stream_access(e.into()))?.len();
        if length > MAX_BLOB_BYTES {
            return Err(RetryStreamError::Corrupt);
        }
        Ok(length)
    }
    fn read_windowed(
        &self,
        hash: &BlobHash,
        window: &mut [u8],
        cancel: &CancellationToken,
        consume: &mut dyn FnMut(&[u8]) -> StreamResult<()>,
    ) -> StreamResult<()> {
        self.read_windowed_observed(hash, window, cancel, consume, &mut None)
    }
    fn with_stream_hasher(
        &self,
        work: &mut dyn FnMut(&mut dyn BlobStreamHasher) -> StreamResult<()>,
    ) -> StreamResult<()> {
        work(&mut StreamHasher(Sha256::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Whether the blob has its final name, looked at through the same confined module.
    fn published(root: &ConfinedDir, hash: &BlobHash) -> bool {
        let (ab, cd, name) = names(hash);
        root.open_dir("blobs")
            .and_then(|d| d.open_dir("sha256"))
            .and_then(|d| d.open_dir(&ab))
            .and_then(|d| d.open_dir(&cd))
            .and_then(|d| d.open_file(&name, FileAccess::Read))
            .is_ok()
    }

    /// Records each step with whether the blob had its final name when the step ran, runs the real
    /// operation, and fails the chosen step instead of running it.
    struct Probe {
        root: ConfinedDir,
        hash: BlobHash,
        fail: Option<Step>,
        seen: Mutex<Vec<(Step, bool)>>,
    }

    impl Hooks for Arc<Probe> {
        fn run(&self, step: Step, op: &mut dyn FnMut() -> Result<()>) -> Result<()> {
            let named = published(&self.root, &self.hash);
            self.seen.lock().unwrap().push((step, named));
            if self.fail == Some(step) {
                return Err(BlobError::Io(io::Error::other("injected")));
            }
            op()
        }
    }

    struct Rig {
        _tmp: tempfile::TempDir,
        root: ConfinedDir,
        store: DiskBlobStore,
        probe: Arc<Probe>,
        hash: BlobHash,
    }

    fn rig(bytes: &[u8], fail: Option<Step>) -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let hash = BlobHash::of(bytes);
        let probe = Arc::new(Probe {
            root: ConfinedDir::open(tmp.path()).unwrap(),
            hash,
            fail,
            seen: Mutex::new(Vec::new()),
        });
        let root = ConfinedDir::open(tmp.path()).unwrap();
        let store = DiskBlobStore::with_hooks(&root, Box::new(probe.clone())).unwrap();
        Rig {
            _tmp: tmp,
            root,
            store,
            probe,
            hash,
        }
    }

    fn steps(rig: &Rig) -> Vec<(Step, bool)> {
        rig.probe.seen.lock().unwrap().clone()
    }

    fn tmp_entries(rig: &Rig) -> usize {
        rig.root
            .open_dir("blobs")
            .and_then(|d| d.open_dir("tmp"))
            .and_then(|d| d.list())
            .unwrap()
            .len()
    }

    #[test]
    fn bytes_are_written_and_synced_before_the_blob_has_its_name() {
        let rig = rig(b"ordered", None);
        rig.store.put(b"ordered").unwrap();
        assert_eq!(
            steps(&rig),
            [
                (Step::WriteTmp, false),
                (Step::SyncTmp, false),
                (Step::Publish, false),
                (Step::SyncDirs, true),
            ]
        );
    }

    #[test]
    fn a_stored_blob_only_rechecks_durability() {
        let rig = rig(b"twice", None);
        rig.store.put(b"twice").unwrap();
        let first = steps(&rig).len();
        rig.store.put(b"twice").unwrap();
        assert_eq!(&steps(&rig)[first..], &[(Step::SyncDirs, true)]);
    }

    #[test]
    fn a_failure_before_publication_leaves_no_blob_and_no_temporary_file() {
        for step in [Step::WriteTmp, Step::SyncTmp, Step::Publish] {
            let rig = rig(b"fails", Some(step));
            assert!(rig.store.put(b"fails").is_err(), "{step:?}");
            assert!(!published(&rig.root, &rig.hash), "{step:?}");
            assert_eq!(tmp_entries(&rig), 0, "{step:?}");
            // A retry without the fault stores the blob.
            let retry = DiskBlobStore::open(&rig.root).unwrap();
            assert!(retry.get(&rig.hash).is_err(), "{step:?}");
            retry.put(b"fails").unwrap();
            assert_eq!(retry.get(&rig.hash).unwrap(), b"fails");
        }
    }

    #[test]
    fn a_retry_after_failed_directory_sync_must_sync_before_returning_a_hash() {
        let rig = rig(b"retry sync", Some(Step::SyncDirs));
        assert!(rig.store.put(b"retry sync").is_err());
        // Failure persists: a dedup hit cannot turn the unsynced publication into success.
        assert!(rig.store.put(b"retry sync").is_err());
    }

    #[test]
    fn a_failure_to_sync_the_directories_reports_the_error_and_leaves_an_intact_blob() {
        let rig = rig(b"dirs", Some(Step::SyncDirs));
        assert!(rig.store.put(b"dirs").is_err());
        // The caller got no hash, so no row names it; the bytes are whole and a later put must retry the directory sync.
        assert_eq!(rig.store.get(&rig.hash).unwrap(), b"dirs");
    }
}

#[cfg(test)]
mod retry_stream_tests {
    use super::*;
    use orch_core::store::blob::BlobStore;
    use orch_core::store::retries::stream::{RetryStreamAdmission, RetryStreamError};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn confined_stream_verifies_without_whole_get_and_honors_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = ConfinedDir::open(tmp.path()).unwrap();
        let store = DiskBlobStore::open(&root).unwrap();
        let bytes = "é🙂".repeat(22_000).into_bytes();
        let hash = store.put(&bytes).unwrap();
        assert_eq!(store.stream_len(&hash).unwrap(), bytes.len() as u64);
        let admission = RetryStreamAdmission::new(65_536);
        let mut window = admission.buffer(65_536).unwrap();
        let token = CancellationToken::new();
        let mut seen = 0;
        store
            .read_windowed(&hash, window.as_mut_slice(), &token, &mut |part| {
                assert!(part.len() <= 65_536);
                assert_eq!(part, &bytes[seen..seen + part.len()]);
                seen += part.len();
                Ok(())
            })
            .unwrap();
        assert_eq!(seen, bytes.len());
        token.cancel();
        assert!(matches!(
            store.read_windowed(&hash, window.as_mut_slice(), &token, &mut |_| panic!(
                "cancelled callback"
            )),
            Err(RetryStreamError::Cancelled)
        ));
        let (ab, cd, name) = names(&hash);
        let path = tmp.path().join("blobs/sha256").join(ab).join(cd).join(name);
        std::fs::write(path, vec![b'x'; bytes.len()]).unwrap();
        assert!(matches!(
            store.read_windowed(
                &hash,
                window.as_mut_slice(),
                &CancellationToken::new(),
                &mut |_| Ok(())
            ),
            Err(RetryStreamError::Corrupt)
        ));
    }
}
