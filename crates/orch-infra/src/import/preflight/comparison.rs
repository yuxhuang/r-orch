//! Compare bounded content without imposing Value's numeric range on ignored fields.
use serde_json::{Value, value::RawValue};
use std::collections::BTreeMap;

/// A YAML-only ignored key cannot be represented in any JSON index entry.
pub(super) struct DocumentContent(pub(super) Option<Box<RawValue>>);

impl DocumentContent {
    pub(super) fn matches(&self, index: &RawValue) -> bool {
        self.0.as_deref().is_some_and(|file| same_json(file, index))
    }
}

// Both documents passed the depth/node budgets before this comparison. All JSON
// parsing stays with serde_json. Collections compare structurally; scalar Value
// equality is unchanged, except opaque ignored numbers compare by their lexemes.
fn same_json(a: &RawValue, b: &RawValue) -> bool {
    let (a, b) = (a.get().trim(), b.get().trim());
    if a.starts_with('{') || b.starts_with('{') {
        let (Ok(a), Ok(b)) = (
            serde_json::from_str::<BTreeMap<String, &RawValue>>(a),
            serde_json::from_str::<BTreeMap<String, &RawValue>>(b),
        ) else {
            return false;
        };
        return a.len() == b.len()
            && a.iter()
                .all(|(key, value)| b.get(key).is_some_and(|other| same_json(value, other)));
    }
    if a.starts_with('[') || b.starts_with('[') {
        let (Ok(a), Ok(b)) = (
            serde_json::from_str::<Vec<&RawValue>>(a),
            serde_json::from_str::<Vec<&RawValue>>(b),
        ) else {
            return false;
        };
        return a.len() == b.len() && a.iter().zip(&b).all(|(a, b)| same_json(a, b));
    }
    match (
        serde_json::from_str::<Value>(a),
        serde_json::from_str::<Value>(b),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
