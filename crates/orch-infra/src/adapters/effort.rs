//! Native selectors come only from the domain dispatch policy.

use orch_core::AdapterError;
use orch_domain::{AgentConfig, EffortDispatch, EffortWire, plan_effort_dispatch};

pub(super) struct Selection {
    pub model: Option<String>,
    pub args: Vec<String>,
}

pub(super) fn selection(adapter: &str, config: &AgentConfig) -> Result<Selection, AdapterError> {
    let mut selected = Selection {
        model: config.model.clone(),
        args: Vec::new(),
    };
    match plan_effort_dispatch(adapter, config.model.as_deref(), config.effort) {
        EffortDispatch::Unset => return Ok(selected),
        EffortDispatch::Reject(error) => return Err(AdapterError::new(error.to_string(), None)),
        EffortDispatch::IgnoreLegacy { .. } => {}
        EffortDispatch::Forward { wire, .. } => match wire {
            EffortWire::Flag { flag, level } => {
                selected.args = vec![flag.to_owned(), level.as_str().to_owned()];
            }
            EffortWire::ConfigOverride { key, level } => {
                selected.args = vec!["-c".to_owned(), format!("{key}=\"{}\"", level.as_str())];
            }
            EffortWire::ModelVariant { model } => selected.model = Some(model),
        },
    }
    Ok(selected)
}

/// Pure pre-execution effort **warnings** for a non-OpenCode adapter's
/// resolved config: one string per [`EffortDiagnostic::is_warning`]
/// diagnostic. The informational `request-only` note is not one; it is shown
/// on `agent add`/`show`, never at run start (lead ruling 20). OpenCode
/// callers must use `EffortPreparation::diagnostics` instead, since its wire
/// check can change the dispatch outcome.
///
/// [`EffortDiagnostic::is_warning`]: orch_domain::EffortDiagnostic::is_warning
pub fn effort_diagnostics(
    adapter: &str,
    config: &AgentConfig,
) -> Result<Vec<String>, AdapterError> {
    let diagnostics = match plan_effort_dispatch(adapter, config.model.as_deref(), config.effort) {
        EffortDispatch::Unset => return Ok(Vec::new()),
        EffortDispatch::Reject(error) => return Err(AdapterError::new(error.to_string(), None)),
        EffortDispatch::IgnoreLegacy { diagnostics }
        | EffortDispatch::Forward { diagnostics, .. } => diagnostics,
    };
    Ok(warnings(diagnostics))
}

