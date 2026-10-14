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

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use orch_core::error::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

/// Owner-only, for files that carry credentials or agent instructions.
///
/// `config.yml` holds adapter commands and API-key environment names, and
/// `agents/*.yml` hold the role prompts an agent is run with. Both are worth
/// keeping out of a group- or world-readable project directory on a shared
/// machine. Everything else keeps the process umask: changing the mode of
/// `state.json` or the indexes would be churn without a secret to protect.
#[cfg(unix)]
pub const OWNER_ONLY: u32 = 0o600;

/// [`OWNER_ONLY`] where the platform has modes, `None` where it does not, so a
/// caller can ask for owner-only without repeating the `cfg` itself.
pub fn owner_only() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(OWNER_ONLY)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Write atomically: temp file in the same directory, then rename.
///
/// Temp name is `.{basename}.{8 hex}.tmp`, matching the TS layout so a crashed
/// write leaves a file the other binary also recognizes as debris.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()> {
    atomic_write_with_mode(path, content, None).await
}

/// `atomic_write`, with an explicit unix mode for the created file.
///
/// The mode is applied to the **temp file as it is created**, not to the final
/// path afterwards. Doing it after the rename would publish the file at the
/// umask's mode first and tighten it a moment later, which is a window another
/// user on the machine can read through. `mode` is ignored on non-unix, where
/// there is no equivalent and the directory ACL is the control.
pub async fn atomic_write_with_mode(path: &Path, content: &str, mode: Option<u32>) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_dir(dir).await?;

    let basename = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp_path = dir.join(format!(".{basename}.{}.tmp", random_hex8()));

    // `create_new` so the open fails rather than following a symlink or
    // truncating something that is already there. The name has 8 random hex
    // characters, so a collision means another writer is mid-flight on the
    // same path and failing is the right answer (sre C5, security).
    let write_result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // `tokio::fs::OpenOptions::mode` on unix; the mode lands on the
        // temp file at creation, so the rename publishes it already tight.
        #[cfg(unix)]
        if let Some(mode) = mode {
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&tmp_path).await?;
        file.write_all(content.as_bytes()).await?;
        // Without this the rename can be durable while the *contents* are
        // not: a crash between the two leaves a correctly named file full of
        // zeroes. `atomic_write` is what every store writes through, so this
        // is the one place it has to be right.
        file.sync_all().await?;
        Ok::<(), std::io::Error>(())
    }
    .await;

    if let Err(err) = write_result {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(Error::io(
            format!("write {}", tmp_path.display()),
            &tmp_path,
            err,
        ));
    }

    if let Err(err) = tokio::fs::rename(&tmp_path, path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(Error::io(
            format!("rename into {}", path.display()),
            path,
            err,
        ));
    }

    // And fsync the directory, or the rename itself can be lost: the entry is
    // only durable once the directory's own metadata is. Best-effort — a
    // filesystem that refuses to open a directory for sync is not a reason to
    // fail a write that already succeeded.
    #[cfg(unix)]
    if let Ok(dir_handle) = tokio::fs::File::open(dir).await {
        let _ = dir_handle.sync_all().await;
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
    serde_json::from_str(&repair_lone_surrogates(&content))
        .map(Some)
        .map_err(|err| Error::serde(format!("parse {}", path.display()), path, err))
}
/// Replace unpaired surrogate escapes with `U+FFFD`, so a TS-written file
/// stays readable (PARITY.md §14, docs/TRAITS-FROZEN.md §11).
///
/// `JSON.stringify` will happily emit a lone surrogate as the six-character
/// escape `\ud83d`, and `serde_json` rejects that outright — "unexpected end
/// of hex escape", whatever follows it. One `msg send` with a 4001-code-unit
/// body ending in a non-BMP character is enough to produce such a file,
/// because JS truncates by UTF-16 code unit and cuts between the surrogates.
/// Without this repair that single message makes `msg list` fail **entirely**
/// rather than show one slightly mangled body.
///
/// Per the ruling: decode to U+FFFD, never fail the listing, and leave the
/// file alone. This is read-side only, so the bytes on disk stay as the oracle
/// wrote them until something re-saves the record for its own reasons — at
/// which point the replacement character is what gets written, which is
/// accepted.
///
/// A valid surrogate *pair* is passed through untouched, so this cannot change
/// the value of any well-formed file, and the return is `Borrowed` whenever
/// there is nothing to repair — which is every file r-orch writes itself.
fn repair_lone_surrogates(content: &str) -> Cow<'_, str> {
    // Cheap reject: no `\u` escape at all means nothing to do.
    if !content.contains("\\u") {
        return Cow::Borrowed(content);
    }

    let bytes = content.as_bytes();
    let mut out: Option<String> = None;
    // Everything before this index is already in `out`.
    let mut copied_to = 0usize;
    let mut at = 0usize;

    while at < bytes.len() {
        match escape_at(bytes, at) {
            // A high surrogate followed by a low one is a real character.
            Some((high, next))
                if (0xD800..=0xDBFF).contains(&high)
                    && escape_at(bytes, next)
                        .is_some_and(|(low, _)| (0xDC00..=0xDFFF).contains(&low)) =>
            {
                at = next + 6;
            }
            // Any other escape in the surrogate range is unpaired.
            Some((lone, next)) if (0xD800..=0xDFFF).contains(&lone) => {
                let buffer = out.get_or_insert_with(String::new);
                buffer.push_str(&content[copied_to..at]);
                buffer.push_str("\\ufffd");
                copied_to = next;
                at = next;
            }
            Some((_, next)) => at = next,
            // Only ASCII `\` starts an escape, so a byte step stays on a
            // char boundary for anything we slice.
            None => at += 1,
        }
    }

    match out {
        Some(mut buffer) => {
            buffer.push_str(&content[copied_to..]);
            Cow::Owned(buffer)
        }
        None => Cow::Borrowed(content),
    }
}

/// The code unit of a `\uXXXX` escape at `at`, and the index just past it —
/// but only when the backslash is itself unescaped.
fn escape_at(bytes: &[u8], at: usize) -> Option<(u32, usize)> {
    if bytes.get(at) != Some(&b'\\') || bytes.get(at + 1) != Some(&b'u') {
        return None;
    }
    // `\\uD83D` is a literal backslash followed by the text `uD83D`, not an
    // escape, so the run of backslashes ending here must be of even length.
    let preceding = bytes[..at]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    if preceding % 2 != 0 {
        return None;
    }
    let hex = bytes.get(at + 2..at + 6)?;
    let mut code = 0u32;
    for digit in hex {
        code = code * 16 + (*digit as char).to_digit(16)?;
    }
    Some((code, at + 6))
}

