//! Operation handlers: one method per operation in the D7 catalogue, generated
//! from [`orch_proto::for_each_op!`] so the trait cannot drift from the wire.
//!
//! The implementation (S1b, senior-eng-2) runs each handler body as one
//! [`ProjectCoordinator::run_op`](crate::coordinator::ProjectCoordinator::run_op)
//! and enforces the session's mode against [`OpClass`] before admission. The
//! PR 1 skeleton's default bodies answer `unimplemented`, so an unwritten
//! handler is a visible error, never a silent success.

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
