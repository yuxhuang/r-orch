//! Agent, Team and Run detail bytes and private native persistence. Every SQL
//! operation borrows the selecting snapshot or authorized caller transaction.
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
pub(super) enum Family {
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
mod tests {
    // Keep the qualified prerequisite corpus byte-for-byte while native tests
    // grow separately in this bounded leaf.
    include!("tests.rs");

    fn native_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::sqlite::migrations::INITIAL_SCHEMA)
            .unwrap();
        conn.execute_batch(crate::sqlite::migrations::ATTENTION_FACTS)
            .unwrap();
        conn.execute_batch(crate::sqlite::migrations::TASK_FORMAT)
            .unwrap();
        conn.execute_batch(crate::sqlite::migrations::AGENT_TEAM_RUN)
            .unwrap();
        conn
    }

    #[test]
    fn native_create_explicitly_projects_status_attempt_time_and_advancing_run_positions() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let mut a = agent();
        a.status = orch_domain::AgentStatus::Running;
        let saved = sql.create(Entity::Agent(Box::new(a))).unwrap();
        assert_eq!(saved.position, 1);
        assert_eq!(
            tx.query_row("SELECT status FROM agents", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "running"
        );
        for (id, attempt, position) in [("r", 10, 1), ("s", 2, 2)] {
            let mut r = run();
            r.id = id.into();
            r.attempt = attempt;
            let saved = sql.create(Entity::Run(Box::new(r))).unwrap();
            assert_eq!(saved.position, position);
            let actual: (String, u32, i64) = tx
                .query_row(
                    "SELECT started_at_text,attempt,position FROM runs WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(actual, ("opaque".into(), attempt, position as i64));
        }
        assert_eq!(
            tx.query_row(
                "SELECT value FROM counters WHERE name='position:run'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }

    fn selection(
        filter: &orch_core::store::Filter,
        sort: orch_core::store::Sort,
    ) -> NativeSelection<'_> {
        NativeSelection {
            filter,
            sort,
            name: None,
            member: None,
        }
    }
    fn page_ids(page: &NativePage) -> Vec<String> {
        page.items
            .iter()
            .map(|r| r.entity.id().to_owned())
            .collect()
    }

    #[test]
    fn native_selection_non_utf8_identity_is_typed_corrupt_with_stored_form() {
        for family in [Family::Agent, Family::Team, Family::Run] {
            for form in [0, 1] {
                let (_temp, data) = crate::sqlite::test_data();
                let disk = DiskBlobStore::open(data.root()).unwrap();
                let mut conn = native_conn();
                conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
                    .unwrap();
                let tx = conn.transaction().unwrap();
                let mut sql = NativeSql::new(&tx, &disk, "p", 123);
                let id = if form == 0 {
                    "bad".into()
                } else {
                    "z".repeat(65537)
                };
                let filter = match family {
                    Family::Agent => Filter::Agents,
                    Family::Team => Filter::Teams,
                    Family::Run => Filter::Runs {
                        task: None,
                        agent: None,
                    },
                };
                for id in ["good".to_owned(), id.clone()] {
                    let entity = match family {
                        Family::Agent => {
                            let mut a = agent();
                            a.id = id;
                            Entity::Agent(Box::new(a))
                        }
                        Family::Team => {
                            let mut t = team();
                            t.id = id;
                            Entity::Team(Box::new(t))
                        }
                        Family::Run => {
                            let mut r = run();
                            r.id = id;
                            Entity::Run(Box::new(r))
                        }
                    };
                    sql.create(entity).unwrap();
                }
                let (_, stored) = key(&id);
                assert_eq!(
                    tx.execute(
                        &format!(
                            "UPDATE {} SET id=CAST(X'FF61' AS TEXT) WHERE id_form=?1 AND id=?2",
                            family.table()
                        ),
                        params![form, stored],
                    )
                    .unwrap(),
                    1
                );
                assert_eq!(
                    tx.query_row(
                        &format!("SELECT COUNT(*) FROM {}", family.table()),
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    2
                );
                // No valid first item may escape ahead of a corrupt later candidate.
                let error = sql
                    .select(selection(&filter, Sort::Default), None, 1)
                    .err()
                    .unwrap();
                assert!(
                    matches!(error, NativeError::Corrupt { family: actual, form: actual_form, check: "text encoding", ref locator }
                    if actual == family.name() && actual_form == form && locator == "native/id"),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn native_selection_non_utf8_detail_is_typed_corrupt_with_stored_form() {
        for family in [Family::Agent, Family::Team, Family::Run] {
            for form in [0, 1] {
                let (_temp, data) = crate::sqlite::test_data();
                let disk = DiskBlobStore::open(data.root()).unwrap();
                let mut conn = native_conn();
                conn.execute_batch("PRAGMA ignore_check_constraints=ON")
                    .unwrap();
                let tx = conn.transaction().unwrap();
                let mut sql = NativeSql::new(&tx, &disk, "p", 123);
                let id = if form == 0 {
                    "literal".into()
                } else {
                    "z".repeat(65537)
                };
                let (entity, filter) = match family {
                    Family::Agent => {
                        let mut a = agent();
                        a.id = id;
                        (Entity::Agent(Box::new(a)), Filter::Agents)
                    }
                    Family::Team => {
                        let mut t = team();
                        t.id = id;
                        (Entity::Team(Box::new(t)), Filter::Teams)
                    }
                    Family::Run => {
                        let mut r = run();
                        r.id = id;
                        (
                            Entity::Run(Box::new(r)),
                            Filter::Runs {
                                task: None,
                                agent: None,
                            },
                        )
                    }
                };
                sql.create(entity).unwrap();
                assert_eq!(
                    tx.execute("UPDATE entity_details SET detail=CAST(X'FF61' AS TEXT)", [])
                        .unwrap(),
                    1
                );
                let error = sql
                    .select(selection(&filter, Sort::Default), None, 1)
                    .err()
                    .unwrap();
                assert!(
                    matches!(error, NativeError::Corrupt { family: actual, form: actual_form, check: "text encoding", ref locator }
                    if actual == family.name() && actual_form == form && locator == "native/detail/ref"),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn native_selection_refuses_poisoned_helper() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        sql.poisoned = true;
        assert!(matches!(
            sql.select(selection(&Filter::Agents, Sort::Default), None, 10),
            Err(NativeError::Aborted)
        ));
    }

    #[test]
    fn native_selection_team_name_order_limit_1000_and_total_before_anchor() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (id, name) in [("a", "z"), ("b", "y"), ("c", "x")] {
            let mut t = team();
            t.id = id.into();
            t.name = name.into();
            sql.create(Entity::Team(Box::new(t))).unwrap();
        }
        let teams = Filter::Teams;
        let first = sql
            .select(selection(&teams, Sort::Name), None, 1000)
            .unwrap();
        assert_eq!(page_ids(&first), ["c", "b", "a"]);
        assert_eq!(first.total, 3);
        assert!(!first.has_more);
        let second = sql
            .select(selection(&teams, Sort::Name), first.items.first(), 1)
            .unwrap();
        assert_eq!(page_ids(&second), ["b"]);
        assert_eq!(second.total, 3);
        assert!(second.has_more);
        let last = sql
            .select(selection(&teams, Sort::Name), second.items.first(), 1)
            .unwrap();
        assert_eq!(page_ids(&last), ["a"]);
        assert_eq!(last.total, 3);
        assert!(!last.has_more);
    }

    #[test]
    fn native_selection_anchor_must_match_name_member_and_run_filters() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let a = sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        let t = sql.create(Entity::Team(Box::new(team()))).unwrap();
        let r = sql.create(Entity::Run(Box::new(run()))).unwrap();
        let agents = Filter::Agents;
        let teams = Filter::Teams;
        for (filter, anchor) in [(&agents, &a), (&teams, &t)] {
            let mut q = selection(filter, Sort::Default);
            q.name = Some("other");
            assert!(matches!(
                sql.select(q, Some(anchor), 1),
                Err(NativeError::Store(StoreError::InvalidCursor))
            ));
        }
        let mut q = selection(&teams, Sort::Default);
        q.member = Some("absent");
        assert!(matches!(
            sql.select(q, Some(&t), 1),
            Err(NativeError::Store(StoreError::InvalidCursor))
        ));
        for filter in [
            Filter::Runs {
                task: Some("other".into()),
                agent: None,
            },
            Filter::Runs {
                task: None,
                agent: Some("other".into()),
            },
        ] {
            assert!(matches!(
                sql.select(selection(&filter, Sort::Default), Some(&r), 1),
                Err(NativeError::Store(StoreError::InvalidCursor))
            ));
        }
    }

    #[test]
    fn native_selection_indirect_detail_cannot_alias_a_literal_identity() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let literal = agent();
        let mut indirect = literal.clone();
        indirect.id = "z".repeat(65537);
        sql.create(Entity::Agent(Box::new(literal.clone())))
            .unwrap();
        sql.create(Entity::Agent(Box::new(indirect))).unwrap();
        let detail = encode_agent(&disk, &literal).unwrap();
        tx.execute(
            "DELETE FROM entity_blob_refs WHERE family='agent' AND key_form=1",
            [],
        )
        .unwrap();
        assert_eq!(tx.execute("UPDATE entity_details SET detail=?1,detail_bytes=?2,detail_sha256=?3 WHERE family='agent' AND key_form=1", params![detail.inline, detail.bytes as i64, detail.sha256]).unwrap(), 1);
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        // Re-looking up decoded 'a' would duplicate the literal row without this precheck.
        assert!(
            matches!(sql.select(selection(&Filter::Agents, Sort::Id), None, 100), Err(NativeError::Corrupt { family: "agent", form: 1, check: "identity projection", ref locator }) if locator == "id")
        );
    }

    #[test]
    fn native_selection_team_name_filter_without_member_filter() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (id, name) in [("a", "yes"), ("b", "no"), ("c", "yes")] {
            let mut t = team();
            t.id = id.into();
            t.name = name.into();
            sql.create(Entity::Team(Box::new(t))).unwrap();
        }
        let teams = Filter::Teams;
        let mut q = selection(&teams, Sort::Id);
        q.name = Some("yes");
        let page = sql.select(q, None, 100).unwrap();
        assert_eq!(page_ids(&page), ["a", "c"]);
        assert_eq!(page.total, 2);
    }

    #[test]
    fn native_selection_runs_refuse_name_sort_and_name_filter() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        sql.create(Entity::Run(Box::new(run()))).unwrap();
        for filter in [
            Filter::Runs {
                task: None,
                agent: None,
            },
            Filter::Runs {
                task: Some("t".into()),
                agent: None,
            },
        ] {
            assert!(matches!(
                sql.select(selection(&filter, Sort::Name), None, 1),
                Err(NativeError::Store(StoreError::InvalidPage(_)))
            ));
            let mut q = selection(&filter, Sort::Default);
            q.name = Some("name");
            assert!(matches!(
                sql.select(q, None, 1),
                Err(NativeError::Store(StoreError::InvalidPage(_)))
            ));
        }
    }

    #[test]
    fn native_selection_team_member_matches_any_of_distinct_members() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (id, members) in [
            ("a", vec!["other", "wanted"]),
            ("b", vec!["other"]),
            ("c", vec![]),
        ] {
            let mut t = team();
            t.id = id.into();
            t.members = serde_json::from_value(Value::Array(
                members
                    .into_iter()
                    .map(|id| json!({"agent_id": id, "role": "member", "joined_at": ""}))
                    .collect(),
            ))
            .unwrap();
            sql.create(Entity::Team(Box::new(t))).unwrap();
        }
        let teams = Filter::Teams;
        let mut q = selection(&teams, Sort::Id);
        q.member = Some("wanted");
        let page = sql.select(q, None, 100).unwrap();
        assert_eq!(page_ids(&page), ["a"]);
        assert_eq!(page.total, 1);
    }

    #[test]
    fn native_selection_numeric_attempts_task_agent_and_deleted_anchor() {
        use orch_core::store::{Filter, Sort};
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (id, attempt, started, task, agent) in [
            ("z", 2, "zzz", "t", "a"),
            ("b", 10, "aaa", "t", "a"),
            ("a", 10, "bbb", "t", "a"),
            ("x", 99, "xxx", "other", "a"),
            ("q", 20, "qqq", "t", "other"),
        ] {
            let mut r = run();
            r.id = id.into();
            r.attempt = attempt;
            r.started_at = started.into();
            r.task_id = task.into();
            r.agent_id = agent.into();
            sql.create(Entity::Run(Box::new(r))).unwrap();
        }
        for agent in [None, Some("a".into())] {
            let filter = Filter::Runs {
                task: Some("t".into()),
                agent: agent.clone(),
            };
            let expected = if agent.is_some() {
                vec!["a", "b", "z"]
            } else {
                vec!["q", "a", "b", "z"]
            };
            for sort in [Sort::Default, Sort::Attempt] {
                let page = sql.select(selection(&filter, sort), None, 100).unwrap();
                assert_eq!(page_ids(&page), expected);
                assert_eq!(page.total, expected.len() as u64);
            }
        }
        let filter = Filter::Runs {
            task: Some("t".into()),
            agent: Some("a".into()),
        };
        let first = sql
            .select(selection(&filter, Sort::Default), None, 1)
            .unwrap();
        assert_eq!(page_ids(&first), ["a"]);
        assert!(first.has_more);
        assert_eq!(first.total, 3);
        let anchor = &first.items[0];
        sql.delete(Family::Run, anchor.entity.id(), anchor.revision)
            .unwrap();
        let next = sql
            .select(selection(&filter, Sort::Default), Some(anchor), 1)
            .unwrap();
        assert_eq!(page_ids(&next), ["b"]);
        assert!(next.has_more);
        assert_eq!(next.total, 2);
        let last = sql
            .select(selection(&filter, Sort::Default), next.items.first(), 1)
            .unwrap();
        assert_eq!(page_ids(&last), ["z"]);
        assert!(!last.has_more);
        let filter = Filter::Runs {
            task: None,
            agent: Some("a".into()),
        };
        assert_eq!(
            page_ids(
                &sql.select(selection(&filter, Sort::Default), None, 100)
                    .unwrap()
            ),
            ["z", "x", "b"]
        );
        assert!(
            sql.select(selection(&filter, Sort::Attempt), None, 1)
                .is_err()
        );
    }

    #[test]
    fn native_selection_original_id_time_and_indirect_filter_collisions() {
        use orch_core::store::{Filter, Sort};
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let long = "z".repeat(65537);
        let digest = BlobHash::of(long.as_bytes()).to_string();
        let indirect_time = "opaque".repeat(14000);
        let times = [
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00+01:00",
            indirect_time.as_str(),
            "é",
        ];
        let ids = [long.clone(), digest.clone(), "A".into(), "a".into()];
        for (i, id) in ids.iter().enumerate() {
            let mut r = run();
            r.id = id.clone();
            r.started_at = times[i].into();
            r.task_id = if i % 2 == 0 {
                long.clone()
            } else {
                digest.clone()
            };
            r.agent_id = r.task_id.clone();
            sql.create(Entity::Run(Box::new(r))).unwrap();
        }
        let all = Filter::Runs {
            task: None,
            agent: None,
        };
        assert_eq!(
            page_ids(
                &sql.select(selection(&all, Sort::Default), None, 100)
                    .unwrap()
            ),
            [
                ids[3].clone(),
                ids[2].clone(),
                ids[1].clone(),
                ids[0].clone()
            ]
        );
        let mut sorted = ids.to_vec();
        sorted.sort();
        let first = sql.select(selection(&all, Sort::Id), None, 2).unwrap();
        let second = sql
            .select(selection(&all, Sort::Id), first.items.last(), 2)
            .unwrap();
        assert_eq!([page_ids(&first), page_ids(&second)].concat(), sorted);
        for (value, expected) in [
            (&long, vec!["A".to_owned(), long.clone()]),
            (&digest, vec![digest.clone(), "a".into()]),
        ] {
            let f = Filter::Runs {
                task: Some(value.clone()),
                agent: Some(value.clone()),
            };
            assert_eq!(
                page_ids(&sql.select(selection(&f, Sort::Default), None, 100).unwrap()),
                expected
            );
        }
        assert!(
            NativeSql::new(&tx, &disk, "other", 123)
                .select(selection(&all, Sort::Default), None, 100)
                .unwrap()
                .items
                .is_empty()
        );
    }

    #[test]
    fn native_selection_agent_names_positions_and_team_members_are_original_bytes() {
        use orch_core::store::{Filter, Sort};
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let long = "z".repeat(65537);
        let digest = BlobHash::of(long.as_bytes()).to_string();
        for (id, name) in [
            ("b", long.as_str()),
            ("a", digest.as_str()),
            ("c", long.as_str()),
        ] {
            let mut a = agent();
            a.id = id.into();
            a.name = name.into();
            sql.create(Entity::Agent(Box::new(a))).unwrap();
        }
        let agents = Filter::Agents;
        assert_eq!(
            page_ids(
                &sql.select(selection(&agents, Sort::Default), None, 100)
                    .unwrap()
            ),
            ["b", "a", "c"]
        );
        assert_eq!(
            page_ids(
                &sql.select(selection(&agents, Sort::Name), None, 100)
                    .unwrap()
            ),
            ["a", "b", "c"]
        );
        let mut q = selection(&agents, Sort::Name);
        q.name = Some(&long);
        assert_eq!(page_ids(&sql.select(q, None, 100).unwrap()), ["b", "c"]);
        let b = sql.get(Family::Agent, "b").unwrap().unwrap();
        sql.move_to_end(Family::Agent, "b", b.revision).unwrap();
        assert_eq!(
            page_ids(
                &sql.select(selection(&agents, Sort::Name), None, 100)
                    .unwrap()
            ),
            ["a", "c", "b"]
        );
        for (id, name, member, created) in [
            ("b", long.as_str(), long.as_str(), "z"),
            ("a", digest.as_str(), digest.as_str(), "a"),
            ("c", long.as_str(), long.as_str(), "m"),
        ] {
            let mut t = team();
            t.id = id.into();
            t.name = name.into();
            t.created_at = created.into();
            t.members=serde_json::from_value(json!([{ "agent_id":member,"role":"member","joined_at":""},{ "agent_id":member,"role":"lead","joined_at":"opaque"}])).unwrap();
            sql.create(Entity::Team(Box::new(t))).unwrap();
        }
        let teams = Filter::Teams;
        assert_eq!(
            page_ids(
                &sql.select(selection(&teams, Sort::Default), None, 100)
                    .unwrap()
            ),
            ["a", "c", "b"]
        );
        assert_eq!(
            page_ids(
                &sql.select(selection(&teams, Sort::Name), None, 100)
                    .unwrap()
            ),
            ["a", "b", "c"]
        );
        let mut q = selection(&teams, Sort::Name);
        q.name = Some(&long);
        q.member = Some(&long);
        let page = sql.select(q, None, 100).unwrap();
        assert_eq!(page_ids(&page), ["b", "c"]);
        assert_eq!(page.total, 2);
        let mut q = selection(&teams, Sort::Name);
        q.member = Some(&digest);
        assert_eq!(page_ids(&sql.select(q, None, 100).unwrap()), ["a"]);
    }

    #[test]
    fn native_selection_full_page_corruption_and_invalid_combinations_fail() {
        use orch_core::store::{Filter, Sort};
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for id in ["a", "b"] {
            let mut a = agent();
            a.id = id.into();
            sql.create(Entity::Agent(Box::new(a))).unwrap();
        }
        let agents = Filter::Agents;
        assert!(
            sql.select(selection(&agents, Sort::Default), None, 0)
                .is_err()
        );
        assert!(
            sql.select(selection(&agents, Sort::Default), None, 1001)
                .is_err()
        );
        assert!(
            sql.select(selection(&agents, Sort::CreatedAt), None, 1)
                .is_err()
        );
        let mut q = selection(&agents, Sort::Default);
        q.member = Some("a");
        assert!(sql.select(q, None, 1).is_err());
        let anchor = sql.create(Entity::Team(Box::new(team()))).unwrap();
        assert!(
            sql.select(selection(&agents, Sort::Default), Some(&anchor), 1)
                .is_err()
        );
        let mut long_agent = agent();
        long_agent.id = "z".repeat(65537);
        sql.create(Entity::Agent(Box::new(long_agent.clone())))
            .unwrap();
        tx.execute(
            "UPDATE entity_details SET detail_sha256=?1 WHERE family='agent' AND key_form=1",
            ["0".repeat(64)],
        )
        .unwrap();
        assert!(matches!(
            sql.select(selection(&agents, Sort::Default), None, 1),
            Err(NativeError::Corrupt {
                family: "agent",
                form: 1,
                ..
            })
        ));
        // Remove only the injected corrupt fixture before testing projection drift.
        tx.execute(
            "DELETE FROM entity_details WHERE family='agent' AND key_form=1",
            [],
        )
        .unwrap();
        tx.execute(
            "DELETE FROM entity_blob_refs WHERE family='agent' AND key_form=1",
            [],
        )
        .unwrap();
        tx.execute("DELETE FROM agents WHERE id_form=1", [])
            .unwrap();
        tx.execute("UPDATE agents SET name='drift' WHERE id='b'", [])
            .unwrap();
        assert!(matches!(
            sql.select(selection(&agents, Sort::Default), None, 1),
            Err(NativeError::Corrupt {
                family: "agent",
                ..
            })
        ));
    }

    #[test]
    fn native_run_prompt_identity_threshold_round_trips() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (size, form) in [(65536, 0), (65537, 1)] {
            let mut r = run();
            r.id = "é".repeat(size / 2) + if size % 2 == 1 { "a" } else { "" };
            r.prompt = "boundary prompt".into();
            sql.create(Entity::Run(Box::new(r.clone()))).unwrap();
            let expected = if form == 0 {
                r.id.clone()
            } else {
                BlobHash::of(r.id.as_bytes()).to_string()
            };
            let stored: (i64, String) = tx
                .query_row(
                    "SELECT id_form,id FROM runs WHERE id_form=?1",
                    [form],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(stored, (form, expected));
            assert_eq!(
                sql.get(Family::Run, &r.id).unwrap().unwrap().entity,
                Entity::Run(Box::new(r))
            );
        }
    }

    #[test]
    fn native_prompt_create_update_and_read_share_manifest_hash_and_revision() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let mut r = run();
        let mut saved = sql.create(Entity::Run(Box::new(r.clone()))).unwrap();
        assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(saved.clone()));
        for prompt in [
            "short".into(),
            "é".repeat(32768),
            "é".repeat(32768) + "x",
            String::new(),
        ] {
            r.prompt = prompt;
            let old_revision = saved.revision;
            saved = sql
                .update(Entity::Run(Box::new(r.clone())), old_revision)
                .unwrap();
            assert_eq!(saved.revision.get(), old_revision.get() + 1);
            assert_eq!(saved.position, 1);
            assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(saved.clone()));
            assert_eq!(
                sql.hydrate(Family::Run, std::slice::from_ref(&r.id))
                    .unwrap(),
                vec![saved.clone()]
            );
            let (hash, count, size): (String, i64, i64) = tx.query_row(
                "SELECT prompt_sha256,(SELECT COUNT(*) FROM entity_blob_refs WHERE project_id='p' AND family='run' AND locator='/prompt'),(SELECT size FROM entity_blob_refs WHERE project_id='p' AND family='run' AND locator='/prompt') FROM runs WHERE project_id='p'",
                [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
            assert_eq!(
                (hash, count, size),
                (
                    BlobHash::of(r.prompt.as_bytes()).to_string(),
                    1,
                    r.prompt.len() as i64
                )
            );
            let before = native_state(&tx);
            assert!(matches!(
                sql.update(Entity::Run(Box::new(run())), old_revision),
                Err(NativeError::Store(StoreError::StaleRevision { .. }))
            ));
            assert_eq!(native_state(&tx), before);
        }
        sql.ensure_committable().unwrap();
        tx.commit().unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            NativeSql::new(&tx, &disk, "p", 124)
                .get(Family::Run, &r.id)
                .unwrap(),
            Some(saved)
        );
    }

    #[test]
    fn native_prompt_multichunk_reader_returns_original_bytes_after_create_and_update() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let mut r = run();
        // The first object's last byte is the first half of a UTF-8 character.
        r.prompt = "x".repeat(MAX_BLOB_BYTES as usize - 1) + "é";
        let first = sql.create(Entity::Run(Box::new(r.clone()))).unwrap();
        assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(first.clone()));
        let rows: Vec<(i64, String, i64)> = tx.prepare("SELECT ordinal,sha256,size FROM entity_blob_refs WHERE family='run' AND locator='/prompt' ORDER BY ordinal").unwrap()
            .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].0, rows[0].2, rows[1].0, rows[1].2),
            (0, MAX_BLOB_BYTES as i64, 1, 1)
        );
        let whole = BlobHash::of(r.prompt.as_bytes());
        assert!(matches!(
            disk.get(&whole),
            Err(crate::blob::BlobError::NotFound(_))
        ));
        assert_eq!(
            tx.query_row::<String, _, _>("SELECT prompt_sha256 FROM runs", [], |row| row.get(0))
                .unwrap(),
            whole.to_string()
        );
        let filter = Filter::Runs {
            task: None,
            agent: None,
        };
        assert_eq!(
            sql.select(selection(&filter, Sort::Default), None, 10)
                .unwrap()
                .items,
            vec![first.clone()]
        );
        // Replacing the prompt uses the same manifest writer, even across object sizes.
        r.prompt = "short".into();
        let short = sql
            .update(Entity::Run(Box::new(r.clone())), first.revision)
            .unwrap();
        assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(short.clone()));
        r.prompt = "x".repeat(MAX_BLOB_BYTES as usize - 1) + "é";
        let large = sql
            .update(Entity::Run(Box::new(r.clone())), short.revision)
            .unwrap();
        assert_eq!(
            sql.hydrate(Family::Run, std::slice::from_ref(&r.id))
                .unwrap(),
            vec![large.clone()]
        );
        sql.ensure_committable().unwrap();
        tx.commit().unwrap();
        let tx = conn.transaction().unwrap();
        let sql = NativeSql::new(&tx, &disk, "p", 124);
        assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(large.clone()));
        tx.execute(
            "UPDATE entity_blob_refs SET ordinal=2 WHERE locator='/prompt' AND ordinal=1",
            [],
        )
        .unwrap();
        assert!(matches!(
            sql.get(Family::Run, &r.id),
            Err(NativeError::Corrupt { .. })
        ));
    }

    #[test]
    fn native_prompt_form_project_isolation_and_outer_rollback_preserve_old_bytes() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let indirect = "é".repeat(32769);
        let literal = BlobHash::of(indirect.as_bytes()).to_string();
        let mut original = Vec::new();
        {
            let tx = conn.transaction().unwrap();
            for project in ["p", "other"] {
                let mut sql = NativeSql::new(&tx, &disk, project, 123);
                for id in [&literal, &indirect] {
                    let mut r = run();
                    r.id = id.clone();
                    r.prompt = format!("{project}-{}", id.len());
                    original.push((project, sql.create(Entity::Run(Box::new(r))).unwrap()));
                }
            }
            tx.commit().unwrap();
        }
        {
            let tx = conn.transaction().unwrap();
            let mut sql = NativeSql::new(&tx, &disk, "p", 124);
            for (_, saved) in &original[..2] {
                let Entity::Run(r) = &saved.entity else {
                    panic!("run fixture")
                };
                let mut changed = r.clone();
                changed.prompt = "replacement".into();
                let updated = sql.update(Entity::Run(changed), saved.revision).unwrap();
                assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(updated));
            }
            for (project, saved) in &original[2..] {
                assert_eq!(
                    NativeSql::new(&tx, &disk, project, 124)
                        .get(Family::Run, saved.entity.id())
                        .unwrap(),
                    Some(saved.clone())
                );
            }
            sql.ensure_committable().unwrap();
            tx.rollback().unwrap();
        }
        let tx = conn.transaction().unwrap();
        for (project, saved) in &original {
            assert_eq!(
                NativeSql::new(&tx, &disk, project, 125)
                    .get(Family::Run, saved.entity.id())
                    .unwrap(),
                Some(saved.clone())
            );
        }
        let mut sql = NativeSql::new(&tx, &disk, "p", 125);
        sql.delete(Family::Run, &indirect, Revision::ONE).unwrap();
        let before = native_state(&tx);
        let mut missing = run();
        missing.id = indirect.clone();
        missing.prompt = "must not overwrite literal sibling".into();
        assert!(matches!(
            sql.update(Entity::Run(Box::new(missing)), Revision::ONE),
            Err(NativeError::Store(StoreError::StaleRevision {
                actual: None,
                ..
            }))
        ));
        assert_eq!(native_state(&tx), before);
        assert!(sql.get(Family::Run, &indirect).unwrap().is_none());
        assert_eq!(
            sql.get(Family::Run, &literal).unwrap(),
            Some(original[0].1.clone())
        );
    }

    fn native_state(tx: &Transaction<'_>) -> Vec<Vec<Vec<SqlValue>>> {
        [
            "agents",
            "teams",
            "runs",
            "team_members",
            "team_tasks",
            "entity_details",
            "entity_blob_refs",
            "blobs",
            "counters",
        ]
        .iter()
        .map(|table| {
            let mut stmt = tx
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2,3"))
                .unwrap();
            let n = stmt.column_count();
            stmt.query_map([], |r| (0..n).map(|i| r.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        })
        .collect()
    }

    #[test]
    fn native_crud_round_trips_all_families_project_isolation_and_checked_moves() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for entity in [
            Entity::Agent(Box::new(agent())),
            Entity::Team(Box::new(team())),
            Entity::Run(Box::new(run())),
        ] {
            let family = Family::of(&entity).unwrap();
            let id = entity.id().to_owned();
            let first = sql.create(entity.clone()).unwrap();
            assert_eq!(first.revision, Revision::ONE);
            assert_eq!(first.position, 1);
            assert_eq!(sql.get(family, &id).unwrap(), Some(first.clone()));
            assert_eq!(
                sql.hydrate(family, std::slice::from_ref(&id)).unwrap(),
                vec![first.clone()]
            );
            assert!(
                NativeSql::new(&tx, &disk, "other", 123)
                    .get(family, &id)
                    .unwrap()
                    .is_none()
            );
            let before = native_state(&tx);
            assert!(matches!(
                sql.create(entity.clone()),
                Err(NativeError::Store(StoreError::AlreadyExists { .. }))
            ));
            assert_eq!(native_state(&tx), before);
            let updated = sql.update(entity.clone(), first.revision).unwrap();
            assert_eq!(updated.entity, entity);
            assert_eq!(updated.position, 1);
            assert_eq!(updated.revision.get(), 2);
            let before = native_state(&tx);
            assert!(matches!(
                sql.update(entity.clone(), Revision::ONE),
                Err(NativeError::Store(StoreError::StaleRevision { .. }))
            ));
            assert!(matches!(
                sql.delete(family, &id, Revision::ONE),
                Err(NativeError::Store(StoreError::StaleRevision { .. }))
            ));
            assert_eq!(native_state(&tx), before);
            let moved = sql.move_to_end(family, &id, updated.revision).unwrap();
            assert_eq!(moved.position, 2);
            assert_eq!(moved.revision.get(), 3);
            assert_eq!(sql.get(family, &id).unwrap(), Some(moved.clone()));
            sql.delete(family, &id, moved.revision).unwrap();
            assert!(sql.get(family, &id).unwrap().is_none());
            let next = sql.create(entity).unwrap();
            assert_eq!(next.position, 3);
        }
        assert!(
            sql.hydrate(Family::Agent, &["a".into(), "missing".into()])
                .is_err()
        );
        sql.ensure_committable().unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn native_team_oversized_identity_names_lead_members_and_pool_preserve_occurrences() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let long = "é".repeat(32769);
        let mut t = team();
        t.id = long.clone();
        t.name = long.clone();
        t.lead_agent_id = long.clone();
        t.description = Some(String::new());
        t.config.max_concurrent_tasks = Some(u32::MAX);
        t.config.message_ttl_ms = Some(u64::MAX);
        t.members = serde_json::from_value(json!([
        {"agent_id":long,"role":"lead","joined_at":"2026-11-20T10:00:00.123+02:00"},
        {"agent_id":"a","role":"member","joined_at":"opaque"},
        {"agent_id":"a","role":"lead","joined_at":""}]))
        .unwrap();
        t.task_pool = vec![long, "b".into(), "a".into(), "b".into()];
        let saved = sql.create(Entity::Team(Box::new(t.clone()))).unwrap();
        assert_eq!(sql.get(Family::Team, &t.id).unwrap(), Some(saved));
        let rows: Vec<(i64, Option<String>, String, Option<i64>)> = tx
        .prepare(
            "SELECT ordinal,agent_id,joined_at_text,joined_at FROM team_members ORDER BY ordinal",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 0);
        assert!(rows[0].1.is_none());
        assert!(rows[0].3.is_some());
        assert_eq!(rows[1], (1, Some("a".into()), "opaque".into(), None));
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM team_tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let literal = BlobHash::of(t.id.as_bytes()).to_string();
        t.id = literal.clone();
        sql.create(Entity::Team(Box::new(t.clone()))).unwrap();
        assert_eq!(
            sql.get(Family::Team, &literal).unwrap().unwrap().entity,
            Entity::Team(Box::new(t))
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn native_agent_optional_states_config_maxima_and_unsigned_stats_are_lossless() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (i, autonomous) in [None, Some(false), Some(true)].into_iter().enumerate() {
            let mut a = agent();
            a.id = format!("a{i}");
            a.autonomous = autonomous;
            a.role = Some(String::new());
            a.current_task = Some("x".repeat(65537));
            a.config.max_turns = Some(u32::MAX);
            a.config.timeout_ms = Some(u64::MAX);
            a.config.stall_timeout_ms = Some(u64::MAX);
            a.stats.tasks_completed = i64::MAX as u64;
            a.stats.tasks_failed = i64::MAX as u64 + 1;
            a.stats.total_runs = u64::MAX;
            a.stats.total_runtime_ms = u64::MAX;
            a.stats.tokens_used = Some(u64::MAX);
            a.capabilities = vec!["b".into(), "a".into(), "b".into()];
            let saved = sql.create(Entity::Agent(Box::new(a.clone()))).unwrap();
            assert_eq!(sql.get(Family::Agent, &a.id).unwrap(), Some(saved));
            let values: [Option<i64>; 6] = tx.query_row(
            "SELECT autonomous,tasks_completed,tasks_failed,total_runs,total_runtime_ms,tokens_used FROM agents WHERE id=?1",
            [&a.id], |r| Ok([r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?])
        ).unwrap();
            assert_eq!(
                values,
                [
                    autonomous.map(i64::from),
                    Some(i64::MAX),
                    None,
                    None,
                    None,
                    None
                ]
            );
        }
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM agents WHERE name='same'", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            3
        );
    }

    #[test]
    fn native_run_local_uses_serialized_json_byte_cap_keeps_pid_and_tokens() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (i, path) in ["small".into(), "\"".repeat(65536), "é".repeat(32769)]
            .into_iter()
            .enumerate()
        {
            let mut r = run();
            r.id = format!("r{i}");
            r.workspace_path = path;
            r.pid = Some(u32::MAX);
            r.attempt = u32::MAX;
            r.tokens=serde_json::from_value(json!({"input":i64::MAX,"output":i64::MAX as u64+1,"reasoning":u64::MAX,"cache_read":0,"cache_write":u64::MAX,"total":99})).ok();
            let saved = sql.create(Entity::Run(Box::new(r.clone()))).unwrap();
            assert_eq!(sql.get(Family::Run, &r.id).unwrap(), Some(saved));
            let actual:(String,i64,Option<i64>,Option<i64>,i64,Option<i64>,i64)=tx.query_row("SELECT local,tokens_input,tokens_output,tokens_reasoning,tokens_cache_read,tokens_cache_write,tokens_total FROM runs WHERE id=?1",[&r.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
            assert!(actual.0.len() <= 65536);
            let local: Value = serde_json::from_str(&actual.0).unwrap();
            assert_eq!(local["pid"], u32::MAX);
            assert_eq!(local.get("workspace_path").is_some(), i == 0);
            assert_eq!(
                (actual.1, actual.2, actual.3, actual.4, actual.5, actual.6),
                (i64::MAX, None, None, 0, None, 99)
            );
        }
    }

    #[test]
    fn native_claim_identity_is_preserved_and_form_aware_run_collisions_stay_separate() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let mut indirect = run();
        indirect.id = "r".repeat(65537);
        indirect.task_id = "t".repeat(65537);
        indirect.agent_id = "a".repeat(65537);
        let mut literal = indirect.clone();
        literal.id = BlobHash::of(indirect.id.as_bytes()).to_string();
        literal.task_id = BlobHash::of(indirect.task_id.as_bytes()).to_string();
        literal.agent_id = BlobHash::of(indirect.agent_id.as_bytes()).to_string();
        literal.prompt = "literal".into();
        let a = sql.create(Entity::Run(Box::new(indirect.clone()))).unwrap();
        let b = sql.create(Entity::Run(Box::new(literal.clone()))).unwrap();
        // Two claim identities with the same stored task bytes, separated by form.
        tx.execute(
            "UPDATE runs SET run_no=1,counted=1,epoch=1,attempt_no=1",
            [],
        )
        .unwrap();
        let before = native_state(&tx);
        let mut wrong = indirect.clone();
        wrong.task_id = literal.task_id.clone();
        assert!(matches!(
            sql.update(Entity::Run(Box::new(wrong)), a.revision),
            Err(NativeError::ClaimedIdentity)
        ));
        let mut wrong = indirect.clone();
        wrong.agent_id = literal.agent_id.clone();
        assert!(matches!(
            sql.update(Entity::Run(Box::new(wrong)), a.revision),
            Err(NativeError::ClaimedIdentity)
        ));
        assert_eq!(native_state(&tx), before);
        indirect.prompt = "changed".into();
        indirect.attempt = 2;
        let a = sql
            .update(Entity::Run(Box::new(indirect.clone())), a.revision)
            .unwrap();
        assert_eq!(sql.get(Family::Run, &indirect.id).unwrap(), Some(a));
        assert_eq!(sql.get(Family::Run, &literal.id).unwrap(), Some(b));
        let claims: Vec<(i64, i64, i64, i64)> = tx
            .prepare("SELECT run_no,counted,epoch,attempt_no FROM runs ORDER BY id_form")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(claims, vec![(1, 1, 1, 1), (1, 1, 1, 1)]);
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM counters WHERE name NOT LIKE 'position:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn native_failed_install_update_delete_and_move_are_caught_without_partial_rows() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let saved = sql.create(Entity::Run(Box::new(run()))).unwrap();
        tx.execute_batch("CREATE TEMP TRIGGER fail_ref BEFORE INSERT ON entity_blob_refs WHEN NEW.locator='/prompt' BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
        let before = native_state(&tx);
        let mut changed = run();
        changed.prompt = "changed".into();
        assert!(
            sql.update(Entity::Run(Box::new(changed.clone())), saved.revision)
                .is_err()
        );
        assert_eq!(native_state(&tx), before);
        assert_eq!(sql.get(Family::Run, "r").unwrap(), Some(saved.clone()));
        changed.id = "other".into();
        assert!(sql.create(Entity::Run(Box::new(changed))).is_err());
        assert_eq!(native_state(&tx), before);
        tx.execute_batch("DROP TRIGGER fail_ref; CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON runs BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
        assert!(sql.delete(Family::Run, "r", saved.revision).is_err());
        assert_eq!(native_state(&tx), before);
        tx.execute_batch("DROP TRIGGER fail_delete; CREATE TEMP TRIGGER fail_counter BEFORE UPDATE ON counters BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
        assert!(sql.move_to_end(Family::Run, "r", saved.revision).is_err());
        assert_eq!(native_state(&tx), before);
        tx.execute_batch("DROP TRIGGER fail_counter").unwrap();
        sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        sql.ensure_committable().unwrap();
        tx.commit().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn native_projection_membership_reference_drift_and_empty_prompt_ordinal_are_refused() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        let mut t = team();
        t.members =
            serde_json::from_value(json!([{"agent_id":"a","role":"lead","joined_at":"opaque"}]))
                .unwrap();
        t.task_pool = vec!["x".into()];
        sql.create(Entity::Team(Box::new(t))).unwrap();
        sql.create(Entity::Run(Box::new(run()))).unwrap();
        for (family, id, mutation) in [
            (Family::Agent, "a", "UPDATE agents SET status='running'"),
            (Family::Agent, "a", "UPDATE agents SET autonomous=0"),
            (Family::Agent, "a", "UPDATE agents SET tasks_completed=1"),
            (Family::Agent, "a", "UPDATE agents SET current_task=''"),
            (Family::Team, "t", "UPDATE teams SET name='different'"),
            (Family::Team, "t", "UPDATE team_members SET ordinal=5"),
            (
                Family::Team,
                "t",
                "UPDATE team_members SET joined_at_text='different'",
            ),
            (
                Family::Team,
                "t",
                "UPDATE team_tasks SET task_id='different'",
            ),
            (Family::Run, "r", "UPDATE runs SET attempt=2"),
            (Family::Run, "r", "UPDATE runs SET started_at_text=''"),
            (Family::Run, "r", "UPDATE runs SET prompt_sha256=NULL"),
            (Family::Run, "r", "UPDATE runs SET local='{}'"),
            (
                Family::Run,
                "r",
                "UPDATE entity_blob_refs SET ordinal=1 WHERE family='run'",
            ),
            (Family::Run, "r", "DELETE FROM blobs"),
            (
                Family::Run,
                "r",
                "UPDATE entity_details SET format=2 WHERE family='run'",
            ),
            (
                Family::Run,
                "r",
                "DELETE FROM entity_details WHERE family='run'",
            ),
        ] {
            tx.execute_batch("SAVEPOINT damage").unwrap();
            tx.execute_batch(mutation).unwrap();
            assert!(
                matches!(sql.get(family, id), Err(NativeError::Corrupt { .. })),
                "{mutation}"
            );
            tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
                .unwrap();
            assert!(sql.get(family, id).unwrap().is_some());
        }
    }

    #[test]
    fn native_counter_missing_behind_negative_exhausted_and_revision_overflow_do_not_write() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        sql.create(Entity::Run(Box::new(run()))).unwrap();
        for mutation in [
            "DELETE FROM counters",
            "UPDATE counters SET value=0",
            "UPDATE counters SET value=-1",
            "UPDATE counters SET value=9223372036854775807",
        ] {
            tx.execute_batch("SAVEPOINT damage").unwrap();
            tx.execute_batch(mutation).unwrap();
            let before = native_state(&tx);
            let mut r = run();
            r.id = "new".into();
            assert!(sql.create(Entity::Run(Box::new(r))).is_err());
            assert_eq!(native_state(&tx), before);
            tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
                .unwrap();
        }
        tx.execute("UPDATE runs SET revision=?1", [i64::MAX])
            .unwrap();
        let max: Revision = serde_json::from_value(json!(i64::MAX)).unwrap();
        let before = native_state(&tx);
        assert!(matches!(
            sql.update(Entity::Run(Box::new(run())), max),
            Err(NativeError::Store(StoreError::CounterExhausted))
        ));
        assert!(matches!(
            sql.move_to_end(Family::Run, "r", max),
            Err(NativeError::Store(StoreError::CounterExhausted))
        ));
        assert_eq!(native_state(&tx), before);
    }

    #[test]
    fn native_updates_and_moves_preserve_independent_columns() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let a = sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        let r = sql.create(Entity::Run(Box::new(run()))).unwrap();
        tx.execute_batch("UPDATE agents SET created_at=17,updated_at=19,last_run_finished_at=23; UPDATE runs SET branch='retained',worker_id='retained',events_pruned_count=7").unwrap();
        for saved in [a, r] {
            let family = Family::of(&saved.entity).unwrap();
            let updated = sql.update(saved.entity, saved.revision).unwrap();
            sql.move_to_end(family, updated.entity.id(), updated.revision)
                .unwrap();
        }
        assert_eq!(
            tx.query_row(
                "SELECT created_at,updated_at,last_run_finished_at FROM agents",
                [],
                |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?
                ))
            )
            .unwrap(),
            (17, 19, 23)
        );
        assert_eq!(
            tx.query_row(
                "SELECT branch,worker_id,events_pruned_count FROM runs",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?
                ))
            )
            .unwrap(),
            ("retained".into(), "retained".into(), 7)
        );
    }

    #[test]
    fn native_rollback_failure_retains_both_errors_and_poison_blocks_later_writes() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        tx.execute_batch("CREATE TEMP TRIGGER abort_transaction BEFORE INSERT ON entity_blob_refs BEGIN SELECT RAISE(ROLLBACK,'injected'); END").unwrap();
        let error = sql.create(Entity::Run(Box::new(run()))).unwrap_err();
        assert!(
            matches!(error,NativeError::Rollback {operation,rollback:rusqlite::Error::SqliteFailure(..)} if matches!(*operation,NativeError::Sql(rusqlite::Error::SqliteFailure(..))))
        );
        assert!(matches!(
            sql.ensure_committable(),
            Err(NativeError::Aborted)
        ));
        assert!(matches!(
            sql.create(Entity::Team(Box::new(team()))),
            Err(NativeError::Aborted)
        ));
        assert!(tx.is_autocommit());
    }

    #[test]
    fn native_value_equivalent_detail_is_readable_without_reencoding_hash_equality() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let saved = sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        let encoded: String = tx
            .query_row(
                "SELECT detail FROM entity_details WHERE family='agent'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        let mut reordered = Map::new();
        for (key, value) in value.as_object().unwrap().iter().rev() {
            reordered.insert(key.clone(), value.clone());
        }
        reordered.insert("capabilities".into(), json!([]));
        let text = serde_json::to_string_pretty(&reordered).unwrap();
        assert_ne!(text, encoded);
        tx.execute(
        "UPDATE entity_details SET detail=?1,detail_bytes=?2,detail_sha256=?3 WHERE family='agent'",
        params![
            text,
            text.len() as i64,
            BlobHash::of(text.as_bytes()).to_string()
        ],
    )
    .unwrap();
        assert_eq!(sql.get(Family::Agent, "a").unwrap(), Some(saved));
    }

    #[test]
    fn native_read_snapshot_and_caller_transaction_each_hydrate_one_state() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let db = _temp.path().join("snapshot.sqlite");
        let mut writer = rusqlite::Connection::open(&db).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        for schema in [
            crate::sqlite::migrations::INITIAL_SCHEMA,
            crate::sqlite::migrations::ATTENTION_FACTS,
            crate::sqlite::migrations::TASK_FORMAT,
            crate::sqlite::migrations::AGENT_TEAM_RUN,
        ] {
            writer.execute_batch(schema).unwrap();
        }
        let first = {
            let tx = writer.transaction().unwrap();
            let mut sql = NativeSql::new(&tx, &disk, "p", 123);
            let first = sql.create(Entity::Agent(Box::new(agent()))).unwrap();
            tx.commit().unwrap();
            first
        };
        let mut reader = rusqlite::Connection::open(&db).unwrap();
        let snapshot = reader.transaction().unwrap();
        assert_eq!(
            NativeSql::new(&snapshot, &disk, "p", 123)
                .get(Family::Agent, "a")
                .unwrap(),
            Some(first.clone())
        );
        let changed = {
            let tx = writer.transaction().unwrap();
            let mut sql = NativeSql::new(&tx, &disk, "p", 123);
            let mut a = agent();
            a.name = "new".into();
            let saved = sql
                .update(Entity::Agent(Box::new(a)), first.revision)
                .unwrap();
            assert_eq!(sql.get(Family::Agent, "a").unwrap(), Some(saved.clone()));
            let page = sql
                .select(selection(&Filter::Agents, Sort::Name), None, 1)
                .unwrap();
            assert_eq!(page.items, vec![saved.clone()]);
            tx.commit().unwrap();
            saved
        };
        assert_eq!(
            NativeSql::new(&snapshot, &disk, "p", 123)
                .hydrate(Family::Agent, &["a".into()])
                .unwrap(),
            vec![first]
        );
        let page = NativeSql::new(&snapshot, &disk, "p", 123)
            .select(selection(&Filter::Agents, Sort::Name), None, 1)
            .unwrap();
        assert_eq!(page.items[0].entity, Entity::Agent(Box::new(agent())));
        snapshot.commit().unwrap();
        let fresh = reader.transaction().unwrap();
        let page = NativeSql::new(&fresh, &disk, "p", 123)
            .select(selection(&Filter::Agents, Sort::Name), None, 1)
            .unwrap();
        assert_eq!(page.items, vec![changed.clone()]);
        assert_eq!(
            NativeSql::new(&fresh, &disk, "p", 123)
                .get(Family::Agent, "a")
                .unwrap(),
            Some(changed)
        );
    }

    #[test]
    fn native_exact_time_projection_keeps_opaque_offsets_submillisecond_and_original_text() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (i, text, expected) in [
            ("0", "opaque", None),
            ("1", "2026-11-20T10:00:00.1239+02:00", None),
            (
                "2",
                "2026-11-20T10:00:00.123+02:00",
                Some(1795161600123_i64),
            ),
            ("3", "2026-11-20T08:00:00.123Z", Some(1795161600123_i64)),
        ] {
            let mut r = run();
            r.id = i.into();
            r.started_at = text.into();
            r.finished_at = Some(text.into());
            let saved = sql.create(Entity::Run(Box::new(r))).unwrap();
            assert_eq!(sql.get(Family::Run, i).unwrap(), Some(saved));
            let values: (String, Option<i64>, Option<i64>) = tx
                .query_row(
                    "SELECT started_at_text,started_at,finished_at FROM runs WHERE id=?1",
                    [i],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(values, (text.into(), expected, expected));
        }
    }

    #[test]
    fn native_every_scalar_projection_and_invalid_utf8_are_checked_on_read() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        sql.create(Entity::Agent(Box::new(agent()))).unwrap();
        sql.create(Entity::Team(Box::new(team()))).unwrap();
        sql.create(Entity::Run(Box::new(run()))).unwrap();
        for (family, id, table, columns) in [
            (
                Family::Agent,
                "a",
                "agents",
                "status autonomous tasks_completed tasks_failed total_runs total_runtime_ms tokens_used",
            ),
            (
                Family::Team,
                "t",
                "teams",
                "status config created_at updated_at",
            ),
            (
                Family::Run,
                "r",
                "runs",
                "status attempt started_at finished_at error_kind prompt_sha256 tokens_input tokens_output tokens_reasoning tokens_cache_read tokens_cache_write tokens_total local",
            ),
        ] {
            for column in columns.split_whitespace() {
                tx.execute_batch("SAVEPOINT damage").unwrap();
                tx.execute_batch(&format!("UPDATE {table} SET {column}=CASE typeof({column}) WHEN 'integer' THEN {column}+1 WHEN 'text' THEN 'drift' ELSE 0 END")).unwrap();
                assert!(
                    matches!(sql.get(family, id), Err(NativeError::Corrupt { .. })),
                    "{table}.{column}"
                );
                tx.execute_batch("ROLLBACK TO damage; RELEASE damage")
                    .unwrap();
            }
        }
        tx.execute_batch("UPDATE agents SET name=CAST(x'80' AS TEXT)")
            .unwrap();
        assert!(matches!(
            sql.get(Family::Agent, "a"),
            Err(NativeError::Corrupt {
                check: "text encoding",
                ..
            })
        ));
    }

    #[test]
    fn native_prompt_tag_registration_time_and_absent_vs_zero_tokens_stay_distinct() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (id, tokens) in [
            ("absent", None),
            ("zero", Some(orch_domain::TokenUsage::default())),
        ] {
            let mut r = run();
            r.id = id.into();
            r.tokens = tokens;
            let saved = sql.create(Entity::Run(Box::new(r))).unwrap();
            assert_eq!(sql.get(Family::Run, id).unwrap(), Some(saved));
            let detail: String = tx
                .query_row(
                    "SELECT detail FROM entity_details WHERE family='run' AND entity_key=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            let value: Value = serde_json::from_str(&detail).unwrap();
            assert!(value["prompt"]["$blob"].is_object());
        }
        assert_eq!(
            tx.query_row("SELECT COUNT(*),MIN(created_at) FROM blobs", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?
            )))
            .unwrap(),
            (1, 123)
        );
        assert_eq!(
            tx.query_row("SELECT tokens_total FROM runs WHERE id='absent'", [], |r| r
                .get::<_, Option<i64>>(0))
                .unwrap(),
            None
        );
        assert_eq!(
            tx.query_row("SELECT tokens_total FROM runs WHERE id='zero'", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn native_run_update_explicitly_writes_preserved_position() {
        let (_temp, data) = crate::sqlite::test_data();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = native_conn();
        let tx = conn.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let saved = sql.create(Entity::Run(Box::new(run()))).unwrap();
        tx.execute_batch("CREATE TEMP TABLE observed_position(value INTEGER); CREATE TEMP TRIGGER observe_position AFTER UPDATE OF position ON runs BEGIN INSERT INTO observed_position VALUES(NEW.position); END").unwrap();
        let updated = sql
            .update(Entity::Run(Box::new(run())), saved.revision)
            .unwrap();
        let positions: Vec<i64> = tx
            .prepare("SELECT value FROM observed_position")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(positions, vec![saved.position as i64]);
        assert_eq!(updated.position, saved.position);
        assert_eq!(
            tx.query_row(
                "SELECT value FROM counters WHERE name='position:run'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

// Private native leaf: authorization, fencing and R140 observation belong to
// the combined adapter. This type neither opens nor commits a transaction.
use crate::blob::{BlobReferences, ReferenceError};
use orch_core::store::{Entity, EntityKind, Filter, Record, Revision, Sort, StoreError};
use rusqlite::{OptionalExtension, Transaction, params, types::Value as SqlValue};
use std::cmp::Ordering;

type NativeResult<T> = std::result::Result<T, NativeError>;

#[derive(Debug, thiserror::Error)]
pub(super) enum NativeError {
    #[error("stored {family} form {form} failed {check} at {locator}")]
    Corrupt {
        family: &'static str,
        form: i64,
        check: &'static str,
        locator: String,
    },
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Counter(Box<super::SqliteError>),
    #[error("entity object access failed: {0:?}")]
    Access(std::io::ErrorKind),
    #[error(transparent)]
    Publication(crate::blob::BlobError),
    #[error("entity encoding is not representable")]
    Encoding,
    #[error("ordinary CRUD cannot reassign a claimed run")]
    ClaimedIdentity,
    #[error("native transaction cannot commit after failed rollback")]
    Aborted,
    #[error("operation failed: {operation}; rollback failed: {rollback}")]
    Rollback {
        operation: Box<NativeError>,
        rollback: rusqlite::Error,
    },
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Run => "run",
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Agent => "agents",
            Self::Team => "teams",
            Self::Run => "runs",
        }
    }
    fn counter(self) -> &'static str {
        match self {
            Self::Agent => "position:agent",
            Self::Team => "position:team",
            Self::Run => "position:run",
        }
    }
    fn kind(self) -> EntityKind {
        match self {
            Self::Agent => EntityKind::Agent,
            Self::Team => EntityKind::Team,
            Self::Run => EntityKind::Run,
        }
    }
    fn of(entity: &Entity) -> NativeResult<Self> {
        match entity {
            Entity::Agent(_) => Ok(Self::Agent),
            Entity::Team(_) => Ok(Self::Team),
            Entity::Run(_) => Ok(Self::Run),
            _ => Err(NativeError::Encoding),
        }
    }
    fn texts(self) -> &'static [&'static str] {
        match self {
            Self::Agent => &[
                "id",
                "name",
                "adapter",
                "role",
                "current_task",
                "last_error",
            ],
            Self::Team => &[
                "id",
                "name",
                "description",
                "lead_agent_id",
                "created_at_text",
            ],
            Self::Run => &["id", "task_id", "agent_id", "started_at_text", "error"],
        }
    }
    fn scalars(self) -> &'static [&'static str] {
        match self {
            Self::Agent => &[
                "config",
                "status",
                "autonomous",
                "tasks_completed",
                "tasks_failed",
                "total_runs",
                "total_runtime_ms",
                "tokens_used",
            ],
            Self::Team => &["status", "config", "created_at", "updated_at"],
            Self::Run => &[
                "status",
                "attempt",
                "started_at",
                "finished_at",
                "error_kind",
                "prompt_sha256",
                "tokens_input",
                "tokens_output",
                "tokens_reasoning",
                "tokens_cache_read",
                "tokens_cache_write",
                "tokens_total",
                "local",
            ],
        }
    }
    fn columns(self) -> Vec<String> {
        let mut columns = Vec::new();
        for field in self.texts() {
            columns.extend([
                field.to_string(),
                format!("{field}_form"),
                format!("{field}_sha256"),
                format!("{field}_bytes"),
            ]);
        }
        columns.extend(self.scalars().iter().map(|s| s.to_string()));
        columns
    }
}
fn native_corrupt(family: Family, id: &str, check: &'static str, locator: &str) -> NativeError {
    NativeError::Corrupt {
        family: family.name(),
        form: key(id).0,
        check,
        locator: locator.into(),
    }
}
fn key(id: &str) -> (i64, String) {
    if id.len() <= INLINE_TEXT_MAX_BYTES {
        (0, id.into())
    } else {
        (1, BlobHash::of(id.as_bytes()).to_string())
    }
}
fn integer(value: u64) -> NativeResult<i64> {
    i64::try_from(value).map_err(|_| StoreError::CounterExhausted.into())
}
fn unsigned(value: u64) -> SqlValue {
    i64::try_from(value)
        .map(SqlValue::Integer)
        .unwrap_or(SqlValue::Null)
}
fn nullable_text(value: Option<&str>) -> SqlValue {
    value
        .map(|v| SqlValue::Text(v.into()))
        .unwrap_or(SqlValue::Null)
}
fn time(value: &str) -> SqlValue {
    value
        .parse::<jiff::Timestamp>()
        .ok()
        .filter(|t| t.as_nanosecond() % 1_000_000 == 0)
        .map(|t| SqlValue::Integer(t.as_millisecond()))
        .unwrap_or(SqlValue::Null)
}
fn quartet(value: Option<&str>, identity: bool) -> Vec<SqlValue> {
    match value {
        Some(text) if text.len() > INLINE_TEXT_MAX_BYTES => {
            let hash = BlobHash::of(text.as_bytes()).to_string();
            vec![
                SqlValue::Text(if identity {
                    hash.clone()
                } else {
                    String::new()
                }),
                SqlValue::Integer(1),
                SqlValue::Text(hash),
                SqlValue::Integer(text.len() as i64),
            ]
        }
        other => vec![
            nullable_text(other),
            SqlValue::Integer(0),
            SqlValue::Null,
            SqlValue::Null,
        ],
    }
}
fn local(run: &Run) -> NativeResult<String> {
    let mut map = Map::new();
    if let Some(pid) = run.pid {
        map.insert("pid".into(), pid.into());
    }
    if run.workspace_path.len() <= INLINE_TEXT_MAX_BYTES {
        map.insert("workspace_path".into(), run.workspace_path.clone().into());
    }
    let mut json = serde_json::to_string(&map).map_err(|_| NativeError::Encoding)?;
    if json.len() > INLINE_TEXT_MAX_BYTES {
        map.remove("workspace_path");
        json = serde_json::to_string(&map).map_err(|_| NativeError::Encoding)?;
    }
    Ok(json)
}
fn projections(entity: &Entity) -> NativeResult<Vec<SqlValue>> {
    let mut values = Vec::new();
    match entity {
        Entity::Agent(a) => {
            for (text, identity) in [
                (Some(a.id.as_str()), true),
                (Some(a.name.as_str()), false),
                (Some(a.adapter.as_str()), false),
                (a.role.as_deref(), false),
                (a.current_task.as_deref(), false),
                (a.last_error.as_ref().map(|e| e.message.as_str()), false),
            ] {
                values.extend(quartet(text, identity));
            }
            values.extend([
                SqlValue::Text("{}".into()),
                SqlValue::Text(a.status.to_string()),
                a.autonomous
                    .map(|v| SqlValue::Integer(v.into()))
                    .unwrap_or(SqlValue::Null),
                unsigned(a.stats.tasks_completed),
                unsigned(a.stats.tasks_failed),
                unsigned(a.stats.total_runs),
                unsigned(a.stats.total_runtime_ms),
                a.stats.tokens_used.map(unsigned).unwrap_or(SqlValue::Null),
            ]);
        }
        Entity::Team(t) => {
            for text in [
                Some(t.id.as_str()),
                Some(t.name.as_str()),
                t.description.as_deref(),
                Some(t.lead_agent_id.as_str()),
                Some(t.created_at.as_str()),
            ] {
                values.extend(quartet(text, values.is_empty()));
            }
            values.extend([
                SqlValue::Text(t.status.to_string()),
                SqlValue::Text(
                    serde_json::to_string(&t.config).map_err(|_| NativeError::Encoding)?,
                ),
                time(&t.created_at),
                time(&t.updated_at),
            ]);
        }
        Entity::Run(r) => {
            for (text, identity) in [
                (Some(r.id.as_str()), true),
                (Some(r.task_id.as_str()), true),
                (Some(r.agent_id.as_str()), true),
                (Some(r.started_at.as_str()), false),
                (r.error.as_deref(), false),
            ] {
                values.extend(quartet(text, identity));
            }
            let error_kind = r
                .error_kind
                .map(|v| serde_json::to_value(v).map_err(|_| NativeError::Encoding))
                .transpose()?;
            values.extend([
                SqlValue::Text(r.status.to_string()),
                SqlValue::Integer(r.attempt.into()),
                time(&r.started_at),
                r.finished_at.as_deref().map(time).unwrap_or(SqlValue::Null),
                nullable_text(error_kind.as_ref().and_then(Value::as_str)),
                SqlValue::Text(BlobHash::of(r.prompt.as_bytes()).to_string()),
            ]);
            for value in [
                r.tokens.as_ref().map(|v| v.input),
                r.tokens.as_ref().map(|v| v.output),
                r.tokens.as_ref().map(|v| v.reasoning),
                r.tokens.as_ref().map(|v| v.cache_read),
                r.tokens.as_ref().map(|v| v.cache_write),
                r.tokens.as_ref().map(|v| v.total),
            ] {
                values.push(value.map(unsigned).unwrap_or(SqlValue::Null));
            }
            values.push(SqlValue::Text(local(r)?));
        }
        _ => return Err(NativeError::Encoding),
    }
    Ok(values)
}
fn format_error(family: Family, id: &str, error: FormatError, writing: bool) -> NativeError {
    match error {
        FormatError::Access { kind, .. } | FormatError::Publish(TextError::Access(kind)) => {
            NativeError::Access(kind)
        }
        FormatError::Publish(TextError::Publish(error)) if writing => {
            NativeError::Publication(error)
        }
        _ if writing => NativeError::Encoding,
        FormatError::Invalid { check, locator } => native_corrupt(family, id, check, &locator),
        _ => native_corrupt(family, id, "detail encoding", ""),
    }
}

