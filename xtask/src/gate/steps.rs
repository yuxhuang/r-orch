//! Direct owned children only: no group signaling, descendant census or PID reuse
//! after reaping. Capture never joins a thread waiting for a descendant's EOF.

use std::io::{self, Read};
use std::os::fd::AsFd;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use super::{lease::Lease, signals::Signals, target::Marker};

pub(super) const SIGNAL_GRACE: Duration = Duration::from_secs(2);
pub(super) const KILL_REAP: Duration = Duration::from_secs(3);
pub(super) const PIPE_DRAIN: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub(super) struct Status {
    pub(super) exit: ExitStatus,
    pub(super) elapsed: Duration,
    pub(super) command: String,
}

pub(super) struct Runner<'a, 'm> {
    lease: &'a Lease,
    marker: Option<&'m Marker<'m>>,
    child: Option<Child>,
    #[cfg(test)]
    qualification_drop_inheritance: bool,
    start: Option<Instant>,
    active_command: Option<String>,
    statuses: Vec<Status>,
    probe_statuses: Vec<Status>,
    unreaped_probe: bool,
    failed: bool,
    interrupted_cleanly: bool,
    failure_detail: Option<String>,
    signals: Option<Signals>,
    kill_end: Option<Instant>,
    finishing: bool,
    observer: Option<std::rc::Rc<std::cell::RefCell<super::contention::Sampler>>>,
}

impl<'a, 'm> Runner<'a, 'm> {
    pub(super) fn new(lease: &'a Lease, marker: &'m Marker<'m>) -> Result<Self> {
        marker.enable_signals()?;
        marker.check_identity()?;
        Self::with_marker(lease, Some(marker))
    }

    /// Only lifecycle finish may enter after a handled cancellation, and only
    /// after the old runner has reaped its owned child. No suite work resumes.
    pub(super) fn finish_only(lease: &'a Lease, marker: &'m Marker<'m>) -> Result<Self> {
        let mut runner = Self::new(lease, marker)?;
        runner.finishing = true;
        Ok(runner)
    }

    // Lease exec has no target, but owns its direct command through interruption.
    pub(super) fn without_marker(lease: &'a Lease) -> Result<Self> {
        Self::with_marker(lease, None)
    }

    fn with_marker(lease: &'a Lease, marker: Option<&'m Marker<'m>>) -> Result<Self> {
        lease.validate()?;
        Ok(Self {
            lease,
            marker,
            child: None,
            #[cfg(test)]
            qualification_drop_inheritance: false,
            start: None,
            active_command: None,
            statuses: Vec::new(),
            probe_statuses: Vec::new(),
            unreaped_probe: false,
            failed: false,
            interrupted_cleanly: false,
            failure_detail: None,
            signals: if marker.is_none() {
                Some(Signals::new()?)
            } else {
                None
            },
            kill_end: None,
            finishing: false,
            observer: None,
        })
    }

    pub(super) fn probe_statuses(&self) -> &[Status] {
        &self.probe_statuses
    }
    pub(super) fn observation(&mut self) -> Result<super::contention::Observation> {
        let mut probe = Self::with_marker(self.lease, self.marker)?;
        let result = super::contention::query(&mut probe);
        if !probe.child_reaped() {
            let _ = probe.stop();
        }
        let reaped = probe.child_reaped();
        self.probe_statuses
            .extend(probe.statuses().iter().cloned().map(|mut status| {
                status.command = format!("sampler-probe {}", status.command);
                status
            }));
        drop(probe);
        if !reaped {
            self.failed = true;
            self.unreaped_probe = true;
            bail!("sampler direct child unreaped; target kept; status unknown");
        }
        result
    }
    pub(super) fn observe(
        &mut self,
        observer: std::rc::Rc<std::cell::RefCell<super::contention::Sampler>>,
    ) {
        self.observer = Some(observer);
    }
    fn sample_due(&mut self) {
        if let Some(observer) = self.observer.clone() {
            let due = observer.borrow().due();
            if !due {
                return;
            }
            // The probe owns its direct children too and duplicates the same
            // marker description. It has no observer, so sampling cannot recur.
            let result = self.observation();
            observer.borrow_mut().record(result);
        }
    }

