//! The typed operation surface every frontend uses (design D7).
//!
//! One method per catalogue entry, generated from [`orch_proto::for_each_op!`]
//! so an operation cannot be added to the protocol and missed here. The
//! exceptions are the operations whose wire contract is more than one
//! request and one reply; each has a dedicated method instead:
//!
//! | Operation | Use |
//! |---|---|
//! | `logs.follow` | [`Client::follow_logs`] (runs until cancelled) |
//! | `transfer.begin/chunk/finish` | [`Client::upload`] |
//! | `request.cancel` | [`Call::cancel`] |
//! | `daemon.status`, `dispatch.set`, `daemon.stop` | [`Client::status`], [`Client::dispatch`], [`Client::daemon_stop`] (bounded control deadline) |
//! | `subscribe`, `unsubscribe` | not exposed before the push API (PR 3) |
use crate::call::{Call, hex_sha256};
use crate::{Client, ClientError};
use base64::Engine as _;
use orch_proto::bounded::{BoundedString, Id};
use orch_proto::frame::limits;
use orch_proto::ops::args::{self, TransferPurpose, TransferToken};
use orch_proto::ops::{Op, Request};
use serde::de::DeserializeOwned;

/// Operations that are not one request and one terminal reply, with the
/// method that carries each. `request`/`start` refuse them before any byte.
const MULTI_FRAME: &[(&str, &str)] = &[
    ("logs.follow", "Client::follow_logs"),
    ("transfer.begin", "Client::upload"),
    ("transfer.chunk", "Client::upload"),
    ("transfer.finish", "Client::upload"),
    ("request.cancel", "Call::cancel"),
    ("subscribe", "the push API (PR 3)"),
    ("unsubscribe", "the push API (PR 3)"),
];

fn refuse_multi_frame(name: &str, allow_follow: bool) -> Result<(), ClientError> {
    match MULTI_FRAME.iter().find(|(op, _)| *op == name) {
        Some(("logs.follow", _)) if allow_follow => Ok(()),
        Some((op, method)) => Err(ClientError::InvalidArguments(format!(
            "{op} is not a single-reply operation; use {method}"
        ))),
        None => Ok(()),
    }
}

impl Client {
    /// Send one typed operation and wait for its result. Events a dispatch
    /// operation streams meanwhile are discarded; use [`Client::start`] to
    /// render them.
    pub async fn request<A: Op>(&mut self, args: A) -> Result<A::Output, ClientError> {
        refuse_multi_frame(A::NAME, false)?;
        self.begin(args.into_request()).await?.finish().await
    }

