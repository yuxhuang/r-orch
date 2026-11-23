//! What the importer may ask of the source directory: a bounded listing of one level,
//! and the kind of a listed name without following it (R103). Nothing here builds a
//! path; every call takes a descriptor and one name the listing gave.
use crate::data_dir::confined::ConfinedDir;
use nix::dir::Dir;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, SFlag, fstatat};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;

/// What a listed name is, read with `AT_SYMLINK_NOFOLLOW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Regular { links: u64 },
    Directory,
    Symlink,
    Fifo,
    Socket,
    CharDevice,
    BlockDevice,
    Other,
}

impl Kind {
    /// R103: a file where the table expects one has exactly one hard link.
    pub(super) fn why_not_file(self) -> Option<&'static str> {
        match self {
            Self::Regular { links: 1 } => None,
            Self::Regular { .. } => Some("a file with more than one hard link"),
            Self::Directory => Some("a directory where a file is expected"),
            Self::Symlink => Some("a symbolic link"),
            Self::Fifo => Some("a FIFO"),
            Self::Socket => Some("a socket"),
            Self::CharDevice | Self::BlockDevice => Some("a device"),
            Self::Other => Some("a special file"),
        }
    }
    pub(super) fn why_not_directory(self) -> Option<&'static str> {
        match self {
            Self::Directory => None,
            Self::Regular { .. } => Some("a file where a directory is expected"),
            Self::Symlink => Some("a symbolic link"),
            Self::Fifo => Some("a FIFO"),
            Self::Socket => Some("a socket"),
            Self::CharDevice | Self::BlockDevice => Some("a device"),
            Self::Other => Some("a special file"),
        }
    }
}

#[allow(
    clippy::useless_conversion,
    reason = "st_nlink is narrower than u64 on some targets"
)]
pub(super) fn kind(dir: &ConfinedDir, name: &OsStr) -> io::Result<Kind> {
    let stat = fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    Ok(
        match SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT {
            SFlag::S_IFREG => Kind::Regular {
                links: u64::from(stat.st_nlink),
            },
            SFlag::S_IFDIR => Kind::Directory,
            SFlag::S_IFLNK => Kind::Symlink,
            SFlag::S_IFSOCK => Kind::Socket,
            SFlag::S_IFCHR => Kind::CharDevice,
            SFlag::S_IFBLK => Kind::BlockDevice,
            SFlag::S_IFIFO => Kind::Fifo,
            _ => Kind::Other,
        },
    )
}

/// The names of one directory level, sorted by bytes. `over` says that the listing had
/// more entries than the caller's budget allowed; no more than `budget` names are
/// kept, however long the directory is.
#[derive(Debug)]
pub(super) struct Listing {
    pub(super) names: Vec<OsString>,
    pub(super) over: bool,
}

/// `ConfinedDir::list` reads a whole directory into memory, which is the wrong
/// shape for a source that is not the owner's word. This stops at the budget.
pub(super) fn list_bounded(dir: &ConfinedDir, budget: u64) -> io::Result<Listing> {
    // A fresh open description keeps this iterator's offset its own.
    let fd = openat(
        dir.as_fd(),
        ".",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut reader = Dir::from_fd(fd).map_err(io::Error::from)?;
    let mut names = Vec::new();
    let mut over = false;
    for entry in reader.iter() {
        let entry = entry.map_err(io::Error::from)?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        if names.len() as u64 >= budget {
            over = true;
            break;
        }
        names.push(name.to_owned());
    }
    names.sort();
    Ok(Listing { names, over })
}
