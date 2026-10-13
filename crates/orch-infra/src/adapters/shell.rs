//! Shell adapter — `src/infrastructure/adapters/shell.ts`,
//! `kind = "shell"` (`docs/parity/adapters.md` §2.8).
//!
//! Runs an arbitrary command through `bash -lc` with the task prompt in
//! `ORCHESTRY_TASK_PROMPT` — the only environment variable any adapter sets
//! itself. It is also the only adapter that **streams** stderr: each stderr
//! line becomes an `error` event. stdout and stderr are read **concurrently**
//! into one [`EventBuffer`]; reading them in sequence would deadlock on a full
//! pipe (PARITY.md §8.11).
//!
//! No quoting happens here: the single command string is handed to bash, which
//! parses it. That is the adapter's contract — `config.command` comes from the
//! agent's own configuration, which is as trusted as the orchestrator itself.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::StreamExt;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{
    DEFAULT_KILL_GRACE, ExitOutcome, ProcessManager, SpawnOptions, StdioMode,
};
use orch_domain::AdapterErrorKind;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::buffer::{EventBuffer, drain};
use super::util::{build_full_prompt, classify, system_clock};
use crate::process::line_reader::read_lines;
use crate::process::spawn::{ChildIo, ChildReader, StderrPolicy, spawn_child};

/// The grace period the abort handler uses, unique to this adapter.
const ABORT_GRACE: Duration = Duration::from_millis(5_000);

pub struct ShellAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl ShellAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        ShellAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for ShellAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellAdapter").finish_non_exhaustive()
    }
}

/// Aborts the task it holds when dropped — the TS
/// `signal?.removeEventListener('abort', onAbort)` in its `finally`.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `${signal ?? 'unknown'}` — node reports a signal by name.
fn signal_name(signal: Option<i32>) -> String {
    signal
        .and_then(|signal| nix::sys::signal::Signal::try_from(signal).ok())
        .map(|signal| signal.as_str().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn exit_failure(outcome: ExitOutcome) -> Option<AdapterError> {
    if outcome.code == Some(0) {
        return None;
    }
    let message = match outcome.code {
        // Unlike `createStreamingEvents`, a signal kill *is* a failure here.
        None => format!(
            "Shell command terminated by signal {}",
            signal_name(outcome.signal)
        ),
        Some(code) => format!("Shell command exited with code {code}"),
    };
    let kind = orch_domain::classify_adapter_error(&message, outcome.code);
    Some(AdapterError::with_kind(message, kind, outcome.code))
}

/// Read one stream to EOF, pushing an event per line.
async fn pump(
    reader: ChildReader,
    buffer: Arc<EventBuffer>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    kind: AgentEventKind,
) {
    let mut lines = Box::pin(read_lines(reader));
    while let Some(line) = lines.next().await {
        if cancel.is_cancelled() {
            return;
        }
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                // The TS rejection also just ends this reader; the buffer is
                // closed either way, which is what keeps the run from hanging.
                tracing::debug!(error = %err, ?kind, "shell stream read failed");
                return;
            }
        };
        let mut event = AgentEvent::new(kind, clock.now_iso(), Value::String(line.clone()));
        if kind == AgentEventKind::Error {
            event = event.with_error_kind(classify(&line));
        }
        buffer.push(event).await;
    }
}

fn shell_events(
    io: ChildIo,
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
) -> futures::stream::BoxStream<'static, Result<AgentEvent, AdapterError>> {
    let ChildIo {
        pid,
        stdout,
        stderr,
        exit,
        ..
    } = io;

    Box::pin(async_stream::try_stream! {
        // Reap the process on abort: SIGTERM, 5 s grace, SIGKILL. Subscribed
        // before anything is read, and dropped with the stream.
        let _abort = AbortOnDrop(tokio::spawn({
            let cancel = cancel.clone();
            let process_manager = Arc::clone(&process_manager);
            async move {
                cancel.cancelled().await;
                process_manager.kill_with_grace(pid, ABORT_GRACE).await;
            }
        }));

        let buffer = Arc::new(EventBuffer::new());
        let mut pumps = Vec::new();
        for (reader, kind) in [
            (stdout, AgentEventKind::Output),
            (stderr, AgentEventKind::Error),
        ] {
            if let Some(reader) = reader {
                pumps.push(tokio::spawn(pump(
                    reader,
                    Arc::clone(&buffer),
                    Arc::clone(&clock),
                    cancel.clone(),
                    kind,
                )));
            }
        }

        // Close the buffer once both streams are drained, so the consumer
        // below terminates.
        tokio::spawn({
            let buffer = Arc::clone(&buffer);
            async move {
                for pump in pumps {
                    let _ = pump.await;
                }
                buffer.close();
            }
        });

        let mut events = Box::pin(drain(Arc::clone(&buffer)));
        while let Some(event) = events.next().await {
            yield event;
        }

        let outcome = exit.wait().await;
        if cancel.is_cancelled() {
            return;
        }
        if let Some(err) = exit_failure(outcome) {
            Err(err)?;
        }
    })
}

