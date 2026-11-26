//! Message format v1: the detail of the `message` family of Entity format v1.
//!
//! One compact JSON object in the domain's field order, with ten text slots.
//! A slot is a JSON string of at most 65,536 UTF-8 bytes or the tag of the Task
//! rules, and `to_agent_id` is `null` for no recipient. The byte-level text
//! rules (thresholds, chunking, hashing) are the shared `task_format::text`
//! ones; only the typed checks of a message are here. Locators are the fixed
//! RFC 6901 pointers of the slots, so a locator is never stored data.
use std::collections::{BTreeMap, BTreeSet};

use orch_domain::{Message, MessageChannel, MessageStatus};
use serde_json::{Map, Value};

use crate::blob::{BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use crate::sqlite::task_format::{
    codec::{Detail, UniqueValue},
    text::{self, Chunk, Reference, Tag, TextError},
};

/// The ten text slots with the detail key each one is stored under.
pub(super) const SLOTS: [(&str, &str); 10] = [
    ("id", "/id"),
    ("from_agent_id", "/from_agent_id"),
    ("to_agent_id", "/to_agent_id"),
    ("subject", "/subject"),
    ("body", "/body"),
    ("created_at", "/created_at"),
    ("expires_at", "/expires_at"),
    ("delivered_at", "/delivered_at"),
    ("team_id", "/team_id"),
    ("reply_to", "/reply_to"),
];
const REQUIRED: [&str; 8] = [
    "id",
    "channel",
    "from_agent_id",
    "to_agent_id",
    "subject",
    "body",
    "created_at",
    "status",
];

#[derive(Debug)]
pub(super) enum FormatError {
    /// Stored data that cannot be a detail this format wrote.
    Invalid {
        check: &'static str,
        locator: &'static str,
    },
    /// A transient failure to read an object; it proves no damage.
    Access(std::io::ErrorKind),
    /// Publishing an object failed on a write; nothing refers to it.
    Publish(crate::blob::BlobError),
}
type Result<T> = std::result::Result<T, FormatError>;

fn invalid(check: &'static str, locator: &'static str) -> FormatError {
    FormatError::Invalid { check, locator }
}

fn publish_error(error: TextError) -> FormatError {
    match error {
        TextError::Publish(error) => FormatError::Publish(error),
        TextError::Access(kind) => FormatError::Access(kind),
        TextError::Inline | TextError::Corrupt(_) => invalid("published object", ""),
    }
}

/// The text of a slot of one message, if the message has one.
pub(super) fn slot_text<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    match name {
        "id" => Some(&message.id),
        "from_agent_id" => Some(&message.from_agent_id),
        "to_agent_id" => message.to_agent_id.as_deref(),
        "subject" => Some(&message.subject),
        "body" => Some(&message.body),
        "created_at" => Some(&message.created_at),
        "expires_at" => message.expires_at.as_deref(),
        "delivered_at" => message.delivered_at.as_deref(),
        "team_id" => message.team_id.as_deref(),
        "reply_to" => message.reply_to.as_deref(),
        _ => None,
    }
}

/// One slot as the detail holds it: the text, or the tag of an object published
/// first. The chunk rows of the slot are kept for the reachability rows.
fn slot(
    store: &DiskBlobStore,
    refs: &mut BTreeMap<String, Vec<Chunk>>,
    locator: &'static str,
    text: &str,
) -> Result<Value> {
    if text.len() <= INLINE_TEXT_MAX_BYTES {
        return Ok(Value::String(text.to_owned()));
    }
    let (reference, chunks) = text::publish(store, text.as_bytes()).map_err(publish_error)?;
    refs.insert(locator.to_owned(), chunks);
    serde_json::to_value(Tag { reference }).map_err(|_| invalid("tag encoding", locator))
}

