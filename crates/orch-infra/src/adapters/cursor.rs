//! Cursor Agent adapter for `kind = "cursor"`.
//!
//! Spawns Cursor's headless agent CLI with `--output-format stream-json`,
//! which emits the same format as Claude's, so the line dispatch is shared
//! (`super::util::parse_stream_json_line`) — minus claude's `stats.usage`
//! fallback.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use orch_domain::AdapterErrorKind;

use super::util::{
    build_full_prompt, parse_stream_json_line, probe_version, streaming_events, system_clock,
};
use crate::process::spawn::{StderrPolicy, spawn_child};

/// Tried in order by `test()`; the first that answers `--version` wins.
const COMMANDS: [&str; 2] = ["cursor-agent", "agent"];

pub struct CursorAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
    /// Per-instance state assigned **only** by `test()`. An `execute()` before
    /// a successful `test()` always spawns `cursor-agent`.
    resolved_command: Mutex<&'static str>,
}

impl CursorAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        CursorAdapter {
            process_manager,
            clock: system_clock(),
            resolved_command: Mutex::new(COMMANDS[0]),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn command(&self) -> &'static str {
        *self
            .resolved_command
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }
}

impl std::fmt::Debug for CursorAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorAdapter")
            .field("command", &self.command())
            .finish_non_exhaustive()
    }
}

/// Cursor's print mode needs the prompt as a positional argument; unlike
/// codex it cannot read it from stdin.
fn build_args(params: &ExecuteParams) -> Result<Vec<String>, AdapterError> {
    let effort = super::effort::selection("cursor", &params.config)?;
    let mut args = vec![
        "-p".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--workspace".to_owned(),
        params.workspace.to_string_lossy().into_owned(),
        // Bypass interactive prompts, and trust the fresh worktree ORCH made.
        "--yolo".to_owned(),
        "--trust".to_owned(),
    ];

    if let Some(model) = &effort.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    args.extend(effort.args);

    // `config.system_prompt` is deliberately ignored here.
    args.push(build_full_prompt(
        params.system_prompt.as_deref(),
        &params.prompt,
    ));
    Ok(args)
}

#[async_trait]
impl Adapter for CursorAdapter {
    fn kind(&self) -> &'static str {
        "cursor"
    }

    async fn test(&self) -> AdapterTestResult {
        for command in COMMANDS {
            if let Ok(version) = probe_version(self.process_manager.as_ref(), command).await {
                *self
                    .resolved_command
                    .lock()
                    .unwrap_or_else(|err| err.into_inner()) = command;
                return AdapterTestResult::ok(version);
            }
        }
        // Unlike the other adapters this one does not classify the probe
        // error; a missing binary is the only outcome it reports.
        AdapterTestResult::failed(
            "Cursor Agent CLI not found. The headless agent CLI is required (cursor-agent or agent).",
            AdapterErrorKind::AdapterNotFound,
        )
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let effort = super::effort::selection("cursor", &params.config)?;
        let args = build_args(&params)?;
        let io = spawn_child(
            &self.process_manager,
            self.command(),
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                stdin: StdioMode::Ignore,
                inherit_env: params.inherit_env,
            },
            StderrPolicy::Tail,
            &params.cancel,
        )?;

        let pid = io.pid;
        let clock = Arc::clone(&self.clock);
        let events = streaming_events(
            io,
            move |line| parse_stream_json_line(line, clock.as_ref(), false),
            "Cursor agent",
            params.cancel,
        );
        Ok(super::effort::with_diagnostics(
            ExecuteHandle { pid, events },
            effort.diagnostics,
            self.clock.as_ref(),
        ))
    }

    async fn stop(&self, pid: u32) -> orch_core::Result<()> {
        self.process_manager
            .kill_with_grace(pid, DEFAULT_KILL_GRACE)
            .await;
        Ok(())
    }
}
