//! Runtime model discovery, ported from
//! `src/infrastructure/models/model-discovery.ts` (PARITY.md §8.12).
//!
//! Prefer the catalogue the installed CLI exposes; when a CLI has no listing
//! command (Codex and Claude) use its own local cache or stable aliases. The
//! fallbacks deliberately avoid pinned model versions so they cannot go stale.
//!
//! There is **no in-process cache** — every call re-probes
//! (`docs/parity/adapters.md` §7.3). This feeds the TUI model picker only;
//! nothing in the engine depends on it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use indexmap::IndexMap;
use orch_core::error::{Error, Result};
use orch_domain::AdapterKind;
use serde::{Deserialize, Serialize};

use super::parsers::{
    parse_claude_model_aliases, parse_codex_models_cache, parse_cursor_models, parse_grok_models,
    parse_line_models, parse_pi_models, with_default,
};

const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(15_000);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ModelOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>, hint: impl Into<String>) -> Self {
        ModelOption {
            value: value.into(),
            label: label.into(),
            hint: Some(hint.into()),
        }
    }
}

/// Discovered options per adapter, in the order the adapters were requested.
pub type ModelCatalog = IndexMap<AdapterKind, Vec<ModelOption>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
}

/// The seams `discoverModelOptions`'s `ModelDiscoveryRuntime` parameter
/// provides: a command runner, a file reader and the Codex home.
#[async_trait]
pub trait DiscoveryRuntime: Send + Sync {
    async fn run_command(&self, command: &str, args: &[&str]) -> Result<CommandOutput>;
    async fn read_text_file(&self, path: &Path) -> Result<String>;
    /// `$CODEX_HOME`, else `~/.codex`.
    fn codex_home(&self) -> PathBuf;
}

/// Probes the real CLIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDiscoveryRuntime;

#[async_trait]
impl DiscoveryRuntime for SystemDiscoveryRuntime {
    /// Mirrors the TS `run()`: pipe both streams, 15 s timeout, and a non-zero
    /// exit is a failure carrying the signal-or-code and stderr.
    async fn run_command(&self, command: &str, args: &[&str]) -> Result<CommandOutput> {
        let mut cmd = tokio::process::Command::new(command);
        // The TS kills with SIGTERM on timeout and leaves the child behind;
        // dropping the handle here SIGKILLs and reaps it instead. Both end the
        // probe with the same error, and neither outcome is observable beyond
        // this function.
        cmd.args(args).kill_on_drop(true);

        let described = || format!("{command} {}", args.join(" "));
        let output = match tokio::time::timeout(DISCOVERY_TIMEOUT, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(source)) => {
                return Err(Error::Io {
                    context: format!("Failed to spawn process: {command}"),
                    path: Some(PathBuf::from(command)),
                    source,
                });
            }
            Err(_) => return Err(Error::other(format!("{} timed out", described()))),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            let reason = exit_reason(&output.status);
            let suffix = if stderr.is_empty() {
                String::new()
            } else {
                format!(" {stderr}")
            };
            return Err(Error::other(format!(
                "{} failed: {reason}{suffix}",
                described()
            )));
        }
        Ok(CommandOutput { stdout, stderr })
    }

    async fn read_text_file(&self, path: &Path) -> Result<String> {
        tokio::fs::read_to_string(path)
            .await
            .map_err(|source| Error::io(format!("Failed to read {}", path.display()), path, source))
    }

    fn codex_home(&self) -> PathBuf {
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return PathBuf::from(home);
        }
        dirs::home_dir().unwrap_or_default().join(".codex")
    }
}

/// `${signal ?? code}` — node reports a signal by name and otherwise the code.
fn exit_reason(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(signal) => nix::sys::signal::Signal::try_from(signal)
            .map(|signal| signal.as_str().to_owned())
            .unwrap_or_else(|_| signal.to_string()),
        None => status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_owned()),
    }
}

pub struct ModelDiscovery {
    runtime: Arc<dyn DiscoveryRuntime>,
}

impl Default for ModelDiscovery {
    fn default() -> Self {
        ModelDiscovery::new()
    }
}

impl std::fmt::Debug for ModelDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelDiscovery").finish_non_exhaustive()
    }
}

impl ModelDiscovery {
    pub fn new() -> Self {
        ModelDiscovery {
            runtime: Arc::new(SystemDiscoveryRuntime),
        }
    }

    pub fn with_runtime(runtime: Arc<dyn DiscoveryRuntime>) -> Self {
        ModelDiscovery { runtime }
    }

