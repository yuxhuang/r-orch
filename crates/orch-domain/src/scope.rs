//! Scope overlap detection — pure functions, no side effects
//! (`src/domain/scope.ts`).
//!
//! A scope is an array of glob patterns (e.g. `["src/auth/**"]`). Two tasks
//! overlap when any pattern pair shares a common path prefix. The matching is
//! deliberately **raw string-prefix logic**, not real glob matching
//! (PARITY.md §7.11): conservative, so it may produce false positives but
//! never false negatives.

use std::collections::{HashMap, HashSet};

/// Whether two scope arrays have at least one overlapping pattern pair.
/// Tasks with no scope never overlap — they are unconstrained by convention.
pub fn scopes_overlap(a: Option<&[String]>, b: Option<&[String]>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.iter()
        .any(|pa| b.iter().any(|pb| patterns_overlap(pa, pb)))
}

/// Whether two glob patterns overlap, by comparing their base prefixes.
fn patterns_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }

    let a_base = pattern_base(a);
    let b_base = pattern_base(b);

    if a_base.starts_with(b_base) || b_base.starts_with(a_base) {
        return true;
    }

    // Sibling files in the same directory overlap (e.g. src/auth/login.ts and
    // src/auth/logout.ts). Only compare dirname when both bases are file-like.
    if !a_base.ends_with('/') && !b_base.ends_with('/') {
        let a_dir = dirname(a_base);
        let b_dir = dirname(b_base);
        return a_dir == b_dir && a_dir != ".";
    }

    false
}

/// Everything before the first `*`, or the whole pattern when it has none.
fn pattern_base(pattern: &str) -> &str {
    match pattern.find('*') {
        Some(at) => &pattern[..at],
        None => pattern,
    }
}

/// Pre-computed pattern info, as used while adding patterns to a [`ScopeIndex`].
struct PatternInfo<'p> {
    base: &'p str,
    is_file: bool,
}

fn compute_pattern_info(pattern: &str) -> PatternInfo<'_> {
    let base = pattern_base(pattern);
    PatternInfo {
        base,
        is_file: !base.ends_with('/'),
    }
}

/// Node's POSIX `path.dirname`, which the TS original uses. Reimplemented
/// rather than approximated because its edge cases are load-bearing: `""` and
/// a bare filename both yield `"."`, which is the value the sibling-directory
/// rule explicitly excludes.
fn dirname(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".";
    }

    let has_root = bytes[0] == b'/';
    let mut end: Option<usize> = None;
    let mut matched_slash = true;

    for i in (1..bytes.len()).rev() {
        if bytes[i] == b'/' {
            if !matched_slash {
                end = Some(i);
                break;
            }
        } else {
            matched_slash = false;
        }
    }

    match end {
        None if has_root => "/",
        None => ".",
        Some(1) if has_root => "//",
        Some(end) => &path[..end],
    }
}

/// A radix trie node. Edge labels are UTF-16 code units, exactly like the TS
/// original, so a split can land between the two halves of a surrogate pair.
/// That is deliberate: the overlap contract is defined in terms of JavaScript
/// string prefixes.
struct RadixTrieNode {
    /// The non-empty UTF-16 edge label from the parent to this node.
    label: Vec<u16>,
    /// Keyed by the label's first code unit, mirroring the TS sparse array.
    children: HashMap<u16, RadixTrieNode>,
    /// Whether an indexed base ends exactly at this node.
    terminal: bool,
}

impl RadixTrieNode {
    fn new(label: Vec<u16>, terminal: bool) -> Self {
        RadixTrieNode {
            label,
            children: HashMap::new(),
            terminal,
        }
    }
}

/// Mutable compressed radix trie for string-prefix queries.
struct PrefixTrie {
    root: RadixTrieNode,
}

