//! The serving daemon must not gain another uncoordinated process builder.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// These constructions execute in other processes, or only in test binaries.
// Counts are deliberate: adding a second construction to an allowed file fails.
const EXCEPTIONS: &[(&str, usize, &str)] = &[
    (
        "orch-cli/src/spawn_subscriber_tests.rs",
        1,
        "cfg(test) fresh-process production subscriber fixture",
    ),
    (
        "orch-cli/src/commands/init.rs",
        2,
        "explicit initialization client; never serves a daemon",
    ),
    ("orch-cli/src/editor.rs", 1, "editor runs in the CLI client"),
    (
        "orch-daemon/src/detach.rs",
        1,
        "explicit starter re-executes before serving; lifecycle never calls it",
    ),
    (
        "orch-engine/src/review_runner.rs",
        1,
        "fallback runner; daemon injects SnapshotCommandRunner in lifecycle::Running::build",
    ),
    (
        "orch-infra/src/adapters/claude.rs",
        2,
        "cfg(test) child cleanup fixtures",
    ),
    (
        "orch-infra/src/adapters/util.rs",
        1,
        "cfg(test) ETXTBSY-safe version-probe script writer",
    ),
    (
        "orch-infra/src/storage/message.rs",
        1,
        "cfg(test) cross-process append fixture",
    ),
    (
        "orch-infra/src/storage/remote.rs",
        1,
        "cfg(test) process-liveness fixture; production hostname must be gated",
    ),
    (
        "orch-infra/src/workspace/tests.rs",
        1,
        "cfg(test) workspace git setup",
    ),
    (
        "orch-infra/src/workspace/test_support.rs",
        2,
        "cfg(test) scripted/real process fake",
    ),
    (
        "orch-engine/src/orchestrator/tests/resilience.rs",
        1,
        "cfg(test) process existence assertion",
    ),
    (
        "orch-engine/src/orchestrator/tests/remote.rs",
        1,
        "cfg(test) fake remote child",
    ),
    (
        "orch-peer/src/worker_env_tests.rs",
        1,
        "cfg(test) environment isolation subprocess",
    ),
];

fn sources(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, paths);
        } else if path.extension().is_some_and(|s| s == "rs") {
            paths.push(path);
        }
    }
}

// Tokenize only what this guard needs. Comments and string/character literals
// cannot invent a construction or unbalance a closure. The scoped fix permits
// no new parser dependency, so this guard uses only the tokens it checks.
fn tokens(source: &str) -> Vec<String> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Raw strings (also catches the r in br#"..."# on the next iteration).
        if b[i] == b'r' {
            let start = i;
            let mut quote = i + 1;
            while quote < b.len() && b[quote] == b'#' {
                quote += 1;
            }
            if quote < b.len() && b[quote] == b'"' {
                let end = format!("\"{}", "#".repeat(quote - start - 1));
                i = source[quote + 1..]
                    .find(&end)
                    .map_or(b.len(), |n| quote + 1 + n + end.len());
                continue;
            }
        }
        let character =
            b[i] == b'\'' && (b.get(i + 2) == Some(&b'\'') || b.get(i + 1) == Some(&b'\\'));
        if b[i] == b'"' || character {
            let quote = b[i];
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(source[start..i].into());
        } else {
            out.push(char::from(b[i]).to_string());
            i += 1;
        }
    }
    out
}

fn gated_sections(t: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    for i in 0..t.len().saturating_sub(4) {
        if t[i..i + 5] != ["with_spawn_gate", "(", "|", "|", "{"] {
            continue;
        }
        let mut depth = 1;
        let mut end = i + 5;
        while depth > 0 && end < t.len() {
            if t[end] == "{" {
                depth += 1;
            }
            if t[end] == "}" {
                depth -= 1;
            }
            end += 1;
        }
        assert_eq!(depth, 0, "unterminated spawn section");
        let body = &t[i + 5..end - 1];
        assert!(
            !body.iter().any(|s| s == "await"),
            "spawn gate held across await"
        );
        assert!(
            !body.windows(3).any(|w| w[0] == "."
                && ["output", "wait", "wait_with_output", "status"].contains(&w[1].as_str())
                && w[2] == "("),
            "spawn gate held across child wait"
        );
        ranges.push(i + 5..end - 1);
    }
    ranges
}

