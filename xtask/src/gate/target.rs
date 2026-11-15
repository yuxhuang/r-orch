//! Cooperative target state, checked native lifecycle and direct-command publication.
//! The final tier caller integrates NativeTarget::begin and NativeUse::run.
//!
//! Recovery follows the owner's cooperative policy: a free same-inode marker
//! permits reset under the lease. A direct writer that closes its inherited
//! description can overlap recovery; that legacy/direct-writer risk is accepted,
//! not fixed. FS1 descendants and D45 namespace races remain distinct residuals.

mod native;
#[cfg(test)]
mod qualification;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, Metadata, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Inspect the constant paths without creating state or trusting diagnostics.
/// Keep the returned target alive, then call `begin` and `NativeUse::run`.
/// Inspection alone is never permission to build or a qualifying tier result.
pub(super) fn native_admission(
    root: &Path,
    cold: bool,
    lease: &super::lease::Lease,
) -> anyhow::Result<NativeTarget> {
    lease.validate_exclusive()?;
    let owner = nix::unistd::geteuid().as_raw();
    let files = StateFiles::inspect(root, owner, if cold { Kind::Cold } else { Kind::Warm })?;
    native::Boundary::state(&files)?;
    Ok(NativeTarget { files })
}

pub(super) struct NativeTarget {
    files: StateFiles,
}

pub(super) struct NativeUse<'a> {
    marker: Marker<'a>,
    before: Captured,
    action: Action,
}

impl NativeTarget {
    pub(super) fn begin<'a>(
        &'a self,
        lease: &'a super::lease::Lease,
    ) -> anyhow::Result<NativeUse<'a>> {
        // The owner permits recovery on a free same-inode marker under the
        // lease. Diagnostics, PID, EOF and current-tool identity add no authority.
        let mut marker = self.files.admit_configured(Some(lease))?;
        marker.enable_signals()?;
        marker.write_diagnostics(
            format!(
                "purpose=native-{}\npid={}\n",
                self.files.build_name(),
                std::process::id()
            )
            .as_bytes(),
        )?;
        marker.protect()?;
        native::Boundary::inspect(&self.files)?;
        let (before, action) = {
            let mut runner = super::steps::Runner::new(lease, &marker)?;
            let before = Captured::read(&mut runner, &self.files.root)?;
            runner.finished()?;
            let stored = if self.files.kind == Kind::Warm {
                self.files.read_record()?
            } else {
                Stored::Missing
            };
            let action = decide(
                Admission {
                    lease: true,
                    structure: true,
                    marker_found: marker.found,
                    lock_granted: true,
                    marker_same: true,
                    target_exists: self.files.check_target()?,
                    kind: self.files.kind,
                },
                &stored,
                before.current(&self.files.root),
            );
            (before, action)
        };
        anyhow::ensure!(
            !matches!(action, Action::Refuse(_)),
            "REFUSED: target decision {action:?}"
        );
        lease.validate()?;
        marker.check_identity()?;
        // Decide while the old record is available, then invalidate before any
        // destructive operation. A failed removal leaves no reusable record.
        marker.invalidate()?;
        if action != Action::Reuse {
            native::Boundary::state(&self.files)?.remove_build()?;
        }
        lease.validate()?;
        marker.check_identity()?;
        native::Boundary::state(&self.files)?.create_build()?;
        lease.validate()?;
        marker.check_identity()?;
        Ok(NativeUse {
            marker,
            before,
            action,
        })
    }
}

impl Captured {
    fn current<'a>(&'a self, root: &'a Path) -> Current<'a> {
        Current {
            root: root.as_os_str().as_bytes(),
            toolchain: &self.toolchain,
            boot: Some(self.boot),
            sources: &self.sources,
        }
    }
}

/// The callback receives the only runner for this use and the constant target.
/// It must route every direct command through that runner. A nonzero command or
/// a failed judged assertion may finish the target, while never passing the step.
/// An interrupted/failed runner or lost authority retains marker and target.
pub(super) struct NativeOutcome<T> {
    pub(super) result: anyhow::Result<T>,
    pub(super) statuses: Vec<super::steps::Status>,
    pub(super) publication: &'static str,
    pub(super) end_ns: i128,
}

impl NativeUse<'_> {
    pub(super) fn target(&self) -> PathBuf {
        self.marker.files.build_path()
    }

    pub(super) fn decision(&self) -> &'static str {
        match self.action {
            Action::Reuse => "reused",
            Action::Cold => "cold",
            Action::Reset(Reset::NoRecord) => "reset: no record",
            Action::Reset(Reset::Unreadable) => "reset: unreadable record",
            Action::Reset(Reset::Worktree) => "reset: different worktree",
            Action::Reset(Reset::Toolchain) => "reset: toolchain changed",
            Action::Reset(Reset::Content) => "reset: source content changed",
            Action::Reset(Reset::Unfinished) => "reset: previous build did not finish",
            Action::Reset(Reset::Boot) => "reset: boot changed",
            Action::Refuse(_) => "REFUSED",
        }
    }

    pub(super) fn run<T>(
        mut self,
        lease: &super::lease::Lease,
        keep: bool,
        work: impl FnOnce(&mut super::steps::Runner<'_, '_>, &Path) -> anyhow::Result<T>,
    ) -> anyhow::Result<NativeOutcome<T>> {
        let target = self.target();
        native::Boundary::inspect(self.marker.files)?;
        let mut runner = super::steps::Runner::new(lease, &self.marker)?;
        let work_result = work(&mut runner, &target);
        // A handled signal changes the verdict, not the definition of a finished
        // use. Real direct-child reap plus retained marker/lease authority permits
        // finish-only source capture. EOF and an elapsed grace never suffice.
        let interrupted = runner.interrupted().is_some();
        let direct_reaped = runner.child_reaped();
        let ordinary_finish = runner.finished();
        let mut statuses = runner.statuses().to_vec();
        statuses.extend_from_slice(runner.probe_statuses());
        drop(runner);
        let captured = (|| {
            if interrupted && direct_reaped {
                self.marker.finishing_interrupted.set(true);
                lease.validate_exclusive()?;
                self.marker.check_identity()?;
            } else {
                ordinary_finish?;
            }
            let mut finish_runner = if interrupted {
                super::steps::Runner::finish_only(lease, &self.marker)?
            } else {
                super::steps::Runner::new(lease, &self.marker)?
            };
            let after = Captured::read(&mut finish_runner, &self.marker.files.root)?;
            finish_runner.finished()?;
            statuses.extend(finish_runner.statuses().iter().cloned());
            anyhow::ensure!(
                self.before.boot == after.boot && self.before.toolchain == after.toolchain,
                "toolchain/boot identity changed; keep marker, no publication"
            );
            Ok(after)
        })();
        let finished = (|| {
            let after: Captured = captured?;
            lease.validate()?;
            self.marker.check_identity()?;
            let end_ns = i128::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
            )?;
            let finished = Finished {
                direct_reaped: true,
                lease_held: true,
                inherited_holder_valid: true,
            };
            let publication = if self.marker.files.kind == Kind::Warm {
                match self.marker.finish_warm(
                    finished,
                    &self.before.sources,
                    after.current(&self.marker.files.root),
                    end_ns,
                )? {
                    Publication::Record => "record published",
                    Publication::SourceChanged => "no record: source changed",
                    Publication::NoBoot => "no record: boot unavailable",
                }
            } else {
                if !keep {
                    native::Boundary::state(self.marker.files)?.remove_build()?;
                }
                lease.validate()?;
                self.marker.finish_cold(finished)?;
                if keep {
                    "cold target kept by request"
                } else {
                    "cold target removed"
                }
            };
            Ok((publication, end_ns))
        })();
        match finished {
            Ok((publication, end_ns)) => Ok(NativeOutcome {
                result: work_result,
                statuses,
                publication,
                end_ns,
            }),
            Err(source) => Err(UseFailure {
                exits: statuses.iter().map(|s| s.exit).collect(),
                statuses,
                source,
            }
            .into()),
        }
    }
}

pub(super) fn marker_diagnostics(root: &Path, cold: bool) -> Option<Vec<u8>> {
    (|| -> Result<Vec<u8>> {
        let owner = nix::unistd::geteuid().as_raw();
        let files = StateFiles::inspect(root, owner, if cold { Kind::Cold } else { Kind::Warm })?;
        let file = files.open_regular(Leaf::Marker, false, false)?;
        let mut bytes = vec![];
        file.take((MAX_DIAGNOSTIC + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| Fault::Io {
                operation: "read marker diagnostic",
                path: files.path(Leaf::Marker),
                source: e,
            })?;
        if bytes.len() > MAX_DIAGNOSTIC {
            return Err(Fault::Unavailable("marker diagnostic exceeds budget"));
        }
        Ok(bytes)
    })()
    .ok()
}

pub(super) fn cold_space(output: &str) -> anyhow::Result<()> {
    let row = output
        .lines()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("free space unavailable: df row"))?;
    let available = row
        .split_whitespace()
        .nth(3)
        .ok_or_else(|| anyhow::anyhow!("free space unavailable: df columns"))?
        .parse::<u64>()?;
    anyhow::ensure!(
        available >= 12 * 1024 * 1024,
        "cold gate requires 12 GiB free; only {} KiB available",
        available
    );
    Ok(())
}

// Encoding budgets, not timing/teardown bounds. 8 MiB contains a 32K-source
// inventory with ordinary repository paths; 4096 bounds each path/tool identity.
// The byte cap takes precedence over the count cap: hitting either withholds a
// publication/reuse. Diagnostic bytes never participate in a state decision.
const MAX_RECORD: usize = 8 * 1024 * 1024;
// Header (44), two field lengths (8), count (4), terminator (8), and
// maximum root/toolchain payloads. Any accepted Sources fits a complete record,
// even with both identity fields at their limit; no subset/truncated publication.
const MAX_RECORD_OVERHEAD: usize = 64 + 2 * MAX_FIELD;
const MAX_SOURCE_BYTES: usize = MAX_RECORD - MAX_RECORD_OVERHEAD;
const MAX_SOURCES: usize = 32 * 1024;
const MAX_FIELD: usize = 4096;
const MAX_DIAGNOSTIC: usize = 16 * 1024;
const MAGIC: &[u8; 8] = b"ORCHFR01";
const END: &[u8; 8] = b"ENDREC01";
const BOOT_OFFSET: usize = 12;

#[derive(Debug)]
pub(super) enum Fault {
    Boundary {
        path: PathBuf,
        why: &'static str,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Codec(&'static str),
    Unavailable(&'static str),
    MarkerChanged,
    Busy,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boundary { path, why } => {
                write!(
                    f,
                    "REFUSED: structural refusal at {}: {why}",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
            Self::Codec(why) => write!(f, "record damaged: {why}"),
            Self::Unavailable(why) => write!(f, "unavailable: {why}"),
            Self::MarkerChanged => f.write_str("marker path no longer names locked file; no retry"),
            Self::Busy => f.write_str("marker lock not granted"),
        }
    }
}

impl std::error::Error for Fault {}

type Result<T> = std::result::Result<T, Fault>;

fn at<T>(result: io::Result<T>, operation: &'static str, path: &Path) -> Result<T> {
    result.map_err(|source| Fault::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })
}

fn boundary(path: &Path, why: &'static str) -> Fault {
    Fault::Boundary {
        path: path.to_path_buf(),
        why,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BootId([u8; 16]);

impl BootId {
    // Accept a UUID observation, never PID/uptime/time or a fabricated default.
    // Acquisition and trimming platform command output belong to reviewed F2.
    fn parse(text: &str) -> Result<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 36 {
            return Err(Fault::Codec("boot identity must be a complete UUID"));
        }
        let mut out = [0u8; 16];
        let mut nibble = 0;
        for (position, byte) in bytes.iter().copied().enumerate() {
            if [8, 13, 18, 23].contains(&position) {
                if byte != b'-' {
                    return Err(Fault::Codec("invalid boot UUID separator"));
                }
                continue;
            }
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(Fault::Codec("invalid boot UUID digit")),
            };
            out[nibble / 2] = (out[nibble / 2] << 4) | digit;
            nibble += 1;
        }
        Self::from_bytes(out)
    }

    fn from_bytes(bytes: [u8; 16]) -> Result<Self> {
        if bytes == [0; 16] {
            return Err(Fault::Codec("nil boot identity is not an observation"));
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    path: Vec<u8>,
    content: String,
    modified_ns: Option<i128>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sources(BTreeMap<Vec<u8>, Source>);

fn source_path(path: &[u8]) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_FIELD
        || path.contains(&0)
        || path
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
    {
        return Err(Fault::Codec("source path must be a bounded relative path"));
    }
    Ok(())
}

fn content_id(text: &str) -> Result<()> {
    if ![40, 64].contains(&text.len())
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Fault::Codec("source content is not a Git object identity"));
    }
    Ok(())
}

impl Sources {
    fn new(entries: Vec<Source>) -> Result<Self> {
        if entries.len() > MAX_SOURCES {
            return Err(Fault::Codec("too many source entries"));
        }
        let mut all = BTreeMap::new();
        let mut total = 0usize;
        for entry in entries {
            source_path(&entry.path)?;
            content_id(&entry.content)?;
            total = total.saturating_add(entry.path.len() + entry.content.len() + 8);
            if total > MAX_SOURCE_BYTES {
                return Err(Fault::Codec("source inventory byte limit"));
            }
            if all.insert(entry.path.clone(), entry).is_some() {
                return Err(Fault::Codec("duplicate source entry"));
            }
        }
        Ok(Self(all))
    }

