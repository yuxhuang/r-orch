//! The typed tool registry over the SDK router (design D3).
//!
//! Each tool group registers its tools here, once, with their metadata, an
//! argument type and an async handler. The registry is the only place that
//! knows how a call becomes work:
//!
//! 1. the class is re-checked against the launch mode (a route is only added
//!    when the mode allows its class, so this is defence against a
//!    registration mistake; a refused class looks exactly like an absent
//!    tool);
//! 2. the arguments are decoded **explicitly** — absent arguments are `{}`,
//!    unknown fields are refused by the args type's `deny_unknown_fields` —
//!    and a failure is JSON-RPC `-32602` `Invalid arguments for <tool>: …`.
//!    The SDK's own decoder would turn that into a tool result instead,
//!    because its router rewrites messages that start with its own prefix;
//! 3. [`ToolArgs::validate`] runs, still before the daemon is contacted;
//! 4. the call is admitted to its lane, the handler runs — its first request
//!    opens this call's own daemon session — and its [`ToolResult`] becomes
//!    the tool result: the value as one JSON text block, or the error as
//!    `isError` text. A daemon error keeps its message and hint; a failure to
//!    reach the daemon is the uniform no-daemon text plus
//!    `structuredContent.exit_code` (7).

use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use orch_client::{Client, ClientError};
use orch_proto::ops::Op;
use rmcp::ErrorData;
use rmcp::handler::server::common::schema_for_input;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{Mutex, OnceCell};

pub(crate) use super::identity::Identity;
pub(crate) use super::mode::{Class, Mode};
use super::server::{Connector, McpServer};

/// Which admission lane a tool runs in (design D8). Every tool is
/// [`Execution::Store`] except `doctor`, the only [`Execution::Diagnostic`]
/// registration the registry accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Execution {
    Store,
    Diagnostic,
}

/// A tool's static metadata. All four annotation hints are always emitted.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolMeta {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) class: Class,
    pub(crate) execution: Execution,
    pub(crate) destructive_hint: bool,
    pub(crate) idempotent_hint: bool,
    pub(crate) open_world_hint: bool,
}

/// Argument-form validation that runs after decoding and before the daemon
/// is contacted: cross-field forms, identity-dependent requirements, `file: "-"`,
/// checked durations, mode-dependent argument gates. The default accepts.
pub(crate) trait ToolArgs {
    fn validate(&self, _identity: &Identity, _mode: Mode) -> Result<(), ErrorData> {
        Ok(())
    }
}

/// The arguments of a tool that takes none. Unknown fields are refused.
#[derive(Debug, Clone, Default, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyArgs {}

impl ToolArgs for EmptyArgs {}

/// Why a tool call failed.
#[derive(Debug)]
pub(crate) enum ToolError {
    /// The daemon refused or failed the request, or could not be reached.
    Client(ClientError),
    /// Bridge-side text ready for the caller: a local argument conversion, a
    /// definition file that cannot be read, a refusal before admission.
    Text(String),
}

impl From<ClientError> for ToolError {
    fn from(error: ClientError) -> Self {
        ToolError::Client(error)
    }
}

impl From<orch_core::Error> for ToolError {
    fn from(error: orch_core::Error) -> Self {
        log_error_chain(&error);
        ToolError::Text(error_text(&error))
    }
}

impl ToolError {
    /// The exit code the CLI would report for the same failure; 7 for every
    /// form of "no usable daemon".
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            ToolError::Client(error) => error.exit_code(),
            ToolError::Text(_) => 1,
        }
    }

    /// The caller-facing text. A daemon error is its message and hint, as
    /// the CLI prints a core error; a connection failure is the uniform
    /// no-daemon message in its owner or agent variant.
    pub(crate) fn text(&self, agent_context: bool) -> String {
        match self {
            ToolError::Client(ClientError::Connection(failure)) => failure.render(agent_context),
            ToolError::Client(ClientError::Remote(body)) => match &body.hint {
                Some(hint) => format!("{}\nHint: {}", body.message.as_str(), hint.as_str()),
                None => body.message.as_str().to_owned(),
            },
            ToolError::Client(ClientError::InvalidArguments(text)) | ToolError::Text(text) => {
                text.clone()
            }
        }
    }
}

