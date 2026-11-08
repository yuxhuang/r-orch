//! Resources: a static inventory plus four templates (design D15).
//!
//! Every URI is matched exactly — `orch` scheme, a known collection as the
//! authority, at most one id segment — and never joined to a filesystem path.
//! The id grammar is deliberately narrow so no URL parser is needed: ASCII
//! letters, digits, `_` and `-`, plus for agents one `@` and a nonempty alias
//! in the same alphabet. Percent escapes are refused outright rather than
//! decoded inconsistently.

use orch_core::error::Error;
use orch_domain::DomainError;
use orch_engine::Engine;
use rmcp::ErrorData;
use rmcp::model::{Resource, ResourceTemplate};
use serde_json::Value;

use super::projections;

pub(crate) const MIME_JSON: &str = "application/json";

const SCHEME: &str = "orch://";

/// The resource a valid URI names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Status,
    Tasks,
    Task(String),
    Goals,
    Goal(String),
    Agents,
    Agent(String),
    Context,
    Inbox(String),
}

/// `resources/list`: the five collections.
pub(crate) fn inventory() -> Vec<Resource> {
    [
        (
            "orch://status",
            "status",
            "Tasks, local agents and orchestrator state",
        ),
        ("orch://tasks", "tasks", "Every task"),
        ("orch://goals", "goals", "Every goal"),
        (
            "orch://agents",
            "agents",
            "Local agents plus remote snapshots",
        ),
        ("orch://context", "context", "Live shared context entries"),
    ]
    .into_iter()
    .map(|(uri, name, description)| {
        Resource::new(uri, name)
            .with_description(description)
            .with_mime_type(MIME_JSON)
    })
    .collect()
}

/// `resources/templates/list`: the four entity templates.
pub(crate) fn templates() -> Vec<ResourceTemplate> {
    [
        ("orch://tasks/{id}", "task", "One task"),
        (
            "orch://goals/{id}",
            "goal",
            "One goal with its linked tasks and progress",
        ),
        (
            "orch://agents/{id}",
            "agent",
            "One agent (local, or id@alias for a remote)",
        ),
        (
            "orch://inbox/{agent}",
            "inbox",
            "An agent's pending messages, not marked delivered",
        ),
    ]
    .into_iter()
    .map(|(template, name, description)| {
        ResourceTemplate::new(template, name)
            .with_description(description)
            .with_mime_type(MIME_JSON)
    })
    .collect()
}

/// Route a concrete URI. Bad syntax is invalid params; a well-formed URI
/// naming nothing this server has is not found.
pub(crate) fn parse(uri: &str) -> Result<Target, ErrorData> {
    let invalid = |reason: &str| {
        ErrorData::invalid_params(format!("Invalid resource URI {uri:?}: {reason}"), None)
    };
    let Some(rest) = uri.strip_prefix(SCHEME) else {
        return Err(invalid("expected an orch:// URI"));
    };
    if !uri.is_ascii() {
        return Err(invalid("non-ASCII characters are not allowed"));
    }
    if let Some(bad) = rest
        .chars()
        .find(|c| matches!(c, '%' | '?' | '#' | '\\' | ':' | '\0'))
    {
        return Err(invalid(&format!("{bad:?} is not allowed")));
    }
    let mut segments = rest.split('/');
    let collection = segments.next().unwrap_or_default();
    let id = segments.next();
    if segments.next().is_some() {
        return Err(invalid("too many path segments"));
    }
    if collection.is_empty() {
        return Err(invalid("missing collection"));
    }
    if collection.contains('@') {
        return Err(invalid("userinfo is not allowed"));
    }
    let target = match (collection, id) {
        ("status", None) => Target::Status,
        ("tasks", None) => Target::Tasks,
        ("goals", None) => Target::Goals,
        ("agents", None) => Target::Agents,
        ("context", None) => Target::Context,
        ("tasks", Some(id)) => Target::Task(entity_id(id, false).map_err(invalid)?),
        ("goals", Some(id)) => Target::Goal(entity_id(id, false).map_err(invalid)?),
        ("agents", Some(id)) => Target::Agent(entity_id(id, true).map_err(invalid)?),
        ("inbox", Some(id)) => Target::Inbox(entity_id(id, true).map_err(invalid)?),
        _ => return Err(not_found(uri)),
    };
    Ok(target)
}

fn entity_id(segment: &str, qualified: bool) -> Result<String, &'static str> {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if segment.is_empty() {
        return Err("empty id segment");
    }
    let valid = match segment.split_once('@') {
        None => plain(segment),
        Some((id, alias)) => qualified && plain(id) && plain(alias),
    };
    if valid {
        Ok(segment.to_owned())
    } else if segment == "." || segment == ".." {
        Err("dot segments are not allowed")
    } else {
        Err("ids are ASCII letters, digits, '_' and '-' (agents may add @alias)")
    }
}

pub(crate) fn not_found(uri: &str) -> ErrorData {
    ErrorData::resource_not_found(format!("Resource not found: {uri}"), None)
}

