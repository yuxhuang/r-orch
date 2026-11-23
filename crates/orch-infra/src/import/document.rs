//! Budget checks run before deserialization on the same retained bytes.
use serde::de::DeserializeOwned;
use std::{collections::HashMap, fmt, io::Read, path::Path};

/// Source document ceilings from the spec's Import bounds table.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) bytes: usize,
    pub(crate) depth: u64,
    pub(crate) nodes: u64,
    pub(crate) aliases: u64,
    pub(crate) expanded_bytes: u64,
}
impl Limits {
    pub(crate) const YAML: Self = Self {
        bytes: 1_048_576,
        depth: 64,
        nodes: 200_000,
        aliases: 64,
        expanded_bytes: 4_194_304,
    };
    pub(crate) const JSON: Self = Self {
        bytes: 16_777_216,
        depth: 64,
        nodes: 1_000_000,
        aliases: 0,
        expanded_bytes: 0,
    };
    pub(crate) const EVENT: Self = Self {
        bytes: 65_536,
        ..Self::JSON
    };
}

/// A path-free refusal; the caller supplies the escaped source-relative path.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) rule: &'static str,
    pub(crate) limit: u64,
    pub(crate) observed: u64,
    pub(crate) detail: String,
}
impl Refusal {
    fn budget(rule: &'static str, limit: u64, observed: u64) -> Self {
        let compatibility = if matches!(rule, "depth" | "expanded_bytes" | "tag") {
            "; this import rule is stricter than the 1.0 reader"
        } else {
            ""
        };
        Self {
            rule,
            limit,
            observed,
            detail: format!("{rule}: limit {limit}, observed at least {observed}{compatibility}"),
        }
    }
    pub(super) fn invalid(rule: &'static str, error: impl fmt::Display) -> Self {
        Self {
            rule,
            limit: 0,
            observed: 0,
            detail: error.to_string(),
        }
    }
}
impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for Refusal {}
type Result<T> = std::result::Result<T, Refusal>;

fn within(rule: &'static str, value: u64, limit: u64) -> Result<u64> {
    if value > limit {
        Err(Refusal::budget(rule, limit, value))
    } else {
        Ok(value)
    }
}
fn add(rule: &'static str, a: u64, b: u64, limit: u64) -> Result<u64> {
    let sum = a
        .checked_add(b)
        .ok_or_else(|| Refusal::budget(rule, limit, u64::MAX))?;
    within(rule, sum, limit)
}

/// Reads at most the ceiling plus one byte. Does not open or modify a path.
pub(crate) fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let cap = (limit as u64)
        .checked_add(1)
        .ok_or_else(|| Refusal::invalid("bytes", "invalid byte ceiling"))?;
    let mut bytes = Vec::new();
    reader
        .take(cap)
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::invalid("io", e))?;
    within("bytes", bytes.len() as u64, limit as u64)?;
    Ok(bytes)
}

#[derive(Clone, Copy, Default)]
struct Count {
    nodes: u64,
    bytes: u64,
    height: u64,
}
fn place(
    node: Count,
    frames: &mut [(usize, Count)],
    total: &mut Count,
    limits: Limits,
) -> Result<()> {
    let target = frames.last_mut().map_or(total, |(_, count)| count);
    target.nodes = add("expanded_nodes", target.nodes, node.nodes, limits.nodes)?;
    target.bytes = add(
        "expanded_bytes",
        target.bytes,
        node.bytes,
        limits.expanded_bytes,
    )?;
    target.height = target.height.max(node.height);
    Ok(())
}

