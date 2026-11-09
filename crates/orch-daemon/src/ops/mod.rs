//! Operation handlers: one method per operation in the D7 catalogue, generated
//! from [`orch_proto::for_each_op!`] so the trait cannot drift from the wire.
//!
//! The concrete ServiceHandler runs each state handler body as one
//! [`ProjectCoordinator::run_op`](crate::coordinator::ProjectCoordinator::run_op)
//! and enforces the session's mode against [`OpClass`] before admission. The
//! concrete implementation is generated exhaustively from the catalogue;
//! phased execution releases the state permit for external work.

mod authored;
mod control;
mod core;
mod peripheral;
mod transport;

use crate::coordinator::{Committed, Lane, ProjectCoordinator};
use orch_engine::container::FullEngine;
use orch_proto::frame::{Part, RequestId, Store};
use orch_proto::ops::Op;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::coordinator::OpResult;
use orch_proto::error::ErrorBody;
use orch_proto::handshake::Session;
use orch_proto::ops::{OpClass, Request};
use serde_json::value::RawValue;

/// Per-request context: the session's fixed identity and mode, and the
/// request's class. Identity is attribution, not authentication.
#[derive(Debug, Clone)]
pub struct OpContext {
    pub session: Session,
    pub class: OpClass,
    /// The kernel peer pid, when the platform reports one.
    pub peer_pid: Option<u32>,
    /// One id per authenticated connection, shared by every request on it.
    pub session_id: u64,
    /// The terminal observer owns no agent lifetime.
    pub request_id: Option<RequestId>,
    pub parts: Option<tokio::sync::mpsc::Sender<Part>>,
    pub cancelled: Arc<AtomicBool>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl OpContext {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            class: OpClass::Read,
            peer_pid: None,
            session_id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            request_id: None,
            parts: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn for_request(
        &self,
        request_id: RequestId,
        class: OpClass,
        parts: Option<tokio::sync::mpsc::Sender<Part>>,
    ) -> Self {
        Self {
            request_id: Some(request_id),
            class,
            parts,
            cancelled: Arc::new(AtomicBool::new(false)),
            ..self.clone()
        }
    }

    pub fn cancel_observation(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

type Observations = HashMap<(u64, RequestId), Arc<AtomicBool>>;

/// The live project's operation adapter. All clones keep one engine, run
/// registry, admission boundary, dispatch mode and connection-local transfers.
#[derive(Clone)]
pub struct ServiceHandler {
    pub(crate) engine: Arc<FullEngine>,
    pub(crate) coordinator: Arc<ProjectCoordinator>,
    control: Arc<control::ControlState>,
    transfers: Arc<Mutex<transport::Transfers>>,
    pub(crate) subscriptions: Arc<Mutex<HashMap<u64, Vec<u64>>>>,
    observations: Arc<Mutex<Observations>>,
}

impl ServiceHandler {
    pub fn new(engine: Arc<FullEngine>, coordinator: Arc<ProjectCoordinator>) -> Self {
        engine.orchestrator.enable_daemon_mode();
        Self {
            engine: Arc::clone(&engine),
            coordinator,
            control: Arc::new(control::ControlState::new(&engine)),
            transfers: Arc::new(Mutex::new(transport::Transfers::default())),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            observations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn coordinator(&self) -> &Arc<ProjectCoordinator> {
        &self.coordinator
    }

    /// Session teardown discards transfers/observers; accepted state work is
    /// owned by the dispatcher task and is allowed to finish.
    pub fn disconnect(&self, session_id: u64) {
        self.transfers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove_session(session_id);
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
        self.observations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(sid, _), cancelled| {
                if *sid == session_id {
                    cancelled.store(true, Ordering::SeqCst);
                    false
                } else {
                    true
                }
            });
    }
}

fn service_error(error: orch_core::error::Error) -> ErrorBody {
    use orch_proto::error::{ErrorCode, Execution};
    match error {
        orch_core::error::Error::Domain(error) => {
            ErrorBody::from_domain(&error, Execution::Unknown)
        }
        error => ErrorBody::new(ErrorCode::General, &error.to_string(), Execution::Unknown),
    }
}

pub(super) fn invalid(message: &str) -> ErrorBody {
    ErrorBody::new(
        orch_proto::error::ErrorCode::InvalidArguments,
        message,
        orch_proto::error::Execution::NotExecuted,
    )
}

fn external_operation(name: &str) -> bool {
    matches!(
        name,
        "task.cancel"
            | "agent.stop"
            | "daemon.stop"
            | "doctor.run"
            | "models.catalog"
            | "dispatch.tick"
            | "dispatch.run_task"
            | "dispatch.run_all"
            | "dispatch.until_terminal"
            | "logs.follow"
    )
}

fn invalidation_store(name: &str) -> Store {
    match name.split('.').next().unwrap_or("") {
        "task" | "attachment" => Store::Tasks,
        "agent" => Store::Agents,
        "goal" => Store::Goals,
        "message" => Store::Messages,
        "context" | "maintenance" => Store::Context,
        "team" => Store::Teams,
        "remote" => Store::Remote,
        "state" | "dispatch" => Store::State,
        _ => Store::Config,
    }
}

macro_rules! define_handler {
    ($( ($variant:ident, $method:ident, $name:literal, $args:ty, $out:ty, $class:ident) ),* $(,)?) => {
        /// Typed handler for every operation. Override each method; the
        /// default answers [`ErrorBody::unimplemented`] (PR 1 skeleton, owner
        /// S1b).
        #[async_trait::async_trait]
        pub trait Handler: Send + Sync {
            $(
                #[doc = concat!("`", $name, "`")]
                async fn $method(&self, ctx: &OpContext, args: $args) -> OpResult<$out> {
                    let _ = (ctx, args);
                    Err(ErrorBody::unimplemented($name))
                }
            )*
        }

        /// Route one decoded request to its typed handler method and encode
        /// the result as the `ok` payload.
        pub async fn dispatch<H: Handler + ?Sized>(
            handler: &H,
            ctx: &OpContext,
            request: Request,
        ) -> OpResult<Box<RawValue>> {
            match request {
                $( Request::$variant(args) => encode(handler.$method(ctx, args).await?), )*
            }
        }
    };
}

orch_proto::for_each_op!(define_handler);

macro_rules! implement_handler {
    ($( ($variant:ident, $method:ident, $name:literal, $args:ty, $out:ty, $class:ident) ),* $(,)?) => {
        impl ServiceHandler {
            /// Accepted operations are daemon-owned tasks. Dropping the socket
            /// observer detaches this JoinHandle; it cannot abort a partial write.
            pub async fn execute_request(&self, ctx: &OpContext, request: Request) -> OpResult<Committed<Box<RawValue>>> {
                transport::authorize(ctx, &request)?;
                let handler = self.clone();
                let context = ctx.clone();
                tokio::spawn(async move { handler.execute_inner(context, request).await }).await
                    .map_err(|_| ErrorBody::new(orch_proto::error::ErrorCode::General,
                        "daemon operation task failed", orch_proto::error::Execution::Unknown))?
            }

            async fn execute_inner(&self, ctx: OpContext, request: Request) -> OpResult<Committed<Box<RawValue>>> {
                let _observation = ObservationGuard::new(self, &ctx);
                if self.ownership_lost() && matches!(request,Request::DispatchTick(_) | Request::DispatchRunTask(_) | Request::DispatchRunAll(_) | Request::DispatchUntilTerminal(_)) {
                    return Err(ErrorBody::new(orch_proto::error::ErrorCode::Conflict,"daemon ownership was lost",orch_proto::error::Execution::NotExecuted));
                }
                let name = request.name();
                if external_operation(name) {
                    let bytes = serde_json::to_vec(&request).map_err(|_| invalid("invalid request"))?.len();
                    let lane = if matches!(name, "task.cancel" | "agent.stop" | "daemon.stop") { Lane::Control } else { Lane::Ordinary };
                    let _admission = self.coordinator.admit(lane, bytes)?;
                    let _probe = if matches!(name, "doctor.run" | "models.catalog") {
                        Some(self.control.probe_permit().await?)
                    } else { None };
                    let boundary = self.engine.events.operation_boundary();
                    let result = boundary.track(async {
                        match request {
                            $( Request::$variant(args) => {
                                let output = if matches!(name,"doctor.run" | "models.catalog") {
                                    tokio::time::timeout(std::time::Duration::from_secs(120), ServiceHandler::$method(self,&ctx,args)).await
                                        .map_err(|_| ErrorBody::new(orch_proto::error::ErrorCode::General,"diagnostic deadline exceeded",orch_proto::error::Execution::Unknown))??
                                } else { ServiceHandler::$method(self,&ctx,args).await? };
                                encode(output)
                            }, )*
                        }
                    }).await;
                    return result.value.map(|value| Committed { value, commit_seq: result.commit_seq });
                }
                let bytes = serde_json::to_vec(&request).map_err(|_| invalid("invalid request"))?.len();
                let lane = if matches!(name, "dispatch.set" | "request.cancel") { Lane::Control } else { Lane::Ordinary };
                let class = request.class();
                let handler = self.clone();
                self.coordinator.run_op_sized(lane, bytes, move |permit| Box::pin(async move {
                    if ctx.is_cancelled() { return Err(ErrorBody::new(orch_proto::error::ErrorCode::Conflict,
                        "request cancelled before execution", orch_proto::error::Execution::NotExecuted)); }
                    if matches!(class, OpClass::Mutate | OpClass::Destructive) || name == "dispatch.set" {
                        permit.invalidate(invalidation_store(name));
                    }
                    match request {
                        $( Request::$variant(args) => encode(ServiceHandler::$method(&handler, &ctx, args).await?), )*
                    }
                })).await
            }
        }
        #[async_trait::async_trait]
        impl Handler for ServiceHandler {
            $( async fn $method(&self, ctx: &OpContext, args: $args) -> OpResult<$out> {
                let committed = self.execute_request(ctx, args.into_request()).await?;
                serde_json::from_str(committed.value.get()).map_err(|_| invalid("handler result has invalid shape"))
            } )*
        }
    };
}
orch_proto::for_each_op!(implement_handler);

struct ObservationGuard {
    handler: ServiceHandler,
    key: Option<(u64, RequestId)>,
}
impl ObservationGuard {
    fn new(handler: &ServiceHandler, ctx: &OpContext) -> Self {
        let key = ctx.request_id.map(|id| (ctx.session_id, id));
        if let Some(key) = key {
            handler
                .observations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, Arc::clone(&ctx.cancelled));
        }
        Self {
            handler: handler.clone(),
            key,
        }
    }
}
impl Drop for ObservationGuard {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            self.handler
                .observations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
    }
}

fn encode<T: serde::Serialize>(value: T) -> OpResult<Box<RawValue>> {
    serde_json::value::to_raw_value(&value).map_err(|_| {
        ErrorBody::new(
            orch_proto::error::ErrorCode::General,
            "result could not be encoded",
            orch_proto::error::Execution::Unknown,
        )
    })
}

/// A handler with every default: answers `unimplemented` for every operation.
/// Lets S1a-impl wire sessions before S1b lands.
pub struct UnimplementedHandler;

impl Handler for UnimplementedHandler {}
