//! Metadata only, breadth first and name ordered. A bounded comparison is never
//! described as unchanged. Concurrent real-home writers are findings too.
use anyhow::{Result, ensure};
use nix::dir::Dir;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, SFlag, fstat, fstatat};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

const DEPTH: usize = 4;
const COUNT: usize = 5000;
#[derive(Debug, Eq, PartialEq)]
struct Entry {
    kind: u64,
    size: u64,
    mtime: (i128, i128),
    inode: (i128, u64),
}
#[derive(Debug)]
pub(super) struct Snapshot {
    entries: BTreeMap<PathBuf, Entry>,
    partial: bool,
}

fn flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK
}
fn directory(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "real-home requires absolute path");
    let mut file = File::open("/")?;
    for part in path.components() {
        match part {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => {
                file = File::from(openat(&file, name, flags(), Mode::empty())?)
            }
            _ => anyhow::bail!("noncanonical real-home directory"),
        }
    }
    Ok(file)
}

pub(super) fn snapshot(home: &Path) -> Result<Snapshot> {
    let root = home.canonicalize()?.join(".orchestry");
    let mut entries = BTreeMap::new();
    let mut partial = false;
    let mut queue = VecDeque::from([(PathBuf::new(), 0usize)]);
    while let Some((relative, depth)) = queue.pop_front() {
        let path = root.join(&relative);
        let parent = directory(path.parent().unwrap())?;
        let meta = match fstatat(
            &parent,
            path.file_name().unwrap(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(meta) => meta,
            Err(nix::errno::Errno::ENOENT) if relative.as_os_str().is_empty() => continue,
            Err(e) => return Err(e.into()),
        };
        ensure!(entries.len() < COUNT, "real-home count accounting");
        let kind = SFlag::from_bits_truncate(meta.st_mode);
        entries.insert(
            relative.clone(),
            Entry {
                kind: u64::from(meta.st_mode) & 0o170000,
                size: u64::try_from(meta.st_size)?,
                mtime: (i128::from(meta.st_mtime), i128::from(meta.st_mtime_nsec)),
                inode: (i128::from(meta.st_dev), meta.st_ino),
            },
        );
        if !kind.contains(SFlag::S_IFDIR) {
            continue;
        }
        let opened = File::from(openat(
            &parent,
            path.file_name().unwrap(),
            flags(),
            Mode::empty(),
        )?);
        let after = fstat(&opened)?;
        ensure!(
            meta.st_dev == after.st_dev && meta.st_ino == after.st_ino,
            "real-home directory replaced during inspection"
        );
        let mut names = BTreeSet::new();
        let mut dir = Dir::openat(&opened, ".", flags(), Mode::empty())?;
        for item in dir.iter() {
            let item = item?;
            let bytes = item.file_name().to_bytes();
            if [b".".as_slice(), b"..".as_slice()].contains(&bytes) {
                continue;
            }
            names.insert(std::ffi::OsString::from_vec(bytes.to_vec()));
            if names.len() > COUNT {
                names.pop_last();
                partial = true;
            }
        }
        if depth == DEPTH {
            partial |= !names.is_empty();
            continue;
        }
        for name in names {
            queue.push_back((relative.join(name), depth + 1));
        }
        if entries.len() + queue.len() > COUNT {
            queue.truncate(COUNT - entries.len());
            partial = true;
        }
    }
    Ok(Snapshot { entries, partial })
}

pub(super) fn compare(before: &Snapshot, after: &Snapshot) -> Result<(String, &'static str)> {
    let names: BTreeSet<_> = before.entries.keys().chain(after.entries.keys()).collect();
    let changed: Vec<_> = names
        .into_iter()
        .filter(|p| before.entries.get(*p) != after.entries.get(*p))
        .take(20)
        .map(|p| super::report::diagnostic(p.as_os_str().as_encoded_bytes()))
        .collect();
    ensure!(
        changed.is_empty(),
        "real ~/.orchestry created or changed (a concurrent writer also fails): {}",
        changed.join(", ")
    );
    if before.partial || after.partial {
        Ok((
            format!(
                "no difference in {} entries compared; partial: depth {DEPTH} or count {COUNT} bound",
                before.entries.len()
            ),
            "partial",
        ))
    } else {
        Ok((
            format!("unchanged ({} entries)", before.entries.len()),
            "complete",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::steps::tests::Scratch;
    #[test]
    fn metadata_detects_created_and_changed_home_without_reading_contents() {
        let scratch = Scratch::new();
        let before = snapshot(&scratch.0).unwrap();
        std::fs::create_dir(scratch.0.join(".orchestry")).unwrap();
        assert!(
            compare(&before, &snapshot(&scratch.0).unwrap())
                .unwrap_err()
                .to_string()
                .contains("created")
        );
        let file = scratch.0.join(".orchestry/global.yml");
        std::fs::write(&file, b"private").unwrap();
        let before = snapshot(&scratch.0).unwrap();
        std::fs::write(file, b"different-length").unwrap();
        assert!(compare(&before, &snapshot(&scratch.0).unwrap()).is_err());
    }
    #[test]
    fn depth_limit_is_partial_and_links_are_not_followed() {
        let scratch = Scratch::new();
        let root = scratch.0.join(".orchestry");
        std::fs::create_dir_all(root.join("a/b/c/d/e")).unwrap();
        std::os::unix::fs::symlink("/", root.join("z")).unwrap();
        let snap = snapshot(&scratch.0).unwrap();
        let (text, coverage) = compare(&snap, &snapshot(&scratch.0).unwrap()).unwrap();
        assert_eq!(coverage, "partial");
        assert!(!text.contains("unchanged"));
        assert!(snap.entries.contains_key(Path::new("z")));
        assert!(!snap.entries.contains_key(Path::new("z/tmp")));
    }
}
