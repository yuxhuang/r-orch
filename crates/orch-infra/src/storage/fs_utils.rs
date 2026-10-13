//! Low-level filesystem utilities — ported from
//! `src/infrastructure/storage/fs-utils.ts`.
//!
//! All file persistence goes through here. Two things in this module are the
//! cross-compat crux (PARITY.md §6.4):
//!
//! - [`write_yaml`] runs every emitted document through
//!   [`quote_ambiguous_scalars`], because an **unquoted**
//!   `created_at: 2026-10-13T16:37:58.959Z` re-parses in the TypeScript binary
//!   as a JavaScript `Date`, and `MessageStore.list()` then calls
//!   `.localeCompare()` on it and **throws**.
//! - [`read_yaml`] calls `apply_merge()`, because js-yaml resolves `<<` and
//!   `serde_yaml_ng` leaves it as a literal key (§6.4.2, V38).
//!
//! Neither is optional and neither has a caller-visible switch, so no caller
//! can forget.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use orch_core::error::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Write atomically: temp file in the same directory, then rename.
///
/// Temp name is `.{basename}.{8 hex}.tmp`, matching the TS layout so a crashed
/// write leaves a file the other binary also recognizes as debris.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_dir(dir).await?;

    let basename = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp_path = dir.join(format!(".{basename}.{}.tmp", random_hex8()));

    match tokio::fs::write(&tmp_path, content).await {
        Ok(()) => {}
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(Error::io(
                format!("write {}", tmp_path.display()),
                &tmp_path,
                err,
            ));
        }
    }
    if let Err(err) = tokio::fs::rename(&tmp_path, path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(Error::io(
            format!("rename into {}", path.display()),
            path,
            err,
        ));
    }
    Ok(())
}

/// 8 hex characters, matching the TS `randomBytes(4).toString('hex')`.
///
/// Uses `nanoid` rather than pulling in a second RNG — the only requirement is
/// that two concurrent writers to the same path pick different temp names.
fn random_hex8() -> String {
    const HEX: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];
    nanoid::nanoid!(8, &HEX)
}

/// `JSON.stringify(data, null, 2) + "\n"` — 2-space indent, **trailing
/// newline**. `serde_json::to_string_pretty` matches the indentation but omits
/// the newline (PARITY.md §6.4).
pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut content = serde_json::to_string_pretty(value)
        .map_err(|err| Error::serde(format!("serialize {}", path.display()), path, err))?;
    content.push('\n');
    atomic_write(path, &content).await
}

/// Read and parse a JSON file. `None` when the file does not exist; a parse
/// error is an error, exactly like the TS version.
pub async fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let Some(content) = read_to_string_opt(path).await? else {
        return Ok(None);
    };
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|err| Error::serde(format!("parse {}", path.display()), path, err))
}

/// Write data as YAML atomically, with the timestamp guard applied.
///
/// `js-yaml.dump(data, { indent: 2, lineWidth: 120, noRefs: true,
/// sortKeys: false })` is the reference. Emitted *bytes* are not contract
/// (§1.4) — js-yaml folds long scalars with `>-` where `serde_yaml_ng` picks
/// `|-` or a quoted style, and both round-trip to the same value (§6.4.1).
pub async fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let raw = serde_yaml_ng::to_string(value)
        .map_err(|err| Error::serde(format!("serialize {}", path.display()), path, err))?;
    atomic_write(path, &quote_ambiguous_scalars(&raw)).await
}

/// Read and parse a YAML file, resolving merge keys. `None` when absent.
pub async fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let Some(value) = read_yaml_value(path).await? else {
        return Ok(None);
    };
    serde_yaml_ng::from_value(value)
        .map(Some)
        .map_err(|err| Error::serde(format!("parse {}", path.display()), path, err))
}

/// Read a YAML file as an untyped value, resolving merge keys. `None` when
/// absent.
///
/// Config reading needs the untyped form: the TS store deep-merges the raw
/// parsed object over `DEFAULT_CONFIG`, so unknown keys survive.
pub async fn read_yaml_value(path: &Path) -> Result<Option<serde_yaml_ng::Value>> {
    let Some(content) = read_to_string_opt(path).await? else {
        return Ok(None);
    };
    let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&content)
        .map_err(|err| Error::serde(format!("parse {}", path.display()), path, err))?;
    // js-yaml's default schema resolves `<<`; serde_yaml_ng does not until
    // asked (PARITY.md §6.4.2). Unconditional: it is a no-op on every document
    // ORCH itself writes.
    value.apply_merge().map_err(|err| {
        Error::serde(format!("apply merge keys in {}", path.display()), path, err)
    })?;
    Ok(Some(value))
}

