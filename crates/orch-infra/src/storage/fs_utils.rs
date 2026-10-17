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
/// `config.yml` holds adapter commands and API-key environment names,
/// `agents/*.yml` hold the role prompts an agent is run with, and `peer-id`
/// plus everything under `remotes/` hold the peer session identity and remote
/// agent snapshots (security review F3/F7). All are worth
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
    write_json_with_mode(path, value, None).await
}

/// `write_json`, with an explicit unix mode — see [`atomic_write_with_mode`].
pub async fn write_json_with_mode<T: Serialize>(
    path: &Path,
    value: &T,
    mode: Option<u32>,
) -> Result<()> {
    let mut content = serde_json::to_string_pretty(value)
        .map_err(|err| Error::serde(format!("serialize {}", path.display()), path, err))?;
    content.push('\n');
    atomic_write_with_mode(path, &content, mode).await
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

/// Ceiling on a document's **exact** expanded node count.
///
/// This is the load-bearing defence against alias amplification, and it is
/// exact rather than estimated: [`expanded_node_count`] walks the document's
/// parse *events* once and computes what the expanded tree would contain,
/// without ever building it.
///
/// It replaced a `length × (1 + aliases)` estimate that the security review
/// broke (C0). The estimate assumed an alias expands to at most the whole
/// document, which is wildly wrong for *nested* aliases: doubling needs only
/// two aliases per level, so 30 levels is 60 aliases — under any sane alias
/// cap — for 2^30 expansion, while both factors in the estimate stay small.
/// The review's `shapeD_6000_30` is 84,661 bytes with 60 aliases, estimates at
/// 4.93 MiB, passes every cap, and took `orch status` to **19.3 GiB RSS in 28
/// seconds**, still climbing (180 GiB at three minutes in an unbounded run).
///
/// 200,000 nodes is roughly two orders of magnitude above anything ORCH writes
/// — the largest real file is an agent with a long role, a few hundred nodes —
/// and well under what `serde_yaml_ng` can build quickly.
const MAX_YAML_EXPANDED_NODES: u64 = 200_000;

/// Exact number of nodes the expanded document would contain, or an error if
/// that exceeds [`MAX_YAML_EXPANDED_NODES`]. Also enforces the exact
/// [`MAX_YAML_ALIASES`] cap across the stream.
///
/// # This is the sole DoS defence for YAML reads, and it is load-bearing alone
///
/// There is no second line. The size cap before it and the alias cap enforced
/// within it are defence in depth, and **the 2 s parse deadline that
/// used to sit behind it has been deleted** — it could not cancel the work it
/// timed, so it turned a slow failure into a non-terminating one (security
/// review C0). Nothing else bounds what `serde_yaml_ng` is asked to build.
///
/// So: weakening, short-circuiting or bypassing this function reopens a
/// remote-ish DoS with a measured 19.3 GiB RSS in 28 seconds and no process
/// exit. **Any change to it, or to the budget it enforces, needs a security
/// review** — not a passing test suite, which the vulnerable build also had.
///
/// **This walks events only. It never resolves an alias and never materialises
/// one.** That is the entire point: the cost being avoided is the expansion,
/// so a later refactor must not "simplify" this into a parse and read the
/// structure back. `saphyr-parser` streams events without expanding aliases, so
/// the walk is O(input) in time and O(depth + anchors) in memory however large
/// the expansion would be.
///
/// Counting rule: a scalar is 1 node; a collection is 1 plus the sum of its
/// children; an alias is however many nodes its anchor expands to. Anchors are
/// memoised by the parser's numeric anchor **id**, not by name, which is what
/// makes redefinition correct for free — redefining `&x` yields a fresh id and
/// later aliases carry that new id, so each alias is counted against the
/// definition in scope at that point.
///
/// Merge keys need no special case: `<<: *x` is emitted as a `<<` scalar
/// followed by an ordinary `Alias`, so `x`'s full expanded count is added at
/// every use. `apply_merge`'s deep copy is therefore bounded by the same total.
///
/// Arithmetic is checked and the budget is tested at every accumulation, not
/// once at the end: nested doubling with two or three children per level wraps
/// `u64` after roughly 40–64 levels, and a wrapped count is a small number that
/// would sail through. Overflow is treated as over budget, never as a value.
fn expanded_node_count(path: &Path, content: &str) -> Result<u64> {
    use saphyr_parser::{Event, Parser};

    let over_budget = |count: u64| -> Error {
        Error::other(format!(
            "{} expands to at least {count} nodes, over the limit of \
             {MAX_YAML_EXPANDED_NODES}: its aliases multiply out to far more than \
             the document's own size. Nothing ORCH writes uses aliases at all.",
            path.display()
        ))
    };

    // Expanded node count of each *closed* anchor definition, keyed by the
    // parser's anchor id.
    let mut anchors: std::collections::HashMap<usize, u64> = std::collections::HashMap::new();
    // One frame per open collection: the anchor id to memoise on close, and
    // the running total of its children's expanded counts.
    let mut open: Vec<(usize, u64)> = Vec::new();
    let mut total: u64 = 0;
    let mut aliases: usize = 0;

    // Fold one finished node of `count` nodes into its parent, or into the
    // document total at the top level, checking the budget as we go.
    let place = |count: u64, open: &mut Vec<(usize, u64)>, total: &mut u64| -> Result<()> {
        if count > MAX_YAML_EXPANDED_NODES {
            return Err(over_budget(count));
        }
        match open.last_mut() {
            Some((_, sum)) => {
                *sum = sum
                    .checked_add(count)
                    .ok_or_else(|| over_budget(u64::MAX))?;
                if *sum > MAX_YAML_EXPANDED_NODES {
                    return Err(over_budget(*sum));
                }
            }
            None => {
                *total = total
                    .checked_add(count)
                    .ok_or_else(|| over_budget(u64::MAX))?;
                if *total > MAX_YAML_EXPANDED_NODES {
                    return Err(over_budget(*total));
                }
            }
        }
        Ok(())
    };

    for event in Parser::new_from_str(content) {
        let (event, _span) = event
            .map_err(|err| Error::other(format!("{} is not valid YAML: {err}", path.display())))?;
        match event {
            Event::Scalar(_, _, aid, _) => {
                if aid != 0 {
                    anchors.insert(aid, 1);
                }
                place(1, &mut open, &mut total)?;
            }
            Event::SequenceStart(aid, _) | Event::MappingStart(aid, _) => {
                open.push((aid, 0));
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (aid, children) = open.pop().ok_or_else(|| {
                    Error::other(format!(
                        "{} is not valid YAML: unbalanced collection",
                        path.display()
                    ))
                })?;
                // The collection node itself, plus its children.
                let count = children
                    .checked_add(1)
                    .ok_or_else(|| over_budget(u64::MAX))?;
                if aid != 0 {
                    anchors.insert(aid, count);
                }
                place(count, &mut open, &mut total)?;
            }
            Event::Alias(aid) => {
                aliases += 1;
                if aliases > MAX_YAML_ALIASES {
                    return Err(Error::other(format!(
                        "{} contains more than {MAX_YAML_ALIASES} YAML aliases, over the limit of \
                         {MAX_YAML_ALIASES}: nested aliases amplify, and the parser spends \
                         seconds on a few kilobytes of them. Nothing ORCH writes uses aliases \
                         at all.",
                        path.display()
                    )));
                }
                // Absent means the anchor is not *closed* yet, i.e. the alias
                // sits inside its own definition (`a: &r [*r]`). That is a
                // cycle: its expansion is unbounded and serde_yaml_ng will
                // reject it too, so refuse rather than guess a count.
                let count = *anchors.get(&aid).ok_or_else(|| {
                    Error::other(format!(
                        "{} aliases an anchor from inside its own definition, which \
                         expands forever. Nothing ORCH writes uses aliases at all.",
                        path.display()
                    ))
                })?;
                place(count, &mut open, &mut total)?;
            }
            _ => {}
        }
    }
    Ok(total)
}

/// Most YAML aliases a document may contain, counted exactly from parser
/// events by [`expanded_node_count`].
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
/// enough. [`MAX_YAML_EXPANDED_NODES`] is what rejects this shape, by counting
/// the expansion exactly instead of bounding its inputs. (A *scalar* anchor of
/// the same size is cheap: 316 KB aliased 64 times parses in 31 ms. The cost is
/// in copying structure, not bytes.)
///
/// Nested aliases are a different and lesser problem: `serde_yaml_ng`'s own
/// repetition limit rejects them, though it spends seconds doing so.
const MAX_YAML_ALIASES: usize = 64;

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
    // Two steps before the value parse: cap the input size, then walk parser
    // events to enforce both the expanded-node budget and the exact alias cap.
    if content.len() > MAX_YAML_READ_SIZE {
        return Err(Error::other(format!(
            "{} is {} bytes, over the {MAX_YAML_READ_SIZE}-byte YAML limit",
            path.display(),
            content.len()
        )));
    }
    // **This single call is what makes the parse below safe.** The size and
    // alias caps are defence in depth, and there is no backstop behind it:
    // the deadline that used to be there was removed because it could not
    // stop the parse it was timing. Removing or weakening this line reopens
    // security review C0 (19.3 GiB RSS in 28 s, no exit), so it needs a
    // security review rather than a green suite.
    expanded_node_count(path, &content)?;
    // Parsed on a blocking thread, with **no deadline**, because the work is
    // now bounded by construction rather than by a clock.
    //
    // There was a 2 s timeout here and it was worse than nothing (security
    // review C0). `spawn_blocking` work cannot be cancelled and tokio waits
    // for outstanding blocking tasks at shutdown, so on a runaway document the
    // timeout printed a correct error and then the process allocated ~0.55
    // GiB/s forever — 19 GiB in 28 s, 180 GiB at three minutes — and could not
    // exit. It converted a slow failure that terminated into a fast message
    // followed by a non-terminating one.
    //
    // A timeout cannot be the defence here: a clock that fires on work it
    // cannot stop is a diagnostic, not a control. `expanded_node_count` above
    // is the control, and it bounds the parse exactly. Measured ceilings for a
    // document that passes all three limits — 1 MiB, <=64 aliases, <=200,000
    // expanded nodes:
    //
    // | worst case                | release | debug  |
    // |---------------------------|---------|--------|
    // | max expansion (63 alias)  | 31 ms   | 86 ms  |
    // | max length, no aliases    | 161 ms  | 899 ms |
    //
    // Restoring a 2 s deadline on top of that would be actively harmful if it
    // killed the process, which is the only way a deadline could contain this
    // work: the slowest document that reaches the parser is a *legitimate*
    // 1 MiB file at 899 ms in debug, a margin of 2.2x, so a loaded machine
    // would terminate a valid run. The parse stays on a blocking thread so a
    // large legitimate file does not stall the runtime's other tasks.
    let owned_path = path.to_path_buf();
    let parsed = tokio::task::spawn_blocking(move || {
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&content).map_err(|err| {
            Error::serde(format!("parse {}", owned_path.display()), &owned_path, err)
        })
    })
    .await;

    let mut value: serde_yaml_ng::Value = match parsed {
        Ok(result) => result?,
        Err(join_err) => {
            return Err(Error::other(format!(
                "parsing {} panicked: {join_err}",
                path.display()
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
fn alias_events(content: &str) -> usize {
    use saphyr_parser::{Event, Parser};

    Parser::new_from_str(content)
        .map(|event| event.expect("valid YAML").0)
        .filter(|event| matches!(event, Event::Alias(_)))
        .count()
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
        // Inside the size and alias caps, so the node budget must refuse it.
        assert!(bomb.len() < MAX_YAML_READ_SIZE, "inside the size cap");
        assert!(
            alias_events(&bomb) <= MAX_YAML_ALIASES,
            "inside the alias cap"
        );
        std::fs::write(&path, &bomb).unwrap();

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path)
            .await
            .expect_err("an amplifying document must be refused");
        let elapsed = started.elapsed();

        // Refused before `serde_yaml_ng` builds anything: 7.5 s unguarded.
        //
        // The bound is seconds rather than microseconds because the exact node
        // count is a second O(input) pass, and this document is 974 KB of
        // 60,000 keys — the pre-pass alone measures ~310 ms in debug and
        // ~48 ms in release on 1 MiB. That cost is the price of exactness; the
        // property under test is that it is an order of magnitude below the
        // unguarded parse, not a specific millisecond figure.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "rejection took {elapsed:?}; it should refuse this without expanding"
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

            // Refused in the event walk before the value parse, by either the
            // alias cap or the node budget: against 7 s and 104 s unguarded.
            assert!(
                elapsed < std::time::Duration::from_millis(200),
                "w={w} took {elapsed:?}"
            );
            let message = format!("{err}");
            assert!(
                message.contains("expands to at least")
                    || message.contains("YAML aliases, over the limit"),
                "w={w} must be refused by a pre-parse guard: {message}"
            );
        }
    }

    #[tokio::test]
    async fn an_ordinary_document_is_nowhere_near_any_limit() {
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
            elapsed < std::time::Duration::from_secs(2),
            "an alias-free 20k-key document took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_nested_alias_bomb_is_refused_by_the_node_count() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        let path = temp.path().join("bomb.yml");
        let bomb = alias_bomb();
        std::fs::write(&path, &bomb).unwrap();

        // Inside the count cap, so the exact node count is what stops it.
        // `serde_yaml_ng` would reject it eventually through its own
        // repetition limit, but only after about seven seconds; this test used
        // to assert the 2 s deadline, which was removed because it could not
        // stop the work it was timing (security review C0).
        assert!(alias_events(&bomb) <= MAX_YAML_ALIASES);

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path)
            .await
            .expect_err("an alias bomb must be refused");
        let elapsed = started.elapsed();

        // Refused before the parser runs, so this is microseconds rather than
        // the seven seconds serde_yaml_ng's own repetition limit would take.
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "the caller waited {elapsed:?}"
        );
        let message = format!("{err}");
        assert!(
            message.contains("bomb.yml"),
            "must name the file: {message}"
        );
        assert!(
            message.contains("expands to at least")
                || message.contains("inside its own definition"),
            "must say why: {message}"
        );
    }

    #[tokio::test]
    async fn too_many_aliases_is_refused_before_the_value_parse() {
        // This document is cheap to expand: only the alias count rejects it,
        // during the event walk and before the value parse.
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
            "the count cap must stop before the value parse"
        );
        let message = format!("{err}");
        assert!(
            message.contains("many.yml"),
            "must name the file: {message}"
        );
        assert!(
            message.contains("YAML aliases, over the limit of 64"),
            "must say why: {message}"
        );
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
    fn alias_events_ignore_quotes_comments_and_multiplication() {
        assert_eq!(alias_events("anchor: &anchor 1\na: *anchor\n"), 1);
        assert_eq!(
            alias_events("x: &x 1\ny: &y 2\nz: &z 3\na: [*x, *y, *z]\n"),
            3
        );
        for doc in [
            "a: '*x'\n",
            "b: \"*y\"\n",
            "# *z\n",
            "c: 3 * 4\n",
            "d: '**'\n",
            "",
        ] {
            assert_eq!(alias_events(doc), 0, "{doc:?}");
        }
        assert_eq!(alias_events(&alias_bomb()), 30);
    }

    // V1-V3: reverting the scanner deletion must refuse these valid scalars.
    #[tokio::test]
    async fn markdown_in_a_literal_block_scalar_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("literal.yml");
        let body = (0..40)
            .map(|n| format!("- **Role {n}:** be *efficient*\n"))
            .collect::<String>();
        let doc = format!(
            "id: t\ndescription: |-\n{}",
            body.lines()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        );
        std::fs::write(&path, doc).unwrap();
        let value = read_yaml_value(&path).await.unwrap().expect("present");
        assert_eq!(value["description"].as_str(), Some(body.trim_end()));
    }

    #[tokio::test]
    async fn markdown_in_a_folded_block_scalar_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folded.yml");
        let lines = (0..40)
            .map(|n| format!("- **Role {n}:** be *efficient*"))
            .collect::<Vec<_>>();
        let doc = format!(
            "title: \"it's **done**\"\nresult: >-\n{}",
            lines
                .iter()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        );
        std::fs::write(&path, doc).unwrap();
        let value = read_yaml_value(&path).await.unwrap().expect("present");
        assert_eq!(value["result"].as_str(), Some(lines.join(" ").as_str()));
    }

    #[tokio::test]
    async fn markdown_inside_plain_scalars_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.yml");
        let doc = (0..70)
            .map(|n| format!("k{n}: see *efficient* code\n"))
            .collect::<String>();
        std::fs::write(&path, doc).unwrap();
        let value = read_yaml_value(&path).await.unwrap().expect("present");
        assert_eq!(value.as_mapping().unwrap().len(), 70);
        assert_eq!(value["k69"].as_str(), Some("see *efficient* code"));
    }

    #[tokio::test]
    async fn exactly_64_aliases_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boundary.yml");
        let doc = format!("a: &a 1\nl: [{}]\n", vec!["*a"; 64].join(","));
        std::fs::write(&path, &doc).unwrap();
        assert_eq!(alias_events(&doc), 64);
        let value = read_yaml_value(&path).await.unwrap().expect("present");
        assert_eq!(value["l"].as_sequence().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn markdown_and_anchor_like_strings_round_trip_without_aliases() {
        #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
        struct Text {
            document: String,
            role: String,
            alias: String,
            anchor: String,
        }

        let original = Text {
            document: (0..200)
                .map(|n| format!("- **Role {n}:** be *efficient*; it's \"done\" # &anchor <<\n"))
                .collect(),
            role: "**Role:** x".into(),
            alias: "*x".into(),
            anchor: "&anchor".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("round-trip.yml");
        write_yaml(&path, &original).await.unwrap();
        assert_eq!(alias_events(&std::fs::read_to_string(&path).unwrap()), 0);
        assert_eq!(read_yaml::<Text>(&path).await.unwrap(), Some(original));
    }

    // X2-X3: removing the event counter must allow these cheap expansions.
    #[tokio::test]
    async fn an_apostrophe_cannot_hide_200_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apostrophe.yml");
        let doc = format!(
            "note: it's fine\na: &a [1,2,3]\nl: [{}]\n",
            vec!["*a"; 200].join(",")
        );
        std::fs::write(&path, doc).unwrap();
        let err = read_yaml_value(&path)
            .await
            .expect_err("over the alias cap");
        let message = err.to_string();
        assert!(
            message.contains("YAML aliases, over the limit of 64"),
            "{message}"
        );
        assert!(message.contains(path.to_str().unwrap()), "{message}");
    }

    #[tokio::test]
    async fn alias_65_is_refused_before_a_large_suffix_is_walked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suffix.yml");
        let mut doc = format!("a: &a 1\nl: [{}]\n", vec!["*a"; 65].join(","));
        for n in 0..30_000 {
            doc.push_str(&format!("k{n}: a plain scalar value here\n"));
        }
        assert!(doc.len() > 900_000 && doc.len() < MAX_YAML_READ_SIZE);
        std::fs::write(&path, &doc).unwrap();
        let started = std::time::Instant::now();
        let err = read_yaml_value(&path)
            .await
            .expect_err("over the alias cap");
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "{elapsed:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("YAML aliases, over the limit of 64"),
            "{message}"
        );
        assert!(message.contains(path.to_str().unwrap()), "{message}");
        // An invalid tail must also remain unseen: this proves early stopping
        // without relying solely on the timing assertion above.
        doc.push_str("invalid: [\n");
        let err = expanded_node_count(&path, &doc).expect_err("stop at alias 65");
        assert!(
            err.to_string()
                .contains("YAML aliases, over the limit of 64")
        );
    }

    #[tokio::test]
    async fn an_unknown_alias_is_refused_as_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown.yml");
        std::fs::write(&path, "a: *nope\n").unwrap();
        let message = read_yaml_value(&path)
            .await
            .expect_err("unknown anchor")
            .to_string();
        assert!(message.contains("not valid YAML"), "{message}");
        assert!(message.contains(path.to_str().unwrap()), "{message}");
    }

    #[test]
    fn the_alias_cap_counts_across_documents() {
        let doc = format!(
            "a: &a 1\nl: [{}]\n---\nb: &b 2\nl: [{}]\n",
            vec!["*a"; 32].join(","),
            vec!["*b"; 33].join(",")
        );
        let err = expanded_node_count(Path::new("stream.yml"), &doc).expect_err("65 across stream");
        assert!(
            err.to_string()
                .contains("YAML aliases, over the limit of 64")
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

#[cfg(test)]
mod c0_rereview {
    use super::*;

    /// gate-security-rereview §1 `shapeD_6000_30`: a 6000-element leaf, then
    /// 30 levels of `[*prev, *prev]`. 60 aliases, ~84 KB, estimates at 4.93 MiB
    /// under the old heuristic — and took `orch status` to 19.3 GiB RSS in 28 s.
    fn shape_d(leaf: usize, depth: usize) -> String {
        let mut s = String::from("b0: &b0 [");
        for i in 0..leaf {
            if i > 0 {
                s.push(',');
            }
            s.push_str("\"yyyyyyyyyy\"");
        }
        s.push_str("]\n");
        for level in 1..=depth {
            s.push_str(&format!(
                "b{level}: &b{level} [*b{}, *b{}]\n",
                level - 1,
                level - 1
            ));
        }
        s
    }

    #[tokio::test]
    async fn shape_d_is_refused_in_microseconds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shapeD.yml");
        let doc = shape_d(6000, 30);
        println!(
            "shapeD: {} bytes, {} aliases",
            doc.len(),
            alias_events(&doc)
        );
        tokio::fs::write(&path, &doc).await.unwrap();

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path).await.expect_err("must be refused");
        let elapsed = started.elapsed();
        println!("refused in {elapsed:?}: {err}");
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "took {elapsed:?}"
        );
        assert!(format!("{err}").contains("expands to at least"), "{err}");
    }

    /// Trap 1 (overflow), and what measuring it actually showed.
    ///
    /// The requirement was that counts use checked arithmetic so a wrapped
    /// value cannot sail through as a small number. They do. But the probe
    /// that was supposed to demonstrate it cannot: **u64 overflow is
    /// unreachable**, because the budget is tested at every accumulation and
    /// 200,001 is reached about forty doublings before 2^64 is. My first
    /// version of this test "passed" via the alias cap at 192 aliases, which
    /// proved nothing about overflow at all.
    ///
    /// So this asserts the reachable property: tripling for 40 levels stays
    /// *under* the alias cap (120 aliases is over 64, so use 2 children and 32
    /// levels — 64 aliases, exactly at the cap) and is still refused by the
    /// node budget, in microseconds. The checked arithmetic stays because it
    /// is what keeps overflow unreachable if the budget is ever raised.
    #[tokio::test]
    async fn deep_nesting_inside_the_alias_cap_is_refused_by_the_node_budget() {
        let mut doc = String::from("c0: &c0 [1]\n");
        for level in 1..=32 {
            doc.push_str(&format!(
                "c{level}: &c{level} [*c{}, *c{}]\n",
                level - 1,
                level - 1
            ));
        }
        assert_eq!(alias_events(&doc), 64, "exactly at the alias cap");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep.yml");
        tokio::fs::write(&path, &doc).await.unwrap();

        let started = std::time::Instant::now();
        let err = read_yaml_value(&path).await.expect_err("must be refused");
        let elapsed = started.elapsed();
        println!("deep nesting ({} bytes) refused in {elapsed:?}", doc.len());
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "took {elapsed:?}"
        );
        assert!(
            format!("{err}").contains("expands to at least"),
            "must be the node budget, not the alias cap: {err}"
        );
    }

    #[tokio::test]
    async fn a_merge_key_bomb_is_counted_like_any_alias() {
        let mut doc = String::from("big: &big\n");
        for i in 0..5000 {
            doc.push_str(&format!("  k{i}: v{i}\n"));
        }
        for n in 0..60 {
            doc.push_str(&format!("use{n}:\n  <<: *big\n"));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merge.yml");
        tokio::fs::write(&path, &doc).await.unwrap();
        let err = read_yaml_value(&path).await.expect_err("must be refused");
        println!("merge-key bomb: {err}");
        assert!(format!("{err}").contains("expands to at least"), "{err}");
    }

    #[tokio::test]
    async fn anchor_redefinition_counts_against_the_definition_in_scope() {
        // Small defined first, then large: the aliases after the redefinition
        // must be counted against the LARGE one.
        let mut doc = String::from("a: &x [1]\nuse_small: *x\nbig: &x [");
        for i in 0..5000 {
            if i > 0 {
                doc.push(',');
            }
            doc.push_str("\"z\"");
        }
        doc.push_str("]\n");
        for n in 0..60 {
            doc.push_str(&format!("u{n}: *x\n"));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("redef.yml");
        tokio::fs::write(&path, &doc).await.unwrap();
        let err = read_yaml_value(&path)
            .await
            .expect_err("large-after-small must be refused");
        assert!(format!("{err}").contains("expands to at least"), "{err}");

        // And the reverse: large first, redefined small, then aliased. Those
        // aliases are cheap, so the document must still PARSE.
        let mut ok = String::from("big: &y [");
        for i in 0..200 {
            if i > 0 {
                ok.push(',');
            }
            ok.push_str("\"z\"");
        }
        ok.push_str("]\nsmall: &y [1]\n");
        for n in 0..60 {
            ok.push_str(&format!("u{n}: *y\n"));
        }
        let path2 = dir.path().join("redef2.yml");
        tokio::fs::write(&path2, &ok).await.unwrap();
        let value = read_yaml_value(&path2)
            .await
            .expect("small-after-large must parse");
        assert!(value.is_some());
    }

    #[tokio::test]
    async fn a_realistic_merge_key_config_still_parses() {
        let doc = "\
defaults: &defaults
  adapter: claude
  approval_policy: suggest
  max_turns: 40
agents:
  nav:
    <<: *defaults
    name: nav
  review:
    <<: *defaults
    approval_policy: never
";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yml");
        tokio::fs::write(&path, doc).await.unwrap();
        let value = read_yaml_value(&path).await.unwrap().expect("must parse");
        // apply_merge ran: the merged key is visible on the child.
        let adapter = value["agents"]["review"]["adapter"].as_str();
        assert_eq!(
            adapter,
            Some("claude"),
            "merge keys must still resolve: {value:?}"
        );
        assert_eq!(
            value["agents"]["review"]["approval_policy"].as_str(),
            Some("never")
        );
    }

    #[tokio::test]
    async fn a_recursive_alias_is_refused_rather_than_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cycle.yml");
        tokio::fs::write(&path, "a: &r [*r]\n").await.unwrap();
        let err = read_yaml_value(&path)
            .await
            .expect_err("a cycle must be refused");
        println!("cycle: {err}");
        assert!(
            format!("{err}").contains("inside its own definition"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod worst_case_after_prepass {
    /// What does the exact pre-pass cost on documents that are *legitimate*?
    /// It is a second O(input) pass over the bytes, so it is not free.
    #[tokio::test]
    #[ignore = "measurement, not an assertion"]
    async fn prepass_overhead() {
        for (label, doc) in [
            (
                "typical config (1 KB)",
                "project:\n  name: p\nscheduling:\n  max_concurrent_agents: 2\n".repeat(12),
            ),
            (
                "large agent (20 KB)",
                format!("role: |\n{}", "  a line of role prompt\n".repeat(900)),
            ),
            ("20k keys (320 KB)", {
                let mut s = String::from("items:\n");
                for i in 0..20_000 {
                    s.push_str(&format!("  - k{i}: v{i}\n"));
                }
                s
            }),
            ("1 MiB", {
                let mut s = String::from("items:\n");
                while s.len() < 1_000_000 {
                    s.push_str("  - keyX: valueY\n");
                }
                s
            }),
        ] {
            let t = std::time::Instant::now();
            let nodes = expanded_node_count(std::path::Path::new("x.yml"), &doc).unwrap_or(0);
            let prepass = t.elapsed();
            let t2 = std::time::Instant::now();
            let _ = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&doc);
            let parse = t2.elapsed();
            println!(
                "  {label:24} {:>8} B  nodes {nodes:>7}  prepass {prepass:>10.1?}  parse {parse:>10.1?}",
                doc.len()
            );
        }
    }

    use super::*;

    /// How slow can a document be that passes all three pre-parse limits?
    /// Ceiling: 1 MiB, <=64 aliases, <=200,000 expanded nodes.
    #[tokio::test]
    #[ignore = "measurement, not an assertion"]
    async fn measure() {
        let dir = tempfile::tempdir().unwrap();

        // (a) Maximum expansion: one anchor of ~3,100 nodes aliased 64 times
        // lands just under 200,000 expanded nodes.
        let mut s = String::from("a: &a [");
        for i in 0..3_100 {
            if i > 0 {
                s.push(',');
            }
            s.push_str("\"xxxxxxxx\"");
        }
        s.push_str("]\n");
        for n in 0..63 {
            s.push_str(&format!("u{n}: *a\n"));
        }

        // (b) Maximum length: just under 1 MiB of distinct scalars, no aliases.
        let mut flat = String::from("items:\n");
        while flat.len() < 1_000_000 {
            flat.push_str("  - keyX: valueY\n");
        }

        for (label, doc) in [("max expansion", s), ("max length, no aliases", flat)] {
            let path = dir.path().join("w.yml");
            tokio::fs::write(&path, &doc).await.unwrap();
            let t = std::time::Instant::now();
            let result = read_yaml_value(&path).await;
            println!(
                "  {label:24} {} bytes, {} aliases -> {:?} in {:?}",
                doc.len(),
                alias_events(&doc),
                result
                    .as_ref()
                    .map(|v| v.is_some())
                    .map_err(|e| format!("{e}").chars().take(40).collect::<String>()),
                t.elapsed()
            );
        }
    }
}
