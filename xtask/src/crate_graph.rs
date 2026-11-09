//! `cargo xtask crate-graph` — the layering invariant from ARCHITECTURE §3.1.
//!
//! The bold, normative rule is that **`orch-engine` has zero dependency on
//! `orch-cli` or `orch-tui`**, so the core engine stays free of the CLI and TUI
//! layers. §8.5 asks for this as a CI step
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
    // The engine→infra edge is real; ARCHITECTURE §3.1 has the rationale and
    // the per-site table. Deliberately not repeated here: that list has now
    // gone stale twice — first as the diagram claiming the edge did not exist,
    // then as a table naming three of the five non-test sites — so
    // `ENGINE_INFRA_SITES` below checks it instead of prose describing it.
    ("orch-engine", &["orch-core", "orch-domain", "orch-infra"]),
    // Transport composes the engine and concrete peer persistence; the engine
    // reaches it only through the network-independent bridge in orch-core.
    // orch-proto supplies the shared bounded NDJSON framing only (design D10).
    (
        "orch-peer",
        &[
            "orch-core",
            "orch-domain",
            "orch-engine",
            "orch-infra",
            "orch-proto",
        ],
    ),
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
    // The local daemon protocol: data and pure codecs only, so a client crate
    // can depend on it without gaining a store capability (design D1).
    ("orch-proto", &["orch-domain"]),
    ("orch-client", &["orch-domain", "orch-proto"]),
    // The per-project daemon composes everything that owns state.
    (
        "orch-daemon",
        &[
            "orch-core",
            "orch-domain",
            "orch-engine",
            "orch-infra",
            "orch-peer",
            "orch-proto",
        ],
    ),
    // The CLI is the top of the stack and may name anything below it. The
    // proto/daemon edges serve `orch daemon` in PR 1; PR 4 tightens this row
    // to the client-only set (design D1).
    (
        "orch-cli",
        &[
            "orch-client",
            "orch-core",
            "orch-daemon",
            "orch-domain",
            "orch-engine",
            "orch-infra",
            "orch-peer",
            "orch-proto",
            "orch-tui",
        ],
    ),
];

/// The `orch-engine` files allowed to name `orch_infra`, and why.
///
/// ARCHITECTURE §3.1's useful claim is not "the engine does not depend on
/// infra" — it does — but the narrower one: **every service takes only
/// `orch-core` traits**, so the engine stays unit-testable against
/// `test_support` fakes and `orch-infra` stays swappable wholesale. That
/// property is about *which files* reach for a concrete type, which prose
/// cannot keep up with: the claim has gone stale twice, first as a diagram
/// denying the edge, then as a table naming three of the five sites.
///
/// So it is a check. A new file reaching into `orch-infra` fails here and has
/// to be justified by adding a line, and a line that no longer corresponds to
/// anything fails too.
const ENGINE_INFRA_SITES: &[(&str, &str)] = &[
    (
        "container.rs",
        "assembly — every concrete store, adapter and manager it builds; \
         the composition root",
    ),
    (
        "orchestrator.rs",
        "the tick-scoped cache wrappers, which the orchestrator \
         constructs itself around the injected stores",
    ),
    (
        "orchestrator/dispatch.rs",
        "the default templates and `build_prompt_context` from the \
         template engine",
    ),
    (
        "task_service.rs",
        "`Paths` and `fs_utils`, for attachment paths, which the task \
         service resolves itself",
    ),
    (
        "prompt_mode.rs",
        "the project prompt-mode toggle edits one key of the raw config \
         under the `.definition.lock` (`Paths`, `fs_utils`), via \
         `FileConfigStore::read_raw`/`write_raw`, which the orch-core \
         `ConfigStore` trait does not expose",
    ),
    (
        "lock.rs",
        "the PID-file lock mechanism, re-exported so the engine's \
         `ProjectLock` impl is a thin wrapper rather than a second \
         implementation",
    ),
    (
        "message_service.rs",
        "remote recipient validation uses the explicitly opt-in RemoteAgentDirectory; AgentStore remains local",
    ),
    (
        "orchestrator/tick.rs",
        "reconcile reads the persistent remote registry to fail removed executions without retry",
    ),
    (
        "orchestrator/recovery.rs",
        "startup cleanup reads the same remote registry as tick.rs, to keep remote runs whose peer is still registered",
    ),
    (
        "orchestrator/remote.rs",
        "rehydrating remote runs at startup reads the persisted RemoteAgentDirectory snapshot, as message_service.rs does",
    ),
    (
        "orchestrator/tests/harness.rs",
        "test assembly wires the explicit RemoteAgentDirectory to TaskService",
    ),
    (
        "orchestrator/tests/remote.rs",
        "remote lifecycle tests exercise real registry/status files and stale-lock guards",
    ),
    (
        "orchestrator/tests/skills.rs",
        "a test asserting the dispatch templates are the real ones",
    ),
    (
        "orchestrator/tests/token_eco.rs",
        "a test asserting a normal-mode agent with only a custom system \
         template still renders the real default user template, as \
         skills.rs does",
    ),
];

/// Paths and tokens that would give `orch-client` a process capability.
///
/// No client code path may spawn, fork or exec anything — in particular a
/// daemon (spec R4, plan Q6: the owner starts the daemon, never a client).
/// The crate edge table keeps `orch-client` to `orch-domain` + `orch-proto`;
/// this keeps the crate's own source and dependency features free of the
/// standard, Tokio, nix and libc process APIs. Comments count, as they do
/// for `orch_infra` above: the crate has no reason to name them at all.
/// Tasks (`tokio::spawn`, `spawn_blocking`) are not processes and are fine.
const CLIENT_PROCESS_PATHS: &[&str] = &["std::process", "tokio::process"];
const CLIENT_PROCESS_TOKENS: &[&str] = &[
    "Command",
    "fork",
    "vfork",
    "exec",
    "execl",
    "execle",
    "execlp",
    "execv",
    "execve",
    "execvp",
    "execvpe",
    "fexecve",
    "posix_spawn",
    "posix_spawnp",
];
const CLIENT_PROCESS_FEATURES: &[(&str, &[&str])] =
    &[("tokio", &["process", "full"]), ("nix", &["process"])];

