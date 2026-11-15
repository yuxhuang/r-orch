//! Source-only cooperative target state. No tier calls this module.
//!
//! Admission decisions are data, never capabilities to delete or signal. F2 must
//! supply actual lease/direct-handle observations and source captures; no process
//! or host-identity acquisition lives here. Recovery has no production supported
//! constructor: RC18 is unshown for every filesystem and command.
//!
//! Candidate native primitives use std's Unix no-follow/nonblocking opens,
//! try_lock and sync_all. They are deliberately behind an unavailable native
//! admission: std alone supplies no descriptor-relative namespace mutation or
//! mount-constrained recursive deletion. Rechecking a pathname cannot close a
//! concurrent parent-replacement race. Native lock behavior and stable directory
//! synchronization still need evidence. Do not turn the private
//! primitive controls into a production permit. These are precise F2/platform
//! prerequisites, not a fallback to a weaker boundary.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

// Encoding budgets, not timing/teardown bounds. 8 MiB contains a 32K-source
// inventory with ordinary repository paths; 4096 bounds each path/tool identity.
// The byte cap takes precedence over the count cap: hitting either withholds a
// publication/reuse. Diagnostic bytes never participate in a state decision.
const MAX_RECORD: usize = 8 * 1024 * 1024;
const MAX_SOURCES: usize = 32 * 1024;
const MAX_FIELD: usize = 4096;
const MAX_DIAGNOSTIC: usize = 16 * 1024;
const MAGIC: &[u8; 8] = b"ORCHFR01";
const END: &[u8; 8] = b"ENDREC01";
const BOOT_OFFSET: usize = 12;