async fn read_to_string_opt(path: &Path) -> Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io(format!("read {}", path.display()), path, err)),
    }
}

/// Force single-quoting on any **plain** scalar in value position that
/// js-yaml's default schema would resolve as a YAML 1.1 timestamp or date.
///
/// Keyed off the scalar grammar, not off field names, so a user-supplied
/// `description: 2026-10-13` is protected too (ARCHITECTURE §5.3).
///
/// Block scalars are skipped entirely: their content is already unambiguous
/// and quoting inside one would corrupt the text.
///
/// **Known gap:** scalars in *key* position are not quoted. Nothing ORCH
/// writes produces a date-shaped key — every key comes from a struct field
/// name — and the only reachable source would be a hand-authored `config.yml`.
/// Recorded rather than guessed at.
pub fn quote_ambiguous_scalars(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len() + 16);
    // Indentation of the block-scalar header we are inside, if any. Content
    // belonging to it is more indented than this.
    let mut block_indent: Option<usize> = None;

    for line in yaml.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let indent = body.len() - body.trim_start().len();

        if let Some(header_indent) = block_indent {
            // Blank lines and more-indented lines are still block content.
            if body.trim().is_empty() || indent > header_indent {
                out.push_str(line);
                continue;
            }
            block_indent = None;
        }

        match rewrite_value_scalar(body) {
            ScalarRewrite::Quoted(rewritten) => {
                out.push_str(&rewritten);
                out.push_str(newline);
            }
            ScalarRewrite::BlockHeader => {
                block_indent = Some(indent);
                out.push_str(line);
            }
            ScalarRewrite::Unchanged => out.push_str(line),
        }
    }
    out
}

enum ScalarRewrite {
    Quoted(String),
    BlockHeader,
    Unchanged,
}

fn rewrite_value_scalar(body: &str) -> ScalarRewrite {
    let Some(value_start) = find_value_start(body) else {
        return ScalarRewrite::Unchanged;
    };

    let raw = &body[value_start..];
    let value = raw.trim_end_matches([' ', '\t']);
    let trailing = &raw[value.len()..];

    // Any value-position scalar beginning with `|` or `>` is a block-scalar
    // header. Remember it so its content is left alone.
    if value.starts_with(['|', '>']) {
        return ScalarRewrite::BlockHeader;
    }

    if !is_plain_scalar(value) || !is_yaml11_timestamp(value) {
        return ScalarRewrite::Unchanged;
    }

    // Single-quote it. A YAML 1.1 timestamp cannot contain `'`, so no escaping
    // is needed; assert that rather than assume it.
    debug_assert!(!value.contains('\''));
    ScalarRewrite::Quoted(format!("{}'{value}'{trailing}", &body[..value_start]))
}

/// Byte offset at which a line's **value** begins: after the `: ` of a mapping
/// entry, or after the `- ` of a bare sequence item. `None` for a line with no
/// scalar value — a key introducing a nested mapping (`key:`), a blank line,
/// or a comment.
fn find_value_start(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let indent = body.len() - body.trim_start().len();
    if indent == body.len() {
        return None;
    }

    let mut i = indent;
    let mut sequence_value_start = None;
    if bytes[i] == b'-' {
        match bytes.get(i + 1) {
            Some(b' ') => {
                i += 2;
                sequence_value_start = Some(i);
            }
            // A bare `-` (empty sequence item) or `--` has no inline value.
            None => return None,
            _ => {}
        }
    }

    // The first `: ` outside a quoted key separates key from value.
    let mut quote: Option<u8> = None;
    let mut j = i;
    while j < bytes.len() {
        let c = bytes[j];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'\'' || c == b'"' => quote = Some(c),
            None if c == b':' && bytes.get(j + 1) == Some(&b' ') => return Some(j + 2),
            None => {}
        }
        j += 1;
    }

    // No `: ` — a bare sequence item is still a value; `key:` is not.
    sequence_value_start
}

/// Whether a scalar was emitted in plain style — i.e. unquoted and not a
/// flow/block/anchor/tag construct, so its type is resolved by the schema.
fn is_plain_scalar(value: &str) -> bool {
    !value.is_empty() && !value.starts_with(['\'', '"', '[', '{', '|', '>', '&', '*', '!', '#'])
}

