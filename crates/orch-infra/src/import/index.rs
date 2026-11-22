//! Reconcile a parsed index with independently parsed files; no names are opened.
use super::document::Refusal;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, PartialEq, Eq)]
pub enum IndexProblem {
    Unreadable,
    Differs(String),
    MissingFile(String),
    Unlisted(String),
}

pub type Reconciled = (Vec<(String, Value)>, Vec<IndexProblem>);

/// Files provide content; an index supplies order only. The caller has already
/// validated stems and document identities. Missing/unusable indexes use id order.
/// `identity` is `id` for entities and `key` for context documents.
pub fn reconcile(
    index: Option<&Value>,
    mut files: BTreeMap<String, Value>,
    identity: &str,
) -> Result<Reconciled, Refusal> {
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    if let Some(entries) = index.and_then(Value::as_array) {
        for entry in entries {
            let Some(id) = entry.get(identity).and_then(Value::as_str) else {
                return Err(Refusal::invalid(
                    "index_entry",
                    "index entry has no string identity",
                ));
            };
            if !seen.insert(id.to_owned()) {
                return Err(Refusal::invalid("index_entry", "duplicate index identity"));
            }
            match files.remove(id) {
                Some(file) => {
                    if &file != entry {
                        problems.push(IndexProblem::Differs(id.into()));
                    }
                    rows.push((id.into(), file));
                }
                None => problems.push(IndexProblem::MissingFile(id.into())),
            }
        }
    } else {
        problems.push(IndexProblem::Unreadable);
    }
    for (id, file) in files {
        if index.and_then(Value::as_array).is_some() {
            problems.push(IndexProblem::Unlisted(id.clone()));
        }
        rows.push((id, file));
    }
    Ok((rows, problems))
}
