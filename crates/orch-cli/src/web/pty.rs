//! One pseudo-terminal running one child, with its blocking I/O moved onto
//! plain threads and handed to the async side as channels.
//!
//! `portable-pty`'s reader and writer are blocking, so neither may run on the
//! tokio runtime. The reader thread ends when the child (and anything else
//! holding the slave side) closes the pty, and keeps draining it even after
//! the session is dropped; the writer thread ends when
//! [`PtySession::input`] is dropped or a write fails.
//!
//! Dropping a [`PtySession`] never blocks: a reaper thread sends SIGHUP, waits
//! up to [`KILL_GRACE`] for the child to exit on its own, then SIGKILLs it.
//! The grace matters because `orch tui` turns SIGHUP into its normal shutdown,
//! which stops the agents it started; a SIGKILL would orphan them.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

const READ_CHUNK: usize = 8 * 1024;
const OUTPUT_DEPTH: usize = 64;
/// Messages queued toward the child before the session is closed as stuck.
/// At the page's 16 KiB chunks this is 2 MiB of paste backlog.
pub const INPUT_DEPTH: usize = 128;

/// How long a child gets between SIGHUP and SIGKILL. `orch tui` answers SIGHUP
/// with `orchestrator.stop()`, which gives each agent 10 s of its own grace and
/// then saves state and releases the lock, so this has to be longer.
pub const KILL_GRACE: Duration = Duration::from_secs(15);

/// The environment variable `orch web` reads the password from; the child
/// must not inherit it.
const SCRUBBED_ENV: &str = super::PASSWORD_ENV;

/// What to run in the pty. `orch web` runs `orch tui`; tests run `sh -c ...`.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
}

/// One unit of the server's session cap. Held until the child is actually
/// gone, so a session that is still shutting down keeps counting.
#[derive(Debug)]
pub struct SessionSlot(Arc<AtomicUsize>);

impl SessionSlot {
    pub fn acquire(live: &Arc<AtomicUsize>, max: usize) -> Option<SessionSlot> {
        let slot = SessionSlot(Arc::clone(live));
        // Constructed first so that going over the cap releases it again.
        (live.fetch_add(1, Ordering::SeqCst) < max).then_some(slot)
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    slot: Option<SessionSlot>,
    /// Raw bytes to type into the terminal.
    pub input: mpsc::Sender<Vec<u8>>,
    /// Raw terminal output. Yields `None` once the child has closed the pty.
    pub output: mpsc::Receiver<Vec<u8>>,
}

impl PtySession {
    pub fn spawn(spec: &SpawnSpec, size: PtySize, slot: SessionSlot) -> std::io::Result<Self> {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(std::io::Error::other)?;

        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command.cwd(&spec.cwd);
        command.env("TERM", "xterm-256color");
        command.env("ORCH_TUI_CLIENT", "web");
        command.env_remove(SCRUBBED_ENV);

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(std::io::Error::other)?;
        // The child holds its own copy; keeping ours open would stop the
        // reader from ever seeing EOF.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let mut writer = pair.master.take_writer().map_err(std::io::Error::other)?;

        let (output_tx, output) = mpsc::channel::<Vec<u8>>(OUTPUT_DEPTH);
        let (input, mut input_rx) = mpsc::channel::<Vec<u8>>(INPUT_DEPTH);

        std::thread::Builder::new()
            .name("orch-web-pty-read".into())
            .spawn(move || {
                let mut buf = [0u8; READ_CHUNK];
                // Once the receiver is gone the session is being torn down, but
                // the child may still be writing frames while it shuts down.
                // Keep draining (and discarding) until the pty closes: a child
                // blocked on a full pty never gets to handle SIGHUP and exit.
                let mut listening = true;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if listening && output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                                listening = false;
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        // Linux reports a closed pty as EIO rather than EOF.
                        Err(err) => {
                            tracing::debug!(%err, "pty read ended");
                            break;
                        }
                    }
                }
            })?;

        std::thread::Builder::new()
            .name("orch-web-pty-write".into())
            .spawn(move || {
                while let Some(chunk) = input_rx.blocking_recv() {
                    if let Err(err) = writer.write_all(&chunk).and_then(|()| writer.flush()) {
                        tracing::debug!(%err, "pty write ended");
                        break;
                    }
                }
            })?;

        Ok(PtySession {
            master: pair.master,
            child: Some(child),
            slot: Some(slot),
            input,
            output,
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        if let Err(err) = self.master.resize(size) {
            tracing::warn!(%err, cols, rows, "pty resize failed");
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let (Some(child), Some(slot)) = (self.child.take(), self.slot.take()) else {
            return;
        };
        let pid = child.process_id();
        let reaper = std::thread::Builder::new()
            .name("orch-web-pty-reap".into())
            .spawn(move || reap(child, slot));
        if let Err(err) = reaper {
            // The child handle went down with the closure, but the pid is
            // enough to stop it. There is no thread to wait out a grace on.
            eprintln!("orch web: could not start the reaper thread ({err}); killing child {pid:?}");
            signal_pid(pid, false);
            signal_pid(pid, true);
        }
    }
}

fn reap(mut child: Box<dyn Child + Send + Sync>, _slot: SessionSlot) {
    let pid = child.process_id();
    signal_pid(pid, false);

    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                tracing::debug!(%err, "waiting for the pty child failed");
                return;
            }
        }
    }

    eprintln!("orch web: child {pid:?} ignored SIGHUP for {KILL_GRACE:?}; killing it");
    signal_pid(pid, true);
    #[cfg(not(unix))]
    let _ = child.kill();
    if let Err(err) = child.wait() {
        tracing::debug!(%err, "waiting for the killed pty child failed");
    }
}

#[cfg(unix)]
fn signal_pid(pid: Option<u32>, kill: bool) {
    use nix::sys::signal::{Signal, kill as send};
    use nix::unistd::Pid;

    let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return;
    };
    let signal = if kill {
        Signal::SIGKILL
    } else {
        Signal::SIGHUP
    };
    if let Err(err) = send(Pid::from_raw(pid), signal) {
        // ESRCH: it already exited.
        tracing::debug!(%err, ?signal, pid, "signalling the pty child failed");
    }
}

#[cfg(not(unix))]
fn signal_pid(_pid: Option<u32>, _kill: bool) {}
