//! Bounded JSON/YAML definition input shared by file mutation commands.

use std::fmt;
use std::path::Path;

use orch_core::error::{Error, Result};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use tokio::io::AsyncReadExt;

pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_DOCUMENT_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InputFormat {
    Json,
    Yaml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Update,
    Patch,
}

/// Read one object from a regular file or `-` (stdin). Parser selection is final;
/// output flags never influence the input language.
pub async fn read_document(file: &Path, format: Option<InputFormat>) -> Result<Value> {
    let source = if file == Path::new("-") {
        Path::new("<stdin>")
    } else {
        file
    };
    let mut bytes = Vec::new();
    let limit = (MAX_DOCUMENT_BYTES + 1) as u64;
    if file == Path::new("-") {
        bytes = tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .take(limit)
                .read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        })
        .await
        .map_err(|_| invalid(source, "stdin reader failed"))?
        .map_err(|err| Error::io("read <stdin>", source, err))?;
    } else {
        let metadata = tokio::fs::metadata(file)
            .await
            .map_err(|err| Error::io("read mutation input", file, err))?;
        if !metadata.is_file() {
            return Err(invalid(source, "expected a regular input file"));
        }
        tokio::fs::File::open(file)
            .await
            .map_err(|err| Error::io("open mutation input", file, err))?
            .take(limit)
            .read_to_end(&mut bytes)
            .await
            .map_err(|err| Error::io("read mutation input", file, err))?;
    }
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid(source, "document exceeds 1 MiB"));
    }
    let format = format.unwrap_or_else(|| match file.extension().and_then(|s| s.to_str()) {
        Some("json") => InputFormat::Json,
        Some("yml" | "yaml") => InputFormat::Yaml,
        _ if matches!(
            bytes.iter().find(|b| !b.is_ascii_whitespace()),
            Some(b'{' | b'[')
        ) =>
        {
            InputFormat::Json
        }
        _ => InputFormat::Yaml,
    });
    let value = match format {
        InputFormat::Json => parse_json(source, &bytes)?,
        InputFormat::Yaml => {
            orch_infra::storage::fs_utils::parse_definition_yaml_bytes(source, &bytes).await?
        }
    };
    require_object(&value).map_err(|_| invalid(source, "document root: expected an object"))?;
    Ok(value)
}

/// Decode JSON with the same duplicate-key and depth checks as file mutations.
pub(crate) fn parse_json(source: &Path, bytes: &[u8]) -> Result<Value> {
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue(0).deserialize(&mut parser).map_err(|_| {
        invalid(
            source,
            "invalid JSON (check types, duplicate keys and depth <= 64)",
        )
    })?;
    parser
        .end()
        .map_err(|_| invalid(source, "expected exactly one JSON document"))?;
    Ok(value)
}

/// Attach input provenance without changing the existing error/exit-code class.
pub(crate) fn at_source(file: &Path, error: Error) -> Error {
    let source = if file == Path::new("-") {
        "<stdin>".to_owned()
    } else {
        file.display().to_string()
    };
    match error {
        Error::Domain(orch_domain::DomainError::InvalidArguments(reason)) => {
            Error::invalid_arguments(format!("{source}: {reason}"))
        }
        Error::Io {
            context,
            path,
            source: cause,
        } => Error::Io {
            context: format!("{source}: {context}"),
            path,
            source: cause,
        },
        other => other,
    }
}

fn invalid(source: &Path, reason: &str) -> Error {
    Error::invalid_arguments(format!("{}: {reason}", source.display()))
}

fn require_object(value: &Value) -> Result<()> {
    if !value.is_object() {
        return Err(Error::invalid_arguments(
            "document root: expected an object",
        ));
    }
    Ok(())
}

/// Pure RFC 7396 operation. Allowed/immutable paths and target validation belong
/// to the caller; validate those paths BEFORE this call (including null keys).
pub fn apply_mutation(current: &Value, input: &Value, kind: MutationKind) -> Result<Value> {
    require_object(input)?;
    check_depth(input, 0)?;
    let candidate = match kind {
        MutationKind::Update => input.clone(),
        MutationKind::Patch => {
            require_object(current)?;
            check_depth(current, 0)?;
            let mut candidate = current.clone();
            json_patch::merge(&mut candidate, input);
            candidate
        }
    };
    check_depth(&candidate, 0)?;
    Ok(candidate)
}

