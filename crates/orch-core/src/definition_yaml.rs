//! Guarded YAML byte parsing; paths label errors and are never opened.

use crate::error::{Error, Result};
use std::path::Path;

/// Largest YAML document `read_yaml_value` will hand to the parser.
///
/// Every file ORCH writes is orders of magnitude smaller; the cap exists so a
/// hand-placed file cannot make the parser the expensive part.
pub const MAX_YAML_READ_SIZE: usize = 1024 * 1024;

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
pub const MAX_YAML_ALIASES: usize = 64;

/// Parse an in-memory YAML document with the same size, alias and expanded-node
/// guards as file reads, resolving merge keys. `path` labels errors; no file is read.
pub async fn parse_yaml_bytes(path: &Path, content: &[u8]) -> Result<serde_yaml_ng::Value> {
    // Two steps before the value parse: cap the input size, then walk parser
    // events to enforce both the expanded-node budget and the exact alias cap.
    if content.len() > MAX_YAML_READ_SIZE {
        return Err(Error::other(format!(
            "{} is {} bytes, over the {MAX_YAML_READ_SIZE}-byte YAML limit",
            path.display(),
            content.len()
        )));
    }
    let content = std::str::from_utf8(content)
        .map_err(|err| {
            Error::io(
                format!("read {}", path.display()),
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, err),
            )
        })?
        .to_owned();
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
    // YAML 1.1 resolves `<<`; serde_yaml_ng does not until
    // asked. Unconditional: it is a no-op on every document
    // ORCH itself writes.
    value.apply_merge().map_err(|err| {
        Error::serde(format!("apply merge keys in {}", path.display()), path, err)
    })?;
    Ok(value)
}

