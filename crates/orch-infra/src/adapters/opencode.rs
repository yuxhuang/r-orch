//! OpenCode adapter for `kind = "opencode"`.
//!
//! Spawns `opencode run --format json`. There is **no workspace flag**: the
//! workspace reaches the child only through the spawn cwd.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, AgentEventKind, ExecuteHandle,
    ExecuteParams,
};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use orch_domain::{
    EffortDispatch, TokenUsage, TokenUsageOpts, create_token_usage, plan_after_wire_check_failure,
    plan_effort_dispatch,
};
use serde_json::{Map, Value};

use super::util::{
    build_full_prompt, copy_present, error_event, number_field, probe_version, streaming_events,
    stringify, system_clock, version_probe_failure,
};
use crate::process::spawn::{StderrPolicy, spawn_child};

const INSTALL_HINT: &str = "OpenCode CLI not found. Install: npm i -g opencode";

pub struct OpenCodeAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl OpenCodeAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        OpenCodeAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

const PROBE_LIMIT: usize = 64 * 1024;
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

// Dropping preparation (including task cancellation) must not orphan a probe.
struct ProbeGuard {
    manager: Arc<dyn ProcessManager>,
    pid: u32,
    armed: bool,
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if self.armed {
            self.manager.kill(self.pid, orch_core::Signal::Kill);
        }
    }
}

impl OpenCodeAdapter {
    async fn check_effort_wire(&self, params: &ExecuteParams) -> Result<(), &'static str> {
        let version = self.probe(params, &["--version"]).await?;
        let version = version
            .trim()
            .strip_prefix("opencode ")
            .unwrap_or(version.trim());
        let version = version.strip_prefix('v').unwrap_or(version);
        let parts: Vec<_> = version.split('.').collect();
        if parts.len() != 3
            || parts[0] != "2"
            || !parts[1..]
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
        {
            return Err("OpenCode version is not recognized major 2");
        }
        let help = self.probe(params, &["run", "--help"]).await?;
        if !help.lines().any(|line| {
            line.split_whitespace()
                .any(|word| word == "--model" || word == "--model,")
                && line.contains("provider/model#variant")
        }) {
            return Err("OpenCode run help lacks the v2 model variant signature");
        }
        Ok(())
    }

    async fn probe(&self, params: &ExecuteParams, args: &[&str]) -> Result<String, &'static str> {
        use tokio::io::AsyncReadExt;
        let mut child = self
            .process_manager
            .spawn(
                "opencode",
                &args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>(),
                SpawnOptions {
                    cwd: params.workspace.clone(),
                    env: params.env.clone(),
                    stdin: StdioMode::Ignore,
                    inherit_env: params.inherit_env,
                },
            )
            .map_err(|_| "OpenCode syntax probe could not spawn")?;
        let mut guard = ProbeGuard {
            manager: Arc::clone(&self.process_manager),
            pid: child.pid,
            armed: true,
        };
        let collect = async {
            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut out_buf = [0u8; 4096];
            let mut err_buf = [0u8; 4096];
            // Read both pipes concurrently. One noisy pipe cannot starve the other,
            // and no allocation can exceed the shared output budget.
            while stdout.is_some() || stderr.is_some() {
                tokio::select! {
                    result = async { stdout.as_mut().expect("guarded stdout").read(&mut out_buf).await }, if stdout.is_some() => {
                        let n = result.map_err(|_| "OpenCode syntax probe output read failed")?;
                        if n == 0 { stdout = None; }
                        else {
                            if out.len() + err.len() + n > PROBE_LIMIT { return Err("OpenCode syntax probe exceeded 64 KiB"); }
                            out.extend_from_slice(&out_buf[..n]);
                        }
                    }
                    result = async { stderr.as_mut().expect("guarded stderr").read(&mut err_buf).await }, if stderr.is_some() => {
                        let n = result.map_err(|_| "OpenCode syntax probe output read failed")?;
                        if n == 0 { stderr = None; }
                        else {
                            if out.len() + err.len() + n > PROBE_LIMIT { return Err("OpenCode syntax probe exceeded 64 KiB"); }
                            err.extend_from_slice(&err_buf[..n]);
                        }
                    }
                }
            }
            if !child.exit.wait().await.success() {
                return Err("OpenCode syntax probe exited unsuccessfully");
            }
            // Never include raw child output in diagnostics: help/errors can contain secrets.
            let bytes = if out.is_empty() { err } else { out };
            String::from_utf8(bytes).map_err(|_| "OpenCode syntax probe output is not UTF-8")
        };
        let result = tokio::select! {
            result = tokio::time::timeout(PROBE_DEADLINE, collect) => result.unwrap_or(Err("OpenCode syntax probe exceeded 2 s")),
            _ = params.cancel.cancelled() => Err("OpenCode syntax probe cancelled"),
        };
        if result.is_err() {
            // The process manager signals the entire group, even when the parent
            // has exited while a descendant holds a pipe. Its background waiter
            // reaps the child; await that acknowledgement within the kill grace.
            self.process_manager
                .kill(child.pid, orch_core::Signal::Kill);
            if tokio::time::timeout(DEFAULT_KILL_GRACE, child.exit.wait())
                .await
                .is_err()
            {
                return Err("OpenCode syntax probe reap did not complete within kill grace");
            }
        }
        guard.armed = false;
        result
    }
}