/// Private query composition only; public cursor identity and validation remain
/// owned by the combined adapter. All strings are exact original UTF-8 bytes.
pub(super) struct NativeSelection<'a> {
    pub(super) filter: &'a Filter,
    pub(super) sort: Sort,
    pub(super) name: Option<&'a str>,
    pub(super) member: Option<&'a str>,
}
pub(super) struct NativePage {
    pub(super) items: Vec<Record>,
    pub(super) total: u64,
    pub(super) has_more: bool,
}
impl NativeSelection<'_> {
    fn family(&self) -> NativeResult<Family> {
        let family = match self.filter {
            Filter::Agents => Family::Agent,
            Filter::Teams => Family::Team,
            Filter::Runs { .. } => Family::Run,
            _ => return Err(StoreError::InvalidPage("unsupported native family".into()).into()),
        };
        let valid = matches!(self.sort, Sort::Default | Sort::Id)
            || (self.sort == Sort::Name && family != Family::Run)
            || (self.sort == Sort::Attempt
                && matches!(self.filter, Filter::Runs { task: Some(_), .. }));
        if !valid
            || (self.name.is_some() && family == Family::Run)
            || (self.member.is_some() && family != Family::Team)
        {
            return Err(StoreError::InvalidPage("unsupported native selection".into()).into());
        }
        Ok(family)
    }
    fn matches(&self, row: &Record) -> bool {
        match (&row.entity, self.filter) {
            (Entity::Agent(a), Filter::Agents) => self.name.is_none_or(|name| a.name == name),
            (Entity::Team(t), Filter::Teams) => {
                self.name.is_none_or(|name| t.name == name)
                    && self
                        .member
                        .is_none_or(|member| t.members.iter().any(|m| m.agent_id == member))
            }
            (Entity::Run(r), Filter::Runs { task, agent }) => {
                task.as_ref().is_none_or(|id| r.task_id == *id)
                    && agent.as_ref().is_none_or(|id| r.agent_id == *id)
            }
            _ => false,
        }
    }
    fn compare(&self, a: &Record, b: &Record) -> Ordering {
        let primary = match (&a.entity, &b.entity, self.sort) {
            (_, _, Sort::Id) => Ordering::Equal,
            (Entity::Agent(ae), Entity::Agent(be), Sort::Name) => ae
                .name
                .as_bytes()
                .cmp(be.name.as_bytes())
                .then(a.position.cmp(&b.position)),
            (Entity::Agent(_), Entity::Agent(_), _) => a.position.cmp(&b.position),
            (Entity::Team(a), Entity::Team(b), Sort::Name) => {
                a.name.as_bytes().cmp(b.name.as_bytes())
            }
            (Entity::Team(a), Entity::Team(b), _) => {
                a.created_at.as_bytes().cmp(b.created_at.as_bytes())
            }
            (Entity::Run(a), Entity::Run(b), _)
                if matches!(self.filter, Filter::Runs { task: Some(_), .. }) =>
            {
                b.attempt.cmp(&a.attempt)
            }
            (Entity::Run(a), Entity::Run(b), _) => {
                b.started_at.as_bytes().cmp(a.started_at.as_bytes())
            }
            // Family and sort are validated before comparing, including anchors.
            _ => Ordering::Equal,
        };
        primary.then_with(|| a.entity.id().as_bytes().cmp(b.entity.id().as_bytes()))
    }
}