    /// Probe one adapter. **Every failure is an empty list**, as the TS
    /// `try { … } catch { return [] }` is — the caller substitutes the
    /// fallbacks.
    pub async fn discover(&self, adapter: AdapterKind) -> Vec<ModelOption> {
        match self.discover_inner(adapter).await {
            Ok(options) => options,
            Err(err) => {
                tracing::debug!(%adapter, error = %err, "model discovery failed");
                Vec::new()
            }
        }
    }

    async fn discover_inner(&self, adapter: AdapterKind) -> Result<Vec<ModelOption>> {
        match adapter {
            AdapterKind::Claude => {
                self.probe(
                    "claude",
                    &["--help"],
                    parse_claude_model_aliases,
                    "use Claude configured default",
                    false,
                )
                .await
            }
            AdapterKind::Cursor => {
                self.probe(
                    "cursor-agent",
                    &["--list-models"],
                    parse_cursor_models,
                    "use Cursor configured default",
                    false,
                )
                .await
            }
            AdapterKind::Grok => {
                self.probe(
                    "grok",
                    &["models"],
                    parse_grok_models,
                    "use Grok configured default",
                    false,
                )
                .await
            }
            AdapterKind::Antigravity => {
                self.probe(
                    "agy",
                    &["models"],
                    parse_runtime_line_models,
                    "use Antigravity configured default",
                    false,
                )
                .await
            }
            AdapterKind::Opencode => {
                self.probe(
                    "opencode",
                    &["models"],
                    parse_runtime_line_models,
                    "use model configured in OpenCode",
                    false,
                )
                .await
            }
            // Pi prints its table on stderr when it has no credentials, so
            // both streams are parsed.
            AdapterKind::Pi => {
                self.probe(
                    "pi",
                    &["--list-models"],
                    parse_pi_models,
                    "use Pi configured default",
                    true,
                )
                .await
            }
            AdapterKind::Codex => {
                let path = self.runtime.codex_home().join("models_cache.json");
                let cache = self.runtime.read_text_file(&path).await?;
                Ok(with_default(
                    parse_codex_models_cache(&cache),
                    "use Codex configured default",
                ))
            }
            AdapterKind::Shell => Ok(fallback_model_options("shell")),
        }
    }

    async fn probe(
        &self,
        command: &str,
        args: &[&str],
        parse: fn(&str) -> Vec<ModelOption>,
        default_hint: &str,
        include_stderr: bool,
    ) -> Result<Vec<ModelOption>> {
        let CommandOutput { stdout, stderr } = self.runtime.run_command(command, args).await?;
        let output = if include_stderr {
            format!("{stdout}\n{stderr}")
        } else if stdout.is_empty() {
            stderr
        } else {
            stdout
        };
        Ok(with_default(parse(&output), default_hint))
    }

    /// Probe several adapters in parallel, reporting each as it resolves.
    ///
    /// The returned catalogue is in the requested order; `on_update` fires in
    /// completion order, which is what makes the TUI picker fill in
    /// progressively.
    pub async fn load_catalog(
        &self,
        adapters: &[AdapterKind],
        mut on_update: impl FnMut(AdapterKind, &[ModelOption]),
    ) -> ModelCatalog {
        let mut pending: FuturesUnordered<_> = adapters
            .iter()
            .enumerate()
            .map(|(index, adapter)| async move {
                let discovered = self.discover(*adapter).await;
                let options = if discovered.is_empty() {
                    fallback_model_options(adapter.as_str())
                } else {
                    discovered
                };
                (index, *adapter, options)
            })
            .collect();

        let mut resolved: BTreeMap<usize, (AdapterKind, Vec<ModelOption>)> = BTreeMap::new();
        while let Some((index, adapter, options)) = pending.next().await {
            on_update(adapter, &options);
            resolved.insert(index, (adapter, options));
        }
        resolved.into_values().collect()
    }
}

fn parse_runtime_line_models(output: &str) -> Vec<ModelOption> {
    parse_line_models(output, "runtime")
}