    fn wait_signal(&mut self, delay: Duration) -> Option<i32> {
        if self.finishing {
            self.pause(delay);
            return None;
        }
        match self.marker {
            Some(marker) => marker.signal_wait(delay),
            None => self.signals.as_mut().unwrap().wait(delay),
        }
    }

    fn pause(&self, delay: Duration) {
        match self.marker {
            Some(marker) => marker.signal_pause(delay),
            None => self.signals.as_ref().unwrap().pause(delay),
        }
    }

    fn validate(&self) -> Result<()> {
        self.lease.validate()?;
        if let Some(marker) = self.marker {
            marker.check_identity()?;
        }
        Ok(())
    }

    fn start(&mut self, command: &mut Command) -> Result<()> {
        ensure!(
            !self.failed && self.child.is_none(),
            "command lifecycle already failed or busy"
        );
        ensure!(
            self.wait_signal(Duration::ZERO).is_none(),
            "INTERRUPTED before command"
        );
        self.validate()?;
        // Duplicate the SAME locked open description on an extra fd. Keep it
        // only across spawn, never in Command's retained stdio configuration.
        let inherited = self.marker.map(Marker::command_descriptor).transpose()?;
        #[cfg(test)]
        let inherited = if self.qualification_drop_inheritance {
            drop(inherited);
            None
        } else {
            inherited
        };
        let start = Instant::now();
        let identity = format!(
            "program={:?} args={:?} cwd={:?}",
            command.get_program(),
            command.get_args().collect::<Vec<_>>(),
            command.get_current_dir()
        );
        let spawned = command.spawn();
        drop(inherited); // close, NEVER unlock, on success and spawn failure
        match spawned {
            Ok(child) => {
                self.child = Some(child);
                self.start = Some(start);
                self.active_command = Some(identity);
                Ok(())
            }
            Err(e) => {
                self.failed = true;
                Err(e).context("start direct command")
            }
        }
    }

    // Test-only access to the real start/drive path. No recovery policy is
    // injected: qualification records the actual native admission outcome.
    #[cfg(test)]
    pub(super) fn qualification_run(
        &mut self,
        command: &mut Command,
        ready: &std::path::Path,
        drop_inheritance: bool,
        resistant: bool,
        shared_unlock: bool,
    ) -> Result<ExitStatus> {
        self.qualification_drop_inheritance = drop_inheritance;
        self.start(command)?;
        if resistant {
            let prepared = ready.with_file_name("resistant.ready");
            let end = Instant::now() + Duration::from_secs(10);
            while !prepared.exists() {
                ensure!(Instant::now() < end, "resistant child handshake");
                std::thread::sleep(POLL);
            }
            self.forward(nix::libc::SIGTERM)?;
            std::thread::sleep(SIGNAL_GRACE + Duration::from_millis(100));
            ensure!(
                self.child.as_mut().unwrap().try_wait()?.is_none(),
                "child did not resist TERM"
            );
        }
        if shared_unlock {
            ensure!(
                resistant,
                "unlock discriminator needs a resistant direct child"
            );
            // The command has inherited the locked description and resisted TERM
            // beyond the grace. Release ALL sharers only now, not before spawn.
            self.marker.unwrap().qualification_unlock()?;
        }
        std::fs::write(ready, self.child.as_ref().unwrap().id().to_string())?;
        self.drive(None)
    }

