//! R102: one component per descriptor-relative operation; never follow a link.
use nix::dir::Dir;
use nix::fcntl::{OFlag, open, openat, renameat};
use nix::sys::stat::{Mode, SFlag, fchmod, fstat, mkdirat};
use nix::unistd::{UnlinkatFlags, unlinkat};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Refusals are separate from operating-system errors so callers can name bad input.
#[derive(Debug, thiserror::Error)]
pub enum ConfinedError {
    #[error("not one path component: {0:?}")]
    InvalidName(OsString),
    #[error("symbolic link refused: {0:?}")]
    Symlink(OsString),
    #[error("not a regular file or expected directory: {0:?}")]
    WrongType(OsString),
    #[error("multiple hard links refused: {0:?}")]
    HardLink(OsString),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, ConfinedError>;

/// Data-root reads allow snapshots made with hard links. Untrusted trees and
/// in-place writes do not: a write could otherwise modify an outside inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    Read,
    Untrusted,
    Modify,
}

#[derive(Debug)]
pub struct ConfinedDir {
    file: File,
}

fn component(name: &OsStr) -> Result<()> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
        return Err(ConfinedError::InvalidName(name.to_owned()));
    }
    Ok(())
}

fn error(name: &OsStr, e: nix::errno::Errno) -> ConfinedError {
    if e == nix::errno::Errno::ELOOP {
        ConfinedError::Symlink(name.to_owned())
    } else {
        ConfinedError::Io(io::Error::from(e))
    }
}

impl ConfinedDir {
    /// The root is the owner's chosen path. Descendants never use path strings.
    pub fn open(root: &Path) -> Result<Self> {
        let fd = open(
            root,
            OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| error(root.as_os_str(), e))?;
        let stat = fstat(&fd).map_err(io::Error::from)?;
        if SFlag::from_bits_truncate(stat.st_mode) != SFlag::S_IFDIR {
            return Err(ConfinedError::WrongType(root.as_os_str().to_owned()));
        }
        Ok(Self {
            file: File::from(fd),
        })
    }

    /// Create only the last component; its parent must already exist.
    pub fn create(root: &Path) -> Result<Self> {
        let parent = root
            .parent()
            .ok_or_else(|| ConfinedError::InvalidName(root.as_os_str().to_owned()))?;
        let name = root
            .file_name()
            .ok_or_else(|| ConfinedError::InvalidName(root.as_os_str().to_owned()))?;
        Self::open(parent)?.create_dir(name)
    }

    pub fn open_dir(&self, name: impl AsRef<OsStr>) -> Result<Self> {
        let name = name.as_ref();
        component(name)?;
        let fd = openat(
            &self.file,
            name,
            OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| error(name, e))?;
        let stat = fstat(&fd).map_err(io::Error::from)?;
        if SFlag::from_bits_truncate(stat.st_mode) != SFlag::S_IFDIR {
            return Err(ConfinedError::WrongType(name.to_owned()));
        }
        Ok(Self {
            file: File::from(fd),
        })
    }

    /// Exclusive directory creation; never change an existing entry's mode.
    pub fn create_dir(&self, name: impl AsRef<OsStr>) -> Result<Self> {
        let name = name.as_ref();
        component(name)?;
        mkdirat(&self.file, name, Mode::S_IRWXU).map_err(|e| error(name, e))?;
        let dir = self.open_dir(name)?;
        fchmod(&dir.file, Mode::S_IRWXU).map_err(io::Error::from)?;
        Ok(dir)
    }

    pub fn ensure_dir(&self, name: impl AsRef<OsStr>) -> Result<Self> {
        match self.create_dir(name.as_ref()) {
            Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
                self.open_dir(name)
            }
            result => result,
        }
    }

    pub fn open_file(&self, name: impl AsRef<OsStr>, access: FileAccess) -> Result<File> {
        let name = name.as_ref();
        component(name)?;
        let mode = if access == FileAccess::Modify {
            OFlag::O_RDWR
        } else {
            OFlag::O_RDONLY
        };
        let fd = openat(
            &self.file,
            name,
            mode | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|e| error(name, e))?;
        let stat = fstat(&fd).map_err(io::Error::from)?;
        if SFlag::from_bits_truncate(stat.st_mode) != SFlag::S_IFREG {
            return Err(ConfinedError::WrongType(name.to_owned()));
        }
        if access != FileAccess::Read && stat.st_nlink != 1 {
            return Err(ConfinedError::HardLink(name.to_owned()));
        }
        Ok(File::from(fd))
    }

    pub fn create_file(&self, name: impl AsRef<OsStr>) -> Result<File> {
        self.create_file_mode(name.as_ref(), Mode::S_IRUSR | Mode::S_IWUSR)
    }

    /// Only the project pointer uses 0644; all data files use create_file.
    pub fn create_project_pointer(&self) -> Result<File> {
        self.create_file_mode(OsStr::new(".orch-project"), Mode::from_bits_truncate(0o644))
    }

    fn create_file_mode(&self, name: &OsStr, mode: Mode) -> Result<File> {
        component(name)?;
        let fd = openat(
            &self.file,
            name,
            OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            mode,
        )
        .map_err(|e| error(name, e))?;
        fchmod(&fd, mode).map_err(io::Error::from)?;
        Ok(File::from(fd))
    }

    /// Names are listing results, not paths or evidence of an entry's type.
    pub fn list(&self) -> Result<Vec<OsString>> {
        // A fresh open description keeps concurrent iterators' offsets separate.
        let fd = openat(
            &self.file,
            ".",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let mut dir = Dir::from_fd(fd).map_err(io::Error::from)?;
        let mut names = Vec::new();
        for entry in dir.iter() {
            let entry = entry.map_err(io::Error::from)?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name != "." && name != ".." {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    /// renameat replaces a destination link itself, never its target.
    pub fn rename(
        &self,
        from: impl AsRef<OsStr>,
        to_dir: &Self,
        to: impl AsRef<OsStr>,
    ) -> Result<()> {
        component(from.as_ref())?;
        component(to.as_ref())?;
        self.open_file(from.as_ref(), FileAccess::Modify)?;
        renameat(&self.file, from.as_ref(), &to_dir.file, to.as_ref()).map_err(io::Error::from)?;
        Ok(())
    }

    pub fn remove_file(&self, name: impl AsRef<OsStr>) -> Result<()> {
        self.open_file(name.as_ref(), FileAccess::Read)?;
        unlinkat(&self.file, name.as_ref(), UnlinkatFlags::NoRemoveDir).map_err(io::Error::from)?;
        Ok(())
    }

    pub fn remove_dir(&self, name: impl AsRef<OsStr>) -> Result<()> {
        self.open_dir(name.as_ref())?;
        unlinkat(&self.file, name.as_ref(), UnlinkatFlags::RemoveDir).map_err(io::Error::from)?;
        Ok(())
    }

    pub fn sync(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }
}

impl AsFd for ConfinedDir {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}
