//! Connection-local transport controls and daemon-side mode enforcement.
use super::{OpContext, ServiceHandler, invalid};
use crate::coordinator::OpResult;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::Mode;
use nix::unistd::{UnlinkatFlags, unlinkat};
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::frame::limits;
use orch_proto::handshake::{CallerKind, SessionMode};
use orch_proto::ops::{OpClass, Request, args::*, results::*};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

fn validate_private_directory(file: &File) -> OpResult<()> {
    let metadata = file
        .metadata()
        .map_err(|_| invalid("cannot inspect staging directory"))?;
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(invalid(
            "staging directory must be owned by this uid and private",
        ));
    }
    Ok(())
}

fn private_directory(path: &Path) -> OpResult<()> {
    if let Err(error) = DirBuilder::new().mode(0o700).create(path)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(invalid("cannot create staging directory"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| invalid("staging directory must be a real directory"))?;
    // Never repair permissions on an unverified pre-existing directory.
    validate_private_directory(&file)
}

fn private_directory_at(parent: &File, name: &str) -> OpResult<Option<File>> {
    let fd = match openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::ENOENT) => return Ok(None),
        Err(_) => return Err(invalid("cannot open private staging directory")),
    };
    let file = File::from(fd);
    validate_private_directory(&file)?;
    Ok(Some(file))
}

fn cleanup_stale_directory(file: File, extension: &str) -> OpResult<()> {
    let fd = file
        .try_clone()
        .map_err(|_| invalid("cannot inspect stale transfer directory"))?;
    let mut directory =
        Dir::from_fd(fd.into()).map_err(|_| invalid("cannot inspect stale transfer directory"))?;
    for entry in directory.iter() {
        let entry = entry.map_err(|_| invalid("cannot inspect stale transfer"))?;
        let name = entry.file_name();
        if Path::new(OsStr::from_bytes(name.to_bytes()))
            .extension()
            .is_some_and(|ext| ext == extension)
        {
            // Names come from the pinned directory; unlinkat never follows a
            // final symlink, and a renamed ancestor cannot redirect removal.
            unlinkat(&file, name, UnlinkatFlags::NoRemoveDir)
                .map_err(|_| invalid("cannot remove stale transfer"))?;
        }
    }
    Ok(())
}

pub(super) fn authorize(ctx: &OpContext, request: &Request) -> OpResult<()> {
    let refuse = || {
        ErrorBody::new(
            ErrorCode::NotPermitted,
            "operation is not permitted for this session",
            Execution::NotExecuted,
        )
    };
    let name = request.name();
    let class = request.class(); // Never trust ctx.class from a raw client.
    if ctx.session.mode == SessionMode::ReadOnly
        && class != OpClass::Read
        && !matches!(name, "subscribe" | "unsubscribe" | "request.cancel")
    {
        return Err(refuse());
    }
    if ctx.session.caller.kind != CallerKind::Mcp {
        return Ok(());
    }
    // Exactly the existing MCP tool/resource surface and required transport
    // helpers. Owning the socket is not permission to widen an MCP mode.
    let exposed = matches!(
        name,
        "status.get"
            | "task.create"
            | "task.list"
            | "task.get"
            | "task.assign"
            | "task.cancel"
            | "task.approve"
            | "task.reject"
            | "task.retry"
            | "task.definition.get"
            | "task.definition.apply"
            | "agent.create"
            | "agent.list"
            | "agent.get"
            | "agent.remove"
            | "agent.enable"
            | "agent.disable"
            | "agent.autonomous"
            | "agent.definition.get"
            | "agent.definition.apply"
            | "goal.create"
            | "goal.list"
            | "goal.get"
            | "goal.update"
            | "goal.status"
            | "goal.delete"
            | "message.send"
            | "message.broadcast"
            | "message.inbox"
            | "message.list"
            | "context.set"
            | "context.get"
            | "context.list"
            | "context.delete"
            | "team.create"
            | "team.list"
            | "team.get"
            | "team.join"
            | "team.leave"
            | "team.add_task"
            | "team.set_lead"
            | "team.disband"
            | "config.get"
            | "config.set"
            | "config.definition.get"
            | "config.definition.apply"
            | "run.list"
            | "run.get"
            | "run.events"
            | "run.events_tail"
            | "remote.list"
            | "remote.status"
            | "doctor.run"
            | "subscribe"
            | "unsubscribe"
            | "request.cancel"
            | "transfer.begin"
            | "transfer.chunk"
            | "transfer.finish"
    );
    if !exposed {
        return Err(refuse());
    }
    if class == OpClass::Destructive && ctx.session.mode != SessionMode::AllowDestructive {
        return Err(refuse());
    }
    if matches!(request,Request::GoalStatus(a) if a.force)
        && ctx.session.mode != SessionMode::AllowDestructive
    {
        return Err(refuse());
    }
    if matches!(request,Request::TransferBegin(a) if a.purpose==TransferPurpose::ClipboardImage) {
        return Err(refuse());
    }
    Ok(())
}