/// Length of the common prefix of two code-unit slices.
fn common_prefix_len(a: &[u16], b: &[u16]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

impl PrefixTrie {
    fn new() -> Self {
        PrefixTrie {
            root: RadixTrieNode::new(Vec::new(), false),
        }
    }

    fn add(&mut self, value: &[u16]) {
        if value.is_empty() {
            self.root.terminal = true;
            return;
        }

        /// What the next step of the walk does, decided before taking the
        /// mutable borrow the step needs.
        enum Step {
            /// No edge for this code unit — hang the whole remainder off it.
            Insert,
            /// The edge label is fully consumed; descend past `len` units.
            Descend(usize),
            /// The edge and the value diverge after `len` common units.
            Split(usize),
        }

        let mut parent = &mut self.root;
        let mut offset = 0usize;

        while offset < value.len() {
            let key = value[offset];
            let step = match parent.children.get(&key) {
                None => Step::Insert,
                Some(child) => {
                    let common = common_prefix_len(&child.label, &value[offset..]);
                    if common == child.label.len() {
                        Step::Descend(common)
                    } else {
                        Step::Split(common)
                    }
                }
            };

            match step {
                Step::Insert => {
                    parent
                        .children
                        .insert(key, RadixTrieNode::new(value[offset..].to_vec(), true));
                    return;
                }
                Step::Descend(common) => {
                    offset += common;
                    let child = parent.children.get_mut(&key).expect("edge exists");
                    if offset == value.len() {
                        child.terminal = true;
                        return;
                    }
                    parent = child;
                }
                Step::Split(common) => {
                    // Keep the common prefix as a branch node and retain both
                    // suffixes below it.
                    let mut existing = parent.children.remove(&key).expect("edge exists");
                    let mut split = RadixTrieNode::new(existing.label[..common].to_vec(), false);
                    existing.label = existing.label[common..].to_vec();
                    let existing_key = existing.label[0];
                    split.children.insert(existing_key, existing);

                    let new_offset = offset + common;
                    if new_offset == value.len() {
                        split.terminal = true;
                    } else {
                        let new_child = RadixTrieNode::new(value[new_offset..].to_vec(), true);
                        split.children.insert(new_child.label[0], new_child);
                    }
                    parent.children.insert(key, split);
                    return;
                }
            }
        }
    }

    /// Whether any indexed base has a prefix relationship with `value` — in
    /// either direction.
    fn overlaps(&self, value: &[u16]) -> bool {
        let mut node = &self.root;

        // An empty indexed base is a prefix of every query; conversely every
        // non-empty indexed base has an empty query as its prefix.
        if node.terminal || value.is_empty() {
            return node.terminal || !node.children.is_empty();
        }

        let mut offset = 0usize;
        while offset < value.len() {
            let Some(child) = node.children.get(&value[offset]) else {
                return false;
            };

            let remaining = value.len() - offset;
            let common = common_prefix_len(&child.label, &value[offset..]);

            // The query ends in the middle of an edge, so it is a prefix of
            // the indexed base represented by that edge.
            if common == remaining {
                return true;
            }
            if common != child.label.len() {
                return false;
            }

            offset += common;
            node = child;
            if node.terminal {
                return true;
            }
        }

        // The query ends exactly at a trie node: it is either an indexed base
        // itself or a prefix of one of the node's descendants.
        true
    }
}

/// Pre-computed scope index for batch overlap checking.
///
/// A pattern pair overlaps when either base is a string prefix of the other,
/// or when both are file-like patterns in the same non-root directory. The
/// trie stores each unique prefix once and is intentionally mutable, so
/// `add` keeps the same query cost for dynamic workloads (the dispatch loop
/// adds approved candidates as it goes).
pub struct ScopeIndex {
    base_trie: PrefixTrie,
    /// Directories containing indexed file-like patterns, excluding `.`.
    file_dirs: HashSet<String>,
    pattern_count: usize,
}

impl ScopeIndex {
    pub fn new<'s, I>(scopes: I) -> Self
    where
        I: IntoIterator<Item = Option<&'s [String]>>,
    {
        let mut index = ScopeIndex {
            base_trie: PrefixTrie::new(),
            file_dirs: HashSet::new(),
            pattern_count: 0,
        };
        for scope in scopes {
            index.add(scope);
        }
        index
    }

    /// Whether the given scope overlaps any pattern in the index.
    pub fn overlaps_any(&self, scope: Option<&[String]>) -> bool {
        let Some(scope) = scope else { return false };
        if scope.is_empty() || self.pattern_count == 0 {
            return false;
        }
        scope.iter().any(|raw| {
            let info = compute_pattern_info(raw);
            let base: Vec<u16> = info.base.encode_utf16().collect();
            self.base_trie.overlaps(&base)
                || (info.is_file && self.has_sibling_directory(info.base))
        })
    }

    /// Add patterns to the index (e.g. from an approved dispatch candidate).
    pub fn add(&mut self, scope: Option<&[String]>) {
        let Some(scope) = scope else { return };
        if scope.is_empty() {
            return;
        }
        for pattern in scope {
            let info = compute_pattern_info(pattern);
            self.pattern_count += 1;
            self.base_trie
                .add(&info.base.encode_utf16().collect::<Vec<_>>());
            if info.is_file {
                let dir = dirname(info.base);
                if dir != "." {
                    self.file_dirs.insert(dir.to_owned());
                }
            }
        }
    }

    /// Total pattern count, duplicates included.
    pub fn len(&self) -> usize {
        self.pattern_count
    }

    pub fn is_empty(&self) -> bool {
        self.pattern_count == 0
    }

    /// Computed only after the prefix check misses, since most patterns are globs.
    fn has_sibling_directory(&self, base: &str) -> bool {
        let dir = dirname(base);
        dir != "." && self.file_dirs.contains(dir)
    }
}

impl Default for ScopeIndex {
    fn default() -> Self {
        ScopeIndex::new(std::iter::empty::<Option<&[String]>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|p| (*p).to_owned()).collect()
    }

    /// `ScopeIndex::new` over a list of scopes, like the TS constructor.
    fn index(scopes: &[Option<Vec<String>>]) -> ScopeIndex {
        ScopeIndex::new(scopes.iter().map(|o| o.as_deref()))
    }

    // ── scopesOverlap ──────────────────────────────────────────────────────

    #[test]
    fn false_when_either_scope_is_empty_or_absent() {
        assert!(!scopes_overlap(None, Some(&s(&["src/**"]))));
        assert!(!scopes_overlap(Some(&s(&["src/**"])), None));
        assert!(!scopes_overlap(Some(&[]), Some(&s(&["src/**"]))));
        assert!(!scopes_overlap(Some(&s(&["src/**"])), Some(&[])));
    }

    #[test]
    fn true_for_identical_patterns() {
        assert!(scopes_overlap(
            Some(&s(&["src/auth/**"])),
            Some(&s(&["src/auth/**"]))
        ));
    }

    #[test]
    fn true_when_one_prefix_contains_the_other() {
        assert!(scopes_overlap(
            Some(&s(&["src/**"])),
            Some(&s(&["src/auth/**"]))
        ));
        assert!(scopes_overlap(
            Some(&s(&["src/auth/**"])),
            Some(&s(&["src/**"]))
        ));
    }

    #[test]
    fn true_for_sibling_files_in_the_same_directory() {
        assert!(scopes_overlap(
            Some(&s(&["src/auth/login.ts"])),
            Some(&s(&["src/auth/logout.ts"]))
        ));
    }

    #[test]
    fn true_for_a_sibling_glob_and_file_in_the_same_directory() {
        assert!(scopes_overlap(
            Some(&s(&["src/auth/*.ts"])),
            Some(&s(&["src/auth/logout.ts"]))
        ));
    }

    #[test]
    fn false_for_files_in_different_directories() {
        assert!(!scopes_overlap(
            Some(&s(&["src/auth/login.ts"])),
            Some(&s(&["src/db/pool.ts"]))
        ));
    }

    #[test]
    fn false_for_disjoint_subtrees() {
        assert!(!scopes_overlap(
            Some(&s(&["src/auth/**"])),
            Some(&s(&["src/db/**"]))
        ));
    }

    #[test]
    fn true_when_any_pair_overlaps_across_arrays() {
        assert!(scopes_overlap(
            Some(&s(&["src/db/**", "src/auth/**"])),
            Some(&s(&["src/auth/login.ts"]))
        ));
    }

    #[test]
    fn root_level_files_are_not_siblings() {
        // dirname("login.ts") === "." , which the rule excludes.
        assert!(!scopes_overlap(
            Some(&s(&["login.ts"])),
            Some(&s(&["logout.ts"]))
        ));
    }

    // ── ScopeIndex ─────────────────────────────────────────────────────────

    #[test]
    fn overlaps_any_false_on_an_empty_index() {
        let idx = index(&[]);
        assert!(!idx.overlaps_any(Some(&s(&["src/auth/**"]))));
    }

    #[test]
    fn overlaps_any_false_when_the_scope_is_absent() {
        let idx = index(&[Some(s(&["src/auth/**"]))]);
        assert!(!idx.overlaps_any(None));
    }

    #[test]
    fn overlaps_any_false_when_the_scope_is_empty() {
        let idx = index(&[Some(s(&["src/auth/**"]))]);
        assert!(!idx.overlaps_any(Some(&[])));
    }

    #[test]
    fn overlaps_any_detects_prefix_overlap() {
        let idx = index(&[Some(s(&["src/auth/**"]))]);
        assert!(idx.overlaps_any(Some(&s(&["src/auth/login.ts"]))));
    }

    #[test]
    fn overlaps_any_detects_identical_pattern_overlap() {
        let idx = index(&[Some(s(&["src/db/**"]))]);
        assert!(idx.overlaps_any(Some(&s(&["src/db/**"]))));
    }

    #[test]
    fn overlaps_any_false_for_disjoint_scopes() {
        let idx = index(&[Some(s(&["src/auth/**"]))]);
        assert!(!idx.overlaps_any(Some(&s(&["src/db/**"]))));
    }

    #[test]
    fn overlaps_any_detects_sibling_file_overlap() {
        let idx = index(&[Some(s(&["src/auth/login.ts"]))]);
        assert!(idx.overlaps_any(Some(&s(&["src/auth/logout.ts"]))));
    }

    #[test]
    fn does_not_treat_files_in_the_root_directory_as_siblings() {
        let idx = index(&[Some(s(&["login.ts"]))]);
        assert!(!idx.overlaps_any(Some(&s(&["logout.ts"]))));
    }

    #[test]
    fn preserves_string_prefix_semantics_without_a_path_boundary_check() {
        let idx = index(&[Some(s(&["src/auth"]))]);
        assert!(idx.overlaps_any(Some(&s(&["src/authorisation/**"]))));
    }

    #[test]
    fn matches_patterns_with_an_empty_base_prefix() {
        let idx = index(&[Some(s(&["*"]))]);
        assert!(idx.overlaps_any(Some(&s(&["services/api/**"]))));
    }

    #[test]
    fn matches_a_parent_added_after_a_longer_indexed_base() {
        let mut idx = index(&[]);
        idx.add(Some(&s(&["src/auth/private/credentials/**"])));
        assert!(idx.overlaps_any(Some(&s(&["src/auth/**"]))));
    }

    #[test]
    fn matches_a_longer_base_added_after_its_parent() {
        let mut idx = index(&[]);
        idx.add(Some(&s(&["src/auth/**"])));
        idx.add(Some(&s(&["src/auth/private/credentials/**"])));
        assert!(idx.overlaps_any(Some(&s(&["src/auth/private/credentials/key.json"]))));
    }

    #[test]
    fn keeps_duplicate_bases_observable_through_size_and_overlap() {
        let mut idx = index(&[Some(s(&["src/auth/**"]))]);
        idx.add(Some(&s(&["src/auth/**"])));
        assert_eq!(idx.len(), 2);
        assert!(idx.overlaps_any(Some(&s(&["src/auth/login.ts"]))));
    }

    #[test]
    fn handles_a_radix_split_between_surrogate_pair_code_units() {
        // U+1F600 and U+1F601 share the high surrogate 0xD83D and differ only
        // in the low one, so the split lands *inside* a surrogate pair — the
        // case the TS comment calls out. (The TS test uses lone surrogates,
        // which a Rust `String` cannot represent; these two emoji exercise the
        // same code path with valid UTF-8.)
        let grin = '\u{1F600}';
        let beam = '\u{1F601}';
        assert_eq!(grin.encode_utf16(&mut [0u16; 2]).len(), 2);

        let mut idx = index(&[Some(s(&[&format!("src/{grin}/one/**")]))]);
        assert!(!idx.overlaps_any(Some(&s(&[&format!("src/{beam}/two/**")]))));
        idx.add(Some(&s(&[&format!("src/{beam}/three/**")])));
        assert!(idx.overlaps_any(Some(&s(&[&format!("src/{beam}/three/file.ts")]))));
        // The first base is still reachable after the split.
        assert!(idx.overlaps_any(Some(&s(&[&format!("src/{grin}/one/x.ts")]))));
    }

    #[test]
    fn updates_the_sibling_directory_index_when_patterns_are_added() {
        let mut idx = index(&[]);
        assert!(!idx.overlaps_any(Some(&s(&["src/auth/logout.ts"]))));

        idx.add(Some(&s(&["src/auth/login.ts"])));
        assert!(idx.overlaps_any(Some(&s(&["src/auth/logout.ts"]))));
        assert!(!idx.overlaps_any(Some(&s(&["src/db/pool.ts"]))));
    }

    #[test]
    fn constructor_skips_absent_and_empty_scopes() {
        let idx = index(&[None, Some(vec![]), Some(s(&["src/auth/**"]))]);
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn constructor_collects_all_patterns_from_multiple_scopes() {
        let idx = index(&[
            Some(s(&["src/auth/**", "src/db/**"])),
            Some(s(&["src/api/**"])),
        ]);
        assert_eq!(idx.len(), 3);
    }

    #[test]
    fn add_makes_later_overlaps_any_aware_of_new_patterns() {
        let mut idx = index(&[Some(s(&["src/auth/**"]))]);
        assert!(!idx.overlaps_any(Some(&s(&["src/db/**"]))));

        idx.add(Some(&s(&["src/db/**"])));
        assert!(idx.overlaps_any(Some(&s(&["src/db/pool.ts"]))));
    }

    #[test]
    fn add_with_absent_or_empty_scope_is_a_no_op() {
        let mut idx = index(&[]);
        idx.add(None);
        idx.add(Some(&[]));
        assert_eq!(idx.len(), 0);
        assert!(idx.is_empty());
    }

    #[test]
    fn size_reflects_the_total_pattern_count() {
        let mut idx = index(&[Some(s(&["a/**", "b/**"]))]);
        assert_eq!(idx.len(), 2);
        idx.add(Some(&s(&["c/**"])));
        assert_eq!(idx.len(), 3);
    }

    #[test]
    fn peer_candidate_scenario_approved_candidate_blocks_later_overlap() {
        // dispatchAll: in-progress scopes in the index, then approved candidates.
        let mut idx = index(&[Some(s(&["src/auth/**"]))]);

        assert!(!idx.overlaps_any(Some(&s(&["src/db/**"]))));
        idx.add(Some(&s(&["src/db/**"])));

        assert!(idx.overlaps_any(Some(&s(&["src/db/pool.ts"]))));
        assert!(!idx.overlaps_any(Some(&s(&["src/api/**"]))));
    }

    // ── V12: the index agrees with the pairwise function ───────────────────

    #[test]
    fn index_agrees_with_the_pairwise_function_over_a_fixed_corpus() {
        let corpus = s(&[
            "src/**",
            "src/auth/**",
            "src/auth",
            "src/authorisation/**",
            "src/auth/login.ts",
            "src/auth/logout.ts",
            "src/auth/*.ts",
            "src/db/**",
            "src/db/pool.ts",
            "login.ts",
            "logout.ts",
            "*",
            "",
            "a/b/c/d/e",
            "a/b/c/d/f",
            "/abs/path/x.ts",
            "/abs/path/y.ts",
            "/",
            "dir/",
            "dir/sub/",
            "src/\u{1F600}/one/**",
            "src/\u{1F601}/two/**",
        ]);

        for indexed in &corpus {
            let indexed_scope = vec![indexed.clone()];
            let idx = ScopeIndex::new([Some(indexed_scope.as_slice())]);
            for query in &corpus {
                let query_scope = vec![query.clone()];
                assert_eq!(
                    idx.overlaps_any(Some(&query_scope)),
                    scopes_overlap(Some(&indexed_scope), Some(&query_scope)),
                    "indexed={indexed:?} query={query:?}"
                );
            }
        }
    }

    #[test]
    fn a_multi_pattern_index_matches_the_or_of_the_pairwise_results() {
        let indexed = s(&["src/auth/**", "docs/readme.md", "lib/"]);
        let queries = [
            s(&["src/auth/login.ts"]),
            s(&["docs/changelog.md"]),
            s(&["lib/x"]),
            s(&["other/**"]),
            s(&["other/**", "src/auth/x"]),
        ];
        let idx = ScopeIndex::new([Some(indexed.as_slice())]);
        for query in &queries {
            assert_eq!(
                idx.overlaps_any(Some(query)),
                scopes_overlap(Some(&indexed), Some(query)),
                "query={query:?}"
            );
        }
    }

    // ── dirname ────────────────────────────────────────────────────────────

    #[test]
    fn dirname_matches_node_posix() {
        let cases: &[(&str, &str)] = &[
            ("", "."),
            (".", "."),
            ("foo", "."),
            ("src/auth", "src"),
            ("src/auth/", "src"),
            ("a//b", "a/"),
            ("/foo", "/"),
            ("/", "/"),
            ("//", "/"),
            ("//a", "//"),
            ("//a/b", "//a"),
            ("src/auth/login.ts", "src/auth"),
            ("a/", "."),
            ("./x", "."),
            ("/a", "/"),
        ];
        for (input, expected) in cases {
            assert_eq!(dirname(input), *expected, "dirname({input:?})");
        }
    }
}