/// Strict definition documents add JSON compatibility to the legacy guarded
/// parser, without changing legacy config/entity reads.
pub async fn parse_definition_yaml_bytes(path: &Path, content: &[u8]) -> Result<serde_json::Value> {
    use saphyr_parser::{Event, Parser};
    let invalid = |reason: &str| Error::invalid_arguments(format!("{}: {reason}", path.display()));
    if content.len() > MAX_YAML_READ_SIZE {
        return Err(invalid("document exceeds 1 MiB"));
    }
    let text = std::str::from_utf8(content).map_err(|_| invalid("expected UTF-8"))?;
    let mut depth = 0usize;
    let mut documents = 0usize;
    for event in Parser::new_from_str(text) {
        let (event, _) = event.map_err(|_| invalid("invalid YAML document"))?;
        match event {
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return Err(invalid("expected exactly one YAML document"));
                }
            }
            Event::Scalar(_, _, _, Some(_)) => {
                return Err(invalid("explicit YAML tags are not allowed"));
            }
            Event::SequenceStart(_, tag) | Event::MappingStart(_, tag) => {
                if tag.is_some() {
                    return Err(invalid("explicit YAML tags are not allowed"));
                }
                depth += 1;
                if depth > 64 {
                    return Err(invalid("document depth exceeds 64"));
                }
            }
            Event::SequenceEnd | Event::MappingEnd => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let value = parse_yaml_bytes(path, content)
        .await
        .map_err(|_| invalid("invalid YAML (check duplicate keys and alias/node limits)"))?;
    fn convert(
        value: serde_yaml_ng::Value,
        depth: usize,
    ) -> std::result::Result<serde_json::Value, &'static str> {
        use serde_json::Value as Json;
        use serde_yaml_ng::Value as Yaml;
        if depth > 64 {
            return Err("document depth exceeds 64");
        }
        Ok(match value {
            Yaml::Null => Json::Null,
            Yaml::Bool(value) => Json::Bool(value),
            Yaml::String(value) => Json::String(value),
            Yaml::Number(value) => {
                if let Some(n) = value.as_i64() {
                    Json::from(n)
                } else if let Some(n) = value.as_u64() {
                    Json::from(n)
                } else {
                    Json::Number(
                        serde_json::Number::from_f64(value.as_f64().ok_or("invalid number")?)
                            .ok_or("nonfinite number")?,
                    )
                }
            }
            Yaml::Sequence(items) => Json::Array(
                items
                    .into_iter()
                    .map(|v| convert(v, depth + 1))
                    .collect::<std::result::Result<_, _>>()?,
            ),
            Yaml::Mapping(map) => {
                let mut result = serde_json::Map::new();
                for (key, value) in map {
                    let Yaml::String(key) = key else {
                        return Err("mapping keys must be strings");
                    };
                    result.insert(key, convert(value, depth + 1)?);
                }
                Json::Object(result)
            }
            Yaml::Tagged(_) => return Err("explicit YAML tags are not allowed"),
        })
    }
    convert(value, 0).map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn alias_events(content: &str) -> usize {
        use saphyr_parser::{Event, Parser};

        Parser::new_from_str(content)
            .map(|event| event.expect("valid YAML").0)
            .filter(|event| matches!(event, Event::Alias(_)))
            .count()
    }
    #[tokio::test]
    async fn yaml_bytes_resolve_merge_keys_without_reading_a_file() {
        let path = Path::new("buffer.yml");
        let value = parse_yaml_bytes(path, b"defaults: &d {a: 1}\nchild: {<<: *d, b: 2}\n")
            .await
            .unwrap();
        assert_eq!(value["child"]["a"].as_u64(), Some(1));
        assert_eq!(value["child"]["b"].as_u64(), Some(2));
    }

    #[tokio::test]
    async fn yaml_bytes_reject_oversized_input_before_parsing() {
        let err = parse_yaml_bytes(Path::new("buffer.yml"), &vec![b'x'; MAX_YAML_READ_SIZE + 1])
            .await
            .expect_err("over the size cap");
        assert!(err.to_string().contains("1048576-byte YAML limit"), "{err}");
    }

    #[tokio::test]
    async fn yaml_bytes_reject_invalid_utf8_with_the_source_name() {
        let err = parse_yaml_bytes(Path::new("buffer.yml"), &[0xff])
            .await
            .expect_err("not UTF-8");
        assert!(err.to_string().contains("read buffer.yml"), "{err}");
    }

    #[tokio::test]
    async fn yaml_bytes_enforce_the_alias_cap() {
        let doc = format!(
            "a: &a 1\nlist:\n{}",
            "  - *a\n".repeat(MAX_YAML_ALIASES + 1)
        );
        let err = parse_yaml_bytes(Path::new("buffer.yml"), doc.as_bytes())
            .await
            .expect_err("over the alias cap");
        assert!(
            err.to_string()
                .contains("YAML aliases, over the limit of 64"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn yaml_bytes_enforce_the_expanded_node_budget() {
        // Just over the budget, so a break check remains safe without the guard.
        let doc = format!(
            "a: &a [{}]\nlist:\n{}",
            vec!["1"; 3500].join(","),
            "  - *a\n".repeat(60)
        );
        assert!(doc.len() < MAX_YAML_READ_SIZE);
        assert!(alias_events(&doc) <= MAX_YAML_ALIASES);
        let err = parse_yaml_bytes(Path::new("buffer.yml"), doc.as_bytes())
            .await
            .expect_err("over the node budget");
        assert!(err.to_string().contains("expands to at least"), "{err}");
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

    #[test]
    fn alias_65_is_refused_before_a_large_suffix_is_walked() {
        let path = Path::new("suffix.yml");
        let mut doc = format!("a: &a 1\nl: [{}]\n", vec!["*a"; 65].join(","));
        for n in 0..30_000 {
            doc.push_str(&format!("k{n}: a plain scalar value here\n"));
        }
        // An invalid tail must also remain unseen: this proves early stopping
        // without relying solely on the timing assertion above.
        doc.push_str("invalid: [\n");
        let err = expanded_node_count(path, &doc).expect_err("stop at alias 65");
        assert!(
            err.to_string()
                .contains("YAML aliases, over the limit of 64")
        );
    }
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
}
