//! Strict complete Task detail codec. SQL reachability and corruption policy
//! remain with the selecting transaction, not with this byte-format leaf.
use super::text::{self, Chunk, Reference, Tag, TextError};
use crate::blob::{BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use orch_domain::Task;
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub(in crate::sqlite) enum CodecError {
    #[error("task value is not representable: {0}")]
    Unrepresentable(&'static str),
    #[error("task detail failed {check} at {locator}")]
    Invalid {
        check: &'static str,
        locator: String,
    },
    #[error("task object access failed at {locator}: {kind:?}")]
    Access {
        kind: std::io::ErrorKind,
        locator: String,
    },
    #[error(transparent)]
    Text(#[from] TextError),
}
type Result<T> = std::result::Result<T, CodecError>;

/// Rows are ordered by locator then ordinal, retaining repeated object hashes.
/// The SQL caller adds the authorized project and task to every row.
#[derive(Debug, Clone)]
pub(in crate::sqlite) struct Detail {
    pub inline: Option<String>,
    pub bytes: u64,
    pub sha256: String,
    pub refs: BTreeMap<String, Vec<Chunk>>,
}

fn invalid(check: &'static str, locator: &str) -> CodecError {
    CodecError::Invalid {
        check,
        locator: locator.into(),
    }
}

/// Validate keys and canonical times before the first object is published.
pub(in crate::sqlite) fn validate(task: &Task) -> Result<()> {
    for key in [
        Some(task.id.as_str()),
        task.assignee.as_deref(),
        task.goal_id.as_deref(),
        Some(task.created_at.as_str()),
        Some(task.updated_at.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if key.len() > INLINE_TEXT_MAX_BYTES {
            return Err(CodecError::Unrepresentable("key over 65536 bytes"));
        }
    }
    milliseconds(&task.created_at)?;
    milliseconds(&task.updated_at)?;
    Ok(())
}

pub(in crate::sqlite) fn milliseconds(text: &str) -> Result<i64> {
    let parsed = text
        .parse::<jiff::Timestamp>()
        .map_err(|_| CodecError::Unrepresentable("invalid task timestamp"))?;
    let ms = parsed.as_millisecond();
    if !(-62_167_219_200_000..=jiff::Timestamp::MAX.as_millisecond()).contains(&ms)
        || orch_core::clock::iso_from_epoch_ms(ms) != text
    {
        return Err(CodecError::Unrepresentable(
            "task timestamp must be canonical millisecond UTC",
        ));
    }
    Ok(ms)
}

pub(in crate::sqlite) fn encode(store: &DiskBlobStore, task: &Task) -> Result<Detail> {
    validate(task)?;
    // preserve_order is pinned in the workspace: domain field order is the wire order.
    let mut value = serde_json::to_value(task).map_err(|_| invalid("typed encoding", ""))?;
    let slots = slots(&value)?;
    let mut refs = BTreeMap::new();
    for locator in slots {
        let slot = value
            .pointer_mut(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        let text = slot
            .as_str()
            .ok_or_else(|| invalid("text type", &locator))?;
        if text.len() > INLINE_TEXT_MAX_BYTES {
            let (reference, chunks) = text::publish(store, text.as_bytes())?;
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

/// The caller must verify project-scoped `blobs` metadata in the same snapshot.
/// This function validates exact locator coverage and immutable object contents.
pub(in crate::sqlite) fn decode(store: &DiskBlobStore, detail: &Detail) -> Result<Task> {
    let bytes = match &detail.inline {
        Some(text) if text.len() <= INLINE_TEXT_MAX_BYTES && text.len() as u64 == detail.bytes => {
            text.as_bytes().to_vec()
        }
        Some(_) => return Err(invalid("inline detail size", "")),
        None => reconstruct(
            store,
            &Reference {
                sha256: detail.sha256.clone(),
                bytes: detail.bytes,
                chunks: detail.bytes.div_ceil(MAX_BLOB_BYTES),
            },
            detail.refs.get("").map(Vec::as_slice).unwrap_or_default(),
            "",
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
    for locator in slots(&value)? {
        let slot = value
            .pointer(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        if let Some(text) = slot.as_str() {
            if text.len() > INLINE_TEXT_MAX_BYTES {
                return Err(invalid("inline text size", &locator));
            }
        } else {
            if matches!(
                locator.as_str(),
                "/id" | "/assignee" | "/goalId" | "/created_at" | "/updated_at"
            ) {
                return Err(invalid("indirect key", &locator));
            }
            let tag: Tag = serde_json::from_value(slot.clone())
                .map_err(|_| invalid("reference tag", &locator))?;
            text::validate_chunks(
                &tag.reference,
                detail
                    .refs
                    .get(&locator)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
            .map_err(|_| invalid("reference metadata", &locator))?;
            used.insert(locator.clone());
            tags.push((locator, tag));
        }
    }
    if detail.refs.keys().cloned().collect::<BTreeSet<_>>() != used {
        return Err(invalid("reference locators", ""));
    }
    // Prove inline types and timestamps before transient slot access can hide
    // their corruption. Referenced text is checked after reconstruction below.
    let mut typed = value.clone();
    for (locator, _) in &tags {
        let slot = typed
            .pointer_mut(locator)
            .ok_or_else(|| invalid("text slot", locator))?;
        *slot = Value::String(String::new());
    }
    let typed: Task = serde_json::from_value(typed).map_err(|_| invalid("typed detail", ""))?;
    validate(&typed).map_err(|_| invalid("keys or timestamps", ""))?;
    // The root necessarily precedes JSON parsing. Slot metadata, reachability
    // and inline typed values are now proven before opening any slot's object.
    for (locator, tag) in tags {
        let bytes = reconstruct(
            store,
            &tag.reference,
            detail
                .refs
                .get(&locator)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &locator,
        )?;
        let slot = value
            .pointer_mut(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        *slot =
            Value::String(String::from_utf8(bytes).map_err(|_| invalid("text UTF-8", &locator))?);
    }
    let task: Task = serde_json::from_value(value).map_err(|_| invalid("typed detail", ""))?;
    validate(&task).map_err(|_| invalid("keys or timestamps", ""))?;
    Ok(task)
}

fn reconstruct(
    store: &DiskBlobStore,
    reference: &Reference,
    rows: &[Chunk],
    locator: &str,
) -> Result<Vec<u8>> {
    // Read errors may contain paths. Only fixed checks and schema-derived locators escape.
    text::reconstruct(store, reference, rows).map_err(|error| match error {
        TextError::Access(kind) => CodecError::Access {
            kind,
            locator: locator.into(),
        },
        _ => invalid("referenced bytes", locator),
    })
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

fn text_list(value: &Value, path: &str, slots: &mut Vec<String>) -> Result<()> {
    for (index, _) in array(value, path)?.iter().enumerate() {
        slots.push(format!("{path}/{index}"));
    }
    Ok(())
}

/// Enumerate only schema-owned string slots. Unknown keys, explicit nulls and
/// absent required collections must be refused before the domain's permissive serde.
fn slots(value: &Value) -> Result<Vec<String>> {
    let root = object(
        value,
        "",
        &[
            "id",
            "title",
            "description",
            "status",
            "priority",
            "labels",
            "depends_on",
            "created_at",
            "updated_at",
            "attempts",
            "max_attempts",
        ],
        &[
            "assignee",
            "workspace_mode",
            "workspace",
            "proof",
            "review_criteria",
            "review_results",
            "scope",
            "feedback",
            "goalId",
            "attachments",
        ],
    )?;
    let mut slots = Vec::new();
    for (key, value) in root {
        let path = format!("/{key}");
        match key.as_str() {
            "priority" | "attempts" | "max_attempts" => (),
            "labels" | "depends_on" | "scope" | "attachments" | "review_criteria" => {
                text_list(value, &path, &mut slots)?
            }
            "proof" => {
                let proof = object(
                    value,
                    &path,
                    &["files_changed"],
                    &[
                        "remote",
                        "branch",
                        "pr_url",
                        "test_results",
                        "agent_summary",
                    ],
                )?;
                for (key, value) in proof {
                    let path = format!("/proof/{key}");
                    if key == "files_changed" {
                        text_list(value, &path, &mut slots)?;
                    } else {
                        slots.push(path);
                    }
                }
            }
            "review_results" => {
                for (index, result) in array(value, &path)?.iter().enumerate() {
                    let path = format!("/review_results/{index}");
                    object(result, &path, &["criterion", "passed", "output"], &[])?;
                    slots.push(format!("{path}/criterion"));
                    slots.push(format!("{path}/output"));
                }
            }
            _ => slots.push(path),
        }
    }
    Ok(slots)
}

/// serde_json::Value alone overwrites duplicate keys. Preserve the JSON parser's
/// numeric types and recursion limit while refusing duplicates at every depth.
pub(in crate::sqlite) struct UniqueValue(pub(in crate::sqlite) Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: de::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("unique-key JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite JSON"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

#[cfg(test)]
mod tests;
