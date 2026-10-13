//! Process management port — ported from
//! `src/infrastructure/process/process-manager.ts` (the interface only; the
//! `readLines` implementation and POSIX details belong to `orch-infra`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};

use crate::error::Result;

/// The signals the orchestrator actually sends.
///
/// A local enum rather than `nix::sys::signal::Signal` so `orch-core` stays
/// platform-free; `orch-infra` maps it to `nix` behind a `cfg(unix)`
/// boundary (ARCHITECTURE §2 A3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Term,
    Kill,
    Int,
    Hup,
}

impl Signal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Signal::Term => "SIGTERM",
            Signal::Kill => "SIGKILL",
            Signal::Int => "SIGINT",
            Signal::Hup => "SIGHUP",
        }
    }
}

impl std::fmt::Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a child's stdin is wired. `stdio: ['ignore', 'pipe', 'pipe']` is the
/// default — stdin is ignored, which is why every adapter passes a
/// permission-bypass flag. `codex` and `pi` override it to a pipe
/// (PARITY.md §8.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StdioMode {
    #[default]
    Ignore,
    Pipe,
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub cwd: PathBuf,
    /// Overlaid on the inherited environment.
    pub env: BTreeMap<String, String>,
    pub stdin: StdioMode,
}

/// How a child ended, as reported to node's `close` handler.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExitOutcome {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitOutcome {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Resolves once the child exits.
///
/// Cloneable and awaitable any number of times, because both the collector
/// loop and the error path subscribe to it — a plain `oneshot` cannot be
/// awaited twice (ARCHITECTURE §5.5).
#[derive(Debug, Clone)]
pub struct ExitSignal {
    rx: tokio::sync::watch::Receiver<Option<ExitOutcome>>,
}

/// The write half of an [`ExitSignal`], held by whoever reaps the child.
#[derive(Debug)]
pub struct ExitNotifier {
    tx: tokio::sync::watch::Sender<Option<ExitOutcome>>,
}

impl ExitNotifier {
    /// Publish the outcome. Later calls are ignored: the first exit wins.
    pub fn notify(&self, outcome: ExitOutcome) {
        self.tx.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(outcome);
                true
            } else {
                false
            }
        });
    }
}

impl ExitSignal {
    pub fn channel() -> (ExitNotifier, ExitSignal) {
        let (tx, rx) = tokio::sync::watch::channel(None);
        (ExitNotifier { tx }, ExitSignal { rx })
    }

    /// An already-resolved signal, for fakes and for a child that failed to
    /// spawn.
    pub fn resolved(outcome: ExitOutcome) -> ExitSignal {
        // The value is already set, so `wait` returns on the first borrow and
        // never observes the dropped sender.
        let (_tx, rx) = tokio::sync::watch::channel(Some(outcome));
        ExitSignal { rx }
    }

    /// Wait for the exit. Returns immediately once it has happened, however
    /// many times it is called.
    pub async fn wait(&self) -> ExitOutcome {
        let mut rx = self.rx.clone();
        loop {
            if let Some(outcome) = *rx.borrow_and_update() {
                return outcome;
            }
            if rx.changed().await.is_err() {
                // The notifier was dropped without an outcome — treat it as a
                // signal-less abnormal exit rather than hanging forever.
                return ExitOutcome {
                    code: None,
                    signal: None,
                };
            }
        }
    }

    /// The outcome if the child has already exited.
    pub fn try_get(&self) -> Option<ExitOutcome> {
        *self.rx.borrow()
    }
}

#[derive(Debug)]
pub struct SpawnedChild {
    pub pid: u32,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    pub stdin: Option<ChildStdin>,
    pub exit: ExitSignal,
}

#[async_trait]
pub trait ProcessManager: Send + Sync {
    /// `kill(pid, 0)` is `Ok`, **or** fails with `EPERM` — the process exists
    /// but we may not signal it.
    fn is_alive(&self, pid: u32) -> bool;

