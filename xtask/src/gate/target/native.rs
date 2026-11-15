//! Descriptor-relative *inspection*, not permission to mutate a target.
//!
//! Safe nix APIs close the pathname traversal gap, but do not pin a directory
//! inside the worktree against relocation, or condition unlink/rename on the
//! inspected inode. Keeping those distinctions explicit preserves D45. No
//! destructive primitive is exported from this module.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path};

use nix::dir::Dir;
use nix::fcntl::{AtFlags, OFlag, open, openat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstatat};
use nix::unistd::geteuid;

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
            if let Some(build) = &build {
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
        let now = Self::inspect(self.files)?;
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

    pub(super) fn require_mutation(&self) -> Result<()> {
        Err(Fault::Unavailable(
            "native APIs cannot pin namespace or condition unlink/rename on checked inode",
        ))
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
