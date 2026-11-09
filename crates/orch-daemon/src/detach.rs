//! `orch daemon start --detach` (design D3/D13, spec R5/R20/R35).
//!
//! The parent re-executes the same binary as a foreground start with
//! `--detach` removed. The child calls `setsid` itself, has stdin
//! `/dev/null`, writes stderr to the private `.orchestry/daemon.log`, and
//! reports readiness on its stdout pipe; after READY it points stdout at the
//! log too. The parent succeeds only after the child's `listening <socket>`
//! line; a child that exits or misses the deadline is the parent's failure.
//!
//! The child's stderr (and stdout after READY) is a pipe into one writer
//! thread, [`DetachedLog`], which rotates before a write would take the log
//! past [`LOG_CAP`]: a hard bound on everything written to those descriptors
//! (`tracing`, panics, children that inherit stderr), not a periodic check.
//!
//! Re-exec rather than an in-process double fork: `fork` without `exec` in a
//! process that already runs a multi-threaded runtime may only call
//! async-signal-safe functions, and the workspace forbids `unsafe`, so
//! `pre_exec(setsid)` is unavailable. The child is not a process-group leader,
//! so its own `setsid` succeeds.

use crate::DaemonError;
use crate::discovery::io_error;
use crate::lifecycle::{DaemonExit, DispatchMode, StartOptions};
use nix::libc;
use std::fs::{File, OpenOptions};
use std::io::{PipeReader, Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Set (to `1`) only in the environment of the re-executed child. Removed
/// from the agent environment snapshot.
pub(crate) const DETACHED_CHILD_ENV: &str = "ORCH_DAEMON_DETACHED_CHILD";
pub(crate) const LOG_FILE: &str = "daemon.log";
pub(crate) const LOG_BACKUP: &str = "daemon.log.1";
/// Neither the log nor its one backup grows past this (20 MiB total).
pub(crate) const LOG_CAP: u64 = 10 * 1024 * 1024;
/// Ordinary output rotates this far below [`LOG_CAP`], leaving room for the
/// fixed failure notice and a failed start's last error line.
const LOG_RESERVE: u64 = 4 * 1024;
/// How long the exiting child waits for the writer to drain the pipe.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Bounded wait for the child's READY: recovery and index rebuild included.
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const FAILURE_TAIL: u64 = 4 * 1024;

pub(crate) fn log_path(root: &Path) -> PathBuf {
    root.join(".orchestry").join(LOG_FILE)
}

/// Open the log for appending: never through a symlink, only a regular
/// 0600 file owned by the euid, rotated once when over the cap. A backup over
/// the cap (from a build without the write-time bound) is cut to it.
pub(crate) fn open_log(root: &Path) -> Result<File, DaemonError> {
    let path = log_path(root);
    let mut file = open_log_file(&path)?;
    if file
        .metadata()
        .map_err(|source| io_error(&format!("stat {}", path.display()), source))?
        .len()
        > LOG_CAP
    {
        rotate(root)?;
        file = open_log_file(&path)?;
    }
    cap_backup(root)?;
    Ok(file)
}

/// Truncate an oversized regular backup to [`LOG_CAP`]; anything else at that
/// path is left for the next rotation to replace or report.
fn cap_backup(root: &Path) -> Result<(), DaemonError> {
    let path = root.join(".orchestry").join(LOG_BACKUP);
    let oversized = std::fs::symlink_metadata(&path)
        .is_ok_and(|meta| meta.file_type().is_file() && meta.len() > LOG_CAP);
    if !oversized {
        return Ok(());
    }
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|source| io_error(&format!("open {}", path.display()), source))?;
    check_log(&path, &file)?;
    file.set_len(LOG_CAP)
        .map_err(|source| io_error(&format!("truncate {}", path.display()), source))
}

fn open_log_file(path: &Path) -> Result<File, DaemonError> {
    // Readable too: a failed start shows the child's last lines.
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error(&format!("open {}", path.display()), source))?;
    check_log(path, &file)?;
    Ok(file)
}

fn check_log(path: &Path, file: &File) -> Result<(), DaemonError> {
    let meta = file
        .metadata()
        .map_err(|source| io_error(&format!("stat {}", path.display()), source))?;
    if !meta.file_type().is_file()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(DaemonError::InvalidArguments(format!(
            "{} must be a regular file owned by you with mode 0600; remove it and start again",
            path.display()
        )));
    }
    Ok(())
}