/// Publish every object of a text over 65,536 bytes, then return the detail.
/// Objects of a failed encoding stay unreferenced for the normal sweep.
pub(super) fn encode(store: &DiskBlobStore, message: &Message) -> Result<Detail> {
    let mut refs = BTreeMap::new();
    let mut map = Map::new();
    for (name, locator) in SLOTS {
        // The domain's order interleaves the two words with the slots.
        match name {
            "from_agent_id" => {
                map.insert("channel".into(), message.channel.as_str().into());
            }
            "delivered_at" => {
                map.insert("status".into(), message.status.as_str().into());
            }
            _ => {}
        }
        match (slot_text(message, name), name) {
            (Some(text), _) => {
                let value = slot(store, &mut refs, locator, text)?;
                map.insert(name.into(), value);
            }
            (None, "to_agent_id") => {
                map.insert(name.into(), Value::Null);
            }
            (None, _) => {}
        }
    }
    let bytes =
        serde_json::to_vec(&Value::Object(map)).map_err(|_| invalid("detail encoding", ""))?;
    let sha256 = BlobHash::of(&bytes).to_string();
    let inline = if bytes.len() <= INLINE_TEXT_MAX_BYTES {
        Some(String::from_utf8(bytes.clone()).map_err(|_| invalid("detail UTF-8", ""))?)
    } else {
        let (_, chunks) = text::publish(store, &bytes).map_err(publish_error)?;
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

fn read_error(error: TextError, locator: &'static str) -> FormatError {
    match error {
        TextError::Access(kind) => FormatError::Access(kind),
        _ => invalid("referenced bytes", locator),
    }
}

/// A slot of the stored detail after its structure was checked, before any
/// object was opened.
enum Slot {
    Inline(String),
    Object(Reference),
}

fn check_slot(
    value: &Value,
    locator: &'static str,
    detail: &Detail,
    used: &mut BTreeSet<String>,
) -> Result<Slot> {
    if let Some(text) = value.as_str() {
        if text.len() > INLINE_TEXT_MAX_BYTES {
            return Err(invalid("inline text size", locator));
        }
        return Ok(Slot::Inline(text.to_owned()));
    }
    let tag: Tag =
        serde_json::from_value(value.clone()).map_err(|_| invalid("slot type", locator))?;
    let rows = detail
        .refs
        .get(locator)
        .map(Vec::as_slice)
        .unwrap_or_default();
    text::validate_chunks(&tag.reference, rows)
        .map_err(|_| invalid("reference metadata", locator))?;
    used.insert(locator.to_owned());
    Ok(Slot::Object(tag.reference))
}

/// Decode the authoritative detail. The caller has checked, in the selecting
/// snapshot, the row's format, project, family and key, the `blobs` rows and
/// the reference rows' own shape.
pub(super) fn decode(store: &DiskBlobStore, detail: &Detail) -> Result<Message> {
    let bytes = match &detail.inline {
        Some(text) if text.len() <= INLINE_TEXT_MAX_BYTES && text.len() as u64 == detail.bytes => {
            text.as_bytes().to_vec()
        }
        Some(_) => return Err(invalid("inline detail size", "")),
        None => {
            let reference = Reference {
                sha256: detail.sha256.clone(),
                bytes: detail.bytes,
                chunks: detail.bytes.div_ceil(MAX_BLOB_BYTES),
            };
            let rows = detail.refs.get("").map(Vec::as_slice).unwrap_or_default();
            text::validate_chunks(&reference, rows)
                .map_err(|_| invalid("reference metadata", ""))?;
            text::reconstruct(store, &reference, rows).map_err(|e| read_error(e, ""))?
        }
    };
    if bytes.len() as u64 != detail.bytes || BlobHash::of(&bytes).to_string() != detail.sha256 {
        return Err(invalid("detail length or hash", ""));
    }
    let value = serde_json::from_slice::<UniqueValue>(&bytes)
        .map_err(|_| invalid("strict JSON", ""))?
        .0;
    let object = value
        .as_object()
        .ok_or_else(|| invalid("object type", ""))?;
    let known = |key: &str| {
        key == "channel" || key == "status" || SLOTS.iter().any(|(name, _)| *name == key)
    };
    if REQUIRED.iter().any(|key| !object.contains_key(*key))
        || object.keys().any(|key| !known(key))
        || object
            .iter()
            .any(|(key, value)| value.is_null() && key != "to_agent_id")
    {
        return Err(invalid("detail keys", ""));
    }
    let word = |key: &'static str, locator: &'static str| {
        object[key]
            .as_str()
            .ok_or_else(|| invalid("word type", locator))
    };
    let channel: MessageChannel = word("channel", "/channel")?
        .parse()
        .map_err(|()| invalid("channel word", "/channel"))?;
    let status: MessageStatus = word("status", "/status")?
        .parse()
        .map_err(|()| invalid("status word", "/status"))?;
    let mut used = BTreeSet::new();
    if detail.inline.is_none() {
        used.insert(String::new());
    }
    let mut slots: Vec<(&str, &'static str, Option<Slot>)> = Vec::new();
    for (name, locator) in SLOTS {
        let found = match object.get(name) {
            None => None,
            Some(Value::Null) => None,
            Some(value) => Some(check_slot(value, locator, detail, &mut used)?),
        };
        slots.push((name, locator, found));
    }
    if detail.refs.keys().cloned().collect::<BTreeSet<_>>() != used {
        return Err(invalid("reference locators", ""));
    }
    let mut texts: BTreeMap<&str, String> = BTreeMap::new();
    for (name, locator, found) in slots {
        let text = match found {
            None => continue,
            Some(Slot::Inline(text)) => text,
            Some(Slot::Object(reference)) => {
                let rows = detail
                    .refs
                    .get(locator)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let bytes = text::reconstruct(store, &reference, rows)
                    .map_err(|e| read_error(e, locator))?;
                String::from_utf8(bytes).map_err(|_| invalid("text UTF-8", locator))?
            }
        };
        texts.insert(name, text);
    }
    // A required slot is a string: only `to_agent_id` may be `null`.
    let mut required = |name: &'static str| {
        let locator = SLOTS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or("", |(_, l)| *l);
        texts
            .remove(name)
            .ok_or_else(|| invalid("slot type", locator))
    };
    let (id, from_agent_id, subject, body, created_at) = (
        required("id")?,
        required("from_agent_id")?,
        required("subject")?,
        required("body")?,
        required("created_at")?,
    );
    Ok(Message {
        id,
        channel,
        from_agent_id,
        to_agent_id: texts.remove("to_agent_id"),
        subject,
        body,
        created_at,
        expires_at: texts.remove("expires_at"),
        status,
        delivered_at: texts.remove("delivered_at"),
        team_id: texts.remove("team_id"),
        reply_to: texts.remove("reply_to"),
    })
}