/// Write data as YAML atomically, with the timestamp guard applied.
///
/// `js-yaml.dump(data, { indent: 2, lineWidth: 120, noRefs: true,
/// sortKeys: false })` is the reference. Emitted *bytes* are not contract
/// (§1.4) — js-yaml folds long scalars with `>-` where `serde_yaml_ng` picks
/// `|-` or a quoted style, and both round-trip to the same value (§6.4.1).
pub async fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_yaml_with_mode(path, value, None).await
}

/// `write_yaml`, with an explicit unix mode — see [`atomic_write_with_mode`].
pub async fn write_yaml_with_mode<T: Serialize>(
    path: &Path,
    value: &T,
    mode: Option<u32>,
) -> Result<()> {
    let raw = serde_yaml_ng::to_string(value)
        .map_err(|err| Error::serde(format!("serialize {}", path.display()), path, err))?;
    atomic_write_with_mode(path, &quote_ambiguous_scalars(&raw), mode).await
}

/// Largest YAML document `read_yaml_value` will hand to the parser.
///
/// Every file ORCH writes is orders of magnitude smaller; the cap exists so a
/// hand-placed file cannot make the parser the expensive part.
const MAX_YAML_READ_SIZE: usize = 1024 * 1024;

/// Ceiling on a document's *expanded* size, estimated as
/// `length × (1 + aliases)`.
///
/// This is the check that actually stops flat amplification, and it stops it
/// in microseconds rather than at the deadline. The estimate is deliberately
/// crude — it assumes the worst case, that every alias refers to the whole
/// document — because the real figure needs the anchor spans, and anything
/// that walks structure is the parse we are trying to avoid.
///
/// The two populations are nowhere near each other, which is what makes a
/// crude estimate workable:
///
/// - a 974 KB document aliased 64 times estimates at **62 MB** and parses in
///   7.5 s;
/// - the largest plausible real file — a 20 KB agent with a long role — with
///   64 aliases estimates at 1.3 MB, and in practice ORCH's own files contain
///   **no** aliases at all, so they estimate at their own length.
///
/// It does not catch *nested* amplification, where the product is small but
/// expansion is exponential (5 KB × 30 ≈ 150 KB, yet 7 s to parse). That is
/// what [`YAML_PARSE_TIMEOUT`] is for. Neither check subsumes the other.
const MAX_YAML_EXPANDED_SIZE: usize = 8 * 1024 * 1024;

/// How long `read_yaml_value` will spend parsing one document.
///
/// Generous: the largest file ORCH writes parses in single-digit milliseconds,
/// and the worst *legitimate* document allowed by [`MAX_YAML_READ_SIZE`] is
/// well under this. It is the backstop for amplification that
/// [`MAX_YAML_EXPANDED_SIZE`]'s estimate does not model — chiefly *nested*
/// aliases, where the product of length and count stays small but expansion is
/// exponential (5 KB × 30 aliases, 7 s to reject).
///
/// **Caveat worth knowing:** `spawn_blocking` work cannot be cancelled, so the
/// parse keeps running on its blocking thread after the timeout fires. The
/// caller gets a prompt error, but the thread is occupied until the parse
/// finishes — bounded, because the document is already capped at 1 MiB, which
/// is about 8 s in the worst measured case. Returning promptly while one
/// thread finishes its work is strictly better than blocking the caller for
/// the same duration.
const YAML_PARSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Most YAML aliases a document may contain.
///
/// Defence in depth. A count alone cannot be the defence, because the cost of
/// an alias depends entirely on what it points at:
///
/// **The amplifying shape is one large *collection* anchor aliased many
/// times**, and each alias deep-copies every node in it. Measured on this
/// crate, 64 aliases of a map anchor:
///
/// | document | parse time |
/// |----------|------------|
/// | 27 KB    | 272 ms     |
/// | 71 KB    | 628 ms     |
/// | 211 KB   | 1.69 s     |
/// | 974 KB   | **7.51 s** |
///
/// Every one of those **parses successfully**, and the last is inside both
/// this cap and [`MAX_YAML_READ_SIZE`] — so size and count together are not
/// enough. [`MAX_YAML_EXPANDED_SIZE`] is what rejects this shape, by combining
/// them into an estimate rather than checking each alone. (A *scalar* anchor
/// of the same size is cheap: 316 KB aliased 64 times parses in 31 ms. The
/// cost is in copying structure, not bytes.)
///
/// Nested aliases are a different and lesser problem: `serde_yaml_ng`'s own
/// repetition limit rejects them, though it spends seconds doing so.
const MAX_YAML_ALIASES: usize = 64;