/// The hard-coded fallbacks, used when a probe fails. Deliberately
/// version-free so they cannot go stale.
pub fn fallback_model_options(adapter: &str) -> Vec<ModelOption> {
    let hint = match adapter {
        "claude" => {
            return vec![
                ModelOption::new("", "Default", "use Claude configured default"),
                ModelOption::new("opus", "Opus", "latest Opus alias"),
                ModelOption::new("sonnet", "Sonnet", "latest Sonnet alias"),
            ];
        }
        "cursor" => {
            return vec![
                ModelOption::new("", "Default", "use Cursor configured default"),
                ModelOption::new("auto", "Auto", "let Cursor choose"),
            ];
        }
        "codex" => "use Codex configured default",
        "opencode" => "use model configured in OpenCode",
        "pi" => "use Pi configured default; run /login in Pi if unavailable",
        "grok" => "use Grok configured default",
        "antigravity" => "use Antigravity configured default",
        "shell" => "model is not used by the shell adapter",
        // `isAdapterKind(adapter)` is false for anything else.
        _ => "use adapter default",
    };
    vec![ModelOption::new("", "Default", hint)]
}

/// The full fallback table, in [`orch_domain::SUPPORTED_ADAPTERS`] order.
pub fn fallback_model_table() -> IndexMap<AdapterKind, Vec<ModelOption>> {
    orch_domain::SUPPORTED_ADAPTERS
        .into_iter()
        .map(|adapter| (adapter, fallback_model_options(adapter.as_str())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubRuntime {
        outputs: BTreeMap<String, CommandOutput>,
        codex_cache: Option<String>,
        pub commands: Mutex<Vec<String>>,
    }

    impl StubRuntime {
        fn with_outputs(pairs: &[(&str, &str)]) -> Self {
            StubRuntime {
                outputs: pairs
                    .iter()
                    .map(|(key, stdout)| {
                        (
                            (*key).to_owned(),
                            CommandOutput {
                                stdout: (*stdout).to_owned(),
                                stderr: String::new(),
                            },
                        )
                    })
                    .collect(),
                codex_cache: Some(
                    serde_json::json!({
                        "models": [
                            { "slug": "codex-current", "display_name": "Codex Current", "visibility": "list" }
                        ]
                    })
                    .to_string(),
                ),
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl DiscoveryRuntime for StubRuntime {
        async fn run_command(&self, command: &str, args: &[&str]) -> Result<CommandOutput> {
            let key = format!("{command} {}", args.join(" "));
            self.commands
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(key.clone());
            self.outputs
                .get(&key)
                .cloned()
                .ok_or_else(|| Error::other("unexpected command"))
        }

        async fn read_text_file(&self, _path: &Path) -> Result<String> {
            self.codex_cache
                .clone()
                .ok_or_else(|| Error::other("no cache"))
        }

        fn codex_home(&self) -> PathBuf {
            PathBuf::from("/test/codex")
        }
    }

    fn stub() -> Arc<StubRuntime> {
        Arc::new(StubRuntime::with_outputs(&[
            (
                "claude --help",
                "--model <model>  Use 'opus' or 'sonnet'.\n  --name <name>  Name",
            ),
            (
                "cursor-agent --list-models",
                "Available models\ncurrent - Current Model (default)",
            ),
            ("grok models", "Available models:\n  * grok-current (default)"),
            ("agy models", "antigravity-current"),
            ("opencode models", "provider/opencode-current"),
            (
                "pi --list-models",
                "provider  model       context  max-out  thinking  images\nopenai   pi-current  1M       64K      yes       yes",
            ),
        ]))
    }

    #[tokio::test]
    async fn discovers_models_for_every_adapter() {
        let discovery = ModelDiscovery::with_runtime(stub());
        for (adapter, expected) in [
            (AdapterKind::Claude, "opus"),
            (AdapterKind::Codex, "codex-current"),
            (AdapterKind::Cursor, "current"),
            (AdapterKind::Opencode, "provider/opencode-current"),
            (AdapterKind::Pi, "openai/pi-current"),
            (AdapterKind::Grok, "grok-current"),
            (AdapterKind::Antigravity, "antigravity-current"),
            (AdapterKind::Shell, ""),
        ] {
            let options = discovery.discover(adapter).await;
            assert!(
                options.iter().any(|option| option.value == expected),
                "{adapter} should offer {expected:?}, got {options:?}"
            );
        }
    }

    #[tokio::test]
    async fn every_probe_gets_a_default_row_prepended() {
        let discovery = ModelDiscovery::with_runtime(stub());
        let options = discovery.discover(AdapterKind::Grok).await;
        assert_eq!(options[0].value, "");
        assert_eq!(options[0].label, "Default");
        assert_eq!(
            options[0].hint.as_deref(),
            Some("use Grok configured default")
        );
    }

    #[tokio::test]
    async fn a_failed_probe_yields_nothing_and_the_caller_falls_back() {
        let discovery = ModelDiscovery::with_runtime(Arc::new(StubRuntime::default()));
        assert!(discovery.discover(AdapterKind::Cursor).await.is_empty());

        assert_eq!(
            fallback_model_options("cursor")
                .iter()
                .map(|option| option.value.clone())
                .collect::<Vec<_>>(),
            ["", "auto"]
        );
        assert_eq!(
            fallback_model_options("codex")
                .iter()
                .map(|option| option.value.clone())
                .collect::<Vec<_>>(),
            [""]
        );
    }

    #[tokio::test]
    async fn pi_is_the_only_probe_that_reads_stderr() {
        struct StderrOnly;

        #[async_trait]
        impl DiscoveryRuntime for StderrOnly {
            async fn run_command(&self, _command: &str, _args: &[&str]) -> Result<CommandOutput> {
                Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: "provider  model  context\nopenai  pi-stderr  1M".to_owned(),
                })
            }
            async fn read_text_file(&self, _path: &Path) -> Result<String> {
                Err(Error::other("unused"))
            }
            fn codex_home(&self) -> PathBuf {
                PathBuf::new()
            }
        }

        let discovery = ModelDiscovery::with_runtime(Arc::new(StderrOnly));
        let options = discovery.discover(AdapterKind::Pi).await;
        assert!(options.iter().any(|option| option.value == "openai/pi-stderr"));
    }

    #[tokio::test]
    async fn the_catalog_streams_each_entry_and_keeps_the_requested_order() {
        let discovery = ModelDiscovery::with_runtime(stub());
        let mut updates = Vec::new();
        let catalog = discovery
            .load_catalog(&[AdapterKind::Shell, AdapterKind::Grok], |adapter, _| {
                updates.push(adapter)
            })
            .await;

        assert_eq!(updates.len(), 2);
        assert!(updates.contains(&AdapterKind::Shell));
        assert_eq!(
            catalog.keys().copied().collect::<Vec<_>>(),
            [AdapterKind::Shell, AdapterKind::Grok]
        );
        assert_eq!(catalog[&AdapterKind::Shell], fallback_model_options("shell"));
    }

    #[tokio::test]
    async fn the_catalog_substitutes_fallbacks_when_a_probe_fails() {
        let discovery = ModelDiscovery::with_runtime(Arc::new(StubRuntime::default()));
        let catalog = discovery
            .load_catalog(&[AdapterKind::Claude], |_, _| {})
            .await;
        assert_eq!(catalog[&AdapterKind::Claude], fallback_model_options("claude"));
    }

    #[test]
    fn the_fallback_table_covers_every_supported_adapter() {
        let table = fallback_model_table();
        assert_eq!(table.len(), orch_domain::SUPPORTED_ADAPTERS.len());
        for (adapter, options) in &table {
            assert!(
                options.iter().any(|option| option.value.is_empty()),
                "{adapter} needs a default row"
            );
        }
        assert_eq!(
            table.keys().copied().collect::<Vec<_>>(),
            orch_domain::SUPPORTED_ADAPTERS.to_vec()
        );
    }

    #[test]
    fn an_unknown_adapter_gets_the_generic_default() {
        let options = fallback_model_options("nope");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].hint.as_deref(), Some("use adapter default"));
    }

    #[tokio::test]
    async fn the_real_runner_reports_a_failing_command() {
        let runtime = SystemDiscoveryRuntime;
        let err = runtime
            .run_command("sh", &["-c", "printf oops >&2; exit 2"])
            .await
            .expect_err("non-zero exit is a failure");
        assert_eq!(err.to_string(), "sh -c printf oops >&2; exit 2 failed: 2 oops");
    }

    #[tokio::test]
    async fn the_real_runner_captures_stdout() {
        let runtime = SystemDiscoveryRuntime;
        let output = runtime
            .run_command("sh", &["-c", "printf hello"])
            .await
            .expect("success");
        assert_eq!(output.stdout, "hello");
        assert_eq!(output.stderr, "");
    }

    #[tokio::test]
    async fn the_real_runner_reports_a_missing_binary() {
        let runtime = SystemDiscoveryRuntime;
        let err = runtime
            .run_command("orch-definitely-not-a-binary", &[])
            .await
            .expect_err("missing binary");
        assert_eq!(
            err.to_string(),
            "Failed to spawn process: orch-definitely-not-a-binary"
        );
    }
}