#[derive(Debug)]
enum Fault {
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
                write!(f, "structural refusal at {}: {why}", path.display())
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
        || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
            if total > MAX_RECORD {
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
        for (path, content) in &self.sources {
            source_path(path)?;
            content_id(content)?;
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
            cursor.take(16)?.try_into().map_err(|_| Fault::Codec("boot field"))?,
        )?;
        let end_ns = i128::from_be_bytes(
            cursor.take(16)?.try_into().map_err(|_| Fault::Codec("end field"))?,
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
        if self.sources.keys().any(|p| !current.sources.0.contains_key(p)) {
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
        let end = self.offset.checked_add(length).ok_or(Fault::Codec("length overflow"))?;
        let result = self.bytes.get(self.offset..end).ok_or(Fault::Codec("truncated record"))?;
        self.offset = end;
        Ok(result)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| Fault::Codec("integer field"))?,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recovery {
    Unsupported,
    // There is intentionally no production success value, filesystem allowlist,
    // environment switch or "local disk" guess standing in for RC18 evidence.
    #[cfg(test)]
    ShownInSimulation,
}

impl Recovery {
    fn shown(self) -> bool {
        match self {
            Self::Unsupported => false,
            #[cfg(test)]
            Self::ShownInSimulation => true,
        }
    }
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
    recovery: Recovery,
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
    UnsupportedRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Reuse,
    Reset(Reset),
    Cold,
    Refuse(Refusal),
}

// All observations are injected. This function performs no filesystem action
// and cannot validate the lease or prove RC18. Its result is never an I/O permit.
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
    if o.marker_found && !o.recovery.shown() {
        return Action::Refuse(Refusal::UnsupportedRecovery);
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
        Stored::Damaged => Action::Reset(Reset::Unreadable),
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
        return Err(boundary(path, "not the required real directory/regular leaf"));
    }
    if meta.uid() != owner || meta.dev() != device {
        return Err(boundary(path, "different owner or filesystem"));
    }
    if !directory && (meta.mode() & 0o7077 != 0 || meta.nlink() != 1) {
        return Err(boundary(path, "state leaf must be owner-only and singly linked"));
    }
    Ok(())
}

// Constant ABI bits, not a dependency on a transitive libc crate. Only these
// target ABIs are candidates. Linux arm64 and x86-64 differ for two of the bits.
fn open_flags(directory: bool) -> Result<i32> {
    #[cfg(target_os = "macos")]
    let (nofollow, nonblock, dir) = (0x100, 0x4, 0x100000);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let (nofollow, nonblock, dir) = (0x20000, 0x800, 0x10000);
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let (nofollow, nonblock, dir) = (0x8000, 0x800, 0x4000);
    #[cfg(any(
        target_os = "macos",
        all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
    ))]
    {
        Ok(nofollow | nonblock | if directory { dir } else { 0 })
    }
    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
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
            return Err(boundary(root, "worktree root must already be canonical and absolute"));
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
        let name = if self.kind == Kind::Warm { "gate-warm" } else { "gate" };
        Ok(self
            .check_directory(&self.root.join("target").join(name), true)?
            .is_some())
    }

    fn check_leaf(&self, leaf: Leaf) -> Result<Option<Metadata>> {
        if self.kind == Kind::Cold && !matches!(leaf, Leaf::Marker) {
            return Err(Fault::Unavailable("cold target has no completed record or temporary"));
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
        let before = self.check_leaf(leaf)?;
        let path = self.path(leaf);
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(write)
            .create_new(exclusive)
            .mode(0o600)
            .custom_flags(open_flags(false)?);
        // No truncate(true), create(true), append or path-based diagnostic write.
        let file = at(
            options.open(&path),
            "open state leaf no-follow/nonblocking",
            &path,
        )?;
        let opened = at(file.metadata(), "fstat state leaf", &path)?;
        check_metadata(&path, &opened, false, self.owner, self.device)?;
        self.check_parents()?;
        let now = self
            .check_leaf(leaf)?
            .ok_or_else(|| boundary(&path, "leaf disappeared"))?;
        if identity(&opened) != identity(&now)
            || (!exclusive && before.as_ref().is_none_or(|m| identity(m) != identity(&opened)))
        {
            return Err(boundary(&path, "leaf identity changed during open"));
        }
        Ok(file)
    }

    fn read_record(&self) -> Result<Stored> {
        if self.check_leaf(Leaf::Record)?.is_none() {
            return Ok(Stored::Missing);
        }
        let file = match self.open_regular(Leaf::Record, false, false) {
            Ok(file) => file,
            Err(Fault::Io { .. }) => return Ok(Stored::Damaged),
            Err(error) => return Err(error),
        };
        let opened = at(
            file.metadata(),
            "fstat record before read",
            &self.path(Leaf::Record),
        )?;
        let mut bytes = Vec::new();
        if (&file).take((MAX_RECORD + 1) as u64).read_to_end(&mut bytes).is_err() {
            return Ok(Stored::Damaged);
        }
        // Recheck the structural boundary even if bytes are malformed.
        let now = self
            .check_leaf(Leaf::Record)?
            .ok_or_else(|| boundary(&self.path(Leaf::Record), "record disappeared during read"))?;
        if identity(&opened) != identity(&now) {
            return Err(boundary(&self.path(Leaf::Record), "record replaced during read"));
        }
        Ok(match Record::decode(&bytes) {
            Ok(record) => Stored::Complete(record),
            Err(_) => Stored::Damaged,
        })
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
        self.check_parents()?;
        let before = self
            .check_directory(&self.state, false)?
            .ok_or_else(|| boundary(&self.state, "missing state directory"))?;
        let directory = at(
            OpenOptions::new()
                .read(true)
                .custom_flags(open_flags(true)?)
                .open(&self.state),
            "open state directory no-follow/nonblocking",
            &self.state,
        )?;
        let opened = at(directory.metadata(), "fstat state directory", &self.state)?;
        check_metadata(&self.state, &opened, true, self.owner, self.device)?;
        if identity(&before) != identity(&opened) {
            return Err(boundary(&self.state, "directory changed during open"));
        }
        // Installed same-version std Unix source maps sync_all to fsync on Linux
        // and F_FULLFSYNC on Apple. No fsync fallback on error. In particular,
        // Apple's directory call may fail: that is an unavailable durability
        // mechanism, not successful publication. Native evidence is still due.
        at(directory.sync_all(), "synchronize state directory", &self.state)
    }

    fn remove_leaf(&self, leaf: Leaf) -> Result<()> {
        if self.check_leaf(leaf)?.is_some() {
            at(
                fs::remove_file(self.path(leaf)),
                "remove state leaf",
                &self.path(leaf),
            )?;
        }
        Ok(())
    }

    fn admit(&self, configured_lease_observed: bool) -> Result<Marker<'_>> {
        if !configured_lease_observed {
            return Err(Fault::Unavailable("configured lease not observed"));
        }
        self.check_parents()?;
        for leaf in self.leaves() {
            self.check_leaf(*leaf)?;
        }
        // There is no bypass boolean or environment opt-in. The standard library
        // has no approved stable openat/renameat/unlinkat/mount-confined removal
        // interface (its documented Dir interface still requires feature dirfd).
        // This guard is BEFORE mkdir, marker creation and lock acquisition.
        Err(Fault::Unavailable(
            "native namespace confinement and directory durability not qualified",
        ))
    }

    fn create_directories_candidate(&self) -> Result<()> {
        // Candidate source only; admit never reaches this. No recursive mkdir
        // through unknown parents, no repair of pre-existing directory modes.
        for path in [self.root.join("target"), self.state.clone()] {
            self.check_parents()?;
            if self.check_directory(&path, true)?.is_none() {
                at(
                    fs::DirBuilder::new().mode(0o700).create(&path),
                    "create state directory",
                    &path,
                )?;
                self.check_directory(&path, false)?;
                // Parent creation durability needs its own reviewed platform
                // implementation before this candidate can be used by F2.
            }
        }
        Ok(())
    }

    fn lock_marker_primitive(&self) -> Result<Marker<'_>> {
        // Private primitive for review/child-free controls. Native admission
        // above is deliberately unavailable; no tier can reach this primitive.
        let found = self.check_leaf(Leaf::Marker)?.is_some();
        let file = self.open_regular(Leaf::Marker, true, !found)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Fault::Busy),
            Err(TryLockError::Error(e)) => {
                return at(Err(e), "try marker OFD-class lock", &self.path(Leaf::Marker));
            }
        }
        let marker = Marker {
            files: self,
            file,
            found,
            temporary: None,
            phase: Phase::Locked,
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

struct Marker<'a> {
    files: &'a StateFiles,
    file: File,
    found: bool,
    temporary: Option<File>,
    phase: Phase,
}