impl std::fmt::Debug for OpenCodeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeAdapter").finish_non_exhaustive()
    }
}

fn build_args(params: &ExecuteParams) -> Result<Vec<String>, AdapterError> {
    let effort = super::effort::selection("opencode", &params.config)?;
    let mut args = vec!["run".to_owned(), "--format".to_owned(), "json".to_owned()];

    if let Some(model) = &effort.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    args.extend(effort.args);

    // No native `--system-prompt`; it is prepended to the user prompt.
    args.push(build_full_prompt(
        params.system_prompt.as_deref(),
        &params.prompt,
    ));
    Ok(args)
}

/// `part.tokens` with a numeric `input`. Unlike the shared `extract_tokens`
/// this one has no camelCase aliases and no cache counters.
fn extract_opencode_tokens(part: &Value) -> Option<TokenUsage> {
    let tokens = part.get("tokens")?;
    let input = number_field(tokens, "input")?;
    Some(create_token_usage(
        input,
        number_field(tokens, "output").unwrap_or(0),
        Some(TokenUsageOpts {
            reasoning: Some(number_field(tokens, "reasoning").unwrap_or(0)),
            ..Default::default()
        }),
    ))
}

fn parse_event(line: &str, clock: &dyn Clock) -> Option<AgentEvent> {
    if line.trim().is_empty() {
        return None;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        return Some(AgentEvent::new(
            AgentEventKind::Output,
            clock.now_iso(),
            Value::String(line.to_owned()),
        ));
    };

    let timestamp = clock.now_iso();
    let part = match parsed.get("part") {
        Some(part) if !part.is_null() => part.clone(),
        _ => Value::Object(Map::new()),
    };

    match parsed.get("type").and_then(Value::as_str).unwrap_or("") {
        // Lifecycle event — no user-visible content.
        "step_start" => None,

        "text" => Some(AgentEvent::new(
            AgentEventKind::Output,
            timestamp,
            match part.get("text") {
                Some(text) if !text.is_null() => text.clone(),
                _ => part,
            },
        )),

        "tool_use" => {
            let state = match part.get("state") {
                Some(state) if !state.is_null() => state.clone(),
                _ => Value::Object(Map::new()),
            };
            if state.get("status").and_then(Value::as_str) == Some("error") {
                let message = match state.get("error") {
                    Some(Value::String(error)) => error.clone(),
                    _ => stringify(&state),
                };
                return Some(error_event(timestamp, state, &message));
            }
            // The `{ name, input }` shape the TUI's `formatToolInput` expects.
            let mut data = Map::new();
            copy_present(&mut data, "name", &part, "tool");
            copy_present(&mut data, "input", &state, "input");
            Some(AgentEvent::new(
                AgentEventKind::ToolCall,
                timestamp,
                Value::Object(data),
            ))
        }

        "step_finish" => {
            let tokens = extract_opencode_tokens(&part);
            let reason = part.get("reason").and_then(Value::as_str);
            match reason {
                Some("error") => {
                    let message = match part.get("error") {
                        Some(Value::String(error)) => error.clone(),
                        _ => stringify(&part),
                    };
                    let mut event = error_event(timestamp, part, &message);
                    event.tokens = tokens;
                    Some(event)
                }
                // Intermediate lifecycle — the tool_use events carry the
                // actual content.
                Some("tool-calls") => None,
                // `stop`, `max_tokens`, anything else, or absent → done.
                _ => {
                    let mut event = AgentEvent::new(AgentEventKind::Done, timestamp, part);
                    event.tokens = tokens;
                    Some(event)
                }
            }
        }

        _ => Some(AgentEvent::new(AgentEventKind::Output, timestamp, parsed)),
    }
}

