//! Direct command lifecycle. F3 must add handled signals and bounded teardown;
//! F4 must render the statuses and kept-target reasons. No tier runs here yet.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use super::{lease::Lease, target::Marker};

#[derive(Debug)]
pub(super) struct Status {
    pub(super) exit: ExitStatus,
    pub(super) elapsed: Duration,
}

pub(super) struct Runner<'a, 'm> {
    lease: &'a Lease,
    marker: &'m Marker<'m>,
    child: Option<Child>,
    statuses: Vec<Status>,
    failed: bool,
}

impl<'a, 'm> Runner<'a, 'm> {
    pub(super) fn new(lease: &'a Lease, marker: &'m Marker<'m>) -> Result<Self> {
        lease.validate()?;
        Ok(Self {
            lease,
            marker,
            child: None,
            statuses: Vec::new(),
            failed: false,
        })
    }

    fn start(&mut self, command: &mut Command) -> Result<Instant> {
        ensure!(
            !self.failed && self.child.is_none(),
            "command lifecycle already failed or busy"
        );
        self.lease.validate()?;
        // Stdio duplicates the SAME open description. No raw descriptors, no
        // pre_exec, no explicit unlock. This changes stdin from an ordinary
        // cargo run; R4/R11 must use the same stream or diagnose differences.
        command.stdin(self.marker.command_stdin()?);
        let start = Instant::now();
        match command.spawn() {
            Ok(child) => {
                self.child = Some(child);
                Ok(start)
            }
            Err(e) => {
                self.failed = true;
                Err(e).context("start direct command")
            }
        }
    }

    fn reap(&mut self, start: Instant) -> Result<ExitStatus> {
        let result = self.child.as_mut().context("no direct command")?.wait();
        match result {
            Ok(exit) => {
                self.child = None; // only after collecting the status
                self.statuses.push(Status {
                    exit,
                    elapsed: start.elapsed(),
                });
                Ok(exit)
            }
            Err(e) => {
                self.failed = true; // retain the own Child handle, no publication
                Err(e).context("wait/reap direct command")
            }
        }
    }

    pub(super) fn run(&mut self, command: &mut Command) -> Result<ExitStatus> {
        let start = self.start(command)?;
        self.reap(start)
    }

    pub(super) fn capture(&mut self, command: &mut Command, limit: usize) -> Result<Vec<u8>> {
        let result = self.capture_inner(command, limit);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn capture_inner(&mut self, command: &mut Command, limit: usize) -> Result<Vec<u8>> {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let start = self.start(command)?;
        let child = self.child.as_mut().context("no direct command")?;
        let stdout = child.stdout.take().context("missing captured stdout")?;
        let stderr = child.stderr.take().context("missing captured stderr")?;
        // Drain concurrently, retaining bounded bytes even on an overrun. F3
        // still owes bounds on blocked I/O and descendant-held output pipes.
        let out = std::thread::spawn(move || bounded(stdout, limit));
        let err = std::thread::spawn(move || bounded(stderr, 16 * 1024));
        let exit = self.reap(start);
        let output = out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout collector panicked"))?;
        let error = err
            .join()
            .map_err(|_| anyhow::anyhow!("stderr collector panicked"))?;
        let exit = exit?;
        let output = output?;
        let error = error?;
        if !exit.success() {
            // Keep command stderr in the task's redirected raw logs, not an
            // assertion that a failed capture produced an empty source set.
            bail!(
                "identity command exited {exit}: {}",
                String::from_utf8_lossy(&error)
            );
        }
        Ok(output)
    }

    pub(super) fn finished(&self) -> Result<()> {
        ensure!(
            !self.failed && self.child.is_none(),
            "direct command not reaped"
        );
        self.lease.validate()?;
        self.marker.check_identity()?;
        Ok(())
    }

    pub(super) fn statuses(&self) -> &[Status] {
        &self.statuses
    }
}

fn bounded(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    io::copy(&mut reader, &mut io::sink())?;
    if bytes.len() > limit {
        return Err(io::Error::other("command output exceeds capture budget"));
    }
    Ok(bytes)
}

impl Drop for Runner<'_, '_> {
    fn drop(&mut self) {
        // Only a handle we created; never a PID from diagnostics. This ordinary
        // error-path cleanup is not the F3 signal/bounded teardown contract.
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
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
