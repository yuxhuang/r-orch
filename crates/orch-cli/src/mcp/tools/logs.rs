//! Bounded stored-log projections, preserving CLI selector precedence.
use crate::mcp::registry::{
    CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolMeta,
};
use orch_core::{Error, Result};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogsArgs {
    run_id: Option<String>,
    agent: Option<String>,
    task: Option<String>,
    /// ASCII integer followed by s, m, h or d. Validated even when a selector takes precedence.
    since: Option<String>,
}
impl ToolArgs for LogsArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        if let Some(raw) = &self.since {
            checked_duration(raw, jiff::Timestamp::now().as_millisecond())?;
        }
        if self.run_id.is_none()
            && self.task.is_none()
            && self.agent.is_none()
            && self.since.is_none()
        {
            return Err(rmcp::ErrorData::invalid_params(
                "logs requires run_id, task, agent, or since",
                None,
            ));
        }
        Ok(())
    }
}
// The CLI parser slices a byte and multiplies unchecked. Establish both its
// UTF-8 boundary and arithmetic preconditions before ever invoking it.
fn checked_duration(raw: &str, now: i64) -> std::result::Result<i64, rmcp::ErrorData> {
    let invalid = || {
        rmcp::ErrorData::invalid_params(
            "Invalid since duration: expected ASCII digits followed by s, m, h, or d within the supported time range",
            None,
        )
    };
    if !raw.is_ascii() {
        return Err(invalid());
    }
    let (unit, digits) = raw.as_bytes().split_last().ok_or_else(invalid)?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(invalid());
    }
    let multiplier: i64 = match unit {
        b's' => 1_000,
        b'm' => 60_000,
        b'h' => 3_600_000,
        b'd' => 86_400_000,
        _ => return Err(invalid()),
    };
    let number = std::str::from_utf8(digits)
        .map_err(|_| invalid())?
        .parse::<i64>()
        .map_err(|_| invalid())?;
    let duration = number.checked_mul(multiplier).ok_or_else(invalid)?;
    now.checked_sub(duration).ok_or_else(invalid)?;
    Ok(duration)
}
pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add(ToolMeta { name: "logs", description: "Read stored events or run summaries (orch logs). Precedence: run_id > task > agent > since. No follow stream; since is validated first.", class: Class::Read, execution: Execution::Store, destructive_hint: false, idempotent_hint: true, open_world_hint: false }, logs)
}
async fn logs(cx: Arc<CallContext>, args: LogsArgs) -> Result<Value> {
    let now = cx.engine.clock.now_ms();
    let since = args
        .since
        .as_deref()
        .map(|raw| {
            checked_duration(raw, now)
                .map_err(|err| Error::invalid_arguments(err.message.to_string()))?;
            crate::logs::parse_duration(raw)
        })
        .transpose()?;
    let cutoff = since
        .map(|duration| {
            now.checked_sub(duration)
                .ok_or_else(|| Error::invalid_arguments("since exceeds the supported time range"))
        })
        .transpose()?;
    if let Some(id) = args.run_id {
        let events = cx
            .engine
            .runs
            .read_events_tail(&id, if since.is_some() { 500 } else { 50 })
            .await?;
        // CLI treats 0 as unfiltered, and retains malformed timestamps.
        let events: Vec<_> = events
            .into_iter()
            .filter(|event| {
                since == Some(0)
                    || cutoff.is_none_or(|cutoff| {
                        parse_time(&event.timestamp).is_none_or(|at| at >= cutoff)
                    })
            })
            .collect();
        return serde_json::to_value(events).map_err(|_| Error::other("serialize events"));
    }
    let runs = if let Some(id) = args.task {
        let runs = cx.engine.runs.list_for_task(&id).await?;
        if since.is_some() {
            last(runs, 20)
        } else {
            runs
        }
    } else if let Some(id) = args.agent {
        last(cx.engine.runs.list_for_agent(&id).await?, 5)
    } else if let Some(cutoff) = cutoff {
        // Match the CLI's overlap predicate: absent/bad finished_at means now.
        cx.engine
            .runs
            .list_all()
            .await?
            .into_iter()
            .filter(|run| {
                parse_time(&run.started_at).unwrap_or(now) >= cutoff
                    || run
                        .finished_at
                        .as_deref()
                        .and_then(parse_time)
                        .unwrap_or(now)
                        >= cutoff
            })
            .take(20)
            .collect()
    } else {
        return Err(Error::invalid_arguments(
            "logs requires run_id, task, agent, or since",
        ));
    };
    serde_json::to_value(runs).map_err(|_| Error::other("serialize runs"))
}
fn last<T>(items: Vec<T>, n: usize) -> Vec<T> {
    let skip = items.len().saturating_sub(n);
    items.into_iter().skip(skip).collect()
}
fn parse_time(raw: &str) -> Option<i64> {
    raw.parse::<jiff::Timestamp>()
        .ok()
        .map(|time| time.as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::mcp::registry::{Identity, Mode, ToolArgs};
    fn args<A: serde::de::DeserializeOwned + ToolArgs>(value: Value) -> A {
        let args: A = serde_json::from_value(value).unwrap();
        args.validate(&Identity::of(None), Mode::Default).unwrap();
        args
    }
    async fn fixture() -> (tempfile::TempDir, Arc<CallContext>) {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join(".orchestry"))
            .await
            .unwrap();
        let engine = orch_engine::build_light_with(
            dir.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(dir.path().join(".orchestry/global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let cx = Arc::new(CallContext {
            engine: Arc::new(engine),
            project_root: dir.path().to_owned(),
            identity: Identity::of(None),
            mode: Mode::Default,
        });
        (dir, cx)
    }

    #[test]
    fn since_validation_rejects_unicode_overflow_and_missing_selector() {
        assert!(
            LogsArgs::default()
                .validate(&Identity::of(None), Mode::Default)
                .is_err()
        );
        for since in [
            "é",
            "",
            "s",
            "1",
            "1S",
            "-1s",
            "1.1h",
            "1 h",
            "١s",
            "9223372036854775807s",
            "999999999999999999999999999999999d",
        ] {
            let arg = LogsArgs {
                run_id: Some("run".into()),
                since: Some(since.into()),
                ..Default::default()
            };
            assert_eq!(
                arg.validate(&Identity::of(None), Mode::Default)
                    .unwrap_err()
                    .code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{since}"
            );
        }
        for (raw, expected) in [
            ("0s", 0),
            ("01m", 60_000),
            ("1h", 3_600_000),
            ("1d", 86_400_000),
        ] {
            assert_eq!(checked_duration(raw, 0).unwrap(), expected);
        }
        assert!(checked_duration("1s", i64::MIN).is_err());
        assert!(serde_json::from_value::<LogsArgs>(json!({"follow":true})).is_err());
    }
    fn run(index: usize) -> orch_domain::Run {
        orch_domain::Run {
            id: format!("run_{index:02}"),
            task_id: "task".into(),
            agent_id: "agent".into(),
            attempt: 1,
            status: orch_domain::RunStatus::Succeeded,
            started_at: format!("2026-01-01T00:00:{index:02}Z"),
            finished_at: Some("2026-01-01T00:01:00Z".into()),
            workspace_path: "workspace".into(),
            prompt: "prompt".into(),
            pid: None,
            error: None,
            error_kind: None,
            tokens: None,
        }
    }
    #[tokio::test]
    async fn logs_preserves_selector_precedence_counts_and_time_semantics() {
        let (_dir, cx) = fixture().await;
        for index in 0..25 {
            cx.engine.runs.save(&run(index)).await.unwrap();
        }
        let recent = cx.engine.clock.now_iso();
        for index in 0..60 {
            cx.engine
                .runs
                .append_event(
                    "run_00",
                    &orch_domain::RunEvent {
                        timestamp: if index == 59 {
                            recent.clone()
                        } else {
                            "1970-01-01T00:00:00Z".into()
                        },
                        event_type: orch_domain::RunEventType::AgentOutput,
                        data: json!(index),
                    },
                )
                .await
                .unwrap();
        }
        let events = logs(
            cx.clone(),
            args(json!({"run_id":"run_00","task":"task","agent":"agent"})),
        )
        .await
        .unwrap();
        assert_eq!(events.as_array().unwrap().len(), 50);
        assert_eq!(events[0]["data"], 10);
        assert!(events[0].get("type").is_some());
        let recent_events = logs(cx.clone(), args(json!({"run_id":"run_00","since":"1h"})))
            .await
            .unwrap();
        assert_eq!(recent_events.as_array().unwrap().len(), 1);
        assert_eq!(recent_events[0]["data"], 59);
        assert_eq!(
            logs(cx.clone(), args(json!({"run_id":"run_00","since":"0s"})))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            60
        );
        assert_eq!(
            logs(cx.clone(), args(json!({"task":"task"})))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            25
        );
        let tasks = logs(
            cx.clone(),
            args(json!({"task":"task","agent":"missing","since":"1s"})),
        )
        .await
        .unwrap();
        assert_eq!(tasks.as_array().unwrap().len(), 20); // No time filter in this route.
        assert!(tasks[0].get("task_id").is_some());
        assert_eq!(
            logs(cx.clone(), args(json!({"agent":"agent","since":"1s"})))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            logs(cx.clone(), args(json!({"since":"1s"}))).await.unwrap(),
            json!([])
        );
        // An unfinished old run overlaps every window, exactly as the CLI.
        let mut unfinished = run(26);
        unfinished.finished_at = None;
        cx.engine.runs.save(&unfinished).await.unwrap();
        assert_eq!(
            logs(cx, args(json!({"since":"1s"}))).await.unwrap()[0]["id"],
            unfinished.id
        );
    }
    #[test]
    fn catalogue_enforces_modes_annotations_and_strict_schemas() {
        let expected: [(&str, Class, bool, &[&str]); 1] = [("logs", Class::Read, false, &[][..])];
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            let mut registry = Registry::new(mode);
            register(&mut registry).unwrap();
            let tools = registry.into_router().list_all();
            let wanted: Vec<_> = expected
                .iter()
                .filter(|(_, class, _, _)| mode.allows(*class))
                .collect();
            assert_eq!(tools.len(), wanted.len());
            for (tool, (name, class, destructive, required)) in tools.iter().zip(wanted) {
                assert_eq!(tool.name.as_ref(), *name);
                let value = serde_json::to_value(tool).unwrap();
                assert_eq!(
                    value["annotations"],
                    serde_json::json!({
                        "readOnlyHint": *class == Class::Read,
                        "destructiveHint": destructive,
                        "idempotentHint": *class == Class::Read,
                        "openWorldHint": *class != Class::Read,
                    })
                );
                assert_eq!(value["inputSchema"]["additionalProperties"], false);
                let mut actual: Vec<_> = value["inputSchema"]["required"]
                    .as_array()
                    .map(|items| items.iter().map(|v| v.as_str().unwrap()).collect())
                    .unwrap_or_default();
                actual.sort();
                assert_eq!(&actual, required, "{name}");
            }
        }
    }
}
