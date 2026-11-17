//! Checked descriptor-relative native operations under the cooperative lease.
//!
//! Safe nix APIs close the pathname traversal gap, but do not pin a directory
//! inside the worktree against relocation, or condition unlink/rename on the
//! inspected inode. The owner accepts that last-check/mutation race temporarily
//! (D45 amendment); these calls do not claim inode-conditional atomicity.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path};

use nix::dir::Dir;
use nix::fcntl::{AtFlags, OFlag, open, openat, renameat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, geteuid, unlinkat};

use super::{
    Fault, Identity, Kind, Leaf, Result, StateFiles, at, boundary, check_metadata, identity,
};

// Inspection budgets, not a deadline or a supported bound on filesystem calls.
// Exhaustion refuses before any target mutation. A caller never trusts a prefix.
const MAX_DEPTH: usize = 128;
const MAX_ENTRIES: usize = 1_000_000;

fn flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC
}

fn syscall<T>(result: nix::Result<T>, operation: &'static str, path: &Path) -> Result<T> {
    at(result.map_err(std::io::Error::from), operation, path)
}

fn single_name(name: &OsStr, path: &Path) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(boundary(path, "expected exactly one native path component"));
    }
    Ok(())
}

fn stat(parent: &File, name: &OsStr, path: &Path) -> Result<Option<FileStat>> {
    single_name(name, path)?;
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(s) => Ok(Some(s)),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(error) => syscall(Err(error), "fstatat no-follow", path),
    }
}

// Match std MetadataExt::dev: Apple's dev_t is signed, Linux's is u64.
#[allow(
    clippy::unnecessary_cast,
    reason = "dev_t has different widths/signs on native targets"
)]
fn device(stat: &FileStat) -> u64 {
    stat.st_dev as u64
}

fn stat_identity(stat: &FileStat) -> Identity {
    Identity {
        device: device(stat),
        inode: stat.st_ino,
    }
}

fn check_stat(files: &StateFiles, path: &Path, stat: &FileStat, directory: bool) -> Result<()> {
    let kind = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
    if kind
        != if directory {
            SFlag::S_IFDIR
        } else {
            SFlag::S_IFREG
        }
    {
        return Err(boundary(
            path,
            "not the required real directory/regular leaf",
        ));
    }
    if stat.st_uid != files.owner || device(stat) != files.device {
        return Err(boundary(path, "different owner or filesystem"));
    }
    if !directory && (stat.st_mode & 0o7077 != 0 || stat.st_nlink != 1) {
        return Err(boundary(
            path,
            "state leaf must be owner-only and singly linked",
        ));
    }
    Ok(())
}

fn directory(files: &StateFiles, parent: &File, name: &OsStr, path: &Path) -> Result<Option<File>> {
    let Some(before) = stat(parent, name, path)? else {
        return Ok(None);
    };
    check_stat(files, path, &before, true)?;
    let file = File::from(syscall(
        openat(parent, name, flags(), Mode::empty()),
        "openat directory no-follow",
        path,
    )?);
    let opened = at(file.metadata(), "fstat opened directory", path)?;
    check_metadata(path, &opened, true, files.owner, files.device)?;
    let after = stat(parent, name, path)?.ok_or_else(|| boundary(path, "directory disappeared"))?;
    check_stat(files, path, &after, true)?;
    if stat_identity(&before) != identity(&opened) || stat_identity(&after) != identity(&opened) {
        return Err(boundary(
            path,
            "directory replaced during descriptor inspection",
        ));
    }
    Ok(Some(file))
}

// Walk from / with one-component openat calls. O_NOFOLLOW on a full absolute
// pathname protects only its final component and is insufficient here. Ancestor
// ownership/devices may differ; the canonical worktree root and everything
// below it must match the effective account and root device.
fn root(files: &StateFiles) -> Result<File> {
    if files.owner != geteuid().as_raw() {
        return Err(boundary(
            &files.root,
            "root owner is not the effective account",
        ));
    }
    let mut file = File::from(syscall(
        open(Path::new("/"), flags(), Mode::empty()),
        "open filesystem root",
        Path::new("/"),
    )?);
    for component in files.root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                single_name(name, &files.root)?;
                file = File::from(syscall(
                    openat(&file, name, flags(), Mode::empty()),
                    "openat canonical root component",
                    &files.root,
                )?);
            }
            _ => return Err(boundary(&files.root, "noncanonical root component")),
        }
    }
    let meta = at(file.metadata(), "fstat native root", &files.root)?;
    check_metadata(&files.root, &meta, true, files.owner, files.device)?;
    if identity(&meta) != files.root_id {
        return Err(boundary(&files.root, "root replaced"));
    }
    Ok(file)
}