/// Count `*alias` tokens outside quotes and comments.
///
/// Deliberately a scanner rather than a parse: the whole point is to decide
/// before the parser gets the document. It does not need to be exact — a
/// false positive on a document with 17+ aliases is acceptable, because
/// nothing ORCH writes has any.
fn count_yaml_aliases(content: &str) -> usize {
    let bytes = content.as_bytes();
    let mut count = 0usize;
    let mut i = 0usize;
    let (mut in_single, mut in_double, mut in_comment) = (false, false, false);

    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            b'\n' => {
                in_comment = false;
                // A newline also ends a single-quoted scalar only in the sense
                // that we stop tracking it; YAML allows multi-line quotes, and
                // over-counting is the safe direction.
            }
            b'#' if !in_single && !in_double => in_comment = true,
            b'\'' if !in_double && !in_comment => in_single = !in_single,
            b'"' if !in_single && !in_comment => in_double = !in_double,
            b'*' if !in_single && !in_double && !in_comment => {
                // An alias is `*` followed by a name character. `a * b` is
                // multiplication in a plain scalar, not an alias.
                if bytes.get(i + 1).is_some_and(|next| {
                    next.is_ascii_alphanumeric() || *next == b'_' || *next == b'-'
                }) {
                    count += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    count
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
    // Three checks, all *before* the parser sees the document, because the cost
    // being avoided is the parse itself. See the constants for what each one
    // catches; none of them subsumes another.
    if content.len() > MAX_YAML_READ_SIZE {
        return Err(Error::other(format!(
            "{} is {} bytes, over the {MAX_YAML_READ_SIZE}-byte YAML limit",
            path.display(),
            content.len()
        )));
    }
    let aliases = count_yaml_aliases(&content);
    // Estimated expanded size, before the count cap: a document can be inside
    // both the size and alias caps and still expand to tens of megabytes.
    let expanded = content.len().saturating_mul(aliases.saturating_add(1));
    if expanded > MAX_YAML_EXPANDED_SIZE {
        return Err(Error::other(format!(
            "{} is {} bytes with {aliases} aliases, which could expand to ~{} MiB: \
             over the {} MiB limit. Nothing ORCH writes uses aliases at all.",
            path.display(),
            content.len(),
            expanded / 1024 / 1024,
            MAX_YAML_EXPANDED_SIZE / 1024 / 1024
        )));
    }
    if aliases > MAX_YAML_ALIASES {
        return Err(Error::other(format!(
            "{} contains {aliases} YAML aliases, over the limit of \
             {MAX_YAML_ALIASES}: nested aliases amplify, and the parser spends \
             seconds on a few kilobytes of them. Nothing ORCH writes uses aliases \
             at all.",
            path.display()
        )));
    }
    // Parsed on a blocking thread under a hard deadline. Alias expansion is
    // CPU-bound and unbounded in the input's *expanded* size rather than its
    // length, so neither the size cap nor the alias count can bound it — only
    // a clock can. On the async thread this would also stall every other task
    // on the runtime.
    let owned_path = path.to_path_buf();
    let parsed = tokio::time::timeout(
        YAML_PARSE_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&content).map_err(|err| {
                Error::serde(format!("parse {}", owned_path.display()), &owned_path, err)
            })
        }),
    )
    .await;

    let mut value: serde_yaml_ng::Value = match parsed {
        Ok(Ok(result)) => result?,
        Ok(Err(join_err)) => {
            return Err(Error::other(format!(
                "parsing {} panicked: {join_err}",
                path.display()
            )));
        }
        Err(_elapsed) => {
            return Err(Error::other(format!(
                "parsing {} exceeded {:?}: a YAML document whose aliases expand to far more \
                 than its own size. Nothing ORCH writes uses aliases at all.",
                path.display(),
                YAML_PARSE_TIMEOUT
            )));
        }
    };
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

// ── JSONL ──────────────────────────────────────────────────────────────────

/// POSIX `PIPE_BUF`. An `O_APPEND` write up to this size is atomic, which is
/// what stops concurrent writers interleaving mid-line.
const PIPE_BUF: usize = 4096;

/// Max file size for a full [`read_jsonl`] (50 MB). Larger files read only the
/// tail.
const MAX_JSONL_READ_SIZE: u64 = 50 * 1024 * 1024;

/// Records returned when a file exceeds [`MAX_JSONL_READ_SIZE`].
const OVERSIZE_TAIL_RECORDS: usize = 200;

/// Idle time before a cached append handle is closed.
const HANDLE_IDLE: Duration = Duration::from_secs(10);

/// Append a record as one compact JSON line through a cached `O_APPEND`
/// handle.
///
/// `JSON.stringify(record) + "\n"` — compact, no spaces (PARITY.md §6.4).
///
/// When the line exceeds [`PIPE_BUF`] and the record is an object with a
/// non-empty string `data`, `data` is re-truncated so the whole line fits:
///
/// ```text
/// budget = PIPE_BUF - byteLen(JSON.stringify({...record, data: ""}) + "\n") - 3
/// data   = data.slice(0, budget) + "…"
/// ```
///
/// The `- 3` is the UTF-8 width of the `…`, and the slice counts **characters,
/// not bytes**, so multi-byte data can still push the line past `PIPE_BUF`.
/// That is a known bug in the original (PARITY.md §14.10) and is replicated:
/// the truncation is a best-effort interleaving defence, not a guarantee, and
/// local filesystems hold the inode lock anyway. A `budget <= 0` writes the
/// line over-length, unchanged.
pub async fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_dir(dir).await?;

    let value = serde_json::to_value(record)
        .map_err(|err| Error::serde(format!("serialize {}", path.display()), path, err))?;
    let line = jsonl_line(&value)
        .map_err(|err| Error::serde(format!("serialize {}", path.display()), path, err))?;

    let handle = append_handle(path).await?;
    let mut file = handle.lock().await;
    file.write_all(line.as_bytes())
        .await
        .map_err(|err| Error::io(format!("append to {}", path.display()), path, err))?;
    file.flush()
        .await
        .map_err(|err| Error::io(format!("flush {}", path.display()), path, err))
}

/// The exact bytes one JSONL line occupies, truncation applied.
fn jsonl_line(value: &serde_json::Value) -> std::result::Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    if line.len() <= PIPE_BUF {
        return Ok(line);
    }

    let Some(map) = value.as_object() else {
        return Ok(line);
    };
    let Some(data) = map.get("data").and_then(serde_json::Value::as_str) else {
        return Ok(line);
    };
    if data.is_empty() {
        return Ok(line);
    }

    // Measure the shell without `data` to know how much room `data` gets.
    // `{...obj, data: ''}` keeps `data` in its original position, and
    // `preserve_order` makes the Rust map do the same.
    let mut shell = value.clone();
    shell["data"] = serde_json::Value::String(String::new());
    let overhead = serde_json::to_string(&shell)?.len() + 1;
    let budget = PIPE_BUF as isize - overhead as isize - 3;
    if budget <= 0 {
        return Ok(line);
    }

    let mut truncated = slice_utf16_units(data, budget as usize);
    truncated.push('…');
    let mut out = value.clone();
    out["data"] = serde_json::Value::String(truncated);
    let mut line = serde_json::to_string(&out)?;
    line.push('\n');
    Ok(line)
}

/// `String.prototype.slice(0, n)`, which counts UTF-16 code units.
///
/// A cut that would land between the two halves of a surrogate pair is moved
/// back to the preceding code point: JavaScript would produce a lone
/// surrogate, which a Rust `String` cannot hold. Reachable only with an astral
/// character at exactly the budget boundary.
fn slice_utf16_units(text: &str, units: usize) -> String {
    let mut out = String::with_capacity(units);
    let mut used = 0usize;
    for ch in text.chars() {
        let width = ch.len_utf16();
        if used + width > units {
            break;
        }
        out.push(ch);
        used += width;
    }
    out
}