fn rotate(root: &Path) -> Result<(), DaemonError> {
    let from = log_path(root);
    let to = root.join(".orchestry").join(LOG_BACKUP);
    std::fs::rename(&from, &to)
        .map_err(|source| io_error(&format!("rotate {}", from.display()), source))
}

/// Re-execute as a detached foreground start and wait for its readiness.
pub(crate) async fn spawn_detached(
    options: &StartOptions,
    root: &Path,
) -> Result<DaemonExit, DaemonError> {
    let mut log = open_log(root)?;
    let offset = log.seek(SeekFrom::End(0)).unwrap_or(0);
    let stderr = log
        .try_clone()
        .map_err(|source| io_error("duplicate the daemon log", source))?;
    let exe = std::env::current_exe()
        .map_err(|source| io_error("resolve the orch executable", source))?;
    let mut command = tokio::process::Command::new(exe);
    command.args(child_args(options));
    command
        .env(DETACHED_CHILD_ENV, "1")
        .env(orch_proto::discovery::ENV_PROJECT_ROOT, root)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(false);
    match &options.socket {
        Some(socket) => command.env(orch_proto::discovery::ENV_SOCKET, socket),
        None => command.env_remove(orch_proto::discovery::ENV_SOCKET),
    };
    let mut child = command
        .spawn()
        .map_err(|source| io_error("start the detached daemon", source))?;
    let Some(stdout) = child.stdout.take() else {
        return Err(DaemonError::InvalidArguments(
            "the detached daemon has no readiness pipe".into(),
        ));
    };
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(READY_TIMEOUT, async {
        let mut seen = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            let done = line.starts_with("dispatch ");
            seen.push(line);
            if done {
                return Some(seen);
            }
        }
        None
    })
    .await;
    let mut out = std::io::stdout().lock();
    match ready {
        Ok(Some(seen)) if seen.iter().any(|line| line.starts_with("listening ")) => {
            for line in seen {
                let _ = writeln!(out, "{line}");
            }
            let _ = writeln!(out, "log {}", log_path(root).display());
            let _ = out.flush();
            Ok(DaemonExit::CLEAN)
        }
        outcome => {
            if outcome.is_err() {
                // No success before READY: our own child, started a moment
                // ago, must not become ready after we reported failure.
                let _ = child.start_kill();
            }
            let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            let code = match status {
                Ok(Ok(status)) => status.code().unwrap_or(orch_proto::error::exit::GENERAL),
                _ => orch_proto::error::exit::GENERAL,
            };
            let code = if code == 0 {
                orch_proto::error::exit::GENERAL
            } else {
                code
            };
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&log_tail(&mut log, offset));
            let _ = writeln!(
                err,
                "orch daemon did not become ready (exit {code}); see {}",
                log_path(root).display()
            );
            Ok(DaemonExit { code, clean: false })
        }
    }
}

/// The foreground start the child runs. The flags are the spec's `orch daemon
/// start` syntax (R5); project and socket travel in the environment.
fn child_args(options: &StartOptions) -> Vec<String> {
    let mut args = vec![
        "daemon".to_owned(),
        "start".to_owned(),
        "--dispatch".to_owned(),
        match options.dispatch {
            DispatchMode::On => "on",
            DispatchMode::Off => "off",
        }
        .to_owned(),
    ];
    if options.from_agent {
        args.push("--from-agent".to_owned());
    }
    if let Some(listen) = &options.listen {
        args.push("--listen".to_owned());
        args.push(listen.clone());
    }
    if let Some(name) = &options.peer_name {
        // `=`: a name starting with `-` must not parse as a flag.
        args.push(format!("--peer-name={name}"));
    }
    args
}

fn log_tail(log: &mut File, offset: u64) -> Vec<u8> {
    let end = log.seek(SeekFrom::End(0)).unwrap_or(offset);
    let start = end.saturating_sub(FAILURE_TAIL).max(offset);
    let mut tail = Vec::new();
    if log.seek(SeekFrom::Start(start)).is_ok() {
        let _ = log.take(FAILURE_TAIL).read_to_end(&mut tail);
    }
    tail
}

