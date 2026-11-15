//! Cooperative machine lease. Files are close-on-exec; only the target marker
//! is handed to commands. No path is ever unlinked to make a lock available.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

const SOCKET_ENV: &str = "XTASK_GATE_LEASE_SOCKET";
const TOKEN_ENV: &str = "XTASK_GATE_LEASE_TOKEN";
const POLL: Duration = Duration::from_millis(10);
const CHECK_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Exclusive,
    Shared,
}

#[derive(Debug)]
pub(super) struct Contended {
    diagnostics: String,
}

impl std::fmt::Display for Contended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cooperative lease is busy; nothing ran; purpose/holder diagnostics (unvalidated, never signal authority): {}",
            self.diagnostics
        )
    }
}
impl std::error::Error for Contended {}

struct Held {
    file: File,
    path: PathBuf,
    owner: u32,
    lost: AtomicBool,
    exclusive: bool,
}

fn check_file(path: &Path, meta: &fs::Metadata, owner: u32) -> Result<()> {
    ensure!(
        meta.is_file() && meta.uid() == owner && meta.nlink() == 1 && meta.mode() & 0o7077 == 0,
        "REFUSED: lease structural refusal at {}: require private, singly linked regular file of effective account",
        path.display()
    );
    Ok(())
}

fn open(path: &Path, owner: u32) -> Result<File> {
    // Canonicalize the parent: /tmp -> /private/tmp is expected on macOS.
    // Never canonicalize/follow the lock leaf itself.
    let before = match fs::symlink_metadata(path) {
        Ok(meta) => {
            check_file(path, &meta, owner)?;
            Some(meta)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("inspect {}", path.display())),
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(before.is_none())
        .create_new(before.is_none())
        .mode(0o600)
        .custom_flags(super::target::open_flags(false)?)
        .open(path)
        .with_context(|| format!("open lease {}", path.display()))?;
    let held = file.metadata()?;
    check_file(path, &held, owner)?;
    let now = fs::symlink_metadata(path)?;
    check_file(path, &now, owner)?;
    ensure!(
        same(&held, &now) && before.as_ref().is_none_or(|m| same(m, &held)),
        "lease identity changed during open: {}",
        path.display()
    );
    Ok(file)
}

fn same(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

impl Held {
    fn validate(&self) -> Result<()> {
        let result = (|| {
            ensure!(!self.lost.load(Ordering::Acquire), "lease previously lost");
            let now = fs::symlink_metadata(&self.path)?;
            check_file(&self.path, &now, self.owner)?;
            ensure!(same(&self.file.metadata()?, &now), "lease path replaced");
            Ok(())
        })();
        if result.is_err() {
            self.lost.store(true, Ordering::Release);
        }
        result
    }
}

pub(super) struct Owned {
    held: Arc<Held>,
}

fn take(file: &File, mode: Mode, deadline: Instant) -> Result<()> {
    loop {
        let result = match mode {
            Mode::Exclusive => file.try_lock(),
            Mode::Shared => file.try_lock_shared(),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Contended {
                    diagnostics: "unavailable".into(),
                }
                .into());
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

fn contention_diagnostics(error: anyhow::Error, directory: &Path, owner: u32) -> anyhow::Error {
    if !error.is::<Contended>() {
        return error;
    }
    let path = directory.join("r-orch-gate.holder");
    // Read only: never create a missing diagnostic or follow a link. Its
    // contents are unvalidated text, with no signalling/reset authority.
    let read = (|| -> Result<Vec<u8>> {
        let before = fs::symlink_metadata(&path)?;
        check_file(&path, &before, owner)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(super::target::open_flags(false)?)
            .open(&path)?;
        let held = file.metadata()?;
        check_file(&path, &held, owner)?;
        ensure!(
            same(&before, &held) && same(&held, &fs::symlink_metadata(&path)?),
            "holder diagnostic changed"
        );
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 4096, "holder diagnostic exceeds budget");
        Ok(bytes)
    })();
    Contended {
        diagnostics: read
            .map(|b| super::report::diagnostic(&b))
            .unwrap_or_else(|_| "unavailable".into()),
    }
    .into()
}

impl Owned {
    pub(super) fn acquire(
        directory: &Path,
        owner: u32,
        mode: Mode,
        wait: Duration,
    ) -> Result<Self> {
        let directory = directory
            .canonicalize()
            .context("canonicalize lease directory")?;
        let turnstile = open(&directory.join("r-orch-gate.turnstile"), owner)?;
        let deadline = Instant::now()
            .checked_add(wait)
            .context("lease wait overflow")?;
        take(&turnstile, Mode::Exclusive, deadline)
            .map_err(|e| contention_diagnostics(e, &directory, owner))?;
        let path = directory.join("r-orch-gate.lease");
        let file = open(&path, owner)?;
        take(&file, mode, deadline).map_err(|e| contention_diagnostics(e, &directory, owner))?;
        let lease = Self {
            held: Arc::new(Held {
                file,
                path,
                owner,
                lost: AtomicBool::new(false),
                exclusive: matches!(mode, Mode::Exclusive),
            }),
        };
        lease.validate()?;
        // Closing the turnstile lets the next waiter queue; never unlock a
        // description, even here. The machine lease stays in this process.
        drop(turnstile);
        Ok(lease)
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.held.validate()
    }

    pub(super) fn purpose(&self, purpose: &str) -> Result<()> {
        self.validate()?;
        let path = self
            .held
            .path
            .parent()
            .context("lease parent")?
            .join("r-orch-gate.holder");
        let mut file = open(&path, self.held.owner)?;
        let text = format!(
            "purpose={}\npid={}\nstarted={:?}\n",
            super::report::diagnostic(&purpose.as_bytes()[..purpose.len().min(512)]),
            std::process::id(),
            std::time::SystemTime::now()
        );
        file.seek(SeekFrom::Start(0))?;
        file.write_all(text.as_bytes())?;
        file.set_len(text.len() as u64)?;
        file.sync_all()?;
        self.validate()
    }

    pub(super) fn serve(&self, private_parent: &Path) -> Result<Server> {
        self.validate()?;
        ensure!(
            self.held.exclusive,
            "only an exclusive holder can lend the lease"
        );
        Server::new(Arc::clone(&self.held), private_parent)
    }
}

// Inheritance is a live challenge/response to the holder, not a PID, stale
// record, or environment assertion. The server runs IN the holder process;
// no helper keeps its machine lock alive after that process is killed.
pub(super) struct Inherited {
    socket: PathBuf,
    token: [u8; 32],
    lost: AtomicBool,
    lease_path: PathBuf,
    owner: u32,
}

impl Inherited {
    pub(super) fn from_environment(directory: &Path, owner: u32) -> Result<Option<Self>> {
        match (std::env::var_os(SOCKET_ENV), std::env::var_os(TOKEN_ENV)) {
            (None, None) => Ok(None),
            (Some(socket), Some(token)) => {
                let token = token
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid lease token"))?;
                Ok(Some(Self::from_capability(
                    directory,
                    owner,
                    socket.into(),
                    &token,
                )?))
            }
            _ => bail!("incomplete inherited lease capability"),
        }
    }

    pub(super) fn from_capability(
        directory: &Path,
        owner: u32,
        socket: PathBuf,
        token: &str,
    ) -> Result<Self> {
        ensure!(
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid lease token size"
        );
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&token[2 * i..2 * i + 2], 16).context("invalid lease token")?;
        }
        let inherited = Self {
            socket,
            token: bytes,
            lost: AtomicBool::new(false),
            lease_path: directory.canonicalize()?.join("r-orch-gate.lease"),
            owner,
        };
        inherited.validate()?;
        Ok(inherited)
    }

    pub(super) fn validate(&self) -> Result<()> {
        let result = (|| {
            ensure!(
                !self.lost.load(Ordering::Acquire),
                "inherited lease previously lost"
            );
            self.check_holder_record()?;
            let mut stream =
                UnixStream::connect(&self.socket).context("inherited lease holder unavailable")?;
            stream.set_read_timeout(Some(CHECK_TIMEOUT))?;
            stream.set_write_timeout(Some(CHECK_TIMEOUT))?;
            stream.write_all(&self.token)?;
            let mut response = [0];
            stream.read_exact(&mut response)?;
            ensure!(
                response == [1],
                "inherited holder does not validate the lease"
            );
            self.check_holder_record()?;
            Ok(())
        })();
        if result.is_err() {
            self.lost.store(true, Ordering::Release);
        }
        result
    }

    fn check_holder_record(&self) -> Result<()> {
        let before = fs::symlink_metadata(&self.lease_path)?;
        check_file(&self.lease_path, &before, self.owner)?;
        let mut probe = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(super::target::open_flags(false)?)
            .open(&self.lease_path)?;
        let held = probe.metadata()?;
        check_file(&self.lease_path, &held, self.owner)?;
        ensure!(
            same(&before, &held) && same(&held, &fs::symlink_metadata(&self.lease_path)?),
            "inherited lease path changed"
        );
        // A fresh shared attempt must be blocked by an exclusive lock. A stale
        // record/socket/token with an unlocked or merely shared lease is no grant.
        match probe.try_lock_shared() {
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(e.into()),
            Ok(()) => bail!("inherited holder no longer holds the exclusive lease"),
        }
        let mut bytes = Vec::new();
        (&mut probe).take(4097).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 4096, "holder record exceeds budget");
        let record: serde_json::Value = serde_json::from_slice(&bytes)?;
        let socket = self
            .socket
            .to_str()
            .context("non-UTF-8 lease session path")?;
        let token: String = self.token.iter().map(|b| format!("{b:02x}")).collect();
        ensure!(
            record["socket"].as_str() == Some(socket) && record["token"].as_str() == Some(&token),
            "inherited capability does not match the configured locked holder record"
        );
        Ok(())
    }
}