impl std::fmt::Display for ToolError {
    /// The owner-variant text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text(false))
    }
}

/// A handler's outcome.
pub(crate) type ToolResult<T = Value> = Result<T, ToolError>;

/// Build a protocol argument DTO from the bridge's JSON rendering of a
/// tool's arguments. The DTO's own bounds and closed shape apply here,
/// before anything is sent.
pub(crate) fn wire<A: DeserializeOwned>(value: Value) -> ToolResult<A> {
    serde_json::from_value(value)
        .map_err(|err| ToolError::Text(format!("Invalid arguments: {err}")))
}

/// A daemon result as JSON.
pub(crate) fn json<T: serde::Serialize>(value: &T, what: &str) -> ToolResult {
    serde_json::to_value(value).map_err(|_| ToolError::Text(format!("serialize {what}")))
}

/// What a handler receives: the server's fixed launch settings and this
/// call's daemon session, opened by the first request and closed when the
/// call ends. A session is never shared between calls, so a lost reply
/// poisons only the call that lost it.
pub(crate) struct CallContext {
    connector: Connector,
    session: OnceCell<Mutex<Client>>,
    pub(crate) project_root: PathBuf,
    pub(crate) identity: Identity,
    pub(crate) agent_context: bool,
}

impl CallContext {
    pub(crate) fn new(
        connector: Connector,
        project_root: PathBuf,
        identity: Identity,
        agent_context: bool,
    ) -> CallContext {
        CallContext {
            connector,
            session: OnceCell::new(),
            project_root,
            identity,
            agent_context,
        }
    }

    async fn session(&self) -> ToolResult<&Mutex<Client>> {
        Ok(self
            .session
            .get_or_try_init(|| async { (self.connector)().await.map(Mutex::new) })
            .await?)
    }

    /// One typed request to the daemon. Never retried: an unknown outcome
    /// is reported as such.
    pub(crate) async fn request<A: Op>(&self, args: A) -> ToolResult<A::Output> {
        Ok(self.session().await?.lock().await.request(args).await?)
    }

    /// [`CallContext::request`], keeping the daemon's JSON result as sent.
    /// For results whose typed form cannot say "present and null" (a config
    /// value stored as `null`).
    pub(crate) async fn request_json<A: Op>(&self, args: A) -> ToolResult {
        Ok(self
            .session()
            .await?
            .lock()
            .await
            .call::<Value>(args.into_request())
            .await?)
    }

    /// Upload `bytes` as a sealed transfer that a later request of this
    /// same call consumes (a definition document too large to send inline).
    pub(crate) async fn upload(
        &self,
        purpose: orch_proto::ops::args::TransferPurpose,
        bytes: &[u8],
    ) -> ToolResult<orch_proto::ops::args::TransferToken> {
        Ok(self
            .session()
            .await?
            .lock()
            .await
            .upload(purpose, bytes)
            .await?)
    }
}

/// The tools of one server, filtered by its mode.
pub(crate) struct Registry {
    mode: Mode,
    /// Every name ever registered, allowed by the mode or not, so a duplicate
    /// is a startup error in every mode.
    names: BTreeSet<&'static str>,
    router: ToolRouter<McpServer>,
}

impl Registry {
    pub(crate) fn new(mode: Mode) -> Registry {
        Registry {
            mode,
            names: BTreeSet::new(),
            router: ToolRouter::new(),
        }
    }

