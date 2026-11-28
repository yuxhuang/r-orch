//! Durable finish files (R101/R102/R129), without a runtime caller.
//!
//! Keep one spool per locked data directory. Calls are synchronous and must run
//! off the async executor. Publish blobs first, then write the finish before
//! handing it to the writer. An error means the caller must retain the finish,
//! retry both persistence paths and report/suppress claims as R129 requires.
//!
//! Replay delegates R36 and the transaction to the caller. Only a committed
//! receipt permits removal; a failed/unknown commit must return an error and
//! reconcile against durable state on retry. These primitives do not establish
//! SQLite durability under NORMAL or resolve the spec's outstanding O8 question.
//! Linux uses File::sync_all (fsync) for files and directories; this is not a
//! power-loss proof. Temporary files left by a killed publisher are blob scratch.
use super::confined::{ConfinedDir, ConfinedError, FileAccess};
use crate::blob::{BlobHash, INLINE_TEXT_MAX_BYTES};
use orch_domain::{RunStatus, TokenUsage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::Mutex;

pub const MAX_FINISH_BYTES: usize = 128 * 1024;

/// Everything recovery needs from the completed process. Identifiers are data,
/// including separators and NULs. The caller owns task lookup and R36 fencing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishRecord {
    pub project: String,
    pub run: String,
    pub lineage: String,
    pub epoch: u64,
    pub status: RunStatus,
    pub finished_at: String,
    pub result: Option<FinishResult>,
    pub usage: Option<TokenUsage>,
    pub last_event_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FinishResult {
    Inline(String),
    /// Already published and synced by the caller; never opened by the spool.
    Blob {
        hash: String,
        bytes: u64,
    },
}

impl FinishRecord {
    fn valid(&self) -> bool {
        !matches!(self.status, RunStatus::Preparing | RunStatus::Running)
            && match &self.result {
                Some(FinishResult::Inline(text)) => text.len() <= INLINE_TEXT_MAX_BYTES,
                Some(FinishResult::Blob { hash, .. }) => hash.parse::<BlobHash>().is_ok(),
                None => true,
            }
    }
}

/// Domain-separated, length-prefixed hash of the exact UTF-8 identifier bytes.
pub fn finish_file_name(project: &str, run: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orch-finish-file-v1\0");
    for id in [project, run] {
        hash.update((id.len() as u64).to_be_bytes());
        hash.update(id.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error(transparent)]
    Confined(#[from] ConfinedError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid finish record")]
    InvalidRecord,
    #[error("finish record exceeds 128 KiB")]
    TooLarge,
    #[error("a different or damaged finish already occupies this name")]
    Conflict,
    #[error("finish spool unavailable")]
    Unavailable,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError<E> {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error("finish transaction failed")]
    Apply(E),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Write,
    SyncFile,
    SyncScratch,
    Publish,
    SyncFinish,
    SyncScratchAfter,
    Remove,
    SyncRemoval,
}

// Match the blob adapter's operation seam: production executes unchanged;
// private tests inject failures or kill the process at a concrete I/O step.
trait Hooks: std::fmt::Debug + Send + Sync {
    fn run(
        &self,
        step: Step,
        op: &mut dyn FnMut() -> Result<(), SpoolError>,
    ) -> Result<(), SpoolError>;
}
#[derive(Debug)]
struct Direct;
impl Hooks for Direct {
    fn run(
        &self,
        _step: Step,
        op: &mut dyn FnMut() -> Result<(), SpoolError>,
    ) -> Result<(), SpoolError> {
        op()
    }
}

#[derive(Debug)]
pub struct FinishSpool {
    finish: ConfinedDir,
    temporary: ConfinedDir,
    serial: Mutex<()>,
    hooks: Box<dyn Hooks>,
}

#[derive(Debug, Default)]
pub struct ReplayReport {
    pub committed: usize,
    /// Damaged records plus the caller's absent/finished/old-lineage records.
    pub discarded: usize,
    /// Listing entries outside the hash grammar. Never opened or removed.
    pub unrecognized: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayDisposition {
    /// R36 applied, or reconciliation proved that the finish already committed.
    Committed,
    /// No unfinished run exists, or its lineage differs. No row was changed.
    Discard,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    finish: FinishRecord,
    checksum: String,
}

// R129 has a hard encoded-size ceiling. Use serde's streaming serializer, not
// to_vec, so a large/escaped identifier cannot allocate beyond that ceiling.
#[derive(Default)]
struct BoundedBytes {
    bytes: Vec<u8>,
    full: bool,
}

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FINISH_BYTES - self.bytes.len() {
            self.full = true;
            return Err(io::Error::other("finish size limit"));
        }
        self.bytes
            .try_reserve_exact(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "finish allocation failed"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checksum(finish: &FinishRecord) -> Result<String, SpoolError> {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, finish).map_err(|_| SpoolError::InvalidRecord)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

#[derive(Serialize)]
struct Encoded<'a> {
    version: u32,
    finish: &'a FinishRecord,
    checksum: String,
}

fn encode(finish: &FinishRecord) -> Result<Vec<u8>, SpoolError> {
    if !finish.valid() {
        return Err(SpoolError::InvalidRecord);
    }
    let mut writer = BoundedBytes::default();
    let encoded = Encoded {
        version: 1,
        finish,
        checksum: checksum(finish)?,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, &encoded) {
        return Err(if writer.full {
            SpoolError::TooLarge
        } else {
            SpoolError::Io(io::Error::new(
                error.io_error_kind().unwrap_or(io::ErrorKind::Other),
                "finish encoding failed",
            ))
        });
    }
    Ok(writer.bytes)
}

fn absent(error: &ConfinedError) -> bool {
    matches!(error, ConfinedError::Io(e) if e.kind() == io::ErrorKind::NotFound)
}

impl FinishSpool {
    /// Open only fixed directories that DataDir already created. Does not create
    /// roots or acquire their daemon lock; the caller must keep that lock alive.
    pub fn open(root: &ConfinedDir) -> Result<Self, SpoolError> {
        let spool = root.open_dir("spool")?;
        let finish = spool.open_dir("finish")?;
        let blobs = root.open_dir("blobs")?;
        let temporary = blobs.open_dir("tmp")?;
        // DataDir may have just created these ancestors. Sync their child
        // entries too; syncing only finish would not persist spool/finish.
        spool.sync()?;
        blobs.sync()?;
        root.sync()?;
        Ok(Self {
            finish,
            temporary,
            serial: Mutex::new(()),
            hooks: Box::new(Direct),
        })
    }

    /// Atomic publication: exclusive 0600 scratch, write, fsync, rename, fsync
    /// both directories. Failure is never acknowledged as a safe finish, even
    /// when the rename happened. An equal retry resyncs the published record.
    pub fn write(&self, finish: &FinishRecord) -> Result<(), SpoolError> {
        let _guard = self.serial.lock().map_err(|_| SpoolError::Unavailable)?;
        let bytes = encode(finish)?;
        let name = finish_file_name(&finish.project, &finish.run);
        match self.read(&name) {
            Ok((file, Some(existing))) if existing == *finish => {
                self.hooks
                    .run(Step::SyncFile, &mut || Ok(file.sync_all()?))?;
                self.hooks
                    .run(Step::SyncFinish, &mut || Ok(self.finish.sync()?))?;
                self.hooks
                    .run(Step::SyncScratchAfter, &mut || Ok(self.temporary.sync()?))?;
                return Ok(());
            }
            Ok(_) => return Err(SpoolError::Conflict),
            Err(SpoolError::Confined(e)) if absent(&e) => {}
            Err(e) => return Err(e),
        }
        const HEX: [char; 16] = [
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
        ];
        let scratch = nanoid::nanoid!(32, &HEX);
        let mut file = self.temporary.create_file(&scratch)?;
        let result = (|| {
            self.hooks
                .run(Step::Write, &mut || Ok(file.write_all(&bytes)?))?;
            self.hooks
                .run(Step::SyncFile, &mut || Ok(file.sync_all()?))?;
            self.hooks
                .run(Step::SyncScratch, &mut || Ok(self.temporary.sync()?))?;
            self.hooks.run(Step::Publish, &mut || {
                Ok(self.temporary.rename(&scratch, &self.finish, &name)?)
            })?;
            self.hooks
                .run(Step::SyncFinish, &mut || Ok(self.finish.sync()?))?;
            self.hooks
                .run(Step::SyncScratchAfter, &mut || Ok(self.temporary.sync()?))?;
            Ok(())
        })();
        if result.is_err() {
            // Best effort only; the original error still means unspooled. A
            // killed publisher can also leave scratch for blob maintenance.
            let _ = self.temporary.remove_file(&scratch);
            let _ = self.temporary.sync();
        }
        result
    }

    /// Only call after the writer receipt or reconciliation establishes commit.
    /// Compare the full record so a stale receipt cannot remove another finish.
    /// Idempotent, including syncing a previous removal whose sync failed.
    pub fn remove_committed(&self, finish: &FinishRecord) -> Result<(), SpoolError> {
        let _guard = self.serial.lock().map_err(|_| SpoolError::Unavailable)?;
        let name = finish_file_name(&finish.project, &finish.run);
        match self.read(&name) {
            Ok((_, Some(existing))) if existing == *finish => self.remove(&name),
            Err(SpoolError::Confined(e)) if absent(&e) => self
                .hooks
                .run(Step::SyncRemoval, &mut || Ok(self.finish.sync()?)),
            Ok(_) => Err(SpoolError::Conflict),
            Err(e) => Err(e),
        }
    }

    /// Each selected record is bounded and checked before the callback. Callback
    /// errors retain that file and stop replay; previously processed files stay
    /// processed. The callback must not reenter this spool. Unknown names stay
    /// untouched for verification. Links/special files fail closed, not discarded.
    pub fn replay<E>(
        &self,
        mut apply: impl FnMut(&FinishRecord) -> Result<ReplayDisposition, E>,
    ) -> Result<ReplayReport, ReplayError<E>> {
        let _guard = self.serial.lock().map_err(|_| SpoolError::Unavailable)?;
        let mut report = ReplayReport::default();
        for name in self.finish.list().map_err(SpoolError::from)? {
            let Some(hash) = name.to_str().filter(|s| s.parse::<BlobHash>().is_ok()) else {
                report.unrecognized.push(name);
                continue;
            };
            let (_, finish) = self.read(hash)?;
            let disposition = match finish {
                Some(finish) => apply(&finish).map_err(ReplayError::Apply)?,
                None => ReplayDisposition::Discard,
            };
            self.remove(hash)?;
            match disposition {
                ReplayDisposition::Committed => report.committed += 1,
                ReplayDisposition::Discard => report.discarded += 1,
            }
        }
        Ok(report)
    }

    fn read(&self, name: &str) -> Result<(File, Option<FinishRecord>), SpoolError> {
        let mut file = self.finish.open_file(name, FileAccess::Untrusted)?;
        if file.metadata()?.len() > MAX_FINISH_BYTES as u64 {
            return Ok((file, None));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(MAX_FINISH_BYTES + 1)
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "finish allocation failed"))?;
        (&mut file)
            .take(MAX_FINISH_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FINISH_BYTES {
            return Ok((file, None));
        }
        let Ok(envelope) = serde_json::from_slice::<Envelope>(&bytes) else {
            return Ok((file, None));
        };
        let finish = envelope.finish;
        let valid = envelope.version == 1
            && finish.valid()
            && finish_file_name(&finish.project, &finish.run) == name
            && checksum(&finish)? == envelope.checksum;
        Ok((file, valid.then_some(finish)))
    }

    fn remove(&self, name: &str) -> Result<(), SpoolError> {
        // remove_file itself checks regular type; use Untrusted as well to
        // refuse hard links before an unlink changes an outside inode's ctime.
        self.finish.open_file(name, FileAccess::Untrusted)?;
        self.hooks
            .run(Step::Remove, &mut || Ok(self.finish.remove_file(name)?))?;
        self.hooks
            .run(Step::SyncRemoval, &mut || Ok(self.finish.sync()?))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const PUBLICATION: [Step; 6] = [
        Step::Write,
        Step::SyncFile,
        Step::SyncScratch,
        Step::Publish,
        Step::SyncFinish,
        Step::SyncScratchAfter,
    ];

    fn record() -> FinishRecord {
        FinishRecord {
            project: "synthetic-project".into(),
            run: "synthetic-run".into(),
            lineage: "synthetic-lineage".into(),
            epoch: 9,
            status: RunStatus::Failed,
            finished_at: "2026-11-28T00:00:00Z".into(),
            result: Some(FinishResult::Inline("synthetic finish".into())),
            usage: None,
            last_event_number: Some(3),
        }
    }

    fn fixture() -> (tempfile::TempDir, ConfinedDir) {
        let temp = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(temp.path());
        let root = ConfinedDir::open(temp.path()).unwrap();
        root.create_dir("spool")
            .unwrap()
            .create_dir("finish")
            .unwrap();
        root.create_dir("blobs").unwrap().create_dir("tmp").unwrap();
        (temp, root)
    }

    #[derive(Debug)]
    struct Trace {
        log: Arc<Mutex<Vec<Step>>>,
        fail: Option<(Step, bool)>,
    }
    impl Hooks for Trace {
        fn run(
            &self,
            step: Step,
            op: &mut dyn FnMut() -> Result<(), SpoolError>,
        ) -> Result<(), SpoolError> {
            self.log.lock().unwrap().push(step);
            if self.fail == Some((step, false)) {
                return Err(io::Error::other("synthetic I/O failure before step").into());
            }
            op()?;
            if self.fail == Some((step, true)) {
                return Err(io::Error::other("synthetic I/O failure after step").into());
            }
            Ok(())
        }
    }

    #[test]
    fn publication_and_removal_sync_the_file_and_directories_in_order() {
        let (_temp, root) = fixture();
        let mut spool = FinishSpool::open(&root).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        spool.hooks = Box::new(Trace {
            log: log.clone(),
            fail: None,
        });
        spool.write(&record()).unwrap();
        spool.remove_committed(&record()).unwrap();
        let mut expected = PUBLICATION.to_vec();
        expected.extend([Step::Remove, Step::SyncRemoval]);
        assert_eq!(*log.lock().unwrap(), expected);
        // A repeated removal still syncs, so a failed removal sync can recover.
        spool.remove_committed(&record()).unwrap();
        assert_eq!(log.lock().unwrap().last(), Some(&Step::SyncRemoval));
    }

    #[test]
    fn every_publication_failure_is_reported_and_an_equal_retry_resyncs() {
        for step in PUBLICATION {
            for after in [false, true] {
                let (_temp, root) = fixture();
                let mut spool = FinishSpool::open(&root).unwrap();
                spool.hooks = Box::new(Trace {
                    log: Arc::default(),
                    fail: Some((step, after)),
                });
                assert!(spool.write(&record()).is_err(), "{step:?} after={after}");
                let names = spool.finish.list().unwrap();
                let published = matches!(step, Step::SyncFinish | Step::SyncScratchAfter)
                    || (step == Step::Publish && after);
                assert_eq!(names.len(), usize::from(published));
                assert!(spool.temporary.list().unwrap().is_empty());
                let log = Arc::new(Mutex::new(Vec::new()));
                spool.hooks = Box::new(Trace {
                    log: log.clone(),
                    fail: None,
                });
                spool.write(&record()).unwrap();
                if published {
                    assert_eq!(
                        *log.lock().unwrap(),
                        [Step::SyncFile, Step::SyncFinish, Step::SyncScratchAfter]
                    );
                }
                assert_eq!(
                    spool
                        .replay(|found| {
                            assert_eq!(found, &record());
                            Ok::<_, io::Error>(ReplayDisposition::Committed)
                        })
                        .unwrap()
                        .committed,
                    1
                );
            }
        }
    }

    #[test]
    fn removal_errors_are_not_success_and_the_receipt_can_be_retried() {
        for step in [Step::Remove, Step::SyncRemoval] {
            for after in [false, true] {
                let (_temp, root) = fixture();
                let mut spool = FinishSpool::open(&root).unwrap();
                spool.write(&record()).unwrap();
                spool.hooks = Box::new(Trace {
                    log: Arc::default(),
                    fail: Some((step, after)),
                });
                assert!(spool.remove_committed(&record()).is_err());
                spool.hooks = Box::new(Direct);
                spool.remove_committed(&record()).unwrap();
                assert!(spool.finish.list().unwrap().is_empty());
            }
        }
    }

    #[derive(Debug)]
    struct KillAt {
        step: Step,
        after: bool,
    }
    impl Hooks for KillAt {
        fn run(
            &self,
            step: Step,
            op: &mut dyn FnMut() -> Result<(), SpoolError>,
        ) -> Result<(), SpoolError> {
            if step != self.step || self.after {
                op()?;
            }
            if step == self.step {
                nix::sys::signal::kill(nix::unistd::getpid(), nix::sys::signal::Signal::SIGKILL)
                    .unwrap();
                unreachable!("SIGKILL returned");
            }
            Ok(())
        }
    }

    #[test]
    fn spool_crash_child() {
        let Some(root) = std::env::var_os("ORCH_SYNTHETIC_SPOOL_CRASH_ROOT") else {
            return;
        };
        let index: usize = std::env::var("ORCH_SYNTHETIC_SPOOL_CRASH_STEP")
            .unwrap()
            .parse()
            .unwrap();
        let after = std::env::var("ORCH_SYNTHETIC_SPOOL_CRASH_AFTER").unwrap() == "true";
        let mut spool =
            FinishSpool::open(&ConfinedDir::open(std::path::Path::new(&root)).unwrap()).unwrap();
        spool.hooks = Box::new(KillAt {
            step: PUBLICATION[index],
            after,
        });
        spool.write(&record()).unwrap();
        panic!("publication did not stop at configured step");
    }

    #[test]
    fn a_kill_before_or_after_each_publication_step_leaves_a_whole_finish_or_none() {
        use std::os::unix::process::ExitStatusExt;
        for (index, step) in PUBLICATION.iter().enumerate() {
            for after in [false, true] {
                let (temp, root) = fixture();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "data_dir::spool::tests::spool_crash_child",
                        "--nocapture",
                    ])
                    .env("ORCH_SYNTHETIC_SPOOL_CRASH_ROOT", temp.path())
                    .env("ORCH_SYNTHETIC_SPOOL_CRASH_STEP", index.to_string())
                    .env("ORCH_SYNTHETIC_SPOOL_CRASH_AFTER", after.to_string())
                    .output()
                    .unwrap();
                assert_eq!(output.status.signal(), Some(9), "child: {output:?}");
                let spool = FinishSpool::open(&root).unwrap();
                let report = spool
                    .replay(|found| {
                        assert_eq!(found, &record());
                        Ok::<_, io::Error>(ReplayDisposition::Committed)
                    })
                    .unwrap();
                let published = matches!(step, Step::SyncFinish | Step::SyncScratchAfter)
                    || (*step == Step::Publish && after);
                assert_eq!(
                    report.committed,
                    usize::from(published),
                    "{step:?} after={after}"
                );
                assert_eq!(report.discarded, 0);
            }
        }
    }
}