    fn identities(&self) -> BTreeMap<Vec<u8>, String> {
        self.0
            .iter()
            .map(|(p, s)| (p.clone(), s.content.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    root: Vec<u8>,
    toolchain: String,
    boot: BootId,
    end_ns: i128,
    sources: BTreeMap<Vec<u8>, String>,
}

impl Record {
    fn new(
        root: Vec<u8>,
        toolchain: String,
        boot: BootId,
        end_ns: i128,
        sources: &Sources,
    ) -> Result<Self> {
        let record = Self {
            root,
            toolchain,
            boot,
            end_ns,
            sources: sources.identities(),
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        if self.root.first() != Some(&b'/')
            || self.root.len() > MAX_FIELD
            || self.root.contains(&0)
            || self.toolchain.is_empty()
            || self.toolchain.len() > MAX_FIELD
            || self.toolchain.contains('\0')
            || self.sources.len() > MAX_SOURCES
        {
            return Err(Fault::Codec("invalid root, toolchain or source count"));
        }
        BootId::from_bytes(self.boot.0)?;
        let mut size = 64 + self.root.len() + self.toolchain.len();
        for (path, content) in &self.sources {
            source_path(path)?;
            content_id(content)?;
            size = size.saturating_add(8 + path.len() + content.len());
            if size > MAX_RECORD {
                return Err(Fault::Codec("record byte limit"));
            }
        }
        Ok(())
    }

    // No new serializer dependency is authorized. This fixed-depth framing uses
    // the compatibility exemption of AGENTS: finite byte fields, exact total,
    // explicit entry count and terminal sentinel. No prefix of an encoding is
    // complete. This is truncation detection, not authentication or a checksum.
    fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&self.boot.0);
        out.extend_from_slice(&self.end_ns.to_be_bytes());
        put_field(&mut out, &self.root)?;
        put_field(&mut out, self.toolchain.as_bytes())?;
        out.extend_from_slice(&(self.sources.len() as u32).to_be_bytes());
        for (path, content) in &self.sources {
            put_field(&mut out, path)?;
            put_field(&mut out, content.as_bytes())?;
        }
        out.extend_from_slice(END);
        if out.len() > MAX_RECORD {
            return Err(Fault::Codec("record byte limit"));
        }
        let length = (out.len() as u32).to_be_bytes();
        out[8..12].copy_from_slice(&length);
        Ok(out)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_RECORD {
            return Err(Fault::Codec("record byte limit"));
        }
        let mut cursor = Cursor { bytes, offset: 0 };
        if cursor.take(8)? != MAGIC || cursor.u32()? as usize != bytes.len() {
            return Err(Fault::Codec("schema or exact length mismatch"));
        }
        let boot = BootId::from_bytes(
            cursor
                .take(16)?
                .try_into()
                .map_err(|_| Fault::Codec("boot field"))?,
        )?;
        let end_ns = i128::from_be_bytes(
            cursor
                .take(16)?
                .try_into()
                .map_err(|_| Fault::Codec("end field"))?,
        );
        let root = cursor.field()?.to_vec();
        let toolchain = cursor.text()?;
        let count = cursor.u32()? as usize;
        if count > MAX_SOURCES {
            return Err(Fault::Codec("source count limit"));
        }
        let mut sources = BTreeMap::new();
        for _ in 0..count {
            let path = cursor.field()?.to_vec();
            source_path(&path)?;
            let content = cursor.text()?;
            content_id(&content)?;
            if sources.insert(path, content).is_some() {
                return Err(Fault::Codec("duplicate source entry"));
            }
        }
        if cursor.take(8)? != END || cursor.offset != bytes.len() {
            return Err(Fault::Codec("missing terminator or trailing data"));
        }
        let result = Self {
            root,
            toolchain,
            boot,
            end_ns,
            sources,
        };
        result.validate()?;
        Ok(result)
    }

    fn compare(&self, current: Current<'_>) -> std::result::Result<(), Reset> {
        if self.validate().is_err() {
            return Err(Reset::Unreadable);
        }
        if current.boot != Some(self.boot) {
            return Err(Reset::Boot);
        }
        if self.root != current.root {
            return Err(Reset::Worktree);
        }
        if self.toolchain != current.toolchain {
            return Err(Reset::Toolchain);
        }
        if self
            .sources
            .keys()
            .any(|p| !current.sources.0.contains_key(p))
        {
            return Err(Reset::Content);
        }
        for (path, source) in &current.sources.0 {
            if self.sources.get(path) != Some(&source.content)
                && !source.modified_ns.is_some_and(|mtime| mtime > self.end_ns)
            {
                return Err(Reset::Content);
            }
        }
        Ok(())
    }
}

fn put_field(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_FIELD || out.len().saturating_add(bytes.len() + 4) > MAX_RECORD {
        return Err(Fault::Codec("field or record byte limit"));
    }
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(Fault::Codec("length overflow"))?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or(Fault::Codec("truncated record"))?;
        self.offset = end;
        Ok(result)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Fault::Codec("integer field"))?,
        ))
    }

    fn field(&mut self) -> Result<&'a [u8]> {
        let length = self.u32()? as usize;
        if length > MAX_FIELD {
            return Err(Fault::Codec("field byte limit"));
        }
        self.take(length)
    }

    fn text(&mut self) -> Result<String> {
        String::from_utf8(self.field()?.to_vec()).map_err(|_| Fault::Codec("invalid UTF-8 field"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stored {
    Missing,
    Damaged,
    // Unreadable regular content still resets, with the original I/O context
    // available for F4's diagnostic. Codec damage has no OS errno to preserve.
    Unreadable {
        operation: &'static str,
        path: PathBuf,
        errno: Option<i32>,
        detail: String,
    },
    Complete(Record),
}

#[derive(Clone, Copy)]
struct Current<'a> {
    root: &'a [u8],
    toolchain: &'a str,
    boot: Option<BootId>,
    sources: &'a Sources,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Warm,
    Cold,
}

#[derive(Clone, Copy)]
struct Admission {
    lease: bool,
    structure: bool,
    marker_found: bool,
    lock_granted: bool,
    marker_same: bool,
    target_exists: bool,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reset {
    NoRecord,
    Unreadable,
    Worktree,
    Toolchain,
    Content,
    Unfinished,
    Boot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Lease,
    Structure,
    Busy,
    MarkerChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Reuse,
    Reset(Reset),
    Cold,
    Refuse(Refusal),
}

// All observations are injected. This function performs no filesystem action
// and cannot validate the lease or marker identity. It is never an I/O permit.
fn decide(o: Admission, record: &Stored, current: Current<'_>) -> Action {
    if !o.lease {
        return Action::Refuse(Refusal::Lease);
    }
    if !o.structure {
        return Action::Refuse(Refusal::Structure);
    }
    if !o.lock_granted {
        return Action::Refuse(Refusal::Busy);
    }
    if !o.marker_same {
        return Action::Refuse(Refusal::MarkerChanged);
    }
    if o.kind == Kind::Cold {
        return Action::Cold;
    }
    if o.marker_found {
        return Action::Reset(Reset::Unfinished);
    }
    if !o.target_exists {
        return Action::Reset(Reset::NoRecord);
    }
    match record {
        Stored::Missing => Action::Reset(Reset::NoRecord),
        Stored::Damaged | Stored::Unreadable { .. } => Action::Reset(Reset::Unreadable),
        Stored::Complete(record) => match record.compare(current) {
            Ok(()) => Action::Reuse,
            Err(why) => Action::Reset(why),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publication {
    Record,
    SourceChanged,
    NoBoot,
}

fn publication(before: &Sources, after: &Sources, boot: Option<BootId>) -> Publication {
    if before.identities() != after.identities() {
        return Publication::SourceChanged;
    }
    if boot.is_none() {
        return Publication::NoBoot;
    }
    Publication::Record
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

fn identity(meta: &Metadata) -> Identity {
    Identity {
        device: meta.dev(),
        inode: meta.ino(),
    }
}

fn check_metadata(
    path: &Path,
    meta: &Metadata,
    directory: bool,
    owner: u32,
    device: u64,
) -> Result<()> {
    if (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
        return Err(boundary(
            path,
            "not the required real directory/regular leaf",
        ));
    }
    if meta.uid() != owner || meta.dev() != device {
        return Err(boundary(path, "different owner or filesystem"));
    }
    if !directory && (meta.mode() & 0o7077 != 0 || meta.nlink() != 1) {
        return Err(boundary(
            path,
            "state leaf must be owner-only and singly linked",
        ));
    }
    Ok(())
}

// Constant ABI bits, not a dependency on a transitive libc crate. Only these
// target ABIs are candidates. Linux arm64 and x86-64 differ for two of the bits.
pub(super) fn open_flags(directory: bool) -> Result<i32> {
    #[cfg(target_os = "macos")]
    let (nofollow, nonblock, dir) = (0x100, 0x4, 0x100000);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let (nofollow, nonblock, dir) = (0x20000, 0x800, 0x10000);
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let (nofollow, nonblock, dir) = (0x8000, 0x800, 0x4000);
    #[cfg(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))]
    {
        Ok(nofollow | nonblock | if directory { dir } else { 0 })
    }
    #[cfg(not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    {
        let _ = directory;
        Err(Fault::Unavailable("unreviewed open flag ABI"))
    }
}

#[derive(Clone, Copy)]
enum Leaf {
    Marker,
    Record,
    Temporary,
}

struct StateFiles {
    root: PathBuf,
    state: PathBuf,
    owner: u32,
    device: u64,
    root_id: Identity,
    kind: Kind,
}

impl StateFiles {
    // owner must eventually be the effective account from F2's reviewed host
    // boundary. A metadata owner or environment value is not proof of that fact.
    fn inspect(root: &Path, owner: u32, kind: Kind) -> Result<Self> {
        if !root.is_absolute() || at(root.canonicalize(), "canonicalize root", root)? != root {
            return Err(boundary(
                root,
                "worktree root must already be canonical and absolute",
            ));
        }
        let meta = at(fs::symlink_metadata(root), "inspect root", root)?;
        check_metadata(root, &meta, true, owner, meta.dev())?;
        let files = Self {
            root: root.to_path_buf(),
            state: root.join("target/.gate-state"),
            owner,
            device: meta.dev(),
            root_id: identity(&meta),
            kind,
        };
        files.check_parents()?;
        for leaf in files.leaves() {
            files.check_leaf(*leaf)?;
        }
        Ok(files)
    }

    fn path(&self, leaf: Leaf) -> PathBuf {
        let name = match (self.kind, leaf) {
            (Kind::Warm, Leaf::Marker) => "warm.use",
            (Kind::Cold, Leaf::Marker) => "cold.use",
            (_, Leaf::Record) => "warm.record",
            (_, Leaf::Temporary) => "warm.record.tmp",
        };
        self.state.join(name)
    }

    fn leaves(&self) -> &'static [Leaf] {
        match self.kind {
            Kind::Warm => &[Leaf::Marker, Leaf::Record, Leaf::Temporary],
            Kind::Cold => &[Leaf::Marker],
        }
    }

    fn check_directory(&self, path: &Path, absent_ok: bool) -> Result<Option<Metadata>> {
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                check_metadata(path, &meta, true, self.owner, self.device)?;
                Ok(Some(meta))
            }
            Err(e) if absent_ok && e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => at(Err(e), "inspect directory", path),
        }
    }

    fn check_parents(&self) -> Result<()> {
        let root = self
            .check_directory(&self.root, false)?
            .ok_or_else(|| boundary(&self.root, "missing root"))?;
        if identity(&root) != self.root_id {
            return Err(boundary(&self.root, "root replaced"));
        }
        // Sequential component checks never inspect a leaf through a known link.
        // They are not a replacement for atomic descriptor-relative operations.
        let target = self.root.join("target");
        if self.check_directory(&target, true)?.is_some() {
            self.check_directory(&self.state, true)?;
        }
        Ok(())
    }

    fn check_target(&self) -> Result<bool> {
        self.check_parents()?;
        let name = if self.kind == Kind::Warm {
            "gate-warm"
        } else {
            "gate"
        };
        Ok(self
            .check_directory(&self.root.join("target").join(name), true)?
            .is_some())
    }

    fn check_leaf(&self, leaf: Leaf) -> Result<Option<Metadata>> {
        if self.kind == Kind::Cold && !matches!(leaf, Leaf::Marker) {
            return Err(Fault::Unavailable(
                "cold target has no completed record or temporary",
            ));
        }
        self.check_parents()?;
        let path = self.path(leaf);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                check_metadata(&path, &meta, false, self.owner, self.device)?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => at(Err(e), "inspect state leaf", &path),
        }
    }

    fn open_regular(&self, leaf: Leaf, write: bool, exclusive: bool) -> Result<File> {
        self.check_leaf(leaf)?;
        native::Boundary::state(self)?.open_regular(leaf, write, exclusive)
    }

    fn read_record(&self) -> Result<Stored> {
        let Some(before) = self.check_leaf(Leaf::Record)? else {
            return Ok(Stored::Missing);
        };
        let file = match self.open_regular(Leaf::Record, false, false) {
            Ok(file) => file,
            Err(error @ Fault::Io { .. }) => {
                return self.unreadable_record(error, identity(&before));
            }
            Err(error) => return Err(error),
        };
        let opened = at(
            file.metadata(),
            "fstat record before read",
            &self.path(Leaf::Record),
        )?;
        let mut bytes = Vec::new();
        if let Err(error) = at(
            (&file)
                .take((MAX_RECORD + 1) as u64)
                .read_to_end(&mut bytes),
            "read completed record",
            &self.path(Leaf::Record),
        ) {
            return self.unreadable_record(error, identity(&opened));
        }
        // Recheck the structural boundary even if bytes are malformed.
        let now = self
            .check_leaf(Leaf::Record)?
            .ok_or_else(|| boundary(&self.path(Leaf::Record), "record disappeared during read"))?;
        if identity(&opened) != identity(&now) {
            return Err(boundary(
                &self.path(Leaf::Record),
                "record replaced during read",
            ));
        }
        Ok(match Record::decode(&bytes) {
            Ok(record) => Stored::Complete(record),
            Err(_) => Stored::Damaged,
        })
    }

    fn unreadable_record(&self, error: Fault, expected: Identity) -> Result<Stored> {
        // Recheck on both open and read errors: ELOOP/special-file substitution
        // must not be disguised as corrupt content. This is still a pathname
        // recheck, not atomic confinement against concurrent namespace changes.
        let path = self.path(Leaf::Record);
        let now = self
            .check_leaf(Leaf::Record)?
            .ok_or_else(|| boundary(&path, "record disappeared after I/O failure"))?;
        if identity(&now) != expected {
            return Err(boundary(&path, "record replaced after I/O failure"));
        }
        match error {
            Fault::Io {
                operation,
                path,
                source,
            } => Ok(Stored::Unreadable {
                operation,
                path,
                errno: source.raw_os_error(),
                detail: source.to_string(),
            }),
            other => Err(other),
        }
    }

    fn create_temporary(&self, bytes: &[u8]) -> Result<File> {
        self.check_leaf(Leaf::Temporary)?;
        if bytes.len() > MAX_RECORD {
            return Err(Fault::Codec("record byte limit"));
        }
        let mut file = self.open_regular(Leaf::Temporary, true, true)?;
        at(
            file.write_all(bytes),
            "write temporary record",
            &self.path(Leaf::Temporary),
        )?;
        Ok(file)
    }

    fn sync_directory(&self) -> Result<()> {
        native::Boundary::state(self)?.sync_existing()
    }

    fn remove_leaf(&self, leaf: Leaf) -> Result<()> {
        self.check_leaf(leaf)?;
        native::Boundary::state(self)?.remove_leaf(leaf)
    }

    fn admit(&self, configured_lease_observed: bool) -> Result<Marker<'_>> {
        if !configured_lease_observed {
            return Err(Fault::Unavailable("configured lease not observed"));
        }
        native::Boundary::state(self)?;
        native::Boundary::create_directories(self)?;
        self.lock_marker_primitive()
    }

    fn build_name(&self) -> &'static str {
        match self.kind {
            Kind::Warm => "gate-warm",
            Kind::Cold => "gate",
        }
    }

    fn build_path(&self) -> PathBuf {
        self.root.join("target").join(self.build_name())
    }

    fn lock_marker_primitive(&self) -> Result<Marker<'_>> {
        // Acquire exactly the existing inode, or create it exclusively. Closing
        // this File must never explicitly unlock descriptions inherited by children.
        let found = self.check_leaf(Leaf::Marker)?.is_some();
        let file = self.open_regular(Leaf::Marker, true, !found)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Fault::Busy),
            Err(TryLockError::Error(e)) => {
                return at(
                    Err(e),
                    "try marker OFD-class lock",
                    &self.path(Leaf::Marker),
                );
            }
        }
        let marker = Marker {
            files: self,
            lease: None,
            file,
            found,
            temporary: None,
            phase: Phase::Locked,
            signals: std::cell::RefCell::new(None),
            finishing_interrupted: std::cell::Cell::new(false),
        };
        marker.check_identity()?; // Immediately after grant, before target inspection.
        Ok(marker)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Locked,
    Protected,
    Invalidated,
    Removed,
    Failed,
}

pub(super) struct Marker<'a> {
    files: &'a StateFiles,
    lease: Option<&'a super::lease::Lease>,
    file: File,
    found: bool,
    temporary: Option<File>,
    phase: Phase,
    // Keep cancellation alive through the publication sequence, after Runner
    // has relinquished its immutable borrow.
    signals: std::cell::RefCell<Option<super::signals::Signals>>,
    finishing_interrupted: std::cell::Cell<bool>,
}

// No Drop implementation: drop closes owned Files, NEVER unlocks or unlinks.
// command_descriptor duplicates the same open description on an extra inherited
// fd. Runner owns the duplicate only across spawn; stdin remains caller-owned.
impl Marker<'_> {
    pub(super) fn enable_signals(&self) -> anyhow::Result<()> {
        let mut signals = self.signals.borrow_mut();
        if signals.is_none() {
            *signals = Some(super::signals::Signals::new()?);
        }
        Ok(())
    }

    pub(super) fn signal_wait(&self, delay: std::time::Duration) -> Option<i32> {
        self.signals
            .borrow_mut()
            .as_mut()
            .and_then(|s| s.wait(delay))
    }

    pub(super) fn signal_pause(&self, delay: std::time::Duration) {
        if let Some(signals) = self.signals.borrow().as_ref() {
            signals.pause(delay);
        }
    }

    pub(super) fn check_identity(&self) -> Result<()> {
        if let Some(lease) = self.lease {
            lease
                .validate_exclusive()
                .map_err(|_| Fault::Unavailable("configured lease lost; keep marker and target"))?;
        }
        if self.signal_wait(std::time::Duration::ZERO).is_some()
            && !self.finishing_interrupted.get()
        {
            return Err(Fault::Unavailable("INTERRUPTED; keep marker and target"));
        }
        native::Boundary::state(self.files)?;
        let held = at(
            self.file.metadata(),
            "fstat locked marker",
            &self.files.path(Leaf::Marker),
        )?;
        match self.files.check_leaf(Leaf::Marker)? {
            Some(now) if identity(&held) == identity(&now) => check_metadata(
                &self.files.path(Leaf::Marker),
                &held,
                false,
                self.files.owner,
                self.files.device,
            ),
            _ => Err(Fault::MarkerChanged),
        }
    }

    #[cfg(test)]
    pub(super) fn qualification_unlock(&self) -> Result<()> {
        self.check_identity()?;
        at(
            self.file.unlock(),
            "intentional shared-description unlock discriminator",
            &self.files.path(Leaf::Marker),
        )
    }

    pub(super) fn command_descriptor(&self) -> Result<File> {
        self.check_identity()?;
        let file = at(
            self.file.try_clone(),
            "duplicate marker for direct command",
            &self.files.path(Leaf::Marker),
        )?;
        // Only this short-lived duplicate is inherited. The marker and lease
        // keep CLOEXEC, and no shared-description unlock is ever performed.
        at(
            nix::fcntl::fcntl(
                &file,
                nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()),
            )
            .map_err(io::Error::from),
            "inherit marker for direct command",
            &self.files.path(Leaf::Marker),
        )?;
        Ok(file)
    }

    fn diagnostics(&mut self) -> Result<Vec<u8>> {
        // Never open a second descriptor of the marker, even for diagnostics.
        self.check_identity()?;
        let path = self.files.path(Leaf::Marker);
        at(
            self.file.seek(SeekFrom::Start(0)),
            "seek marker diagnostic",
            &path,
        )?;
        let mut bytes = Vec::new();
        at(
            (&mut self.file)
                .take((MAX_DIAGNOSTIC + 1) as u64)
                .read_to_end(&mut bytes),
            "read marker diagnostic",
            &path,
        )?;
        if bytes.len() > MAX_DIAGNOSTIC {
            return Err(Fault::Unavailable("marker diagnostics exceed limit"));
        }
        Ok(bytes)
    }

    fn write_diagnostics(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_DIAGNOSTIC {
            return Err(Fault::Unavailable("marker diagnostics exceed limit"));
        }
        self.check_identity()?;
        let path = self.files.path(Leaf::Marker);
        at(
            self.file.seek(SeekFrom::Start(0)),
            "seek checked marker",
            &path,
        )?;
        at(self.file.write_all(bytes), "write checked marker", &path)?;
        at(
            self.file.set_len(bytes.len() as u64),
            "size checked marker",
            &path,
        )
    }

    fn protect(&mut self) -> Result<()> {
        if self.phase != Phase::Locked {
            return Err(Fault::Unavailable("invalid protect order"));
        }
        protect_order(self)?;
        self.phase = Phase::Protected;
        Ok(())
    }

    fn invalidate(&mut self) -> Result<()> {
        if self.phase != Phase::Protected {
            return Err(Fault::Unavailable("invalid invalidation order"));
        }
        // F2 inserts its structurally checked target decision after protect and
        // before invalidate. These are state operations, not recursive deletion.
        let kind = self.files.kind;
        invalidate_order(self, kind)?;
        self.phase = Phase::Invalidated;
        Ok(())
    }

    fn finish_warm(
        &mut self,
        finished: Finished,
        before: &Sources,
        current: Current<'_>,
        end_ns: i128,
    ) -> Result<Publication> {
        if self.phase != Phase::Invalidated || self.files.kind != Kind::Warm {
            return Err(Fault::Unavailable(
                "warm finish before invalidation or for cold target",
            ));
        }
        if !finished.direct_reaped || !finished.lease_held || !finished.inherited_holder_valid {
            self.phase = Phase::Failed;
            return Err(Fault::Unavailable(
                "use not finished; keep marker and target",
            ));
        }
        let result = self.finish_warm_inner(before, current, end_ns);
        if result.is_err() {
            self.phase = Phase::Failed;
        }
        result
    }

    fn finish_warm_inner(
        &mut self,
        before: &Sources,
        current: Current<'_>,
        end_ns: i128,
    ) -> Result<Publication> {
        self.check_identity()?;
        let verdict = publication(before, current.sources, current.boot);
        let encoded = if verdict == Publication::Record {
            let boot = current
                .boot
                .ok_or(Fault::Unavailable("current boot identity unavailable"))?;
            Some(
                Record::new(
                    self.files.root.as_os_str().as_bytes().to_vec(),
                    current.toolchain.to_owned(),
                    boot,
                    end_ns,
                    current.sources,
                )?
                .encode()?,
            )
        } else {
            None
        };
        finish_order(self, encoded.as_deref())?;
        Ok(verdict)
    }

    fn finish_cold(&mut self, finished: Finished) -> Result<()> {
        // F2 first removes the cold target (or keeps it on --keep), with its
        // structural checks and reviewed removal mechanism. No warm leaf is used.
        if self.phase != Phase::Invalidated || self.files.kind != Kind::Cold {
            return Err(Fault::Unavailable(
                "cold finish before invalidation or for warm target",
            ));
        }
        if !finished.direct_reaped || !finished.lease_held || !finished.inherited_holder_valid {
            self.phase = Phase::Failed;
            return Err(Fault::Unavailable(
                "use not finished; keep marker and target",
            ));
        }
        finish_order(self, None)
    }
}

