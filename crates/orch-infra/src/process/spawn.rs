//! The shared adapter spawn path.
//!
//! Every adapter goes through [`spawn_child`], which is where **stderr
//! draining lives** — structurally, so an adapter cannot forget it
//! (ARCHITECTURE §9 WP-2, PARITY.md §14.12). A child that writes past one
//! pipe buffer (~64 KiB on Linux) to an unread stderr blocks forever; in the
//! TS original `grok` and `antigravity` never attach a consumer and hang until
//! stall detection fires ten minutes later.
//!
//! Three policies, and the difference between the first two is observable:
//!
//! | Policy | Adapters | Behaviour |
//! |---|---|---|
//! | [`StderrPolicy::Tail`] | claude, codex, cursor, opencode, pi | drain, keep a bounded 4096-byte tail that is appended to the error message and *then* classified |
//! | [`StderrPolicy::Discard`] | grok, antigravity | drain and throw away — attaching the tail would change L0 error text, the `classifyAdapterError` result and the persisted `agent.last_error` |
//! | [`StderrPolicy::Stream`] | shell | handed to the adapter, which turns each line into an `error` event |
//!
//! It also hides `tokio::process::Child*` behind boxed `AsyncRead`/`AsyncWrite`
//! so the adapter event generators can be unit-tested against in-memory pipes.
//! Real child handles cannot be fabricated, which is why
//! `orch_core::test_support::FakeProcessManager` hands back `None` for all
//! three.

use std::sync::{Arc, Mutex};

use orch_core::adapter::AdapterError;
use orch_core::process::{ExitSignal, ProcessManager, SpawnOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

/// Bounded stderr retained for diagnostics (`utils.ts:15`).
const STDERR_TAIL_BYTES: usize = 4096;

pub type ChildReader = Box<dyn AsyncRead + Send + Unpin>;
pub type ChildWriter = Box<dyn AsyncWrite + Send + Unpin>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrPolicy {
    /// Drain and keep the last [`STDERR_TAIL_BYTES`] bytes.
    Tail,
    /// Drain to EOF and discard.
    Discard,
    /// Hand stderr to the adapter; it is responsible for reading it
    /// concurrently with stdout.
    Stream,
}

/// A handle onto the bounded stderr tail. Empty for
/// [`StderrPolicy::Discard`]/[`StderrPolicy::Stream`] and for a child with no
/// stderr, which is what makes "no tail in the message" the default.
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Option<Arc<Mutex<Vec<u8>>>>);

impl StderrTail {
    /// The captured tail, decoded and `trimEnd()`-ed as the TS getter is.
    pub fn get(&self) -> String {
        let Some(buf) = self.0.as_ref() else {
            return String::new();
        };
        let bytes = buf.lock().unwrap_or_else(|err| err.into_inner());
        String::from_utf8_lossy(&bytes).trim_end().to_owned()
    }
}

/// A spawned child, decoupled from `tokio::process`.
pub struct ChildIo {
    pub pid: u32,
    pub stdout: Option<ChildReader>,
    /// Only populated under [`StderrPolicy::Stream`].
    pub stderr: Option<ChildReader>,
    pub stdin: Option<ChildWriter>,
    pub exit: ExitSignal,
    pub stderr_tail: StderrTail,
}

impl std::fmt::Debug for ChildIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildIo")
            .field("pid", &self.pid)
            .field("has_stdout", &self.stdout.is_some())
            .field("has_stderr", &self.stderr.is_some())
            .field("has_stdin", &self.stdin.is_some())
            .finish()
    }
}

/// Spawn through the process manager and install the stderr policy.
///
/// A spawn failure becomes an [`AdapterError`] carrying the TS message
/// (`Failed to spawn process: <command>`) and its classification, because the
/// TS `ProcessManager.spawn` throws synchronously out of `execute()` when node
/// reports a child with no pid.
pub fn spawn_child(
    process_manager: &dyn ProcessManager,
    command: &str,
    args: &[String],
    opts: SpawnOptions,
    policy: StderrPolicy,
) -> std::result::Result<ChildIo, AdapterError> {
    let child = process_manager
        .spawn(command, args, opts)
        .map_err(|err| AdapterError::new(err.to_string(), None))?;

    let mut io = ChildIo {
        pid: child.pid,
        stdout: child.stdout.map(|out| Box::new(out) as ChildReader),
        stderr: None,
        stdin: child.stdin.map(|input| Box::new(input) as ChildWriter),
        exit: child.exit,
        stderr_tail: StderrTail::default(),
    };

    let stderr = child.stderr.map(|err| Box::new(err) as ChildReader);
    apply_stderr_policy(&mut io, stderr, policy);
    Ok(io)
}