/// The run-start rendering of a dispatch plan's diagnostics: warnings only,
/// each once, in plan order.
pub(super) fn warnings(diagnostics: Vec<orch_domain::EffortDiagnostic>) -> Vec<String> {
    diagnostics
        .into_iter()
        .filter(|d| d.is_warning())
        .map(|d| d.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use orch_core::AgentEventKind;
    use orch_core::{ExecuteParams, test_support::FakeProcessManager};
    use orch_domain::{EffortDispatch, ReasoningEffort as E};
    use std::sync::Arc;

    fn params(config: AgentConfig) -> ExecuteParams {
        ExecuteParams {
            prompt: "task".into(),
            system_prompt: None,
            workspace: "/fake/workspace".into(),
            env: Default::default(),
            inherit_env: Default::default(),
            config,
            cancel: Default::default(),
        }
    }

    #[tokio::test]
    async fn legacy_claude_high_stream_contains_zero_diagnostic_output_events() {
        let pm = Arc::new(
            FakeProcessManager::new()
                .with_stdout_lines([r#"{"type":"result","result":"provider result"}"#]),
        );
        let registry = super::super::build_registry(pm.clone());
        let handle = registry
            .get("claude")
            .unwrap()
            .execute(params(AgentConfig {
                model: Some("claude-opus-5-5".into()),
                effort: Some(E::High),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(handle.pid, pm.pid());
        let events = handle.events.collect::<Vec<_>>().await;
        let events = events.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == AgentEventKind::Output)
                .count(),
            0,
            "effort diagnostics must never become provider Output: {events:?}"
        );
        assert!(events.iter().any(|e| e.kind == AgentEventKind::Done));
    }

    #[tokio::test]
    async fn all_default_preparations_are_identity_without_io_even_for_invalid_effort() {
        let pm = Arc::new(FakeProcessManager::new());
        let registry = super::super::build_registry(pm.clone());
        let p = params(AgentConfig {
            model: Some("opaque/model:high#native".into()),
            effort: Some(E::Ultra),
            ..Default::default()
        });
        for adapter in registry
            .list()
            .into_iter()
            .filter(|a| a.kind() != "opencode")
        {
            let prepared = adapter.prepare_effort(&p).await.unwrap();
            assert_eq!(
                serde_json::to_value(prepared.config).unwrap(),
                serde_json::to_value(&p.config).unwrap()
            );
            assert!(prepared.diagnostics.is_empty());
        }
        assert!(pm.spawned().is_empty());
    }

    #[tokio::test]
    async fn dispatch_matrix_rejects_before_spawn_or_emits_native_selector_exactly_once() {
        for (kind, model) in [
            ("claude", "claude-opus-5-5"),
            ("codex", "gpt-6-astra"),
            ("pi", "openai-codex/gpt-6.1-sol:high"),
            ("grok", "grok-build"),
            ("antigravity", "gemini-4-argon"),
            ("cursor", "opaque[high]"),
            ("shell", "opaque"),
        ] {
            for level in E::ALL {
                let pm = Arc::new(FakeProcessManager::new());
                let registry = super::super::build_registry(pm.clone());
                let config = AgentConfig {
                    model: Some(model.into()),
                    effort: Some(level),
                    command: Some("echo fake".into()),
                    ..Default::default()
                };
                let planned = plan_effort_dispatch(kind, Some(model), Some(level));
                let diagnostics = effort_diagnostics(kind, &config);
                assert!(pm.spawned().is_empty());
                let result = registry.get(kind).unwrap().execute(params(config));
                match planned {
                    EffortDispatch::Reject(_) => {
                        assert!(result.is_err(), "{kind} {level}");
                        assert!(diagnostics.is_err());
                        assert!(pm.spawned().is_empty());
                    }
                    EffortDispatch::Forward { wire, .. } => {
                        let handle = result.unwrap();
                        assert_eq!(handle.pid, pm.pid());
                        let args = &pm.spawned()[0].1;
                        let selector = match wire {
                            EffortWire::Flag { flag, level } => {
                                let at = args.iter().position(|a| a == flag).unwrap();
                                assert_eq!(args[at + 1], level.as_str(), "{kind} {level}");
                                flag.to_owned()
                            }
                            EffortWire::ConfigOverride { key, .. } => {
                                assert_eq!(args.iter().filter(|a| a.as_str() == "-c").count(), 1);
                                format!("{key}=\"{level}\"")
                            }
                            _ => unreachable!(),
                        };
                        assert_eq!(
                            args.iter().filter(|a| **a == selector).count(),
                            1,
                            "{kind} {level}: {args:?}"
                        );
                        let diagnostics = diagnostics.unwrap();
                        let plain_note =
                            format!("request-only: {}", orch_domain::EFFORT_REQUEST_ONLY_NOTE);
                        assert!(
                            !diagnostics.contains(&plain_note),
                            "{kind} {level}: the request-only note is not a run-start warning: {diagnostics:?}"
                        );
                        let events = handle.events.collect::<Vec<_>>().await;
                        assert!(
                            events
                                .into_iter()
                                .all(|e| e.unwrap().kind != AgentEventKind::Output)
                        );
                    }
                    EffortDispatch::IgnoreLegacy { .. } => {
                        let diagnostics = diagnostics.unwrap();
                        assert!(diagnostics.iter().any(|d| d.starts_with("legacy-ignored")));
                        let events = result.unwrap().events.collect::<Vec<_>>().await;
                        assert!(
                            events
                                .into_iter()
                                .all(|e| e.unwrap().kind != AgentEventKind::Output)
                        );
                        let args = &pm.spawned()[0].1;
                        assert!(!args.iter().any(|a| a == "--effort"
                            || a == "--thinking"
                            || a.starts_with("model_reasoning_effort=")));
                        if kind == "cursor" {
                            assert!(args.iter().any(|a| a == model));
                        }
                    }
                    EffortDispatch::Unset => unreachable!(),
                }
            }
        }
    }
}