pub(super) struct Boundary<'a> {
    files: &'a StateFiles,
    root: File,
    target: Option<File>,
    state: Option<File>,
    build: Option<File>,
}

impl<'a> Boundary<'a> {
    pub(super) fn inspect(files: &'a StateFiles) -> Result<Self> {
        Self::inspect_inner(files, true)
    }

    pub(super) fn state(files: &'a StateFiles) -> Result<Self> {
        Self::inspect_inner(files, false)
    }

    fn inspect_inner(files: &'a StateFiles, scan_build: bool) -> Result<Self> {
        let root = root(files)?;
        let target = directory(
            files,
            &root,
            OsStr::new("target"),
            &files.root.join("target"),
        )?;
        let mut state = None;
        let mut build = None;
        if let Some(target) = &target {
            state = directory(files, target, OsStr::new(".gate-state"), &files.state)?;
            let name = match files.kind {
                Kind::Warm => "gate-warm",
                Kind::Cold => "gate",
            };
            let path = files.root.join("target").join(name);
            build = directory(files, target, OsStr::new(name), &path)?;
            if scan_build && let Some(build) = &build {
                let mut remaining = MAX_ENTRIES;
                scan(files, build, &path, 0, &mut remaining)?;
            }
        }
        let boundary = Self {
            files,
            root,
            target,
            state,
            build,
        };
        if let Some(state) = &boundary.state {
            for leaf in files.leaves() {
                let path = files.path(*leaf);
                let name = path
                    .file_name()
                    .ok_or_else(|| super::boundary(&path, "missing state name"))?;
                if let Some(meta) = stat(state, name, &path)? {
                    check_stat(files, &path, &meta, false)?;
                }
            }
        }
        Ok(boundary)
    }