    /// Send one typed operation and return its in-flight handle, for the
    /// dispatch operations whose events a frontend renders (`orch run <id>`,
    /// `run --once`, `serve --once`) and for `logs.follow`.
    pub async fn start<A: Op>(&mut self, args: A) -> Result<Call<'_, A::Output>, ClientError> {
        refuse_multi_frame(A::NAME, true)?;
        self.begin(args.into_request()).await
    }

    /// `logs.follow`: orchestrator events until [`Call::cancel`] or until the
    /// client is dropped (disconnect ends the stream daemon-side).
    pub async fn follow_logs(
        &mut self,
        args: args::LogsFollowArgs,
    ) -> Result<Call<'_, ()>, ClientError> {
        self.start(args).await
    }

    /// Untyped escape hatch kept for harnesses that build a [`Request`]
    /// directly. Same single-reply rule as [`Client::request`].
    pub async fn call<T: DeserializeOwned>(&mut self, req: Request) -> Result<T, ClientError> {
        refuse_multi_frame(req.name(), false)?;
        self.begin(req).await?.finish().await
    }

    /// Upload `bytes` as a sealed transfer for one later consuming operation
    /// on this same session (e.g. `input_transfer` of a definition apply or
    /// `attachment.stage_image`). A failed upload is discarded daemon-side.
    pub async fn upload(
        &mut self,
        purpose: TransferPurpose,
        bytes: &[u8],
    ) -> Result<TransferToken, ClientError> {
        if bytes.is_empty() || bytes.len() > limits::MAX_TRANSFER_BYTES {
            return Err(ClientError::InvalidArguments(format!(
                "transfer must be 1 to {} bytes",
                limits::MAX_TRANSFER_BYTES
            )));
        }
        let handle = self
            .begin::<orch_proto::ops::results::TransferHandle>(Request::TransferBegin(
                args::TransferBeginArgs {
                    purpose,
                    total_bytes: bytes.len() as u64,
                },
            ))
            .await?
            .finish()
            .await?;
        for (n, chunk) in (0u64..).zip(bytes.chunks(limits::MAX_CHUNK_BYTES)) {
            let data = base64::engine::general_purpose::STANDARD.encode(chunk);
            let data_base64 = BoundedString::new(&data).map_err(|_| {
                ClientError::InvalidArguments("transfer chunk exceeds its bound".into())
            })?;
            self.begin::<()>(Request::TransferChunk(args::TransferChunkArgs {
                token: handle.token.clone(),
                n,
                data_base64,
            }))
            .await?
            .finish()
            .await?;
        }
        let sha256 = Id::new(hex_sha256(bytes))
            .map_err(|_| ClientError::InvalidArguments("transfer digest is invalid".into()))?;
        let sealed = self
            .begin::<orch_proto::ops::results::TransferHandle>(Request::TransferFinish(
                args::TransferFinishArgs {
                    token: handle.token,
                    sha256,
                },
            ))
            .await?
            .finish()
            .await?;
        if !sealed.sealed {
            return Err(self.unknown());
        }
        Ok(sealed.token)
    }
}

/// One method per remaining catalogue entry; the skipped arms are the table
/// in the module docs.
macro_rules! typed_method {
    (LogsFollow, $($rest:tt)*) => {};
    (TransferBegin, $($rest:tt)*) => {};
    (TransferChunk, $($rest:tt)*) => {};
    (TransferFinish, $($rest:tt)*) => {};
    (RequestCancel, $($rest:tt)*) => {};
    (Subscribe, $($rest:tt)*) => {};
    (Unsubscribe, $($rest:tt)*) => {};
    (DaemonStatus, $($rest:tt)*) => {};
    (DispatchSet, $($rest:tt)*) => {};
    (DaemonStop, $($rest:tt)*) => {};
    ($variant:ident, $method:ident, $name:literal, $args:ty, $out:ty, $class:ident) => {
        #[doc = concat!("`", $name, "` (", stringify!($class), ").")]
        pub async fn $method(&mut self, args: $args) -> Result<$out, ClientError> {
            self.request(args).await
        }
    };
}

macro_rules! typed_methods {
    ($( ($variant:ident, $method:ident, $name:literal, $args:ty, $out:ty, $class:ident) ),* $(,)?) => {
        impl Client {
            $( typed_method!($variant, $method, $name, $args, $out, $class); )*
        }
    };
}

orch_proto::for_each_op!(typed_methods);

#[cfg(test)]
mod tests {
    use super::*;
    use orch_proto::ops::CATALOGUE;

    #[test]
    fn every_multi_frame_entry_is_a_catalogue_operation() {
        for (op, _) in MULTI_FRAME {
            assert!(CATALOGUE.iter().any(|(name, _)| name == op), "{op}");
        }
    }

    #[test]
    fn single_reply_entry_points_refuse_multi_frame_operations() {
        for (op, method) in MULTI_FRAME {
            let error = refuse_multi_frame(op, false).unwrap_err();
            assert_eq!(error.exit_code(), 2);
            assert!(error.to_string().contains(method), "{error}");
        }
        assert!(refuse_multi_frame("logs.follow", true).is_ok());
        assert!(refuse_multi_frame("transfer.begin", true).is_err());
        assert!(refuse_multi_frame("task.get", false).is_ok());
    }
}