    /// Register one tool. A duplicate name, an unusable argument schema or a
    /// diagnostic registration other than `doctor` is a startup error. A tool
    /// whose class the mode does not allow is recorded but not routed, so it
    /// is neither listed nor callable.
    pub(crate) fn add<A, F, Fut>(&mut self, meta: ToolMeta, handler: F) -> orch_core::Result<()>
    where
        A: ToolArgs + DeserializeOwned + JsonSchema + Send + 'static,
        F: Fn(Arc<CallContext>, A) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        if !self.names.insert(meta.name) {
            return Err(orch_core::Error::other(format!(
                "MCP tool registered twice: {}",
                meta.name
            )));
        }
        if meta.execution == Execution::Diagnostic && meta.name != "doctor" {
            return Err(orch_core::Error::other(format!(
                "MCP tool {} cannot use the diagnostic lane",
                meta.name
            )));
        }
        let schema = schema_for_input::<A>().map_err(|err| {
            orch_core::Error::other(format!(
                "MCP tool {} has no usable schema: {err}",
                meta.name
            ))
        })?;
        if !self.mode.allows(meta.class) {
            return Ok(());
        }

        let tool = Tool::new(meta.name, meta.description, schema).with_annotations(
            ToolAnnotations::from_raw(
                None,
                Some(meta.class == Class::Read),
                Some(meta.destructive_hint),
                Some(meta.idempotent_hint),
                Some(meta.open_world_hint),
            ),
        );
        let route =
            ToolRoute::<McpServer>::new_dyn(tool, move |call: ToolCallContext<'_, McpServer>| {
                let handler = handler.clone();
                Box::pin(dispatch::<A, F, Fut>(meta, handler, call))
                    as BoxFuture<'_, Result<CallToolResponse, ErrorData>>
            });
        self.router.add_route(route);
        Ok(())
    }

    pub(crate) fn into_router(self) -> ToolRouter<McpServer> {
        self.router
    }
}

/// Decode `arguments` (absent is `{}`) into `A`, as invalid params on
/// failure. The wording deliberately avoids the SDK's
/// `failed to deserialize parameters:` prefix, which its router would
/// rewrite into a tool result.
pub(crate) fn decode<A: DeserializeOwned + JsonSchema + 'static>(
    name: &str,
    arguments: Option<JsonObject>,
) -> Result<A, ErrorData> {
    serde_path_to_error::deserialize(Value::Object(arguments.unwrap_or_default())).map_err(|err| {
        let raw = err.inner().to_string();
        let mut path = err.path().to_string();
        // Serde reports a missing field at its parent object. The name comes
        // from the argument definition, not from a supplied value.
        let missing = raw
            .strip_prefix("missing field `")
            .and_then(|s| s.strip_suffix('`'));
        if let Some(field) = missing {
            if path == "." {
                path = field.to_owned();
            } else {
                path.push('.');
                path.push_str(field);
            }
        }
        let field_path = path.escape_debug().to_string().replace('`', "\\`");
        let detail = match missing {
            Some(field) => format!(
                "missing field `{field_path}`: expected {}",
                missing_field_type::<A>(err.path(), field)
                    .unwrap_or_else(|| "a required field".to_owned())
            ),
            None if raw.starts_with("unknown field ") => {
                format!("unknown field `{field_path}`: expected a declared field")
            }
            None => format!("field `{field_path}`: {}", scrub_serde_error(&raw)),
        };
        ErrorData::invalid_params(format!("Invalid arguments for {name}: {detail}"), None)
    })
}

/// A missing field has no serde expected-type text. Use the same static
/// schema advertised to clients, falling back when its shape is not simple.
fn missing_field_type<A: JsonSchema + 'static>(
    path: &serde_path_to_error::Path,
    field: &str,
) -> Option<String> {
    let schema = schema_for_input::<A>().ok()?;
    let root = Value::Object((*schema).clone());
    let mut node = &root;
    for segment in path {
        node = schema_node(&root, node)?;
        node = match segment {
            serde_path_to_error::Segment::Map { key } => node.get("properties")?.get(key)?,
            serde_path_to_error::Segment::Seq { .. } => node.get("items")?,
            _ => return None,
        };
    }
    let property = schema_node(&root, node)?.get("properties")?.get(field)?;
    let property = schema_node(&root, property)?;
    property
        .get("format")
        .or_else(|| property.get("type"))?
        .as_str()
        .map(str::to_owned)
}

