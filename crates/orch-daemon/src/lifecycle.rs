//! Explicit owner start and bootstrap initialization (design D3, D8; spec
//! R4/R5/R14). Nothing a client runs may call into this module: only
//! `orch daemon start`, `serve`/`run --watch` (PR 2) and `orch init` do.
//!
//! Owner of the bodies: S1a-impl.

use crate::DaemonError;
use std::path::PathBuf;

/// Whether automatic dispatch runs after READY. A fresh daemon defaults to
/// `Off` so the owner can queue work first (R5); `serve`/`run --watch` pass
/// `On`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DispatchMode {
    #[default]
    Off,
    On,
}

/// Options of an explicit daemon start.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// The owning project root, already selected by the caller per D4
    /// (`ORCH_PROJECT_ROOT` over the cwd ancestor walk).
    pub project_root: PathBuf,
    /// An explicit endpoint (`ORCH_SOCKET`). `None` uses the default
    /// `.orchestry/run/orch.sock`, or the `$TMPDIR/orch-<hash>/orch.sock`
    /// fallback when that exceeds 100 bytes.
    pub socket: Option<PathBuf>,
    /// `--detach`: new session via setsid, stdin `/dev/null`, output to the
    /// private capped `.orchestry/daemon.log`; the parent returns only after
    /// the child is READY, and a child failure is the parent's failure.
    pub detach: bool,
    /// `--dispatch on|off`.
    pub dispatch: DispatchMode,
    /// `--from-agent`: the owner deliberately starts from an agent context
    /// (`ORCH_TASK_ID`/`ORCH_AGENT_ID` set). Without it such a start exits 2.
    pub from_agent: bool,
    /// `--listen <addr>`: accept remote peer connections on this address.
    pub listen: Option<String>,
    /// PR 1 developmental gate (SE-20): the caller read
    /// `ORCH_DAEMON_DEVELOPMENTAL=1`. Without it start refuses with exit 2;
    /// with it start prints the banner that legacy writers are not excluded.
    pub developmental: bool,
}

/// How a daemon run ended. `code` is the process exit status for
/// `orch daemon start`: 0 after a clean stop, otherwise the failure's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonExit {
    pub code: i32,
    /// `false` when shutdown cleanup was incomplete (R14): the evidence is
    /// kept and the stop is not reported as clean.
    pub clean: bool,
}

impl DaemonExit {
    pub const CLEAN: DaemonExit = DaemonExit {
        code: 0,
        clean: true,
    };
}

/// Run an explicitly started daemon until stop, SIGTERM, SIGINT or SIGHUP.
///
/// In the foreground this prints exactly `listening <socket>` once recovery,
/// binding and discovery publication are done, then the dispatch state. With
/// `detach` it returns as soon as the detached child reports READY.
///
/// Fails with [`DaemonError::AlreadyRunning`] (exit 4) when another process
/// owns the project and [`DaemonError::RefusedInAgentContext`] (exit 2) from
/// an agent context without `from_agent`. Never called from a client path.
pub async fn start(options: StartOptions) -> Result<DaemonExit, DaemonError> {
    let _ = options;
    Err(DaemonError::unimplemented("orch_daemon::start", "S1a-impl"))
}

/// Options of `orch init`, the only project-creation path. Mirrors the CLI's
/// existing `InitArgs`.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub project_root: PathBuf,
    /// `--name`.
    pub name: Option<String>,
    /// `--adapter`.
    pub adapter: Option<String>,
    /// `--prompt-mode`; omitted writes `normal`.
    pub prompt_mode: Option<orch_domain::PromptMode>,
    /// Both stdout and stdin are a TTY (gates the multi-adapter prompt in the
    /// CLI; the daemon API itself never prompts).
    pub interactive: bool,
}

/// What [`initialize`] did, so the CLI can print the start hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// Re-run on an initialized project: nothing was overwritten.
    pub already_initialized: bool,
    pub adapter: String,
}

/// Create `.orchestry/` (idempotently). Starts no server, never mutates a
/// live daemon's state, and leaves printing `orch daemon start` to the CLI.
pub async fn initialize(options: InitOptions) -> Result<InitOutcome, DaemonError> {
    let _ = options;
    Err(DaemonError::unimplemented(
        "orch_daemon::initialize",
        "S1a-impl",
    ))
}
