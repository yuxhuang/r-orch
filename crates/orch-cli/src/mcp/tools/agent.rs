//! Agent definition and scheduling tools. Authored writes take the daemon's
//! definition lock; the bridge only shapes the request.

use crate::document_input;
use crate::mcp::{
    projections::{self, id},
    registry::{
        CallContext, Class, EmptyArgs, Execution, Identity, Mode, Registry, ToolArgs, ToolError,
        ToolMeta, ToolResult, json, wire,
    },
    tools::task::{at_source, definition_input},
};
use orch_core::Result;
use orch_proto::ops::args::{self as rpc, MutationKind};
use orch_proto::ops::results::AuthoredAgent;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

// The domain crate does not depend on JSON Schema. Keep wire enums here and
// convert exhaustively so adding a spelling cannot silently broaden this API.
macro_rules! wire_enum {
    ($name:ident, $target:ty, {$($variant:ident => $value:expr),+ $(,)?}) => {
        #[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        enum $name { $($variant),+ }
        impl From<$name> for $target {
            fn from(value: $name) -> Self { match value { $($name::$variant => $value),+ } }
        }
    }
}
wire_enum!(Effort, orch_domain::ReasoningEffort, {
    None => Self::None, Off => Self::Off, Minimal => Self::Minimal,
    Low => Self::Low, Medium => Self::Medium, High => Self::High,
    Xhigh => Self::Xhigh, Max => Self::Max, Ultra => Self::Ultra
});
wire_enum!(EffortOrDefault, orch_domain::EffortSelection, {
    Default => Self::Default,
    None => Self::Set(orch_domain::ReasoningEffort::None),
    Off => Self::Set(orch_domain::ReasoningEffort::Off),
    Minimal => Self::Set(orch_domain::ReasoningEffort::Minimal),
    Low => Self::Set(orch_domain::ReasoningEffort::Low),
    Medium => Self::Set(orch_domain::ReasoningEffort::Medium),
    High => Self::Set(orch_domain::ReasoningEffort::High),
    Xhigh => Self::Set(orch_domain::ReasoningEffort::Xhigh),
    Max => Self::Set(orch_domain::ReasoningEffort::Max),
    Ultra => Self::Set(orch_domain::ReasoningEffort::Ultra)
});
wire_enum!(PromptSelection, orch_domain::PromptModeSelection, {
    Inherit => Self::Inherit,
    Normal => Self::Set(orch_domain::PromptMode::Normal),
    Eco => Self::Set(orch_domain::PromptMode::Eco)
});
wire_enum!(Approval, orch_domain::ApprovalPolicy, {
    Suggest => Self::Suggest, Auto => Self::Auto, Manual => Self::Manual
});
wire_enum!(Workspace, orch_domain::WorkspaceMode, {
    Shared => Self::Shared, Worktree => Self::Worktree, Isolated => Self::Isolated
});
wire_enum!(Format, document_input::InputFormat, {
    Json => Self::Json, Yaml => Self::Yaml
});

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}
impl ToolArgs for IdArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    id: String,
    #[serde(default)]
    effective: bool,
}
impl ToolArgs for ShowArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddArgs {
    name: String,
    adapter: String,
    role: Option<String>,
    command: Option<String>,
    model: Option<String>,
    effort: Option<Effort>,
    prompt_mode: Option<PromptSelection>,
    role_summary: Option<String>,
    max_turns: Option<u32>,
    /// Timeout in milliseconds.
    timeout: Option<u64>,
    approval_policy: Option<Approval>,
    workspace_mode: Option<Workspace>,
    /// Comma-separated skills; whitespace around each item is trimmed.
    skills: Option<String>,
}
impl ToolArgs for AddArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    id: String,
    file: Option<String>,
    input_format: Option<Format>,
    prompt_mode: Option<PromptSelection>,
    effort: Option<EffortOrDefault>,
    role_summary: Option<String>,
}
impl ToolArgs for UpdateArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        let flags =
            self.prompt_mode.is_some() || self.effort.is_some() || self.role_summary.is_some();
        if self.file.is_some() == flags || (self.input_format.is_some() && self.file.is_none()) {
            return Err(rmcp::ErrorData::invalid_params(
                "agent_update requires file (with optional input_format) OR at least one of prompt_mode, effort, role_summary",
                None,
            ));
        }
        if let Some(file) = &self.file {
            validate_file(file)?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PatchArgs {
    id: String,
    file: String,
    input_format: Option<Format>,
}
impl ToolArgs for PatchArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        validate_file(&self.file)
    }
}
fn validate_file(file: &str) -> std::result::Result<(), rmcp::ErrorData> {
    if file.is_empty() || file == "-" {
        Err(rmcp::ErrorData::invalid_params(
            "file must be a nonempty regular file path; '-' would consume protocol stdin",
            None,
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AutonomousArgs {
    id: String,
    #[serde(default)]
    on: bool,
    #[serde(default)]
    off: bool,
}
impl ToolArgs for AutonomousArgs {}

fn meta(
    name: &'static str,
    description: &'static str,
    class: Class,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
) -> ToolMeta {
    ToolMeta {
        name,
        description,
        class,
        execution: Execution::Store,
        destructive_hint,
        idempotent_hint,
        open_world_hint,
    }
}

pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add(
        meta(
            "agent_list",
            "List local agents plus remote snapshots (orch agent list).",
            Class::Read,
            false,
            true,
            false,
        ),
        list,
    )?;
    registry.add(
        meta(
            "agent_show",
            "Read an agent, optionally with effective prompt mode (orch agent show).",
            Class::Read,
            false,
            true,
            false,
        ),
        show,
    )?;
    registry.add(
        meta(
            "agent_add",
            "Create an agent definition (orch agent add); timeout is milliseconds.",
            Class::Mutate,
            false,
            false,
            true,
        ),
        add,
    )?;
    registry.add(meta("agent_update", "Replace an agent definition from a file OR update selected flags (orch agent update). inherit clears prompt_mode; default clears effort.", Class::Mutate, true, false, true), update)?;
    registry.add(
        meta(
            "agent_patch",
            "Merge-patch an authored agent definition from a local file (orch agent patch).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        patch,
    )?;
    registry.add(
        meta(
            "agent_enable",
            "Enable agent scheduling (orch agent enable).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        |cx, args: IdArgs| enabled(cx, args, true),
    )?;
    registry.add(
        meta(
            "agent_disable",
            "Disable agent scheduling (orch agent disable).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        |cx, args: IdArgs| enabled(cx, args, false),
    )?;
    registry.add(meta("agent_autonomous", "Set or toggle autonomous mode (orch agent autonomous). on wins over off; neither toggles.", Class::Mutate, true, false, true), autonomous)?;
    registry.add(
        meta(
            "agent_remove",
            "Remove a local agent (orch agent remove).",
            Class::Destructive,
            true,
            false,
            true,
        ),
        remove,
    )?;
    Ok(())
}
async fn list(cx: Arc<CallContext>, _: EmptyArgs) -> ToolResult {
    projections::agents(&cx).await
}
async fn show(cx: Arc<CallContext>, args: ShowArgs) -> ToolResult {
    projections::agent(&cx, &args.id, args.effective).await
}
fn diagnostics(authored: &AuthoredAgent) {
    for diagnostic in &authored.diagnostics {
        tracing::warn!(target: "orch::mcp", diagnostic = ?diagnostic, "agent definition diagnostic");
    }
}
fn authored(authored: &AuthoredAgent) -> ToolResult {
    diagnostics(authored);
    json(&authored.agent, "agent")
}

async fn add(cx: Arc<CallContext>, args: AddArgs) -> ToolResult {
    let skills = args
        .skills
        .as_deref()
        .map(|raw| raw.split(',').map(|part| part.trim().to_owned()).collect::<Vec<_>>());
    let created = cx
        .request(wire::<rpc::AgentCreateArgs>(json!({
            "name": args.name,
            "adapter": args.adapter,
            "role": args.role,
            "command": args.command,
            "model": args.model,
            "effort": args.effort.map(orch_domain::ReasoningEffort::from),
            // Absent means live inheritance of the project default: the
            // default is never copied into the agent (design D1).
            "prompt_mode": args
                .prompt_mode
                .map(orch_domain::PromptModeSelection::from)
                .and_then(orch_domain::PromptModeSelection::into_override),
            "role_summary": args.role_summary,
            "max_turns": args.max_turns,
            "timeout_ms": args.timeout,
            "approval_policy": args.approval_policy.map(orch_domain::ApprovalPolicy::from),
            "workspace_mode": args.workspace_mode.map(orch_domain::WorkspaceMode::from),
            "skills": skills,
        }))?)
        .await?;
    authored(&created)
}
async fn file_mutation(
    cx: &CallContext,
    agent_id: &str,
    file: &str,
    format: Option<Format>,
    kind: MutationKind,
) -> ToolResult {
    let (path, definition) = definition_input(cx, file, format.map(Into::into), kind).await?;
    let updated = cx
        .request(rpc::AgentDefinitionApplyArgs {
            id: id(agent_id)?,
            definition,
        })
        .await
        .map_err(|err| at_source(&path, err))?;
    authored(&updated)
}
/// The flag form as the CLI compiles it: a merge patch of `{"config": …}`
/// where `inherit`/`default` clear the override with `null`.
fn flag_patch(args: &UpdateArgs) -> Value {
    let mut config = serde_json::Map::new();
    if let Some(mode) = args.prompt_mode {
        let mode = orch_domain::PromptModeSelection::from(mode).into_override();
        config.insert(
            "prompt_mode".into(),
            mode.map_or(Value::Null, |mode| Value::String(mode.as_str().to_owned())),
        );
    }
    if let Some(effort) = args.effort {
        let effort = orch_domain::EffortSelection::from(effort).into_effort();
        config.insert(
            "effort".into(),
            effort.map_or(Value::Null, |effort| Value::String(effort.as_str().to_owned())),
        );
    }
    if let Some(summary) = &args.role_summary {
        config.insert("role_summary".into(), Value::String(summary.clone()));
    }
    json!({ "config": config })
}
async fn update(cx: Arc<CallContext>, args: UpdateArgs) -> ToolResult {
    if let Some(file) = &args.file {
        return file_mutation(&cx, &args.id, file, args.input_format, MutationKind::Update).await;
    }
    let document = serde_json::to_string(&flag_patch(&args))
        .map_err(|_| ToolError::Text("serialize the definition".to_owned()))?;
    let definition = wire(json!({
        "input": {"document": document, "format": "json"},
        "kind": MutationKind::Patch,
    }))?;
    let updated = cx
        .request(rpc::AgentDefinitionApplyArgs {
            id: id(&args.id)?,
            definition,
        })
        .await?;
    authored(&updated)
}
async fn patch(cx: Arc<CallContext>, args: PatchArgs) -> ToolResult {
    file_mutation(
        &cx,
        &args.id,
        &args.file,
        args.input_format,
        MutationKind::Patch,
    )
    .await
}
async fn enabled(cx: Arc<CallContext>, args: IdArgs, enable: bool) -> ToolResult {
    let agent = if enable {
        cx.request(rpc::AgentEnableArgs { id: id(&args.id)? }).await?
    } else {
        cx.request(rpc::AgentDisableArgs { id: id(&args.id)? }).await?
    };
    json(&agent, "agent")
}
async fn autonomous(cx: Arc<CallContext>, args: AutonomousArgs) -> ToolResult {
    // `on` wins over `off`; neither toggles, resolved by the daemon inside
    // the same operation.
    let enabled = if args.on {
        Some(true)
    } else if args.off {
        Some(false)
    } else {
        None
    };
    let agent = cx
        .request(rpc::AgentAutonomousArgs {
            id: id(&args.id)?,
            enabled,
        })
        .await?;
    json(&agent, "agent")
}
async fn remove(cx: Arc<CallContext>, args: IdArgs) -> ToolResult {
    cx.request(rpc::AgentRemoveArgs { id: id(&args.id)? })
        .await?;
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    use crate::mcp::registry::{Identity, Mode, ToolArgs};
    fn args<A: serde::de::DeserializeOwned + ToolArgs>(value: Value) -> A {
        let args: A = serde_json::from_value(value).unwrap();
        args.validate(&Identity::of(None), Mode::Default).unwrap();
        args
    }
    async fn fixture() -> (crate::mcp::testing::Fixture, Arc<CallContext>) {
        let fx = crate::mcp::testing::Fixture::start().await;
        let cx = fx.context(Some("cli"), Mode::AllowDestructive);
        (fx, cx)
    }

    #[test]
    fn update_and_patch_validate_forms_and_strict_enums() {
        for value in [
            json!({"id":"x"}),
            json!({"id":"x","input_format":"json"}),
            json!({"id":"x","file":"x.json","effort":"default"}),
            json!({"id":"x","file":"-"}),
            json!({"id":"x","file":""}),
        ] {
            let args: UpdateArgs = serde_json::from_value(value).unwrap();
            assert_eq!(
                args.validate(&Identity::of(None), Mode::Default)
                    .unwrap_err()
                    .code,
                rmcp::model::ErrorCode::INVALID_PARAMS
            );
        }
        for file in ["", "-"] {
            let args: PatchArgs = serde_json::from_value(json!({"id":"x","file":file})).unwrap();
            assert!(args.validate(&Identity::of(None), Mode::Default).is_err());
        }
        for field in ["prompt_mode", "effort", "input_format"] {
            let mut value = json!({"id":"x","file":"x.json"});
            value[field] = json!("bogus");
            assert!(serde_json::from_value::<UpdateArgs>(value).is_err());
        }
        assert!(
            serde_json::from_value::<AddArgs>(
                json!({"name":"x","adapter":"shell","effort":"default"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<AddArgs>(json!({"name":"x","adapter":"shell","timeout":-1}))
                .is_err()
        );
        assert!(serde_json::from_value::<IdArgs>(json!({"id":"x","extra":true})).is_err());
        let _: UpdateArgs =
            args(json!({"id":"x","effort":"default","prompt_mode":"inherit","role_summary":""}));
    }

    #[tokio::test]
    async fn all_nine_tools_preserve_agent_shapes_and_scheduling_semantics() {
        let (fx, cx) = fixture().await;
        let _: EmptyArgs = args(json!({}));
        assert_eq!(list(cx.clone(), args(json!({}))).await.unwrap(), json!([]));
        let created = add(cx.clone(), args(json!({"name":"one","adapter":"shell","command":"true","skills":" a, b ","prompt_mode":"eco"}))).await.unwrap();
        let id = created["id"].as_str().unwrap();
        assert_eq!(created["config"]["skills"], json!(["a", "b"]));
        assert_eq!(
            show(cx.clone(), args(json!({"id":id}))).await.unwrap()["config"],
            created["config"]
        );
        let effective = show(cx.clone(), args(json!({"id":id,"effective":true})))
            .await
            .unwrap();
        assert_eq!(effective["agent"]["id"], id);
        assert_eq!(effective["effective_config"]["prompt_mode"], "eco");
        let updated = update(
            cx.clone(),
            args(json!({"id":id,"prompt_mode":"inherit","role_summary":"summary"})),
        )
        .await
        .unwrap();
        assert_eq!(updated["config"]["role_summary"], "summary");
        assert!(updated["config"].get("prompt_mode").is_none());
        let path = cx.project_root.join("agent.json");
        tokio::fs::write(&path, serde_json::to_vec(&updated).unwrap())
            .await
            .unwrap();
        assert_eq!(
            update(cx.clone(), args(json!({"id":id,"file":"agent.json"})))
                .await
                .unwrap()["id"],
            id
        );
        tokio::fs::write(&path, br#"{"role":"patched"}"#)
            .await
            .unwrap();
        assert_eq!(
            patch(cx.clone(), args(json!({"id":id,"file":"agent.json"})))
                .await
                .unwrap()["role"],
            "patched"
        );
        let disabled = enabled(cx.clone(), args(json!({"id":id})), false)
            .await
            .unwrap();
        assert_eq!(disabled["status"], "disabled");
        let enabled = enabled(cx.clone(), args(json!({"id":id})), true)
            .await
            .unwrap();
        assert_eq!(enabled["status"], "idle");
        assert_eq!(
            autonomous(cx.clone(), args(json!({"id":id,"on":true,"off":true})))
                .await
                .unwrap()["autonomous"],
            true
        );
        assert_eq!(
            autonomous(cx.clone(), args(json!({"id":id})))
                .await
                .unwrap()["autonomous"],
            false
        );
        assert_eq!(
            projections::agents(&cx)
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            remove(cx.clone(), args(json!({"id":id}))).await.unwrap(),
            Value::Null
        );
        assert!(show(cx.clone(), args(json!({"id":id}))).await.is_err());
    }

    #[tokio::test]
    async fn definition_files_are_bounded_and_keep_error_provenance() {
        let (fx, cx) = fixture().await;
        let agent = add(
            cx.clone(),
            args(json!({"name":"one","adapter":"shell","command":"true"})),
        )
        .await
        .unwrap();
        let id = agent["id"].as_str().unwrap();
        let before = serde_json::to_value(fx.engine().agent_service.get(id).await.unwrap()).unwrap();
        let path = cx.project_root.join("bad.json");
        for input in [
            br#"{"unknown":null}"#.to_vec(),
            br#"{"name":"a","name":"b"}"#.to_vec(),
            vec![b'x'; document_input::MAX_DOCUMENT_BYTES + 1],
        ] {
            tokio::fs::write(&path, input).await.unwrap();
            let err = patch(cx.clone(), args(json!({"id":id,"file":"bad.json"})))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("bad.json"), "{err}");
            assert_eq!(
                serde_json::to_value(fx.engine().agent_service.get(id).await.unwrap()).unwrap(),
                before
            );
        }
        assert!(
            add(
                cx.clone(),
                args(json!({"name":"invalid","adapter":"shell"}))
            )
            .await
            .is_err()
        );
        assert!(
            update(
                cx.clone(),
                args(json!({"id":id,"role_summary":"é".repeat(97)}))
            )
            .await
            .is_err()
        );
        assert!(
            enabled(cx.clone(), args(json!({"id":"one@remote"})), true)
                .await
                .is_err()
        );
    }
    #[test]
    fn catalogue_enforces_modes_annotations_and_strict_schemas() {
        let expected: [(&str, Class, bool, &[&str]); 9] = [
            ("agent_add", Class::Mutate, false, &["adapter", "name"][..]),
            ("agent_autonomous", Class::Mutate, true, &["id"][..]),
            ("agent_disable", Class::Mutate, true, &["id"][..]),
            ("agent_enable", Class::Mutate, true, &["id"][..]),
            ("agent_list", Class::Read, false, &[][..]),
            ("agent_patch", Class::Mutate, true, &["file", "id"][..]),
            ("agent_remove", Class::Destructive, true, &["id"][..]),
            ("agent_show", Class::Read, false, &["id"][..]),
            ("agent_update", Class::Mutate, true, &["id"][..]),
        ];
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
