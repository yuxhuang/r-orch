//! Registry construction — the registration order from `container.ts:201-208`.

use std::sync::Arc;

use orch_core::adapter::AdapterRegistry;
use orch_core::process::ProcessManager;

use super::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};

/// Registration order, which is also `listKinds()` order and the `doctor`
/// probe order. **L0** (PARITY.md §8) — note it differs from
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

/// Every adapter, registered in [`REGISTRATION_ORDER`].
pub fn build_registry(process_manager: Arc<dyn ProcessManager>) -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ClaudeAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(CodexAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(CursorAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(ShellAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(OpenCodeAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(PiAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(GrokAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(AntigravityAdapter::new(process_manager)));
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
