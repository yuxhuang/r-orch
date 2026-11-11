//! Runtime entries in `.orchestry/.gitignore` (spec R2; fact-check F3).
//!
//! The daemon creates `run/`, `daemon.json` and the detached log with its
//! one rotation file. A project that tracks parts of `.orchestry` must not
//! pick them up, so `orch init` writes these entries and every explicit start
//! appends whichever are missing — the upgrade path for a v0.13.0 project.
//!
//! This is hygiene, not a security boundary: a failure is the caller's
//! warning and never blocks READY. The file is still opened like the
//! daemon's other `.orchestry` files: never through a symlink, never a FIFO,
//! only a regular file owned by the euid, read with a bound.

use crate::detach::{LOG_BACKUP, LOG_FILE};
use nix::errno::Errno;
use nix::libc;
use orch_proto::discovery::DISCOVERY_FILE;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// The comment line `orch init` and an appending start put above the
/// runtime entries.
pub const RUNTIME_GITIGNORE_HEADER: &str = "# Daemon runtime";

/// The daemon's runtime files, one `.gitignore` line each, in the order they
/// are written. Locks stay covered by the legacy `*.lock` line.
pub const RUNTIME_GITIGNORE_ENTRIES: [&str; 4] = ["run/", DISCOVERY_FILE, LOG_FILE, LOG_BACKUP];

/// A `.gitignore` larger than this is left alone: the daemon cannot tell
/// which entries it already has without reading all of it.
const MAX_GITIGNORE_BYTES: u64 = 64 * 1024;

/// Why the runtime entries could not be ensured. Carries the step and a
/// code only, never a path or the file's contents, so it is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitignoreError {
    pub op: &'static str,
    pub code: GitignoreErrorCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitignoreErrorCode {
    /// `.orchestry/.gitignore` is a symlink; it is never followed.
    Symlink,
    /// Not a regular file (a FIFO, a socket, a device).
    NotRegularFile,
    /// Owned by a uid other than the daemon's euid.
    ForeignOwner,
    /// Over the read bound.
    TooLarge,
    /// Any other system error.
    Io(Errno),
}

/// Append the [`RUNTIME_GITIGNORE_ENTRIES`] missing from
/// `<root>/.orchestry/.gitignore`, creating it (0600) when absent. Existing
/// lines are never rewritten or reordered, so a second call appends nothing.
/// Returns how many entries were appended.
pub fn ensure_runtime_gitignore(root: &Path) -> Result<usize, GitignoreError> {
    let path = root.join(".orchestry").join(".gitignore");
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| fail("open", &error))?;
    check(&file)?;
    let mut content = Vec::new();
    (&mut file)
        .take(MAX_GITIGNORE_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|error| fail("read", &error))?;
    if content.len() as u64 > MAX_GITIGNORE_BYTES {
        return Err(GitignoreError {
            op: "read",
            code: GitignoreErrorCode::TooLarge,
        });
    }
    let text = String::from_utf8_lossy(&content);
    let has = |entry: &str| text.lines().any(|line| line.trim_end() == entry);
    let missing: Vec<&str> = RUNTIME_GITIGNORE_ENTRIES
        .into_iter()
        .filter(|entry| !has(entry))
        .collect();
    if missing.is_empty() {
        return Ok(0);
    }

    let mut block = String::new();
    if !content.is_empty() && !content.ends_with(b"\n") {
        block.push('\n');
    }
    if !has(RUNTIME_GITIGNORE_HEADER) {
        if !content.is_empty() {
            block.push('\n');
        }
        block.push_str(RUNTIME_GITIGNORE_HEADER);
        block.push('\n');
    }
    for entry in &missing {
        block.push_str(entry);
        block.push('\n');
    }
    // One write on an O_APPEND descriptor: nothing before the end moves.
    file.write_all(block.as_bytes())
        .map_err(|error| fail("append", &error))?;
    Ok(missing.len())
}

fn check(file: &File) -> Result<(), GitignoreError> {
    let meta = file.metadata().map_err(|error| fail("stat", &error))?;
    let code = if !meta.file_type().is_file() {
        GitignoreErrorCode::NotRegularFile
    } else if meta.uid() != nix::unistd::geteuid().as_raw() {
        GitignoreErrorCode::ForeignOwner
    } else {
        return Ok(());
    };
    Err(GitignoreError { op: "stat", code })
}

fn fail(op: &'static str, error: &io::Error) -> GitignoreError {
    let errno = Errno::from_raw(error.raw_os_error().unwrap_or(0));
    // O_NOFOLLOW reports a final symlink as ELOOP on Linux and macOS (EMLINK
    // on some BSDs).
    let code = match errno {
        Errno::ELOOP | Errno::EMLINK if op == "open" => GitignoreErrorCode::Symlink,
        // A FIFO without a reader under O_NONBLOCK.
        Errno::ENXIO if op == "open" => GitignoreErrorCode::NotRegularFile,
        errno => GitignoreErrorCode::Io(errno),
    };
    GitignoreError { op, code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn project() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".orchestry");
        std::fs::create_dir(&path).unwrap();
        (temp, path.join(".gitignore"))
    }

    #[test]
    fn the_entries_name_the_daemon_runtime_files() {
        assert_eq!(
            RUNTIME_GITIGNORE_ENTRIES,
            ["run/", "daemon.json", "daemon.log", "daemon.log.1"]
        );
        assert_eq!(
            RUNTIME_GITIGNORE_ENTRIES[0],
            format!("{}/", crate::discovery::RUN_DIR)
        );
    }

    #[test]
    fn an_absent_file_is_created_private_with_the_block() {
        let (temp, path) = project();
        assert_eq!(ensure_runtime_gitignore(temp.path()), Ok(4));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Daemon runtime\nrun/\ndaemon.json\ndaemon.log\ndaemon.log.1\n"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn only_missing_entries_are_appended_after_a_newline() {
        let (temp, path) = project();
        std::fs::write(&path, "daemon.json\n# Daemon runtime\nrun/   ").unwrap();
        assert_eq!(ensure_runtime_gitignore(temp.path()), Ok(2));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "daemon.json\n# Daemon runtime\nrun/   \ndaemon.log\ndaemon.log.1\n"
        );
        assert_eq!(ensure_runtime_gitignore(temp.path()), Ok(0));
    }

    #[test]
    fn an_oversized_file_is_left_alone() {
        let (temp, path) = project();
        let big = "x\n".repeat(MAX_GITIGNORE_BYTES as usize);
        std::fs::write(&path, &big).unwrap();
        assert_eq!(
            ensure_runtime_gitignore(temp.path()),
            Err(GitignoreError {
                op: "read",
                code: GitignoreErrorCode::TooLarge
            })
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), big);
    }

    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let (temp, path) = project();
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        let error = ensure_runtime_gitignore(temp.path()).unwrap_err();
        assert_eq!(error.code, GitignoreErrorCode::NotRegularFile, "{error:?}");
    }

    #[test]
    fn a_symlink_is_not_followed() {
        let (temp, path) = project();
        let target = temp.path().join("elsewhere");
        std::fs::write(&target, "keep\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(
            ensure_runtime_gitignore(temp.path()),
            Err(GitignoreError {
                op: "open",
                code: GitignoreErrorCode::Symlink
            })
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
    }
}