// No Drop implementation: drop closes owned Files, NEVER unlocks or unlinks.
// No clone/raw-FD/stdio accessor is supplied here. F2 must choose and review the
// descriptor hand-over route and its R4/R11 consequences separately.
impl Marker<'_> {
    fn check_identity(&self) -> Result<()> {
        self.files.check_parents()?;
        let held = at(
            self.file.metadata(),
            "fstat locked marker",
            &self.files.path(Leaf::Marker),
        )?;
        match self.files.check_leaf(Leaf::Marker)? {
            Some(now) if identity(&held) == identity(&now) => {
                check_metadata(
                    &self.files.path(Leaf::Marker),
                    &held,
                    false,
                    self.files.owner,
                    self.files.device,
                )
            }
            _ => Err(Fault::MarkerChanged),
        }
    }

    fn diagnostics(&mut self) -> Result<Vec<u8>> {
        // Never open a second descriptor of the marker, even for diagnostics.
        self.check_identity()?;
        let path = self.files.path(Leaf::Marker);
        at(self.file.seek(SeekFrom::Start(0)), "seek marker diagnostic", &path)?;
        let mut bytes = Vec::new();
        at(
            (&mut self.file).take((MAX_DIAGNOSTIC + 1) as u64).read_to_end(&mut bytes),
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
        at(self.file.seek(SeekFrom::Start(0)), "seek checked marker", &path)?;
        at(self.file.write_all(bytes), "write checked marker", &path)?;
        at(self.file.set_len(bytes.len() as u64), "size checked marker", &path)
    }

    fn protect(&mut self) -> Result<()> {
        if self.phase != Phase::Locked {
            return Err(Fault::Unavailable("invalid protect order"));
        }
        self.step(Step::MarkerSync, None)?;
        self.step(Step::DirectorySync, None)?;
        self.phase = Phase::Protected;
        Ok(())
    }

    fn invalidate(&mut self) -> Result<()> {
        if self.phase != Phase::Protected {
            return Err(Fault::Unavailable("invalid invalidation order"));
        }
        // F2 inserts its structurally checked target decision after protect and
        // before invalidate. These are state operations, not recursive deletion.
        if self.files.kind == Kind::Warm {
            self.step(Step::RemoveTemporary, None)?;
            self.step(Step::RemoveRecord, None)?;
        }
        self.step(Step::DirectorySync, None)?;
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
            return Err(Fault::Unavailable("warm finish before invalidation or for cold target"));
        }
        if !finished.direct_reaped || !finished.lease_held || !finished.inherited_holder_valid {
            self.phase = Phase::Failed;
            return Err(Fault::Unavailable("use not finished; keep marker and target"));
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
            let boot = current.boot.ok_or(Fault::Unavailable("current boot identity unavailable"))?;
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
            return Err(Fault::Unavailable("cold finish before invalidation or for warm target"));
        }
        if !finished.direct_reaped || !finished.lease_held || !finished.inherited_holder_valid {
            self.phase = Phase::Failed;
            return Err(Fault::Unavailable("use not finished; keep marker and target"));
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

// The logical journal records the state-file subsequence of D44. Target
// decisions/builds/captures and real crash/power-loss injection are F2 work.
fn begin_order(io: &mut impl StateIo) -> Result<()> {
    for step in [
        Step::MarkerSync,
        Step::DirectorySync,
        Step::RemoveTemporary,
        Step::RemoveRecord,
        Step::DirectorySync,
    ] {
        io.step(step, None)?;
    }
    Ok(())
}

fn finish_order(io: &mut impl StateIo, bytes: Option<&[u8]>) -> Result<()> {
    if let Some(bytes) = bytes {
        io.step(Step::WriteTemporary, Some(bytes))?;
        io.step(Step::TemporarySync, None)?;
        io.step(Step::RenameRecord, None)?;
        io.step(Step::DirectorySync, None)?;
    }
    // No post-unlink directory sync: D44 permits resurrection of the marker on
    // power loss (a rebuild). A failing operation after unlink could not keep
    // that same inode at its path. Close follows this final successful act.
    io.step(Step::RemoveMarker, None)
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
                let now = self
                    .files
                    .check_leaf(Leaf::Temporary)?
                    .ok_or_else(|| boundary(&self.files.path(Leaf::Temporary), "temporary disappeared"))?;
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
                at(
                    fs::rename(self.files.path(Leaf::Temporary), self.files.path(Leaf::Record)),
                    "rename completed record",
                    &self.files.path(Leaf::Record),
                )?;
                self.temporary = None;
                Ok(())
            }
            Step::RemoveMarker => {
                at(
                    fs::remove_file(self.files.path(Leaf::Marker)),
                    "remove finished marker",
                    &self.files.path(Leaf::Marker),
                )?;
                self.phase = Phase::Removed;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
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
            recovery: Recovery::Unsupported,
        }
    }

    #[test]
    fn same_boot_complete_record_reuses_existing_warm_target() {
        assert_eq!(
            decide(observation(), &Stored::Complete(record()), context(&sources())),
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
        assert_eq!(decide(observation(), &r, c), Action::Reset(Reset::Toolchain));
        let mut changed = sources();
        changed.0.get_mut(b"src/main.rs".as_slice()).unwrap().content = "b".repeat(40);
        assert_eq!(
            decide(observation(), &r, context(&changed)),
            Action::Reset(Reset::Content),
        );
        changed.0.get_mut(b"src/main.rs".as_slice()).unwrap().modified_ns = Some(201);
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
            o.recovery = Recovery::ShownInSimulation;
            assert_eq!(
                decide(o, &Stored::Complete(record()), context(&sources())),
                Action::Reset(Reset::Unfinished),
            );
            o.recovery = Recovery::Unsupported;
            assert_eq!(
                decide(o, &Stored::Complete(record()), context(&sources())),
                Action::Refuse(Refusal::UnsupportedRecovery),
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
    fn cold_always_rebuilds_but_never_bypasses_unsupported_recovery() {
        let mut o = observation();
        o.kind = Kind::Cold;
        assert_eq!(decide(o, &Stored::Missing, context(&sources())), Action::Cold);
        o.marker_found = true;
        assert_eq!(
            decide(o, &Stored::Missing, context(&sources())),
            Action::Refuse(Refusal::UnsupportedRecovery),
        );
        o.recovery = Recovery::ShownInSimulation;
        assert_eq!(decide(o, &Stored::Missing, context(&sources())), Action::Cold);
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
    fn invalid_schema_missing_boot_and_field_count_limits_are_damaged() {
        let encoded = record().encode().unwrap();
        let mut bad_schema = encoded.clone();
        bad_schema[0] ^= 1;
        assert!(Record::decode(&bad_schema).is_err());
        let mut no_boot = encoded.clone();
        no_boot.drain(BOOT_OFFSET..BOOT_OFFSET + 16);
        let length = (no_boot.len() as u32).to_be_bytes();
        no_boot[8..12].copy_from_slice(&length);
        assert!(Record::decode(&no_boot).is_err());
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
        current.0.insert(b"old.rs".to_vec(), Source {
            path: b"old.rs".to_vec(), content: "b".repeat(40), modified_ns: Some(200),
        });
        assert_eq!(decide(observation(), &Stored::Complete(record()), context(&current)), Action::Reset(Reset::Content));
        current.0.get_mut(b"old.rs".as_slice()).unwrap().modified_ns = Some(201);
        assert_eq!(decide(observation(), &Stored::Complete(record()), context(&current)), Action::Reuse);
        let empty = Sources::new(vec![]).unwrap();
        assert_eq!(decide(observation(), &Stored::Complete(record()), context(&empty)), Action::Reset(Reset::Content));
    }

    #[test]
    fn duplicate_unsafe_paths_and_invalid_content_are_not_complete_sources() {
        let s = sources().0.into_values().next().unwrap();
        assert!(Sources::new(vec![s.clone(), s.clone()]).is_err());
        for path in [b"../outside".as_slice(), b"/absolute", b"a/../b", b"a//b", b"a\0b", b"."] {
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
        changed.0.get_mut(b"src/main.rs".as_slice()).unwrap().content = "b".repeat(40);
        assert_eq!(publication(&a, &changed, Some(boot())), Publication::SourceChanged);
        let empty = Sources::new(vec![]).unwrap();
        assert_eq!(publication(&a, &empty, Some(boot())), Publication::SourceChanged);
        assert_eq!(publication(&empty, &a, Some(boot())), Publication::SourceChanged);
    }

    #[test]
    fn removal_states_characterize_the_disclosed_manual_removal_residual() {
        // State simulations only: no cargo clean, crash or live writer is proved.
        let mut o = observation();
        o.target_exists = false;
        assert_eq!(decide(o, &Stored::Missing, context(&sources())), Action::Reset(Reset::NoRecord));
        o.target_exists = true;
        assert_eq!(decide(o, &Stored::Missing, context(&sources())), Action::Reset(Reset::NoRecord));
        o.target_exists = false;
        o.marker_found = true;
        o.recovery = Recovery::ShownInSimulation;
        assert_eq!(decide(o, &Stored::Complete(record()), context(&sources())), Action::Reset(Reset::Unfinished));
        o.lock_granted = false;
        assert_eq!(decide(o, &Stored::Complete(record()), context(&sources())), Action::Refuse(Refusal::Busy));
    }

    #[test]
    fn crash_prefixes_never_reuse_an_old_record_without_a_marker() {
        // Logical crash prefixes; these do not prove stable storage or a live death.
        let mut io = Journal::default();
        begin_order(&mut io).unwrap();
        finish_order(&mut io, Some(b"new-record")).unwrap();
        assert_eq!(io.events, [Step::MarkerSync, Step::DirectorySync, Step::RemoveTemporary,
            Step::RemoveRecord, Step::DirectorySync, Step::WriteTemporary, Step::TemporarySync,
            Step::RenameRecord, Step::DirectorySync, Step::RemoveMarker]);
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
        begin_order(&mut reference).unwrap();
        finish_order(&mut reference, Some(b"new")).unwrap();
        for fail in 0..reference.events.len() {
            let mut io = Journal { fail: Some(fail), ..Journal::default() };
            let result = begin_order(&mut io).and_then(|()| finish_order(&mut io, Some(b"new")));
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
            static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
            let private = std::env::var_os("XTASK_F1_PRIVATE_ROOT")
                .expect("future recipe must set a private absolute fixture root");
            let private = PathBuf::from(private);
            assert!(private.is_absolute());
            let private_meta = fs::symlink_metadata(&private).unwrap();
            assert!(private_meta.is_dir());
            assert_eq!(private_meta.mode() & 0o777, 0o700);
            let base = private.join(format!("case-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
            fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
            let root = base.join("repo");
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::DirBuilder::new().mode(0o700).create(root.join("target")).unwrap();
            fs::DirBuilder::new().mode(0o700).create(root.join("target/.gate-state")).unwrap();
            let root = root.canonicalize().unwrap();
            let owner = fs::symlink_metadata(&root).unwrap().uid();
            let files = StateFiles::inspect(&root, owner, Kind::Warm).unwrap();
            let outside = base.join("outside-canary");
            fs::write(&outside, b"outside stays unchanged").unwrap();
            Self { base, files, outside }
        }

        fn canary_unchanged(&self) {
            assert_eq!(fs::read(&self.outside).unwrap(), b"outside stays unchanged");
        }
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
        boundary_at(f.files.check_leaf(Leaf::Marker).map(|_| ()), &path);
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
        fs::DirBuilder::new().mode(0o700)
            .create(f.files.root.join("target/gate-warm")).unwrap();
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
        boundary_at(check_metadata(&path, &meta, false, f.files.owner.wrapping_add(1), f.files.device), &path);
        boundary_at(check_metadata(&path, &meta, false, f.files.owner, f.files.device.wrapping_add(1)), &path);
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
        assert_eq!(fs::read(f.files.path(Leaf::Temporary)).unwrap(), b"existing");
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
        let temporary = f.files.create_temporary(&record().encode().unwrap()).unwrap();
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
        assert!(matches!(f.files.remove_leaf(Leaf::Record), Err(Fault::Unavailable(_))));
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
            let replacement = recreate.then(|| f.files.open_regular(Leaf::Marker, true, true).unwrap());
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
        assert!(held.write_diagnostics(&vec![0; MAX_DIAGNOSTIC + 1]).is_err());
        let id = identity(&held.file.metadata().unwrap());
        drop(held);
        assert_eq!(identity(&fs::symlink_metadata(f.files.path(Leaf::Marker)).unwrap()), id);
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
            assert!(held.finish_warm(finished, &sources(), context(&sources()), 300).is_err());
            assert_eq!(held.phase, Phase::Failed);
            assert!(matches!(held.step(Step::RemoveMarker, None), Err(Fault::Unavailable(_))));
            assert!(f.files.path(Leaf::Marker).exists());
            assert!(!f.files.path(Leaf::Record).exists());
            f.canary_unchanged();
        }
    }

    #[test]
    fn native_admission_withholds_unproved_platform_and_lease_without_creation() {
        let f = Fixture::new();
        assert!(matches!(f.files.admit(false), Err(Fault::Unavailable("configured lease not observed"))));
        assert!(matches!(f.files.admit(true), Err(Fault::Unavailable(_))));
        assert!(!f.files.path(Leaf::Marker).exists());
        f.canary_unchanged();
    }
}
