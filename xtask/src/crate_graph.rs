//! `cargo xtask crate-graph` — the layering invariant from ARCHITECTURE §3.1.
//!
//! The bold, normative rule is that **`orch-engine` has zero dependency on
//! `orch-cli` or `orch-tui`**, mirroring the TS claim that "the core engine has
//! zero dependencies on CLI/TUI layers". §8.5 asks for this as a CI step
//! precisely so it is checked rather than assumed.
//!
//! This asserts more than that one rule: the **whole** set of inter-crate edges
//! against a table. A single forbidden-edge check would pass a workspace where
//! `orch-infra` had started depending on `orch-engine`, which inverts the
//! layering just as badly and is just as easy to do by adding one line to a
//! Cargo.toml. A table also makes the intended shape readable in one place.
//!
//! Edges are read from `cargo metadata`, so dev-dependencies and
//! build-dependencies count too: a test-only edge still compiles the crate and
//! still makes a cycle impossible to ignore later.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Every allowed **direct** dependency between workspace crates.
///
/// Anything not listed is a failure, and anything listed that no longer exists
/// is also a failure — a stale entry is permission nobody needs any more, and
/// it would silently license a future re-introduction.
const ALLOWED: &[(&str, &[&str])] = &[
    // No dependency on anything in the workspace: serde and thiserror only.
    ("orch-domain", &[]),
    // Traits only.
    ("orch-core", &["orch-domain"]),
    ("orch-infra", &["orch-core", "orch-domain"]),
    // The engine→infra edge is **not** in ARCHITECTURE §3.1's diagram, which
    // says "engine depends on core, NOT on infra". The diagram describes the
    // services, and it is accurate about them: every service takes orch-core
    // traits. The edge exists because `container::build_light`/`build_full`
    // live in the engine and have to name the concrete infra types they
    // assemble. Recorded here rather than left as a silent discrepancy — see
    // the note in crates/orch-engine/Cargo.toml.
    ("orch-engine", &["orch-core", "orch-domain", "orch-infra"]),
    // **Tighter than ARCHITECTURE §3.1's diagram**, which has the TUI
    // depending on the engine. WP-6 inverted it: everything the dashboard
    // needs goes through one trait, `orch_tui::backend::TuiBackend`, so the
    // TUI names only the domain types that cross it. `orch-cli` implements
    // that trait over the DI container (`cli/src/tui/backend.rs`) and the
    // TUI's own tests use an in-crate `FakeBackend`.
    //
    // This table entry is what noticed the change: the previous one listed
    // `orch-core` and `orch-engine`, and the stale-entry check turned
    // "someone quietly improved the layering" into a failure that had to be
    // looked at rather than a diff nobody read.
    ("orch-tui", &["orch-domain"]),
    // The CLI is the top of the stack and may name anything below it.
    (
        "orch-cli",
        &[
            "orch-core",
            "orch-domain",
            "orch-engine",
            "orch-infra",
            "orch-tui",
        ],
    ),
];