struct Finished {
    direct_reaped: bool,
    lease_held: bool,
    inherited_holder_valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    MarkerSync,
    DirectorySync,
    RemoveTemporary,
    RemoveRecord,
    WriteTemporary,
    TemporarySync,
    RenameRecord,
    RemoveMarker,
}

trait StateIo {
    fn step(&mut self, step: Step, bytes: Option<&[u8]>) -> Result<()>;
}

// Shared phase sequences: production and the logical journal consume these
// same steps. Independent literal expected orders in tests guard missing syncs.
// Target decisions stay BETWEEN protect and invalidate; no combined begin phase
// can accidentally move that F2 boundary. Journal failures remain simulations.
const PROTECT_STEPS: &[Step] = &[Step::MarkerSync, Step::DirectorySync];
const WARM_INVALIDATE_STEPS: &[Step] = &[
    Step::RemoveTemporary,
    Step::RemoveRecord,
    Step::DirectorySync,
];
const COLD_INVALIDATE_STEPS: &[Step] = &[Step::DirectorySync];
const RECORD_FINISH_STEPS: &[Step] = &[
    Step::WriteTemporary,
    Step::TemporarySync,
    Step::RenameRecord,
    Step::DirectorySync,
    Step::RemoveMarker,
];
// No post-unlink directory sync: D44 permits resurrection of the marker on
// power loss (a rebuild). A failure after unlink could not retain that inode
// at its path. Close follows the final successful unlink.
const NO_RECORD_FINISH_STEPS: &[Step] = &[Step::RemoveMarker];

fn sequence(io: &mut impl StateIo, steps: &[Step], bytes: Option<&[u8]>) -> Result<()> {
    for &step in steps {
        io.step(
            step,
            if step == Step::WriteTemporary {
                bytes
            } else {
                None
            },
        )?;
    }
    Ok(())
}

fn protect_order(io: &mut impl StateIo) -> Result<()> {
    sequence(io, PROTECT_STEPS, None)
}

fn invalidate_order(io: &mut impl StateIo, kind: Kind) -> Result<()> {
    sequence(
        io,
        match kind {
            Kind::Warm => WARM_INVALIDATE_STEPS,
            Kind::Cold => COLD_INVALIDATE_STEPS,
        },
        None,
    )
}

fn finish_order(io: &mut impl StateIo, bytes: Option<&[u8]>) -> Result<()> {
    sequence(
        io,
        if bytes.is_some() {
            RECORD_FINISH_STEPS
        } else {
            NO_RECORD_FINISH_STEPS
        },
        bytes,
    )
}

impl StateIo for Marker<'_> {
    fn step(&mut self, step: Step, bytes: Option<&[u8]>) -> Result<()> {
        if matches!(self.phase, Phase::Failed | Phase::Removed) {
            return Err(Fault::Unavailable("state operations ended; no retry"));
        }
        let result = self.perform_step(step, bytes);
        if result.is_err() {
            self.phase = Phase::Failed;
        }
        result
    }
}

impl Marker<'_> {
    fn perform_step(&mut self, step: Step, bytes: Option<&[u8]>) -> Result<()> {
        self.check_identity()?;
        match step {
            Step::MarkerSync => at(
                self.file.sync_all(),
                "synchronize marker",
                &self.files.path(Leaf::Marker),
            ),
            Step::DirectorySync => self.files.sync_directory(),
            Step::RemoveTemporary => self.files.remove_leaf(Leaf::Temporary),
            Step::RemoveRecord => self.files.remove_leaf(Leaf::Record),
            Step::WriteTemporary => {
                let bytes = bytes.ok_or(Fault::Codec("missing publication bytes"))?;
                self.temporary = Some(self.files.create_temporary(bytes)?);
                Ok(())
            }
            Step::TemporarySync => {
                let file = self
                    .temporary
                    .as_ref()
                    .ok_or(Fault::Codec("missing temporary descriptor"))?;
                at(
                    file.sync_all(),
                    "synchronize temporary record",
                    &self.files.path(Leaf::Temporary),
                )
            }
            Step::RenameRecord => {
                let file = self
                    .temporary
                    .as_ref()
                    .ok_or(Fault::Codec("missing temporary descriptor"))?;
                let held = at(
                    file.metadata(),
                    "fstat temporary",
                    &self.files.path(Leaf::Temporary),
                )?;
                let now = self.files.check_leaf(Leaf::Temporary)?.ok_or_else(|| {
                    boundary(&self.files.path(Leaf::Temporary), "temporary disappeared")
                })?;
                if identity(&held) != identity(&now) {
                    return Err(boundary(
                        &self.files.path(Leaf::Temporary),
                        "temporary replaced",
                    ));
                }
                if self.files.check_leaf(Leaf::Record)?.is_some() {
                    return Err(boundary(
                        &self.files.path(Leaf::Record),
                        "record appeared after invalidation",
                    ));
                }
                native::Boundary::state(self.files)?.rename_record(identity(&held))?;
                self.temporary = None;
                Ok(())
            }
            Step::RemoveMarker => {
                self.files.remove_leaf(Leaf::Marker)?;
                self.phase = Phase::Removed;
                Ok(())
            }
        }
    }
}

// Bind the configured authority to the marker so each publication step checks
// it again, including an inherited live holder. Diagnostics supply no authority.
impl StateFiles {
    fn admit_configured<'a>(
        &'a self,
        lease: Option<&'a super::lease::Lease>,
    ) -> anyhow::Result<Marker<'a>> {
        let lease = lease.ok_or_else(|| {
            anyhow::anyhow!("REFUSED: configured lease required; --no-lease cannot admit a target")
        })?;
        lease.validate_exclusive()?;
        let mut marker = self.admit(true)?;
        marker.lease = Some(lease);
        marker.check_identity()?;
        Ok(marker)
    }
}

#[derive(Debug)]
struct UseOutcome {
    exits: Vec<std::process::ExitStatus>,
    publication: Publication,
    end_ns: i128,
}

impl Marker<'_> {
    fn use_warm(
        &mut self,
        lease: &super::lease::Lease,
        commands: &mut [std::process::Command],
    ) -> anyhow::Result<UseOutcome> {
        // The caller must have completed protect, target decision, invalidate.
        // The borrow of self by Runner prevents a publication while it has a
        // live direct command. Capture failures retain the invalidated marker.
        anyhow::ensure!(
            self.phase == Phase::Invalidated && self.files.kind == Kind::Warm,
            "warm commands before protected target decision/invalidation"
        );
        self.enable_signals()?;
        let mut runner = super::steps::Runner::new(lease, self)?;
        let captures = (|| {
            let before = Captured::read(&mut runner, &self.files.root)?;
            for command in commands {
                if !runner.run(command)?.success() {
                    break;
                }
            }
            runner.finished()?;
            let after = Captured::read(&mut runner, &self.files.root)?;
            runner.finished()?;
            anyhow::ensure!(
                before.toolchain == after.toolchain && before.boot == after.boot,
                "toolchain/boot identity changed; keep marker, no publication"
            );
            let end_ns = i128::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
            )?;
            Ok((before, after, end_ns))
        })();
        let exits: Vec<_> = runner.statuses().iter().map(|s| s.exit).collect();
        let statuses = runner.statuses().to_vec();
        drop(runner);
        let result = (|| {
            let (before, after, end_ns) = captures?;
            // Re-check the inherited live holder at the publication boundary.
            lease.validate()?;
            let publication = self.finish_warm(
                Finished {
                    direct_reaped: true,
                    lease_held: true,
                    inherited_holder_valid: true,
                },
                &before.sources,
                Current {
                    root: self.files.root.as_os_str().as_bytes(),
                    toolchain: &after.toolchain,
                    boot: Some(after.boot),
                    sources: &after.sources,
                },
                end_ns,
            )?;
            Ok((publication, end_ns))
        })();
        match result {
            Ok((publication, end_ns)) => Ok(UseOutcome {
                exits,
                publication,
                end_ns,
            }),
            Err(source) => {
                self.phase = Phase::Failed;
                Err(UseFailure {
                    exits,
                    statuses,
                    source,
                }
                .into())
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct UseFailure {
    pub(super) exits: Vec<std::process::ExitStatus>,
    pub(super) statuses: Vec<super::steps::Status>,
    pub(super) source: anyhow::Error,
}
impl fmt::Display for UseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "target: kept ({}); qualifying: no; reaped statuses: {:?}",
            self.source, self.exits
        )
    }
}
impl std::error::Error for UseFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

struct Captured {
    sources: Sources,
    toolchain: String,
    boot: BootId,
}

