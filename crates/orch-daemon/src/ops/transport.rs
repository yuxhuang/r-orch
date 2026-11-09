//! Connection-local transport controls and daemon-side mode enforcement.
use super::{OpContext, ServiceHandler, invalid};
use crate::coordinator::OpResult;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::frame::limits;
use orch_proto::handshake::{CallerKind, SessionMode};
use orch_proto::ops::{OpClass, Request, args::*, results::*};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

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

#[derive(Default)]
pub(super) struct Transfers {
    entries: HashMap<(u64, String), Transfer>,
    reserved: usize,
    next: u64,
}

impl Transfers {
    pub(super) fn remove_session(&mut self, session: u64) {
        self.entries.retain(|(id, _), _| *id != session);
        self.reserved = self.entries.values().map(|t| t.total).sum();
    }
    pub(super) fn expire(&mut self) {
        self.entries
            .retain(|_, t| t.touched.elapsed() < limits::TRANSFER_INACTIVITY);
        self.reserved = self.entries.values().map(|t| t.total).sum();
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
        let dir = self.engine.paths.root().join("run/transfers");
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(invalid("cannot inspect stale transfer directory")),
        };
        for entry in entries {
            let entry = entry.map_err(|_| invalid("cannot inspect stale transfer"))?;
            if entry.file_name().to_string_lossy().ends_with(".part") {
                std::fs::remove_file(entry.path())
                    .map_err(|_| invalid("cannot remove stale transfer"))?;
            }
        }
        Ok(())
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
        if transfers.entries.len() >= limits::MAX_QUEUED_OPERATIONS
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
            std::fs::create_dir_all(&path)
                .map_err(|_| invalid("cannot create transfer directory"))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| invalid("cannot protect transfer directory"))?;
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
        _: SubscribeArgs,
    ) -> OpResult<SubscribeAck> {
        let mut subscriptions = self.subscriptions.lock().unwrap_or_else(|e| e.into_inner());
        let entries = subscriptions.entry(ctx.session_id).or_default();
        if entries.len() >= limits::MAX_SUBSCRIPTIONS_PER_SESSION {
            return Err(ErrorBody::new(
                ErrorCode::Overloaded,
                "subscription limit reached",
                Execution::NotExecuted,
            ));
        }
        let id = self
            .control
            .next_subscription
            .fetch_add(1, Ordering::Relaxed);
        entries.push(id);
        Ok(SubscribeAck {
            subscription: id,
            instance: self.control.instance(),
            seq: self.coordinator.current_commit_seq(),
        })
    }
    pub(super) async fn unsubscribe(&self, ctx: &OpContext, a: UnsubscribeArgs) -> OpResult<()> {
        let mut subscriptions = self.subscriptions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entries) = subscriptions.get_mut(&ctx.session_id) {
            entries.retain(|id| *id != a.subscription);
        }
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