pub(super) struct NativeSql<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    disk: &'a DiskBlobStore,
    project: &'a str,
    poisoned: bool,
    now: i64,
}
impl<'a, 'conn> NativeSql<'a, 'conn> {
    pub(super) fn new(
        tx: &'a Transaction<'conn>,
        disk: &'a DiskBlobStore,
        project: &'a str,
        now: i64,
    ) -> Self {
        Self {
            tx,
            disk,
            project,
            poisoned: false,
            now,
        }
    }
    pub(super) fn ensure_committable(&self) -> NativeResult<()> {
        if self.poisoned || self.tx.is_autocommit() {
            Err(NativeError::Aborted)
        } else {
            Ok(())
        }
    }
    fn atomic<T>(&mut self, work: impl FnOnce(&Self) -> NativeResult<T>) -> NativeResult<T> {
        self.ensure_committable()?;
        self.tx.execute_batch("SAVEPOINT native_entity")?;
        let result = work(self).and_then(|v| {
            self.tx.execute_batch("RELEASE native_entity")?;
            Ok(v)
        });
        match result {
            Ok(value) => Ok(value),
            Err(operation) => match self
                .tx
                .execute_batch("ROLLBACK TO native_entity; RELEASE native_entity")
            {
                Ok(()) => Err(operation),
                Err(rollback) => {
                    self.poisoned = true;
                    Err(NativeError::Rollback {
                        operation: Box::new(operation),
                        rollback,
                    })
                }
            },
        }
    }
    // Compiled identifiers only. Keys, locators and payloads are always bound.
    fn rows(&self, query: &str, values: &[SqlValue]) -> NativeResult<Vec<Vec<SqlValue>>> {
        let mut stmt = self.tx.prepare(query)?;
        let count = stmt.column_count();
        Ok(stmt
            .query_map(rusqlite::params_from_iter(values), |r| {
                (0..count).map(|i| r.get(i)).collect()
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    fn identity(&self, id: &str) -> Vec<SqlValue> {
        let (form, key) = key(id);
        vec![
            SqlValue::Text(self.project.into()),
            SqlValue::Integer(form),
            SqlValue::Text(key),
        ]
    }
    pub(super) fn get(&self, family: Family, id: &str) -> NativeResult<Option<Record>> {
        self.get_inner(family, id).map_err(|error| match error {
            NativeError::Sql(rusqlite::Error::Utf8Error(..)) => {
                native_corrupt(family, id, "text encoding", "native/detail/ref")
            }
            other => other,
        })
    }

    fn get_inner(&self, family: Family, id: &str) -> NativeResult<Option<Record>> {
        let columns = family.columns();
        let rows = self.rows(
            &format!(
                "SELECT {},revision,position FROM {} WHERE project_id=?1 AND id_form=?2 AND id=?3",
                columns.join(","),
                family.table()
            ),
            &self.identity(id),
        )?;
        let Some(native) = rows.first() else {
            return Ok(None);
        };
        if native[..4] != quartet(Some(id), true) {
            return Err(native_corrupt(family, id, "identity projection", "id"));
        }
        let n = columns.len();
        let (SqlValue::Integer(revision), SqlValue::Integer(position)) =
            (&native[n], &native[n + 1])
        else {
            return Err(native_corrupt(
                family,
                id,
                "counter type",
                "revision/position",
            ));
        };
        if *revision <= 0 || *position <= 0 {
            return Err(native_corrupt(family, id, "counter", "revision/position"));
        }
        let detail = self.detail(family, id)?;
        let entity = match family {
            Family::Agent => decode_agent(self.disk, &detail).map(|v| Entity::Agent(Box::new(v))),
            Family::Team => decode_team(self.disk, &detail).map(|v| Entity::Team(Box::new(v))),
            Family::Run => decode_run(self.disk, &detail).map(|v| Entity::Run(Box::new(v))),
        }
        .map_err(|e| format_error(family, id, e, false))?;
        if entity.id() != id {
            return Err(native_corrupt(family, id, "identity", "/id"));
        }
        let expected = projections(&entity)
            .map_err(|_| native_corrupt(family, id, "projection encoding", ""))?;
        for (i, column) in columns.iter().enumerate() {
            if native[i] != expected[i] {
                return Err(native_corrupt(family, id, "projection", column));
            }
        }
        if let Entity::Team(t) = &entity {
            self.check_members(t)?;
        }
        if family == Family::Run {
            self.claim(id)?;
        }
        let revision = serde_json::from_value((*revision).into())
            .map_err(|_| native_corrupt(family, id, "revision", "revision"))?;
        Ok(Some(Record {
            entity,
            revision,
            position: *position as u64,
        }))
    }
    /// Hydrate candidates and compare originals in this selecting transaction.
    /// Full hydration is deliberately conservative: no digest/index order can
    /// exclude a row whose original key must participate in the total order.
    /// No records escape if any candidate is corrupt. The composition owner
    /// must bind the saved anchor to its project/filter/sort in the public cursor.
    pub(super) fn select(
        &self,
        query: NativeSelection<'_>,
        after: Option<&Record>,
        limit: usize,
    ) -> NativeResult<NativePage> {
        self.ensure_committable()?;
        let family = query.family()?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::InvalidPage("limit must be 1..=1000".into()).into());
        }
        if let Some(anchor) = after
            && (Family::of(&anchor.entity)? != family || !query.matches(anchor))
        {
            return Err(StoreError::InvalidCursor.into());
        }
        let mut stmt = self.tx.prepare(&format!(
            "SELECT id_form,id FROM {} WHERE project_id=?1",
            family.table()
        ))?;
        // Keep the stored form even when decoding the key fails. In particular,
        // malformed TEXT is stored-data corruption before any detail is read.
        let keys = stmt
            .query_map([self.project], |row| {
                Ok((row.get::<_, SqlValue>(0)?, row.get::<_, SqlValue>(1)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut records = Vec::new();
        for (form, stored) in keys {
            let SqlValue::Integer(form @ (0 | 1)) = form else {
                return Err(native_corrupt(family, "", "identity metadata", "id"));
            };
            let stored = stored.map_err(|error| match error {
                rusqlite::Error::Utf8Error(..) => NativeError::Corrupt {
                    family: family.name(),
                    form,
                    check: "text encoding",
                    locator: "native/id".into(),
                },
                other => NativeError::Sql(other),
            })?;
            let SqlValue::Text(stored) = stored else {
                return Err(native_corrupt(family, "", "identity metadata", "id"));
            };
            // Stored keys are locators, not original IDs: form 1's key can be
            // equal to a form 0 ID. Never feed a digest into get as an ID.
            let candidate = (|| {
                let detail = self.detail_key(
                    family,
                    "",
                    vec![
                        SqlValue::Text(self.project.into()),
                        SqlValue::Integer(form),
                        SqlValue::Text(stored.clone()),
                    ],
                )?;
                let entity = match family {
                    Family::Agent => {
                        decode_agent(self.disk, &detail).map(|v| Entity::Agent(Box::new(v)))
                    }
                    Family::Team => {
                        decode_team(self.disk, &detail).map(|v| Entity::Team(Box::new(v)))
                    }
                    Family::Run => decode_run(self.disk, &detail).map(|v| Entity::Run(Box::new(v))),
                }
                .map_err(|e| format_error(family, "", e, false))?;
                if key(entity.id()) != (form, stored.clone()) {
                    return Err(native_corrupt(
                        family,
                        entity.id(),
                        "identity projection",
                        "id",
                    ));
                }
                self.get(family, entity.id())?.ok_or_else(|| {
                    native_corrupt(family, entity.id(), "selected row missing", "id")
                })
            })()
            .map_err(|error| match error {
                NativeError::Corrupt {
                    family,
                    check,
                    locator,
                    ..
                } => NativeError::Corrupt {
                    family,
                    form,
                    check,
                    locator,
                },
                NativeError::Sql(rusqlite::Error::Utf8Error(..)) => NativeError::Corrupt {
                    family: family.name(),
                    form,
                    check: "text encoding",
                    locator: "native/detail/ref".into(),
                },
                other => other,
            })?;
            if query.matches(&candidate) {
                records.push(candidate);
            }
        }
        records.sort_by(|a, b| query.compare(a, b));
        let total = records.len() as u64;
        let mut remaining = records.into_iter().filter(|row| {
            after.is_none_or(|anchor| query.compare(row, anchor) == Ordering::Greater)
        });
        let items = remaining.by_ref().take(limit).collect();
        let has_more = remaining.next().is_some();
        Ok(NativePage {
            items,
            total,
            has_more,
        })
    }

    pub(super) fn hydrate(&self, family: Family, ids: &[String]) -> NativeResult<Vec<Record>> {
        ids.iter()
            .map(|id| {
                self.get(family, id)?
                    .ok_or_else(|| native_corrupt(family, id, "selected row missing", "id"))
            })
            .collect()
    }
    fn detail(&self, family: Family, id: &str) -> NativeResult<Detail> {
        self.detail_key(family, id, self.identity(id))
    }
    fn detail_key(
        &self,
        family: Family,
        id: &str,
        mut identity: Vec<SqlValue>,
    ) -> NativeResult<Detail> {
        identity.push(SqlValue::Text(family.name().into()));
        let rows = self.rows("SELECT format,detail,detail_bytes,detail_sha256 FROM entity_details WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4",&identity)?;
        let Some(row) = rows.first() else {
            return Err(native_corrupt(family, id, "detail missing", ""));
        };
        let [
            SqlValue::Integer(1),
            inline,
            SqlValue::Integer(bytes),
            SqlValue::Text(hash),
        ] = row.as_slice()
        else {
            return Err(native_corrupt(family, id, "detail metadata", ""));
        };
        if *bytes < 0 || hash.parse::<BlobHash>().is_err() {
            return Err(native_corrupt(family, id, "detail metadata", ""));
        }
        let inline = match inline {
            SqlValue::Null if *bytes > INLINE_TEXT_MAX_BYTES as i64 => None,
            SqlValue::Text(s) if s.len() <= INLINE_TEXT_MAX_BYTES && s.len() as i64 == *bytes => {
                Some(s.clone())
            }
            _ => return Err(native_corrupt(family, id, "detail size", "")),
        };
        let rows = self.rows("SELECT locator,ordinal,sha256,size FROM entity_blob_refs WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4 ORDER BY locator COLLATE BINARY,ordinal",&identity)?;
        let mut refs: BTreeMap<String, Vec<Chunk>> = BTreeMap::new();
        for row in rows {
            let [
                SqlValue::Text(locator),
                SqlValue::Integer(ordinal),
                SqlValue::Text(hash),
                SqlValue::Integer(size),
            ] = row.as_slice()
            else {
                return Err(native_corrupt(family, id, "reference metadata", ""));
            };
            if *ordinal < 0
                || *size < 0
                || *size as u64 > MAX_BLOB_BYTES
                || (*size == 0 && !(family == Family::Run && locator == "/prompt"))
                || hash.parse::<BlobHash>().is_err()
            {
                return Err(native_corrupt(family, id, "reference metadata", "blob_ref"));
            }
            let registration = self.rows(
                "SELECT size,storage_location FROM blobs WHERE project_id=?1 AND sha256=?2",
                &[
                    SqlValue::Text(self.project.into()),
                    SqlValue::Text(hash.clone()),
                ],
            )?;
            if registration
                != vec![vec![
                    SqlValue::Integer(*size),
                    SqlValue::Text("disk".into()),
                ]]
            {
                return Err(native_corrupt(family, id, "blob registration", "blob_ref"));
            }
            refs.entry(locator.clone()).or_default().push(Chunk {
                ordinal: *ordinal as u64,
                sha256: hash.clone(),
                size: *size as u64,
            });
        }
        Ok(Detail {
            inline,
            bytes: *bytes as u64,
            sha256: hash.clone(),
            refs,
        })
    }
    fn member_values(team: &Team) -> NativeResult<Vec<Vec<SqlValue>>> {
        team.members
            .iter()
            .enumerate()
            .map(|(ordinal, m)| {
                let mut row = quartet(Some(&team.id), true);
                row.push(SqlValue::Integer(integer(ordinal as u64)?));
                row.push(nullable_text(
                    Some(&m.agent_id)
                        .filter(|s| s.len() <= INLINE_TEXT_MAX_BYTES)
                        .map(String::as_str),
                ));
                row.extend(quartet(Some(m.role.as_str()), false));
                row.push(time(&m.joined_at));
                row.extend(quartet(Some(&m.joined_at), false));
                Ok(row)
            })
            .collect()
    }
    fn check_members(&self, team: &Team) -> NativeResult<()> {
        let actual = self.rows("SELECT team_id,team_id_form,team_id_sha256,team_id_bytes,ordinal,agent_id,role,role_form,role_sha256,role_bytes,joined_at,joined_at_text,joined_at_text_form,joined_at_text_sha256,joined_at_text_bytes FROM team_members WHERE project_id=?1 AND team_id_form=?2 AND team_id=?3 ORDER BY ordinal",&self.identity(&team.id))?;
        if actual != Self::member_values(team)? {
            return Err(native_corrupt(
                Family::Team,
                &team.id,
                "membership",
                "members",
            ));
        }
        let actual = self.rows("SELECT team_id,team_id_form,team_id_sha256,team_id_bytes,task_id FROM team_tasks WHERE project_id=?1 AND team_id_form=?2 AND team_id=?3 ORDER BY task_id COLLATE BINARY",&self.identity(&team.id))?;
        if actual != Self::task_values(team) {
            return Err(native_corrupt(
                Family::Team,
                &team.id,
                "membership",
                "task_pool",
            ));
        }
        Ok(())
    }
    fn task_values(team: &Team) -> Vec<Vec<SqlValue>> {
        team.task_pool
            .iter()
            .filter(|s| s.len() <= INLINE_TEXT_MAX_BYTES)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|s| {
                let mut row = quartet(Some(&team.id), true);
                row.push(SqlValue::Text(s.clone()));
                row
            })
            .collect()
    }
    fn claim(&self, id: &str) -> NativeResult<bool> {
        let rows=self.rows("SELECT run_no,counted,epoch,attempt_no FROM runs WHERE project_id=?1 AND id_form=?2 AND id=?3",&self.identity(id))?;
        match rows.first().map(Vec::as_slice) {
            Some(
                [
                    SqlValue::Null,
                    SqlValue::Null,
                    SqlValue::Null,
                    SqlValue::Null,
                ],
            ) => Ok(false),
            Some(
                [
                    SqlValue::Integer(run),
                    SqlValue::Integer(counted),
                    SqlValue::Integer(epoch),
                    attempt,
                ],
            ) if *run >= 0
                && *epoch >= 0
                && ((*counted == 0 && *attempt == SqlValue::Null)
                    || (*counted == 1 && matches!(attempt,SqlValue::Integer(n) if *n>=0))) =>
            {
                Ok(true)
            }
            _ => Err(native_corrupt(Family::Run, id, "claim tuple", "claim")),
        }
    }
    fn current(&self, family: Family, id: &str, expected: Revision) -> NativeResult<Record> {
        match self.get(family, id)? {
            Some(record) if record.revision == expected => Ok(record),
            other => Err(StoreError::StaleRevision {
                kind: family.kind(),
                id: id.into(),
                expected,
                actual: other.map(|r| r.revision),
            }
            .into()),
        }
    }
    fn next_position(&self, family: Family, id: &str) -> NativeResult<u64> {
        let counter: Option<i64> = self
            .tx
            .query_row(
                "SELECT value FROM counters WHERE project_id=?1 AND name=?2",
                params![self.project, family.counter()],
                |r| r.get(0),
            )
            .optional()?;
        let last = counter.unwrap_or(0);
        if last < 0 {
            return Err(NativeError::Counter(Box::new(
                super::SqliteError::CounterNegative {
                    project: self.project.into(),
                    name: family.counter(),
                    value: last,
                },
            )));
        }
        let (min, max): (Option<i64>, Option<i64>) = self.tx.query_row(
            &format!(
                "SELECT MIN(position),MAX(position) FROM {} WHERE project_id=?1",
                family.table()
            ),
            [self.project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if min.is_some_and(|v| v <= 0) {
            return Err(native_corrupt(family, id, "counter", "position"));
        }
        if max.is_some_and(|v| v > last) {
            return Err(NativeError::Counter(Box::new(if counter.is_none() {
                super::SqliteError::CounterMissing {
                    project: self.project.into(),
                    name: family.counter(),
                }
            } else {
                super::SqliteError::CounterBehind {
                    project: self.project.into(),
                    name: family.counter(),
                    value: last,
                }
            })));
        }
        Ok(last.checked_add(1).ok_or(StoreError::CounterExhausted)? as u64)
    }
    fn store_position(&self, family: Family, position: u64) -> NativeResult<()> {
        self.tx.execute("INSERT INTO counters(project_id,name,value) VALUES(?1,?2,?3) ON CONFLICT(project_id,name) DO UPDATE SET value=excluded.value",params![self.project,family.counter(),integer(position)?])?;
        Ok(())
    }
    fn encode(&self, entity: &Entity) -> NativeResult<Detail> {
        let family = Family::of(entity)?;
        match entity {
            Entity::Agent(v) => encode_agent(self.disk, v),
            Entity::Team(v) => encode_team(self.disk, v),
            Entity::Run(v) => encode_run(self.disk, v),
            _ => return Err(NativeError::Encoding),
        }
        .map_err(|e| format_error(family, entity.id(), e, true))
    }
    pub(super) fn create(&mut self, entity: Entity) -> NativeResult<Record> {
        self.ensure_committable()?;
        let family = Family::of(&entity)?;
        let id = entity.id();
        if self.get(family, id)?.is_some() {
            return Err(StoreError::AlreadyExists {
                kind: family.kind(),
                id: id.into(),
            }
            .into());
        }
        let mut identity = self.identity(id);
        identity.push(SqlValue::Text(family.name().into()));
        for table in ["entity_details", "entity_blob_refs"] {
            if !self.rows(&format!("SELECT 1 FROM {table} WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4 LIMIT 1"),&identity)?.is_empty() {return Err(native_corrupt(family,id,"orphan row",table));}
        }
        if family == Family::Team {
            for table in ["team_members", "team_tasks"] {
                if !self.rows(&format!("SELECT 1 FROM {table} WHERE project_id=?1 AND team_id_form=?2 AND team_id=?3 LIMIT 1"),&self.identity(id))?.is_empty() {return Err(native_corrupt(family,id,"orphan membership",table));}
            }
        }
        let position = self.next_position(family, id)?;
        let projected = projections(&entity)?;
        let detail = self.encode(&entity)?;
        self.atomic(|this| {
            let mut columns = family.columns();
            columns.extend(["project_id".into(), "revision".into(), "position".into()]);
            let mut values = projected;
            values.extend([
                SqlValue::Text(this.project.into()),
                SqlValue::Integer(1),
                SqlValue::Integer(integer(position)?),
            ]);
            let slots = (1..=values.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            this.tx.execute(
                &format!(
                    "INSERT INTO {}({}) VALUES({slots})",
                    family.table(),
                    columns.join(",")
                ),
                rusqlite::params_from_iter(values),
            )?;
            this.install(&entity, &detail)?;
            this.store_position(family, position)?;
            Ok(Record {
                entity,
                revision: Revision::ONE,
                position,
            })
        })
    }
    pub(super) fn update(&mut self, entity: Entity, expected: Revision) -> NativeResult<Record> {
        self.ensure_committable()?;
        let family = Family::of(&entity)?;
        let old = self.current(family, entity.id(), expected)?;
        if let (Entity::Run(old), Entity::Run(new)) = (&old.entity, &entity)
            && self.claim(&old.id)?
            && (old.task_id != new.task_id || old.agent_id != new.agent_id)
        {
            return Err(NativeError::ClaimedIdentity);
        }
        let revision = expected.next()?;
        integer(revision.get())?;
        let projected = projections(&entity)?;
        let detail = self.encode(&entity)?;
        self.atomic(|this| {
            let mut values = this.identity(entity.id());
            values.extend(projected);
            values.push(SqlValue::Integer(integer(revision.get())?));
            let mut assignments = family
                .columns()
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c}=?{}", i + 4))
                .collect::<Vec<_>>();
            assignments.push(format!("revision=?{}", values.len()));
            values.push(SqlValue::Integer(integer(old.position)?));
            assignments.push(format!("position=?{}", values.len()));
            this.tx.execute(
                &format!(
                    "UPDATE {} SET {} WHERE project_id=?1 AND id_form=?2 AND id=?3",
                    family.table(),
                    assignments.join(",")
                ),
                rusqlite::params_from_iter(values),
            )?;
            this.install(&entity, &detail)?;
            Ok(Record {
                entity,
                revision,
                position: old.position,
            })
        })
    }
    pub(super) fn move_to_end(
        &mut self,
        family: Family,
        id: &str,
        expected: Revision,
    ) -> NativeResult<Record> {
        self.ensure_committable()?;
        let old = self.current(family, id, expected)?;
        let revision = expected.next()?;
        integer(revision.get())?;
        let position = self.next_position(family, id)?;
        self.atomic(|this| {
            let mut values=this.identity(id);values.extend([SqlValue::Integer(integer(revision.get())?),SqlValue::Integer(integer(position)?)]);
            this.tx.execute(&format!("UPDATE {} SET revision=?4,position=?5 WHERE project_id=?1 AND id_form=?2 AND id=?3",family.table()),rusqlite::params_from_iter(values))?;
            this.store_position(family,position)?;Ok(Record {entity:old.entity,revision,position})
        })
    }
    pub(super) fn delete(
        &mut self,
        family: Family,
        id: &str,
        expected: Revision,
    ) -> NativeResult<()> {
        self.ensure_committable()?;
        self.current(family, id, expected)?;
        self.atomic(|this| {
            this.clear(family, id)?;
            this.tx.execute(
                &format!(
                    "DELETE FROM {} WHERE project_id=?1 AND id_form=?2 AND id=?3",
                    family.table()
                ),
                rusqlite::params_from_iter(this.identity(id)),
            )?;
            Ok(())
        })
    }
    fn clear(&self, family: Family, id: &str) -> NativeResult<()> {
        let mut values = self.identity(id);
        values.push(SqlValue::Text(family.name().into()));
        for table in ["entity_details", "entity_blob_refs"] {
            self.tx.execute(&format!("DELETE FROM {table} WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4"),rusqlite::params_from_iter(&values))?;
        }
        if family == Family::Team {
            for table in ["team_members", "team_tasks"] {
                self.tx.execute(
                    &format!(
                        "DELETE FROM {table} WHERE project_id=?1 AND team_id_form=?2 AND team_id=?3"
                    ),
                    rusqlite::params_from_iter(self.identity(id)),
                )?;
            }
        }
        Ok(())
    }
    fn install(&self, entity: &Entity, detail: &Detail) -> NativeResult<()> {
        let family = Family::of(entity)?;
        let id = entity.id();
        let (form, key) = key(id);
        self.clear(family, id)?;
        self.tx.execute("INSERT INTO entity_details(project_id,family,key_form,entity_key,format,detail,detail_bytes,detail_sha256) VALUES(?1,?2,?3,?4,1,?5,?6,?7)",params![self.project,family.name(),form,key,detail.inline,integer(detail.bytes)?,detail.sha256])?;
        for (locator, chunks) in &detail.refs {
            for chunk in chunks {
                let hash = chunk
                    .sha256
                    .parse::<BlobHash>()
                    .map_err(|_| NativeError::Encoding)?;
                BlobReferences::register_published(
                    self.tx,
                    self.project,
                    &hash,
                    integer(chunk.size)?,
                    self.now,
                )
                .map_err(|e| match e {
                    ReferenceError::Sql(e) => NativeError::Sql(e),
                    _ => native_corrupt(family, id, "blob registration", "blob_ref"),
                })?;
                self.tx.execute("INSERT INTO entity_blob_refs(project_id,family,key_form,entity_key,locator,ordinal,sha256,size) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![self.project,family.name(),form,key,locator,integer(chunk.ordinal)?,chunk.sha256,integer(chunk.size)?])?;
            }
        }
        if let Entity::Team(team) = entity {
            for row in Self::member_values(team)? {
                let mut values = vec![SqlValue::Text(self.project.into())];
                values.extend(row);
                self.tx.execute("INSERT INTO team_members(project_id,team_id,team_id_form,team_id_sha256,team_id_bytes,ordinal,agent_id,role,role_form,role_sha256,role_bytes,joined_at,joined_at_text,joined_at_text_form,joined_at_text_sha256,joined_at_text_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",rusqlite::params_from_iter(values))?;
            }
            for row in Self::task_values(team) {
                let mut values = vec![SqlValue::Text(self.project.into())];
                values.extend(row);
                self.tx.execute("INSERT INTO team_tasks(project_id,team_id,team_id_form,team_id_sha256,team_id_bytes,task_id) VALUES(?1,?2,?3,?4,?5,?6)",rusqlite::params_from_iter(values))?;
            }
        }
        Ok(())
    }
}