struct Transfer {
    purpose: TransferPurpose,
    total: usize,
    path: PathBuf,
    len: usize,
    next: u64,
    sealed: bool,
    touched: Instant,
}

impl Transfer {
    fn read(&self) -> OpResult<Vec<u8>> {
        let data = std::fs::read(&self.path).map_err(|_| invalid("cannot read staged transfer"))?;
        if data.len() != self.len || data.len() > self.total {
            return Err(invalid("staged transfer length changed"));
        }
        Ok(data)
    }
}
impl Drop for Transfer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct StagedImage {
    path: PathBuf,
    bytes: usize,
}
impl Drop for StagedImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Default)]
pub(super) struct Transfers {
    entries: HashMap<(u64, String), Transfer>,
    images: HashMap<(u64, PathBuf), StagedImage>,
    reserved: usize,
    next: u64,
}

impl Transfers {
    pub(super) fn remove_session(&mut self, session: u64) {
        self.entries.retain(|(id, _), _| *id != session);
        self.images.retain(|(id, _), _| *id != session);
        self.recount();
    }
    pub(super) fn expire(&mut self) {
        self.entries
            .retain(|_, t| t.touched.elapsed() < limits::TRANSFER_INACTIVITY);
        self.recount();
    }
    fn recount(&mut self) {
        self.reserved = self.entries.values().map(|t| t.total).sum::<usize>()
            + self.images.values().map(|image| image.bytes).sum::<usize>();
    }
    fn remove(&mut self, key: &(u64, String)) -> Option<Transfer> {
        let transfer = self.entries.remove(key)?;
        self.reserved -= transfer.total;
        Some(transfer)
    }
}