fn check_depth(value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_DOCUMENT_DEPTH {
        return Err(Error::invalid_arguments("document depth exceeds 64"));
    }
    match value {
        Value::Object(map) => {
            for value in map.values() {
                check_depth(value, depth + 1)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                check_depth(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Serde supplies the grammar and scalar parsing; this visitor adds duplicate-key
// and depth validation before a JSON Value can discard that information.
struct StrictValue(usize);
impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Value, D::Error> {
        if self.0 > MAX_DOCUMENT_DEPTH {
            return Err(serde::de::Error::custom("document depth exceeds 64"));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for StrictValue {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(StrictValue(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate mapping key"));
            }
            values.insert(key, map.next_value_seed(StrictValue(self.0 + 1))?);
        }
        Ok(Value::Object(values))
    }
}

/// Strip only equal known read-only echoes from full entity-show replacements.
/// Patches reject all read-only fields. Unknown fields cannot disappear in merge.
pub(crate) fn writable_input(
    input: &Value,
    current: &Value,
    writable: &[&str],
    readonly: &[&str],
    kind: MutationKind,
) -> Result<Value> {
    require_object(input)?;
    let mut result = serde_json::Map::new();
    for (key, value) in input.as_object().expect("object checked") {
        if writable.contains(&key.as_str()) {
            result.insert(key.clone(), value.clone());
        } else if readonly.contains(&key.as_str()) {
            if kind == MutationKind::Patch || current.get(key) != Some(value) {
                return Err(Error::invalid_arguments(format!("{key}: read-only field")));
            }
        } else {
            return Err(Error::invalid_arguments(format!("{key}: unknown field")));
        }
    }
    Ok(Value::Object(result))
}

pub(crate) fn reject_nulls(value: &Value, path: &str) -> Result<()> {
    match value {
        Value::Null => {
            return Err(Error::invalid_arguments(format!(
                "{path}: null is not allowed; omit the field"
            )));
        }
        Value::Object(map) => {
            for (key, value) in map {
                reject_nulls(value, &format!("{path}.{key}"))?;
            }
        }
        Value::Array(items) => {
            for value in items {
                reject_nulls(value, path)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_removes_members_and_replaces_arrays() {
        assert_eq!(
            apply_mutation(
                &json!({"a": 1, "b": [1, 2]}),
                &json!({"a": null, "b": [3]}),
                MutationKind::Patch
            )
            .unwrap(),
            json!({"b": [3]})
        );
    }

    #[test]
    fn replacement_does_not_require_a_parseable_old_definition() {
        assert_eq!(
            apply_mutation(
                &Value::Null,
                &json!({"name":"repaired"}),
                MutationKind::Update
            )
            .unwrap(),
            json!({"name":"repaired"})
        );
    }

    #[test]
    fn replacement_drops_omitted_fields() {
        assert_eq!(
            apply_mutation(&json!({"a": 1}), &json!({}), MutationKind::Update).unwrap(),
            json!({})
        );
    }

    #[tokio::test]
    async fn file_input_rejects_ambiguous_or_nonobject_documents() {
        let dir = tempfile::tempdir().unwrap();
        for (extension, content) in [
            ("json", "{\"a\":1,\"a\":2}"),
            ("yaml", "a: 1\na: 2\n"),
            ("yaml", "1: value\n"),
            ("yaml", "a: !!str value\n"),
            ("yaml", "a: .inf\n"),
            ("yaml", "---\na: 1\n---\nb: 2\n"),
            ("json", "[]"),
            ("yaml", ""),
        ] {
            let path = dir.path().join(format!("input.{extension}"));
            std::fs::write(&path, content).unwrap();
            assert!(read_document(&path, None).await.is_err(), "{content:?}");
        }
    }

    #[tokio::test]
    async fn file_input_preserves_strings_and_rejects_depth_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.yaml");
        std::fs::write(&path, "role: |\n  hello 世界\n").unwrap();
        assert_eq!(
            read_document(&path, None).await.unwrap(),
            json!({"role": "hello 世界\n"})
        );
        std::fs::write(&path, format!("a: {}1{}", "[".repeat(65), "]".repeat(65))).unwrap();
        assert!(read_document(&path, None).await.is_err());
        std::fs::write(&path, "x".repeat(1024 * 1024 + 1)).unwrap();
        assert!(read_document(&path, None).await.is_err());
    }
}
