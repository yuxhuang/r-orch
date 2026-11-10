//! Operator diagnostics, independent of tracing and dispatch mode.

use orch_domain::{EffortDiagnostic, EffortReason, EffortStatus, OrchestratorEvent};
use orch_engine::events::EventBus;
use std::io::Write as _;

/// One synchronous subscriber for the engine's lifetime: no task, lag window,
/// replay or per-dispatch registration. Detached stderr already goes to daemon.log.
pub(crate) fn attach(events: &EventBus) {
    let _ = events.on(
        orch_domain::OrchestratorEventType::OrchestratorError,
        |event| {
            let _ = writeln!(std::io::stderr().lock(), "{}", render(event));
        },
    );
}

fn render(event: &OrchestratorEvent) -> String {
    if let OrchestratorEvent::OrchestratorError { error, context, .. } = event {
        if context.starts_with("effort preparation")
            && let Some(warning) = effort_warning(error)
        {
            return format!("  ⚠ effort preparation: {warning}");
        }
        if context == "context index rebuild"
            && error == "Context index rebuild skipped: invalid context entity"
        {
            return format!("  ⚠ context index rebuild: {error}");
        }
    }
    // Error Display/context may contain config values, paths, child output or
    // environment. Never copy them into daemon.log (ADR 0012 §12). The original
    // event remains available to live logs.follow clients.
    "  ⚠ orchestrator:error (details omitted from daemon stderr; use orch logs --follow)".into()
}

/// Only domain-authored text is safe to log. OpenCode appends a probe reason
/// in parentheses; discard that suffix rather than trusting arbitrary text.
fn effort_warning(error: &str) -> Option<String> {
    use EffortReason::*;
    for reason in [
        CodexUltra,
        PiClamp,
        PiFlagOverridesModelSuffix,
        OpencodeLocalVariant,
        UnknownModel,
        DefaultModel,
        CodexManagedModelDefault,
        AntigravitySuffixedModel,
        LegacyAdapterUnsupported,
        LegacyModelRequired,
        LegacyLevelNotOnModel,
        LegacySuffixConflict,
        LegacyWireCheckFailed,
    ] {
        for status in [
            EffortStatus::RequestOnly,
            EffortStatus::ClampPossible,
            EffortStatus::LocalOverridePossible,
            EffortStatus::CapabilityUnknown,
            EffortStatus::LegacyIgnored,
        ] {
            let warning = EffortDiagnostic { status, reason }.to_string();
            if error
                .strip_prefix(&warning)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(" ("))
            {
                return Some(warning);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(context: &str, error: &str) -> OrchestratorEvent {
        OrchestratorEvent::OrchestratorError {
            context: context.into(),
            error: error.into(),
            fatal: false,
        }
    }

    #[test]
    fn canonical_effort_text_survives_without_untrusted_suffix_or_context() {
        let warning = EffortDiagnostic {
            status: EffortStatus::LegacyIgnored,
            reason: EffortReason::LegacyWireCheckFailed,
        }
        .to_string();
        let rendered = render(&error(
            "effort preparation for run secret-context",
            &format!("{warning} (SECRET=value\nforged line)"),
        ));
        assert_eq!(rendered, format!("  ⚠ effort preparation: {warning}"));
        assert_eq!(rendered.matches("legacy-ignored").count(), 1);
    }

    #[test]
    fn arbitrary_errors_and_effort_lookalikes_never_reach_stderr() {
        for event in [
            error("SECRET=context", "SECRET=error"),
            error("effort preparation", "legacy-ignored: SECRET=error"),
        ] {
            let rendered = render(&event);
            assert!(rendered.contains("orchestrator:error"));
            assert!(!rendered.contains("SECRET"));
            assert!(!rendered.contains("legacy-ignored"));
        }
    }

    #[test]
    fn stderr_subscriber_leaves_the_original_event_for_logs_follow() {
        let events = EventBus::new();
        let mut follow = events.subscribe();
        attach(&events);
        let original = error(
            "context index rebuild",
            "Context index rebuild skipped: invalid context entity",
        );
        events.emit(original.clone());
        let received = follow.try_drain();
        assert_eq!(received.len(), 1);
        assert_eq!(*received[0], original);
        assert_eq!(
            events.listener_count(orch_domain::OrchestratorEventType::OrchestratorError),
            1
        );
    }
}
