//! Project selection, endpoint choice and `daemon.json` publication (design
//! D4, spec R2/R33).
//!
//! Everything here runs in the owner process only. Clients read the published
//! record through `orch-client`; nothing in this module repairs an endpoint on
//! a client's behalf.

use crate::DaemonError;
use nix::libc;
use orch_proto::discovery::{
    DISCOVERY_FILE, DaemonRecord, MAX_DISCOVERY_BYTES, MAX_SOCKET_PATH_BYTES,
};
use orch_proto::handshake::ProjectId;
use sha2::{Digest, Sha256};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// `.orchestry/run`: the daemon-owned private directory (socket, transfers).
pub(crate) const RUN_DIR: &str = "run";
pub(crate) const SOCKET_NAME: &str = "orch.sock";
const STATE_DIR: &str = ".orchestry";

/// The project root a daemon start serves (D4 rows 2–3): an explicit
/// `ORCH_PROJECT_ROOT` wins and never falls back to the cwd; otherwise the
/// nearest ancestor of `cwd` holding `.orchestry/`. The result is canonical.
pub fn select_project_root(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf, DaemonError> {
    if let Some(root) = explicit {
        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err(DaemonError::InvalidArguments(format!(
                "ORCH_PROJECT_ROOT must be an absolute path, got {}",
                orch_proto::path::escape_controls(&root.to_string_lossy())
            )));
        }
        let root = root
            .canonicalize()
            .map_err(|_| DaemonError::NotInitialized)?;
        return if root.join(STATE_DIR).is_dir() {
            Ok(root)
        } else {
            Err(DaemonError::NotInitialized)
        };
    }
    cwd.ancestors()
        .find(|dir| dir.join(STATE_DIR).is_dir())
        .ok_or(DaemonError::NotInitialized)?
        .canonicalize()
        .map_err(|source| io_error("resolve the project root", source))
}

/// Reject what can never be dialled: empty, relative, NUL or over the
/// 100-byte budget. Never truncates (D4).
pub(crate) fn validate_socket_path(path: &Path) -> Result<(), DaemonError> {
    let bytes = path.as_os_str().as_bytes();
    let shown = || orch_proto::path::escape_controls(&path.to_string_lossy());
    if bytes.is_empty() || !path.is_absolute() {
        return Err(DaemonError::InvalidArguments(format!(
            "socket path must be absolute, got {:?}",
            shown()
        )));
    }
    if bytes.contains(&0) {
        return Err(DaemonError::InvalidArguments(
            "socket path contains a NUL byte".into(),
        ));
    }
    if bytes.len() > MAX_SOCKET_PATH_BYTES {
        return Err(DaemonError::InvalidArguments(format!(
            "socket path {} is {} bytes, more than the {MAX_SOCKET_PATH_BYTES}-byte limit; \
             set ORCH_SOCKET to a shorter absolute path, e.g. ORCH_SOCKET=/tmp/orch-$USER/orch.sock",
            shown(),
            bytes.len()
        )));
    }
    Ok(())
}

/// The endpoint a start binds: `explicit` (`ORCH_SOCKET`) as given, else
/// `<root>/.orchestry/run/orch.sock` when that fits the budget, else
/// `<tmpdir>/orch-<sha256(root)[..16]>/orch.sock`.
pub(crate) fn endpoint(
    root: &Path,
    explicit: Option<&Path>,
    tmpdir: &Path,
) -> Result<PathBuf, DaemonError> {
    if let Some(path) = explicit {
        validate_socket_path(path)?;
        return Ok(path.to_path_buf());
    }
    let default = root.join(STATE_DIR).join(RUN_DIR).join(SOCKET_NAME);
    if default.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
        return Ok(default);
    }
    let fallback = tmpdir
        .join(format!("orch-{}", root_hash(root)))
        .join(SOCKET_NAME);
    validate_socket_path(&fallback)?;
    Ok(fallback)
}

