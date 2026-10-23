//! Antigravity CLI adapter for `kind = "antigravity"`.
//!
//! `agy`'s headless mode is plain-text oriented, so each stdout line becomes
//! an `output` event and a terminal `done` carries the joined text.
//!
//! Two quirks: the exit checks
//! have **no done-event guard**, so a non-zero exit always fails the run even
//! when output was produced; and an exit with no code (killed by a signal)
//! still yields a synthetic `done`.
//!
//! Like grok, its stderr is drained and **discarded**.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::StreamExt;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use tokio_util::sync::CancellationToken;

use super::util::{build_full_prompt, probe_version, system_clock, version_probe_failure};
use crate::process::line_reader::read_lines;
use crate::process::spawn::{ChildIo, StderrPolicy, spawn_child};

const INSTALL_HINT: &str =
    "Antigravity CLI not found. Install Google Antigravity CLI and ensure `agy` is on PATH.";

pub struct AntigravityAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl AntigravityAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        AntigravityAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for AntigravityAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityAdapter").finish_non_exhaustive()
    }
}

/// The prompt is argv[1] — **before** `--dangerously-skip-permissions`, not
/// last.
fn build_args(params: &ExecuteParams) -> Result<Vec<String>, AdapterError> {
    let effort = super::effort::selection("antigravity", &params.config)?;
    let mut args = vec![
        "-p".to_owned(),
        build_full_prompt(
            params
                .system_prompt
                .as_deref()
                .or(params.config.system_prompt.as_deref()),
            &params.prompt,
        ),
        "--dangerously-skip-permissions".to_owned(),
    ];

    if let Some(model) = &effort.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    args.extend(effort.args);
    Ok(args)
}

fn antigravity_events(
    io: ChildIo,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
) -> futures::stream::BoxStream<'static, Result<AgentEvent, AdapterError>> {
    let ChildIo { stdout, exit, .. } = io;

    Box::pin(async_stream::try_stream! {
        let mut final_text = String::new();

        if let Some(stdout) = stdout {
            let mut lines = Box::pin(read_lines(stdout));
            let mut read_error = None;
            while let Some(line) = lines.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                let line = match line {
                    Ok(line) => line,
                    Err(err) => {
                        read_error = Some(err);
                        break;
                    }
                };

                if !final_text.is_empty() {
                    final_text.push('\n');
                }
                final_text.push_str(&line);

                yield AgentEvent::new(
                    AgentEventKind::Output,
                    clock.now_iso(),
                    serde_json::json!({ "text": line }),
                );
            }
            drop(lines);

            if let Some(err) = read_error {
                Err(AdapterError::new(err.to_string(), None))?;
            }
        }

        let outcome = exit.wait().await;

        // No done-event guard, deliberately: see the module docs.
        if let Some(code) = outcome.code
            && code != 0
            && !cancel.is_cancelled()
        {
            let message = format!("Antigravity process exited with code {code}");
            Err(AdapterError::with_kind(
                message.clone(),
                orch_domain::classify_adapter_error(&message, Some(code)),
                Some(code),
            ))?;
        }

        if !cancel.is_cancelled() {
            yield AgentEvent::new(
                AgentEventKind::Done,
                clock.now_iso(),
                serde_json::json!({ "result": final_text }),
            );
        }
    })
}

#[async_trait]
impl Adapter for AntigravityAdapter {
    fn kind(&self) -> &'static str {
        "antigravity"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "agy").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let args = build_args(&params)?;
        let io = spawn_child(
            &self.process_manager,
            "agy",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                stdin: StdioMode::Ignore,
                inherit_env: params.inherit_env,
            },
            StderrPolicy::Discard,
            &params.cancel,
        )?;

        let pid = io.pid;
        let events = antigravity_events(io, Arc::clone(&self.clock), params.cancel);
        Ok(ExecuteHandle { pid, events })
    }

    async fn stop(&self, pid: u32) -> orch_core::Result<()> {
        self.process_manager
            .kill_with_grace(pid, DEFAULT_KILL_GRACE)
            .await;
        Ok(())
    }
}