/// Iterative event fold. Anchors memoize counts and heights, never expanded trees.
/// Uses the same maintained event parser as the guarded 1.0 reader.
pub(crate) fn check_yaml(bytes: &[u8], limits: Limits) -> Result<()> {
    use saphyr_parser::{Event, Parser};
    within("bytes", bytes.len() as u64, limits.bytes as u64)?;
    let text = std::str::from_utf8(bytes).map_err(|e| Refusal::invalid("parse", e))?;
    let mut anchors = HashMap::new();
    let mut frames: Vec<(usize, Count)> = Vec::new();
    let mut total = Count::default();
    let mut aliases = 0;
    let mut documents = 0;
    for event in Parser::new_from_str(text) {
        let (event, _) = event.map_err(|e| {
            // saphyr's scanner can look ahead before emitting the first event.
            // Its u8 flow-level counter refuses the 256th open collection.
            if e.info() == "recursion limit exceeded" {
                Refusal::budget("depth", limits.depth, 256)
            } else {
                Refusal::invalid("parse", e)
            }
        })?;
        match event {
            Event::DocumentStart(_) => {
                documents = add("documents", documents, 1, 1)?;
            }
            Event::Scalar(value, _, anchor, tag) => {
                if tag.is_some() {
                    return Err(Refusal::budget("tag", 0, 1));
                }
                let node = Count {
                    nodes: 1,
                    bytes: value.len() as u64,
                    height: 0,
                };
                if anchor != 0 {
                    anchors.insert(anchor, node);
                }
                place(node, &mut frames, &mut total, limits)?;
            }
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) => {
                if tag.is_some() {
                    return Err(Refusal::budget("tag", 0, 1));
                }
                within("depth", frames.len() as u64 + 1, limits.depth)?;
                within("expanded_nodes", 1, limits.nodes)?;
                frames.push((
                    anchor,
                    Count {
                        nodes: 1,
                        ..Count::default()
                    },
                ));
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (anchor, mut node) = frames
                    .pop()
                    .ok_or_else(|| Refusal::invalid("parse", "unbalanced collection"))?;
                node.height = add("depth", node.height, 1, limits.depth)?;
                if anchor != 0 {
                    anchors.insert(anchor, node);
                }
                place(node, &mut frames, &mut total, limits)?;
            }
            Event::Alias(anchor) => {
                aliases = add("aliases", aliases, 1, limits.aliases)?;
                let node: Count = *anchors
                    .get(&anchor)
                    .ok_or_else(|| Refusal::budget("alias_cycle", 0, 1))?;
                add("depth", frames.len() as u64, node.height, limits.depth)?;
                place(node, &mut frames, &mut total, limits)?;
            }
            _ => {}
        }
    }
    if documents == 0 {
        // Zero documents is not proof that the stream scans: this parser emits only
        // stream events for a stream whose sole content is a tab at the start of a
        // line, and 1.0's scanner refuses it (C-V5). Only a stream that 1.0 also
        // scans is a stream of no document. `IgnoredAny` builds no tree and follows
        // no alias, so this scan costs one linear pass whatever the stream holds.
        serde_yaml_ng::from_str::<serde::de::IgnoredAny>(text)
            .map_err(|e| Refusal::invalid("parse", e))?;
    }
    if documents != 1 {
        return Err(Refusal {
            rule: "documents",
            limit: 1,
            observed: documents,
            detail: format!("expected one document, found {documents}"),
        });
    }
    Ok(())
}

/// Budget the bytes first, then use 1.0's real reader to preserve merge semantics.
#[cfg(test)]
pub(crate) async fn parse_yaml<T: DeserializeOwned>(
    reader: impl Read,
    limits: Limits,
) -> Result<T> {
    let bytes = read_bounded(reader, limits.bytes)?;
    decode_yaml(&bytes, limits).await
}

/// The same check and decode on bytes the caller already holds, so a caller that
/// acquires a file off the async executor can still decode the bytes it measured.
pub(crate) async fn decode_yaml<T: DeserializeOwned>(bytes: &[u8], limits: Limits) -> Result<T> {
    check_yaml(bytes, limits)?;
    let value = orch_core::definition_yaml::parse_yaml_bytes(Path::new("<source>"), bytes)
        .await
        .map_err(|e| Refusal::invalid("parse", e))?;
    serde_yaml_ng::from_value(value).map_err(|e| Refusal::invalid("type", e))
}

// serde_json exposes a recursive visitor but no iterative parse-event API.
// The binding import contract forbids input-controlled recursion. This small
// structural pass uses the compatibility exemption of AGENTS.md rule 4:
// serde_json still validates each scalar, including escapes and number syntax.
#[derive(Clone, Copy)]
enum Frame {
    ArrayValue(bool),
    ArrayEnd,
    ObjectKey(bool),
    ObjectColon,
    ObjectValue,
    ObjectEnd,
}

// Keep the structural budget below the recursive typed decoder's ceiling.
// Raising a caller's limit must not expose input-controlled stack growth.
const JSON_DEPTH_CLAMP: u64 = 64;