    pub(super) fn recheck(&self) -> Result<()> {
        let now = Self::state(self.files)?;
        for (before, after) in [
            (Some(&self.root), Some(&now.root)),
            (self.target.as_ref(), now.target.as_ref()),
            (self.state.as_ref(), now.state.as_ref()),
            (self.build.as_ref(), now.build.as_ref()),
        ] {
            let id = |f: Option<&File>| -> Result<Option<Identity>> {
                f.map(|f| {
                    at(f.metadata(), "fstat pinned directory", &self.files.root)
                        .map(|m| identity(&m))
                })
                .transpose()
            };
            if id(before)? != id(after)? {
                return Err(boundary(
                    &self.files.root,
                    "pinned native directory no longer in namespace",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn read_regular(&self, leaf: Leaf) -> Result<Option<File>> {
        self.recheck()?;
        if self.files.kind == Kind::Cold && !matches!(leaf, Leaf::Marker) {
            return Err(Fault::Unavailable(
                "cold target has no completed record or temporary",
            ));
        }
        let Some(state) = &self.state else {
            return Ok(None);
        };
        let path = self.files.path(leaf);
        let name = path
            .file_name()
            .ok_or_else(|| boundary(&path, "missing state name"))?;
        let Some(before) = stat(state, name, &path)? else {
            return Ok(None);
        };
        check_stat(self.files, &path, &before, false)?;
        let file = File::from(syscall(
            openat(
                state,
                name,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                Mode::empty(),
            ),
            "openat state leaf no-follow/nonblocking",
            &path,
        )?);
        let meta = at(file.metadata(), "fstat opened state", &path)?;
        check_metadata(&path, &meta, false, self.files.owner, self.files.device)?;
        let after = stat(state, name, &path)?
            .ok_or_else(|| boundary(&path, "state disappeared during open"))?;
        check_stat(self.files, &path, &after, false)?;
        if stat_identity(&before) != identity(&meta) || stat_identity(&after) != identity(&meta) {
            return Err(boundary(&path, "state replaced during descriptor open"));
        }
        self.recheck()?;
        Ok(Some(file))
    }

    pub(super) fn sync_existing(&self) -> Result<()> {
        self.recheck()?;
        // Same-version std uses fsync on Linux and F_FULLFSYNC on Apple. Keep
        // that durability, including errors; plain fsync is no Apple fallback.
        for (file, path) in [
            (self.state.as_ref(), self.files.state.clone()),
            (self.target.as_ref(), self.files.root.join("target")),
            (Some(&self.root), self.files.root.clone()),
        ] {
            if let Some(file) = file {
                at(
                    file.sync_all(),
                    "synchronize pinned native directory",
                    &path,
                )?;
            }
        }
        self.recheck()
    }

    pub(super) fn create_directories(files: &'a StateFiles) -> Result<()> {
        let root = root(files)?;
        let target_path = files.root.join("target");
        let target = make_directory(files, &root, OsStr::new("target"), &target_path)?;
        make_directory(files, &target, OsStr::new(".gate-state"), &files.state)?;
        Self::state(files)?.sync_existing()
    }

    pub(super) fn open_regular(&self, leaf: Leaf, write: bool, exclusive: bool) -> Result<File> {
        self.recheck()?;
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| boundary(&self.files.state, "missing state directory"))?;
        let path = self.files.path(leaf);
        let name = path
            .file_name()
            .ok_or_else(|| boundary(&path, "missing state name"))?;
        let before = stat(state, name, &path)?;
        if let Some(meta) = &before {
            check_stat(self.files, &path, meta, false)?;
        }
        let mut flags = OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
        flags |= if write {
            OFlag::O_RDWR
        } else {
            OFlag::O_RDONLY
        };
        if exclusive {
            flags |= OFlag::O_CREAT | OFlag::O_EXCL;
        }
        let file = File::from(syscall(
            openat(state, name, flags, Mode::S_IRUSR | Mode::S_IWUSR),
            "openat checked state",
            &path,
        )?);
        let held = at(file.metadata(), "fstat checked state", &path)?;
        check_metadata(&path, &held, false, self.files.owner, self.files.device)?;
        let now = stat(state, name, &path)?
            .ok_or_else(|| boundary(&path, "state disappeared during open"))?;
        check_stat(self.files, &path, &now, false)?;
        if stat_identity(&now) != identity(&held)
            || (!exclusive
                && before
                    .as_ref()
                    .is_none_or(|m| stat_identity(m) != identity(&held)))
        {
            return Err(boundary(&path, "state replaced during descriptor open"));
        }
        self.recheck()?;
        Ok(file)
    }

    pub(super) fn remove_leaf(&self, leaf: Leaf) -> Result<()> {
        self.recheck()?;
        let Some(state) = &self.state else {
            return Ok(());
        };
        let path = self.files.path(leaf);
        let name = path
            .file_name()
            .ok_or_else(|| boundary(&path, "missing state name"))?;
        if let Some(meta) = stat(state, name, &path)? {
            check_stat(self.files, &path, &meta, false)?;
            syscall(
                unlinkat(state, name, UnlinkatFlags::NoRemoveDir),
                "unlinkat checked state",
                &path,
            )?;
        }
        Ok(())
    }

    pub(super) fn rename_record(&self, expected: Identity) -> Result<()> {
        self.recheck()?;
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| boundary(&self.files.state, "missing state directory"))?;
        let temporary = self.files.path(Leaf::Temporary);
        let record = self.files.path(Leaf::Record);
        let from = temporary
            .file_name()
            .ok_or_else(|| boundary(&temporary, "missing temporary name"))?;
        let to = record
            .file_name()
            .ok_or_else(|| boundary(&record, "missing record name"))?;
        let now = stat(state, from, &temporary)?
            .ok_or_else(|| boundary(&temporary, "temporary disappeared"))?;
        check_stat(self.files, &temporary, &now, false)?;
        if stat_identity(&now) != expected {
            return Err(boundary(&temporary, "temporary replaced"));
        }
        if stat(state, to, &record)?.is_some() {
            return Err(boundary(&record, "record appeared after invalidation"));
        }
        syscall(
            renameat(state, from, state, to),
            "renameat completed record",
            &record,
        )
    }

    // Preflight the entire tree before deleting anything. During removal repeat
    // the checks at each component; never follow an artifact symlink. A racing
    // rename after the final check is the disclosed D45 residual.
    pub(super) fn remove_build(&self) -> Result<()> {
        self.recheck()?;
        let Some(target) = &self.target else {
            return Ok(());
        };
        let name = self.files.build_name();
        let path = self.files.build_path();
        if let Some(build) = directory(self.files, target, OsStr::new(name), &path)? {
            let mut remaining = MAX_ENTRIES;
            scan(self.files, &build, &path, 0, &mut remaining)?;
            remaining = MAX_ENTRIES;
            remove_contents(self.files, &build, &path, 0, &mut remaining)?;
            let now = stat(target, OsStr::new(name), &path)?
                .ok_or_else(|| boundary(&path, "target disappeared before removal"))?;
            check_stat(self.files, &path, &now, true)?;
            if stat_identity(&now)
                != identity(&at(build.metadata(), "fstat removed target", &path)?)
            {
                return Err(boundary(&path, "target replaced before removal"));
            }
            syscall(
                unlinkat(target, name, UnlinkatFlags::RemoveDir),
                "unlinkat target directory",
                &path,
            )?;
            at(
                target.sync_all(),
                "synchronize target parent",
                &self.files.root.join("target"),
            )?;
        }
        Ok(())
    }

    pub(super) fn create_build(&self) -> Result<()> {
        self.recheck()?;
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| boundary(&self.files.root.join("target"), "missing target parent"))?;
        make_directory(
            self.files,
            target,
            OsStr::new(self.files.build_name()),
            &self.files.build_path(),
        )?;
        Ok(())
    }
}

fn scan(
    files: &StateFiles,
    parent: &File,
    path: &Path,
    depth: usize,
    remaining: &mut usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Fault::Unavailable("native target inspection depth limit"));
    }
    // Opening . creates a separate directory description. Duplicating the
    // parent's FD would share directory offsets with another reader.
    let mut dir = syscall(
        Dir::openat(parent, ".", flags(), Mode::empty()),
        "openat target enumeration",
        path,
    )?;
    let mut names = Vec::new();
    for entry in dir.iter() {
        let entry = syscall(entry, "read native target directory", path)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Fault::Unavailable("native target inspection entry limit"))?;
        names.push(OsString::from_vec(name.to_vec()));
    }
    // Do not retain both the enumeration stream and the pinned directory at
    // every recursive level: the extra descriptors can exhaust the native
    // soft limit before the explicit depth refusal can be reached.
    drop(dir);
    names.sort();
    for name in names {
        let child = path.join(&name);
        let meta = stat(parent, &name, &child)?
            .ok_or_else(|| boundary(&child, "target entry disappeared during inspection"))?;
        if meta.st_uid != files.owner || device(&meta) != files.device {
            return Err(boundary(&child, "different owner or filesystem"));
        }
        match SFlag::from_bits_truncate(meta.st_mode) & SFlag::S_IFMT {
            SFlag::S_IFDIR => {
                let directory = directory(files, parent, &name, &child)?
                    .ok_or_else(|| boundary(&child, "target directory disappeared"))?;
                scan(files, &directory, &child, depth + 1, remaining)?;
            }
            SFlag::S_IFREG | SFlag::S_IFLNK => {} // Never follow artifact symlinks.
            _ => return Err(boundary(&child, "special entry inside target")),
        }
    }
    Ok(())
}

