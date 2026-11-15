//! Bounded regular-file reads and exclusive private output creation.
use anyhow::{Result, ensure};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)?;
    ensure!(before.is_file(), "not a regular file: {}", path.display());
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(super::target::open_flags(false)?)
        .open(path)?;
    let after = file.metadata()?;
    ensure!(
        before.dev() == after.dev() && before.ino() == after.ino(),
        "file identity changed: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "file exceeds budget: {}",
        path.display()
    );
    Ok(bytes)
}

pub(super) fn text(path: &Path, limit: usize) -> Result<String> {
    Ok(String::from_utf8(read(path, limit)?)?)
}
pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(super::target::open_flags(false)?)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn private_dir(parent: &Path, prefix: &str) -> Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let name = format!(
            "{prefix}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = parent.join(name);
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("exclusive directory creation exhausted")
}

pub(super) fn real_directories(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "absolute directory required");
    for p in path.ancestors() {
        let meta = fs::symlink_metadata(p)?;
        ensure!(
            meta.is_dir(),
            "directory link or special refused: {}",
            p.display()
        );
    }
    Ok(())
}

pub(super) fn sync(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
