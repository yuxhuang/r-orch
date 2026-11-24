//! Agent, Team and Run detail bytes. SQL projections and reachability belong
//! to the selecting transaction; this module never installs a reference row.
use super::task_format::{
    codec::{Detail, UniqueValue},
    text::{self, Chunk, Reference, Tag, TextError},
};
use crate::blob::{BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use orch_domain::{Agent, Run, Team};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub(super) enum FormatError {
    #[error("stored entity detail failed {check} at {locator}")]
    Invalid {
        check: &'static str,
        locator: String,
    },
    #[error("entity object access failed at {locator}: {kind:?}")]
    Access {
        kind: std::io::ErrorKind,
        locator: String,
    },
    #[error(transparent)]
    Publish(#[from] TextError),
}
type Result<T> = std::result::Result<T, FormatError>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Agent,
    Team,
    Run,
}

fn invalid(check: &'static str, locator: &str) -> FormatError {
    FormatError::Invalid {
        check,
        locator: locator.into(),
    }
}

pub(super) fn encode_agent(store: &DiskBlobStore, agent: &Agent) -> Result<Detail> {
    encode(store, Family::Agent, agent)
}
pub(super) fn encode_team(store: &DiskBlobStore, team: &Team) -> Result<Detail> {
    encode(store, Family::Team, team)
}
pub(super) fn encode_run(store: &DiskBlobStore, run: &Run) -> Result<Detail> {
    encode(store, Family::Run, run)
}
pub(super) fn decode_agent(store: &DiskBlobStore, detail: &Detail) -> Result<Agent> {
    serde_json::from_value(decode(store, Family::Agent, detail)?)
        .map_err(|_| invalid("typed detail", ""))
}
pub(super) fn decode_team(store: &DiskBlobStore, detail: &Detail) -> Result<Team> {
    serde_json::from_value(decode(store, Family::Team, detail)?)
        .map_err(|_| invalid("typed detail", ""))
}
pub(super) fn decode_run(store: &DiskBlobStore, detail: &Detail) -> Result<Run> {
    serde_json::from_value(decode(store, Family::Run, detail)?)
        .map_err(|_| invalid("typed detail", ""))
}

fn encode(store: &DiskBlobStore, family: Family, entity: &impl Serialize) -> Result<Detail> {
    let mut value = serde_json::to_value(entity).map_err(|_| invalid("typed encoding", ""))?;
    if family == Family::Agent
        && let Some(env) = value.pointer_mut("/config/env")
    {
        let entries = env
            .as_object()
            .ok_or_else(|| invalid("environment type", "/config/env"))?;
        *env = Value::Array(
            entries
                .iter()
                .map(|(key, value)| Value::Array(vec![Value::String(key.clone()), value.clone()]))
                .collect(),
        );
    }
    let mut refs = BTreeMap::new();
    for locator in slots(family, &value)? {
        let slot = value
            .pointer_mut(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        let bytes = slot
            .as_str()
            .ok_or_else(|| invalid("text type", &locator))?
            .as_bytes();
        let prompt = family == Family::Run && locator == "/prompt";
        if bytes.len() > INLINE_TEXT_MAX_BYTES || prompt {
            let (reference, chunks) = if prompt && bytes.len() <= INLINE_TEXT_MAX_BYTES {
                let hash = store.put(bytes).map_err(TextError::from)?;
                (
                    Reference {
                        sha256: hash.to_string(),
                        bytes: bytes.len() as u64,
                        chunks: 1,
                    },
                    vec![Chunk {
                        ordinal: 0,
                        sha256: hash.to_string(),
                        size: bytes.len() as u64,
                    }],
                )
            } else {
                text::publish(store, bytes)?
            };
            *slot = serde_json::to_value(Tag { reference })
                .map_err(|_| invalid("tag encoding", &locator))?;
            refs.insert(locator, chunks);
        }
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| invalid("detail encoding", ""))?;
    let sha256 = BlobHash::of(&bytes).to_string();
    let inline = if bytes.len() <= INLINE_TEXT_MAX_BYTES {
        Some(String::from_utf8(bytes.clone()).map_err(|_| invalid("detail UTF-8", ""))?)
    } else {
        let (_, chunks) = text::publish(store, &bytes)?;
        refs.insert(String::new(), chunks);
        None
    };
    Ok(Detail {
        inline,
        bytes: bytes.len() as u64,
        sha256,
        refs,
    })
}

/// The SQL caller checks format, native projections, metadata and project-scoped
/// blob registration in its selecting snapshot. All locators here are schema-owned.
fn decode(store: &DiskBlobStore, family: Family, detail: &Detail) -> Result<Value> {
    let bytes = match &detail.inline {
        Some(text) if text.len() <= INLINE_TEXT_MAX_BYTES && text.len() as u64 == detail.bytes => {
            text.as_bytes().to_vec()
        }
        Some(_) => return Err(invalid("inline detail size", "")),
        None => read(
            store,
            &Reference {
                sha256: detail.sha256.clone(),
                bytes: detail.bytes,
                chunks: detail.bytes.div_ceil(MAX_BLOB_BYTES),
            },
            detail.refs.get("").map(Vec::as_slice).unwrap_or_default(),
            "",
            false,
        )?,
    };
    if bytes.len() as u64 != detail.bytes || BlobHash::of(&bytes).to_string() != detail.sha256 {
        return Err(invalid("detail length or hash", ""));
    }
    let mut value = serde_json::from_slice::<UniqueValue>(&bytes)
        .map_err(|_| invalid("strict JSON", ""))?
        .0;
    let mut used = BTreeSet::new();
    if detail.inline.is_none() {
        used.insert(String::new());
    }
    let mut tags = Vec::new();
    for locator in slots(family, &value)? {
        let slot = value
            .pointer(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        let prompt = family == Family::Run && locator == "/prompt";
        if let Some(text) = slot.as_str() {
            if text.len() > INLINE_TEXT_MAX_BYTES || prompt {
                return Err(invalid("inline text size or prompt", &locator));
            }
        } else {
            let tag: Tag = serde_json::from_value(slot.clone())
                .map_err(|_| invalid("reference tag", &locator))?;
            check_reference(
                &tag.reference,
                detail
                    .refs
                    .get(&locator)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &locator,
                prompt,
            )?;
            used.insert(locator.clone());
            tags.push((locator, tag));
        }
    }
    if detail.refs.keys().cloned().collect::<BTreeSet<_>>() != used {
        return Err(invalid("reference locators", ""));
    }
    // Prove inline types before opening slot objects; root reconstruction above
    // necessarily precedes schema inspection. Environment names are special:
    // placeholders must not invent collisions with real map names.
    let mut preflight = value.clone();
    for (locator, _) in &tags {
        *preflight
            .pointer_mut(locator)
            .ok_or_else(|| invalid("text slot", locator))? = Value::String(String::new());
    }
    if family == Family::Agent && preflight.pointer("/config/env").is_some() {
        *preflight
            .pointer_mut("/config/env")
            .ok_or_else(|| invalid("environment type", "/config/env"))? = Value::Array(vec![]);
    }
    domain_value(family, preflight)?;
    for (locator, tag) in tags {
        let bytes = read(
            store,
            &tag.reference,
            detail
                .refs
                .get(&locator)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &locator,
            family == Family::Run && locator == "/prompt",
        )?;
        *value
            .pointer_mut(&locator)
            .ok_or_else(|| invalid("text slot", &locator))? =
            Value::String(String::from_utf8(bytes).map_err(|_| invalid("text UTF-8", &locator))?);
    }
    domain_value(family, value)
}

fn check_reference(
    reference: &Reference,
    rows: &[Chunk],
    locator: &str,
    prompt: bool,
) -> Result<()> {
    if prompt && reference.bytes <= INLINE_TEXT_MAX_BYTES as u64 {
        if reference.chunks != 1
            || rows.len() != 1
            || rows[0].ordinal != 0
            || rows[0].size != reference.bytes
            || rows[0].sha256 != reference.sha256
            || reference.sha256.parse::<BlobHash>().is_err()
        {
            return Err(invalid("prompt metadata", locator));
        }
        Ok(())
    } else {
        text::validate_chunks(reference, rows)
            .map(|_| ())
            .map_err(|_| invalid("reference metadata", locator))
    }
}
fn read(
    store: &DiskBlobStore,
    reference: &Reference,
    rows: &[Chunk],
    locator: &str,
    prompt: bool,
) -> Result<Vec<u8>> {
    check_reference(reference, rows, locator, prompt)?;
    let bytes = if prompt && reference.bytes <= INLINE_TEXT_MAX_BYTES as u64 {
        let hash = reference
            .sha256
            .parse::<BlobHash>()
            .map_err(|_| invalid("prompt hash", locator))?;
        store.get(&hash).map_err(|error| match error {
            crate::blob::BlobError::Io(error)
            | crate::blob::BlobError::Confined(crate::data_dir::confined::ConfinedError::Io(
                error,
            )) => FormatError::Access {
                kind: error.kind(),
                locator: locator.into(),
            },
            _ => invalid("prompt object", locator),
        })?
    } else {
        text::reconstruct(store, reference, rows).map_err(|error| match error {
            TextError::Access(kind) => FormatError::Access {
                kind,
                locator: locator.into(),
            },
            _ => invalid("referenced bytes", locator),
        })?
    };
    if bytes.len() as u64 != reference.bytes || BlobHash::of(&bytes).to_string() != reference.sha256
    {
        return Err(invalid("object length or hash", locator));
    }
    Ok(bytes)
}

fn domain_value(family: Family, mut value: Value) -> Result<Value> {
    if family == Family::Agent
        && let Some(env) = value.pointer_mut("/config/env")
    {
        let mut entries = Map::new();
        for pair in array(env, "/config/env")? {
            let pair = array(pair, "/config/env")?;
            if pair.len() != 2 {
                return Err(invalid("environment pair", "/config/env"));
            }
            let key = pair[0]
                .as_str()
                .ok_or_else(|| invalid("environment name", "/config/env"))?;
            if !pair[1].is_string() || entries.insert(key.into(), pair[1].clone()).is_some() {
                return Err(invalid("environment value or duplicate", "/config/env"));
            }
        }
        *env = Value::Object(entries);
    }
    let valid = match family {
        Family::Agent => serde_json::from_value::<Agent>(value.clone()).is_ok(),
        Family::Team => serde_json::from_value::<Team>(value.clone()).is_ok(),
        Family::Run => serde_json::from_value::<Run>(value.clone()).is_ok(),
    };
    if !valid {
        return Err(invalid("typed detail", ""));
    }
    Ok(value)
}

fn object<'a>(
    value: &'a Value,
    path: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("object type", path))?;
    if required.iter().any(|key| !object.contains_key(*key))
        || object.iter().any(|(key, value)| {
            value.is_null()
                || (!required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
        })
    {
        return Err(invalid("object fields", path));
    }
    Ok(object)
}
fn array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>> {
    value.as_array().ok_or_else(|| invalid("array type", path))
}
fn list(value: &Value, path: &str, slots: &mut Vec<String>) -> Result<()> {
    for (i, _) in array(value, path)?.iter().enumerate() {
        slots.push(format!("{path}/{i}"));
    }
    Ok(())
}
fn slots(family: Family, value: &Value) -> Result<Vec<String>> {
    let (required, optional): (&[&str], &[&str]) = match family {
        Family::Agent => (
            &["id", "name", "adapter", "config", "status", "stats"],
            &[
                "role",
                "current_task",
                "autonomous",
                "last_error",
                "capabilities",
                "remote",
            ],
        ),
        Family::Team => (
            &[
                "id",
                "name",
                "status",
                "members",
                "task_pool",
                "lead_agent_id",
                "created_at",
                "updated_at",
                "config",
            ],
            &["description"],
        ),
        Family::Run => (
            &[
                "id",
                "task_id",
                "agent_id",
                "attempt",
                "status",
                "started_at",
                "workspace_path",
                "prompt",
            ],
            &["finished_at", "pid", "error", "error_kind", "tokens"],
        ),
    };
    let root = object(value, "", required, optional)?;
    let mut slots = Vec::new();
    for (key, value) in root {
        let path = format!("/{key}");
        match key.as_str() {
            "autonomous" | "attempt" | "pid" => (),
            "capabilities" | "task_pool" => list(value, &path, &mut slots)?,
            "config" if family == Family::Agent => {
                let config = object(
                    value,
                    &path,
                    &[],
                    &[
                        "command",
                        "model",
                        "effort",
                        "approval_policy",
                        "max_turns",
                        "timeout_ms",
                        "stall_timeout_ms",
                        "env",
                        "system_prompt",
                        "workspace_mode",
                        "skills",
                        "prompt_mode",
                        "role_summary",
                    ],
                )?;
                for (key, value) in config {
                    let path = format!("/config/{key}");
                    match key.as_str() {
                        "max_turns" | "timeout_ms" | "stall_timeout_ms" => (),
                        "skills" => list(value, &path, &mut slots)?,
                        "env" => {
                            let mut names = BTreeSet::new();
                            for (i, pair) in array(value, &path)?.iter().enumerate() {
                                let pair = array(pair, &path)?;
                                if pair.len() != 2 {
                                    return Err(invalid("environment pair", &path));
                                }
                                if let Some(name) = pair[0].as_str()
                                    && !names.insert(name)
                                {
                                    return Err(invalid("environment duplicate", &path));
                                }
                                slots.push(format!("{path}/{i}/0"));
                                slots.push(format!("{path}/{i}/1"));
                            }
                        }
                        _ => slots.push(path),
                    }
                }
            }
            "config" => {
                object(
                    value,
                    &path,
                    &["auto_claim"],
                    &["max_concurrent_tasks", "message_ttl_ms"],
                )?;
            }
            "stats" => {
                object(
                    value,
                    &path,
                    &[
                        "tasks_completed",
                        "tasks_failed",
                        "total_runs",
                        "total_runtime_ms",
                    ],
                    &["tokens_used"],
                )?;
            }
            "tokens" => {
                object(
                    value,
                    &path,
                    &[
                        "input",
                        "output",
                        "reasoning",
                        "total",
                        "cache_read",
                        "cache_write",
                    ],
                    &[],
                )?;
            }
            "last_error" => {
                object(value, &path, &["message", "kind", "timestamp"], &[])?;
                for key in ["message", "kind", "timestamp"] {
                    slots.push(format!("{path}/{key}"));
                }
            }
            "remote" => {
                object(value, &path, &["alias", "online"], &[])?;
                slots.push(format!("{path}/alias"));
            }
            "members" => {
                for (i, member) in array(value, &path)?.iter().enumerate() {
                    let path = format!("{path}/{i}");
                    object(member, &path, &["agent_id", "role", "joined_at"], &[])?;
                    for key in ["agent_id", "role", "joined_at"] {
                        slots.push(format!("{path}/{key}"));
                    }
                }
            }
            _ => slots.push(path),
        }
    }
    Ok(slots)
}

#[cfg(test)]
mod tests;