/// First 16 hex digits of SHA-256 over the canonical root's raw bytes.
pub(crate) fn root_hash(root: &Path) -> String {
    let digest = Sha256::digest(root.as_os_str().as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Open a directory without following a final symlink. The descriptor is
/// what is checked, so a swap after the check cannot redirect it.
fn open_dir_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

/// Create a private 0700 directory, or accept an existing one only when the
/// descriptor shows the euid as owner and no group/world bits. An existing
/// directory is never chmodded or chowned (SE-4).
pub(crate) fn prepare_private_dir(dir: &Path) -> Result<(), DaemonError> {
    let shown = || dir.display().to_string();
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(io_error(&format!("create {}", shown()), source)),
    }
    let file = open_dir_nofollow(dir).map_err(|source| {
        DaemonError::InvalidArguments(format!(
            "{} is not a private directory ({}); set ORCH_SOCKET to a short path in a \
             directory you own",
            shown(),
            describe_open_error(dir, &source)
        ))
    })?;
    let meta = file
        .metadata()
        .map_err(|source| io_error(&format!("stat {}", shown()), source))?;
    let euid = nix::unistd::geteuid().as_raw();
    if meta.uid() != euid || meta.mode() & 0o077 != 0 {
        return Err(DaemonError::InvalidArguments(format!(
            "{} must be owned by uid {euid} with no group or world access (it is uid {}, \
             mode {:o}); it is not changed automatically. Remove it or set ORCH_SOCKET to a \
             short path in a directory you own",
            shown(),
            meta.uid(),
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

fn describe_open_error(path: &Path, error: &io::Error) -> &'static str {
    // O_NOFOLLOW reports a final symlink as ELOOP on Linux and as ENOTDIR
    // with O_DIRECTORY on macOS; name it the same way on both.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return "it is a symbolic link";
    }
    match error.raw_os_error() {
        Some(libc::ELOOP) => "it is a symbolic link",
        Some(libc::ENOTDIR) => "it is not a directory",
        Some(libc::ENOENT) => "it does not exist",
        Some(libc::EACCES) => "permission denied",
        _ => "it cannot be opened",
    }
}

/// `(st_dev, st_ino)` of `<root>/.orchestry`, opened `O_DIRECTORY |
/// O_NOFOLLOW` so a symlink replacing `.orchestry` itself is rejected while a
/// symlinked root alias resolves to the same identity (RT6, SE-18).
pub(crate) fn project_identity(root: &Path) -> Result<(ProjectId, std::fs::Metadata), DaemonError> {
    let dir = root.join(STATE_DIR);
    let file = open_dir_nofollow(&dir).map_err(|source| match source.raw_os_error() {
        Some(libc::ENOENT) => DaemonError::NotInitialized,
        _ => DaemonError::InvalidArguments(format!(
            "{} is not usable ({})",
            dir.display(),
            describe_open_error(&dir, &source)
        )),
    })?;
    let meta = file
        .metadata()
        .map_err(|source| io_error(&format!("stat {}", dir.display()), source))?;
    Ok((
        ProjectId {
            dev: meta.dev(),
            ino: meta.ino(),
        },
        meta,
    ))
}

/// Start refuses (exit 2) unless the euid owns `.orchestry` and it is not
/// group/world-writable. Its mode is never changed (SE-3, SE-21).
pub(crate) fn check_state_dir_owner(
    root: &Path,
    meta: &std::fs::Metadata,
) -> Result<(), DaemonError> {
    let euid = nix::unistd::geteuid().as_raw();
    if meta.uid() != euid || meta.mode() & 0o022 != 0 {
        return Err(DaemonError::InvalidArguments(state_dir_refusal(
            root,
            meta.uid(),
            euid,
            meta.mode(),
        )));
    }
    Ok(())
}

fn state_dir_refusal(root: &Path, uid: u32, euid: u32, mode: u32) -> String {
    let state = root.join(".orchestry");
    let project_display = orch_proto::path::escape_controls(&root.to_string_lossy());
    let state_display = orch_proto::path::escape_controls(&state.to_string_lossy());
    if uid != euid {
        return format!(
            "Refusing project `{project_display}`: `{state_display}` is owned by uid {uid}, \
             but this process runs as uid {euid}; run as the project owner/select your project. \
             No permissions were changed."
        );
    }
    format!(
        "Refusing project `{project_display}`: `{state_display}` is group/world-writable \
         (mode {:04o}). No permissions were changed. For an owner-only project, run \
         `chmod 700 {}` as its owner, keep its parent directories and ACLs under trusted \
         control, then start explicitly with `(cd {} && orch daemon start)`. If other \
         accounts could already write state, verify or restore it from trusted data before starting.",
        mode & 0o7777,
        shell_quote_path(&state),
        shell_quote_path(root),
    )
}

// Both callers quote directories. Keep advice printable even when the CLI
// escapes terminal controls; fixed octal operands also preserve non-UTF-8.
fn shell_quote_path(path: &Path) -> String {
    if let Some(path) = path.to_str()
        && !path.chars().any(char::is_control)
    {
        return format!("'{}'", path.replace('\'', "'\\''"));
    }
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut encoded = String::new();
    for byte in bytes {
        encoded.push_str(&format!("\\{byte:03o}"));
    }
    // Shell substitution strips trailing newlines. A directory's `/.` alias
    // preserves its identity and keeps that newline inside the quoted word.
    if bytes.ends_with(b"\n") {
        encoded.push_str("\\057\\056");
    }
    format!("\"$(printf '{encoded}')\"")
}

/// What occupies an endpoint path before bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketState {
    Absent,
    /// A same-owner socket nobody listens on: safe for the owning start to
    /// remove. Carries the inode that was probed.
    Stale {
        dev: u64,
        ino: u64,
    },
    /// Something accepts connections: never removed.
    Live,
    /// A same-owner socket the probe could not reach for a reason other than
    /// "nobody listens" (permission, resources, ...): it may still be live,
    /// so it is never removed. Carries the probe's error kind.
    Indeterminate(io::ErrorKind),
    /// A symlink, regular file, other file type or another user's socket:
    /// never removed.
    Unsafe(&'static str),
}

/// Classify `path` by `lstat` plus a connect probe (D4: only a proven stale
/// same-owner socket may be replaced). Only `ECONNREFUSED` proves that nobody
/// listens; `EACCES` on a live listener whose socket mode denies us, or a
/// full backlog, says nothing about staleness and is `Indeterminate`.
pub(crate) async fn classify_socket(path: &Path) -> Result<SocketState, DaemonError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(SocketState::Absent),
        Err(source) => return Err(io_error(&format!("stat {}", path.display()), source)),
    };
    let kind = meta.file_type();
    if kind.is_symlink() {
        return Ok(SocketState::Unsafe("a symbolic link"));
    }
    if !kind.is_socket() {
        return Ok(SocketState::Unsafe(if kind.is_file() {
            "a regular file"
        } else {
            "not a socket"
        }));
    }
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        return Ok(SocketState::Unsafe("a socket owned by another user"));
    }
    let probe = tokio::time::timeout(
        orch_proto::handshake::CONNECT_TIMEOUT,
        tokio::net::UnixStream::connect(path),
    )
    .await;
    Ok(match probe {
        // A timeout means a listener exists but is slow: treat it as live.
        Err(_) | Ok(Ok(_)) => SocketState::Live,
        Ok(Err(error)) => match error.kind() {
            io::ErrorKind::NotFound => SocketState::Absent,
            io::ErrorKind::ConnectionRefused => SocketState::Stale {
                dev: meta.dev(),
                ino: meta.ino(),
            },
            kind => SocketState::Indeterminate(kind),
        },
    })
}

/// Unlink `path` only if it is still the probed inode, so a socket that a
/// racing process bound after the probe is left alone.
pub(crate) fn remove_if_same(path: &Path, dev: u64, ino: u64) -> Result<(), DaemonError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() && meta.dev() == dev && meta.ino() == ino => {
            std::fs::remove_file(path)
                .or_else(|error| match error.kind() {
                    io::ErrorKind::NotFound => Ok(()),
                    _ => Err(error),
                })
                .map_err(|source| io_error(&format!("remove {}", path.display()), source))
        }
        _ => Ok(()),
    }
}

