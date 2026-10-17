//! Helpers shared by the dashboard tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_infra::models::{CommandOutput, DiscoveryRuntime, ModelDiscovery};

/// A discovery runtime that execs nothing and reads nothing, so every adapter
/// falls back to its built-in model list.
///
/// The real one runs `claude --help`, `cursor-agent --list-models` and others,
/// and reads `~/.codex`. `App::tick` starts the catalog load on the first tick,
/// so any test that ticks a real `EngineBackend` would otherwise reach the
/// developer's agent CLIs (the gate's `no-real-cli` step catches that).
struct OfflineDiscovery;

#[async_trait]
impl DiscoveryRuntime for OfflineDiscovery {
    async fn run_command(&self, command: &str, _args: &[&str]) -> Result<CommandOutput> {
        Err(Error::other(format!("{command}: not run in tests")))
    }

    async fn read_text_file(&self, path: &Path) -> Result<String> {
        Err(Error::other(format!(
            "{}: not read in tests",
            path.display()
        )))
    }

    fn codex_home(&self) -> PathBuf {
        PathBuf::from("/nonexistent/orch-test-codex-home")
    }
}

pub fn offline_model_discovery() -> ModelDiscovery {
    ModelDiscovery::with_runtime(Arc::new(OfflineDiscovery))
}
