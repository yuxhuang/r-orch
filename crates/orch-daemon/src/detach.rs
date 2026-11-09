//! `orch daemon start --detach` (design D3/D13, spec R5/R20/R35).
//!
//! The parent re-executes the same binary as a foreground start with
//! `--detach` removed. The child calls `setsid` itself, has stdin
//! `/dev/null`, writes stderr to the private `.orchestry/daemon.log`, and
//! reports readiness on its stdout pipe; after READY it points stdout at the
//! log too. The parent succeeds only after the child's `listening <socket>`
//! line; a child that exits or misses the deadline is the parent's failure.
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
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Set (to `1`) only in the environment of the re-executed child. Removed
/// from the agent environment snapshot.
pub(crate) const DETACHED_CHILD_ENV: &str = "ORCH_DAEMON_DETACHED_CHILD";
pub(crate) const LOG_FILE: &str = "daemon.log";
pub(crate) const LOG_BACKUP: &str = "daemon.log.1";
/// The log rotates past this size, keeping one backup (20 MiB total).
pub(crate) const LOG_CAP: u64 = 10 * 1024 * 1024;
const LOG_CHECK: Duration = Duration::from_secs(30);
/// Bounded wait for the child's READY: recovery and index rebuild included.
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const FAILURE_TAIL: u64 = 4 * 1024;

pub(crate) fn log_path(root: &Path) -> PathBuf {
    root.join(".orchestry").join(LOG_FILE)
}

/// Open the log for appending: never through a symlink, only a regular
/// 0600 file owned by the euid, rotated once when over the cap.
pub(crate) fn open_log(root: &Path) -> Result<File, DaemonError> {
    let path = log_path(root);
    let open = || {
        // Readable too: a failed start shows the child's last lines.
        OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|source| io_error(&format!("open {}", path.display()), source))
    };
    let mut file = open()?;
    check_log(&path, &file)?;
    if file
        .metadata()
        .map_err(|source| io_error(&format!("stat {}", path.display()), source))?
        .len()
        > LOG_CAP
    {
        rotate(root)?;
        file = open()?;
        check_log(&path, &file)?;
    }
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

/// After READY: stdout leaves the readiness pipe for the log (stderr).
pub(crate) fn release_readiness_pipe() {
    if let Err(errno) = nix::unistd::dup2_stdout(std::io::stderr()) {
        tracing::warn!(%errno, "could not redirect stdout to the daemon log");
    }
}

/// Keep `daemon.log` within its cap while the detached child runs.
pub(crate) async fn rotate_log_while_running(
    root: PathBuf,
    mut quit: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            () = crate::session::until_set(&mut quit) => return,
            () = tokio::time::sleep(LOG_CHECK) => {}
        }
        let over = std::fs::symlink_metadata(log_path(&root)).is_ok_and(|m| m.len() > LOG_CAP);
        if !over {
            continue;
        }
        if rotate(&root).is_err() {
            continue;
        }
        if let Ok(file) = open_log(&root) {
            let _ = nix::unistd::dup2_stderr(&file);
            let _ = nix::unistd::dup2_stdout(&file);
        }
    }
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
        let backup = temp.path().join(".orchestry").join(LOG_BACKUP);
        assert_eq!(std::fs::metadata(&backup).unwrap().len(), LOG_CAP + 1);
        assert_eq!(std::fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
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

    #[test]
    fn child_args_are_the_foreground_start_without_detach() {
        let options = StartOptions {
            dispatch: DispatchMode::On,
            from_agent: true,
            listen: Some("127.0.0.1:7000".into()),
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
                "127.0.0.1:7000"
            ]
        );
    }
}
