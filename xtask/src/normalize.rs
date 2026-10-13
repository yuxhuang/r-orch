//! The comparison normalizers of PARITY.md §1.4.
//!
//! Everything the two binaries legitimately disagree about — random ids, wall
//! clock, temp paths, PIDs, measured durations, ANSI styling — is replaced
//! with a placeholder before comparison. Everything else is contract.
//!
//! Ids are mapped **consistently per run**: the first `tsk_` seen becomes
//! `<tsk_1>`, the second `<tsk_2>`, and so on, in the order the ids first
//! appear in the scenario's own output. That is what makes the two trees have
//! the same *file names* after normalization, which is what lets paths be
//! compared at all.

use std::path::Path;

use indexmap::IndexMap;

/// Entity id prefixes (PARITY.md §6.2).
const PREFIXES: [&str; 6] = ["tsk", "agt", "run", "goal", "team", "msg"];

/// `init` writes this id as a fixed string, so it is **not** normalized —
/// masking it would hide a divergence rather than tolerate one
/// (PARITY.md §6.2).
const FIXED_IDS: [&str; 1] = ["agt_creator"];

/// Builds and applies a per-run id mapping.
#[derive(Debug, Default)]
pub struct Normalizer {
    /// Real id → placeholder, in order of first sighting.
    ids: IndexMap<String, String>,
    counters: IndexMap<String, usize>,
    /// Absolute paths replaced with `<ROOT>`, longest first so a nested root
    /// does not shadow its parent.
    roots: Vec<(String, String)>,
}

impl Normalizer {
    pub fn new() -> Self {
        Normalizer::default()
    }

    /// Register a directory whose absolute path becomes `<ROOT>`.
    pub fn with_root(mut self, root: &Path, placeholder: &str) -> Self {
        self.roots
            .push((root.to_string_lossy().into_owned(), placeholder.to_owned()));
        // Also register the symlink-resolved form: macOS hands out
        // /var/folders paths that resolve to /private/var/folders, and a child
        // process reports the resolved one.
        if let Ok(resolved) = root.canonicalize() {
            let resolved = resolved.to_string_lossy().into_owned();
            if !self.roots.iter().any(|(r, _)| *r == resolved) {
                self.roots.push((resolved, placeholder.to_owned()));
            }
        }
        self.roots
            .sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        self
    }

    /// Learn the ids in `text`, in order, without rewriting anything. Call
    /// this over the scenario's output in step order **before** normalizing
    /// the file tree, so the mapping follows creation order.
    pub fn learn(&mut self, text: &str) {
        for (id, prefix) in find_ids(text) {
            self.intern(&id, &prefix);
        }
    }

    fn intern(&mut self, id: &str, prefix: &str) -> String {
        if let Some(mapped) = self.ids.get(id) {
            return mapped.clone();
        }
        let counter = self.counters.entry(prefix.to_owned()).or_insert(0);
        *counter += 1;
        let placeholder = format!("<{prefix}_{counter}>");
        self.ids.insert(id.to_owned(), placeholder.clone());
        placeholder
    }

    /// Apply every §1.4 rule.
    pub fn text(&mut self, text: &str) -> String {
        let text = strip_ansi(text);
        let text = self.replace_roots(&text);
        let text = self.replace_ids(&text);
        let text = replace_timestamps(&text);
        replace_pids_and_durations(&text)
    }

    /// Normalize a path: ids only, since a path carries no timestamps.
    pub fn path(&mut self, path: &str) -> String {
        self.replace_ids(path)
    }