fn schema_node<'a>(root: &'a Value, node: &'a Value) -> Option<&'a Value> {
    let node = if let Some(choices) = node.get("anyOf").and_then(Value::as_array) {
        choices
            .iter()
            .find(|choice| choice.get("type").and_then(Value::as_str) != Some("null"))?
    } else {
        node
    };
    match node.get("$ref").and_then(Value::as_str) {
        Some(reference) => root.pointer(reference.strip_prefix('#')?),
        None => Some(node),
    }
}

/// Serde's diagnostics include argument literals. rmcp logs this message at
/// WARN as well as returning it to the client, so neither may carry values.
/// Only recognized serde diagnostics pass through; custom error prose is not
/// guaranteed to quote its values and must use a fixed fallback.
fn scrub_serde_error(raw: &str) -> String {
    let diagnostic = if raw.starts_with("missing field ") {
        return "missing field; expected a required field".to_owned();
    } else if raw.starts_with("unknown field ") {
        return "unknown field; expected a declared field".to_owned();
    } else if raw.starts_with("duplicate field ") {
        return "duplicate field; expected one occurrence".to_owned();
    } else if raw.starts_with("invalid type:")
        || raw.starts_with("invalid value:")
        || raw.starts_with("invalid length")
        || raw.starts_with("unknown variant")
    {
        // Ignore the unexpected value wholesale. Custom serde errors can
        // include unquoted values too; only the expected side is useful.
        raw.rsplit_once(", expected ").map(|(_, expected)| expected)
    } else {
        None
    };
    let Some(diagnostic) = diagnostic else {
        return "invalid argument; expected the declared input schema".to_owned();
    };
    let mut safe = String::from("expected ");
    let mut chars = diagnostic.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '\"' | '\'' | '`') {
            safe.push_str("[redacted]");
            while let Some(next) = chars.next() {
                if next == '\\' {
                    chars.next();
                } else if next == ch {
                    break;
                }
            }
        } else if ch.is_alphanumeric() || ch == '_' {
            let mut word = String::from(ch);
            while chars
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            if ch.is_numeric() || matches!(word.as_str(), "true" | "false") {
                safe.push_str("[redacted]");
            } else {
                safe.push_str(&word);
            }
        } else {
            safe.push(ch);
        }
    }
    safe
}

/// The `tool not found` error the SDK router gives an absent tool.
pub(crate) fn tool_not_found() -> ErrorData {
    ErrorData::invalid_params("tool not found", None)
}

async fn dispatch<A, F, Fut>(
    meta: ToolMeta,
    handler: F,
    call: ToolCallContext<'_, McpServer>,
) -> Result<CallToolResponse, ErrorData>
where
    A: ToolArgs + DeserializeOwned + JsonSchema + Send + 'static,
    F: Fn(Arc<CallContext>, A) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ToolResult> + Send + 'static,
{
    let server = call.service;
    // Stored mode filtering is the gate; this repeats it before execution.
    if !server.mode().allows(meta.class) {
        return Err(tool_not_found());
    }
    let args: A = decode(meta.name, call.arguments)?;
    args.validate(server.identity(), server.mode())?;
    let cancelled = call.request_context.ct.clone();
    let outcome = server
        .run_call(meta.execution, &cancelled, move |cx| handler(cx, args))
        .await;
    Ok(encode(outcome, server.agent_context()).into())
}

/// The tool result for a handler outcome: the value as compact JSON text, or
/// the error's text as an `isError` result. A failure whose exit code is 7
/// (no usable daemon, or a daemon that did not admit the request) also
/// carries `structuredContent: {"exit_code": 7}`, so a client can tell it
/// from an ordinary tool error without parsing text.
pub(crate) fn encode(outcome: ToolResult, agent_context: bool) -> CallToolResult {
    match outcome {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(err) => error_result(&format!("cannot serialize the result: {err}")),
        },
        Err(err) => {
            let mut result = error_result(&err.text(agent_context));
            let exit_code = err.exit_code();
            if exit_code == orch_proto::error::exit::NO_DAEMON {
                result.structured_content = Some(serde_json::json!({ "exit_code": exit_code }));
            }
            result
        }
    }
}

/// `message` as an `isError` tool result.
pub(crate) fn error_result(message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.to_owned())])
}