/// In the detached child, before anything else: leave the starter's session.
pub(crate) fn enter_own_session() {
    if let Err(errno) = nix::unistd::setsid() {
        tracing::warn!(%errno, "setsid failed; the daemon stays in the starter's session");
    }
}

/// The detached child's capped log (R20/R35). Created before the banner; its
/// drop drains the pipe and hands the descriptors back to the log file, so a
/// failed start's error line still reaches the parent's tail.
#[derive(Debug)]
pub(crate) struct DetachedLog {
    drained: mpsc::Receiver<Option<File>>,
    stdout_captured: bool,
}

impl DetachedLog {
    /// Route stderr through the capped writer. The log is the parent's,
    /// inherited as stderr; any failure here fails the start rather than
    /// leave the log unbounded.
    pub(crate) fn capture(root: &Path) -> Result<DetachedLog, DaemonError> {
        let path = log_path(root);
        let file = std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map(File::from)
            .map_err(|source| io_error("duplicate the daemon log", source))?;
        check_log(&path, &file)?;
        let (reader, writer) =
            std::io::pipe().map_err(|source| io_error("create the daemon log pipe", source))?;
        let (done, drained) = mpsc::sync_channel(1);
        let log = CappedLog::new(root, file);
        std::thread::Builder::new()
            .name("orch-daemon-log".into())
            .spawn(move || {
                let _ = done.send(log.drain(reader));
            })
            .map_err(|source| io_error("start the daemon log writer", source))?;
        nix::unistd::dup2_stderr(&writer)
            .map_err(|errno| io_error("redirect stderr to the daemon log", errno.into()))?;
        Ok(DetachedLog {
            drained,
            stdout_captured: false,
        })
    }

    /// After READY: stdout leaves the readiness pipe for the log (stderr).
    pub(crate) fn release_readiness_pipe(&mut self) {
        match nix::unistd::dup2_stdout(std::io::stderr()) {
            Ok(()) => self.stdout_captured = true,
            Err(errno) => {
                tracing::warn!(%errno, "could not redirect stdout to the daemon log");
            }
        }
    }

    /// Point the captured descriptors at `to`, closing the pipe's write ends.
    fn redirect(&self, to: impl AsFd) {
        let _ = nix::unistd::dup2_stderr(&to);
        if self.stdout_captured {
            let _ = nix::unistd::dup2_stdout(&to);
        }
    }
}

impl Drop for DetachedLog {
    fn drop(&mut self) {
        // Until the writer reaches EOF, late output goes nowhere rather than
        // past the cap; afterwards it lands in the reserve.
        let Ok(null) = OpenOptions::new().write(true).open("/dev/null") else {
            return;
        };
        self.redirect(&null);
        if let Ok(Some(file)) = self.drained.recv_timeout(DRAIN_TIMEOUT) {
            self.redirect(&file);
        }
    }
}

/// `daemon.log` written through one handle: rotate before a write would take
/// it past `cap - LOG_RESERVE`, keep one backup, never write past `cap`. A
/// failed rotation, open or write drops output and says so with a fixed
/// notice (an error kind, never a path or payload).
struct CappedLog {
    path: PathBuf,
    backup: PathBuf,
    /// The log; while `reopen`, the file it was (no longer at `path`).
    file: File,
    /// A rotation moved or lost the log but could not open a new one.
    reopen: bool,
    cap: u64,
    dropped: u64,
    failed: bool,
}

impl CappedLog {
    fn new(root: &Path, file: File) -> CappedLog {
        CappedLog {
            path: log_path(root),
            backup: root.join(".orchestry").join(LOG_BACKUP),
            file,
            reopen: false,
            cap: LOG_CAP,
            dropped: 0,
            failed: false,
        }
    }

