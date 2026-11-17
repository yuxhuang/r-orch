use std::collections::BTreeSet;

use anyhow::{Result, ensure};

use super::{Change, Class, MAX_PATH, Reason, graph::Graph};

pub(super) fn paths(change: &Change) -> Result<Vec<&[u8]>> {
    let status = change.status.as_str();
    ensure!(status != "T", "special file, submodule or type change");
    let pair = status.starts_with('R') || status.starts_with('C');
    if pair {
        let score = status.get(1..).unwrap_or_default();
        ensure!(
            !score.is_empty() && score.len() <= 3 && score.bytes().all(|b| b.is_ascii_digit()),
            "malformed rename/copy status"
        );
        ensure!(
            score.parse::<u16>().is_ok_and(|score| score <= 100),
            "invalid rename/copy score"
        );
        let old = change
            .old_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| anyhow::anyhow!("rename/copy source missing"))?;
        Ok(vec![old, &change.path])
    } else {
        ensure!(
            matches!(status, "A" | "M" | "D" | "?"),
            "unsupported or malformed change status"
        );
        ensure!(change.old_path.is_none(), "unexpected rename/copy source");
        Ok(vec![&change.path])
    }
}

pub(super) fn path(bytes: &[u8]) -> Result<&str> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_PATH,
        "empty or oversized path"
    );
    let path = std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("non-UTF-8 path"))?;
    ensure!(!path.contains('\0'), "NUL in path");
    ensure!(
        path.split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        "path is not repository-relative and normalized"
    );
    Ok(path)
}

// Preserve every byte on an invalid-UTF-8 diagnostic without conflating it with
// another path. Valid UTF-8 remains exact; renderers escape it for terminals.
pub(super) fn diagnostic_path(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(path) => path.to_owned(),
        Err(_) => format!(
            "bytes:{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ),
    }
}

pub(super) fn classify(path: &str, graph: &Graph) -> (Reason, BTreeSet<String>) {
    let components: Vec<_> = path.split('/').collect();
    let global = components.last() == Some(&"Cargo.toml")
        || matches!(
            path,
            "Cargo.lock"
                | "rust-toolchain.toml"
                | "rust-toolchain"
                | "clippy.toml"
                | "deny.toml"
                | ".gitignore"
                | ".gitattributes"
        )
        || [
            ".cargo", "xtask", ".github", ".config", "fixtures", "skills", "examples",
        ]
        .contains(&components[0])
        || components
            .iter()
            .any(|part| matches!(*part, "test_support" | "test-support"))
        || components.windows(2).any(|pair| {
            pair[0] == "tests"
                && (matches!(pair[1], "common" | "support" | "common.rs" | "support.rs")
                    || pair[1].ends_with("_support")
                    || pair[1].ends_with("_support.rs"))
        })
        || path.ends_with("/test_support.rs");
    if global {
        return (
            reason(
                path,
                Class::Global,
                "all",
                "shared build, configuration or test input",
            ),
            BTreeSet::new(),
        );
    }
    if path == "docs/ARCHITECTURE.md" {
        return if graph.contains("xtask") {
            (
                reason(
                    path,
                    Class::Coupled,
                    "xtask",
                    "crate-graph and its tests read this document",
                ),
                BTreeSet::from(["xtask".into()]),
            )
        } else {
            (
                reason(
                    path,
                    Class::Error,
                    "all",
                    "document coupling names a missing xtask member",
                ),
                BTreeSet::new(),
            )
        };
    }
    if path.starts_with("docs/")
        || (!path.contains('/') && (path.ends_with(".md") || matches!(path, "LICENSE" | "NOTICE")))
    {
        return (
            reason(path, Class::Doc, "none", "documentation-only input"),
            BTreeSet::new(),
        );
    }
    if let Some(owner) = graph.owner(path) {
        let packages = graph.dependents(owner);
        return (
            reason(
                path,
                Class::Crate,
                &packages.iter().cloned().collect::<Vec<_>>().join(","),
                "workspace member and transitive normal/build/dev dependents",
            ),
            packages,
        );
    }
    (
        reason(
            path,
            Class::Unknown,
            "all",
            "no known workspace owner or classification",
        ),
        BTreeSet::new(),
    )
}

pub(super) fn reason(path: &str, class: Class, selects: &str, why: &str) -> Reason {
    Reason {
        path: path.into(),
        class,
        selects: selects.into(),
        why: why.into(),
    }
}
