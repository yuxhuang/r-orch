//! Process management port — ported from
//! `src/infrastructure/process/process-manager.ts` (the interface only; the
//! `readLines` implementation and POSIX details belong to `orch-infra`).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
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

/// Which part of the orch process's own environment a child inherits.
///
/// Local runs inherit everything (PARITY.md §8.3). Work submitted by a remote
/// peer must not receive the worker user's credentials, so it inherits
/// [`EnvInheritance::WithoutSecrets`] (security review F2). The explicit
/// overlay ([`SpawnOptions::env`]: the agent's own `config.env` plus the
/// `ORCH_*` keys) is applied after filtering and always reaches the child.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EnvInheritance {
    #[default]
    Full,
    /// Drops every inherited variable whose name [`is_secret_env_name`]
    /// matches, unless the worker owner lists it in
    /// [`REMOTE_ENV_PASSTHROUGH`].
    WithoutSecrets,
}

/// Set on the worker's `orch` process to a comma/whitespace separated list of
/// exact variable names that remote-submitted runs may still inherit, e.g.
/// `ORCH_REMOTE_ENV_PASSTHROUGH=ANTHROPIC_API_KEY`. Read at spawn time.
pub const REMOTE_ENV_PASSTHROUGH: &str = "ORCH_REMOTE_ENV_PASSTHROUGH";

/// Name segments (split on `_`, case-insensitive) that mark a credential.
const SECRET_SEGMENTS: &[&str] = &[
    "KEY",
    "KEYS",
    "APIKEY",
    "TOKEN",
    "TOKENS",
    "SECRET",
    "SECRETS",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "PAT",
    "AUTH",
    "OAUTH",
    "CREDENTIAL",
    "CREDENTIALS",
    "COOKIE",
];

/// Substrings that mark a credential even inside a single segment
/// (`PGPASSWORD`, `GITHUBTOKEN`, `MYSECRET`).
const SECRET_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "APIKEY",
    "CREDENTIAL",
    "PRIVATEKEY",
];

/// Whether an inherited variable looks like a credential: `*_KEY`, `*_TOKEN`,
/// `*_SECRET`, `*PASSWORD*`, `SSH_AUTH_SOCK`, `AWS_ACCESS_KEY_ID`, ….
///
/// Name-based, so it is a denylist: a secret stored under an innocuous name
/// (`DATABASE_URL=postgres://u:p@…`) is not caught. `GIT_AUTHOR_*` and
/// similar are deliberately *not* matched (`AUTHOR` is not the `AUTH`
/// segment).
pub fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper
        .split('_')
        .any(|segment| SECRET_SEGMENTS.contains(&segment))
        || SECRET_SUBSTRINGS
            .iter()
            .any(|needle| upper.contains(needle))
}

/// The inherited variables a [`EnvInheritance::WithoutSecrets`] child must
/// not see: every secret-looking name in `inherited`, minus the exact names
/// listed in `passthrough` (the value of [`REMOTE_ENV_PASSTHROUGH`]).
/// A name that is not valid UTF-8 cannot be vetted, so it is dropped too.
pub fn secret_env_names<I>(inherited: I, passthrough: Option<&OsStr>) -> Vec<OsString>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let allowed: Vec<String> = passthrough
        .map(|list| {
            list.to_string_lossy()
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    inherited
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| match name.to_str() {
            Some(name) => is_secret_env_name(name) && !allowed.iter().any(|a| a == name),
            None => true,
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub cwd: PathBuf,
    /// Overlaid on the inherited environment.
    pub env: BTreeMap<String, String>,
    pub stdin: StdioMode,
    /// Filter applied to the inherited environment before `env` is overlaid.
    pub inherit_env: EnvInheritance,
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

/// A child's stdout or stderr.
pub type ChildReader = Box<dyn AsyncRead + Send + Unpin>;

/// A child's stdin.
pub type ChildWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// A spawned child, with its pipes as trait objects rather than
/// `tokio::process::Child*` handles.
///
/// **Amendment 1** (see `docs/TRAITS-FROZEN.md`): the concrete handles cannot
/// be fabricated without a process, so a fake could only ever hand back
/// `None` — which left every adapter's event stream and the two stdin
/// protocols (codex, pi) untestable with an in-memory double.
/// [`crate::test_support::FakeProcessManager`] now scripts both output streams
/// and captures stdin.
pub struct SpawnedChild {
    pub pid: u32,
    pub stdout: Option<ChildReader>,
    pub stderr: Option<ChildReader>,
    pub stdin: Option<ChildWriter>,
    pub exit: ExitSignal,
}

impl SpawnedChild {
    /// Wrap the real handles a `tokio::process::Child` hands out.
    pub fn from_tokio(
        pid: u32,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        stdin: Option<ChildStdin>,
        exit: ExitSignal,
    ) -> Self {
        SpawnedChild {
            pid,
            stdout: stdout.map(|out| Box::new(out) as ChildReader),
            stderr: stderr.map(|err| Box::new(err) as ChildReader),
            stdin: stdin.map(|input| Box::new(input) as ChildWriter),
            exit,
        }
    }
}

impl std::fmt::Debug for SpawnedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnedChild")
            .field("pid", &self.pid)
            .field("has_stdout", &self.stdout.is_some())
            .field("has_stderr", &self.stderr.is_some())
            .field("has_stdin", &self.stdin.is_some())
            .finish()
    }
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
    ///
    /// Cancellation is **not** handled here. The TS passes an `AbortSignal` to
    /// `spawn`, which makes node signal the child itself; the Rust equivalent
    /// lives in `orch-infra`'s shared adapter spawn path, which every adapter
    /// goes through. The orchestrator's own grace-period kills are still
    /// required — see the spec issue in `docs/TRAITS-FROZEN.md`.
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
    fn spawn_inherits_the_full_environment_by_default() {
        assert_eq!(SpawnOptions::default().inherit_env, EnvInheritance::Full);
    }

    #[test]
    fn credential_shaped_names_are_secret() {
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENROUTER_API_KEY",
            "HF_TOKEN",
            "OMNIROUTE_API_KEY",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GITHUB_PAT",
            "GH_TOKEN",
            "PGPASSWORD",
            "SMTP_PASS",
            "SSH_AUTH_SOCK",
            "ORCH_WEB_PASSWORD",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "client_secret",
        ] {
            assert!(is_secret_env_name(name), "{name} must be treated as secret");
        }
    }

    #[test]
    fn ordinary_environment_names_are_not_secret() {
        for name in [
            "PATH",
            "HOME",
            "USER",
            "SHELL",
            "LANG",
            "LC_ALL",
            "TERM",
            "TMPDIR",
            "PWD",
            "XDG_CONFIG_HOME",
            "CARGO_HOME",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_ASKPASS",
            "ANTHROPIC_BASE_URL",
            "NO_PROXY",
            "ORCH_AGENT_ID",
            "KEYCHAIN_PATH",
        ] {
            assert!(!is_secret_env_name(name), "{name} must be inherited");
        }
    }

    #[test]
    fn secret_env_names_honours_exact_passthrough() {
        let inherited = [
            ("ANTHROPIC_API_KEY", "a"),
            ("HF_TOKEN", "b"),
            ("PATH", "/bin"),
            ("hf_token", "c"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let removed = secret_env_names(
            inherited.clone(),
            Some(OsStr::new("ANTHROPIC_API_KEY, OTHER")),
        );
        assert_eq!(
            removed,
            vec![OsString::from("HF_TOKEN"), OsString::from("hf_token")]
        );
        let removed = secret_env_names(inherited, None);
        assert_eq!(removed.len(), 3);
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