fn command_names(streams: &[&[String]]) -> BTreeSet<String> {
    let mut names = BTreeSet::from(["Command".to_owned(), "CommandBuilder".to_owned()]);
    loop {
        let before = names.len();
        for t in streams {
            for w in t.windows(3) {
                if names.contains(&w[0]) && w[1] == "as" {
                    names.insert(w[2].clone());
                }
            }
            for (i, w) in t.windows(3).enumerate() {
                if w[0] == "type" && w[2] == "=" {
                    let end = t[i..].iter().position(|s| s == ";").unwrap_or(t.len() - i) + i;
                    if t[i..end].iter().any(|s| names.contains(s)) {
                        names.insert(w[1].clone());
                    }
                }
            }
        }
        if names.len() == before {
            return names;
        }
    }
}

fn constructions(t: &[String], names: &BTreeSet<String>) -> Vec<usize> {
    // Also reject constructor function references passed to an indirect caller.
    t.windows(4)
        .enumerate()
        .filter_map(|(i, w)| {
            (names.contains(&w[0])
                && w[1] == ":"
                && w[2] == ":"
                && ["new", "default"].contains(&w[3].as_str()))
            .then_some(i)
        })
        .collect()
}

#[test]
fn daemon_linked_process_constructions_are_gated_or_explicitly_excluded() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            sources(&src, &mut paths);
        }
    }
    paths.sort();
    let parsed: Vec<_> = paths
        .iter()
        .map(|path| {
            (
                path.strip_prefix(crates)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
                tokens(&std::fs::read_to_string(path).unwrap()),
            )
        })
        .collect();
    // Track aliases across files and re-exports, not just at a call site.
    let streams: Vec<_> = parsed.iter().map(|(_, t)| t.as_slice()).collect();
    let names = command_names(&streams);
    let mut excluded = BTreeMap::<String, usize>::new();
    let mut gated = BTreeMap::<String, usize>::new();
    for (relative, t) in &parsed {
        let ranges = gated_sections(t);
        for site in constructions(t, &names) {
            if ranges.iter().any(|r| r.contains(&site)) {
                *gated.entry(relative.clone()).or_default() += 1;
                continue;
            }
            // PTY is a separate client process, like its editor and clipboard.
            if relative == "orch-cli/src/web/pty.rs" {
                *excluded.entry(relative.clone()).or_default() += 1;
                continue;
            }
            assert!(
                EXCEPTIONS
                    .iter()
                    .any(|(file, _, _)| *file == relative.as_str()),
                "ungated process Command construction in {relative}, token {site}"
            );
            *excluded.entry(relative.clone()).or_default() += 1;
        }
    }
    for (file, count, reason) in EXCEPTIONS {
        assert_eq!(
            excluded.remove(*file),
            Some(*count),
            "unreviewed process construction count: {file}: {reason}"
        );
    }
    assert_eq!(
        excluded.remove("orch-cli/src/web/pty.rs"),
        Some(1),
        "only the client PTY may use portable_pty::CommandBuilder"
    );
    assert!(excluded.is_empty(), "unreviewed exceptions: {excluded:?}");
    assert_eq!(
        gated,
        BTreeMap::from([
            ("orch-infra/src/process/manager.rs".into(), 1),
            ("orch-infra/src/models/discovery.rs".into(), 1),
            ("orch-infra/src/storage/remote.rs".into(), 1),
        ])
    );
}

#[test]
fn guard_sees_aliases_and_ignores_literals_and_comments() {
    let declaration = tokens("pub use std::process::Command as First;");
    let alias = tokens("use crate::First as Second; type Child = Second;");
    let call = tokens("let make = Child::new; Second::new(p);");
    let names = command_names(&[&call, &alias, &declaration]);
    assert_eq!(
        constructions(&call, &names).len(),
        2,
        "cross-file aliases and constructor references must not escape the guard"
    );
    let t = tokens(
        "use std::process::Command as Child; Child::new(p); // Command::new(x)\nlet s = r#\"Command::new(x) }\"#;",
    );
    assert_eq!(constructions(&t, &command_names(&[&t])).len(), 1);
}