pub(super) enum Lease {
    Owned(Owned),
    Inherited(Inherited),
}

impl Lease {
    pub(super) fn configured(no_lease: bool, mode: Mode, wait: Duration) -> Result<Self> {
        ensure!(
            !no_lease,
            "REFUSED: configured lease required before target admission"
        );
        let directory = std::env::var_os("XTASK_GATE_LEASE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let output = Command::new("/usr/bin/id")
            .arg("-u")
            .env_clear()
            .output()
            .context("read effective account")?;
        ensure!(
            output.status.success(),
            "effective account command failed: {}",
            output.status
        );
        let owner = std::str::from_utf8(&output.stdout)?.trim().parse::<u32>()?;
        if let Ok(Some(inherited)) = Inherited::from_environment(&directory, owner) {
            return Ok(Self::Inherited(inherited));
        }
        // A stale/invalid token is no authority. Acquire the configured lease
        // normally; do not adopt a claimed holder or release anybody's lock.
        Ok(Self::Owned(Owned::acquire(&directory, owner, mode, wait)?))
    }

    pub(super) fn validate_exclusive(&self) -> Result<()> {
        self.validate()?;
        if let Self::Owned(owned) = self {
            ensure!(
                owned.held.exclusive,
                "REFUSED: target lifecycle requires the exclusive cooperative lease"
            );
        }
        // Inherited::validate challenges a live holder and independently checks
        // that a fresh shared attempt is excluded by its exclusive kernel lock.
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Owned(l) => l.validate(),
            Self::Inherited(l) => l.validate(),
        }
    }
}

