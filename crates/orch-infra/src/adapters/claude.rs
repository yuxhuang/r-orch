//! Claude Code adapter — `src/infrastructure/adapters/claude.ts`,
//! `kind = "claude"` (`docs/parity/adapters.md` §2.1).
//!
//! Spawns `claude --print --output-format stream-json` in headless mode and
//! parses JSON lines from stdout.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};

use super::util::{
    parse_stream_json_line, probe_version, streaming_events, system_clock, version_probe_failure,
};
use crate::process::spawn::{StderrPolicy, spawn_child};

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

/// `--max-turns` is **always** passed, defaulting to 50, and the prompt is
/// always the last positional.
fn build_args(params: &ExecuteParams) -> Vec<String> {
    let mut args = vec![
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--max-turns".to_owned(),
        params.config.max_turns.unwrap_or(50).to_string(),
        "--verbose".to_owned(),
        // Agents run autonomously and stdin is ignored, so an interactive
        // permission prompt would hang forever.
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

    // The orchestrator-generated (cacheable) system prompt wins over the
    // per-agent one.
    if let Some(system_prompt) = params
        .system_prompt
        .as_deref()
        .or(params.config.system_prompt.as_deref())
    {
        args.push("--system-prompt".to_owned());
        args.push(system_prompt.to_owned());
    }

    args.push(params.prompt.clone());
    args
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
        let args = build_args(&params);
        let io = spawn_child(
            self.process_manager.as_ref(),
            "claude",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                stdin: StdioMode::Ignore,
            },
            StderrPolicy::Tail,
        )?;

        let pid = io.pid;
        let clock = Arc::clone(&self.clock);
        let events = streaming_events(
            io,
            move |line| parse_stream_json_line(line, clock.as_ref(), true),
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