pub fn run(repo_root: &Path) -> Result<()> {
    let actual = workspace_edges(repo_root)?;
    let allowed: BTreeMap<&str, BTreeSet<&str>> = ALLOWED
        .iter()
        .map(|(crate_name, deps)| (*crate_name, deps.iter().copied().collect()))
        .collect();

    let mut problems = Vec::new();

    for (crate_name, deps) in &actual {
        let Some(permitted) = allowed.get(crate_name.as_str()) else {
            problems.push(format!(
                "`{crate_name}` is a workspace crate that xtask/src/crate_graph.rs does not \
                 know about — add it to ALLOWED with its intended edges"
            ));
            continue;
        };
        for dep in deps {
            if !permitted.contains(dep.as_str()) {
                problems.push(format!(
                    "`{crate_name}` depends on `{dep}`, which ARCHITECTURE §3.1 does not allow"
                ));
            }
        }
        for permitted_dep in permitted {
            if !deps.contains(*permitted_dep) {
                problems.push(format!(
                    "`{crate_name}` no longer depends on `{permitted_dep}`, so that entry in \
                     ALLOWED is stale — remove it rather than leaving standing permission for \
                     an edge nobody needs"
                ));
            }
        }
    }
    for crate_name in allowed.keys() {
        if !actual.contains_key(*crate_name) {
            problems.push(format!(
                "ALLOWED lists `{crate_name}`, which is not a workspace crate any more"
            ));
        }
    }

    // Stated separately because it is the one rule the contract puts in bold,
    // and it should fail by name rather than as one row of a table.
    if let Some(engine_deps) = actual.get("orch-engine") {
        for forbidden in ["orch-cli", "orch-tui"] {
            if engine_deps.contains(forbidden) {
                problems.push(format!(
                    "ARCHITECTURE §3.1's enforced invariant is broken: `orch-engine` depends \
                     on `{forbidden}`. The engine must be usable as a library with no CLI or \
                     TUI present."
                ));
            }
        }
    }

    println!("crate graph ({} crates):", actual.len());
    for (crate_name, deps) in &actual {
        let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
        println!(
            "  {crate_name:<12} -> {}",
            if deps.is_empty() {
                "(nothing in the workspace)".to_owned()
            } else {
                deps.join(", ")
            }
        );
    }
    println!();

    if !problems.is_empty() {
        for problem in &problems {
            println!("  {problem}");
        }
        bail!("{} layering problem(s)", problems.len());
    }
    println!("layering matches ARCHITECTURE §3.1");
    Ok(())
}

/// Each workspace crate's direct dependencies on other workspace crates,
/// including dev- and build-dependencies.
fn workspace_edges(repo_root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root)
        .output()
        .context("run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: Value =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata output")?;
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata has no `packages` array")?;

    let names: BTreeSet<&str> = packages
        .iter()
        .filter_map(|package| package["name"].as_str())
        .filter(|name| name.starts_with("orch-"))
        .collect();

    let mut edges = BTreeMap::new();
    for package in packages {
        let Some(name) = package["name"].as_str() else {
            continue;
        };
        if !names.contains(name) {
            continue;
        }
        let deps = package["dependencies"]
            .as_array()
            .map(|deps| {
                deps.iter()
                    .filter_map(|dep| dep["name"].as_str())
                    .filter(|dep| names.contains(dep))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        edges.insert(name.to_owned(), deps);
    }

    if edges.is_empty() {
        bail!("cargo metadata reported no orch-* crates — is this the right directory?");
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowed_table_is_acyclic_and_self_consistent() {
        // The table is the spec, so it should be checkable on its own: no
        // crate may depend on itself, and no pair may depend on each other.
        let allowed: BTreeMap<&str, BTreeSet<&str>> = ALLOWED
            .iter()
            .map(|(name, deps)| (*name, deps.iter().copied().collect()))
            .collect();

        for (name, deps) in &allowed {
            assert!(!deps.contains(name), "{name} depends on itself");
            for dep in deps {
                assert!(
                    allowed.contains_key(*dep),
                    "{name} depends on unknown crate {dep}"
                );
                assert!(
                    !allowed[*dep].contains(name),
                    "{name} and {dep} depend on each other"
                );
            }
        }
    }

    #[test]
    fn the_engine_may_not_name_the_front_ends() {
        // The one rule ARCHITECTURE §3.1 puts in bold, asserted against the
        // table itself so a careless edit to ALLOWED cannot license it.
        let engine = ALLOWED
            .iter()
            .find(|(name, _)| *name == "orch-engine")
            .expect("orch-engine is in the table")
            .1;
        assert!(!engine.contains(&"orch-cli"));
        assert!(!engine.contains(&"orch-tui"));
    }

    #[test]
    fn the_table_matches_this_workspace() {
        // The real check also runs in CI via `xtask gate`, but having it as a
        // unit test means `cargo test` alone catches a Cargo.toml edit.
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a parent");
        run(repo_root).expect("the workspace layering matches ARCHITECTURE §3.1");
    }
}