pub(super) fn exec(command: &[std::ffi::OsString], wait: Duration) -> Result<i32> {
    exec_purpose(command, wait, "gate lease exec")
}

pub(super) fn exec_purpose(
    command: &[std::ffi::OsString],
    wait: Duration,
    purpose: &str,
) -> Result<i32> {
    use std::os::unix::process::ExitStatusExt;
    let lease = match Lease::configured(false, Mode::Exclusive, wait) {
        Err(e) if e.is::<Contended>() => {
            eprintln!("{e}");
            return Ok(75);
        }
        other => other?,
    };
    if let Lease::Owned(owned) = &lease {
        owned.purpose(purpose)?;
    }
    let mut child_command = Command::new(command.first().context("missing lease command")?);
    child_command.args(&command[1..]);
    let _server = match &lease {
        Lease::Owned(owned) => {
            // Short, exclusively created private session; macOS's normal
            // TMPDIR can be too long for a Unix control socket.
            let server = owned.serve(Path::new("/tmp"))?;
            server.attach(&mut child_command);
            Some(server)
        }
        Lease::Inherited(_) => None,
    };
    let mut runner = super::steps::Runner::without_marker(&lease)?;
    let result = runner.run(&mut child_command);
    // Only after teardown; closed stderr must not suppress cleanup or statuses.
    if let Some(number) = runner.interrupted() {
        let _ = writeln!(
            io::stderr(),
            "gate: INTERRUPTED by signal {number}; statuses: {:?}",
            runner.statuses()
        );
        ensure!(
            runner.child_reaped(),
            "interrupted direct child not reaped; status unknown"
        );
        return Ok(128 + number);
    }
    let status = result?;
    runner.finished()?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

/// Read-only observation, never creates a missing lock or adopts a diagnostic
/// pid. A free observation is not permission for a later command to skip acquire.
pub(super) fn status() -> Result<i32> {
    let directory = std::env::var_os("XTASK_GATE_LEASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/tmp".into())
        .canonicalize()?;
    let path = directory.join("r-orch-gate.lease");
    let owner = nix::unistd::geteuid().as_raw();
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            println!("lease: absent (observation only)")
        }
        Err(e) => return Err(e.into()),
        Ok(before) => {
            check_file(&path, &before, owner)?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(super::target::open_flags(false)?)
                .open(&path)?;
            ensure!(
                same(&before, &file.metadata()?) && same(&before, &fs::symlink_metadata(&path)?),
                "lease identity changed during status"
            );
            let state = match file.try_lock() {
                Ok(()) => "free (observation only)",
                Err(TryLockError::WouldBlock) => "busy",
                Err(TryLockError::Error(e)) => return Err(e.into()),
            };
            println!("lease: {state}\npath: {}", path.display());
            // Close, never unlock an open description shared with anyone else.
        }
    }
    let holder = directory.join("r-orch-gate.holder");
    match fs::symlink_metadata(&holder)
        .map_err(anyhow::Error::from)
        .and_then(|meta| {
            check_file(&holder, &meta, owner)?;
            super::files::read(&holder, 4096)
        }) {
        Ok(bytes) => println!(
            "purpose/holder diagnostics (unvalidated): {}",
            super::report::diagnostic(&bytes)
        ),
        Err(_) => println!("purpose/holder diagnostics (unvalidated): unavailable"),
    }
    Ok(0)
}