    fn observe_exit(&mut self) -> Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        if let Some(exit) = child.try_wait().context("try_wait direct command")? {
            self.child = None; // signal authority ends here; retain the real status
            self.statuses.push(Status {
                exit,
                elapsed: self.start.take().unwrap().elapsed(),
                command: self.active_command.take().unwrap(),
            });
            Ok(Some(exit))
        } else {
            Ok(None)
        }
    }

    fn forward(&mut self, number: i32) -> Result<()> {
        if let Some(child) = &self.child {
            // The child is unreaped and exclusively owned. An exited zombie
            // retains its PID until our next try_wait; never signal saved PIDs.
            let pid = i32::try_from(child.id()).context("direct child pid overflow")?;
            match kill(Pid::from_raw(pid), Signal::try_from(number)?) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(e) => return Err(e).context("forward signal to direct child"),
            }
        }
        Ok(())
    }

    fn drive(&mut self, mut capture: Option<&mut dyn Drain>) -> Result<ExitStatus> {
        let mut failure: Option<anyhow::Error> = None;
        let mut cancellation_only = false;
        let mut grace = None;
        let mut exit = None;
        let mut drain_end = None;
        loop {
            if failure.is_none() {
                self.sample_due();
            }
            let mut interrupted = self.wait_signal(Duration::ZERO);
            if failure.is_none() {
                if let Some(number) = interrupted {
                    failure = Some(anyhow::anyhow!("INTERRUPTED by signal {number}"));
                    cancellation_only = true;
                } else if let Err(e) = self.validate() {
                    // Validation may itself deliver a signal through the shared
                    // marker listener. Preserve its number before forwarding;
                    // never translate a late HUP/INT into a spurious TERM.
                    interrupted = self.wait_signal(Duration::ZERO);
                    cancellation_only = interrupted.is_some();
                    failure = Some(if let Some(number) = interrupted {
                        anyhow::anyhow!("INTERRUPTED by signal {number}")
                    } else {
                        e.context("lease/marker lost; target kept")
                    });
                }
            }
            match self.observe_exit() {
                Ok(Some(status)) => {
                    exit = Some(status);
                    drain_end = Some(Instant::now() + PIPE_DRAIN);
                }
                Ok(None) => {}
                Err(e) => {
                    cancellation_only = false;
                    if failure.is_none() {
                        failure = Some(e);
                    }
                }
            }
            if let Some(streams) = capture.as_deref_mut()
                && let Err(e) = streams.drain()
            {
                failure = Some(match failure {
                    Some(error) if cancellation_only => error.context(e),
                    Some(error) => error,
                    None => e,
                });
                cancellation_only = false;
            }
            if exit.is_some() {
                if capture.as_ref().is_none_or(|c| c.eof()) {
                    break;
                }
                if Instant::now() >= drain_end.unwrap() {
                    cancellation_only = false;
                    let detail = "capture EOF deadline; descendant may hold output; target kept";
                    failure = Some(match failure {
                        Some(error) => error.context(detail),
                        None => anyhow::anyhow!(detail),
                    });
                    break;
                }
                // No signaling here: the leader is already reaped.
            } else if failure.is_some() {
                self.failed = true;
                if grace.is_none() {
                    // Teardown never writes a log or waits for an output pipe.
                    if let Err(e) = self.forward(interrupted.unwrap_or(nix::libc::SIGTERM)) {
                        cancellation_only = false;
                        failure = Some(failure.unwrap().context(e));
                    }
                    grace = Some(Instant::now() + SIGNAL_GRACE);
                }
                if self.kill_end.is_none() && Instant::now() >= grace.unwrap() {
                    if let Some(child) = self.child.as_mut() {
                        let _ = child.kill(); // still our unreaped direct child
                    }
                    self.kill_end = Some(Instant::now() + KILL_REAP);
                }
                if self.kill_end.is_some_and(|end| Instant::now() >= end) {
                    bail!(
                        "{}; direct child not reaped after KILL; target kept; status unknown",
                        failure.unwrap()
                    );
                }
            }
            // Polling drives signal delivery without collector threads. Bound
            // each capture read batch so continuous output cannot starve teardown.
            if interrupted.is_some() {
                self.pause(POLL);
            } else {
                self.wait_signal(POLL);
            }
        }
        if let Some(e) = failure {
            self.failed = true;
            self.interrupted_cleanly = cancellation_only && self.child_reaped();
            return Err(e);
        }
        self.validate().inspect_err(|_| self.failed = true)?;
        exit.context("missing terminal direct status")
    }

    fn remember_failure<T>(&mut self, result: &Result<T>) {
        if let Err(error) = result {
            self.failed = true;
            // The first failure can disappear before finish (for example a
            // temporary link removed by another process). Keep its full causal
            // chain rather than replacing it with the sticky lifecycle flag.
            self.failure_detail
                .get_or_insert_with(|| format!("{error:#}"));
        }
    }

    pub(super) fn run(&mut self, command: &mut Command) -> Result<ExitStatus> {
        let result = self.start(command).and_then(|()| self.drive(None));
        self.remember_failure(&result);
        result
    }

    pub(super) fn capture(&mut self, command: &mut Command, limit: usize) -> Result<Vec<u8>> {
        let result = self.capture_raw(command, limit).and_then(|out| {
            ensure!(
                out.exit.success(),
                "identity command exited {}: {}",
                out.exit,
                String::from_utf8_lossy(&out.stderr)
            );
            Ok(out.stdout)
        });
        self.remember_failure(&result);
        result
    }

    pub(super) fn capture_raw(&mut self, command: &mut Command, limit: usize) -> Result<Raw> {
        let result = (|| {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            self.start(command)?;
            let child = self.child.as_mut().context("no direct command")?;
            let mut capture = Capture {
                out: Pipe::new(child.stdout.take().context("missing stdout")?, limit)?,
                err: Pipe::new(child.stderr.take().context("missing stderr")?, 16 * 1024)?,
            };
            let exit = self.drive(Some(&mut capture))?;
            Ok(Raw {
                stdout: capture.out.bytes,
                stderr: capture.err.bytes,
                exit,
            })
        })();
        self.remember_failure(&result);
        if result.is_err() {
            // Includes pipe setup failure after spawn. Still own/reap the child.
            if self.child.is_some() && self.kill_end.is_none() {
                let _ = self.stop();
            }
        }
        result
    }

    /// Combined stdout/stderr through one kernel pipe preserves ordering for
    /// Cargo's Running lines and libtest results. Nonzero child exit is data,
    /// not a capture error. Never fabricate an exit on interrupted collection.
    pub(super) fn logged(&mut self, command: &mut Command, limit: usize) -> Result<Logged> {
        let result = (|| {
            let (reader, writer) = nix::unistd::pipe()?;
            for fd in [&reader, &writer] {
                fcntl(fd, FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
            }
            command
                .stdout(Stdio::from(writer.try_clone()?))
                .stderr(Stdio::from(writer));
            let mut stream = Pipe::new(std::fs::File::from(reader), limit)?;
            self.start(command)?;
            // Command retains configured pipe handles after spawn. Close these
            // parent-side duplicates before waiting for the pipe's real EOF.
            command.stdout(Stdio::null()).stderr(Stdio::null());
            let result = self.drive(Some(&mut stream));
            match result {
                Ok(exit) => Ok(Logged {
                    bytes: stream.bytes,
                    exit,
                }),
                Err(error) => Err(LogFailure {
                    error,
                    bytes: stream.bytes,
                }
                .into()),
            }
        })();
        self.remember_failure(&result);
        if result.is_err() && self.child.is_some() && self.kill_end.is_none() {
            let _ = self.stop();
        }
        result
    }

    fn stop(&mut self) -> Result<()> {
        self.failed = true;
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        let end = *self
            .kill_end
            .get_or_insert_with(|| Instant::now() + KILL_REAP);
        while self.child.is_some() && Instant::now() < end {
            self.observe_exit()?;
            if self.child.is_some() {
                std::thread::sleep(POLL);
            }
        }
        ensure!(
            self.child.is_none(),
            "direct child not reaped; target kept; status unknown"
        );
        Ok(())
    }

    pub(super) fn finished(&mut self) -> Result<()> {
        ensure!(
            self.wait_signal(Duration::ZERO).is_none(),
            "INTERRUPTED at finish; target kept"
        );
        ensure!(
            !self.failed && self.child.is_none(),
            "direct command not reaped or lifecycle failed: {}",
            self.failure_detail
                .as_deref()
                .unwrap_or("no causal detail available")
        );
        self.validate()
    }

    /// Cancellation alone may finish after actual reap and the ordinary output
    /// drain. Any collection, spawn, authority or reap failure stays sticky.
    /// NativeUse rechecks lease/marker authority before finish-only capture.
    pub(super) fn finished_interrupted(&self) -> Result<()> {
        ensure!(
            self.child_reaped() && (!self.failed || self.interrupted_cleanly),
            "interrupted command lifecycle unresolved: {}",
            self.failure_detail
                .as_deref()
                .unwrap_or("direct child not reaped")
        );
        Ok(())
    }

    pub(super) fn interrupted(&mut self) -> Option<i32> {
        self.wait_signal(Duration::ZERO)
    }
    pub(super) fn child_reaped(&self) -> bool {
        self.child.is_none() && !self.unreaped_probe
    }
    pub(super) fn statuses(&self) -> &[Status] {
        &self.statuses
    }
}