impl Captured {
    fn read(runner: &mut super::steps::Runner<'_, '_>, root: &Path) -> anyhow::Result<Self> {
        use std::ffi::OsStr;
        use std::process::Command;
        fn inventory(
            runner: &mut super::steps::Runner<'_, '_>,
            root: &Path,
        ) -> anyhow::Result<Vec<Vec<u8>>> {
            let bytes = runner.capture(
                Command::new("git").current_dir(root).args([
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                ]),
                MAX_SOURCE_BYTES,
            )?;
            anyhow::ensure!(
                bytes.is_empty() || bytes.last() == Some(&0),
                "incomplete git source inventory"
            );
            let mut paths: Vec<Vec<u8>> = if bytes.is_empty() {
                Vec::new()
            } else {
                bytes[..bytes.len() - 1]
                    .split(|b| *b == 0)
                    .map(<[u8]>::to_vec)
                    .collect()
            };
            anyhow::ensure!(
                paths.len() <= MAX_SOURCES,
                "source count exceeds record budget"
            );
            paths.sort();
            anyhow::ensure!(
                paths.windows(2).all(|p| p[0] != p[1]),
                "duplicate source inventory"
            );
            for path in &paths {
                source_path(path)?;
            }
            // A tracked deletion is part of the current source set, not a
            // missing-file I/O error. Query Git explicitly so a vanished
            // untracked file/racing parent still fails the capture checks.
            let deleted = runner.capture(
                Command::new("git")
                    .current_dir(root)
                    .args(["ls-files", "--deleted", "-z"]),
                MAX_SOURCE_BYTES,
            )?;
            anyhow::ensure!(
                deleted.is_empty() || deleted.last() == Some(&0),
                "incomplete deleted source inventory"
            );
            let deleted: std::collections::BTreeSet<_> = deleted
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(<[u8]>::to_vec)
                .collect();
            for path in &deleted {
                source_path(path)?;
            }
            paths.retain(|p| !deleted.contains(p));
            Ok(paths)
        }
        let paths = inventory(runner, root)?;
        let mut entries = Vec::new();
        for chunk in paths.chunks(32) {
            let mut command = Command::new("git");
            command
                .current_dir(root)
                .args(["hash-object", "--no-filters", "--"]);
            let mut metadata = Vec::new();
            for path in chunk {
                let path = Path::new(OsStr::from_bytes(path));
                let full = root.join(path);
                // Check every source component before hashing. Symlinks and
                // submodules are refused rather than following a source outside
                // the root. Richer source kinds need an explicit capture design.
                let mut component = root.to_path_buf();
                for part in path.components() {
                    component.push(part);
                    let m = fs::symlink_metadata(&component)?;
                    anyhow::ensure!(
                        !m.file_type().is_symlink(),
                        "symbolic source component: {}",
                        component.display()
                    );
                }
                let m = fs::symlink_metadata(&full)?;
                anyhow::ensure!(m.is_file(), "non-regular source: {}", full.display());
                metadata.push(m);
                command.arg(path);
            }
            // argv is bounded in small batches and every path follows --.
            // stdin is left at its ordinary configuration.
            let hashes = runner.capture(&mut command, 65 * chunk.len())?;
            let hashes = std::str::from_utf8(&hashes)?;
            let hashes: Vec<_> = hashes.lines().collect();
            anyhow::ensure!(
                hashes.len() == chunk.len(),
                "incomplete content identity capture"
            );
            for ((path, before), hash) in chunk.iter().zip(metadata).zip(hashes) {
                let full = root.join(OsStr::from_bytes(path));
                let after = fs::symlink_metadata(&full)?;
                anyhow::ensure!(
                    after.is_file()
                        && identity(&before) == identity(&after)
                        && before.len() == after.len()
                        && before.mtime() == after.mtime()
                        && before.mtime_nsec() == after.mtime_nsec(),
                    "source changed during capture"
                );
                entries.push(Source {
                    path: path.clone(),
                    content: hash.to_owned(),
                    modified_ns: Some(
                        i128::from(after.mtime()) * 1_000_000_000 + i128::from(after.mtime_nsec()),
                    ),
                });
            }
        }
        anyhow::ensure!(
            paths == inventory(runner, root)?,
            "source set changed during capture"
        );
        let toolchain = runner.capture(
            Command::new("rustc").current_dir(root).arg("-vV"),
            MAX_FIELD,
        )?;
        let toolchain = String::from_utf8(toolchain)?;
        anyhow::ensure!(!toolchain.trim().is_empty(), "empty toolchain identity");
        #[cfg(target_os = "macos")]
        let boot = runner.capture(
            Command::new("/usr/sbin/sysctl").args(["-n", "kern.bootsessionuuid"]),
            128,
        )?;
        #[cfg(target_os = "linux")]
        let boot = fs::read("/proc/sys/kernel/random/boot_id")?;
        let boot = BootId::parse(std::str::from_utf8(&boot)?.trim())?;
        Ok(Self {
            sources: Sources::new(entries)?,
            toolchain,
            boot,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn boot() -> BootId {
        BootId::parse("58d23266-5946-4fca-99c7-36f775e61eaa").unwrap()
    }

    fn sources() -> Sources {
        Sources::new(vec![Source {
            path: b"src/main.rs".to_vec(),
            content: "a".repeat(40),
            modified_ns: Some(100),
        }])
        .unwrap()
    }

    fn record() -> Record {
        Record::new(
            b"/work/repo".to_vec(),
            "rust-1.99.1".into(),
            boot(),
            200,
            &sources(),
        )
        .unwrap()
    }

    fn context<'a>(sources: &'a Sources) -> Current<'a> {
        Current {
            root: b"/work/repo",
            toolchain: "rust-1.99.1",
            boot: Some(boot()),
            sources,
        }
    }

    fn observation() -> Admission {
        Admission {
            lease: true,
            structure: true,
            marker_found: false,
            lock_granted: true,
            marker_same: true,
            target_exists: true,
            kind: Kind::Warm,
        }
    }

    #[test]
    fn cold_free_space_refuses_low_missing_or_invalid_portable_df_output() {
        assert!(cold_space("Filesystem 1024-blocks Used Available Capacity Mounted on\nfixture 99999999 1000 12582912 1% /fixture\n").is_ok());
        for text in [
            "",
            "header\nfixture 99999999 1 12582911 1% /fixture\n",
            "header\nfixture bad bad bad\n",
        ] {
            assert!(cold_space(text).is_err());
        }
    }

    #[test]
    fn same_boot_complete_record_reuses_existing_warm_target() {
        assert_eq!(
            decide(
                observation(),
                &Stored::Complete(record()),
                context(&sources())
            ),
            Action::Reuse,
        );
    }

    #[test]
    fn changed_root_toolchain_or_backdated_content_resets() {
        let s = sources();
        let r = Stored::Complete(record());
        let mut c = context(&s);
        c.root = b"/another/worktree";
        assert_eq!(decide(observation(), &r, c), Action::Reset(Reset::Worktree));
        let mut c = context(&s);
        c.toolchain = "different";
        assert_eq!(
            decide(observation(), &r, c),
            Action::Reset(Reset::Toolchain)
        );
        let mut changed = sources();
        changed
            .0
            .get_mut(b"src/main.rs".as_slice())
            .unwrap()
            .content = "b".repeat(40);
        assert_eq!(
            decide(observation(), &r, context(&changed)),
            Action::Reset(Reset::Content),
        );
        changed
            .0
            .get_mut(b"src/main.rs".as_slice())
            .unwrap()
            .modified_ns = Some(201);
        assert_eq!(decide(observation(), &r, context(&changed)), Action::Reuse);
    }

    #[test]
    fn missing_and_damaged_records_reset_separately() {
        assert_eq!(
            decide(observation(), &Stored::Missing, context(&sources())),
            Action::Reset(Reset::NoRecord),
        );
        assert_eq!(
            decide(observation(), &Stored::Damaged, context(&sources())),
            Action::Reset(Reset::Unreadable),
        );
    }

    #[test]
    fn absent_target_never_reuses_even_with_a_complete_record() {
        let mut o = observation();
        o.target_exists = false;
        assert_eq!(
            decide(o, &Stored::Complete(record()), context(&sources())),
            Action::Reset(Reset::NoRecord),
        );
    }

    #[test]
    fn prior_marker_dominates_record_and_partial_creation() {
        for exists in [false, true] {
            let mut o = observation();
            o.marker_found = true;
            o.target_exists = exists;
            assert_eq!(
                decide(o, &Stored::Complete(record()), context(&sources())),
                Action::Reset(Reset::Unfinished),
            );
            o.marker_same = false;
            assert_eq!(
                decide(o, &Stored::Complete(record()), context(&sources())),
                Action::Refuse(Refusal::MarkerChanged),
            );
        }
    }

    #[test]
    fn busy_marker_refuses_warm_and_cold_on_every_attempt() {
        for kind in [Kind::Warm, Kind::Cold] {
            let mut o = observation();
            o.kind = kind;
            o.marker_found = true;
            o.lock_granted = false;
            for _ in 0..3 {
                assert_eq!(
                    decide(o, &Stored::Complete(record()), context(&sources())),
                    Action::Refuse(Refusal::Busy),
                );
            }
        }
    }

    #[test]
    fn cold_always_rebuilds_but_never_bypasses_busy_marker() {
        let mut o = observation();
        o.kind = Kind::Cold;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Cold
        );
        o.marker_found = true;
        o.lock_granted = false;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Refuse(Refusal::Busy),
        );
        o.lock_granted = true;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Cold
        );
    }

    #[test]
    fn structural_lease_and_identity_failures_precede_decisions() {
        let mut o = observation();
        o.lease = false;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Refuse(Refusal::Lease),
        );
        o = observation();
        o.structure = false;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Refuse(Refusal::Structure),
        );
        o = observation();
        o.marker_same = false;
        assert_eq!(
            decide(o, &Stored::Complete(record()), context(&sources())),
            Action::Refuse(Refusal::MarkerChanged),
        );
    }

    #[test]
    fn boot_missing_invalid_or_cross_boot_cannot_reuse_or_publish() {
        for text in ["", "not-a-boot", "00000000-0000-0000-0000-000000000000"] {
            assert!(BootId::parse(text).is_err());
        }
        let s = sources();
        let r = Stored::Complete(record());
        let mut c = context(&s);
        c.boot = None;
        assert_eq!(decide(observation(), &r, c), Action::Reset(Reset::Boot));
        assert_eq!(publication(&s, &s, None), Publication::NoBoot);
        c = context(&s);
        c.boot = Some(BootId::parse("a03520cf-0d33-4822-9ba1-2292769d23cb").unwrap());
        // Comparator-bypass discriminator: removing the equality check makes this Reuse.
        assert_eq!(decide(observation(), &r, c), Action::Reset(Reset::Boot));
        let mut bytes = record().encode().unwrap();
        bytes[BOOT_OFFSET..BOOT_OFFSET + 16].fill(0);
        assert!(Record::decode(&bytes).is_err());
    }

    #[test]
    fn every_truncation_oversize_and_trailing_byte_is_damaged() {
        let r = record();
        let encoded = r.encode().unwrap();
        assert_eq!(Record::decode(&encoded).unwrap(), r);
        for cut in 0..encoded.len() {
            assert!(Record::decode(&encoded[..cut]).is_err(), "cut={cut}");
        }
        let mut extra = encoded;
        extra.push(0);
        assert!(Record::decode(&extra).is_err());
        assert!(Record::decode(&vec![0; MAX_RECORD + 1]).is_err());
    }

    #[test]
    fn invalid_schema_misaligned_frame_and_field_count_limits_are_damaged() {
        let encoded = record().encode().unwrap();
        let mut bad_schema = encoded.clone();
        bad_schema[0] ^= 1;
        assert!(Record::decode(&bad_schema).is_err());
        // Removing a positional field misaligns the frame; nil-boot validation
        // is discriminated separately by boot_missing_invalid_or_cross_boot.
        let mut misaligned = encoded.clone();
        misaligned.drain(BOOT_OFFSET..BOOT_OFFSET + 16);
        let length = (misaligned.len() as u32).to_be_bytes();
        misaligned[8..12].copy_from_slice(&length);
        assert!(Record::decode(&misaligned).is_err());
        let mut bad_field = encoded.clone();
        bad_field[44..48].copy_from_slice(&((MAX_FIELD + 1) as u32).to_be_bytes());
        assert!(Record::decode(&bad_field).is_err());
        let count_offset = 44 + 4 + b"/work/repo".len() + 4 + "rust-1.99.1".len();
        let mut bad_count = encoded;
        bad_count[count_offset..count_offset + 4]
            .copy_from_slice(&((MAX_SOURCES + 1) as u32).to_be_bytes());
        assert!(Record::decode(&bad_count).is_err());
        let mut too_long = record();
        too_long.toolchain = "x".repeat(MAX_FIELD + 1);
        assert!(too_long.encode().is_err());
    }

    #[test]
    fn omitted_old_source_and_deleted_listed_source_reset() {
        let mut current = sources();
        current.0.insert(
            b"old.rs".to_vec(),
            Source {
                path: b"old.rs".to_vec(),
                content: "b".repeat(40),
                modified_ns: Some(200),
            },
        );
        assert_eq!(
            decide(
                observation(),
                &Stored::Complete(record()),
                context(&current)
            ),
            Action::Reset(Reset::Content)
        );
        current.0.get_mut(b"old.rs".as_slice()).unwrap().modified_ns = Some(201);
        assert_eq!(
            decide(
                observation(),
                &Stored::Complete(record()),
                context(&current)
            ),
            Action::Reuse
        );
        let empty = Sources::new(vec![]).unwrap();
        assert_eq!(
            decide(observation(), &Stored::Complete(record()), context(&empty)),
            Action::Reset(Reset::Content)
        );
    }

    #[test]
    fn duplicate_unsafe_paths_and_invalid_content_are_not_complete_sources() {
        let s = sources().0.into_values().next().unwrap();
        assert!(Sources::new(vec![s.clone(), s.clone()]).is_err());
        for path in [
            b"../outside".as_slice(),
            b"/absolute",
            b"a/../b",
            b"a//b",
            b"a\0b",
            b".",
        ] {
            let mut invalid = s.clone();
            invalid.path = path.to_vec();
            assert!(Sources::new(vec![invalid]).is_err());
        }
        let mut invalid = s;
        invalid.content = "not a blob".into();
        assert!(Sources::new(vec![invalid]).is_err());
    }

    #[test]
    fn publication_compares_every_content_and_both_source_sets() {
        let a = sources();
        assert_eq!(publication(&a, &a, Some(boot())), Publication::Record);
        let mut changed = sources();
        changed
            .0
            .get_mut(b"src/main.rs".as_slice())
            .unwrap()
            .content = "b".repeat(40);
        assert_eq!(
            publication(&a, &changed, Some(boot())),
            Publication::SourceChanged
        );
        let empty = Sources::new(vec![]).unwrap();
        assert_eq!(
            publication(&a, &empty, Some(boot())),
            Publication::SourceChanged
        );
        assert_eq!(
            publication(&empty, &a, Some(boot())),
            Publication::SourceChanged
        );
    }

    #[test]
    fn removal_states_characterize_the_disclosed_manual_removal_residual() {
        // State simulations only: no cargo clean, crash or live writer is proved.
        let mut o = observation();
        o.target_exists = false;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Reset(Reset::NoRecord)
        );
        o.target_exists = true;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Reset(Reset::NoRecord)
        );
        o.target_exists = false;
        o.marker_found = true;
        assert_eq!(
            decide(o, &Stored::Complete(record()), context(&sources())),
            Action::Reset(Reset::Unfinished)
        );
        o.lock_granted = false;
        assert_eq!(
            decide(o, &Stored::Complete(record()), context(&sources())),
            Action::Refuse(Refusal::Busy)
        );
    }

    #[test]
    fn crash_prefixes_never_reuse_an_old_record_without_a_marker() {
        // Logical crash prefixes; these do not prove stable storage or a live death.
        let mut io = Journal::default();
        protect_order(&mut io).unwrap();
        invalidate_order(&mut io, Kind::Warm).unwrap();
        finish_order(&mut io, Some(b"new-record")).unwrap();
        assert_eq!(
            io.events,
            [
                Step::MarkerSync,
                Step::DirectorySync,
                Step::RemoveTemporary,
                Step::RemoveRecord,
                Step::DirectorySync,
                Step::WriteTemporary,
                Step::TemporarySync,
                Step::RenameRecord,
                Step::DirectorySync,
                Step::RemoveMarker
            ]
        );
        let mut marker = true;
        let mut old_record = true;
        for event in &io.events {
            match event {
                Step::RemoveRecord => old_record = false,
                Step::RemoveMarker => marker = false,
                _ => {}
            }
            assert!(marker || !old_record);
        }
    }

    #[test]
    fn each_ordering_error_stops_once_and_retains_marker() {
        let mut reference = Journal::default();
        protect_order(&mut reference).unwrap();
        invalidate_order(&mut reference, Kind::Warm).unwrap();
        finish_order(&mut reference, Some(b"new")).unwrap();
        for fail in 0..reference.events.len() {
            let mut io = Journal {
                fail: Some(fail),
                ..Journal::default()
            };
            let result = protect_order(&mut io)
                .and_then(|()| invalidate_order(&mut io, Kind::Warm))
                .and_then(|()| finish_order(&mut io, Some(b"new")));
            assert!(result.is_err());
            assert_eq!(io.events.len(), fail + 1);
            assert!(!io.events[..fail].contains(&Step::RemoveMarker));
        }
    }

    #[derive(Default)]
    struct Journal {
        events: Vec<Step>,
        fail: Option<usize>,
    }

    impl StateIo for Journal {
        fn step(&mut self, step: Step, _bytes: Option<&[u8]>) -> Result<()> {
            self.events.push(step);
            if self.fail == Some(self.events.len() - 1) {
                return Err(Fault::Unavailable("injected I/O failure"));
            }
            Ok(())
        }
    }

    struct Fixture {
        base: PathBuf,
        files: StateFiles,
        outside: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let private = std::env::var_os("XTASK_F1_PRIVATE_ROOT").map(PathBuf::from);
            Self::with_root(private.as_deref())
        }

        fn with_root(private: Option<&Path>) -> Self {
            let parent = match private {
                Some(path) => {
                    assert!(path.is_absolute(), "fixture override must be absolute");
                    let meta = fs::symlink_metadata(path).unwrap();
                    assert!(meta.is_dir(), "fixture override must be a real directory");
                    assert_eq!(
                        meta.mode() & 0o7777,
                        0o700,
                        "fixture override must be private"
                    );
                    path.canonicalize().unwrap()
                }
                None => std::env::temp_dir().canonicalize().unwrap(),
            };
            let base = fresh_case(&parent);
            let meta = fs::symlink_metadata(&base).unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.mode() & 0o7777, 0o700);
            let root = base.join("r");
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("target"))
                .unwrap();
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("target/.gate-state"))
                .unwrap();
            let root = root.canonicalize().unwrap();
            let owner = fs::symlink_metadata(&root).unwrap().uid();
            let files = StateFiles::inspect(&root, owner, Kind::Warm).unwrap();
            let outside = base.join("outside-canary");
            fs::write(&outside, b"outside stays unchanged").unwrap();
            Self {
                base,
                files,
                outside,
            }
        }

        fn canary_unchanged(&self) {
            assert_eq!(fs::read(&self.outside).unwrap(), b"outside stays unchanged");
        }
    }

    fn fresh_case(parent: &Path) -> PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        // create, never create_dir_all: a collision is never adopted or cleaned.
        // Short hex names and the one-character repo name leave room for the
        // socket boundary plant under an ordinary macOS temporary directory.
        // PID is just a name, not authority or a boot observation.
        for _ in 0..128 {
            let path = parent.join(format!(
                "f1-{:x}-{:x}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return path,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private fixture {}: {error}", path.display()),
            }
        }
        panic!("exclusive fixture creation exhausted its finite name attempts");
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // No child exists. Retain evidence on panic instead of hiding the failed boundary.
            if !std::thread::panicking() {
                fs::remove_dir_all(&self.base).unwrap();
            }
        }
    }

    fn boundary_at(result: Result<()>, expected: &Path) {
        match result {
            Err(Fault::Boundary { path, .. }) => assert_eq!(path, expected),
            other => panic!("expected boundary refusal at {expected:?}, got {other:?}"),
        }
    }

    #[test]
    fn marker_symlink_is_refused_at_marker_boundary_without_touching_canary() {
        let f = Fixture::new();
        let path = f.files.path(Leaf::Marker);
        symlink(&f.outside, &path).unwrap();
        boundary_at(f.files.lock_marker_primitive().map(|_| ()), &path);
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        f.canary_unchanged();
    }

    #[test]
    fn record_socket_is_refused_before_open_or_parse() {
        let f = Fixture::new();
        let path = f.files.path(Leaf::Record);
        // Socket is the RC17 'another special file'; no mkfifo subprocess or unsafe FFI.
        let socket = UnixListener::bind(&path).unwrap();
        boundary_at(f.files.read_record().map(|_| ()), &path);
        f.canary_unchanged();
        drop(socket);
    }

    #[test]
    fn temporary_symlink_is_refused_at_temporary_boundary_without_replacement() {
        let f = Fixture::new();
        let path = f.files.path(Leaf::Temporary);
        symlink(&f.outside, &path).unwrap();
        boundary_at(f.files.create_temporary(b"not written").map(|_| ()), &path);
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        f.canary_unchanged();
    }

    #[test]
    fn state_and_target_directory_links_are_refused_without_removal() {
        for leaf in ["target/.gate-state", "target/gate-warm"] {
            let f = Fixture::new();
            let path = f.files.root.join(leaf);
            if path.exists() {
                fs::remove_dir(&path).unwrap();
            }
            symlink(&f.base, &path).unwrap();
            boundary_at(f.files.check_directory(&path, false).map(|_| ()), &path);
            f.canary_unchanged();
        }
    }

    #[test]
    fn target_parent_link_is_refused_before_any_state_leaf_is_opened() {
        let f = Fixture::new();
        let target = f.files.root.join("target");
        fs::rename(&target, f.base.join("saved-target")).unwrap();
        symlink(&f.base, &target).unwrap();
        boundary_at(f.files.check_leaf(Leaf::Marker).map(|_| ()), &target);
        f.canary_unchanged();
    }

    #[test]
    fn known_regular_leaves_and_absent_target_pass_structural_checks() {
        let f = Fixture::new();
        for leaf in [Leaf::Marker, Leaf::Record, Leaf::Temporary] {
            let file = f.files.open_regular(leaf, true, true).unwrap();
            drop(file);
            assert!(f.files.check_leaf(leaf).unwrap().is_some());
        }
        assert!(!f.files.check_target().unwrap());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(f.files.root.join("target/gate-warm"))
            .unwrap();
        assert!(f.files.check_target().unwrap());
        f.canary_unchanged();
    }

    #[test]
    fn wrong_owner_device_permissions_and_hardlinks_fail_structural_checks() {
        let f = Fixture::new();
        let path = f.files.path(Leaf::Record);
        fs::write(&path, b"damaged content").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();
        boundary_at(
            check_metadata(
                &path,
                &meta,
                false,
                f.files.owner.wrapping_add(1),
                f.files.device,
            ),
            &path,
        );
        boundary_at(
            check_metadata(
                &path,
                &meta,
                false,
                f.files.owner,
                f.files.device.wrapping_add(1),
            ),
            &path,
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        boundary_at(f.files.read_record().map(|_| ()), &path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, f.base.join("alias")).unwrap();
        boundary_at(f.files.read_record().map(|_| ()), &path);
        f.canary_unchanged();
    }

    #[test]
    fn checked_exclusive_regular_opens_preserve_existing_contents() {
        let f = Fixture::new();
        let file = f.files.open_regular(Leaf::Temporary, true, true).unwrap();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o600);
        drop(file);
        fs::write(f.files.path(Leaf::Temporary), b"existing").unwrap();
        assert!(f.files.open_regular(Leaf::Temporary, true, true).is_err());
        assert_eq!(
            fs::read(f.files.path(Leaf::Temporary)).unwrap(),
            b"existing"
        );
        f.canary_unchanged();
    }

    #[test]
    fn regular_damaged_and_oversize_records_reset_without_structural_refusal() {
        let f = Fixture::new();
        let path = f.files.path(Leaf::Record);
        let mut file = f.files.open_regular(Leaf::Record, true, true).unwrap();
        file.write_all(b"broken").unwrap();
        drop(file);
        assert_eq!(f.files.read_record().unwrap(), Stored::Damaged);
        fs::write(&path, vec![0; MAX_RECORD + 1]).unwrap();
        assert_eq!(f.files.read_record().unwrap(), Stored::Damaged);
        f.canary_unchanged();
    }

    #[test]
    fn temporary_record_is_never_a_completed_record_fallback() {
        let f = Fixture::new();
        let temporary = f
            .files
            .create_temporary(&record().encode().unwrap())
            .unwrap();
        drop(temporary);
        assert_eq!(f.files.read_record().unwrap(), Stored::Missing);
        f.canary_unchanged();
    }

    #[test]
    fn cold_state_has_no_access_to_warm_record_leaves() {
        let mut f = Fixture::new();
        let warm_record = f.files.path(Leaf::Record);
        let mut file = f.files.open_regular(Leaf::Record, true, true).unwrap();
        file.write_all(b"warm record is separate").unwrap();
        drop(file);
        f.files.kind = Kind::Cold;
        assert!(matches!(f.files.read_record(), Err(Fault::Unavailable(_))));
        assert!(matches!(
            f.files.remove_leaf(Leaf::Record),
            Err(Fault::Unavailable(_))
        ));
        assert_eq!(fs::read(warm_record).unwrap(), b"warm record is separate");
        f.canary_unchanged();
    }

    #[test]
    fn unlinked_and_recreated_marker_inodes_fail_admission_identity() {
        for recreate in [false, true] {
            let f = Fixture::new();
            // Primitive control, not native admission, lease validation or RC18 evidence.
            let held = f.files.lock_marker_primitive().unwrap();
            let path = f.files.path(Leaf::Marker);
            fs::remove_file(&path).unwrap();
            let replacement =
                recreate.then(|| f.files.open_regular(Leaf::Marker, true, true).unwrap());
            assert!(matches!(held.check_identity(), Err(Fault::MarkerChanged)));
            assert!(!f.files.root.join("target/gate-warm").exists());
            f.canary_unchanged();
            drop(replacement);
            drop(held);
        }
    }

    #[test]
    fn marker_close_keeps_inode_and_diagnostics_are_bounded() {
        let f = Fixture::new();
        let mut held = f.files.lock_marker_primitive().unwrap();
        held.write_diagnostics(b"diagnostic only").unwrap();
        assert_eq!(held.diagnostics().unwrap(), b"diagnostic only");
        assert!(
            held.write_diagnostics(&vec![0; MAX_DIAGNOSTIC + 1])
                .is_err()
        );
        let id = identity(&held.file.metadata().unwrap());
        drop(held);
        assert_eq!(
            identity(&fs::symlink_metadata(f.files.path(Leaf::Marker)).unwrap()),
            id
        );
        f.canary_unchanged();
    }

    #[test]
    fn unfinished_observations_keep_marker_and_poison_future_publication() {
        for missing in 0..3 {
            let f = Fixture::new();
            let mut held = f.files.lock_marker_primitive().unwrap();
            // Phase simulation only; no command, lease, sync or reap is proved.
            held.phase = Phase::Invalidated;
            let finished = Finished {
                direct_reaped: missing != 0,
                lease_held: missing != 1,
                inherited_holder_valid: missing != 2,
            };
            assert!(
                held.finish_warm(finished, &sources(), context(&sources()), 300)
                    .is_err()
            );
            assert_eq!(held.phase, Phase::Failed);
            assert!(matches!(
                held.step(Step::RemoveMarker, None),
                Err(Fault::Unavailable(_))
            ));
            assert!(f.files.path(Leaf::Marker).exists());
            assert!(!f.files.path(Leaf::Record).exists());
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_admission_withholds_missing_lease_and_preserves_exclusive_marker() {
        let f = Fixture::new();
        assert!(matches!(
            f.files.admit(false),
            Err(Fault::Unavailable("configured lease not observed"))
        ));
        let marker = f.files.admit(true).unwrap();
        marker.check_identity().unwrap();
        assert!(matches!(f.files.admit(true), Err(Fault::Busy)));
        assert!(f.files.path(Leaf::Marker).is_file());
        f.canary_unchanged();
    }

    #[test]
    fn native_owner_relaxation_admits_and_locks_without_mutation_capability_refusal() {
        let f = Fixture::new();
        assert!(f.files.admit(false).is_err());
        assert!(!f.files.path(Leaf::Marker).exists());
        let marker = f.files.admit(true).unwrap();
        marker.check_identity().unwrap();
        assert!(matches!(f.files.admit(true), Err(Fault::Busy)));
        f.canary_unchanged();
    }

    #[test]
    fn native_periodic_sampler_runs_while_a_direct_command_is_live_and_finish_reaps_it() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        let observation = std::rc::Rc::new(std::cell::RefCell::new(
            crate::gate::contention::Sampler::new(),
        ));
        let outcome = use_
            .run(&lease, false, |runner, _| {
                runner.observe(observation.clone());
                let exit = runner.run(
                    std::process::Command::new("/usr/bin/python3")
                        .args(["-c", "import time; time.sleep(16)"])
                        .env_clear()
                        .env("HOME", &f.base)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null()),
                )?;
                assert!(exit.success());
                assert!(runner.child_reaped());
                assert!(
                    observation.borrow().samples >= 2,
                    "no periodic sample during the live command"
                );
                Ok(())
            })
            .unwrap();
        outcome.result.unwrap();
        assert_eq!(outcome.publication, "record published");
        assert!(!f.files.path(Leaf::Marker).exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_begin_resets_unknown_target_and_invalidates_before_build() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        fs::create_dir(f.files.build_path()).unwrap();
        fs::write(f.files.build_path().join("stale"), b"stale").unwrap();
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        assert_eq!(use_.action, Action::Reset(Reset::NoRecord));
        assert!(!f.files.build_path().join("stale").exists());
        assert!(f.files.build_path().is_dir());
        assert!(f.files.path(Leaf::Marker).is_file());
        assert!(!f.files.path(Leaf::Record).exists());
        assert_eq!(use_.marker.phase, Phase::Invalidated);
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_success_and_failed_command_publish_then_warm_reuse() {
        for code in [0, 7] {
            let f = Fixture::new();
            f2_repo(&f);
            let lease = f2_lease(&f);
            let target = native_admission(&f.files.root, false, &lease).unwrap();
            let use_ = target.begin(&lease).unwrap();
            let result = use_
                .run(&lease, false, |runner, path| {
                    fs::write(path.join("built"), b"current artifact")?;
                    let status = runner.run(&mut f2_command(&f, &format!("exit {code}")))?;
                    anyhow::ensure!(status.success(), "actual command failed");
                    Ok(())
                })
                .unwrap();
            assert_eq!(result.result.is_ok(), code == 0);
            assert!(result.statuses.iter().any(|s| s.exit.code() == Some(code)));
            assert!(result.end_ns > 0);
            assert_eq!(result.publication, "record published");
            assert!(matches!(
                f.files.read_record().unwrap(),
                Stored::Complete(_)
            ));
            assert!(!f.files.path(Leaf::Marker).exists());
            let next = target.begin(&lease).unwrap();
            assert_eq!(next.decision(), "reused");
            assert_eq!(
                fs::read(next.target().join("built")).unwrap(),
                b"current artifact"
            );
            next.run(&lease, false, |_, _| Ok(()))
                .unwrap()
                .result
                .unwrap();
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_lifecycle_backdated_source_and_damaged_record_reset_actual_artifacts() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        target
            .begin(&lease)
            .unwrap()
            .run(&lease, false, |_, path| {
                fs::write(path.join("stale"), b"old")?;
                Ok(())
            })
            .unwrap();
        let source = f.files.root.join("source.rs");
        fs::write(&source, b"backdated changed content").unwrap();
        File::open(&source)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        let use_ = target.begin(&lease).unwrap();
        assert_eq!(use_.decision(), "reset: source content changed");
        assert!(!use_.target().join("stale").exists());
        use_.run(&lease, false, |_, _| Ok(())).unwrap();
        for damaged in [
            vec![],
            b"truncated".to_vec(),
            record().encode().unwrap()[..20].to_vec(),
        ] {
            fs::write(f.files.path(Leaf::Record), damaged).unwrap();
            fs::write(f.files.build_path().join("stale"), b"old").unwrap();
            let use_ = target.begin(&lease).unwrap();
            assert_eq!(use_.decision(), "reset: unreadable record");
            assert!(!use_.target().join("stale").exists());
            use_.run(&lease, false, |_, _| Ok(())).unwrap();
        }
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_source_change_finishes_without_record() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let result = target
            .begin(&lease)
            .unwrap()
            .run(&lease, false, |runner, _| {
                assert!(
                    runner
                        .run(&mut f2_command(&f, "printf changed > source.rs"))?
                        .success()
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(result.publication, "no record: source changed");
        assert!(!f.files.path(Leaf::Record).exists());
        assert!(!f.files.path(Leaf::Marker).exists());
        assert_eq!(target.begin(&lease).unwrap().decision(), "reset: no record");
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_cold_cleanup_and_keep_never_touch_warm_or_sibling() {
        for keep in [false, true] {
            let f = Fixture::new();
            f2_repo(&f);
            let lease = f2_lease(&f);
            fs::create_dir(f.files.build_path()).unwrap();
            fs::write(f.files.build_path().join("warm-canary"), b"warm").unwrap();
            fs::write(f.files.path(Leaf::Record), b"warm-state").unwrap();
            let cold = StateFiles::inspect(&f.files.root, f.files.owner, Kind::Cold).unwrap();
            fs::create_dir(cold.build_path()).unwrap();
            fs::write(cold.build_path().join("stale"), b"old").unwrap();
            let target = native_admission(&f.files.root, true, &lease).unwrap();
            let use_ = target.begin(&lease).unwrap();
            assert_eq!(use_.decision(), "cold");
            assert!(!use_.target().join("stale").exists());
            let result = use_
                .run(&lease, keep, |_, path| {
                    fs::write(path.join("built"), b"new")?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(cold.build_path().exists(), keep);
            assert!(!cold.path(Leaf::Marker).exists());
            assert_eq!(fs::read(f.files.path(Leaf::Record)).unwrap(), b"warm-state");
            assert_eq!(
                fs::read(f.files.build_path().join("warm-canary")).unwrap(),
                b"warm"
            );
            assert_eq!(
                result.publication,
                if keep {
                    "cold target kept by request"
                } else {
                    "cold target removed"
                }
            );
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_lifecycle_failed_publication_keeps_marker_without_following_link() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        let result = use_.run(&lease, false, |_, _| {
            symlink(&f.outside, f.files.path(Leaf::Temporary))?;
            Ok(())
        });
        let error = result.err().unwrap();
        assert!(error.to_string().contains("kept"));
        assert!(f.files.path(Leaf::Marker).exists());
        assert!(!f.files.path(Leaf::Record).exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_lease_loss_at_finish_keeps_marker_and_never_publishes() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        let error = use_
            .run(&lease, false, |_, _| {
                fs::rename(f.base.join("r-orch-gate.lease"), f.base.join("lost-lease"))?;
                Ok(())
            })
            .err()
            .unwrap();
        assert!(error.to_string().contains("kept"));
        assert!(f.files.path(Leaf::Marker).exists());
        assert!(!f.files.path(Leaf::Record).exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_leaf_link_controls_preserve_canary_after_real_success() {
        for leaf in [Leaf::Marker, Leaf::Record, Leaf::Temporary] {
            let f = Fixture::new();
            f2_repo(&f);
            let lease = f2_lease(&f);
            let target = native_admission(&f.files.root, false, &lease).unwrap();
            target
                .begin(&lease)
                .unwrap()
                .run(&lease, false, |_, _| Ok(()))
                .unwrap();
            let path = f.files.path(leaf);
            if path.exists() {
                fs::remove_file(&path).unwrap();
            }
            symlink(&f.outside, &path).unwrap();
            assert!(target.begin(&lease).is_err());
            assert!(
                fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_lifecycle_artifact_symlink_is_unlinked_without_following_it() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        fs::create_dir(f.files.build_path()).unwrap();
        symlink(&f.outside, f.files.build_path().join("artifact-link")).unwrap();
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        assert!(!use_.target().join("artifact-link").exists());
        use_.run(&lease, false, |_, _| Ok(())).unwrap();
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_free_same_inode_marker_resets_and_rebuilds_under_lease() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        fs::write(use_.target().join("unfinished"), b"untouched").unwrap();
        let inode = use_.marker.file.metadata().unwrap().ino();
        drop(use_);
        let use_ = target.begin(&lease).unwrap();
        assert_eq!(use_.decision(), "reset: previous build did not finish");
        assert_eq!(use_.marker.file.metadata().unwrap().ino(), inode);
        assert!(!use_.target().join("unfinished").exists());
        let outcome = use_
            .run(&lease, false, |runner, path| {
                runner.run(
                    std::process::Command::new("/bin/sh")
                        .args(["-c", "printf rebuilt > rebuilt"])
                        .current_dir(path),
                )
            })
            .unwrap();
        assert!(outcome.result.unwrap().success());
        assert_eq!(
            fs::read(f.files.build_path().join("rebuilt")).unwrap(),
            b"rebuilt"
        );
        let reused = target.begin(&lease).unwrap();
        assert_eq!(reused.decision(), "reused");
        reused.run(&lease, false, |_, _| Ok(())).unwrap();
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_inherited_marker_excludes_until_last_close_then_control_recovers() {
        use super::super::steps::tests::{ChildGuard, Scratch, fixture, ready};
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let control = Scratch::new();
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        let inode = use_.marker.file.metadata().unwrap().ino();
        fs::write(use_.target().join("unfinished"), b"old").unwrap();
        let inherited = use_.marker.command_descriptor().unwrap();
        let mut child = ChildGuard(fixture(&control, "direct", "hold").spawn().unwrap());
        drop(inherited);
        ready(&mut child.0, &control.0.join("direct.ready"));
        drop(use_); // close, NEVER unlock; direct child keeps same description
        let error = target.begin(&lease).err().unwrap();
        assert!(
            error
                .downcast_ref::<Fault>()
                .is_some_and(|f| matches!(f, Fault::Busy))
        );
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(
            fs::metadata(f.files.path(Leaf::Marker)).unwrap().ino(),
            inode
        );
        assert!(f.files.build_path().join("unfinished").exists());
        fs::write(control.0.join("direct.release"), b"finish").unwrap();
        assert!(child.0.wait().unwrap().success());
        // Last close allows the same production policy used by normal admission.
        let use_ = target.begin(&lease).unwrap();
        assert_eq!(use_.decision(), "reset: previous build did not finish");
        assert_eq!(use_.marker.file.metadata().unwrap().ino(), inode);
        assert!(!use_.target().join("unfinished").exists());
        use_.run(&lease, false, |_, _| Ok(())).unwrap();
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_retains_first_structural_cause_after_link_is_removed() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        let temporary = f.files.path(Leaf::Temporary);
        let error = use_
            .run(&lease, false, |runner, _| {
                symlink(&f.outside, &temporary).unwrap();
                let first = runner
                    .run(&mut std::process::Command::new("/usr/bin/true"))
                    .unwrap_err();
                assert!(format!("{first:#}").contains("warm.record.tmp"));
                fs::remove_file(&temporary).unwrap();
                // A later generic busy/failed flag must not overwrite the cause.
                assert!(
                    runner
                        .run(&mut std::process::Command::new("/usr/bin/true"))
                        .is_err()
                );
                Ok(())
            })
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("warm.record.tmp"),
            "{error:#}"
        );
        assert!(f.files.path(Leaf::Marker).exists());
        assert!(!f.files.path(Leaf::Record).exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_every_publication_step_rechecks_configured_lease() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let mut use_ = target.begin(&lease).unwrap();
        fs::rename(f.base.join("r-orch-gate.lease"), f.base.join("lost-lease")).unwrap();
        let error = use_
            .marker
            .finish_warm(
                finished_observations(),
                &use_.before.sources,
                use_.before.current(&f.files.root),
                300,
            )
            .unwrap_err();
        assert!(error.to_string().contains("configured lease lost"));
        assert!(f.files.path(Leaf::Marker).exists());
        assert!(!f.files.path(Leaf::Record).exists());
        assert!(!f.files.path(Leaf::Temporary).exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_existing_private_temporary_refuses_exclusive_publication() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        let use_ = target.begin(&lease).unwrap();
        let error = use_
            .run(&lease, false, |_, _| {
                fs::write(f.files.path(Leaf::Temporary), b"preexisting temporary")?;
                fs::set_permissions(
                    f.files.path(Leaf::Temporary),
                    fs::Permissions::from_mode(0o600),
                )?;
                Ok(())
            })
            .err()
            .unwrap();
        let failure = error.downcast_ref::<UseFailure>().unwrap();
        assert!(
            matches!(failure.source.downcast_ref::<Fault>(), Some(Fault::Io { operation: "openat checked state", source, .. }) if source.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            fs::read(f.files.path(Leaf::Temporary)).unwrap(),
            b"preexisting temporary"
        );
        assert!(f.files.path(Leaf::Marker).exists());
        assert!(!f.files.path(Leaf::Record).exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_shared_lease_refuses_before_state_creation() {
        let f = Fixture::new();
        let lease = super::super::lease::Lease::Owned(
            super::super::lease::Owned::acquire(
                &f.base,
                f.files.owner,
                super::super::lease::Mode::Shared,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        );
        fs::remove_dir_all(f.files.root.join("target")).unwrap();
        let error = native_admission(&f.files.root, false, &lease)
            .err()
            .unwrap();
        assert!(error.to_string().contains("exclusive"), "{error}");
        assert!(!f.files.root.join("target").exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_lifecycle_real_cargo_build_and_compile_failure_finish_owned_target() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        fs::write(f.files.root.join("Cargo.toml"), "[package]\nname=\"native-lifecycle\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[lib]\npath=\"source.rs\"\n[workspace]\n").unwrap();
        fs::write(
            f.files.root.join("Cargo.lock"),
            r#"version = 4
[[package]]
name = "native-lifecycle"
version = "0.0.0"
"#,
        )
        .unwrap();
        fs::write(
            f.files.root.join("source.rs"),
            b"pub fn built() -> u8 { 7 }\n",
        )
        .unwrap();
        let output = std::process::Command::new("rustup")
            .args(["which", "rustc"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let rustc = PathBuf::from(std::str::from_utf8(&output.stdout).unwrap().trim());
        let cargo = rustc.with_file_name("cargo");
        let target = native_admission(&f.files.root, false, &lease).unwrap();
        for expected in [0, 101] {
            if expected == 101 {
                fs::write(
                    f.files.root.join("source.rs"),
                    b"compile_error!(\"intentional native control\");\n",
                )
                .unwrap();
                File::open(f.files.root.join("source.rs"))
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
                    .unwrap();
            }
            let use_ = target.begin(&lease).unwrap();
            assert_eq!(
                use_.decision(),
                if expected == 0 {
                    "reset: no record"
                } else {
                    "reset: source content changed"
                }
            );
            let result = use_
                .run(&lease, false, |runner, path| {
                    let mut command = std::process::Command::new(&cargo);
                    command
                        .args(["build", "--offline", "--locked", "--manifest-path"])
                        .arg(f.files.root.join("Cargo.toml"))
                        .current_dir(&f.files.root)
                        .env_clear()
                        .env("HOME", &f.base)
                        .env("CARGO_HOME", f.base.join("cargo"))
                        .env("CARGO_TARGET_DIR", path)
                        .env("RUSTC", &rustc)
                        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", f.base.join("global.yml"));
                    let log = runner.logged(&mut command, 1024 * 1024)?;
                    eprintln!(
                        "native cargo control expected={expected} actual={:?}\n{}",
                        log.exit.code(),
                        String::from_utf8_lossy(&log.bytes)
                    );
                    assert_eq!(log.exit.code(), Some(expected));
                    Ok(())
                })
                .unwrap();
            assert_eq!(result.publication, "record published");
            assert!(
                result
                    .statuses
                    .iter()
                    .any(|s| s.exit.code() == Some(expected))
            );
            assert!(!f.files.path(Leaf::Marker).exists());
        }
        let next = target.begin(&lease).unwrap();
        assert_eq!(next.decision(), "reused"); // failed compilation finished; it did not pass
        next.run(&lease, false, |_, _| Ok(())).unwrap();
        f.canary_unchanged();
    }

    #[test]
    fn source_budget_reserves_complete_record_overhead_at_capacity() {
        // 2022 * (4096 + 40 + 8) + (1136 + 40 + 8) = 8 MiB - 8256.
        // Removing the overhead reserve accepts the one-byte-too-large capture.
        let mut entries: Vec<_> = (0..2022)
            .map(|index| Source {
                path: format!("{index:04}{}", "x".repeat(4092)).into_bytes(),
                content: "a".repeat(40),
                modified_ns: Some(100),
            })
            .collect();
        entries.push(Source {
            path: vec![b'z'; 1136],
            content: "a".repeat(40),
            modified_ns: Some(100),
        });
        let at_capacity = Sources::new(entries.clone()).unwrap();
        let r = Record::new(
            format!("/{}", "r".repeat(4095)).into_bytes(),
            "t".repeat(4096),
            boot(),
            200,
            &at_capacity,
        )
        .unwrap();
        let encoded = r.encode().unwrap();
        assert_eq!(encoded.len(), 8 * 1024 * 1024);
        assert_eq!(Record::decode(&encoded).unwrap(), r);
        entries.last_mut().unwrap().path.push(b'z');
        assert!(matches!(
            Sources::new(entries),
            Err(Fault::Codec("source inventory byte limit")),
        ));
        // Even a hand-constructed Record cannot evade complete-size validation.
        let mut too_large = r;
        let content = too_large.sources.remove(&vec![b'z'; 1136]).unwrap();
        too_large.sources.insert(vec![b'z'; 1137], content);
        assert!(matches!(
            too_large.validate(),
            Err(Fault::Codec("record byte limit"))
        ));
        assert!(too_large.encode().is_err());
    }

    #[test]
    fn shared_phase_sequences_match_independent_expected_orders() {
        // Each literal is independent of the production step lists. In
        // particular, dropping a sync from a shared list must fail this oracle.
        let mut io = Journal::default();
        protect_order(&mut io).unwrap();
        assert_eq!(io.events, [Step::MarkerSync, Step::DirectorySync]);
        io.events.clear();
        invalidate_order(&mut io, Kind::Warm).unwrap();
        assert_eq!(
            io.events,
            [
                Step::RemoveTemporary,
                Step::RemoveRecord,
                Step::DirectorySync
            ],
        );
        io.events.clear();
        invalidate_order(&mut io, Kind::Cold).unwrap();
        assert_eq!(io.events, [Step::DirectorySync]);
        io.events.clear();
        finish_order(&mut io, Some(b"complete")).unwrap();
        assert_eq!(
            io.events,
            [
                Step::WriteTemporary,
                Step::TemporarySync,
                Step::RenameRecord,
                Step::DirectorySync,
                Step::RemoveMarker,
            ],
        );
        io.events.clear();
        finish_order(&mut io, None).unwrap();
        assert_eq!(io.events, [Step::RemoveMarker]);
    }

    #[test]
    fn fixture_fallback_and_override_are_private_exclusive_and_cleaned() {
        // Direct selection exercises both configuration paths without changing
        // process-global environment or silently skipping ordinary suite tests.
        let fallback = Fixture::with_root(None);
        let fallback_base = fallback.base.clone();
        assert_eq!(
            fallback_base.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert_eq!(
            fs::symlink_metadata(&fallback_base).unwrap().mode() & 0o7777,
            0o700
        );
        let nested = Fixture::with_root(Some(&fallback.base));
        let nested_base = nested.base.clone();
        assert_eq!(nested_base.parent().unwrap(), fallback.base.as_path());
        assert_ne!(nested_base, fallback_base);
        assert_eq!(
            fs::symlink_metadata(&nested_base).unwrap().mode() & 0o7777,
            0o700
        );
        nested.canary_unchanged();
        drop(nested);
        assert!(!nested_base.exists());
        fallback.canary_unchanged();
        drop(fallback);
        assert!(!fallback_base.exists());
    }

    #[test]
    fn fixture_retains_its_case_during_unwind() {
        let outer = Fixture::with_root(None);
        let mut retained = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inner = Fixture::with_root(Some(&outer.base));
            retained = Some(inner.base.clone());
            panic!("intentional fixture unwind");
        }));
        assert!(result.is_err());
        let retained = retained.unwrap();
        assert!(retained.is_dir());
        // The simulated panic has ended; no child or file handle exists.
        // Remove only the exclusive nested case, never the shared temp parent.
        fs::remove_dir_all(&retained).unwrap();
        outer.canary_unchanged();
    }

    fn finished_observations() -> Finished {
        // Injected observations only: these do not establish a lease or reaping.
        Finished {
            direct_reaped: true,
            lease_held: true,
            inherited_holder_valid: true,
        }
    }

    fn prepare_warm_marker(f: &Fixture) -> Marker<'_> {
        let mut old = f.files.open_regular(Leaf::Record, true, true).unwrap();
        old.write_all(b"old complete record must be invalidated")
            .unwrap();
        drop(old);
        let temporary = f
            .files
            .create_temporary(b"old temporary must be removed")
            .unwrap();
        drop(temporary);
        let mut held = f.files.lock_marker_primitive().unwrap();
        held.protect().unwrap();
        assert_eq!(held.phase, Phase::Protected);
        // Target-action boundary: protect cannot invalidate either old name.
        assert_eq!(
            fs::read(f.files.path(Leaf::Record)).unwrap(),
            b"old complete record must be invalidated",
        );
        assert_eq!(
            fs::read(f.files.path(Leaf::Temporary)).unwrap(),
            b"old temporary must be removed",
        );
        held.invalidate().unwrap();
        assert_eq!(held.phase, Phase::Invalidated);
        assert!(!f.files.path(Leaf::Record).exists());
        assert!(!f.files.path(Leaf::Temporary).exists());
        held
    }

    fn assert_removed(held: &mut Marker<'_>) {
        assert_eq!(held.phase, Phase::Removed);
        assert!(!held.files.path(Leaf::Marker).exists());
        assert!(!held.files.path(Leaf::Temporary).exists());
        assert!(matches!(
            held.step(Step::MarkerSync, None),
            Err(Fault::Unavailable(_))
        ));
        assert!(matches!(
            held.step(Step::RemoveMarker, None),
            Err(Fault::Unavailable(_))
        ));
        assert_eq!(held.phase, Phase::Removed);
    }

    #[test]
    fn real_marker_warm_finish_publishes_exact_record_and_removes_old_names() {
        // Real private I/O, including directory sync; no platform success is
        // presumed. Any sync error fails, never skips or weakens this control.
        let f = Fixture::new();
        let mut held = prepare_warm_marker(&f);
        let s = sources();
        let mut current = context(&s);
        current.root = f.files.root.as_os_str().as_bytes();
        assert_eq!(
            held.finish_warm(finished_observations(), &s, current, 300)
                .unwrap(),
            Publication::Record,
        );
        let expected = Record {
            root: f.files.root.as_os_str().as_bytes().to_vec(),
            toolchain: "rust-1.99.1".into(),
            boot: boot(),
            end_ns: 300,
            sources: BTreeMap::from([(b"src/main.rs".to_vec(), "a".repeat(40))]),
        };
        let bytes = fs::read(f.files.path(Leaf::Record)).unwrap();
        assert_eq!(Record::decode(&bytes).unwrap(), expected);
        assert_eq!(f.files.read_record().unwrap(), Stored::Complete(expected));
        assert_removed(&mut held);
        drop(held); // Close own descriptor before Fixture removes its case.
        f.canary_unchanged();
    }

    #[test]
    fn real_marker_source_changed_finish_publishes_no_record_and_removes_names() {
        let f = Fixture::new();
        let mut held = prepare_warm_marker(&f);
        let before = sources();
        let mut after = sources();
        after.0.get_mut(b"src/main.rs".as_slice()).unwrap().content = "b".repeat(40);
        assert_eq!(
            held.finish_warm(finished_observations(), &before, context(&after), 300)
                .unwrap(),
            Publication::SourceChanged,
        );
        assert_eq!(f.files.read_record().unwrap(), Stored::Missing);
        assert_removed(&mut held);
        drop(held);
        f.canary_unchanged();
    }

    #[test]
    fn real_marker_no_boot_finish_publishes_no_record_and_removes_names() {
        let f = Fixture::new();
        let mut held = prepare_warm_marker(&f);
        let s = sources();
        let mut current = context(&s);
        current.boot = None;
        assert_eq!(
            held.finish_warm(finished_observations(), &s, current, 300)
                .unwrap(),
            Publication::NoBoot,
        );
        assert_eq!(f.files.read_record().unwrap(), Stored::Missing);
        assert_removed(&mut held);
        drop(held);
        f.canary_unchanged();
    }

    #[test]
    fn real_marker_cold_finish_removes_only_cold_marker() {
        let mut f = Fixture::new();
        let record_path = f.files.path(Leaf::Record);
        let temp_path = f.files.path(Leaf::Temporary);
        let mut record = f.files.open_regular(Leaf::Record, true, true).unwrap();
        record.write_all(b"warm record unchanged").unwrap();
        drop(record);
        let temporary = f
            .files
            .create_temporary(b"warm temporary unchanged")
            .unwrap();
        drop(temporary);
        f.files.kind = Kind::Cold;
        let mut held = f.files.lock_marker_primitive().unwrap();
        held.protect().unwrap();
        held.invalidate().unwrap();
        assert_eq!(fs::read(&record_path).unwrap(), b"warm record unchanged");
        assert_eq!(fs::read(&temp_path).unwrap(), b"warm temporary unchanged");
        // No cold target removal is implemented/proved; F2 owns that boundary.
        held.finish_cold(finished_observations()).unwrap();
        assert_eq!(held.phase, Phase::Removed);
        assert!(!f.files.path(Leaf::Marker).exists());
        assert!(matches!(
            held.step(Step::MarkerSync, None),
            Err(Fault::Unavailable(_))
        ));
        assert!(matches!(
            held.step(Step::RemoveMarker, None),
            Err(Fault::Unavailable(_))
        ));
        assert_eq!(fs::read(record_path).unwrap(), b"warm record unchanged");
        assert_eq!(fs::read(temp_path).unwrap(), b"warm temporary unchanged");
        drop(held);
        f.canary_unchanged();
    }

    #[test]
    fn record_io_error_recheck_retains_regular_fault_context_and_reset() {
        let f = Fixture::new();
        let path = f.files.path(Leaf::Record);
        let file = f.files.open_regular(Leaf::Record, true, true).unwrap();
        let expected = identity(&file.metadata().unwrap());
        // Injected EACCES/EIO, not an OS permission/media-failure demonstration.
        for (operation, errno) in [
            ("open state leaf no-follow/nonblocking", 13),
            ("read completed record", 5),
        ] {
            let source = io::Error::from_raw_os_error(errno);
            let detail = source.to_string();
            let unreadable = f
                .files
                .unreadable_record(
                    Fault::Io {
                        operation,
                        path: path.clone(),
                        source,
                    },
                    expected,
                )
                .unwrap();
            assert_eq!(
                unreadable,
                Stored::Unreadable {
                    operation,
                    path: path.clone(),
                    errno: Some(errno),
                    detail,
                },
            );
            assert_eq!(
                decide(observation(), &unreadable, context(&sources())),
                Action::Reset(Reset::Unreadable),
            );
        }
        drop(file);
        f.canary_unchanged();
    }

    #[test]
    fn record_io_error_recheck_refuses_unsafe_or_replaced_leaf() {
        for replacement in 0..4 {
            let f = Fixture::new();
            let path = f.files.path(Leaf::Record);
            let file = f.files.open_regular(Leaf::Record, true, true).unwrap();
            let expected = identity(&file.metadata().unwrap());
            fs::remove_file(&path).unwrap();
            let mut socket = None;
            let mut regular = None;
            match replacement {
                0 => symlink(&f.outside, &path).unwrap(),
                1 => socket = Some(UnixListener::bind(&path).unwrap()),
                2 => regular = Some(f.files.open_regular(Leaf::Record, true, true).unwrap()),
                _ => {} // Missing after an I/O error is also a structural refusal.
            }
            // Simulates an I/O error after the checked open/read raced a swap.
            for operation in [
                "open state leaf no-follow/nonblocking",
                "read completed record",
            ] {
                let error = Fault::Io {
                    operation,
                    path: path.clone(),
                    source: io::Error::from_raw_os_error(5),
                };
                boundary_at(
                    f.files.unreadable_record(error, expected).map(|_| ()),
                    &path,
                );
            }
            f.canary_unchanged();
            drop(regular);
            drop(socket);
            drop(file);
        }
    }

    #[test]
    fn directory_special_files_are_refused_at_each_directory_boundary() {
        for name in ["target", "target/.gate-state", "target/gate-warm"] {
            let f = Fixture::new();
            let path = f.files.root.join(name);
            if name == "target" {
                fs::rename(&path, f.base.join("saved-target")).unwrap();
            } else if path.exists() {
                fs::remove_dir(&path).unwrap();
            }
            let socket = UnixListener::bind(&path).unwrap();
            let result = if name == "target/gate-warm" {
                f.files.check_target().map(|_| ())
            } else {
                f.files.check_parents()
            };
            boundary_at(result, &path);
            assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
            f.canary_unchanged();
            drop(socket);
        }
    }

    #[test]
    fn directory_owner_and_device_observations_refuse_each_boundary() {
        for name in ["target", "target/.gate-state", "target/gate-warm"] {
            let mut f = Fixture::new();
            let path = f.files.root.join(name);
            if !path.exists() {
                fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            }
            f.files.check_directory(&path, false).unwrap();
            // Inject expected observations at the directory operation boundary;
            // no chown or mount fixture, and no ancestor refusal substitutes.
            let owner = f.files.owner;
            f.files.owner = owner.wrapping_add(1);
            boundary_at(f.files.check_directory(&path, false).map(|_| ()), &path);
            f.files.owner = owner;
            let device = f.files.device;
            f.files.device = device.wrapping_add(1);
            boundary_at(f.files.check_directory(&path, false).map(|_| ()), &path);
            f.files.device = device;
            assert!(path.is_dir());
            f.canary_unchanged();
        }
    }

    fn f2_lease(f: &Fixture) -> super::super::lease::Lease {
        super::super::lease::Lease::Owned(
            super::super::lease::Owned::acquire(
                &f.base,
                f.files.owner,
                super::super::lease::Mode::Exclusive,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
    }

    fn f2_repo(f: &Fixture) {
        let exit = std::process::Command::new("/usr/bin/git")
            .args(["init", "-q"])
            .current_dir(&f.files.root)
            .env_clear()
            .env("HOME", &f.base)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(exit.success());
        fs::write(f.files.root.join(".gitignore"), b"target/\n").unwrap();
        fs::write(f.files.root.join("source.rs"), b"source bytes").unwrap();
    }

    fn f2_command(f: &Fixture, script: &str) -> std::process::Command {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", script])
            .current_dir(&f.files.root)
            .env_clear()
            .env("HOME", &f.base)
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", f.base.join("global.yml"));
        command
    }

    #[test]
    fn f2_real_normal_and_failed_commands_reap_then_publish_with_actual_boot_and_source() {
        for code in [0, 7] {
            let f = Fixture::new();
            f2_repo(&f);
            let lease = f2_lease(&f);
            let mut marker = f.files.lock_marker_primitive().unwrap();
            marker.protect().unwrap();
            marker.invalidate().unwrap();
            let start_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i128;
            let result = marker
                .use_warm(&lease, &mut [f2_command(&f, &format!("exit {code}"))])
                .unwrap();
            assert_eq!(result.publication, Publication::Record);
            assert!(result.exits.iter().any(|e| e.code() == Some(code)));
            assert!(result.end_ns >= start_ns);
            assert!(f.files.check_leaf(Leaf::Marker).unwrap().is_none());
            let Stored::Complete(record) = f.files.read_record().unwrap() else {
                panic!("record absent")
            };
            assert_eq!(record.end_ns, result.end_ns);
            assert!(record.sources.contains_key(b"source.rs".as_slice()));
            assert_eq!(record.sources.len(), 2);
        }
    }

    #[test]
    fn f2_source_set_change_reaps_but_withholds_record() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let mut marker = f.files.lock_marker_primitive().unwrap();
        marker.protect().unwrap();
        marker.invalidate().unwrap();
        let result = marker
            .use_warm(&lease, &mut [f2_command(&f, "printf changed > added.rs")])
            .unwrap();
        assert_eq!(result.publication, Publication::SourceChanged);
        assert!(matches!(f.files.read_record().unwrap(), Stored::Missing));
        assert!(f.files.check_leaf(Leaf::Marker).unwrap().is_none());
    }

    #[test]
    fn f2_capture_failure_retains_marker_and_prevents_second_publication() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let mut marker = f.files.lock_marker_primitive().unwrap();
        marker.protect().unwrap();
        marker.invalidate().unwrap();
        // Removing the fixture's .git is an actual identity-command failure.
        let error = marker
            .use_warm(&lease, &mut [f2_command(&f, "rm -rf .git")])
            .unwrap_err();
        let error = error
            .downcast_ref::<UseFailure>()
            .expect("retained failure status");
        assert!(error.exits.iter().any(|e| !e.success()));
        assert!(f.files.check_leaf(Leaf::Marker).unwrap().is_some());
        assert!(matches!(f.files.read_record().unwrap(), Stored::Missing));
        assert!(marker.use_warm(&lease, &mut []).is_err());
    }

    #[test]
    fn f2_lease_loss_after_reap_retains_marker_without_publication() {
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let mut marker = f.files.lock_marker_primitive().unwrap();
        marker.protect().unwrap();
        marker.invalidate().unwrap();
        let mut command = std::process::Command::new("/bin/mv");
        command
            .arg(f.base.join("r-orch-gate.lease"))
            .arg(f.base.join("replaced-lease"))
            .env_clear()
            .env("HOME", &f.base);
        assert!(marker.use_warm(&lease, &mut [command]).is_err());
        assert!(f.files.check_leaf(Leaf::Marker).unwrap().is_some());
        assert!(matches!(f.files.read_record().unwrap(), Stored::Missing));
    }

    #[test]
    fn f2_missing_lease_refuses_before_any_marker_or_directory_creation() {
        let f = Fixture::new();
        assert!(f.files.admit_configured(None).is_err());
        assert!(f.files.check_leaf(Leaf::Marker).unwrap().is_none());
    }

    #[test]
    fn f2_runner_hands_same_locked_description_to_direct_child_and_preserves_failed_status() {
        let f = Fixture::new();
        let lease = f2_lease(&f);
        let marker = f.files.lock_marker_primitive().unwrap();
        let mut runner = super::super::steps::Runner::new(&lease, &marker).unwrap();
        // Discover the extra descriptor by inode, and read ordinary caller stdin.
        // A missing inherited description or stdin substitution fails this oracle.
        let script = "import os,fcntl\nassert os.read(0,64)==b'normal stdin\\n'\nfound=[]\nfor fd in range(3,256):\n try: st=os.fstat(fd)\n except OSError: continue\n if (st.st_dev,st.st_ino)==(int(os.environ['DEV']),int(os.environ['INO'])): found.append(fd)\nassert len(found)==1,found\nf=open(os.environ['MARKER'],'r+')\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: pass\nelse: raise AssertionError('marker was not locked')\nraise SystemExit(7)";
        let input = f.base.join("input");
        fs::write(&input, b"normal stdin\n").unwrap();
        let meta = marker.file.metadata().unwrap();
        let mut command = std::process::Command::new("/usr/bin/python3");
        command
            .args(["-c", script])
            .env_clear()
            .env("HOME", &f.base)
            .env("MARKER", f.files.path(Leaf::Marker))
            .env("DEV", meta.dev().to_string())
            .env("INO", meta.ino().to_string())
            .stdin(File::open(input).unwrap());
        let status = runner.run(&mut command).unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(runner.statuses().len(), 1);
        assert_eq!(runner.statuses()[0].exit.code(), Some(7));
        assert!(runner.statuses()[0].elapsed > std::time::Duration::ZERO);
        runner.finished().unwrap();
        let flags = nix::fcntl::fcntl(&marker.file, nix::fcntl::FcntlArg::F_GETFD).unwrap();
        assert_ne!(flags & nix::fcntl::FdFlag::FD_CLOEXEC.bits(), 0);
        drop(runner);
        drop(marker);
        // Command is still alive: it must not retain a marker duplicate.
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.files.path(Leaf::Marker))
            .unwrap();
        probe.try_lock().unwrap();
    }

    #[test]
    fn f2_runner_spawn_failure_closes_temporary_inherited_description() {
        let f = Fixture::new();
        let lease = f2_lease(&f);
        let marker = f.files.lock_marker_primitive().unwrap();
        let mut runner = super::super::steps::Runner::new(&lease, &marker).unwrap();
        let mut command = std::process::Command::new(f.base.join("absent-program"));
        assert!(runner.run(&mut command).is_err());
        assert!(runner.child_reaped());
        assert!(runner.statuses().is_empty());
        assert!(runner.finished().is_err());
        drop(runner);
        drop(marker);
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.files.path(Leaf::Marker))
            .unwrap();
        probe.try_lock().unwrap();
    }

    #[test]
    fn f2_runner_capture_failure_is_sticky_after_actual_reaping() {
        let f = Fixture::new();
        let lease = f2_lease(&f);
        let marker = f.files.lock_marker_primitive().unwrap();
        let mut runner = super::super::steps::Runner::new(&lease, &marker).unwrap();
        assert!(
            runner
                .capture(&mut f2_command(&f, "printf over-budget"), 2)
                .is_err()
        );
        assert_eq!(runner.statuses().len(), 1);
        assert!(runner.statuses()[0].exit.success());
        assert!(runner.finished().is_err());
        assert!(runner.run(&mut f2_command(&f, "exit 0")).is_err());
    }

    #[test]
    fn f2_inherited_actual_holder_death_after_command_reap_prevents_publication() {
        use super::super::steps::tests::{ChildGuard, Scratch, fixture, ready};
        let f = Fixture::new();
        f2_repo(&f);
        let machine = Scratch::new();
        let sockets = Scratch::temporary();
        let mut command = fixture(&machine, "holder", "server-holder");
        command.env("F2_SOCKET_ROOT", &sockets.0);
        let mut holder = ChildGuard(command.spawn().unwrap());
        ready(&mut holder.0, &machine.0.join("holder.ready"));
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(machine.0.join("r-orch-gate.lease")).unwrap())
                .unwrap();
        let inherited = super::super::lease::Inherited::from_capability(
            &machine.0,
            machine.owner(),
            PathBuf::from(record["socket"].as_str().unwrap()),
            record["token"].as_str().unwrap(),
        )
        .unwrap();
        let lease = super::super::lease::Lease::Inherited(inherited);
        let mut direct = fixture(&machine, "command", "hold");
        let root = machine.0.clone();
        let killer = std::thread::spawn(move || {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while !root.join("command.ready").exists() {
                assert!(std::time::Instant::now() < end, "command did not start");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            holder.0.kill().unwrap();
            assert!(!holder.0.wait().unwrap().success());
            // F3 observes loss while this direct child is still live. Do not
            // release it: a terminal SIGTERM status must prove actual teardown.
        });
        let mut marker = f.files.lock_marker_primitive().unwrap();
        marker.protect().unwrap();
        marker.invalidate().unwrap();
        let error = marker
            .use_warm(&lease, std::slice::from_mut(&mut direct))
            .unwrap_err();
        killer.join().unwrap();
        let failure = error.downcast_ref::<UseFailure>().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(failure.exits.last().unwrap().signal(), Some(15));
        assert!(
            failure.exits[..failure.exits.len() - 1]
                .iter()
                .all(|e| e.success())
        );
        assert!(f.files.check_leaf(Leaf::Marker).unwrap().is_some());
        assert!(matches!(f.files.read_record().unwrap(), Stored::Missing));
    }
    #[test]
    fn native_existing_directories_open_and_sync_by_descriptor() {
        let f = Fixture::new();
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        boundary.sync_existing().unwrap();
        f.canary_unchanged();
    }

    #[test]
    fn native_absent_directories_are_observed_without_creation() {
        let f = Fixture::new();
        fs::remove_dir(f.files.state.clone()).unwrap();
        fs::remove_dir(f.files.root.join("target")).unwrap();
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        boundary.sync_existing().unwrap();
        assert!(boundary.read_regular(Leaf::Marker).unwrap().is_none());
        assert!(!f.files.root.join("target").exists());
        f.canary_unchanged();
    }

    #[test]
    fn native_root_walk_never_follows_an_ancestor_link() {
        let mut f = Fixture::new();
        let alias = f.base.join("alias");
        symlink(&f.files.root, &alias).unwrap();
        f.files.root = alias;
        assert!(native::Boundary::inspect(&f.files).is_err());
        f.canary_unchanged();
    }

    #[test]
    fn native_directory_links_and_specials_refuse_without_opening_children() {
        for name in ["target", "target/.gate-state", "target/gate-warm"] {
            for special in [false, true] {
                let f = Fixture::new();
                let path = f.files.root.join(name);
                if path.exists() {
                    fs::remove_dir_all(&path).unwrap();
                }
                let socket = if special {
                    Some(UnixListener::bind(&path).unwrap())
                } else {
                    symlink(&f.base, &path).unwrap();
                    None
                };
                assert!(native::Boundary::inspect(&f.files).is_err());
                assert!(fs::symlink_metadata(&path).is_ok());
                f.canary_unchanged();
                drop(socket);
            }
        }
    }

    #[test]
    fn native_all_state_leaf_kinds_are_checked_without_touching_canary() {
        for leaf in [Leaf::Marker, Leaf::Record, Leaf::Temporary] {
            for plant in ["link", "socket", "public", "hardlink"] {
                let f = Fixture::new();
                let path = f.files.path(leaf);
                let socket = match plant {
                    "link" => {
                        symlink(&f.outside, &path).unwrap();
                        None
                    }
                    "socket" => Some(UnixListener::bind(&path).unwrap()),
                    _ => {
                        let mut file = f.files.open_regular(leaf, true, true).unwrap();
                        file.write_all(b"preserved state").unwrap();
                        drop(file);
                        if plant == "public" {
                            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                        } else {
                            fs::hard_link(&path, f.base.join("alias")).unwrap();
                        }
                        None
                    }
                };
                assert!(native::Boundary::inspect(&f.files).is_err(), "{plant}");
                assert!(fs::symlink_metadata(&path).is_ok());
                f.canary_unchanged();
                drop(socket);
            }
        }
    }

    #[test]
    fn native_effective_owner_and_device_are_not_inferred_from_path() {
        for change_owner in [false, true] {
            let mut f = Fixture::new();
            if change_owner {
                f.files.owner = f.files.owner.wrapping_add(1);
            } else {
                f.files.device = f.files.device.wrapping_add(1);
            }
            assert!(native::Boundary::inspect(&f.files).is_err());
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_checked_regular_read_preserves_busy_marker_and_inode() {
        let f = Fixture::new();
        let mut marker = f.files.lock_marker_primitive().unwrap();
        marker
            .write_diagnostics(b"not admission authority")
            .unwrap();
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        let mut file = boundary.read_regular(Leaf::Marker).unwrap().unwrap();
        assert_eq!(
            identity(&file.metadata().unwrap()),
            identity(&marker.file.metadata().unwrap())
        );
        assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"not admission authority");
        assert!(matches!(f.files.lock_marker_primitive(), Err(Fault::Busy)));
        drop(file);
        marker.check_identity().unwrap();
        assert!(f.files.path(Leaf::Marker).is_file());
        f.canary_unchanged();
    }

    #[test]
    fn native_pinned_parent_replacement_is_detected_before_regular_read() {
        let f = Fixture::new();
        let file = f.files.open_regular(Leaf::Record, true, true).unwrap();
        drop(file);
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        fs::rename(&f.files.state, f.base.join("saved-state")).unwrap();
        fs::create_dir(&f.files.state).unwrap();
        assert!(boundary.recheck().is_err());
        assert!(boundary.read_regular(Leaf::Record).is_err());
        assert!(f.base.join("saved-state/warm.record").is_file());
        f.canary_unchanged();
    }

    #[test]
    fn native_target_scan_does_not_follow_artifact_symlinks() {
        let f = Fixture::new();
        let target = f.files.root.join("target/gate-warm");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("artifact"), b"artifact").unwrap();
        symlink(&f.base, target.join("outside-link")).unwrap();
        native::Boundary::inspect(&f.files)
            .unwrap()
            .recheck()
            .unwrap();
        assert_eq!(fs::read(target.join("artifact")).unwrap(), b"artifact");
        f.canary_unchanged();
    }

    #[test]
    fn native_nested_special_target_entry_and_depth_exhaustion_refuse() {
        for deep in [false, true] {
            let f = Fixture::new();
            let target = f.files.root.join("target/gate-warm");
            fs::create_dir(&target).unwrap();
            let socket = if deep {
                let mut path = target;
                for _ in 0..128 {
                    path.push("d");
                    fs::create_dir(&path).unwrap();
                }
                None
            } else {
                Some(UnixListener::bind(target.join("socket")).unwrap())
            };
            let error = native::Boundary::inspect(&f.files)
                .err()
                .expect("must refuse target");
            if deep {
                assert!(
                    matches!(
                        error,
                        Fault::Unavailable("native target inspection depth limit")
                    ),
                    "{error}"
                );
            } else {
                assert!(
                    matches!(
                        error,
                        Fault::Boundary {
                            why: "special entry inside target",
                            ..
                        }
                    ),
                    "{error}"
                );
            }
            drop(socket);
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_unlinkat_is_not_conditioned_on_the_last_checked_marker_inode() {
        use nix::unistd::{UnlinkatFlags, unlinkat};
        let f = Fixture::new();
        let marker = f.files.lock_marker_primitive().unwrap();
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        let parent = File::open(&f.files.state).unwrap();
        boundary.recheck().unwrap();
        marker.check_identity().unwrap(); // the last possible pre-syscall check
        fs::rename(f.files.path(Leaf::Marker), f.base.join("locked-old-marker")).unwrap();
        symlink(&f.outside, f.files.path(Leaf::Marker)).unwrap();
        // Characterization of the permitted API, NOT the gate's implementation.
        // This deterministic interleaving would violate D45 even with openat.
        unlinkat(&parent, "warm.use", UnlinkatFlags::NoRemoveDir).unwrap();
        assert!(f.files.path(Leaf::Marker).symlink_metadata().is_err());
        assert!(matches!(
            File::open(f.base.join("locked-old-marker"))
                .unwrap()
                .try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        eprintln!(
            "D45 counterexample: unlinkat removed an unchecked replacement link while the checked inode remained locked"
        );
        f.canary_unchanged();
    }

    #[test]
    fn native_renameat_is_not_conditioned_on_the_checked_temporary_inode() {
        use nix::fcntl::renameat;
        let f = Fixture::new();
        let temp = f.files.open_regular(Leaf::Temporary, true, true).unwrap();
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        let parent = File::open(&f.files.state).unwrap();
        let inspected = boundary.read_regular(Leaf::Temporary).unwrap().unwrap();
        assert_eq!(
            identity(&temp.metadata().unwrap()),
            identity(&inspected.metadata().unwrap())
        );
        fs::rename(
            f.files.path(Leaf::Temporary),
            f.base.join("checked-old-temp"),
        )
        .unwrap();
        symlink(&f.outside, f.files.path(Leaf::Temporary)).unwrap();
        renameat(&parent, "warm.record.tmp", &parent, "warm.record").unwrap();
        assert!(
            f.files
                .path(Leaf::Record)
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(f.base.join("checked-old-temp").is_file());
        eprintln!(
            "D45 counterexample: renameat published an unchecked replacement link instead of the checked temporary inode"
        );
        f.canary_unchanged();
    }

    #[test]
    fn native_directory_descriptor_survives_relocation_outside_the_target() {
        use nix::unistd::{UnlinkatFlags, unlinkat};
        let f = Fixture::new();
        let target = f.files.root.join("target/gate-warm");
        fs::create_dir(&target).unwrap();
        let inside = target.join("nested");
        fs::create_dir(&inside).unwrap();
        let boundary = native::Boundary::inspect(&f.files).unwrap();
        let pinned = File::open(&inside).unwrap();
        boundary.recheck().unwrap(); // the last possible pre-syscall check
        let outside = f.base.join("outside-target");
        fs::rename(&inside, &outside).unwrap();
        // Entirely private canary created OUTSIDE target after relocation.
        fs::write(outside.join("new-outside-canary"), b"outside the target").unwrap();
        unlinkat(&pinned, "new-outside-canary", UnlinkatFlags::NoRemoveDir).unwrap();
        assert!(!outside.join("new-outside-canary").exists());
        eprintln!(
            "D45 counterexample: an open directory descriptor did not keep it inside target; unlinkat removed a private outside-target canary"
        );
        f.canary_unchanged();
    }

    #[test]
    fn native_admission_creates_private_state_from_absence() {
        let f = Fixture::new();
        fs::remove_dir_all(f.files.root.join("target")).unwrap();
        let marker = f.files.admit(true).unwrap();
        marker.check_identity().unwrap();
        assert_eq!(
            fs::metadata(f.files.path(Leaf::Marker)).unwrap().mode() & 0o7777,
            0o600
        );
        assert_eq!(fs::metadata(&f.files.state).unwrap().mode() & 0o7777, 0o700);
        f.canary_unchanged();
    }

    // This subprocess boundary keeps handled test signals out of the test
    // supervisor. All workload processes are hermetic fixtures, never agent CLIs.
    #[test]
    fn f3_holder_fixture() {
        let Ok(mode) = std::env::var("F3_MODE") else {
            return;
        };
        let control = PathBuf::from(std::env::var_os("F3_CONTROL").unwrap());
        let f = Fixture::new();
        f2_repo(&f);
        let lease = f2_lease(&f);
        let mut marker = f.files.lock_marker_primitive().unwrap();
        marker.protect().unwrap();
        marker.invalidate().unwrap();
        if mode == "publication" {
            marker.enable_signals().unwrap();
            fs::write(control.join("ready"), b"ready").unwrap();
            while !control.join("release").exists() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let source = sources();
            let error = marker
                .finish_warm(finished_observations(), &source, context(&source), 300)
                .unwrap_err();
            let value = serde_json::json!({ "error": error.to_string(),
                "marker": f.files.check_leaf(Leaf::Marker).unwrap().is_some(),
                "record_missing": matches!(f.files.read_record().unwrap(), Stored::Missing) });
            fs::write(
                control.join("result.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            return;
        }
        fs::write(
            control.join("lease.path"),
            f.base.join("r-orch-gate.lease").as_os_str().as_bytes(),
        )
        .unwrap();
        fs::write(
            control.join("marker.path"),
            f.files.path(Leaf::Marker).as_os_str().as_bytes(),
        )
        .unwrap();
        let mut command = std::process::Command::new("/usr/bin/python3");
        command.args(["-c", r#"
import os, signal, time, pathlib, subprocess, sys
root = pathlib.Path(os.environ['F3_CONTROL'])
mode = os.environ['F3_MODE']
def caught(number, frame):
    (root/'caught').write_text(str(number))
    sys.exit(40 + number)
for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
    signal.signal(number, signal.SIG_IGN if mode == 'ignore' else caught)
if mode == 'closed':
    os.close(1)
    os.close(2)
if mode == 'descendant':
    leaf = subprocess.Popen([sys.executable, '-c',
        "import pathlib,os,time; r=pathlib.Path(os.environ['F3_CONTROL']); "
        "(r/'leaf.ready').write_text(str(os.getpid())); "
        "exec(\"while not (r/'release').exists():\\n (r/'heartbeat').write_text(str(time.monotonic_ns()))\\n time.sleep(.01)\"); "
        "(r/'leaf.done').write_text('done')"])
    while not (root/'leaf.ready').exists(): time.sleep(.01)
(root/'ready').write_text(str(os.getpid()))
if mode == 'burst': os.write(1, b'x' * 4096)
if mode == 'normal': os.write(1, b'captured')
if mode == 'descendant': sys.exit(0)
while not (root/'release').exists(): time.sleep(.01)
"#]).env_clear().env("HOME", &f.base)
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", f.base.join("global.yml"))
            .env("F3_CONTROL", &control).env("F3_MODE", &mode)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if mode == "lease-exec" || mode == "lease-normal" {
            let argv: Vec<_> = std::iter::once(command.get_program().to_owned())
                .chain(command.get_args().map(|s| s.to_owned()))
                .collect();
            let code = super::super::lease::exec(&argv, std::time::Duration::ZERO).unwrap();
            fs::write(control.join("result.json"), format!("{{\"code\":{code}}}")).unwrap();
            return;
        }
        let (error, statuses, finished) = if mode == "use-warm" || mode == "loss" {
            let error = marker.use_warm(&lease, &mut [command]).unwrap_err();
            let failure = error.downcast_ref::<UseFailure>().unwrap();
            (error.to_string(), failure.exits.clone(), false)
        } else {
            let mut runner = super::super::steps::Runner::new(&lease, &marker).unwrap();
            let result = if mode == "ignore" {
                runner.run(&mut command).map(|_| Vec::new())
            } else {
                runner.capture(&mut command, 1024)
            };
            let finished = runner.finished().is_ok();
            let statuses = runner.statuses().iter().map(|s| s.exit).collect::<Vec<_>>();
            let error = result.err().map(|e| e.to_string()).unwrap_or_default();
            (error, statuses, finished)
        };
        use std::os::unix::process::ExitStatusExt;
        let statuses: Vec<_> = statuses
            .iter()
            .map(|s| {
                serde_json::json!({
                    "code": s.code(), "signal": s.signal()
                })
            })
            .collect();
        let value = serde_json::json!({ "error": error, "statuses": statuses,
            "finished": finished, "marker": f.files.check_leaf(Leaf::Marker).unwrap().is_some(),
            "record_missing": matches!(f.files.read_record().unwrap(), Stored::Missing) });
        fs::write(
            control.join("result.json"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }

    struct F3Holder {
        child: std::process::Child,
        control: super::super::steps::tests::Scratch,
        mode: String,
    }
    impl F3Holder {
        fn new(mode: &str, closed_output: bool) -> Self {
            let control = super::super::steps::tests::Scratch::new();
            let rustc = std::process::Command::new("rustup")
                .args(["which", "rustc"])
                .output()
                .unwrap();
            assert!(rustc.status.success());
            let rustc = PathBuf::from(std::str::from_utf8(&rustc.stdout).unwrap().trim());
            let path = format!("{}:/usr/bin:/bin", rustc.parent().unwrap().display());
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gate::target::tests::f3_holder_fixture",
                    "--nocapture",
                ])
                .env_clear()
                .env("XTASK_GATE_LEASE_DIR", &control.0)
                .env("PATH", path)
                .env("F3_MODE", mode)
                .env("F3_CONTROL", &control.0)
                .env("XTASK_F1_PRIVATE_ROOT", control.0.parent().unwrap())
                .env("HOME", &control.0)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("ORCHESTRY_GLOBAL_CONFIG_PATH", control.0.join("global.yml"))
                .stdin(std::process::Stdio::null())
                .stdout(if closed_output {
                    std::process::Stdio::null()
                } else {
                    std::process::Stdio::from(
                        fs::File::create(control.0.join("holder.log")).unwrap(),
                    )
                })
                .stderr(if closed_output {
                    std::process::Stdio::piped()
                } else {
                    std::process::Stdio::null()
                })
                .spawn()
                .unwrap();
            drop(child.stderr.take()); // a genuinely closed logging pipe
            Self {
                child,
                control,
                mode: mode.to_owned(),
            }
        }
        fn ready(&mut self) {
            super::super::steps::tests::ready(&mut self.child, &self.control.0.join("ready"));
        }
        fn signal(&mut self, signal: nix::sys::signal::Signal) {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "holder already exited"
            );
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.child.id() as i32), signal)
                .unwrap();
        }
        fn result(&mut self) -> serde_json::Value {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(8);
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    std::time::Instant::now() < end,
                    "holder failed to complete teardown"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            assert!(
                status.success(),
                "holder failed: {:?}",
                fs::read_to_string(self.control.0.join("holder.log"))
            );
            let value =
                serde_json::from_slice(&fs::read(self.control.0.join("result.json")).unwrap())
                    .unwrap();
            eprintln!("F3 retained {} outcome: {value}", self.mode);
            value
        }
    }
    impl Drop for F3Holder {
        fn drop(&mut self) {
            // Explicit fixture release on both assertion failure and success;
            // workload has NO self-expiry that could masquerade as teardown.
            let _ = fs::write(self.control.0.join("release"), b"release");
            let end = std::time::Instant::now() + std::time::Duration::from_secs(4);
            while self.child.try_wait().ok().flatten().is_none() && std::time::Instant::now() < end
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            if self.control.0.join("leaf.ready").exists() {
                let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while !self.control.0.join("leaf.done").exists() && std::time::Instant::now() < end
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    #[test]
    fn f3_handled_int_term_hup_reap_live_direct_child_and_withhold_record() {
        use nix::sys::signal::Signal;
        for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
            let mut holder = F3Holder::new("use-warm", false);
            holder.ready();
            holder.signal(signal);
            let result = holder.result();
            assert!(
                result["error"].as_str().unwrap().contains("INTERRUPTED"),
                "{result}"
            );
            assert!(
                result["statuses"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["code"] == 40 + signal as i32),
                "{result}"
            );
            assert_eq!(
                fs::read_to_string(holder.control.0.join("caught")).unwrap(),
                (signal as i32).to_string()
            );
            assert_eq!(result["marker"], true);
            assert_eq!(result["record_missing"], true);
        }
    }

    #[test]
    fn f3_hup_with_closed_output_reaps_instead_of_treating_eof_as_completion() {
        let mut holder = F3Holder::new("closed", true);
        holder.ready();
        assert!(
            !holder.control.0.join("result.json").exists(),
            "EOF falsely completed a live child"
        );
        holder.signal(nix::sys::signal::Signal::SIGHUP);
        let result = holder.result();
        assert!(
            result["error"].as_str().unwrap().contains("INTERRUPTED"),
            "{result}"
        );
        assert_eq!(result["statuses"][0]["code"], 41);
        assert_eq!(result["finished"], false);
        assert_eq!(result["marker"], true);
        assert_eq!(result["record_missing"], true);
    }

    #[test]
    fn f3_ignored_signal_escalates_to_kill_and_retains_actual_signal_status() {
        let mut holder = F3Holder::new("ignore", false);
        holder.ready();
        let start = std::time::Instant::now();
        holder.signal(nix::sys::signal::Signal::SIGTERM);
        let result = holder.result();
        assert!(start.elapsed() >= super::super::steps::SIGNAL_GRACE);
        assert!(
            start.elapsed()
                < super::super::steps::SIGNAL_GRACE
                    + super::super::steps::KILL_REAP
                    + std::time::Duration::from_secs(1)
        );
        assert_eq!(result["statuses"][0]["signal"], 9, "{result}");
        assert!(!holder.control.0.join("caught").exists());
        assert_eq!(result["marker"], true);
        assert_eq!(result["record_missing"], true);
    }

    #[test]
    fn f3_descendant_held_pipes_fail_capture_after_direct_exit_without_waiting_for_eof() {
        let mut holder = F3Holder::new("descendant", false);
        holder.ready();
        let start = std::time::Instant::now();
        let result = holder.result();
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("capture EOF deadline"),
            "{result}"
        );
        assert_eq!(result["statuses"][0]["code"], 0);
        assert_eq!(result["finished"], false);
        assert_eq!(result["marker"], true);
        assert_eq!(result["record_missing"], true);
        assert!(
            !holder.control.0.join("leaf.done").exists(),
            "descendant self-exit is not pipe handling"
        );
        assert!(!holder.control.0.join("release").exists());
        let heartbeat = holder.control.0.join("heartbeat");
        let before = fs::read(&heartbeat).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while fs::read(&heartbeat).unwrap() == before {
            assert!(
                std::time::Instant::now() < end,
                "descendant stopped; no post-reap signal authority"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn f3_live_lease_and_marker_loss_stop_owned_child_without_publication() {
        for name in ["lease", "marker"] {
            let mut holder = F3Holder::new("loss", false);
            holder.ready();
            let path = PathBuf::from(std::ffi::OsString::from_vec(
                fs::read(holder.control.0.join(format!("{name}.path"))).unwrap(),
            ));
            let moved = holder.control.0.join(format!("moved-{name}"));
            fs::rename(path, &moved).unwrap();
            let result = holder.result();
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains("lease/marker lost"),
                "{result}"
            );
            assert!(
                result["statuses"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["code"] == 55),
                "{result}"
            );
            assert_eq!(result["record_missing"], true);
            assert!(moved.is_file());
            assert_eq!(result["marker"], name == "lease");
        }
    }

    #[test]
    fn f3_live_capture_overrun_tears_down_without_waiting_for_fixture_eof() {
        let mut holder = F3Holder::new("burst", false);
        holder.ready();
        let result = holder.result();
        assert!(
            result["error"].as_str().unwrap().contains("capture budget"),
            "{result}"
        );
        assert_eq!(result["statuses"][0]["code"], 55, "{result}");
        assert_eq!(result["finished"], false);
        assert_eq!(result["marker"], true);
        assert_eq!(result["record_missing"], true);
        assert!(!holder.control.0.join("release").exists());
    }

    #[test]
    fn f3_normal_capture_requires_reap_and_eof_and_retains_real_success() {
        let mut holder = F3Holder::new("normal", false);
        holder.ready();
        assert!(!holder.control.0.join("result.json").exists());
        fs::write(holder.control.0.join("release"), b"finish").unwrap();
        let result = holder.result();
        assert_eq!(result["error"], "", "{result}");
        assert_eq!(result["statuses"][0]["code"], 0);
        assert_eq!(result["finished"], true);
    }

    #[test]
    fn f3_lease_exec_hup_closed_log_reaps_then_returns_interrupted_code() {
        let mut holder = F3Holder::new("lease-exec", true);
        holder.ready();
        holder.signal(nix::sys::signal::Signal::SIGHUP);
        let result = holder.result();
        assert_eq!(result["code"], 129);
        assert_eq!(
            fs::read_to_string(holder.control.0.join("caught")).unwrap(),
            "1"
        );
    }

    #[test]
    fn f3_lease_exec_normal_returns_real_direct_status() {
        let mut holder = F3Holder::new("lease-normal", false);
        holder.ready();
        fs::write(holder.control.0.join("release"), b"finish").unwrap();
        assert_eq!(holder.result()["code"], 0);
    }

    #[test]
    fn f3_publication_boundary_observes_cancellation_after_command_owner_lifetime() {
        let mut holder = F3Holder::new("publication", false);
        holder.ready();
        holder.signal(nix::sys::signal::Signal::SIGHUP);
        fs::write(holder.control.0.join("release"), b"finish").unwrap();
        let result = holder.result();
        assert!(
            result["error"].as_str().unwrap().contains("INTERRUPTED"),
            "{result}"
        );
        assert_eq!(result["marker"], true);
        assert_eq!(result["record_missing"], true);
    }
}