/// js-yaml's `DEFAULT_SCHEMA` timestamp and date grammars, which resolve a
/// plain scalar to a JavaScript `Date`:
///
/// ```text
/// date:      ^\d{4}-\d{2}-\d{2}$
/// timestamp: ^\d{4}-\d{1,2}-\d{1,2}(?:[Tt]|[ \t]+)\d{1,2}:\d{2}:\d{2}(?:\.\d*)?
///            (?:[ \t]*(?:Z|[-+]\d{1,2}(?::\d{2})?))?$
/// ```
pub fn is_yaml11_timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);

    // Year: exactly 4 digits, then '-'.
    if !(digit(0) && digit(1) && digit(2) && digit(3)) || b.get(4) != Some(&b'-') {
        return false;
    }
    let mut i = 5;

    // Month and day: 1-2 digits each, '-' separated.
    let take_1_or_2 = |i: &mut usize| -> bool {
        if !digit(*i) {
            return false;
        }
        *i += 1;
        if digit(*i) {
            *i += 1;
        }
        true
    };
    if !take_1_or_2(&mut i) || b.get(i) != Some(&b'-') {
        return false;
    }
    i += 1;
    if !take_1_or_2(&mut i) {
        return false;
    }

    // The date-only form must end here, and it requires 2-digit month and day.
    if i == b.len() {
        return b.len() == 10;
    }

    // Separator: `T`, `t`, or one-or-more space/tab.
    match b[i] {
        b'T' | b't' => i += 1,
        b' ' | b'\t' => {
            while matches!(b.get(i), Some(b' ' | b'\t')) {
                i += 1;
            }
        }
        _ => return false,
    }

    // Hour (1-2 digits) : minute (2) : second (2).
    if !take_1_or_2(&mut i) || b.get(i) != Some(&b':') {
        return false;
    }
    i += 1;
    if !(digit(i) && digit(i + 1)) {
        return false;
    }
    i += 2;
    if b.get(i) != Some(&b':') {
        return false;
    }
    i += 1;
    if !(digit(i) && digit(i + 1)) {
        return false;
    }
    i += 2;

    // Optional fraction: `.` then zero or more digits.
    if b.get(i) == Some(&b'.') {
        i += 1;
        while digit(i) {
            i += 1;
        }
    }

    // Optional timezone, optionally preceded by whitespace.
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    if i == b.len() {
        return true;
    }
    match b[i] {
        b'Z' => i += 1,
        b'-' | b'+' => {
            i += 1;
            if !take_1_or_2(&mut i) {
                return false;
            }
            if b.get(i) == Some(&b':') {
                i += 1;
                if !(digit(i) && digit(i + 1)) {
                    return false;
                }
                i += 2;
            }
        }
        _ => return false,
    }
    i == b.len()
}

/// Directories already created in this process.
///
/// The TS version keeps the same cache to skip ~50 redundant `mkdir` syscalls
/// per tick. It means a directory deleted underneath a long-running process is
/// not recreated — faithful, and the reason `clear_ensured_dirs` exists.
fn ensured_dirs() -> &'static Mutex<HashSet<PathBuf>> {
    static ENSURED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ENSURED.get_or_init(|| Mutex::new(HashSet::new()))
}

pub async fn ensure_dir(dir: &Path) -> Result<()> {
    {
        let cache = ensured_dirs().lock().unwrap_or_else(|e| e.into_inner());
        if cache.contains(dir) {
            return Ok(());
        }
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|err| Error::io(format!("create {}", dir.display()), dir, err))?;
    ensured_dirs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir.to_path_buf());
    Ok(())
}