fn make_directory(files: &StateFiles, parent: &File, name: &OsStr, path: &Path) -> Result<File> {
    if let Some(file) = directory(files, parent, name, path)? {
        return Ok(file);
    }
    syscall(
        mkdirat(parent, name, Mode::S_IRWXU),
        "mkdirat private directory",
        path,
    )?;
    let file = directory(files, parent, name, path)?
        .ok_or_else(|| boundary(path, "created directory disappeared"))?;
    at(parent.sync_all(), "synchronize new directory parent", path)?;
    Ok(file)
}

fn remove_contents(
    files: &StateFiles,
    parent: &File,
    path: &Path,
    depth: usize,
    remaining: &mut usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Fault::Unavailable("native target removal depth limit"));
    }
    let mut dir = syscall(
        Dir::openat(parent, ".", flags(), Mode::empty()),
        "openat removal enumeration",
        path,
    )?;
    let mut names = Vec::new();
    for entry in dir.iter() {
        let entry = syscall(entry, "read removal directory", path)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Fault::Unavailable("native target removal entry limit"))?;
        names.push(OsString::from_vec(name.to_vec()));
    }
    drop(dir);
    names.sort();
    for name in names {
        let child = path.join(&name);
        let meta = stat(parent, &name, &child)?
            .ok_or_else(|| boundary(&child, "entry disappeared during removal"))?;
        if meta.st_uid != files.owner || device(&meta) != files.device {
            return Err(boundary(&child, "different owner or filesystem"));
        }
        match SFlag::from_bits_truncate(meta.st_mode) & SFlag::S_IFMT {
            SFlag::S_IFDIR => {
                let dir = directory(files, parent, &name, &child)?
                    .ok_or_else(|| boundary(&child, "directory disappeared during removal"))?;
                remove_contents(files, &dir, &child, depth + 1, remaining)?;
                let now = stat(parent, &name, &child)?
                    .ok_or_else(|| boundary(&child, "directory disappeared before unlink"))?;
                check_stat(files, &child, &now, true)?;
                if stat_identity(&now)
                    != identity(&at(dir.metadata(), "fstat removal directory", &child)?)
                {
                    return Err(boundary(&child, "directory replaced before unlink"));
                }
                syscall(
                    unlinkat(parent, name.as_os_str(), UnlinkatFlags::RemoveDir),
                    "unlinkat child directory",
                    &child,
                )?;
            }
            SFlag::S_IFREG | SFlag::S_IFLNK => {
                syscall(
                    unlinkat(parent, name.as_os_str(), UnlinkatFlags::NoRemoveDir),
                    "unlinkat target entry no-follow",
                    &child,
                )?;
            }
            _ => return Err(boundary(&child, "special entry inside target")),
        }
    }
    Ok(())
}