pub(super) struct Server {
    directory: PathBuf,
    token: [u8; 32],
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn new(held: Arc<Held>, parent: &Path) -> Result<Self> {
        let mut token = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut token)?;
        let hex: String = token.iter().map(|b| format!("{b:02x}")).collect();
        let directory = parent.join(format!("l-{}", &hex[..16]));
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let listener = UnixListener::bind(directory.join("s"))?;
        listener.set_nonblocking(true)?;
        let record =
            serde_json::to_vec(&serde_json::json!({"socket": directory.join("s"), "token": hex}))?;
        ensure!(record.len() <= 4096, "holder record exceeds budget");
        let mut file = held.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&record)?;
        file.set_len(record.len() as u64)?;
        file.sync_all()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(CHECK_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(CHECK_TIMEOUT));
                        let mut request = [0; 32];
                        let valid = stream.read_exact(&mut request).is_ok()
                            && request == token
                            && held.validate().is_ok();
                        let _ = stream.write_all(&[u8::from(valid)]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            directory,
            token,
            stop,
            thread: Some(thread),
        })
    }

    pub(super) fn attach(&self, command: &mut Command) {
        let token: String = self.token.iter().map(|b| format!("{b:02x}")).collect();
        command
            .env(SOCKET_ENV, self.directory.join("s"))
            .env(TOKEN_ENV, token);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Only the exclusively created session, never a machine lock. Keep it
        // on panic as diagnostic evidence. No process id is cleanup authority.
        if !thread::panicking() {
            let _ = fs::remove_file(self.directory.join("s"));
            let _ = fs::remove_dir(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::steps::tests::{ChildGuard, Scratch, fixture, ready};
    use std::os::unix::fs::symlink;

    #[test]
    fn live_contention_and_close_release_preserve_lock_inode() {
        let s = Scratch::new();
        let lease = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        let before = fs::metadata(s.0.join("r-orch-gate.lease")).unwrap();
        assert!(
            Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO)
                .unwrap_err_is_contention()
        );
        drop(lease);
        let next = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        assert!(same(&before, &fs::metadata(&next.held.path).unwrap()));
    }

    trait ContentionResult {
        fn unwrap_err_is_contention(self) -> bool;
    }
    impl ContentionResult for Result<Owned> {
        fn unwrap_err_is_contention(self) -> bool {
            matches!(self, Err(ref e) if e.is::<Contended>())
        }
    }

    #[test]
    fn contention_includes_unvalidated_holder_purpose_and_pid_without_authority() {
        let s = Scratch::new();
        let lease = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        lease.purpose("private-contract-holder").unwrap();
        let error = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO)
            .err()
            .unwrap();
        assert!(error.is::<Contended>());
        let text = error.to_string();
        assert!(text.contains("unvalidated"), "{text}");
        assert!(text.contains("private-contract-holder"), "{text}");
        assert!(text.contains(&std::process::id().to_string()), "{text}");
        lease.validate().unwrap();
    }

    #[test]
    fn replacement_is_sticky_loss_and_symlink_is_never_followed() {
        let s = Scratch::new();
        let lease = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        let path = s.0.join("r-orch-gate.lease");
        fs::rename(&path, s.0.join("saved")).unwrap();
        symlink(s.0.join("saved"), &path).unwrap();
        assert!(lease.validate().is_err());
        assert!(Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(s.0.join("saved"), path).unwrap();
        assert!(lease.validate().is_err());
    }

    #[test]
    fn wrong_owner_special_leaf_and_nonprivate_file_refuse() {
        let s = Scratch::new();
        assert!(
            Owned::acquire(
                &s.0,
                s.owner().wrapping_add(1),
                Mode::Exclusive,
                Duration::ZERO
            )
            .is_err()
        );
        let path = s.0.join("r-orch-gate.turnstile");
        fs::write(&path, b"canary").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"canary");
    }

    #[test]
    fn inherited_live_capability_is_rechecked_after_server_loss() {
        let s = Scratch::new();
        let lease = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        let socket_scratch = Scratch::temporary();
        let server = lease.serve(&socket_scratch.0).unwrap();
        let valid = Inherited {
            socket: server.directory.join("s"),
            token: server.token,
            lost: AtomicBool::new(false),
            lease_path: lease.held.path.clone(),
            owner: s.owner(),
        };
        valid.validate().unwrap();
        let wrong = Inherited {
            socket: server.directory.join("s"),
            token: [0; 32],
            lost: AtomicBool::new(false),
            lease_path: lease.held.path.clone(),
            owner: s.owner(),
        };
        assert!(wrong.validate().is_err());
        let mut command = fixture(&s, "inherited", "inherited");
        server.attach(&mut command);
        let mut child = ChildGuard(command.spawn().unwrap());
        ready(&mut child.0, &s.0.join("inherited.ready"));
        drop(server);
        assert!(valid.validate().is_err());
        fs::write(s.0.join("inherited.release"), b"check again").unwrap();
        assert!(child.0.wait().unwrap().success());
        drop(lease);
        assert!(valid.validate().is_err());
    }

    #[test]
    fn shared_lease_coexists_and_excludes_exclusive() {
        let s = Scratch::new();
        let _one = Owned::acquire(&s.0, s.owner(), Mode::Shared, Duration::ZERO).unwrap();
        let _two = Owned::acquire(&s.0, s.owner(), Mode::Shared, Duration::ZERO).unwrap();
        assert!(
            Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO)
                .unwrap_err_is_contention()
        );
    }

    #[test]
    fn inherited_actual_holder_death_is_not_replaced_by_live_client_or_stale_record() {
        let s = Scratch::new();
        let sockets = Scratch::temporary();
        let mut command = fixture(&s, "holder", "server-holder");
        command.env("F2_SOCKET_ROOT", &sockets.0);
        let mut holder = ChildGuard(command.spawn().unwrap());
        ready(&mut holder.0, &s.0.join("holder.ready"));
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(s.0.join("r-orch-gate.lease")).unwrap()).unwrap();
        let mut client = fixture(&s, "client", "inherited");
        client
            .env(SOCKET_ENV, record["socket"].as_str().unwrap())
            .env(TOKEN_ENV, record["token"].as_str().unwrap());
        let mut client = ChildGuard(client.spawn().unwrap());
        ready(&mut client.0, &s.0.join("client.ready"));
        holder.0.kill().unwrap();
        assert!(!holder.0.wait().unwrap().success());
        let _next = Owned::acquire(&s.0, s.owner(), Mode::Exclusive, Duration::ZERO).unwrap();
        assert!(client.0.try_wait().unwrap().is_none());
        fs::write(s.0.join("client.release"), b"check dead holder").unwrap();
        assert!(client.0.wait().unwrap().success());
    }
}