#[async_trait]
impl Adapter for OpenCodeAdapter {
    fn kind(&self) -> &'static str {
        "opencode"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "opencode").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> Result<orch_core::EffortPreparation, AdapterError> {
        let mut prepared = orch_core::EffortPreparation {
            config: params.config.clone(),
            diagnostics: Vec::new(),
        };
        let plan = plan_effort_dispatch(
            "opencode",
            params.config.model.as_deref(),
            params.config.effort,
        );
        match plan {
            EffortDispatch::Unset => return Ok(prepared),
            EffortDispatch::Reject(error) => {
                return Err(AdapterError::new(error.to_string(), None));
            }
            // Warnings only (lead ruling 20): the `request-only` note is not
            // reported at run start.
            EffortDispatch::IgnoreLegacy { diagnostics } => {
                prepared.config.effort = None;
                prepared.diagnostics = super::effort::warnings(diagnostics);
                return Ok(prepared);
            }
            EffortDispatch::Forward { diagnostics, .. } => {
                prepared.diagnostics = super::effort::warnings(diagnostics);
            }
        }
        if let Err(reason) = self.check_effort_wire(params).await {
            let level = params
                .config
                .effort
                .expect("forward has an explicit effort");
            match plan_after_wire_check_failure("opencode", params.config.model.as_deref(), level) {
                EffortDispatch::IgnoreLegacy { diagnostics } => {
                    prepared.config.effort = None;
                    prepared.diagnostics = diagnostics
                        .into_iter()
                        .filter(|d| d.is_warning())
                        .map(|d| format!("{d} ({reason})"))
                        .collect();
                }
                EffortDispatch::Reject(error) => {
                    return Err(AdapterError::new(format!("{error} ({reason})"), None));
                }
                _ => unreachable!("wire failure either ignores legacy or rejects"),
            }
        }
        Ok(prepared)
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        let args = build_args(&params)?;
        let io = spawn_child(
            &self.process_manager,
            "opencode",
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
            move |line| parse_event(line, clock.as_ref()),
            "OpenCode",
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
mod effort_tests {
    use super::*;
    use futures::StreamExt;
    use orch_core::process::{
        EnvInheritance, ExitNotifier, ExitOutcome, ExitSignal, Signal, SpawnedChild,
    };
    use orch_domain::{AgentConfig, ReasoningEffort as E};
    use std::sync::Mutex;

    #[derive(Default)]
    struct ProbeProcess {
        calls: Mutex<Vec<(Vec<String>, SpawnOptions)>>,
        failure: Option<&'static str>,
        exits: Mutex<Vec<(ExitNotifier, ExitSignal)>>,
        killed: Mutex<Vec<u32>>,
    }

    #[async_trait]
    impl ProcessManager for ProbeProcess {
        fn is_alive(&self, _: u32) -> bool {
            true
        }
        fn kill(&self, pid: u32, _: Signal) {
            self.killed.lock().unwrap().push(pid);
            for (notifier, _) in self.exits.lock().unwrap().iter() {
                notifier.notify(ExitOutcome {
                    code: None,
                    signal: Some(9),
                });
            }
        }
        async fn kill_with_grace(&self, pid: u32, _: std::time::Duration) {
            self.kill(pid, Signal::Kill);
        }
        fn spawn(
            &self,
            _: &str,
            args: &[String],
            opts: SpawnOptions,
        ) -> orch_core::Result<SpawnedChild> {
            self.calls.lock().unwrap().push((args.to_vec(), opts));
            if self.failure == Some("spawn") {
                return Err(orch_core::Error::other("secret spawn details"));
            }
            let text = if args == ["--version"] {
                match self.failure {
                    Some("v1") => "1.9.0",
                    Some("v3") => "3.0.0",
                    Some("malformed") => "2.not.a.version",
                    _ => "opencode v2.1.2",
                }
            } else if self.failure == Some("signature") {
                "--model provider/model"
            } else {
                "  --model, -m string   Model in the format provider/model#variant"
            };
            let (out, err) = match self.failure {
                Some("overflow") => (vec![b'x'; PROBE_LIMIT / 2 + 1], vec![b'y'; PROBE_LIMIT / 2]),
                Some("limit") => {
                    let mut bytes = text.as_bytes().to_vec();
                    bytes.resize(PROBE_LIMIT, b' ');
                    (bytes, Vec::new())
                }
                Some("stderr") => (Vec::new(), text.as_bytes().to_vec()),
                Some("utf8") => (vec![0xff], Vec::new()),
                _ => (text.as_bytes().to_vec(), Vec::new()),
            };
            let exit = if self.failure == Some("exit-timeout") {
                let (notifier, exit) = ExitSignal::channel();
                self.exits.lock().unwrap().push((notifier, exit.clone()));
                exit
            } else {
                ExitSignal::resolved(ExitOutcome {
                    code: Some(if self.failure == Some("nonzero") {
                        1
                    } else {
                        0
                    }),
                    signal: None,
                })
            };
            let stdout: orch_core::process::ChildReader = if self.failure == Some("pipe-timeout") {
                Box::new(PendingReader)
            } else {
                Box::new(std::io::Cursor::new(out))
            };
            Ok(SpawnedChild {
                pid: if args.iter().any(|a| a == "--format") {
                    456
                } else {
                    123
                },
                stdout: Some(stdout),
                stderr: Some(Box::new(std::io::Cursor::new(err))),
                stdin: None,
                exit,
            })
        }
    }

    struct PendingReader;
    impl tokio::io::AsyncRead for PendingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    fn params(effort: Option<E>) -> ExecuteParams {
        ExecuteParams {
            prompt: "task".into(),
            system_prompt: Some("system".into()),
            workspace: "/fake/workspace".into(),
            env: [("TEST_VALUE".into(), "private-value".into())].into(),
            inherit_env: EnvInheritance::WithoutSecrets,
            config: AgentConfig {
                model: Some("openrouter/anthropic/claude-opus-5.5".into()),
                effort,
                ..Default::default()
            },
            cancel: Default::default(),
        }
    }

    #[tokio::test]
    async fn unset_effort_does_no_io_and_preserves_opaque_model() {
        let pm = Arc::new(ProbeProcess::default());
        let mut p = params(None);
        p.config.model = Some("custom/model#thinking".into());
        let result = OpenCodeAdapter::new(pm.clone())
            .prepare_effort(&p)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result.config).unwrap(),
            serde_json::to_value(p.config).unwrap()
        );
        assert!(result.diagnostics.is_empty());
        assert!(pm.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn successful_probe_preserves_config_and_emits_one_suffix_with_execution_environment() {
        for failure in [None, Some("stderr"), Some("limit")] {
            let pm = Arc::new(ProbeProcess {
                failure,
                ..Default::default()
            });
            let adapter = OpenCodeAdapter::new(pm.clone());
            let mut p = params(Some(E::Xhigh));
            let authored = serde_json::to_value(&p.config).unwrap();
            let prepared = adapter.prepare_effort(&p).await.unwrap();
            assert_eq!(serde_json::to_value(&prepared.config).unwrap(), authored);
            assert!(
                prepared
                    .diagnostics
                    .iter()
                    .any(|d| d.starts_with("local-override-possible"))
            );
            p.config = prepared.config;
            let argv = build_args(&p).unwrap();
            assert_eq!(
                argv,
                [
                    "run",
                    "--format",
                    "json",
                    "--model",
                    "openrouter/anthropic/claude-opus-5.5#xhigh",
                    "system\n\ntask"
                ]
            );
            let handle = adapter.execute(p.clone()).unwrap();
            assert_eq!(
                handle.pid, 456,
                "execute returns the model PID, never the probe PID"
            );
            let events = handle.events.collect::<Vec<_>>().await;
            assert!(
                events.into_iter().all(|event| {
                    let event = event.unwrap();
                    !event.data["text"].as_str().is_some_and(|text| {
                        text.starts_with("request-only:")
                            || text.starts_with("local-override-possible:")
                    })
                }),
                "preparation diagnostics must not be re-emitted by execute"
            );
            let calls = pm.calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[2].0, argv);
            assert_eq!(calls[0].0, ["--version"]);
            assert_eq!(calls[1].0, ["run", "--help"]);
            for (_, opts) in calls.iter() {
                assert_eq!(opts.cwd, p.workspace);
                assert_eq!(opts.env, p.env);
                assert_eq!(opts.inherit_env, EnvInheritance::WithoutSecrets);
                assert_eq!(opts.stdin, StdioMode::Ignore);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn failed_probes_ignore_legacy_but_reject_new_without_exposing_output() {
        for failure in [
            "spawn",
            "v1",
            "v3",
            "malformed",
            "signature",
            "overflow",
            "nonzero",
            "utf8",
            "pipe-timeout",
            "exit-timeout",
        ] {
            for level in [E::High, E::Xhigh] {
                let pm = Arc::new(ProbeProcess {
                    failure: Some(failure),
                    ..Default::default()
                });
                let p = params(Some(level));
                let started = tokio::time::Instant::now();
                let result = OpenCodeAdapter::new(pm.clone()).prepare_effort(&p).await;
                if ["pipe-timeout", "exit-timeout"].contains(&failure) {
                    assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
                }

                let message = if level.is_legacy() {
                    let result = result.unwrap();
                    assert_eq!(result.config.model, p.config.model, "{failure}");
                    assert_eq!(result.config.effort, None);
                    assert_eq!(p.config.effort, Some(level));
                    result.diagnostics.join("\n")
                } else {
                    result.unwrap_err().to_string()
                };
                assert!(!message.contains("private-value"));
                assert!(!message.contains("secret spawn details"));
                if [
                    "overflow",
                    "nonzero",
                    "utf8",
                    "pipe-timeout",
                    "exit-timeout",
                ]
                .contains(&failure)
                {
                    assert_eq!(*pm.killed.lock().unwrap(), [123]);
                    assert!(
                        pm.exits
                            .lock()
                            .unwrap()
                            .iter()
                            .all(|(_, exit)| exit.try_get().is_some())
                    );
                }
                assert!(
                    pm.calls
                        .lock()
                        .unwrap()
                        .iter()
                        .all(|(args, _)| args == &["--version"] || args == &["run", "--help"])
                );
            }
        }
    }

    #[tokio::test]
    async fn suffix_conflicts_never_probe_and_legacy_keeps_original_model() {
        let pm = Arc::new(ProbeProcess::default());
        let adapter = OpenCodeAdapter::new(pm.clone());
        let mut p = params(Some(E::Xhigh));
        p.config.model = Some("openrouter/anthropic/claude-opus-5.5#max".into());
        assert!(adapter.prepare_effort(&p).await.is_err());
        p.config.effort = Some(E::High);
        let result = adapter.prepare_effort(&p).await.unwrap();
        assert_eq!(result.config.model, p.config.model);
        assert_eq!(result.config.effort, None);
        assert!(result.diagnostics[0].starts_with("legacy-ignored"));
        assert!(pm.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn matching_suffix_is_forwarded_once() {
        let pm = Arc::new(ProbeProcess::default());
        let mut p = params(Some(E::High));
        p.config.model = Some("openrouter/anthropic/claude-opus-5.5#high".into());
        OpenCodeAdapter::new(pm).prepare_effort(&p).await.unwrap();
        assert_eq!(
            build_args(&p)
                .unwrap()
                .iter()
                .filter(|s| s.contains("#high"))
                .count(),
            1
        );
        assert!(!build_args(&p).unwrap().join(" ").contains("#high#"));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_probe_is_killed_and_reaped() {
        let pm = Arc::new(ProbeProcess {
            failure: Some("exit-timeout"),
            ..Default::default()
        });
        let p = params(Some(E::Max));
        p.cancel.cancel();
        assert!(
            OpenCodeAdapter::new(pm.clone())
                .prepare_effort(&p)
                .await
                .is_err()
        );
        assert_eq!(*pm.killed.lock().unwrap(), [123]);
        assert!(
            pm.exits
                .lock()
                .unwrap()
                .iter()
                .all(|(_, exit)| exit.try_get().is_some())
        );
    }
    #[tokio::test]
    async fn dropped_preparation_kills_the_probe_group() {
        let pm = Arc::new(ProbeProcess {
            failure: Some("exit-timeout"),
            ..Default::default()
        });
        let adapter = OpenCodeAdapter::new(pm.clone());
        let p = params(Some(E::Max));
        let mut preparation = Box::pin(adapter.prepare_effort(&p));
        assert!(futures::poll!(&mut preparation).is_pending());
        drop(preparation);
        assert_eq!(*pm.killed.lock().unwrap(), [123]);
        assert!(
            pm.exits
                .lock()
                .unwrap()
                .iter()
                .all(|(_, exit)| exit.try_get().is_some())
        );
    }
}