/// The policy half of [`spawn_child`], split out so the adapter tests can
/// drive it with an in-memory stderr.
pub fn apply_stderr_policy(io: &mut ChildIo, stderr: Option<ChildReader>, policy: StderrPolicy) {
    let Some(stderr) = stderr else {
        return;
    };
    match policy {
        StderrPolicy::Stream => io.stderr = Some(stderr),
        StderrPolicy::Discard => {
            tokio::spawn(drain(stderr, None));
        }
        StderrPolicy::Tail => {
            let buf = Arc::new(Mutex::new(Vec::new()));
            io.stderr_tail = StderrTail(Some(Arc::clone(&buf)));
            tokio::spawn(drain(stderr, Some(buf)));
        }
    }
}

/// Read stderr to EOF, optionally retaining the trailing
/// [`STDERR_TAIL_BYTES`]. Read errors are swallowed, mirroring the TS no-op
/// `'error'` handler.
async fn drain(mut stderr: ChildReader, tail: Option<Arc<Mutex<Vec<u8>>>>) {
    let mut chunk = vec![0u8; STDERR_TAIL_BYTES];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) => return,
            Ok(read) => {
                if let Some(tail) = tail.as_ref() {
                    let mut buf = tail.lock().unwrap_or_else(|err| err.into_inner());
                    buf.extend_from_slice(&chunk[..read]);
                    if buf.len() > STDERR_TAIL_BYTES {
                        // Keep an exactly-sized tail rather than growing
                        // forever behind a chatty child.
                        let excess = buf.len() - STDERR_TAIL_BYTES;
                        buf.drain(..excess);
                    }
                }
            }
            Err(err) => {
                tracing::debug!(error = %err, "stderr read failed; dropping the rest");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::process::ExitOutcome;

    fn empty_io() -> ChildIo {
        ChildIo {
            pid: 1,
            stdout: None,
            stderr: None,
            stdin: None,
            exit: ExitSignal::resolved(ExitOutcome {
                code: Some(0),
                signal: None,
            }),
            stderr_tail: StderrTail::default(),
        }
    }

    fn reader(bytes: Vec<u8>) -> ChildReader {
        Box::new(std::io::Cursor::new(bytes))
    }

    async fn settle() {
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn the_tail_policy_keeps_only_the_last_4_kib() {
        let mut io = empty_io();
        let payload = format!("{}ACTIONABLE_ERROR", "x".repeat(5000));
        apply_stderr_policy(
            &mut io,
            Some(reader(payload.into_bytes())),
            StderrPolicy::Tail,
        );
        settle().await;

        let tail = io.stderr_tail.get();
        assert_eq!(tail.len(), STDERR_TAIL_BYTES);
        assert!(tail.ends_with("ACTIONABLE_ERROR"));
    }

    #[tokio::test]
    async fn the_tail_is_trimmed_at_the_end() {
        let mut io = empty_io();
        apply_stderr_policy(
            &mut io,
            Some(reader(b"boom\n\n".to_vec())),
            StderrPolicy::Tail,
        );
        settle().await;
        assert_eq!(io.stderr_tail.get(), "boom");
    }

    #[tokio::test]
    async fn the_discard_policy_keeps_no_tail_but_still_drains() {
        let mut io = empty_io();
        apply_stderr_policy(
            &mut io,
            Some(reader(b"rate limit reached".to_vec())),
            StderrPolicy::Discard,
        );
        settle().await;
        assert_eq!(io.stderr_tail.get(), "");
        assert!(io.stderr.is_none());
    }

    #[tokio::test]
    async fn the_stream_policy_hands_stderr_to_the_adapter() {
        let mut io = empty_io();
        apply_stderr_policy(&mut io, Some(reader(b"line\n".to_vec())), StderrPolicy::Stream);
        assert!(io.stderr.is_some());
        assert_eq!(io.stderr_tail.get(), "");
    }

    #[test]
    fn a_child_without_stderr_has_an_empty_tail() {
        let mut io = empty_io();
        apply_stderr_policy(&mut io, None, StderrPolicy::Tail);
        assert_eq!(io.stderr_tail.get(), "");
    }

    #[tokio::test]
    async fn a_spawn_failure_becomes_a_classified_adapter_error() {
        use crate::process::SystemProcessManager;
        use orch_domain::AdapterErrorKind;

        let pm = SystemProcessManager::new();
        let err = spawn_child(
            &pm,
            "orch-definitely-not-a-binary",
            &[],
            SpawnOptions::default(),
            StderrPolicy::Tail,
        )
        .expect_err("spawn should fail");

        assert_eq!(
            err.message,
            "Failed to spawn process: orch-definitely-not-a-binary"
        );
        // "Failed to spawn process" matches none of the classifier's
        // substrings and there is no exit code, exactly as in the TS.
        assert_eq!(err.kind, AdapterErrorKind::Unknown);
    }
}