/// Bind the endpoint inside its private 0700 parent, then set the socket to
/// 0600 before anything is published (R2, SE-3's in-directory variant). No
/// one but the euid can traverse the parent, so the moment between `bind(2)`
/// and `chmod(2)` grants nobody access. The process umask is never changed:
/// it is process-wide, and even a short 0177 window makes directories that
/// other threads (or tests) create at that moment unusable (mode 0600).
/// Returns the listener and the socket's inode.
pub(crate) fn bind_private(
    path: &Path,
) -> Result<(std::os::unix::net::UnixListener, u64, u64), DaemonError> {
    use std::os::unix::fs::PermissionsExt;
    let listener = std::os::unix::net::UnixListener::bind(path)
        .map_err(|source| io_error(&format!("bind {}", path.display()), source))?;
    let restrict = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    let meta = restrict
        .and_then(|()| std::fs::symlink_metadata(path))
        .map_err(|source| io_error(&format!("restrict {}", path.display()), source))?;
    if !meta.file_type().is_socket() || meta.mode() & 0o777 != 0o600 {
        let _ = std::fs::remove_file(path);
        return Err(DaemonError::InvalidArguments(format!(
            "{} could not be restricted to mode 0600",
            path.display()
        )));
    }
    listener
        .set_nonblocking(true)
        .map_err(|source| io_error("configure the listener", source))?;
    Ok((listener, meta.dev(), meta.ino()))
}