    /// Normalize a **parsed** artifact.
    ///
    /// Numeric fields have to be handled here rather than in [`Self::text`]:
    /// replacing a bare `"pid": 360135` with `"pid": <PID>` in the raw text
    /// produces invalid JSON, so the file could no longer be parsed for
    /// comparison. Strings are normalized in place; `pid` and
    /// `total_runtime_ms` numbers become placeholder strings.
    pub fn value(&mut self, value: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::String(text) => Value::String(self.text(text)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, child)| {
                        let normalized = match (key.as_str(), child) {
                            ("pid", Value::Number(_)) => Value::String("<PID>".to_owned()),
                            ("total_runtime_ms", Value::Number(_)) => {
                                Value::String("<MS>".to_owned())
                            }
                            _ => self.value(child),
                        };
                        (self.text(key), normalized)
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn replace_roots(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (root, placeholder) in &self.roots {
            out = out.replace(root.as_str(), placeholder);
        }
        out
    }

    fn replace_ids(&mut self, text: &str) -> String {
        let found = find_ids(text);
        if found.is_empty() {
            return text.to_owned();
        }
        // Intern first so an id seen only in a file still gets a stable
        // placeholder, then rewrite longest-id-first (they are all the same
        // length, so plain replacement is unambiguous).
        let mut out = text.to_owned();
        for (id, prefix) in found {
            let placeholder = self.intern(&id, &prefix);
            out = out.replace(&id, &placeholder);
        }
        out
    }

    /// The learned mapping, for diagnostics.
    pub fn mapping(&self) -> &IndexMap<String, String> {
        &self.ids
    }
}

/// Every `<prefix>_<7 chars>` id in `text`, in order of appearance, excluding
/// the known fixed ids.
fn find_ids(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();

    for prefix in PREFIXES {
        let needle = format!("{prefix}_");
        let mut from = 0;
        while let Some(rel) = text[from..].find(&needle) {
            let start = from + rel;
            from = start + needle.len();

            // Must not be preceded by an id character, or `xagt_abcdefg`
            // would match.
            if start > 0 && is_id_char(bytes[start - 1]) {
                continue;
            }
            let suffix_start = start + needle.len();
            let suffix_end = suffix_start + 7;
            if suffix_end > bytes.len() || !text.is_char_boundary(suffix_end) {
                continue;
            }
            if !bytes[suffix_start..suffix_end]
                .iter()
                .all(|b| is_id_char(*b))
            {
                continue;
            }
            // Exactly 7 characters, so the next one must not be an id char.
            if bytes.get(suffix_end).is_some_and(|b| is_id_char(*b)) {
                continue;
            }
            let id = &text[start..suffix_end];
            if FIXED_IDS.contains(&id) {
                continue;
            }
            found.push((id.to_owned(), prefix.to_owned()));
        }
    }

    // One pass per prefix finds them out of order; restore document order.
    found.sort_by_key(|(id, _)| text.find(id.as_str()).unwrap_or(usize::MAX));
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

fn is_id_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// ISO-8601 instants → `<TS>`. Matches the `YYYY-MM-DDT…` shape with or
/// without fractional seconds, and the `Z`/offset forms.
fn replace_timestamps(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < bytes.len() {
        if let Some(end) = timestamp_end(bytes, i) {
            out.push_str("<TS>");
            i = end;
            continue;
        }
        let ch = text[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// End offset of an ISO-8601 instant starting at `i`, if there is one.
fn timestamp_end(b: &[u8], i: usize) -> Option<usize> {
    let digit = |at: usize| b.get(at).is_some_and(u8::is_ascii_digit);
    // YYYY-MM-DDT
    if !(digit(i) && digit(i + 1) && digit(i + 2) && digit(i + 3)) {
        return None;
    }
    if b.get(i + 4) != Some(&b'-') || !(digit(i + 5) && digit(i + 6)) {
        return None;
    }
    if b.get(i + 7) != Some(&b'-') || !(digit(i + 8) && digit(i + 9)) {
        return None;
    }
    if b.get(i + 10) != Some(&b'T') {
        return None;
    }
    // hh:mm:ss
    let mut j = i + 11;
    for offset in [0usize, 3, 6] {
        if !(digit(j + offset) && digit(j + offset + 1)) {
            return None;
        }
    }
    if b.get(j + 2) != Some(&b':') || b.get(j + 5) != Some(&b':') {
        return None;
    }
    j += 8;
    // Optional fraction.
    if b.get(j) == Some(&b'.') {
        j += 1;
        while digit(j) {
            j += 1;
        }
    }
    // Optional zone.
    match b.get(j) {
        Some(b'Z') => j += 1,
        Some(b'+') | Some(b'-') => {
            j += 1;
            while digit(j) || b.get(j) == Some(&b':') {
                j += 1;
            }
        }
        _ => {}
    }
    Some(j)
}

/// `"pid": <n>` → `<PID>`, and the measured-duration fields of §1.4.
fn replace_pids_and_durations(text: &str) -> String {
    let mut out = text.to_owned();
    for key in ["pid", "total_runtime_ms"] {
        out = replace_numeric_field(&out, key, if key == "pid" { "<PID>" } else { "<MS>" });
    }
    out
}

/// Replace the number following `"<key>": ` or `<key>: ` with `placeholder`.
fn replace_numeric_field(text: &str, key: &str, placeholder: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    loop {
        let json_key = format!("\"{key}\": ");
        let yaml_key = format!("{key}: ");
        let json_at = rest.find(&json_key).map(|at| (at, json_key.len()));
        let yaml_at = rest.find(&yaml_key).map(|at| (at, yaml_key.len()));

        let next = match (json_at, yaml_at) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some((at, key_len)) = next else {
            out.push_str(rest);
            return out;
        };

        let value_start = at + key_len;
        let digits = rest[value_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        out.push_str(&rest[..value_start]);
        if digits == 0 {
            // Not a number — leave it alone.
            rest = &rest[value_start..];
            continue;
        }
        out.push_str(placeholder);
        rest = &rest[value_start + digits..];
    }
}

/// SGR sequences only, matching the TS `stripAnsi`.
pub fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'm') {
                i = j + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_numbered_in_order_of_first_appearance_per_prefix() {
        let mut n = Normalizer::new();
        let text = "created tsk_AAAAAAA then agt_BBBBBBB then tsk_CCCCCCC then tsk_AAAAAAA";
        assert_eq!(
            n.text(text),
            "created <tsk_1> then <agt_1> then <tsk_2> then <tsk_1>"
        );
    }

    #[test]
    fn the_mapping_is_shared_between_output_and_paths() {
        let mut n = Normalizer::new();
        n.learn("task tsk_ZZZZZZZ created");
        // The same id in a path gets the same placeholder, which is what makes
        // the two trees comparable by file name.
        assert_eq!(n.path("tasks/tsk_ZZZZZZZ.yml"), "tasks/<tsk_1>.yml");
    }

    #[test]
    fn learning_order_determines_the_numbering() {
        let mut a = Normalizer::new();
        a.learn("tsk_AAAAAAA tsk_BBBBBBB");
        assert_eq!(a.path("tasks/tsk_BBBBBBB.yml"), "tasks/<tsk_2>.yml");

        let mut b = Normalizer::new();
        b.learn("tsk_BBBBBBB tsk_AAAAAAA");
        assert_eq!(b.path("tasks/tsk_BBBBBBB.yml"), "tasks/<tsk_1>.yml");
    }

    #[test]
    fn the_fixed_bootstrap_agent_id_is_left_alone() {
        let mut n = Normalizer::new();
        assert_eq!(n.text("agent agt_creator ready"), "agent agt_creator ready");
        assert_eq!(n.path("agents/agt_creator.yml"), "agents/agt_creator.yml");
    }

    #[test]
    fn an_id_like_substring_is_not_matched() {
        let mut n = Normalizer::new();
        // Preceded by an id character.
        assert_eq!(n.text("xtsk_AAAAAAA"), "xtsk_AAAAAAA");
        // Eight suffix characters is not an id.
        assert_eq!(n.text("tsk_AAAAAAAA"), "tsk_AAAAAAAA");
        // Six is not either.
        assert_eq!(n.text("tsk_AAAAAA"), "tsk_AAAAAA");
    }

    #[test]
    fn ids_containing_the_url_safe_punctuation_are_matched() {
        let mut n = Normalizer::new();
        assert_eq!(n.text("agt_veWG_kK and tsk_O5WBXtX"), "<agt_1> and <tsk_1>");
        assert_eq!(n.text("msg_zM-eUPn"), "<msg_1>");
    }

    #[test]
    fn timestamps_collapse_in_every_emitted_form() {
        let mut n = Normalizer::new();
        assert_eq!(n.text("2026-10-13T16:37:33.426Z"), "<TS>");
        assert_eq!(n.text("'2026-10-13T16:37:33.426Z'"), "'<TS>'");
        assert_eq!(n.text("2025-01-01T00:00:00Z"), "<TS>");
        assert_eq!(n.text("2026-10-13T16:37:33.426+02:00"), "<TS>");
        assert_eq!(n.text("2026-10-13T16:37:33"), "<TS>");
        // A date alone is not an instant and stays.
        assert_eq!(n.text("2026-10-13"), "2026-10-13");
        assert_eq!(
            n.text("created_at: <TS> and 2026-10-13"),
            "created_at: <TS> and 2026-10-13"
        );
    }

    #[test]
    fn absolute_temp_roots_collapse() {
        let temp = tempfile::tempdir().unwrap();
        let mut n = Normalizer::new().with_root(temp.path(), "<ROOT>");
        let text = format!("workspace_path: {}/sub", temp.path().display());
        assert_eq!(n.text(&text), "workspace_path: <ROOT>/sub");
    }

    #[test]
    fn pids_and_measured_durations_collapse() {
        let mut n = Normalizer::new();
        assert_eq!(n.text("\"pid\": 360135,"), "\"pid\": <PID>,");
        assert_eq!(n.text("pid: 360135\n"), "pid: <PID>\n");
        assert_eq!(
            n.text("\"total_runtime_ms\": 15"),
            "\"total_runtime_ms\": <MS>"
        );
        // A non-numeric value is left alone.
        assert_eq!(n.text("\"pid\": null"), "\"pid\": null");
    }

    #[test]
    fn ansi_is_stripped_before_anything_else() {
        let mut n = Normalizer::new();
        assert_eq!(n.text("\x1b[38;5;72m✓\x1b[39m ok"), "✓ ok");
    }

    #[test]
    fn trailing_whitespace_on_table_rows_is_preserved() {
        // §1.4: significant, do not strip — the TS table writer pads every
        // cell including the last.
        let mut n = Normalizer::new();
        assert_eq!(
            n.text("  STATUS  PRI  \n  ○ todo  P3   \n"),
            "  STATUS  PRI  \n  ○ todo  P3   \n"
        );
    }

    #[test]
    fn normalization_is_idempotent() {
        let mut n = Normalizer::new();
        let once = n.text("tsk_AAAAAAA at 2026-10-13T16:37:33.426Z");
        let twice = n.text(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn value_normalization_keeps_json_parseable() {
        use serde_json::json;
        let mut n = Normalizer::new();
        let value = json!({
            "id": "run_AAAAAAA",
            "pid": 360135,
            "started_at": "2026-10-13T16:37:33.426Z",
            "stats": { "total_runtime_ms": 15, "total_runs": 2 },
        });
        assert_eq!(
            n.value(&value),
            json!({
                "id": "<run_1>",
                "pid": "<PID>",
                "started_at": "<TS>",
                "stats": { "total_runtime_ms": "<MS>", "total_runs": 2 },
            })
        );
    }

    #[test]
    fn value_normalization_reaches_into_arrays_and_object_keys() {
        use serde_json::json;
        let mut n = Normalizer::new();
        let value =
            json!({ "running": { "tsk_AAAAAAA": { "pid": 7 } }, "claimed": ["tsk_AAAAAAA"] });
        assert_eq!(
            n.value(&value),
            json!({ "running": { "<tsk_1>": { "pid": "<PID>" } }, "claimed": ["<tsk_1>"] })
        );
    }

    #[test]
    fn a_non_numeric_pid_is_left_as_is() {
        use serde_json::json;
        let mut n = Normalizer::new();
        assert_eq!(n.value(&json!({ "pid": null })), json!({ "pid": null }));
    }

    #[test]
    fn the_mapping_is_exposed_for_diagnostics() {
        let mut n = Normalizer::new();
        n.learn("tsk_AAAAAAA");
        assert_eq!(
            n.mapping().get("tsk_AAAAAAA").map(String::as_str),
            Some("<tsk_1>")
        );
    }
}