impl ServiceHandler {
    /// Call once after acquiring project ownership, before admitting sessions.
    /// Only this daemon's private transfer files are startup scratch space.
    pub fn cleanup_stale_transfers(&self) -> OpResult<()> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(self.engine.paths.root())
            .map_err(|_| invalid("cannot inspect staging root"))?;
        let Some(run) = private_directory_at(&root, "run")? else {
            return Ok(());
        };
        for (name, extension) in [("transfers", "part"), ("staged", "png")] {
            if let Some(directory) = private_directory_at(&run, name)? {
                cleanup_stale_directory(directory, extension)?;
            }
        }
        Ok(())
    }

    pub(super) fn finish_staged_paths(&self, paths: &[PathBuf]) {
        let mut transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        transfers
            .images
            .retain(|(_, path), _| !paths.contains(path));
        transfers.recount();
    }

    pub(super) async fn stage_image_transfer(
        &self,
        ctx: &OpContext,
        token: &TransferToken,
    ) -> OpResult<StagedAttachment> {
        let mut transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        transfers.expire();
        let key = (ctx.session_id, token.0.to_string());
        let transfer = transfers
            .entries
            .get(&key)
            .ok_or_else(|| invalid("transfer does not exist or expired"))?;
        if !transfer.sealed || transfer.purpose != TransferPurpose::ClipboardImage {
            return Err(invalid("sealed clipboard-image transfer required"));
        }
        let bytes = transfer.read()?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(invalid("image: expected PNG bytes"));
        }
        let directory = self.engine.paths.root().join("run/staged");
        private_directory(&directory)?;
        let path = directory.join(format!("{}.png", self.engine.ids.generate("img")));
        let wire = orch_proto::path::WirePath::new(path.clone())
            .map_err(|_| invalid("staged image path exceeds protocol limit"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| invalid("cannot stage image"))?;
        if file.write_all(&bytes).is_err() {
            let _ = std::fs::remove_file(&path);
            return Err(invalid("cannot write staged image"));
        }
        let transfer = transfers.entries.remove(&key).expect("transfer checked");
        // Ownership changes from sealed upload to attachment source. Its byte
        // reservation survives until copying into a task or session cleanup.
        transfers.images.insert(
            (ctx.session_id, path.clone()),
            StagedImage {
                path,
                bytes: transfer.total,
            },
        );
        Ok(StagedAttachment {
            path: wire,
            bytes: transfer.total as u64,
        })
    }

    pub(super) async fn take_transfer(
        &self,
        ctx: &OpContext,
        token: &TransferToken,
    ) -> OpResult<Vec<u8>> {
        self.take_transfer_for(ctx, token, TransferPurpose::Document)
            .await
    }
    pub(super) async fn take_transfer_for(
        &self,
        ctx: &OpContext,
        token: &TransferToken,
        purpose: TransferPurpose,
    ) -> OpResult<Vec<u8>> {
        let mut transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        transfers.expire();
        let key = (ctx.session_id, token.0.to_string());
        let transfer = transfers
            .entries
            .get(&key)
            .ok_or_else(|| invalid("transfer does not exist or expired"))?;
        if !transfer.sealed {
            return Err(invalid("transfer is not sealed"));
        }
        if transfer.purpose != purpose {
            return Err(invalid(
                "transfer purpose does not match consuming operation",
            ));
        }
        transfers.remove(&key).expect("transfer checked").read()
    }
    pub(super) async fn transfer_begin(
        &self,
        ctx: &OpContext,
        a: TransferBeginArgs,
    ) -> OpResult<TransferHandle> {
        let total =
            usize::try_from(a.total_bytes).map_err(|_| invalid("transfer exceeds byte limit"))?;
        if total == 0 || total > limits::MAX_TRANSFER_BYTES {
            return Err(invalid("transfer exceeds byte limit"));
        }
        let mut transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        transfers.expire();
        if transfers.entries.len() + transfers.images.len() >= limits::MAX_QUEUED_OPERATIONS
            || transfers
                .entries
                .keys()
                .filter(|(id, _)| *id == ctx.session_id)
                .count()
                >= 1
        {
            return Err(ErrorBody::new(
                ErrorCode::Overloaded,
                "transfer count limit reached",
                Execution::NotExecuted,
            ));
        }
        if transfers.reserved.saturating_add(total) > limits::MAX_STAGED_BYTES {
            return Err(ErrorBody::new(
                ErrorCode::Overloaded,
                "staged transfer budget is full",
                Execution::NotExecuted,
            ));
        }
        transfers.next += 1;
        let token = TransferToken(
            format!("transfer_{}_{}", ctx.session_id, transfers.next)
                .try_into()
                .map_err(|_| invalid("transfer token invalid"))?,
        );
        let dir = self.engine.paths.root().join("run/transfers");
        for path in [self.engine.paths.root().join("run"), dir.clone()] {
            private_directory(&path)?;
        }
        let path = dir.join(format!("{}-{}.part", std::process::id(), &*token.0));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| invalid("cannot create staged transfer"))?;
        transfers.reserved += total;
        transfers.entries.insert(
            (ctx.session_id, token.0.to_string()),
            Transfer {
                purpose: a.purpose,
                total,
                path,
                len: 0,
                next: 0,
                sealed: false,
                touched: Instant::now(),
            },
        );
        Ok(TransferHandle {
            token,
            sealed: false,
        })
    }
    pub(super) async fn transfer_chunk(
        &self,
        ctx: &OpContext,
        a: TransferChunkArgs,
    ) -> OpResult<()> {
        let bytes = STANDARD
            .decode(a.data_base64.as_bytes())
            .map_err(|_| invalid("transfer chunk has invalid base64"))?;
        if bytes.len() > limits::MAX_CHUNK_BYTES {
            return Err(invalid("transfer chunk exceeds byte limit"));
        }
        let mut transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        transfers.expire();
        let key = (ctx.session_id, a.token.0.to_string());
        let transfer = transfers
            .entries
            .get_mut(&key)
            .ok_or_else(|| invalid("transfer does not exist or expired"))?;
        if transfer.sealed
            || a.n != transfer.next
            || transfer.len.saturating_add(bytes.len()) > transfer.total
        {
            transfers.remove(&key);
            return Err(invalid("invalid transfer chunk sequence or length"));
        }
        let write = std::fs::OpenOptions::new()
            .append(true)
            .open(&transfer.path)
            .and_then(|mut file| file.write_all(&bytes));
        if write.is_err() {
            transfers.remove(&key);
            return Err(invalid("cannot write staged transfer"));
        }
        transfer.len += bytes.len();
        transfer.next += 1;
        transfer.touched = Instant::now();
        Ok(())
    }
    pub(super) async fn transfer_finish(
        &self,
        ctx: &OpContext,
        a: TransferFinishArgs,
    ) -> OpResult<TransferHandle> {
        let mut transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        transfers.expire();
        let key = (ctx.session_id, a.token.0.to_string());
        let transfer = transfers
            .entries
            .get_mut(&key)
            .ok_or_else(|| invalid("transfer does not exist or expired"))?;
        let digest = format!("{:x}", Sha256::digest(transfer.read()?));
        if transfer.sealed || transfer.len != transfer.total || digest != a.sha256.as_ref() {
            transfers.remove(&key);
            return Err(invalid("transfer length or digest mismatch"));
        }
        transfer.sealed = true;
        transfer.touched = Instant::now();
        Ok(TransferHandle {
            token: a.token,
            sealed: true,
        })
    }
    pub(super) async fn subscribe(
        &self,
        ctx: &OpContext,
        args: SubscribeArgs,
    ) -> OpResult<SubscribeAck> {
        let id = self
            .control
            .next_subscription
            .fetch_add(1, Ordering::Relaxed);
        let instance = self.control.instance();
        let wire_instance = orch_proto::bounded::Id::new(&instance)
            .map_err(|_| invalid("invalid daemon instance"))?;
        if !self.hub.register(
            ctx.session_id,
            ctx.event_output.clone(),
            id,
            wire_instance,
            self.coordinator.current_commit_seq(),
            args,
        ) {
            return Err(ErrorBody::new(
                ErrorCode::Overloaded,
                "subscription limit reached or session closed",
                Execution::NotExecuted,
            ));
        }
        Ok(SubscribeAck {
            subscription: id,
            instance,
            seq: 0,
        })
    }
    pub(super) async fn unsubscribe(&self, ctx: &OpContext, args: UnsubscribeArgs) -> OpResult<()> {
        self.hub.remove(ctx.session_id, args.subscription);
        Ok(())
    }
    pub(super) async fn request_cancel(
        &self,
        ctx: &OpContext,
        a: RequestCancelArgs,
    ) -> OpResult<()> {
        if let Some(cancelled) = self
            .observations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(ctx.session_id, a.id))
        {
            cancelled.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_proto::handshake::{Caller, Session};
    fn ctx(mode: SessionMode) -> OpContext {
        OpContext::new(Session {
            caller: Caller {
                kind: CallerKind::Mcp,
                agent_id: None,
                pid: None,
            },
            mode,
        })
    }
    #[test]
    fn raw_client_modes_cannot_bypass_daemon_policy() {
        for mode in [SessionMode::ReadOnly, SessionMode::Normal] {
            let err = authorize(
                &ctx(mode),
                &Request::TaskCancel(TaskCancelArgs {
                    id: "tsk_1".try_into().unwrap(),
                }),
            )
            .unwrap_err();
            assert_eq!(err.execution, Execution::NotExecuted);
        }
        let c = ctx(SessionMode::AllowDestructive);
        assert!(
            authorize(
                &c,
                &Request::TaskCancel(TaskCancelArgs {
                    id: "tsk_1".try_into().unwrap()
                })
            )
            .is_ok()
        );
        assert!(authorize(&c, &Request::DaemonStop(DaemonStopArgs {})).is_err());
        assert!(
            authorize(
                &c,
                &Request::MaintenanceExpireContext(MaintenanceExpireContextArgs {})
            )
            .is_err()
        );
    }
    #[test]
    fn force_requires_allow_destructive_even_for_mutate_class() {
        let r = Request::GoalStatus(GoalStatusArgs {
            id: "goal_1".try_into().unwrap(),
            status: orch_domain::GoalStatus::Achieved,
            force: true,
        });
        assert!(authorize(&ctx(SessionMode::Normal), &r).is_err());
        assert!(authorize(&ctx(SessionMode::AllowDestructive), &r).is_ok());
    }
    #[test]
    fn read_only_has_no_inbound_transfer() {
        let r = Request::TransferBegin(TransferBeginArgs {
            purpose: TransferPurpose::Document,
            total_bytes: 1,
        });
        assert!(authorize(&ctx(SessionMode::ReadOnly), &r).is_err());
    }
}