/// A core error as the CLI reports it: the message, then `Hint: …` when
/// there is one. No source chain — that goes to stderr at debug level.
pub(crate) fn error_text(err: &orch_core::Error) -> String {
    match err.hint() {
        Some(hint) => format!("{err}\nHint: {hint}"),
        None => err.to_string(),
    }
}

fn log_error_chain(err: &orch_core::Error) {
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        tracing::debug!(target: "orch::mcp", "caused by: {cause}");
        source = std::error::Error::source(cause);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct ShowArgs {
        id: String,
        #[serde(default)]
        effective: bool,
    }
    impl ToolArgs for ShowArgs {}

    fn meta(name: &'static str, class: Class) -> ToolMeta {
        ToolMeta {
            name,
            description: "test tool",
            class,
            execution: Execution::Store,
            destructive_hint: class != Class::Read,
            idempotent_hint: class == Class::Read,
            open_world_hint: false,
        }
    }

    fn object(value: Value) -> Option<JsonObject> {
        match value {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    #[test]
    fn decoding_is_strict_and_names_the_tool() {
        let args: ShowArgs = decode("t", object(serde_json::json!({"id": "x"}))).unwrap();
        assert_eq!(args.id, "x");
        assert!(!args.effective);

        let cases = [
            (serde_json::json!({}), "missing field"),
            (serde_json::json!({"id": 7}), "expected"),
            (serde_json::json!({"id": "x", "extra": 1}), "unknown field"),
            (
                serde_json::json!({"id": "x", "effective": "yes"}),
                "expected",
            ),
        ];
        for (input, reason) in cases {
            let err = decode::<ShowArgs>("task_show", object(input.clone())).expect_err("rejected");
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{input}");
            assert!(
                err.message.starts_with("Invalid arguments for task_show: "),
                "{}",
                err.message
            );
            assert!(err.message.contains(reason), "{input}: {}", err.message);
            assert!(!err.message.starts_with("failed to deserialize parameters:"));
        }

        // Absent arguments are `{}`: fine for EmptyArgs, a missing field otherwise.
        decode::<EmptyArgs>("status", None).unwrap();
        assert!(decode::<ShowArgs>("task_show", None).is_err());
        assert!(decode::<EmptyArgs>("status", object(serde_json::json!({"x": 1}))).is_err());
    }

    #[derive(Debug, serde::Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code, reason = "arguments are only decoded in rejection tests")]
    struct DiagnosticArgs {
        id: String,
        priority: i64,
        nested: Option<DiagnosticNested>,
    }

    #[derive(Debug, serde::Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "arguments are only decoded in rejection tests")]
    struct DiagnosticNested {
        items: Vec<u8>,
    }

    #[test]
    fn decoding_reports_fields_and_types_without_argument_values() {
        for (input, field, expected, secret) in [
            (
                serde_json::json!({"id":"x", "priority":"high"}),
                "priority",
                "i64",
                "high",
            ),
            (
                serde_json::json!({"id":987654, "priority":3}),
                "id",
                "string",
                "987654",
            ),
            (
                serde_json::json!({"id":"x", "priority":"secret\" , expected \"injected\nvalue"}),
                "priority",
                "i64",
                "secret",
            ),
            (
                serde_json::json!({"id":true, "priority":3}),
                "id",
                "string",
                "true",
            ),
            (
                serde_json::json!({"id":"x", "priority":3, "extra":"private"}),
                "extra",
                "expected a declared field",
                "private",
            ),
            (
                serde_json::json!({"priority":3, "nested":null}),
                "id",
                "expected string",
                "null",
            ),
            (
                serde_json::json!({"id":"root-private", "priority":3, "nested":{}}),
                "nested.items",
                "expected array",
                "root-private",
            ),
            (
                serde_json::json!({"id":"x", "priority":3, "nested":{"items":["nested-secret"]}}),
                "nested.items[0]",
                "u8",
                "nested-secret",
            ),
        ] {
            let err = decode::<DiagnosticArgs>("task_add", object(input)).unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(err.data.is_none());
            assert!(
                err.message.contains(&format!("field `{field}`")),
                "{}",
                err.message
            );
            assert!(err.message.contains(expected), "{}", err.message);
            assert!(!err.message.contains(secret), "{}", err.message);
        }
    }

    async fn ok(_cx: Arc<CallContext>, _args: EmptyArgs) -> ToolResult {
        Ok(Value::Null)
    }

    fn names(registry: Registry) -> Vec<String> {
        registry
            .into_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }

    fn populated(mode: Mode) -> Registry {
        let mut registry = Registry::new(mode);
        registry.add(meta("r", Class::Read), ok).unwrap();
        registry.add(meta("m", Class::Mutate), ok).unwrap();
        registry.add(meta("d", Class::Destructive), ok).unwrap();
        registry
    }

    #[test]
    fn the_mode_filters_registration() {
        assert_eq!(names(populated(Mode::ReadOnly)), ["r"]);
        assert_eq!(names(populated(Mode::Default)), ["m", "r"]);
        assert_eq!(names(populated(Mode::AllowDestructive)), ["d", "m", "r"]);
    }

    #[test]
    fn a_duplicate_name_is_a_startup_error_even_when_filtered() {
        let mut registry = Registry::new(Mode::ReadOnly);
        registry.add(meta("d", Class::Destructive), ok).unwrap();
        let err = registry
            .add(meta("d", Class::Destructive), ok)
            .expect_err("duplicate");
        assert_eq!(err.to_string(), "MCP tool registered twice: d");
    }

    #[test]
    fn only_doctor_may_use_the_diagnostic_lane() {
        let mut registry = Registry::new(Mode::Default);
        let mut diagnostic = meta("logs", Class::Read);
        diagnostic.execution = Execution::Diagnostic;
        assert!(registry.add(diagnostic, ok).is_err());
        diagnostic.name = "doctor";
        registry.add(diagnostic, ok).unwrap();
    }

    #[test]
    fn every_annotation_hint_is_emitted() {
        let mut registry = Registry::new(Mode::Default);
        registry.add(meta("m", Class::Mutate), ok).unwrap();
        let tools = registry.into_router().list_all();
        let json = serde_json::to_value(&tools[0]).unwrap();
        assert_eq!(
            json["annotations"],
            serde_json::json!({
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false,
            })
        );
        assert_eq!(json["inputSchema"]["type"], "object");
        assert_eq!(json["description"], "test tool");
    }

    #[test]
    fn errors_carry_their_hint() {
        let err = orch_core::Error::from(orch_domain::DomainError::NotInitialized);
        assert_eq!(error_text(&err), "Not initialized\nHint: Run: orch init");
        let result = encode(Err(err.into()), false);
        assert_eq!(result.is_error, Some(true));
        assert!(result.structured_content.is_none());
        let daemon = ToolError::Client(ClientError::Remote(
            orch_proto::error::ErrorBody::from_domain(
                &orch_domain::DomainError::NotInitialized,
                orch_proto::error::Execution::NotExecuted,
            ),
        ));
        assert_eq!(daemon.text(false), "Not initialized\nHint: Run: orch init");
        let result = encode(Ok(serde_json::json!([1, {"a": null}])), false);
        assert_eq!(result.is_error, Some(false));
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["content"][0]["text"], "[1,{\"a\":null}]");
    }

    #[test]
    fn no_daemon_is_the_uniform_text_with_exit_code_seven_in_both_variants() {
        let failure =
            orch_client::NoDaemon::no_daemon("/p", orch_proto::error::NoDaemonDetail::NoSocket);
        for agent_context in [false, true] {
            let result = encode(
                Err(ToolError::Client(failure.clone().into())),
                agent_context,
            );
            let json = serde_json::to_value(&result).unwrap();
            assert_eq!(json["isError"], true);
            assert_eq!(json["content"][0]["text"], failure.render(agent_context));
            assert_eq!(
                json["structuredContent"],
                serde_json::json!({"exit_code": 7})
            );
        }
        assert!(failure.render(true).contains("Do not start it"));
        assert!(failure.render(false).contains("orch daemon start"));
    }
}