    /// Copy the pipe to the log until every write end is closed; return the
    /// log if it still has its reserve.
    fn drain(mut self, mut reader: PipeReader) -> Option<File> {
        let mut buf = vec![0; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.write(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if self.dropped > 0 {
            self.write(b"");
        }
        fits(&self.file, 0, self.cap - LOG_RESERVE).then_some(self.file)
    }

    fn write(&mut self, chunk: &[u8]) {
        let limit = self.cap - LOG_RESERVE;
        let mut bytes = Vec::new();
        if self.dropped > 0 {
            bytes.extend(
                format!(
                    "orch daemon: {} bytes of output were dropped while {LOG_FILE} could not be written\n",
                    self.dropped
                )
                .as_bytes(),
            );
        }
        bytes.extend_from_slice(chunk);
        let need = bytes.len() as u64;
        // Twice at most: a file another writer rotated in is renamed too.
        for _ in 0..2 {
            if !self.reopen && fits(&self.file, need, limit) {
                break;
            }
            if let Err((action, error)) = self.rotate() {
                return self.fail(action, &error, chunk.len());
            }
        }
        if self.reopen || !fits(&self.file, need, limit) {
            let full = std::io::ErrorKind::StorageFull.into();
            return self.fail("rotate", &full, chunk.len());
        }
        match self.file.write_all(&bytes) {
            Ok(()) => {
                self.dropped = 0;
                self.failed = false;
            }
            Err(error) => self.fail("write", &error, chunk.len()),
        }
    }

    /// Move our file to the backup (unless another writer already moved it)
    /// and open a fresh one. On failure the old handle stays, for the notice.
    fn rotate(&mut self) -> Result<(), (&'static str, std::io::Error)> {
        let ours = match (self.file.metadata(), std::fs::symlink_metadata(&self.path)) {
            (Ok(meta), Ok(at_path)) => meta.dev() == at_path.dev() && meta.ino() == at_path.ino(),
            _ => false,
        };
        if ours {
            std::fs::rename(&self.path, &self.backup).map_err(|error| ("rotate", error))?;
        }
        self.reopen = true;
        self.file = open_log_file(&self.path).map_err(|error| {
            let error = match error {
                DaemonError::Io { source, .. } => source,
                // Not a private regular file of ours.
                _ => std::io::ErrorKind::InvalidData.into(),
            };
            ("open", error)
        })?;
        self.reopen = false;
        Ok(())
    }

    /// Drop `lost` bytes; the first failure of a run leaves one notice in the
    /// reserve of the file still held.
    fn fail(&mut self, action: &str, error: &std::io::Error, lost: usize) {
        self.dropped += lost as u64;
        if std::mem::replace(&mut self.failed, true) {
            return;
        }
        let notice = format!(
            "orch daemon: cannot {action} {LOG_FILE} ({}); output is dropped to keep it under {} MiB\n",
            error.kind(),
            LOG_CAP / (1024 * 1024)
        );
        if fits(&self.file, notice.len() as u64, self.cap) {
            let _ = self.file.write_all(notice.as_bytes());
        }
    }
}

fn fits(file: &File, extra: u64, limit: u64) -> bool {
    file.metadata()
        .is_ok_and(|meta| meta.len().saturating_add(extra) <= limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn log_is_private_and_rotates_once_past_the_cap() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let file = open_log(temp.path()).unwrap();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o600);
        file.set_len(LOG_CAP + 1).unwrap();
        drop(file);
        let file = open_log(temp.path()).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        // An oversized log from a build without the write-time bound is cut
        // to the cap as it becomes the backup.
        let backup = temp.path().join(".orchestry").join(LOG_BACKUP);
        assert_eq!(std::fs::metadata(&backup).unwrap().len(), LOG_CAP);
        assert_eq!(std::fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn an_oversized_backup_left_behind_is_cut_to_the_cap() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let backup = temp.path().join(".orchestry").join(LOG_BACKUP);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&backup)
            .unwrap();
        file.set_len(2 * LOG_CAP).unwrap();
        drop(open_log(temp.path()).unwrap());
        assert_eq!(std::fs::metadata(&backup).unwrap().len(), LOG_CAP);
    }

    #[test]
    fn failure_tail_shows_only_what_the_child_wrote() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let mut log = open_log(temp.path()).unwrap();
        log.write_all(b"earlier run\n").unwrap();
        let offset = log.seek(SeekFrom::End(0)).unwrap();
        let mut child = log.try_clone().unwrap();
        child
            .write_all(b"error: orch daemon already running (PID 7)\n")
            .unwrap();
        assert_eq!(
            log_tail(&mut log, offset),
            b"error: orch daemon already running (PID 7)\n"
        );
    }

    #[test]
    fn log_symlink_and_public_log_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let path = log_path(temp.path());
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();
        assert!(open_log(temp.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_log(temp.path()).is_err());
    }

    const SMALL: u64 = 16 * 1024;