    /// Signals the whole process **group** first (`kill(-pid, sig)`), falling
    /// back to the bare PID, swallowing errors either way. The group kill is
    /// what reaps a child's own children (vitest, playwright, …).
    fn kill(&self, pid: u32, signal: Signal);

    /// Return if already dead; `SIGTERM`; poll [`ProcessManager::is_alive`]
    /// every 200 ms until the deadline; then `SIGKILL`.
    ///
    /// Grace by call site: `cancel_task`/`force_stop_agent` 3 s, stall/abort
    /// 5 s, `stop()` 10 s (ARCHITECTURE §6.4).
    async fn kill_with_grace(&self, pid: u32, grace: Duration);

    /// Spawns in a new process group (`setsid`), so the group kill above
    /// reaches the whole tree.
    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild>;
}

/// The default grace period of the TS `killWithGrace(pid, graceMs = 10_000)`.
pub const DEFAULT_KILL_GRACE: Duration = Duration::from_millis(10_000);

/// The interval at which `kill_with_grace` re-checks liveness (PARITY.md §8.4).
pub const KILL_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_names() {
        assert_eq!(Signal::Term.to_string(), "SIGTERM");
        assert_eq!(Signal::Kill.to_string(), "SIGKILL");
        assert_eq!(Signal::Int.to_string(), "SIGINT");
        assert_eq!(Signal::Hup.to_string(), "SIGHUP");
    }

    #[test]
    fn stdin_defaults_to_ignore() {
        assert_eq!(SpawnOptions::default().stdin, StdioMode::Ignore);
    }

    #[test]
    fn exit_outcome_success_is_code_zero_only() {
        assert!(
            ExitOutcome {
                code: Some(0),
                signal: None
            }
            .success()
        );
        assert!(
            !ExitOutcome {
                code: Some(1),
                signal: None
            }
            .success()
        );
        assert!(
            !ExitOutcome {
                code: None,
                signal: Some(9)
            }
            .success()
        );
    }

    #[tokio::test]
    async fn exit_signal_can_be_awaited_more_than_once() {
        let (notifier, signal) = ExitSignal::channel();
        assert_eq!(signal.try_get(), None);
        notifier.notify(ExitOutcome {
            code: Some(0),
            signal: None,
        });

        for _ in 0..3 {
            assert_eq!(
                signal.wait().await,
                ExitOutcome {
                    code: Some(0),
                    signal: None
                }
            );
        }
        assert_eq!(
            signal.try_get(),
            Some(ExitOutcome {
                code: Some(0),
                signal: None
            })
        );
    }

    #[tokio::test]
    async fn the_first_exit_wins() {
        let (notifier, signal) = ExitSignal::channel();
        notifier.notify(ExitOutcome {
            code: Some(0),
            signal: None,
        });
        notifier.notify(ExitOutcome {
            code: Some(1),
            signal: None,
        });
        assert_eq!(signal.wait().await.code, Some(0));
    }

    #[tokio::test]
    async fn a_subscriber_waiting_before_the_exit_is_woken() {
        let (notifier, signal) = ExitSignal::channel();
        let waiter = tokio::spawn({
            let signal = signal.clone();
            async move { signal.wait().await }
        });
        tokio::task::yield_now().await;
        notifier.notify(ExitOutcome {
            code: Some(7),
            signal: None,
        });
        assert_eq!(waiter.await.unwrap().code, Some(7));
    }

    #[tokio::test]
    async fn dropping_the_notifier_without_an_outcome_does_not_hang() {
        let (notifier, signal) = ExitSignal::channel();
        drop(notifier);
        assert_eq!(
            signal.wait().await,
            ExitOutcome {
                code: None,
                signal: None
            }
        );
    }

    #[tokio::test]
    async fn a_resolved_signal_is_ready_immediately() {
        let signal = ExitSignal::resolved(ExitOutcome {
            code: Some(3),
            signal: None,
        });
        assert_eq!(signal.try_get().unwrap().code, Some(3));
        assert_eq!(signal.wait().await.code, Some(3));
    }
}