/// Iterative structural events; serde_json validates scalar tokens. Member
/// names do not count as nodes. No tree or recursive visitor is constructed.
pub(crate) fn check_json(bytes: &[u8], limits: Limits) -> Result<()> {
    use serde::de::IgnoredAny;
    within("bytes", bytes.len() as u64, limits.bytes as u64)?;
    let invalid = || Refusal::invalid("parse", "invalid JSON structure");
    let mut stack = Vec::new();
    let mut root = false;
    let mut nodes = 0;
    let mut pos = 0;
    while pos < bytes.len() {
        let byte = bytes[pos];
        if matches!(byte, b' ' | b'\n' | b'\r' | b'\t') {
            pos += 1;
            continue;
        }
        if matches!(byte, b']' | b'}') {
            let valid = matches!(
                (byte, stack.last()),
                (b']', Some(Frame::ArrayValue(true) | Frame::ArrayEnd))
                    | (b'}', Some(Frame::ObjectKey(true) | Frame::ObjectEnd))
            );
            if !valid {
                return Err(invalid());
            }
            stack.pop();
            pos += 1;
            continue;
        }
        if byte == b',' {
            match stack.last_mut() {
                Some(state @ Frame::ArrayEnd) => *state = Frame::ArrayValue(false),
                Some(state @ Frame::ObjectEnd) => *state = Frame::ObjectKey(false),
                _ => return Err(invalid()),
            }
            pos += 1;
            continue;
        }
        if byte == b':' {
            match stack.last_mut() {
                Some(state @ Frame::ObjectColon) => *state = Frame::ObjectValue,
                _ => return Err(invalid()),
            }
            pos += 1;
            continue;
        }
        let is_key = matches!(stack.last(), Some(Frame::ObjectKey(_)));
        if is_key && byte != b'"' {
            return Err(invalid());
        }
        match stack.last_mut() {
            Some(state @ Frame::ObjectKey(_)) => *state = Frame::ObjectColon,
            Some(state @ Frame::ObjectValue) => *state = Frame::ObjectEnd,
            Some(state @ Frame::ArrayValue(_)) => *state = Frame::ArrayEnd,
            None if !root => root = true,
            _ => return Err(invalid()),
        }
        if !is_key {
            nodes = add("nodes", nodes, 1, limits.nodes)?;
        }
        if matches!(byte, b'[' | b'{') {
            within(
                "depth",
                stack.len() as u64 + 1,
                limits.depth.min(JSON_DEPTH_CLAMP),
            )?;
            stack.push(if byte == b'[' {
                Frame::ArrayValue(true)
            } else {
                Frame::ObjectKey(true)
            });
            pos += 1;
            continue;
        }
        let start = pos;
        if byte == b'"' {
            pos += 1;
            let mut closed = false;
            while pos < bytes.len() {
                match bytes[pos] {
                    b'"' => {
                        pos += 1;
                        closed = true;
                        break;
                    }
                    b'\\' => pos = pos.saturating_add(2),
                    _ => pos += 1,
                }
            }
            if !closed {
                return Err(invalid());
            }
        } else {
            while pos < bytes.len()
                && !matches!(
                    bytes[pos],
                    b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}' | b':' | b'[' | b'{'
                )
            {
                pos += 1;
            }
        }
        if pos == start {
            return Err(invalid());
        }
        if byte == b'"' {
            // IgnoredAny intentionally accepts unpaired surrogate escapes and
            // invalid UTF-8. The real string reader must validate source text.
            serde_json::from_slice::<String>(&bytes[start..pos])
                .map_err(|e| Refusal::invalid("parse", e))?;
        } else {
            serde_json::from_slice::<IgnoredAny>(&bytes[start..pos])
                .map_err(|e| Refusal::invalid("parse", e))?;
        }
    }
    if !root || !stack.is_empty() {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn parse_json<T: DeserializeOwned>(reader: impl Read, limits: Limits) -> Result<T> {
    let bytes = read_bounded(reader, limits.bytes)?;
    decode_json(&bytes, limits)
}

/// The same check and decode on bytes the caller already holds.
pub(crate) fn decode_json<T: DeserializeOwned>(bytes: &[u8], limits: Limits) -> Result<T> {
    check_json(bytes, limits)?;
    T::deserialize(&mut serde_json::Deserializer::from_slice(bytes))
        .map_err(|e| Refusal::invalid("type", e))
}

#[cfg(test)]
#[path = "tests/arithmetic.rs"]
mod arithmetic_tests;
