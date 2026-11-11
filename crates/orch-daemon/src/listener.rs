//! The Unix socket front end (design D5, spec R2/R20/R36): accept, read the
//! kernel peer credentials, and hand the stream to [`Server::admit`].
//!
//! `UnixStream::peer_cred` is `SO_PEERCRED` on Linux and `getpeereid` plus
//! `LOCAL_PEERPID` on macOS. The uid is mandatory: when the kernel cannot
//! report it the connection is closed without a byte. The pid is attribution
//! only and may be unavailable.

use crate::session::{Peer, Server};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

/// Accept until the server drains.
pub(crate) async fn accept_loop(listener: UnixListener, server: Arc<Server>) {
    let mut drain = server.drain_signal();
    loop {
        tokio::select! {
            biased;
            () = crate::session::until_set(&mut drain) => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let peer = peer_of(&stream);
                    server.admit(stream, peer);
                }
                Err(error) => {
                    // EMFILE and friends: back off rather than spin.
                    tracing::debug!(kind = ?error.kind(), "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

pub(crate) fn peer_of(stream: &UnixStream) -> Peer {
    match stream.peer_cred() {
        Ok(cred) => Peer {
            uid: Some(cred.uid()),
            pid: cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        },
        Err(_) => Peer {
            uid: None,
            pid: None,
        },
    }
}