#[cfg(test)]
mod staging_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn cleanup_keeps_using_checked_descriptors_after_directory_swaps() {
        for swap_run in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let run_path = root.join("run");
            let staging_path = run_path.join("transfers");
            private_directory(&run_path).unwrap();
            private_directory(&staging_path).unwrap();
            std::fs::write(staging_path.join("old.part"), b"stale").unwrap();
            let outside = root.join("outside");
            private_directory(&outside).unwrap();
            private_directory(&outside.join("transfers")).unwrap();
            std::fs::write(outside.join("keep.part"), b"sentinel").unwrap();
            std::fs::write(outside.join("transfers/keep.part"), b"sentinel").unwrap();
            let root_fd = File::open(root).unwrap();
            let run = private_directory_at(&root_fd, "run").unwrap().unwrap();
            if swap_run {
                std::fs::rename(&run_path, root.join("saved-run")).unwrap();
                symlink(&outside, &run_path).unwrap();
            }
            let staging = private_directory_at(&run, "transfers").unwrap().unwrap();
            if !swap_run {
                std::fs::rename(&staging_path, root.join("saved-staging")).unwrap();
                symlink(&outside, &staging_path).unwrap();
            }
            cleanup_stale_directory(staging, "part").unwrap();
            let old = if swap_run {
                root.join("saved-run/transfers/old.part")
            } else {
                root.join("saved-staging/old.part")
            };
            assert!(!old.exists(), "cleanup must use the checked directory");
            assert_eq!(
                std::fs::read(outside.join("keep.part")).unwrap(),
                b"sentinel"
            );
            assert_eq!(
                std::fs::read(outside.join("transfers/keep.part")).unwrap(),
                b"sentinel"
            );
        }
    }

    #[test]
    fn staging_creation_never_chmods_preexisting_directories_or_symlink_targets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("staging");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&path).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        let link = temp.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(private_directory(&link).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        let created = temp.path().join("created");
        private_directory(&created).unwrap();
        assert_eq!(std::fs::metadata(&created).unwrap().mode() & 0o777, 0o700);
    }
}
