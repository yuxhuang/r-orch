//! Native selectors come only from the domain dispatch policy.

use futures::StreamExt;
use orch_core::{AdapterError, AgentEvent, AgentEventKind, Clock, ExecuteHandle};
use orch_domain::{AgentConfig, EffortDispatch, EffortWire, plan_effort_dispatch};

pub(super) struct Selection {
    pub model: Option<String>,
    pub args: Vec<String>,
    pub diagnostics: Vec<String>,
}

pub(super) fn selection(adapter: &str, config: &AgentConfig) -> Result<Selection, AdapterError> {
    let mut selected = Selection {
        model: config.model.clone(),
        args: Vec::new(),
        diagnostics: Vec::new(),
    };
    let diagnostics = match plan_effort_dispatch(adapter, config.model.as_deref(), config.effort) {
        EffortDispatch::Unset => return Ok(selected),
        EffortDispatch::Reject(error) => return Err(AdapterError::new(error.to_string(), None)),
        EffortDispatch::IgnoreLegacy { diagnostics } => diagnostics,
        EffortDispatch::Forward {
            wire, diagnostics, ..
        } => {
            match wire {
                EffortWire::Flag { flag, level } => {
                    selected.args = vec![flag.to_owned(), level.as_str().to_owned()];
                }
                EffortWire::ConfigOverride { key, level } => {
                    selected.args = vec!["-c".to_owned(), format!("{key}=\"{}\"", level.as_str())];
                }
                EffortWire::ModelVariant { model } => selected.model = Some(model),
            }
            diagnostics
        }
    };
    selected.diagnostics = diagnostics.into_iter().map(|d| d.to_string()).collect();
    Ok(selected)
}

pub(super) fn with_diagnostics(
    handle: ExecuteHandle,
    diagnostics: Vec<String>,
    clock: &dyn Clock,
) -> ExecuteHandle {
    if diagnostics.is_empty() {
        return handle;
    }
    let timestamp = clock.now_iso();
    let notes = diagnostics.into_iter().map(move |text| {
        Ok(AgentEvent::new(
            AgentEventKind::Output,
            timestamp.clone(),
            serde_json::json!({ "text": text }),
        ))
    });
    ExecuteHandle {
        pid: handle.pid,
        events: Box::pin(futures::stream::iter(notes).chain(handle.events)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                let result = registry.get(kind).unwrap().execute(params(config));
                match planned {
                    EffortDispatch::Reject(_) => {
                        assert!(result.is_err(), "{kind} {level}");
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
                        let mut events = handle.events;
                        let first = events.next().await.unwrap().unwrap();
                        assert!(
                            first.data["text"]
                                .as_str()
                                .unwrap()
                                .starts_with("request-only")
                        );
                    }
                    EffortDispatch::IgnoreLegacy { .. } => {
                        let mut events = result.unwrap().events;
                        assert!(
                            events.next().await.unwrap().unwrap().data["text"]
                                .as_str()
                                .unwrap()
                                .starts_with("legacy-ignored")
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