pub(crate) fn discovery_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(DISCOVERY_FILE)
}

/// Publish `daemon.json` atomically: a 0600 temp file created exclusively in
/// `.orchestry/`, fsynced, then renamed over the old record.
pub(crate) fn publish(root: &Path, record: &DaemonRecord) -> Result<PathBuf, DaemonError> {
    let mut bytes = serde_json::to_vec_pretty(record)
        .map_err(|_| DaemonError::InvalidArguments("daemon.json could not be encoded".into()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_DISCOVERY_BYTES {
        return Err(DaemonError::InvalidArguments(format!(
            "daemon.json would be {} bytes, more than the {MAX_DISCOVERY_BYTES}-byte limit",
            bytes.len()
        )));
    }
    let target = discovery_path(root);
    let temp = root
        .join(STATE_DIR)
        .join(format!(".{DISCOVERY_FILE}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let write = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &target)
    };
    write().map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        io_error(&format!("publish {}", target.display()), source)
    })?;
    Ok(target)
}

/// Read `daemon.json` the way a client must (D4): no symlink, no blocking on
/// a FIFO, regular file owned by the euid with no group/world bits, bounded.
pub(crate) fn read_record(root: &Path) -> Result<Option<DaemonRecord>, DaemonError> {
    let path = discovery_path(root);
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(&format!("open {}", path.display()), source)),
    };
    let meta = file
        .metadata()
        .map_err(|source| io_error(&format!("stat {}", path.display()), source))?;
    if !meta.file_type().is_file()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.len() > MAX_DISCOVERY_BYTES as u64
    {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&mut file)
        .take(MAX_DISCOVERY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(&format!("read {}", path.display()), source))?;
    Ok(DaemonRecord::decode(&bytes).ok())
}

/// Remove `daemon.json` only when it is this daemon's generation, so a stop
/// never deletes a successor's record.
pub(crate) fn remove_own_record(root: &Path, instance: &str) {
    if let Ok(Some(record)) = read_record(root)
        && record.instance.as_str() == instance
    {
        let _ = std::fs::remove_file(discovery_path(root));
    }
}

