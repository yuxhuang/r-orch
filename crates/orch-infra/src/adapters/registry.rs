//! Registry construction and the registration order.

use std::sync::Arc;

use orch_core::adapter::AdapterRegistry;
use orch_core::clock::Clock;
use orch_core::process::ProcessManager;

use super::util::system_clock;
use super::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};

/// Registration order, which is also `listKinds()` order and the `doctor`
/// probe order. The order is exact and observable — note it differs from
/// [`orch_domain::SUPPORTED_ADAPTERS`], which is the display order.
pub const REGISTRATION_ORDER: [&str; 8] = [
    "claude",
    "codex",
    "cursor",
    "shell",
    "opencode",
    "pi",
    "grok",
    "antigravity",
];

/// Every adapter, registered in [`REGISTRATION_ORDER`], on the system clock.
pub fn build_registry(process_manager: Arc<dyn ProcessManager>) -> AdapterRegistry {
    build_registry_with_clock(process_manager, system_clock())
}

/// Every adapter, registered in [`REGISTRATION_ORDER`], minting its timestamps
/// from `clock`.
///
/// Adapters stamp the `AgentEvent`s they parse out of a child's stdout, so
/// without this the container could inject a clock everywhere *except* the one
/// place a test reads event timestamps from. Each adapter already
/// takes `with_clock`; what this adds is a single place that applies it to all
/// eight, so a caller wanting the seam does not have to re-spell the
/// registration order — which is observable (`doctor` probes in it) and
/// would silently change if a second copy of it drifted.
pub fn build_registry_with_clock(
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
) -> AdapterRegistry {
    let pm = || Arc::clone(&process_manager);
    let tick = || Arc::clone(&clock);

    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ClaudeAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(CodexAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(CursorAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(ShellAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(OpenCodeAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(PiAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(GrokAdapter::new(pm()).with_clock(tick())));
    registry.register(Arc::new(
        AntigravityAdapter::new(process_manager).with_clock(clock),
    ));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::FakeProcessManager;

    fn registry() -> AdapterRegistry {
        build_registry(Arc::new(FakeProcessManager::new()))
    }

    #[test]
    fn registers_every_adapter_in_the_contract_order() {
        assert_eq!(registry().kinds(), REGISTRATION_ORDER.to_vec());
    }

    #[test]
    fn every_registered_adapter_reports_its_own_kind() {
        for (kind, adapter) in REGISTRATION_ORDER.into_iter().zip(registry().list()) {
            assert_eq!(adapter.kind(), kind);
        }
    }

    #[test]
    fn covers_every_supported_adapter_kind() {
        let registry = registry();
        for adapter in orch_domain::SUPPORTED_ADAPTERS {
            assert!(
                registry.has(adapter.as_str()),
                "{adapter} is not registered"
            );
        }
        assert_eq!(registry.len(), orch_domain::SUPPORTED_ADAPTERS.len());
    }

    #[tokio::test]
    async fn the_injected_clock_reaches_the_events_an_adapter_mints() {
        use futures::stream::StreamExt;
        use orch_core::adapter::ExecuteParams;
        use orch_core::test_support::{FIXED_NOW_ISO, FixedClock};

        // The clock is only observable through a minted event, so the test
        // scripts a stdout line and reads the timestamp back off it. Without
        // this, `build_registry_with_clock` could accept a clock and quietly
        // drop it for some of the eight and nothing would notice.
        let processes = Arc::new(FakeProcessManager::new().with_stdout("hello from the child\n"));
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new());
        let registry = build_registry_with_clock(processes, clock);

        let adapter = registry.require("shell").expect("shell is registered");
        let handle = adapter
            .execute(ExecuteParams {
                prompt: "do the thing".to_owned(),
                system_prompt: None,
                workspace: std::path::PathBuf::from("."),
                env: Default::default(),
                inherit_env: Default::default(),
                config: orch_domain::AgentConfig {
                    command: Some("echo hello".to_owned()),
                    ..Default::default()
                },
                cancel: tokio_util::sync::CancellationToken::new(),
            })
            .expect("execute");

        let stamps: Vec<String> = handle
            .events
            .filter_map(|event| async move { event.ok().map(|event| event.timestamp) })
            .collect()
            .await;

        assert!(!stamps.is_empty(), "the adapter minted no events");
        for stamp in &stamps {
            assert_eq!(
                stamp, FIXED_NOW_ISO,
                "an adapter event carries a timestamp the injected clock did not mint"
            );
        }
    }

    #[test]
    fn an_unknown_kind_lists_the_available_ones() {
        let err = registry()
            .require("nope")
            .err()
            .expect("an unknown kind is an error");
        assert_eq!(
            err.to_string(),
            "Unknown adapter: \"nope\". Available: claude, codex, cursor, shell, opencode, pi, grok, antigravity"
        );
    }
}