/// One `O_APPEND` handle per path, so a chatty run does not open and close a
/// file per event.
///
/// The TS version arms a per-path 10 s timer and re-arms it past the midpoint.
/// Here eviction is a sweep performed on each append plus the explicit
/// [`close_append_handle`] the run store calls on completion. The observable
/// property — handles do not accumulate — is the same, and it avoids a
/// detached timer task per open file; the trade is that a process which
/// appends nothing further holds one handle until it exits or closes it.
static APPEND_HANDLES: OnceLock<AsyncMutex<HashMap<PathBuf, HandleEntry>>> = OnceLock::new();

struct HandleEntry {
    file: Arc<AsyncMutex<tokio::fs::File>>,
    last_used: Instant,
}

fn append_handles() -> &'static AsyncMutex<HashMap<PathBuf, HandleEntry>> {
    APPEND_HANDLES.get_or_init(|| AsyncMutex::new(HashMap::new()))
}

async fn append_handle(path: &Path) -> Result<Arc<AsyncMutex<tokio::fs::File>>> {
    let mut handles = append_handles().lock().await;
    let now = Instant::now();
    handles.retain(|_, entry| now.duration_since(entry.last_used) < HANDLE_IDLE);

    if let Some(entry) = handles.get_mut(path) {
        entry.last_used = now;
        return Ok(Arc::clone(&entry.file));
    }

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|err| Error::io(format!("open {} for append", path.display()), path, err))?;
    let file = Arc::new(AsyncMutex::new(file));
    handles.insert(
        path.to_path_buf(),
        HandleEntry {
            file: Arc::clone(&file),
            last_used: now,
        },
    );
    Ok(file)
}

/// Release the cached append handle for a path, reclaiming the descriptor
/// immediately. Called when a run completes.
pub async fn close_append_handle(path: &Path) {
    append_handles().lock().await.remove(path);
}

/// Release every cached append handle. Call on shutdown and in test teardown.
pub async fn close_all_append_handles() {
    append_handles().lock().await.clear();
}

/// Every record in a `.jsonl` file. A missing file reads as empty.
///
/// A file over 50 MB reads only its last 200 records and warns, so a runaway
/// event log cannot exhaust memory.
pub async fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let size = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(format!("stat {}", path.display()), path, err)),
    };

    if size > MAX_JSONL_READ_SIZE {
        tracing::warn!(
            path = %path.display(),
            megabytes = size / 1024 / 1024,
            "jsonl file too large, reading tail only"
        );
        return read_jsonl_tail(path, OVERSIZE_TAIL_RECORDS).await;
    }

    let Some(content) = read_to_string_opt(path).await? else {
        return Ok(Vec::new());
    };
    Ok(parse_jsonl_lines(content.lines()))
}

/// The last `count` records of a `.jsonl` file, without loading the whole
/// thing.
///
/// A file under 32 KB is read whole and sliced. Larger files are read
/// backwards in chunks of 64 KB — 128 KB past 1 MB, because a single
/// tool-result event can exceed 8 KB — for at most 4 chunks. A chunk that did
/// not start at offset 0 may begin mid-line, so its first line is discarded.
pub async fn read_jsonl_tail<T: DeserializeOwned>(path: &Path, count: usize) -> Result<Vec<T>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let size = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(format!("stat {}", path.display()), path, err)),
    };

    if size < 32_768 {
        let Some(content) = read_to_string_opt(path).await? else {
            return Ok(Vec::new());
        };
        let all: Vec<T> = parse_jsonl_lines(content.lines());
        let start = all.len().saturating_sub(count);
        return Ok(all.into_iter().skip(start).collect());
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| Error::io(format!("open {}", path.display()), path, err))?;

    let chunk_size = std::cmp::min(size, if size > 1_048_576 { 131_072 } else { 65_536 });
    let mut position = size.saturating_sub(chunk_size);
    let mut earliest_read = position;
    let mut tail = String::new();

    for _ in 0..4 {
        earliest_read = position;
        let read_size = std::cmp::min(chunk_size, size - position) as usize;
        let mut buffer = vec![0u8; read_size];
        file.seek(std::io::SeekFrom::Start(position))
            .await
            .map_err(|err| Error::io(format!("seek {}", path.display()), path, err))?;
        file.read_exact(&mut buffer)
            .await
            .map_err(|err| Error::io(format!("read {}", path.display()), path, err))?;

        // A chunk boundary can split a multi-byte character; the lossy decode
        // only ever damages the partial first line, which is discarded below.
        tail = format!("{}{tail}", String::from_utf8_lossy(&buffer));

        let lines = tail.lines().filter(|l| !l.trim().is_empty()).count();
        // Strictly more than `count`, because the first line may be partial
        // and is discarded below.
        if lines > count || position == 0 {
            break;
        }
        position = position.saturating_sub(chunk_size);
    }

    let mut lines: Vec<&str> = tail.lines().filter(|l| !l.trim().is_empty()).collect();
    if earliest_read > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(count);
    Ok(parse_jsonl_lines(lines.into_iter().skip(start)))
}