pub(crate) fn io_error(context: &str, source: io::Error) -> DaemonError {
    DaemonError::Io {
        context: context.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn default_endpoint_boundary_is_100_bytes_inclusive() {
        let tmp = Path::new("/t");
        // "/.orchestry/run/orch.sock" is 25 bytes.
        let suffix = "/.orchestry/run/orch.sock".len();
        let root_at = |len: usize| PathBuf::from(format!("/{}", "a".repeat(len - 1)));

        let fits = root_at(100 - suffix);
        let chosen = endpoint(&fits, None, tmp).unwrap();
        assert_eq!(chosen.as_os_str().len(), 100);
        assert_eq!(chosen, fits.join(".orchestry/run/orch.sock"));

        let over = root_at(101 - suffix);
        let chosen = endpoint(&over, None, tmp).unwrap();
        assert_eq!(
            chosen,
            PathBuf::from(format!("/t/orch-{}/orch.sock", root_hash(&over)))
        );
    }

    #[test]
    fn fallback_hash_is_sha256_prefix_of_the_raw_root_bytes() {
        let root = Path::new("/some/project");
        let full: String = Sha256::digest(b"/some/project")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(root_hash(root), full[..16]);
        assert_ne!(root_hash(root), root_hash(Path::new("/some/project2")));
    }

    #[test]
    fn too_long_fallback_and_override_are_rejected_not_truncated() {
        let root = PathBuf::from(format!("/{}", "r".repeat(120)));
        let tmp = PathBuf::from(format!("/{}", "t".repeat(90)));
        let error = endpoint(&root, None, &tmp).unwrap_err().to_string();
        assert!(error.contains("ORCH_SOCKET"), "{error}");

        let long = PathBuf::from(format!("/{}", "s".repeat(100)));
        assert!(endpoint(&root, Some(&long), &tmp).is_err());
        assert!(endpoint(&root, Some(Path::new("rel/orch.sock")), &tmp).is_err());
        assert!(endpoint(&root, Some(Path::new("")), &tmp).is_err());
        let exact = PathBuf::from(format!("/{}", "s".repeat(99)));
        assert_eq!(endpoint(&root, Some(&exact), &tmp).unwrap(), exact);
    }

    #[test]
    fn private_dir_is_created_0700_and_foreign_modes_are_refused_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("run");
        prepare_private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        // Idempotent on the private directory it created.
        prepare_private_dir(&dir).unwrap();

        let open = temp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_private_dir(&open).is_err());
        assert_eq!(std::fs::metadata(&open).unwrap().mode() & 0o777, 0o755);

        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let error = prepare_private_dir(&link).unwrap_err().to_string();
        assert!(error.contains("symbolic link"), "{error}");
    }

    #[test]
    fn identity_accepts_root_aliases_and_rejects_a_symlinked_state_dir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("p");
        std::fs::create_dir_all(root.join(".orchestry")).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let (a, _) = project_identity(&root).unwrap();
        let (b, _) = project_identity(&alias).unwrap();
        assert_eq!(a, b);

        let other = temp.path().join("q");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(root.join(".orchestry"), other.join(".orchestry")).unwrap();
        assert!(matches!(
            project_identity(&other),
            Err(DaemonError::InvalidArguments(_))
        ));
        assert!(matches!(
            project_identity(temp.path()),
            Err(DaemonError::NotInitialized)
        ));
    }

    #[test]
    fn permission_advice_quotes_path_bytes_and_escapes_prose_controls() {
        for (path, expected) in [
            ("/tmp/owner's project", "'/tmp/owner'\\''s project'"),
            (
                "/tmp/line\nnext",
                r#""$(printf '\057\164\155\160\057\154\151\156\145\012\156\145\170\164')""#,
            ),
            ("-leading-dash", "'-leading-dash'"),
            ("/tmp/$(echo bad);`id`", "'/tmp/$(echo bad);`id`'"),
        ] {
            assert_eq!(shell_quote_path(Path::new(path)), expected);
        }
        use std::os::unix::ffi::OsStrExt;
        let raw = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff'\n"));
        assert_eq!(
            shell_quote_path(raw),
            r#""$(printf '\057\164\155\160\057\377\047\012\057\056')""#
        );
        let message = state_dir_refusal(Path::new("/tmp/owner's\nproject"), 42, 42, 0o775);
        for part in [
            "Refusing project `/tmp/owner's\\u{a}project`",
            "`/tmp/owner's\\u{a}project/.orchestry`",
            "is group/world-writable (mode 0775)",
            "No permissions were changed.",
            "`chmod 700 ",
            " && orch daemon start)`",
            "as its owner",
            "keep its parent directories and ACLs under trusted control",
            "verify or restore it from trusted data before starting",
        ] {
            assert!(message.contains(part), "missing {part:?}: {message}");
        }
        assert!(!message.contains('\n'), "{message}");
        let owner = state_dir_refusal(Path::new("/tmp/project"), 41, 42, 0o775);
        for part in [
            "/tmp/project/.orchestry",
            "owned by uid 41",
            "runs as uid 42",
            "run as the project owner/select your project",
            "No permissions were changed.",
        ] {
            assert!(owner.contains(part), "{owner}");
        }
        assert!(!owner.contains("chmod"));
        assert!(!owner.contains("sudo"));
        assert!(!owner.contains("chown"));
    }

    #[test]
    fn writable_state_dir_is_refused_and_not_chmodded() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join(".orchestry");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o775)).unwrap();
        let (_, meta) = project_identity(temp.path()).unwrap();
        assert!(check_state_dir_owner(temp.path(), &meta).is_err());
        assert_eq!(std::fs::metadata(&state).unwrap().mode() & 0o777, 0o775);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (_, meta) = project_identity(temp.path()).unwrap();
        check_state_dir_owner(temp.path(), &meta).unwrap();
    }

    fn short_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("od-")
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn stale_socket_helper_binds_without_descendants() {
        let Some(path) = std::env::var_os("ORCH_TEST_DISCOVERY_STALE_SOCKET") else {
            return;
        };
        let (_listener, _, _) = bind_private(Path::new(&path)).unwrap();
    }

    #[tokio::test]
    async fn stale_socket_classification_never_targets_files_links_or_live_listeners() {
        let dir = short_dir();
        let path = dir.path().join("s");
        assert_eq!(classify_socket(&path).await.unwrap(), SocketState::Absent);

        // Live: a bound, listening socket.
        let live_path = dir.path().join("live");
        let (listener, _, _) = bind_private(&live_path).unwrap();
        let _listener = tokio::net::UnixListener::from_std(listener).unwrap();
        assert_eq!(
            classify_socket(&live_path).await.unwrap(),
            SocketState::Live
        );
        // The stale descriptor never enters this runner, where a sibling's
        // fork could retain it. The child binds without spawning descendants;
        // waiting for its exit proves the socket has no remaining listener.
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery::tests::stale_socket_helper_binds_without_descendants",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", dir.path())
            .env("TMPDIR", dir.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                dir.path().join("global.yml"),
            )
            .env("ORCH_TEST_DISCOVERY_STALE_SOCKET", &path)
            .current_dir(dir.path())
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "stale socket helper: {output:?}");
        let state = classify_socket(&path).await.unwrap();
        let SocketState::Stale { dev, ino } = state else {
            panic!("expected stale, got {state:?}");
        };
        remove_if_same(&path, dev, ino).unwrap();
        assert!(!path.exists());

        std::fs::write(&path, b"x").unwrap();
        assert_eq!(
            classify_socket(&path).await.unwrap(),
            SocketState::Unsafe("a regular file")
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/tmp", &path).unwrap();
        assert_eq!(
            classify_socket(&path).await.unwrap(),
            SocketState::Unsafe("a symbolic link")
        );
    }

    /// `EACCES` from a live listener whose socket mode denies the probe is
    /// not proof that nobody listens (board 1 C2 / security F4).
    #[tokio::test]
    async fn an_unreachable_live_listener_is_indeterminate_not_stale() {
        if nix::unistd::geteuid().is_root() {
            eprintln!("skipped: euid 0 ignores socket permissions (EACCES is unreachable)");
            return;
        }
        let dir = short_dir();
        let path = dir.path().join("s");
        let (_listener, _, _) = bind_private(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(
            classify_socket(&path).await.unwrap(),
            SocketState::Indeterminate(io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn remove_if_same_leaves_a_replaced_inode_alone() {
        let dir = short_dir();
        let path = dir.path().join("s");
        let (first, dev, ino) = bind_private(&path).unwrap();
        drop(first);
        // A second name keeps the first inode allocated while the successor
        // binds; Linux ext4 would otherwise hand the freed number straight
        // back and the successor would look like the probed socket.
        std::fs::hard_link(&path, dir.path().join("keep")).unwrap();
        std::fs::remove_file(&path).unwrap();
        let (_second, _, ino2) = bind_private(&path).unwrap();
        assert_ne!(ino, ino2, "precondition: the successor is a new inode");
        remove_if_same(&path, dev, ino).unwrap();
        assert!(path.exists(), "a successor's socket must survive");
    }

    #[test]
    fn socket_is_bound_0600_in_its_private_parent() {
        let dir = short_dir();
        let run = dir.path().join("run");
        prepare_private_dir(&run).unwrap();
        let path = run.join("s");
        let (_listener, _, _) = bind_private(&path).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::metadata(&run).unwrap().mode() & 0o777, 0o700);
    }

    fn record(root: &Path, instance: &str) -> DaemonRecord {
        use orch_proto::path::WirePath;
        DaemonRecord {
            schema: orch_proto::discovery::DISCOVERY_SCHEMA,
            project_root: WirePath::new(root).unwrap(),
            project_id: ProjectId { dev: 1, ino: 2 },
            socket: WirePath::new("/tmp/x.sock").unwrap(),
            pid: 42,
            binary: orch_proto::bounded::BoundedString::new("0.13.0").unwrap(),
            protocol: 1,
            executable: WirePath::new("/bin/orch").unwrap(),
            started_at: orch_proto::bounded::BoundedString::new("now").unwrap(),
            instance: orch_proto::bounded::BoundedString::new(instance).unwrap(),
        }
    }

    #[test]
    fn discovery_record_is_private_atomic_and_removed_only_by_its_generation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let path = publish(temp.path(), &record(temp.path(), "i1")).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            read_record(temp.path()).unwrap().unwrap().instance.as_str(),
            "i1"
        );
        remove_own_record(temp.path(), "other");
        assert!(path.exists());
        remove_own_record(temp.path(), "i1");
        assert!(!path.exists());
    }

    #[test]
    fn discovery_read_ignores_fifos_links_and_public_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let path = discovery_path(temp.path());
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        assert!(
            read_record(temp.path()).unwrap().is_none(),
            "a FIFO must not hang"
        );
        std::fs::remove_file(&path).unwrap();

        publish(temp.path(), &record(temp.path(), "i1")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_record(temp.path()).unwrap().is_none());
    }
}