/// Clear the [`ensure_dir`] cache. Tests only.
pub fn clear_ensured_dirs() {
    ensured_dirs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

pub async fn path_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

/// Directory entry names, optionally filtered by extension. An absent
/// directory lists as empty, like the TS version.
pub async fn list_files(dir: &Path, ext: Option<&str>) -> Result<Vec<String>> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(format!("read dir {}", dir.display()), dir, err)),
    };

    let mut names = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| Error::io(format!("read dir {}", dir.display()), dir, err))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ext.is_none_or(|ext| name.ends_with(ext)) {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Record {
        id: String,
        created_at: String,
        description: String,
    }

    fn record() -> Record {
        Record {
            id: "tsk_1".into(),
            created_at: "2026-10-13T16:37:58.959Z".into(),
            description: String::new(),
        }
    }

    // ── atomic_write / json ────────────────────────────────────────────────

    #[tokio::test]
    async fn atomic_write_creates_parents_and_leaves_no_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("a/b/c.txt");
        atomic_write(&path, "hello").await.unwrap();

        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "hello");
        let siblings = list_files(path.parent().unwrap(), None).await.unwrap();
        assert_eq!(siblings, vec!["c.txt".to_owned()]);
    }

    #[tokio::test]
    async fn write_json_matches_json_stringify_with_two_space_indent_and_a_newline() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("r.json");
        write_json(&path, &record()).await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            content,
            "{\n  \"id\": \"tsk_1\",\n  \"created_at\": \"2026-10-13T16:37:58.959Z\",\n  \"description\": \"\"\n}\n"
        );
        assert_eq!(read_json::<Record>(&path).await.unwrap(), Some(record()));
    }

    #[tokio::test]
    async fn reading_a_missing_file_is_none_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nope.json");
        assert_eq!(read_json::<Record>(&missing).await.unwrap(), None);
        assert_eq!(
            read_yaml::<Record>(&temp.path().join("nope.yml"))
                .await
                .unwrap(),
            None
        );
        assert!(!path_exists(&missing).await);
    }

    #[tokio::test]
    async fn a_corrupt_file_is_an_error_carrying_the_path() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("bad.json");
        atomic_write(&path, "{not json").await.unwrap();

        let err = read_json::<Record>(&path).await.expect_err("corrupt");
        assert_eq!(err.path(), Some(path.as_path()));
        assert_eq!(err.exit_code(), 1);
    }

    // ── The timestamp guard ────────────────────────────────────────────────

    #[tokio::test]
    async fn write_yaml_quotes_timestamps_so_the_ts_binary_reads_them_as_strings() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("r.yml");
        write_yaml(&path, &record()).await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            content.contains("created_at: '2026-10-13T16:37:58.959Z'"),
            "not quoted:\n{content}"
        );
        // And it still round-trips on our side.
        assert_eq!(read_yaml::<Record>(&path).await.unwrap(), Some(record()));
    }

    /// Every expectation below was produced by
    /// `node -e "yaml.load('k: ' + v).k instanceof Date"` against the
    /// vendored js-yaml, not by reading its regex.
    const JS_YAML_RESOLVES_AS_DATE: &[(&str, bool)] = &[
        ("2026-10-13T16:37:58.959Z", true),
        ("2026-10-13T16:37:58Z", true),
        ("2026-10-13T16:37:58", true),
        ("2026-1-3T4:05:06Z", true),
        ("2026-10-13 16:37:58", true),
        ("2026-10-13\t16:37:58", true),
        ("2026-10-13T16:37:58.959+02:00", true),
        ("2026-10-13T16:37:58.959 Z", true),
        ("2026-10-13", true),
        // A timezone without the colon is *not* a js-yaml timestamp: its
        // grammar is `([-+])(\d{1,2})(?::(\d{2}))?$`, so `-0200` cannot match.
        ("2026-10-13T16:37:58.959-0200", false),
        // The date-only form requires two-digit month and day.
        ("2026-1-3", false),
        ("2026-10-13T", false),
        ("2026-10-13T16:37", false),
        ("2026-10", false),
        ("x2026-10-13", false),
        ("2026-10-13x", false),
        ("2026-10-13T16:37:58.959Q", false),
        ("20261-10-13", false),
        ("todo", false),
        ("123", false),
        ("true", false),
        ("", false),
    ];

    #[test]
    fn the_grammar_agrees_with_js_yaml_on_every_measured_case() {
        for (value, is_date) in JS_YAML_RESOLVES_AS_DATE {
            assert_eq!(
                is_yaml11_timestamp(value),
                *is_date,
                "{value:?}: js-yaml resolves it as {}",
                if *is_date { "a Date" } else { "a string" }
            );
        }
    }

    #[test]
    fn the_guard_quotes_exactly_the_scalars_js_yaml_would_misread() {
        for (value, is_date) in JS_YAML_RESOLVES_AS_DATE {
            if value.is_empty() {
                continue;
            }
            let emitted = format!("created_at: {value}\n");
            let expected = if *is_date {
                format!("created_at: '{value}'\n")
            } else {
                emitted.clone()
            };
            assert_eq!(quote_ambiguous_scalars(&emitted), expected, "{value:?}");
        }
    }

    #[test]
    fn the_guard_leaves_everything_else_alone() {
        // Already-quoted scalars are not double-quoted.
        assert_eq!(
            quote_ambiguous_scalars("created_at: '2026-10-13'\n"),
            "created_at: '2026-10-13'\n"
        );
        assert_eq!(
            quote_ambiguous_scalars("created_at: \"2026-10-13\"\n"),
            "created_at: \"2026-10-13\"\n"
        );
    }

    #[test]
    fn the_guard_quotes_sequence_items_and_nested_mappings() {
        assert_eq!(
            quote_ambiguous_scalars("dates:\n- 2026-10-13\n- 2026-10-14\n"),
            "dates:\n- '2026-10-13'\n- '2026-10-14'\n"
        );
        assert_eq!(
            quote_ambiguous_scalars("outer:\n  inner:\n    created_at: 2026-10-13\n"),
            "outer:\n  inner:\n    created_at: '2026-10-13'\n"
        );
        assert_eq!(
            quote_ambiguous_scalars("members:\n- agent_id: a\n  joined_at: 2026-10-13\n"),
            "members:\n- agent_id: a\n  joined_at: '2026-10-13'\n"
        );
    }

    #[test]
    fn the_guard_never_touches_block_scalar_content() {
        // A role prompt that happens to contain a date-shaped line must come
        // out byte-identical — it is injected into prompts verbatim.
        let yaml = "role: |-\n  Released on\n  2026-10-13\n  and later\nid: x\n";
        assert_eq!(quote_ambiguous_scalars(yaml), yaml);

        // Including the folded and keep-newline variants, and the indentation
        // indicator serde_yaml_ng emits for leading-space content.
        for header in ["|", "|+", ">", ">-", ">+", "|2+"] {
            let yaml = format!("role: {header}\n  2026-10-13\nid: x\n");
            assert_eq!(quote_ambiguous_scalars(&yaml), yaml, "{header}");
        }
    }

    #[test]
    fn the_guard_survives_a_blank_line_inside_a_block_scalar() {
        let yaml = "role: |-\n  first\n\n  2026-10-13\nid: x\n";
        assert_eq!(quote_ambiguous_scalars(yaml), yaml);
    }

    #[tokio::test]
    async fn a_multiline_role_round_trips_byte_exactly_through_the_guard() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("agent.yml");
        let role = "Backend engineer\n\n## WORKFLOW\n\n1) Shipped 2026-10-13\n- keep going";
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct A {
            role: String,
            created_at: String,
        }
        let agent = A {
            role: role.into(),
            created_at: "2026-10-13T16:37:58.959Z".into(),
        };
        write_yaml(&path, &agent).await.unwrap();
        assert_eq!(read_yaml::<A>(&path).await.unwrap().unwrap().role, role);
    }

    // ── Merge keys ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_yaml_resolves_merge_keys() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("config.yml");
        atomic_write(
            &path,
            "base: &b\n  max_turns: 99\n  adapter: codex\nscheduling:\n  <<: *b\n  poll_interval_ms: 5\n",
        )
        .await
        .unwrap();

        let value = read_yaml_value(&path).await.unwrap().expect("present");
        let scheduling = &value["scheduling"];
        assert!(scheduling.get("<<").is_none(), "literal merge key survived");
        assert_eq!(scheduling["max_turns"].as_u64(), Some(99));
        assert_eq!(scheduling["adapter"].as_str(), Some("codex"));
        assert_eq!(scheduling["poll_interval_ms"].as_u64(), Some(5));
    }

    // ── ensure_dir / list_files ────────────────────────────────────────────

    #[tokio::test]
    async fn ensure_dir_is_idempotent_and_cached() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let dir = temp.path().join("x/y");
        ensure_dir(&dir).await.unwrap();
        ensure_dir(&dir).await.unwrap();
        assert!(path_exists(&dir).await);
    }

    #[tokio::test]
    async fn list_files_filters_by_extension_and_tolerates_a_missing_dir() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        for name in ["a.yml", "b.yml", "_index.json"] {
            atomic_write(&temp.path().join(name), "x").await.unwrap();
        }
        let mut yml = list_files(temp.path(), Some(".yml")).await.unwrap();
        yml.sort();
        assert_eq!(yml, vec!["a.yml".to_owned(), "b.yml".to_owned()]);
        assert_eq!(list_files(temp.path(), None).await.unwrap().len(), 3);
        assert!(
            list_files(&temp.path().join("nope"), None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn temp_names_differ_between_calls() {
        let a = random_hex8();
        let b = random_hex8();
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