#[derive(Debug)]
pub(super) struct LogFailure {
    pub(super) error: anyhow::Error,
    pub(super) bytes: Vec<u8>,
}
impl std::fmt::Display for LogFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}
impl std::error::Error for LogFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

pub(super) struct Raw {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) exit: ExitStatus,
}

pub(super) struct Logged {
    pub(super) bytes: Vec<u8>,
    pub(super) exit: ExitStatus,
}

struct Pipe<R> {
    reader: R,
    bytes: Vec<u8>,
    limit: usize,
    eof: bool,
}
impl<R: Read + AsFd> Pipe<R> {
    fn new(reader: R, limit: usize) -> Result<Self> {
        let flags = OFlag::from_bits_truncate(fcntl(&reader, FcntlArg::F_GETFL)?);
        fcntl(&reader, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(Self {
            reader,
            bytes: Vec::new(),
            limit,
            eof: false,
        })
    }
    fn drain(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut chunk = [0u8; 8192];
        // Pinned capture semantics need a byte budget and a finite service batch;
        // a general line/async stream collector does not supply these together.
        for _ in 0..8 {
            match self.reader.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(n) => {
                    let remaining = self.limit.saturating_sub(self.bytes.len());
                    self.bytes.extend_from_slice(&chunk[..n.min(remaining)]);
                    ensure!(n <= remaining, "command output exceeds capture budget");
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("capture pipe read"),
            }
        }
        Ok(())
    }
}
trait Drain {
    fn drain(&mut self) -> Result<()>;
    fn eof(&self) -> bool;
}

impl<R: Read + AsFd> Drain for Pipe<R> {
    fn drain(&mut self) -> Result<()> {
        Pipe::drain(self)
    }
    fn eof(&self) -> bool {
        self.eof
    }
}

struct Capture {
    out: Pipe<std::process::ChildStdout>,
    err: Pipe<std::process::ChildStderr>,
}
impl Drain for Capture {
    fn drain(&mut self) -> Result<()> {
        // Always service both; a broken/full stdout cannot suppress stderr or
        // delay the lifecycle's signal/reap loop.
        let out = self.out.drain();
        let err = self.err.drain();
        out.and(err)
    }
    fn eof(&self) -> bool {
        self.out.eof && self.err.eof
    }
}

impl Drop for Runner<'_, '_> {
    fn drop(&mut self) {
        // Last resort on unwind. No indefinite wait and no target cleanup. A
        // pathological unreapable child leaves an unfinished marker, never a
        // fabricated status. OS blocking spawn/fs calls are not hard bounded.
        if self.child.is_some() {
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::gate::lease::{Mode, Owned};
    use std::fs::{self, File, OpenOptions};
    use std::os::fd::AsFd;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct Scratch(pub(crate) PathBuf);
    impl Scratch {
        pub(crate) fn new() -> Self {
            let parent = std::env::var_os("XTASK_F2_PRIVATE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .canonicalize()
                .unwrap();
            Self::new_at(&parent)
        }
        pub(crate) fn temporary() -> Self {
            Self::new_at(&std::env::temp_dir().canonicalize().unwrap())
        }
        fn new_at(parent: &Path) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            assert!(
                !parent
                    .ancestors()
                    .any(|p| p.file_name().is_some_and(|s| s == ".orchestry"))
            );
            let path = parent.join(format!(
                "f2-{:x}-{:x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        pub(crate) fn owner(&self) -> u32 {
            fs::metadata(&self.0).unwrap().uid()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    pub(crate) struct ChildGuard(pub(crate) Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub(crate) fn fixture(scratch: &Scratch, label: &str, mode: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args([
            "--exact",
            "gate::steps::tests::child_fixture",
            "--nocapture",
        ])
        .env_clear()
        .env("F2_MODE", mode)
        .env("F2_ROOT", &scratch.0)
        .env("F2_LABEL", label)
        .env("HOME", &scratch.0)
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", scratch.0.join("global.yml"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        c
    }

    pub(crate) fn ready(child: &mut Child, path: &Path) {
        let end = Instant::now() + Duration::from_secs(8);
        while !path.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "fixture exited before handshake"
            );
            assert!(Instant::now() < end, "fixture handshake timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // CLOEXEC does not prevent another test thread's in-flight fork from
    // copying a locked description before exec. Clearing CLOEXEC for a Runner
    // handover can also let that unrelated command retain it after exec. Tests
    // asserting the final close need a process that owns only their lifecycle.
    // They still run in parallel; every original immediate lock assertion runs
    // unchanged in the child, without an unlock, retry or delayed probe.
    pub(crate) fn isolated_description_owner(name: &str) -> bool {
        if std::env::var("XTASK_DESCRIPTION_OWNER").as_deref() == Ok(name) {
            return true;
        }
        let scratch = Scratch::new();
        fs::write(scratch.0.join("global.yml"), b"{}\n").unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", name, "--nocapture"]).env_clear();
        for key in [
            "PATH",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "RUSTUP_AUTO_INSTALL",
            "CARGO_NET_OFFLINE",
            "TMPDIR",
            "XTASK_F1_PRIVATE_ROOT",
            "XTASK_F2_PRIVATE_ROOT",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let output = command
            .env("HOME", &scratch.0)
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", scratch.0.join("global.yml"))
            .env("XTASK_DESCRIPTION_OWNER", name)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated description owner failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    #[test]
    fn sampler_failure_fixture() {
        let Some(root) = std::env::var_os("XTASK_PROBE_FAILURE_ROOT").map(PathBuf::from) else {
            return;
        };
        let owned = Owned::acquire(
            &root,
            fs::metadata(&root).unwrap().uid(),
            Mode::Exclusive,
            Duration::ZERO,
        )
        .unwrap();
        let lease = Lease::Owned(owned);
        let mut runner = Runner::without_marker(&lease).unwrap();
        assert!(runner.observation().is_err());
        runner.finished().unwrap();
        assert_eq!(
            runner.probe_statuses().len(),
            1,
            "query wait status was discarded"
        );
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(runner.probe_statuses()[0].exit.signal(), Some(9));
        assert!(
            runner.statuses().is_empty(),
            "probe polluted primary step status indices"
        );
        assert!(
            runner
                .run(&mut Command::new("/usr/bin/true"))
                .unwrap()
                .success()
        );
        assert_eq!(runner.statuses().len(), 1);
        runner.finished().unwrap();
    }

    #[test]
    fn overflowing_sampler_query_reaps_its_direct_child_and_retains_wait_status() {
        let s = Scratch::new();
        let bin = s.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let script = bin.join("ps");
        fs::write(&script,b"#!/bin/sh\ntrap '' TERM\nprintf '%s' \"$$\" > \"$XTASK_PROBE_FAILURE_ROOT/ps.pid\"\nwhile :; do printf '%s\\n' '900000 1 cargo'; done\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let log = File::create(s.0.join("fixture.log")).unwrap();
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gate::steps::tests::sampler_failure_fixture",
                    "--nocapture",
                ])
                .env_clear()
                .env("HOME", &s.0)
                .env("ORCHESTRY_GLOBAL_CONFIG_PATH", s.0.join("global.yml"))
                .env("XTASK_PROBE_FAILURE_ROOT", &s.0)
                .env(
                    "PATH",
                    std::env::join_paths([bin, PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
                        .unwrap(),
                )
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "probe fixture did not finish within direct reap budget"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "{}",
            fs::read_to_string(s.0.join("fixture.log")).unwrap()
        );
        let pid = fs::read_to_string(s.0.join("ps.pid")).unwrap();
        let output = Command::new("/bin/ps")
            .args(["-p", &pid, "-o", "pid="])
            .output()
            .unwrap();
        assert!(output.stdout.is_empty(), "reaped sampler PID still present");
    }

    #[test]
    fn child_fixture() {
        let Some(mode) = std::env::var_os("F2_MODE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("F2_ROOT").unwrap());
        let label = std::env::var("F2_LABEL").unwrap();
        let _lease = if mode == "holder" || mode == "server-holder" {
            Some(
                Owned::acquire(
                    &root,
                    fs::metadata(&root).unwrap().uid(),
                    Mode::Exclusive,
                    Duration::ZERO,
                )
                .unwrap(),
            )
        } else {
            None
        };
        let _server = if mode == "server-holder" {
            let parent = PathBuf::from(std::env::var_os("F2_SOCKET_ROOT").unwrap());
            Some(_lease.as_ref().unwrap().serve(&parent).unwrap())
        } else {
            None
        };
        let inherited = if mode == "inherited" {
            Some(
                crate::gate::lease::Inherited::from_environment(
                    &root,
                    fs::metadata(&root).unwrap().uid(),
                )
                .unwrap()
                .unwrap(),
            )
        } else {
            None
        };
        if mode == "explicit-unlock" {
            let file = File::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
            file.unlock().unwrap(); // intentional RC18 discriminator, never production
        }
        fs::write(root.join(format!("{label}.ready")), b"ready").unwrap();
        let end = Instant::now() + Duration::from_secs(12);
        while !root.join(format!("{label}.release")).exists() {
            assert!(Instant::now() < end, "fixture self-deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(lease) = inherited {
            assert!(lease.validate().is_err());
        }
    }

    #[test]
    fn rc7_rc8_holder_kill_frees_machine_lease_but_direct_description_blocks_marker() {
        if !isolated_description_owner(
            "gate::steps::tests::rc7_rc8_holder_kill_frees_machine_lease_but_direct_description_blocks_marker",
        ) {
            return;
        }
        let s = Scratch::new();
        let path = s.0.join("marker");
        let marker = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        marker.lock().unwrap();
        let mut holder = ChildGuard(
            fixture(&s, "holder", "holder")
                .stdin(marker.try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        ready(&mut holder.0, &s.0.join("holder.ready"));
        let mut direct = ChildGuard(
            fixture(&s, "direct", "hold")
                .stdin(marker.try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        ready(&mut direct.0, &s.0.join("direct.ready"));
        drop(marker); // no parent descriptor keeps either lock alive
        assert!(holder.0.try_wait().unwrap().is_none());
        holder.0.kill().unwrap();
        assert!(!holder.0.wait().unwrap().success());
        let lease = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        lease.validate().unwrap();
        let probe = File::open(&path).unwrap();
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        assert!(direct.0.try_wait().unwrap().is_none());
        fs::write(s.0.join("direct.release"), b"finish").unwrap();
        assert!(direct.0.wait().unwrap().success());
        probe.try_lock().unwrap();
    }

    #[test]
    fn rc18_no_handover_and_explicit_unlock_are_distinguished_from_close() {
        for mode in ["no-handover", "explicit-unlock"] {
            let s = Scratch::new();
            let path = s.0.join("marker");
            let marker = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            marker.lock().unwrap();
            let mut c = fixture(&s, "direct", mode);
            if mode == "explicit-unlock" {
                c.stdin(marker.try_clone().unwrap());
            } else {
                c.stdin(Stdio::null());
            }
            let mut child = ChildGuard(c.spawn().unwrap());
            ready(&mut child.0, &s.0.join("direct.ready"));
            drop(marker);
            let probe = File::open(&path).unwrap();
            probe.try_lock().unwrap(); // incorrect implementations allow reuse while live
            assert!(child.0.try_wait().unwrap().is_none());
            fs::write(s.0.join("direct.release"), b"finish").unwrap();
            assert!(child.0.wait().unwrap().success());
        }
    }

    #[test]
    fn rc18_per_process_lock_does_not_survive_holder_even_with_live_descriptor() {
        let s = Scratch::new();
        let path = s.0.join("marker");
        let marker = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let script = "import fcntl,os,time\nfcntl.lockf(0,fcntl.LOCK_EX)\nopen(os.environ['READY'],'w').close()\ntime.sleep(12)";
        let mut holder = ChildGuard(
            Command::new("/usr/bin/python3")
                .args(["-c", script])
                .env_clear()
                .env("READY", s.0.join("holder.ready"))
                .env("HOME", &s.0)
                .stdin(marker.try_clone().unwrap())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        ready(&mut holder.0, &s.0.join("holder.ready"));
        let mut direct = ChildGuard(
            fixture(&s, "direct", "hold")
                .stdin(marker.try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        ready(&mut direct.0, &s.0.join("direct.ready"));
        drop(marker);
        holder.0.kill().unwrap();
        holder.0.wait().unwrap();
        // Use the SAME lock API for the per-process probe; flock and fcntl are
        // independent on local Unix filesystems, so flock success proves nothing.
        let result = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import fcntl; fcntl.lockf(0,fcntl.LOCK_EX|fcntl.LOCK_NB)",
            ])
            .env_clear()
            .env("HOME", &s.0)
            .stdin(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap(),
            )
            .status()
            .unwrap();
        assert!(result.success());
        assert!(direct.0.try_wait().unwrap().is_none());
        fs::write(s.0.join("direct.release"), b"finish").unwrap();
        assert!(direct.0.wait().unwrap().success());
    }

    #[test]
    fn rc18_real_pinned_cargo_keeps_marker_stdin_while_building_on_native_filesystem() {
        let s = Scratch::new();
        let repo = s.0.join("repo");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(repo.join("src")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname=\"f2-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[workspace]\n",
        )
        .unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        fs::write(
            repo.join("build.rs"),
            r#"
fn main() {
    let root = std::path::PathBuf::from(std::env::var_os("F2_ROOT").unwrap());
    std::fs::write(root.join("cargo.ready"), b"ready").unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while !root.join("cargo.release").exists() {
        assert!(std::time::Instant::now() < end, "fixture self-deadline");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
"#,
        )
        .unwrap();
        let output = Command::new("rustup")
            .args(["which", "rustc"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let rustc = PathBuf::from(std::str::from_utf8(&output.stdout).unwrap().trim());
        let cargo = rustc.with_file_name("cargo");
        let path = s.0.join("marker");
        let marker = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        marker.lock().unwrap();
        let log = File::create(s.0.join("cargo.log")).unwrap();
        let launch = Scratch::temporary();
        let mut child = ChildGuard(
            Command::new(cargo)
                .args(["build", "--offline", "--manifest-path"])
                .arg(repo.join("Cargo.toml"))
                .current_dir(&launch.0)
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("RUSTC", rustc)
                .env("HOME", &s.0)
                .env("F2_ROOT", &s.0)
                .env("CARGO_TARGET_DIR", repo.join("target"))
                .env("ORCHESTRY_GLOBAL_CONFIG_PATH", s.0.join("global.yml"))
                .stdin(marker.try_clone().unwrap())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        ready(&mut child.0, &s.0.join("cargo.ready"));
        drop(marker);
        assert!(child.0.try_wait().unwrap().is_none());
        let probe = File::open(&path).unwrap();
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        // Verify the DIRECT cargo's fd0, so a build-script descendant holding
        // a description by itself cannot masquerade as cargo retaining it.
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("/usr/sbin/lsof")
                .args(["-a", "-p", &child.0.id().to_string(), "-d", "0", "-Ffin"])
                .env_clear()
                .env("HOME", &s.0)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "lsof unavailable: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(
                text.lines()
                    .any(|l| l == format!("i{}", probe.metadata().unwrap().ino())),
                "{text}"
            );
            assert!(
                text.lines().any(|l| l == format!("n{}", path.display())),
                "{text}"
            );
            eprintln!("native direct cargo marker observation: {text}");
        }
        #[cfg(target_os = "linux")]
        {
            let meta = fs::metadata(format!("/proc/{}/fd/0", child.0.id())).unwrap();
            let expected = probe.metadata().unwrap();
            assert_eq!((meta.dev(), meta.ino()), (expected.dev(), expected.ino()));
        }
        fs::write(s.0.join("cargo.release"), b"finish").unwrap();
        assert!(
            child.0.wait().unwrap().success(),
            "{}",
            fs::read_to_string(s.0.join("cargo.log")).unwrap()
        );
        probe.try_lock().unwrap();
    }
}