/// Read a routed resource through the shared projections. A missing entity
/// is not found; any other core failure is an internal error with the
/// message and hint.
pub(crate) async fn read(engine: &Engine, uri: &str, target: &Target) -> Result<Value, ErrorData> {
    let result = match target {
        Target::Status => projections::status(engine).await,
        Target::Tasks => projections::tasks(engine, None).await,
        Target::Task(id) => projections::task(engine, id).await,
        Target::Goals => projections::goals(engine, None).await,
        Target::Goal(id) => projections::goal(engine, id).await,
        Target::Agents => projections::agents(engine).await,
        Target::Agent(id) => match projections::find_agent(engine, id, false).await {
            Ok(Some(value)) => Ok(value),
            Ok(None) => return Err(not_found(uri)),
            Err(err) => Err(err),
        },
        Target::Context => projections::context(engine).await,
        Target::Inbox(agent) => projections::inbox(engine, agent).await,
    };
    result.map_err(|err| core_error(uri, &err))
}

/// A core failure while reading `uri`.
pub(crate) fn core_error(uri: &str, err: &Error) -> ErrorData {
    match err {
        Error::Domain(
            DomainError::TaskNotFound(_)
            | DomainError::GoalNotFound(_)
            | DomainError::AgentNotFound(_),
        ) => ErrorData::resource_not_found(format!("{err} ({uri})"), None),
        _ => ErrorData::internal_error(super::registry::error_text(err), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;

    #[test]
    fn the_inventory_is_five_collections_and_four_templates() {
        let uris: Vec<String> = inventory().into_iter().map(|r| r.uri).collect();
        assert_eq!(
            uris,
            [
                "orch://status",
                "orch://tasks",
                "orch://goals",
                "orch://agents",
                "orch://context"
            ]
        );
        let templates: Vec<String> = templates().into_iter().map(|t| t.uri_template).collect();
        assert_eq!(
            templates,
            [
                "orch://tasks/{id}",
                "orch://goals/{id}",
                "orch://agents/{id}",
                "orch://inbox/{agent}"
            ]
        );
    }

    #[test]
    fn valid_uris_route_exactly() {
        let table = [
            ("orch://status", Target::Status),
            ("orch://tasks", Target::Tasks),
            ("orch://tasks/tsk_a-B9", Target::Task("tsk_a-B9".into())),
            ("orch://goals", Target::Goals),
            ("orch://goals/goal_x", Target::Goal("goal_x".into())),
            ("orch://agents", Target::Agents),
            ("orch://agents/agt_1", Target::Agent("agt_1".into())),
            (
                "orch://agents/agt_1@peer-1",
                Target::Agent("agt_1@peer-1".into()),
            ),
            ("orch://context", Target::Context),
            ("orch://inbox/agt_1", Target::Inbox("agt_1".into())),
            ("orch://inbox/agt_1@w1", Target::Inbox("agt_1@w1".into())),
        ];
        for (uri, target) in table {
            assert_eq!(parse(uri).expect(uri), target, "{uri}");
        }
    }

    #[test]
    fn malformed_uris_are_invalid_params() {
        for uri in [
            "file:///etc/passwd",
            "ORCH://status",
            "orch:status",
            "orch://",
            "orch://tasks/",
            "orch://tasks/a/b",
            "orch://tasks/..",
            "orch://tasks/.",
            "orch://tasks/a%2Fb",
            "orch://tasks/%2e%2e",
            "orch://tasks?x=1",
            "orch://tasks#frag",
            "orch://user@tasks",
            "orch://tasks:80",
            "orch://tasks/a\\b",
            "orch://tasks/a\0",
            "orch://tasks/é",
            "orch://tasks/a b",
            "orch://tasks/tsk_1@alias",
            "orch://agents/a@b@c",
            "orch://agents/a@",
            "orch://agents/@a",
            "orch://inbox/",
        ] {
            let err = parse(uri).expect_err(uri);
            assert_eq!(
                err.code,
                ErrorCode::INVALID_PARAMS,
                "{uri}: {}",
                err.message
            );
        }
    }

    #[test]
    fn well_formed_unknown_resources_are_not_found() {
        for uri in [
            "orch://nope",
            "orch://status/x",
            "orch://context/k",
            "orch://inbox",
            "orch://messages/m",
        ] {
            let err = parse(uri).expect_err(uri);
            assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND, "{uri}");
        }
    }

    #[test]
    fn a_missing_entity_is_not_found_and_other_failures_are_internal() {
        let missing = Error::from(DomainError::TaskNotFound("tsk_x".into()));
        assert_eq!(
            core_error("orch://tasks/tsk_x", &missing).code,
            ErrorCode::RESOURCE_NOT_FOUND
        );
        let other = Error::from(DomainError::NotInitialized);
        let err = core_error("orch://status", &other);
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(err.message, "Not initialized\nHint: Run: orch init");
    }
}
