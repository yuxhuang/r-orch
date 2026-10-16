//! Claude Code adapter — `src/infrastructure/adapters/claude.ts`,
//! `kind = "claude"` (`docs/parity/adapters.md` §2.1).
//!
//! Spawns `claude --print --output-format stream-json` in headless mode and
//! parses JSON lines from stdout.
//!
//! Neither prompt travels in argv: Linux caps a single argv string at 128 KiB
//! (`MAX_ARG_STRLEN`) and the orchestrator system prompt, which embeds every
//! agent's role, can exceed it, failing `exec` with `E2BIG`. The system prompt
//! goes through a private temp file (`--system-prompt-file`) and the task
//! prompt through stdin.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;

use super::util::{
    parse_stream_json_line, probe_version, streaming_events, system_clock, version_probe_failure,
};
use crate::process::spawn::{ChildWriter, StderrPolicy, spawn_child};

const INSTALL_HINT: &str = "Claude Code CLI not found. Install: npm i -g @anthropic-ai/claude-code";

pub struct ClaudeAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl ClaudeAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        ClaudeAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    /// Replace the timestamp source, so tests can assert on exact events.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for ClaudeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeAdapter").finish_non_exhaustive()
    }
}

/// `--max-turns` is **always** passed, defaulting to 50. The prompt is not an
/// argument: `--print` reads it from stdin.
fn build_args(params: &ExecuteParams, system_prompt_file: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--max-turns".to_owned(),
        params.config.max_turns.unwrap_or(50).to_string(),
        "--verbose".to_owned(),
        // Agents run autonomously, so an interactive permission prompt would
        // hang forever.
        "--dangerously-skip-permissions".to_owned(),
    ];

    if let Some(model) = &params.config.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = params.config.effort {
        args.push("--effort".to_owned());
        args.push(effort.as_str().to_owned());
    }
    if let Some(path) = system_prompt_file {
        args.push("--system-prompt-file".to_owned());
        args.push(path.to_string_lossy().into_owned());
    }
    args
}

/// The orchestrator-generated (cacheable) system prompt wins over the
/// per-agent one.
fn effective_system_prompt(params: &ExecuteParams) -> Option<&str> {
    params
        .system_prompt
        .as_deref()
        .or(params.config.system_prompt.as_deref())
}

/// Write the system prompt to a fresh file in the OS temp dir: random name,
/// created exclusively with mode 0600, never under the (possibly git-tracked)
/// agent workspace. The returned guard deletes the file when dropped.
fn write_system_prompt_file(system_prompt: &str) -> Result<TempPath, AdapterError> {
    let fail = |err: std::io::Error| {
        tracing::error!(error = %err, "failed to write the claude system prompt file");
        AdapterError::new(
            format!("Failed to write the Claude system prompt file: {err}"),
            None,
        )
    };
    let mut file = tempfile::Builder::new()
        .prefix("orch-claude-system-prompt-")
        .suffix(".txt")
        .tempfile()
        .map_err(fail)?;
    file.write_all(system_prompt.as_bytes()).map_err(fail)?;
    file.flush().map_err(fail)?;
    Ok(file.into_temp_path())
}

/// A single write of the task prompt, then close stdin — `claude --print`
/// waits for EOF before it starts.
fn write_prompt(stdin: Option<ChildWriter>, prompt: String) {
    let Some(mut stdin) = stdin else {
        return;
    };
    tokio::spawn(async move {
        if let Err(err) = stdin.write_all(prompt.as_bytes()).await {
            tracing::warn!(error = %err, "failed to write the claude prompt to stdin");
            return;
        }
        if let Err(err) = stdin.shutdown().await {
            tracing::debug!(error = %err, "failed to close claude stdin");
        }
    });
}

#[async_trait]
impl Adapter for ClaudeAdapter {
    fn kind(&self) -> &'static str {
        "claude"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "claude").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        // Dropped (deleting the file) on any early return, including a failed
        // spawn.
        let system_prompt_file = effective_system_prompt(&params)
            .map(write_system_prompt_file)
            .transpose()?;
        let args = build_args(&params, system_prompt_file.as_deref());
        let mut io = spawn_child(
            &self.process_manager,
            "claude",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                // stdin must be a pipe to carry the prompt.
                stdin: StdioMode::Pipe,
                inherit_env: params.inherit_env,
            },
            StderrPolicy::Tail,
            &params.cancel,
        )?;

        write_prompt(io.stdin.take(), params.prompt);

        let pid = io.pid;
        let clock = Arc::clone(&self.clock);
        let events = streaming_events(
            io,
            move |line| {
                // Owning the file here ties its deletion to the end (or drop,
                // on cancel) of the event stream, i.e. to the process exiting.
                let _keep_alive = &system_prompt_file;
                parse_stream_json_line(line, clock.as_ref(), true)
            },
            "Claude",
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