/// Parse JSONL lines, skipping corrupt ones with a warning rather than
/// failing the read — a truncated last line must not lose the whole log.
fn parse_jsonl_lines<'l, T: DeserializeOwned>(lines: impl Iterator<Item = &'l str>) -> Vec<T> {
    let mut out = Vec::new();
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(record) => out.push(record),
            Err(_) => {
                let preview: String = line.chars().take(200).collect();
                tracing::warn!(line = %preview, "skipping corrupt jsonl line");
            }
        }
    }
    out
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

    /// Any `.tmp` siblings left behind in `dir`.
    ///
    /// The whole point of writing through a temp file is that a failure leaves
    /// the destination untouched; leaving litter behind instead breaks the
    /// other half of that promise (code m3).
    fn stray_temp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".tmp"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The measured amplification bomb: 14 nesting levels of `<<: [*p, *p]`
    /// over a 4 KB anchor, in ~5 KB of YAML.
    fn alias_bomb() -> String {
        let mut doc = String::from("base: &b\n");
        for i in 0..100 {
            doc.push_str(&format!("  k{i}: {}\n", "v".repeat(40)));
        }
        let mut previous = "b".to_owned();
        for level in 1..=14 {
            doc.push_str(&format!(
                "m{level}: &m{level}\n  <<: [*{previous}, *{previous}]\n  e{level}: 1\n"
            ));
            previous = format!("m{level}");
        }
        doc.push_str(&format!("top:\n  <<: [*{previous}, *{previous}]\n"));
        doc
    }

    /// The shape that actually amplifies: one large **collection** anchor,
    /// aliased many times, so every alias deep-copies every node.
    ///
    /// Sized to sit inside both the size cap and the alias cap, because that
    /// is the point — neither of those can stop it.
    /// gate-security-review C0: a flat anchor, then five levels each aliasing
    /// the previous one `w` times, so the node count is `w^6` while the text
    /// stays a few kilobytes. This is the shape the security review measured
    /// at **7 s (w=300)** and **104 s (w=1200)** unguarded, parsing to a real
    /// error (`repetition limit exceeded`) rather than being refused.
    fn c0_bomb(w: usize) -> String {
        let mut s = String::from("a0: &a0 [");
        for i in 0..w {
            if i > 0 {
                s.push(',');
            }
            s.push_str("\"xxxxxxxx\"");
        }
        s.push_str("]\n");
        for level in 1..=5 {
            s.push_str(&format!("a{level}: &a{level} ["));
            for i in 0..w {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!("*a{}", level - 1));
            }
            s.push_str("]\n");
        }
        s
    }

    fn flat_anchor_bomb() -> String {
        let mut doc = String::from("a: &a\n");
        for i in 0..60_000 {
            doc.push_str(&format!("  k{i}: v{i}\n"));
        }
        doc.push_str("list:\n");
        for _ in 0..64 {
            doc.push_str("  - *a\n");
        }
        doc
    }

    #[tokio::test]
    async fn a_flat_anchor_bomb_is_refused_promptly() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("flat.yml");
        let bomb = flat_anchor_bomb();
        // Inside both cheap caps, which is what makes the timeout necessary.
        assert!(bomb.len() < MAX_YAML_READ_SIZE, "inside the size cap");
        assert!(
            count_yaml_aliases(&bomb) <= MAX_YAML_ALIASES,
            "inside the alias cap"
        );
        std::fs::write(&path, &bomb).unwrap();

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path)
            .await
            .expect_err("an amplifying document must be refused");
        let elapsed = started.elapsed();

        // Refused by the expanded-size estimate, so in microseconds — not at
        // the deadline. 7.5 s unguarded, and the deadline alone would still
        // have made the caller wait two seconds.
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "rejection took {elapsed:?}; the estimate should refuse this without parsing"
        );
        let message = format!("{err}");
        assert!(
            message.contains("flat.yml"),
            "must name the file: {message}"
        );
        assert!(message.contains("expand"), "must say why: {message}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_owner_only_write_is_never_briefly_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.yml");
        atomic_write_with_mode(&path, "k: v\n", owner_only())
            .await
            .unwrap();

        let mode = tokio::fs::metadata(&path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");

        // And the default path is left to the umask rather than quietly
        // tightened: a reader checking `state.json`'s mode should see the
        // ordinary thing, or this change is wider than it claims to be.
        let open = dir.path().join("ordinary.yml");
        atomic_write(&open, "k: v\n").await.unwrap();
        let open_mode = tokio::fs::metadata(&open)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_ne!(
            open_mode, 0o600,
            "atomic_write should not be setting an owner-only mode of its own accord"
        );
    }

    #[tokio::test]
    async fn the_c0_nested_alias_shape_is_refused_in_microseconds() {
        for w in [300usize, 1200] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("c0.yml");
            let doc = c0_bomb(w);
            tokio::fs::write(&path, &doc).await.unwrap();

            let started = std::time::Instant::now();
            let err = read_yaml_value(&path).await.expect_err("must be refused");
            let elapsed = started.elapsed();

            // The expanded-size estimate is what catches this: 9 KB x 1501 is
            // ~13 MiB and 37 KB x 6001 is ~213 MiB, both over the 8 MiB bound,
            // so it never reaches the parser or the deadline. Measured at
            // 195 us and 326 us against 7 s and 104 s unguarded.
            assert!(
                elapsed < std::time::Duration::from_millis(200),
                "w={w} took {elapsed:?}"
            );
            let message = format!("{err}");
            assert!(
                message.contains("expand"),
                "w={w} must be refused by the expansion estimate, not something else: {message}"
            );
        }
    }

    #[tokio::test]
    async fn an_ordinary_document_is_nowhere_near_the_deadline() {
        // The deadline must not be anywhere near a real file's cost. This is
        // larger than anything ORCH writes.
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("big-but-honest.yml");
        let mut doc = String::new();
        for i in 0..20_000 {
            doc.push_str(&format!("k{i}: value number {i}\n"));
        }
        std::fs::write(&path, &doc).unwrap();

        let started = std::time::Instant::now();
        let value = read_yaml_value(&path).await.unwrap().expect("present");
        let elapsed = started.elapsed();

        assert_eq!(value["k19999"].as_str(), Some("value number 19999"));
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "an alias-free 20k-key document took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_nested_alias_bomb_is_refused_by_the_deadline() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("bomb.yml");
        let bomb = alias_bomb();
        std::fs::write(&path, &bomb).unwrap();

        // 28 aliases — inside the count cap, so the deadline is what stops
        // it. `serde_yaml_ng` would reject it eventually, through its own
        // repetition limit, but only after about seven seconds.
        assert!(count_yaml_aliases(&bomb) <= MAX_YAML_ALIASES);

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path)
            .await
            .expect_err("an alias bomb must be refused");
        let elapsed = started.elapsed();

        assert!(
            elapsed < YAML_PARSE_TIMEOUT + std::time::Duration::from_millis(500),
            "the caller waited {elapsed:?}, past the deadline"
        );
        let message = format!("{err}");
        assert!(
            message.contains("bomb.yml"),
            "must name the file: {message}"
        );
        assert!(message.contains("exceeded"), "must say why: {message}");
    }

    #[tokio::test]
    async fn too_many_aliases_is_refused_without_parsing_at_all() {
        // The count cap's own test, now that both bombs are caught by the
        // deadline. This document is cheap to parse, so only the count can be
        // what rejects it — and it must do so without a parse.
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("many.yml");
        let mut doc = String::from("a: &a 1\nlist:\n");
        for _ in 0..(MAX_YAML_ALIASES + 1) {
            doc.push_str("  - *a\n");
        }
        std::fs::write(&path, &doc).unwrap();

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path).await.expect_err("over the cap");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "the count cap must not involve a parse"
        );
        let message = format!("{err}");
        assert!(
            message.contains("many.yml"),
            "must name the file: {message}"
        );
        assert!(message.contains("aliases"), "must say why: {message}");
    }

    #[tokio::test]
    async fn an_oversized_yaml_document_is_rejected_by_size() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("huge.yml");
        // Just over the cap, with no aliases at all, so the size check is
        // unambiguously the one that fires.
        std::fs::write(&path, format!("key: {}\n", "x".repeat(MAX_YAML_READ_SIZE))).unwrap();

        let err = read_yaml_value(&path).await.expect_err("over the size cap");
        let message = format!("{err}");
        assert!(
            message.contains("huge.yml"),
            "must name the file: {message}"
        );
        assert!(message.contains("YAML limit"), "must say why: {message}");
    }

    #[tokio::test]
    async fn a_document_with_a_few_merge_keys_still_reads() {
        // The guard must not break the thing it exists to allow: a
        // hand-written config using merge keys (§6.4.2).
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("merged.yml");
        std::fs::write(
            &path,
            "defaults: &d\n  a: 1\n  b: 2\none:\n  <<: *d\n  b: 3\ntwo:\n  <<: *d\n",
        )
        .unwrap();

        let value = read_yaml_value(&path).await.unwrap().expect("present");
        assert_eq!(value["one"]["a"].as_u64(), Some(1));
        assert_eq!(value["one"]["b"].as_u64(), Some(3), "the override wins");
        assert_eq!(value["two"]["b"].as_u64(), Some(2), "the merged value");
    }

    #[test]
    fn the_alias_scanner_ignores_quotes_comments_and_multiplication() {
        // Over-counting is safe; under-counting is not. These are the cases
        // where a naive `content.matches('*')` would over-count so badly that
        // an ordinary file would be refused.
        assert_eq!(count_yaml_aliases("a: *anchor\n"), 1);
        assert_eq!(count_yaml_aliases("a: [*x, *y, *z]\n"), 3);
        assert_eq!(count_yaml_aliases("a: '*not an alias'\n"), 0);
        assert_eq!(count_yaml_aliases("a: \"*also not\"\n"), 0);
        assert_eq!(count_yaml_aliases("# *commented out\n"), 0);
        assert_eq!(count_yaml_aliases("a: 3 * 4\n"), 0, "multiplication");
        assert_eq!(count_yaml_aliases("a: **\n"), 0, "no name after the star");
        assert_eq!(count_yaml_aliases(""), 0);
        // The scanner counts; it does not judge. The nested bomb sits
        // *under* the cap and is caught by the deadline instead.
        assert_eq!(
            count_yaml_aliases(&alias_bomb()),
            30,
            "14 levels of two, plus the two in `top`"
        );
    }

    #[tokio::test]
    async fn a_failed_write_leaves_no_temp_file_and_no_destination() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let dir = temp.path().join("ro");
        std::fs::create_dir_all(&dir).unwrap();
        // Pre-create so `ensure_dir`'s cache does not try to mkdir it later.
        let target = dir.join("thing.yml");

        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(&dir, perms).unwrap();

        let result = atomic_write(&target, "payload\n").await;

        // Restore first, so an assertion failure cannot leave an undeletable
        // tempdir behind.
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&dir, perms).unwrap();

        assert!(result.is_err(), "a read-only directory must fail the write");
        assert!(!target.exists(), "the destination must not be created");
        assert_eq!(stray_temp_files(&dir), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_failed_rename_leaves_no_temp_file() {
        // The destination is a non-empty *directory*, so the rename fails
        // after the temp file was written — the second error branch.
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let target = temp.path().join("occupied");
        std::fs::create_dir_all(target.join("inner")).unwrap();

        let result = atomic_write(&target, "payload\n").await;

        assert!(result.is_err(), "renaming onto a non-empty dir must fail");
        assert!(target.is_dir(), "the destination is untouched");
        assert_eq!(stray_temp_files(temp.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn the_temp_file_is_created_exclusively_but_the_target_is_replaced() {
        // `create_new` stops the temp path being a planted symlink or an
        // existing file that gets truncated. It applies to the temp name
        // only — replacing the destination must still work.
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let target = temp.path().join("thing.yml");
        atomic_write(&target, "first\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first\n");
        atomic_write(&target, "second\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second\n");
        assert_eq!(stray_temp_files(temp.path()), Vec::<String>::new());
    }

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

    // ── JSONL ──────────────────────────────────────────────────────────────

    /// The append-handle cache is process-global, so a test that asserts on
    /// its contents must not run beside one that mutates it. Cargo runs tests
    /// in one process on many threads, so the serialization has to be
    /// explicit.
    static HANDLE_CACHE_TESTS: AsyncMutex<()> = AsyncMutex::const_new(());

    fn event(data: &str) -> serde_json::Value {
        serde_json::json!({
            "timestamp": "2026-10-13T16:37:59.169Z",
            "type": "agent_output",
            "data": data,
        })
    }

    /// Byte and character counts measured by running the TS `appendJsonl`
    /// arithmetic under node, not derived from reading it.
    #[test]
    fn the_pipe_buf_truncation_matches_the_reference_arithmetic() {
        // A short line is untouched and compact — no spaces.
        let short = jsonl_line(&serde_json::json!({
            "timestamp": "t", "type": "agent_output", "data": "hi"
        }))
        .unwrap();
        assert_eq!(
            short,
            "{\"timestamp\":\"t\",\"type\":\"agent_output\",\"data\":\"hi\"}\n"
        );

        // ASCII data is truncated to land exactly on PIPE_BUF.
        let ascii = jsonl_line(&event(&"x".repeat(8000))).unwrap();
        assert_eq!(ascii.len(), 4096, "node measures 4096 bytes");
        assert_eq!(ascii.chars().count(), 4094, "node measures 4094 chars");
        assert!(ascii.ends_with("…\"}\n"));

        // PARITY.md §14.10: the slice counts characters, so multi-byte data
        // still overshoots PIPE_BUF. Replicated, including the exact size.
        let multibyte = jsonl_line(&event(&"é".repeat(8000))).unwrap();
        assert_eq!(
            multibyte.len(),
            8116,
            "node measures 8116 bytes — the §14.10 overshoot"
        );
        assert_eq!(multibyte.chars().count(), 4094);
        assert!(multibyte.ends_with("…\"}\n"));
    }

    #[test]
    fn a_long_line_without_a_string_data_field_is_written_unchanged() {
        let no_data = serde_json::json!({
            "timestamp": "t", "type": "x", "other": "y".repeat(8000)
        });
        assert_eq!(
            jsonl_line(&no_data).unwrap().len(),
            8040,
            "node measures 8040"
        );

        let empty_data = serde_json::json!({
            "timestamp": "t", "type": "x", "data": "", "pad": "z".repeat(8000)
        });
        assert_eq!(
            jsonl_line(&empty_data).unwrap().len(),
            8048,
            "node measures 8048"
        );

        // A non-object record is left alone too.
        let array = serde_json::json!(["x".repeat(8000)]);
        assert!(jsonl_line(&array).unwrap().len() > PIPE_BUF);
    }

    #[test]
    fn truncation_keeps_the_data_field_in_its_original_position() {
        // `{...obj, data: ''}` does not move `data`, and neither may we — the
        // shell's byte length is what the budget is computed from.
        let line = jsonl_line(&event(&"x".repeat(8000))).unwrap();
        let keys: Vec<&str> = ["timestamp", "type", "data"].to_vec();
        let mut last = 0;
        for key in keys {
            let at = line.find(&format!("\"{key}\"")).expect(key);
            assert!(at >= last, "{key} moved");
            last = at;
        }
    }

    #[test]
    fn slicing_counts_utf16_units_and_never_splits_a_surrogate_pair() {
        assert_eq!(slice_utf16_units("hello", 3), "hel");
        assert_eq!(slice_utf16_units("hello", 99), "hello");
        assert_eq!(slice_utf16_units("", 3), "");
        // A BMP character is one unit.
        assert_eq!(slice_utf16_units("éé", 1), "é");
        // An astral character is two units, so a budget of 1 excludes it
        // rather than emitting a lone surrogate as JavaScript would.
        assert_eq!(slice_utf16_units("\u{1F600}", 1), "");
        assert_eq!(slice_utf16_units("\u{1F600}", 2), "\u{1F600}");
        assert_eq!(slice_utf16_units("a\u{1F600}", 2), "a");
    }

    #[tokio::test]
    async fn append_jsonl_writes_one_compact_line_per_record() {
        let _serial = HANDLE_CACHE_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("run.jsonl");

        for n in 0..3 {
            append_jsonl(&path, &event(&format!("line {n}")))
                .await
                .unwrap();
        }
        // Per-path, not `close_all_*`: the cache is process-global and these
        // tests run in parallel.
        close_append_handle(&path).await;

        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(raw.lines().count(), 3);
        assert!(raw.ends_with('\n'));
        assert!(!raw.contains(": "), "not compact: {raw}");

        let records: Vec<serde_json::Value> = read_jsonl(&path).await.unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["data"], "line 0");
    }

    #[tokio::test]
    async fn appends_from_a_second_handle_interleave_without_corruption() {
        let _serial = HANDLE_CACHE_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("run.jsonl");

        let mut handles = Vec::new();
        for n in 0..32 {
            let path = path.clone();
            handles.push(tokio::spawn(async move {
                append_jsonl(&path, &event(&format!("line {n}")))
                    .await
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        close_append_handle(&path).await;

        // Every line is intact, which is what O_APPEND under PIPE_BUF buys.
        let records: Vec<serde_json::Value> = read_jsonl(&path).await.unwrap();
        assert_eq!(records.len(), 32);
    }

    #[test]
    fn a_lone_surrogate_escape_becomes_the_replacement_character() {
        // The exact shape the oracle writes when `.slice(0, 4000)` cuts
        // between the surrogates of an emoji — measured, not constructed.
        let written = r#"{"body":"aaa\ud83d","subject":"bbb\ud83d"}"#;
        let repaired = repair_lone_surrogates(written);
        assert!(
            matches!(repaired, Cow::Owned(_)),
            "must have been rewritten"
        );

        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("the repaired text must parse");
        assert_eq!(value["body"], "aaa\u{FFFD}");
        assert_eq!(value["subject"], "bbb\u{FFFD}");

        // And the unrepaired original really is unparseable, so the repair is
        // load-bearing rather than belt-and-braces.
        assert!(serde_json::from_str::<serde_json::Value>(written).is_err());
    }

    #[test]
    fn a_valid_surrogate_pair_is_left_exactly_alone() {
        // The common case: nothing may change the value of a well-formed file.
        let pair = r#"{"body":"hi 😀 there"}"#;
        assert!(matches!(repair_lone_surrogates(pair), Cow::Borrowed(_)));
        let value: serde_json::Value = serde_json::from_str(pair).unwrap();
        assert_eq!(value["body"], "hi \u{1F600} there");

        // An *escaped* pair is the case that actually exercises the pairing
        // check, and the hex may be in any case — JSON.stringify emits
        // lowercase, but a hand-edited file or another writer need not.
        for escaped in [
            "\"\\ud83d\\ude00\"",
            "\"\\uD83D\\uDE00\"",
            "\"\\ud83D\\uDe00\"",
        ] {
            assert!(
                matches!(repair_lone_surrogates(escaped), Cow::Borrowed(_)),
                "{escaped}"
            );
            let value: serde_json::Value = serde_json::from_str(escaped).unwrap();
            assert_eq!(value, "\u{1F600}", "{escaped}");
        }
    }

    #[test]
    fn a_lone_low_surrogate_and_a_half_pair_are_both_repaired() {
        // A trailing low surrogate with no high one, and a high surrogate
        // followed by an ordinary escape rather than its partner.
        for (written, expected) in [
            (r#""\ude00""#, "\u{FFFD}"),
            (r#""\ud83dA""#, "\u{FFFD}A"),
            ("\"\\ud83d\\ud83d\\ude00\"", "\u{FFFD}\u{1F600}"),
        ] {
            let repaired = repair_lone_surrogates(written);
            let value: serde_json::Value =
                serde_json::from_str(&repaired).unwrap_or_else(|e| panic!("{written}: {e}"));
            assert_eq!(value, expected, "{written}");
        }
    }

    #[test]
    fn an_escaped_backslash_does_not_start_an_escape() {
        // `\\uD83D` is a literal backslash then the text `uD83D`. Treating it
        // as an escape would corrupt a body that merely talks about one.
        let written = r#"{"body":"literal \\ud83d stays"}"#;
        assert!(matches!(repair_lone_surrogates(written), Cow::Borrowed(_)));
        let value: serde_json::Value = serde_json::from_str(written).unwrap();
        assert_eq!(value["body"], r"literal \ud83d stays");
    }

    #[test]
    fn content_without_escapes_is_borrowed_untouched() {
        for plain in [
            "{}",
            r#"{"body":"nothing to see"}"#,
            r#"{"body":"an emoji é and a 😀 straight through"}"#,
        ] {
            assert!(
                matches!(repair_lone_surrogates(plain), Cow::Borrowed(_)),
                "{plain}"
            );
        }
    }

    #[tokio::test]
    async fn read_json_tolerates_a_ts_written_lone_surrogate() {
        // The end-to-end guarantee from the ruling: a file the oracle wrote
        // must not fail the read.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("msg_lonely.json");
        std::fs::write(&path, "{\"id\":\"msg_abc1234\",\"body\":\"aaa\\ud83d\"}\n").unwrap();

        let value: serde_json::Value = read_json(&path)
            .await
            .expect("a lone surrogate must not fail the read")
            .expect("the file exists");
        assert_eq!(value["id"], "msg_abc1234");
        assert_eq!(value["body"], "aaa\u{FFFD}");

        // Read-side only: the bytes on disk are untouched.
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains(r"\ud83d"), "the file was rewritten: {after}");
    }

    #[tokio::test]
    async fn read_jsonl_skips_a_corrupt_line_rather_than_failing() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("run.jsonl");
        atomic_write(&path, "{\"a\":1}\n{not json\n\n{\"a\":2}\n")
            .await
            .unwrap();

        let records: Vec<serde_json::Value> = read_jsonl(&path).await.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["a"], 1);
        assert_eq!(records[1]["a"], 2);
    }

    #[tokio::test]
    async fn read_jsonl_on_a_missing_file_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let records: Vec<serde_json::Value> =
            read_jsonl(&temp.path().join("nope.jsonl")).await.unwrap();
        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn read_jsonl_tail_slices_a_small_file() {
        let _serial = HANDLE_CACHE_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("run.jsonl");
        for n in 0..10 {
            append_jsonl(&path, &serde_json::json!({ "n": n }))
                .await
                .unwrap();
        }
        close_append_handle(&path).await;

        let tail: Vec<serde_json::Value> = read_jsonl_tail(&path, 3).await.unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0]["n"], 7);
        assert_eq!(tail[2]["n"], 9);

        // Asking for more than exists returns everything.
        let all: Vec<serde_json::Value> = read_jsonl_tail(&path, 99).await.unwrap();
        assert_eq!(all.len(), 10);
        assert!(
            read_jsonl_tail::<serde_json::Value>(&path, 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn read_jsonl_tail_reads_backwards_past_the_32kb_threshold() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("big.jsonl");

        // ~500 records of ~1 KB each: comfortably over 32 KB, so the
        // chunked backwards path runs.
        let mut content = String::new();
        for n in 0..500 {
            content.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "n": n, "pad": "x".repeat(1000)
                }))
                .unwrap(),
            );
            content.push('\n');
        }
        atomic_write(&path, &content).await.unwrap();
        assert!(tokio::fs::metadata(&path).await.unwrap().len() > 32_768);

        let tail: Vec<serde_json::Value> = read_jsonl_tail(&path, 5).await.unwrap();
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0]["n"], 495);
        assert_eq!(tail[4]["n"], 499);

        // A tail larger than one chunk still works, bounded by the 4-chunk cap.
        let tail: Vec<serde_json::Value> = read_jsonl_tail(&path, 100).await.unwrap();
        assert_eq!(tail.len(), 100);
        assert_eq!(tail[99]["n"], 499);
    }

    #[tokio::test]
    async fn read_jsonl_tail_discards_a_partial_leading_line() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("big.jsonl");

        // One record wide enough that a 64 KB chunk starts mid-record.
        let mut content = String::new();
        for n in 0..40 {
            content.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "n": n, "pad": "y".repeat(2000)
                }))
                .unwrap(),
            );
            content.push('\n');
        }
        atomic_write(&path, &content).await.unwrap();

        // Whatever comes back parses — no half-record leaks through.
        let tail: Vec<serde_json::Value> = read_jsonl_tail(&path, 3).await.unwrap();
        assert_eq!(tail.len(), 3);
        for record in &tail {
            assert!(record["n"].is_number(), "{record}");
        }
        assert_eq!(tail[2]["n"], 39);
    }

    #[tokio::test]
    async fn close_append_handle_releases_just_that_path() {
        let _serial = HANDLE_CACHE_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let one = temp.path().join("one.jsonl");
        let two = temp.path().join("two.jsonl");
        append_jsonl(&one, &serde_json::json!({ "a": 1 }))
            .await
            .unwrap();
        append_jsonl(&two, &serde_json::json!({ "a": 2 }))
            .await
            .unwrap();

        // The handle cache is process-global and these tests run in parallel,
        // so assert only about this test's own paths — never the total.
        let held = |path: &std::path::Path| {
            let path = path.to_path_buf();
            async move { append_handles().lock().await.contains_key(&path) }
        };
        assert!(held(&one).await);
        assert!(held(&two).await);

        close_append_handle(&one).await;
        assert!(!held(&one).await);
        assert!(held(&two).await, "closing one path must not close another");

        // Appending again after a close reopens and keeps the earlier data.
        append_jsonl(&one, &serde_json::json!({ "a": 3 }))
            .await
            .unwrap();
        close_append_handle(&one).await;
        let records: Vec<serde_json::Value> = read_jsonl(&one).await.unwrap();
        assert_eq!(records.len(), 2);
    }

    #[tokio::test]
    async fn close_all_append_handles_is_safe_to_call_at_any_time() {
        let _serial = HANDLE_CACHE_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("run.jsonl");
        append_jsonl(&path, &serde_json::json!({ "a": 1 }))
            .await
            .unwrap();

        // Cannot assert the map is empty afterwards: it is process-global and
        // other tests append in parallel. What matters is that it is safe and
        // that appending still works.
        close_all_append_handles().await;
        append_jsonl(&path, &serde_json::json!({ "a": 2 }))
            .await
            .unwrap();
        close_append_handle(&path).await;

        let records: Vec<serde_json::Value> = read_jsonl(&path).await.unwrap();
        assert_eq!(records.len(), 2);
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