/// The process paths and tokens `text` names, in order of appearance.
fn process_capabilities(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = CLIENT_PROCESS_PATHS
        .iter()
        .copied()
        .filter(|path| text.contains(path))
        .collect();
    for token in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if let Some(forbidden) = CLIENT_PROCESS_TOKENS.iter().find(|t| **t == token)
            && !found.contains(forbidden)
        {
            found.push(forbidden);
        }
    }
    found
}

/// The spawn-token gate: `orch-client` sources and declared features.
fn client_process_capabilities(repo_root: &Path, metadata: &Value) -> Result<Vec<String>> {
    let src = repo_root.join("crates/orch-client/src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files)?;
    let mut problems = Vec::new();
    for path in files {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for capability in process_capabilities(&text) {
            let relative = path.strip_prefix(repo_root).unwrap_or(&path);
            problems.push(format!(
                "`{}` names `{capability}`. orch-client must not spawn, fork or exec \
                 anything (spec R4: no client ever starts a daemon); move process work \
                 to the daemon or the CLI entry point",
                relative.display()
            ));
        }
    }
    let client = metadata["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|p| p["name"] == "orch-client"))
        .context("cargo metadata has no orch-client package")?;
    for dep in client["dependencies"].as_array().into_iter().flatten() {
        let kind = dep["kind"].as_str();
        let Some(name) = dep["name"].as_str() else {
            continue;
        };
        let Some((_, forbidden)) = CLIENT_PROCESS_FEATURES.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        for feature in dep["features"].as_array().into_iter().flatten() {
            if let Some(feature) = feature.as_str().filter(|f| forbidden.contains(f))
                && kind.is_none()
            {
                problems.push(format!(
                    "orch-client enables `{name}/{feature}`, a process capability \
                     (spec R4); the client crate must not declare it"
                ));
            }
        }
    }
    Ok(problems)
}

fn collect_rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// Check which `orch-engine` sources name `orch_infra` against the allowlist.
fn engine_infra_sites(repo_root: &Path) -> Result<Vec<String>> {
    let src = repo_root.join("crates/orch-engine/src");
    let mut found = BTreeSet::new();
    collect_infra_users(&src, &src, &mut found)?;

    let allowed: BTreeSet<&str> = ENGINE_INFRA_SITES.iter().map(|(file, _)| *file).collect();
    let mut problems = Vec::new();

    for file in &found {
        if !allowed.contains(file.as_str()) {
            problems.push(format!(
                "`crates/orch-engine/src/{file}` names `orch_infra`, which \
                 ENGINE_INFRA_SITES does not allow. ARCHITECTURE §3.1 permits the edge at \
                 assembly and at the sites listed in ENGINE_INFRA_SITES — if this is one of \
                 those, add it with the reason; if it is a service, it should take an \
                 orch-core trait instead."
            ));
        }
    }
    for file in &allowed {
        if !found.contains(*file) {
            problems.push(format!(
                "ENGINE_INFRA_SITES lists `{file}`, which no longer names `orch_infra` — \
                 remove it rather than leaving standing permission"
            ));
        }
    }
    Ok(problems)
}

fn collect_infra_users(root: &Path, dir: &Path, out: &mut BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_infra_users(root, &path, out)?;
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs") {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            // Any mention counts, doc comments included: a doc link is a
            // deliberate reference to the concrete crate and worth listing.
            if text.contains("orch_infra") {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(relative);
            }
        }
    }
    Ok(())
}

pub fn run(repo_root: &Path) -> Result<()> {
    let metadata = cargo_metadata(repo_root)?;
    let actual = workspace_edges(&metadata)?;
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

    problems.extend(engine_infra_sites(repo_root)?);
    problems.extend(client_process_capabilities(repo_root, &metadata)?);

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
    println!("layering matches ARCHITECTURE §3.1; orch-client has no process capability");
    Ok(())
}

fn cargo_metadata(repo_root: &Path) -> Result<Value> {
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

    serde_json::from_slice(&output.stdout).context("parse cargo metadata output")
}

/// Each workspace crate's direct dependencies on other workspace crates,
/// including dev- and build-dependencies.
fn workspace_edges(metadata: &Value) -> Result<BTreeMap<String, BTreeSet<String>>> {
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
    fn the_spawn_gate_names_every_process_capability() {
        for (text, expected) in [
            (
                "let c = std::process::Command::new(\"orch\");",
                vec!["std::process", "Command"],
            ),
            (
                "use tokio::process::Command as C;",
                vec!["tokio::process", "Command"],
            ),
            ("unsafe { nix::unistd::fork() }", vec!["fork"]),
            ("nix::unistd::execv(&path, &argv)", vec!["execv"]),
            ("libc::posix_spawn(..)", vec!["posix_spawn"]),
            ("// exec the daemon", vec!["exec"]),
        ] {
            assert_eq!(process_capabilities(text), expected, "{text}");
        }
        // Tasks and words that merely contain a token are not processes.
        for text in [
            "tokio::spawn(async {})",
            "tokio::task::spawn_blocking(f)",
            "executes, forks, Commands, process exit",
        ] {
            assert!(process_capabilities(text).is_empty(), "{text}");
        }
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