    fn small_log(root: &Path) -> CappedLog {
        std::fs::create_dir(root.join(".orchestry")).unwrap();
        let mut log = CappedLog::new(root, open_log_file(&log_path(root)).unwrap());
        log.cap = SMALL;
        log
    }

    fn size(path: &Path) -> u64 {
        std::fs::metadata(path).map_or(0, |meta| meta.len())
    }

    #[test]
    fn live_writes_rotate_before_the_cap_and_keep_one_backup() {
        let temp = tempfile::tempdir().unwrap();
        let mut log = small_log(temp.path());
        let backup = temp.path().join(".orchestry").join(LOG_BACKUP);
        for i in 0..2000 {
            log.write(format!("line {i:05} {}\n", "x".repeat(90)).as_bytes());
            assert!(size(&log_path(temp.path())) <= SMALL, "line {i}");
            assert!(size(&backup) <= SMALL, "line {i}");
        }
        let current = std::fs::read_to_string(log_path(temp.path())).unwrap();
        assert!(current.ends_with(&format!("line 01999 {}\n", "x".repeat(90))));
        assert!(size(&backup) > SMALL / 2, "the backup is the previous log");
        for path in [log_path(temp.path()), backup] {
            let mode = std::fs::metadata(path).unwrap().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn failed_rotation_drops_output_says_so_once_and_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let mut log = small_log(temp.path());
        let path = log_path(temp.path());
        // rename(2) cannot replace a non-empty directory.
        let backup = temp.path().join(".orchestry").join(LOG_BACKUP);
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(backup.join("keep"), b"").unwrap();
        for _ in 0..1000 {
            log.write(&[b'p'; 100]);
            assert!(size(&path) <= SMALL);
        }
        let held = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            held.matches("orch daemon: cannot rotate daemon.log")
                .count(),
            1,
            "{}",
            &held[held.len() - 200..]
        );
        assert!(held.ends_with("output is dropped to keep it under 10 MiB\n"));

        std::fs::remove_dir_all(&backup).unwrap();
        log.write(b"after\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), held);
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.starts_with("orch daemon: "), "{fresh}");
        assert!(fresh.contains("bytes of output were dropped"), "{fresh}");
        assert!(fresh.ends_with("after\n"));
    }

    #[test]
    fn failed_reopen_never_follows_a_link_and_is_retried() {
        let temp = tempfile::tempdir().unwrap();
        let mut log = small_log(temp.path());
        let path = log_path(temp.path());
        let held = temp.path().join("held");
        let victim = temp.path().join("victim");
        std::fs::write(&victim, b"").unwrap();
        log.write(&[b'a'; 1000]);
        // Something else moved our log away and left a link in its place.
        std::fs::rename(&path, &held).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        for _ in 0..100 {
            log.write(&[b'b'; 1000]);
            assert!(size(&held) <= SMALL);
        }
        assert_eq!(size(&victim), 0, "nothing is written through the link");
        let notice = std::fs::read_to_string(&held).unwrap();
        assert_eq!(
            notice
                .matches("orch daemon: cannot open daemon.log")
                .count(),
            1
        );

        std::fs::remove_file(&path).unwrap();
        log.write(b"back\n");
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.contains("bytes of output were dropped"), "{fresh}");
        assert!(fresh.ends_with("back\n"));
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn drain_copies_the_pipe_until_eof_and_returns_the_log() {
        let temp = tempfile::tempdir().unwrap();
        let log = small_log(temp.path());
        let (reader, mut writer) = std::io::pipe().unwrap();
        let writer_thread = std::thread::spawn(move || log.drain(reader));
        writer.write_all(b"one\ntwo\n").unwrap();
        drop(writer);
        let mut file = writer_thread.join().unwrap().expect("the log has room");
        file.write_all(b"three\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(log_path(temp.path())).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn child_args_are_the_foreground_start_without_detach() {
        let options = StartOptions {
            dispatch: DispatchMode::On,
            from_agent: true,
            listen: Some("127.0.0.1:7000".into()),
            peer_name: Some("-worker one".into()),
            detach: true,
            ..Default::default()
        };
        assert_eq!(
            child_args(&options),
            [
                "daemon",
                "start",
                "--dispatch",
                "on",
                "--from-agent",
                "--listen",
                "127.0.0.1:7000",
                "--peer-name=-worker one"
            ]
        );
    }
}