/// The handle returned when `config.command` is missing: pid 0 and a single
/// `error` event whose `data` is the bare message string.
fn missing_command(clock: &dyn Clock) -> ExecuteHandle {
    let event = AgentEvent::new(
        AgentEventKind::Error,
        clock.now_iso(),
        Value::String("Shell adapter requires a command in agent config".to_owned()),
    )
    .with_error_kind(AdapterErrorKind::SpawnFailed);

    ExecuteHandle {
        pid: 0,
        events: Box::pin(futures::stream::once(async move { Ok(event) })),
    }
}

#[async_trait]
impl Adapter for ShellAdapter {
    fn kind(&self) -> &'static str {
        "shell"
    }

    async fn test(&self) -> AdapterTestResult {
        match super::util::probe_version(self.process_manager.as_ref(), "bash").await {
            // Only the first line of `bash --version` is the version.
            Ok(version) => AdapterTestResult::ok(
                version
                    .split('\n')
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .unwrap_or("unknown"),
            ),
            Err(_) => AdapterTestResult::failed("bash not found", classify("bash not found")),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let Some(command) = params.config.command.clone() else {
            return Ok(missing_command(self.clock.as_ref()));
        };

        let mut env = params.env;
        env.insert(
            "ORCHESTRY_TASK_PROMPT".to_owned(),
            build_full_prompt(params.system_prompt.as_deref(), &params.prompt),
        );

        let io = spawn_child(
            &self.process_manager,
            "bash",
            &["-lc".to_owned(), command],
            SpawnOptions {
                cwd: params.workspace,
                env,
                stdin: StdioMode::Ignore,
            },
            StderrPolicy::Stream,
            &params.cancel,
        )?;

        let pid = io.pid;
        let events = shell_events(
            io,
            Arc::clone(&self.process_manager),
            Arc::clone(&self.clock),
            params.cancel,
        );
        Ok(ExecuteHandle { pid, events })
    }

    async fn stop(&self, pid: u32) -> orch_core::Result<()> {
        self.process_manager
            .kill_with_grace(pid, DEFAULT_KILL_GRACE)
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::StreamExt;
    use orch_core::process::ExitSignal;
    use orch_core::test_support::FakeProcessManager;

    /// A stdout that fails on first read. The TS equivalent is
    /// `proc.stdout.destroy(new Error(...))`, and the test it guards is the
    /// one that used to hang forever: if the buffer is never closed, the
    /// consumer waits for an event that will never arrive.
    struct FailingReader;

    impl tokio::io::AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let _ = buf;
            std::task::Poll::Ready(Err(std::io::Error::other("stream read error")))
        }
    }

    #[tokio::test]
    async fn a_failing_stream_closes_the_buffer_instead_of_hanging() {
        let io = ChildIo {
            pid: 9999,
            stdout: Some(Box::new(FailingReader)),
            stderr: Some(Box::new(std::io::Cursor::new(b"still readable\n".to_vec()))),
            stdin: None,
            exit: ExitSignal::resolved(ExitOutcome {
                code: Some(1),
                signal: None,
            }),
            stderr_tail: Default::default(),
        };

        let stream = shell_events(
            io,
            Arc::new(FakeProcessManager::new()),
            system_clock(),
            CancellationToken::new(),
        );

        let collected = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the stream must finish rather than wait for a buffer that never closes");

        // The readable half still produced its event, and the run then failed
        // on the exit code.
        assert_eq!(collected.len(), 2);
        assert_eq!(
            collected[0].as_ref().expect("event").data,
            Value::String("still readable".to_owned())
        );
        assert_eq!(
            collected[1].as_ref().expect_err("the exit failed").message,
            "Shell command exited with code 1"
        );
    }

    #[test]
    fn a_clean_exit_is_not_a_failure() {
        assert!(
            exit_failure(ExitOutcome {
                code: Some(0),
                signal: None
            })
            .is_none()
        );
    }

    #[test]
    fn a_non_zero_exit_reports_the_code_and_classifies_as_a_crash() {
        let err = exit_failure(ExitOutcome {
            code: Some(7),
            signal: None,
        })
        .expect("failure");
        assert_eq!(err.message, "Shell command exited with code 7");
        assert_eq!(err.kind, AdapterErrorKind::ProcessCrash);
        assert_eq!(err.exit_code, Some(7));
    }

    #[test]
    fn a_signal_kill_reports_the_signal_name() {
        let err = exit_failure(ExitOutcome {
            code: None,
            signal: Some(15),
        })
        .expect("failure");
        assert_eq!(err.message, "Shell command terminated by signal SIGTERM");
        assert_eq!(err.kind, AdapterErrorKind::Unknown);

        let err = exit_failure(ExitOutcome {
            code: None,
            signal: None,
        })
        .expect("failure");
        assert_eq!(err.message, "Shell command terminated by signal unknown");
    }
}
