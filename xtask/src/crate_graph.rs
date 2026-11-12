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
//! Declared metadata edges retain their kind, alias and target. Normal and dev
//! allowances are separate; inactive optional/target edges are checked too.
//! Source checks are review tripwires, not a proof against a malicious author.

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
    // Only the explicit daemon/init wrappers may use the owner facade.
    (
        "orch-cli",
        &[
            "orch-client",
            "orch-core",
            "orch-daemon",
            "orch-domain",
            "orch-proto",
            "orch-tui",
        ],
    ),
    ("xtask", &[]),
];

/// Dev edges never authorize production imports or enabled test APIs.
const DEV_ALLOWED: &[(&str, &[&str])] = &[
    ("orch-infra", &["orch-core"]),
    ("orch-engine", &["orch-core"]),
    ("orch-peer", &["orch-core"]),
    (
        "orch-cli",
        &["orch-core", "orch-daemon", "orch-engine", "orch-infra"],
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
                && kind != Some("dev")
            {
                problems.push(format!(
                    "orch-client enables `{name}/{feature}`, a process capability \
                     (spec R4); the client crate must not declare it"
                ));
            }
        }
    }
    for features in client["features"]
        .as_object()
        .into_iter()
        .flat_map(|f| f.values())
    {
        for feature in features
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let Some((dependency, capability)) = feature.split_once('/') else {
                continue;
            };
            let dependency = dependency.trim_end_matches('?');
            for dep in client["dependencies"].as_array().into_iter().flatten() {
                let alias = dep["rename"].as_str().or_else(|| dep["name"].as_str());
                if alias == Some(dependency)
                    && CLIENT_PROCESS_FEATURES
                        .iter()
                        .any(|(name, bad)| dep["name"] == *name && bad.contains(&capability))
                {
                    problems.push(format!(
                        "orch-client feature `{feature}` enables a process capability"
                    ));
                }
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
    let mut problems = graph_problems(&actual);
    problems.extend(engine_infra_sites(repo_root)?);
    problems.extend(client_process_capabilities(repo_root, &metadata)?);
    problems.extend(cli_source_sites(repo_root)?);
    problems.extend(bootstrap_contracts(repo_root)?);
    problems.extend(pure_source_sites(repo_root)?);
    problems.extend(promoted_test_features(&metadata));
    println!("crate graph ({} crates):", actual.len());
    for (name, deps) in &actual {
        for dep in deps {
            println!(
                "  {name} -> {} ({}, rename={:?}, target={:?}, optional={})",
                dep.name, dep.kind, dep.rename, dep.target, dep.optional
            );
        }
    }
    if !problems.is_empty() {
        for problem in &problems {
            println!("  {problem}");
        }
        bail!("{} layering problem(s)", problems.len());
    }
    println!(
        "layering matches ARCHITECTURE §3.1; CLI bootstrap sites and client process boundary checked"
    );
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

#[derive(Clone, Debug)]
struct Edge {
    name: String,
    kind: String,
    rename: Option<String>,
    target: Option<String>,
    optional: bool,
    features: Vec<String>,
}

type Edges = BTreeMap<String, Vec<Edge>>;

/// Use declared packages, not resolved nodes: inactive edges still grant capability.
fn workspace_edges(metadata: &Value) -> Result<Edges> {
    let packages = metadata["packages"]
        .as_array()
        .context("metadata packages missing")?;
    let members = metadata["workspace_members"]
        .as_array()
        .context("workspace members missing")?;
    let packages: Vec<_> = packages
        .iter()
        .filter(|p| members.contains(&p["id"]))
        .collect();
    let names: BTreeSet<_> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut edges = BTreeMap::new();
    for package in &packages {
        let name = package["name"].as_str().context("package name missing")?;
        let mut deps = Vec::new();
        for dep in package["dependencies"]
            .as_array()
            .context("dependencies missing")?
        {
            let name = dep["name"].as_str().context("dependency name missing")?;
            if names.contains(name) {
                deps.push(Edge {
                    name: name.to_owned(),
                    kind: dep["kind"].as_str().unwrap_or("normal").to_owned(),
                    rename: dep["rename"].as_str().map(str::to_owned),
                    target: dep["target"].as_str().map(str::to_owned),
                    optional: dep["optional"].as_bool().unwrap_or(false),
                    features: dep["features"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                });
            }
        }
        edges.insert(name.to_owned(), deps);
    }
    if edges.is_empty() {
        bail!("metadata reported no workspace crates");
    }
    Ok(edges)
}

fn graph_problems(actual: &Edges) -> Vec<String> {
    let mut problems = Vec::new();
    for (owner, _) in DEV_ALLOWED {
        if !actual.contains_key(*owner) {
            problems.push(format!(
                "stale dev allowance owner `{owner}` is not a workspace crate"
            ));
        }
    }
    for name in actual.keys() {
        if !ALLOWED.iter().any(|(n, _)| n == name) {
            problems.push(format!(
                "`{name}` is an unclassified workspace crate; add its policy explicitly"
            ));
        }
    }
    for (name, normal) in ALLOWED {
        let Some(deps) = actual.get(*name) else {
            problems.push(format!("ALLOWED lists missing workspace crate `{name}`"));
            continue;
        };
        let dev = DEV_ALLOWED
            .iter()
            .find(|(n, _)| n == name)
            .map_or(&[][..], |(_, d)| *d);
        for edge in deps {
            let permitted = match edge.kind.as_str() {
                "normal" => *normal,
                "dev" => dev,
                _ => &[],
            };
            if !permitted.contains(&edge.name.as_str())
                || edge.rename.is_some()
                || edge.target.is_some()
                || edge.optional
            {
                problems.push(format!("`{name}` -> `{}` violates declared-edge policy: kind={}, rename={:?}, target={:?}, optional={}",
                    edge.name, edge.kind, edge.rename, edge.target, edge.optional));
            }
            let expected_features: &[&str] = match (edge.kind.as_str(), edge.name.as_str()) {
                ("dev", "orch-core") => &["test-support"],
                ("dev", "orch-daemon") => &["testing"],
                _ => &[],
            };
            let features: BTreeSet<_> = edge.features.iter().map(String::as_str).collect();
            if features != expected_features.iter().copied().collect() {
                problems.push(format!(
                    "`{name}` -> `{}` has unreviewed {} features {:?}; expected {:?}",
                    edge.name, edge.kind, features, expected_features
                ));
            }
            if edge.kind != "dev"
                && edge
                    .features
                    .iter()
                    .any(|f| ["testing", "test-support"].contains(&f.as_str()))
            {
                problems.push(format!(
                    "`{name}` -> `{}` promotes a test API into {} dependencies",
                    edge.name, edge.kind
                ));
            }
        }
        for (kind, allowed) in [("normal", *normal), ("dev", dev)] {
            for dep in allowed {
                if !deps.iter().any(|e| {
                    e.name == *dep
                        && e.kind == kind
                        && e.rename.is_none()
                        && e.target.is_none()
                        && !e.optional
                }) {
                    problems.push(format!(
                        "stale {kind} allowance `{name}` -> `{dep}`; remove standing permission"
                    ));
                }
            }
        }
    }
    for pure in ["orch-client", "orch-proto", "orch-tui"] {
        let mut pending = vec![pure];
        let mut seen = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !seen.insert(name) {
                continue;
            }
            if [
                "orch-infra",
                "orch-engine",
                "orch-peer",
                "orch-daemon",
                "orch-core",
                "orch-cli",
            ]
            .contains(&name)
            {
                problems.push(format!("pure client `{pure}` reaches state capability `{name}` through normal/build workspace edges"));
            }
            if let Some(deps) = actual.get(name) {
                pending.extend(
                    deps.iter()
                        .filter(|e| e.kind != "dev")
                        .map(|e| e.name.as_str()),
                );
            }
        }
    }
    problems
}

fn promoted_test_features(metadata: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        for features in package["features"]
            .as_object()
            .into_iter()
            .flat_map(|f| f.values())
        {
            for feature in features
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if feature
                    .split_once('/')
                    .is_some_and(|(_, name)| ["testing", "test-support"].contains(&name))
                {
                    problems.push(format!(
                        "{} feature `{feature}` promotes a dependency test API into production",
                        package["name"]
                    ));
                }
            }
        }
    }
    problems
}

fn pure_source_sites(repo_root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for name in ["orch-client", "orch-proto", "orch-tui"] {
        let mut files = Vec::new();
        collect_rust_files(&repo_root.join("crates").join(name).join("src"), &mut files)?;
        for file in files {
            let text = std::fs::read_to_string(&file)?;
            for token in production_tokens(&rust_tokens(&text)) {
                if !token.literal
                    && [
                        "orch_core",
                        "orch_engine",
                        "orch_infra",
                        "orch_peer",
                        "orch_daemon",
                        "orch_cli",
                    ]
                    .contains(&token.text)
                {
                    problems.push(format!(
                        "{}:{}: pure client source names state capability `{}`",
                        file.strip_prefix(repo_root)?.display(),
                        token.line,
                        token.text
                    ));
                }
            }
        }
    }
    Ok(problems)
}

/// Exact production ownership sites, separate from cfg-test fixture roots.
const CLI_DAEMON_SITES: &[(&str, &[&str])] = &[
    (
        "commands/daemon.rs",
        &[
            "select_project_root",
            "prepare_start",
            "StartOptions",
            "DispatchMode",
            "DaemonExit",
            "DaemonError",
        ],
    ),
    (
        "commands/init.rs",
        &[
            "InitOutcome",
            "is_initialized",
            "initialize",
            "InitOptions",
            "DaemonError",
        ],
    ),
];
const CLI_TEST_ROOTS: &[(&str, &str, &str)] = &[
    (
        "spawn_subscriber_tests.rs",
        "lib.rs",
        "#[cfg(all(test, target_os = \"macos\"))] mod spawn_subscriber_tests;",
    ),
    (
        "commands/org_testing.rs",
        "commands/org.rs",
        "#[cfg(test)] #[path = \"org_testing.rs\"] mod testing;",
    ),
    (
        "mcp/testing.rs",
        "mcp.rs",
        "#[cfg(test)] pub(crate) mod testing;",
    ),
    (
        "mcp/tools/remote_tests.rs",
        "mcp/tools/remote.rs",
        "#[cfg(test)] #[path = \"remote_tests.rs\"] mod tests;",
    ),
];

#[derive(Clone, Debug)]
struct Token<'a> {
    text: &'a str,
    line: usize,
    literal: bool,
}

/// A small lexical tripwire: comments/literals cannot authorize an import.
/// Keep literals as opaque tokens so cfg/path attributes can still be checked.
fn rust_tokens(text: &str) -> Vec<Token<'_>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < b.len() {
        let start = i;
        let start_line = line;
        let mut literal = false;
        if b[i].is_ascii_whitespace() {
            i += 1;
        } else if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
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
        } else {
            // Raw string prefixes r, br and cr; ordinary byte/C strings are
            // handled by their following quote on the next iteration.
            let raw = if b[i] == b'r' {
                Some(i + 1)
            } else if (b[i] == b'b' || b[i] == b'c') && b.get(i + 1) == Some(&b'r') {
                Some(i + 2)
            } else {
                None
            };
            let raw = raw.and_then(|mut j| {
                let hash_start = j;
                while b.get(j) == Some(&b'#') {
                    j += 1;
                }
                (b.get(j) == Some(&b'"')).then_some((j, j - hash_start))
            });
            if let Some((quote, hashes)) = raw {
                literal = true;
                i = quote + 1;
                while i < b.len() {
                    if b[i] == b'"'
                        && b.get(i + 1..i + 1 + hashes)
                            .is_some_and(|s| s.iter().all(|c| *c == b'#'))
                    {
                        i += 1 + hashes;
                        break;
                    }
                    i += 1;
                }
            } else if b[i] == b'"'
                || (b[i] == b'\'' && (b.get(i + 2) == Some(&b'\'') || b.get(i + 1) == Some(&b'\\')))
            {
                literal = true;
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
            } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
                i += 1;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                // A raw identifier is the same crate name, not a string.
                if &text[start..i] == "r"
                    && b.get(i) == Some(&b'#')
                    && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic)
                {
                    i += 1;
                    let ident = i;
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                    out.push(Token {
                        text: &text[ident..i],
                        line,
                        literal: false,
                    });
                    continue;
                }
            } else {
                // Advance by a Unicode scalar, keeping slicing on boundaries.
                i += text[i..].chars().next().map_or(1, char::len_utf8);
            }
            out.push(Token {
                text: &text[start..i],
                line: start_line,
                literal,
            });
        }
        line += b[start..i].iter().filter(|c| **c == b'\n').count();
    }
    out
}

fn group_end(tokens: &[Token<'_>], start: usize) -> usize {
    let close = match tokens[start].text {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        _ => return start + 1,
    };
    let mut i = start + 1;
    while i < tokens.len() {
        if tokens[i].text == close {
            return i + 1;
        }
        i = group_end(tokens, i);
    }
    tokens.len()
}

/// Conservative cfg proof: with test=false, can this expression still be true?
/// Unknown predicates can be either value; only a proven false permits skipping.
fn cfg_possibilities(tokens: &[Token<'_>]) -> (bool, bool) {
    let Some(first) = tokens.first() else {
        return (true, true);
    };
    if first.text == "test" && tokens.len() == 1 {
        return (true, false);
    }
    if tokens.get(1).is_some_and(|t| t.text == "(") {
        let mut args = Vec::new();
        let mut start = 2;
        let mut i = start;
        while i + 1 < tokens.len() {
            if tokens[i].text == "," {
                args.push(cfg_possibilities(&tokens[start..i]));
                start = i + 1;
                i += 1;
            } else {
                i = group_end(tokens, i);
            }
        }
        if start < i {
            args.push(cfg_possibilities(&tokens[start..i]));
        }
        return match first.text {
            "all" => (args.iter().any(|a| a.0), args.iter().all(|a| a.1)),
            "any" => (args.iter().all(|a| a.0), args.iter().any(|a| a.1)),
            "not" if args.len() == 1 => (args[0].1, args[0].0),
            _ => (true, true),
        };
    }
    (true, true)
}

fn production_tokens<'a>(tokens: &[Token<'a>]) -> Vec<Token<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].text == "#" && tokens.get(i + 1).is_some_and(|t| t.text == "[") {
            let begin = i;
            let mut test_only = false;
            while tokens.get(i).is_some_and(|t| t.text == "#")
                && tokens.get(i + 1).is_some_and(|t| t.text == "[")
            {
                let end = group_end(tokens, i + 1);
                if tokens.get(i + 2).is_some_and(|t| t.text == "cfg")
                    && tokens.get(i + 3).is_some_and(|t| t.text == "(")
                {
                    test_only |= !cfg_possibilities(&tokens[i + 4..end.saturating_sub(2)]).1;
                }
                i = end;
            }
            if test_only {
                // Skip this item only, never the containing production file.
                while i < tokens.len() {
                    let text = tokens[i].text;
                    i = group_end(tokens, i);
                    if text == "{" || text == ";" {
                        break;
                    }
                }
            } else {
                out.extend_from_slice(&tokens[begin..i]);
            }
        } else {
            out.push(tokens[i].clone());
            i += 1;
        }
    }
    out
}

fn same_tokens(left: &[Token<'_>], right: &[Token<'_>]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.text == b.text)
}

fn cli_file_problems(file: &str, text: &str) -> Vec<String> {
    let tokens = production_tokens(&rust_tokens(text));
    let mut problems = Vec::new();
    let allowed = CLI_DAEMON_SITES
        .iter()
        .find(|(f, _)| *f == file)
        .map(|(_, names)| *names);
    for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.literal) {
        if ["orch_infra", "orch_engine", "orch_peer"].contains(&token.text) {
            problems.push(format!("crates/orch-cli/src/{file}:{}: production owner import `{}` is forbidden (CLI_SOURCE_SITES)", token.line, token.text));
        }
        if token.text == "orch_daemon" {
            let symbol = tokens.get(i + 3).map(|t| t.text);
            let suffix = tokens.get(i + 4).is_some_and(|t| t.text == ":");
            let approved_variant = match symbol {
                Some("DispatchMode") => tokens
                    .get(i + 6)
                    .is_some_and(|t| ["On", "Off"].contains(&t.text)),
                Some("DaemonError") if file == "commands/init.rs" => {
                    tokens.get(i + 6).is_some_and(|t| t.text == "Core")
                }
                _ => false,
            };
            if suffix && (!approved_variant || tokens.get(i + 7).is_some_and(|t| t.text == ":")) {
                problems.push(format!("crates/orch-cli/src/{file}:{}: unreviewed associated bootstrap API (CLI_DAEMON_SITES)", token.line));
            }

            if tokens.get(i + 1).is_none_or(|t| t.text != ":")
                || tokens.get(i + 2).is_none_or(|t| t.text != ":")
                || !allowed.is_some_and(|names| symbol.is_some_and(|s| names.contains(&s)))
            {
                problems.push(format!("crates/orch-cli/src/{file}:{}: daemon reference is outside the exact CLI_DAEMON_SITES bootstrap API", token.line));
            }
        }
        // Loading a dedicated fixture by its ordinary module name is not a
        // second production route around its guarded declaration.
        if token.text == "mod"
            && let Some(name) = tokens.get(i + 1)
        {
            for (root, _, _) in CLI_TEST_ROOTS {
                if Path::new(root).file_stem().is_some_and(|s| s == name.text) {
                    problems.push(format!("crates/orch-cli/src/{file}:{}: cfg-test root `{root}` loaded in production", token.line));
                }
            }
        }
    }
    // Inspect only module-loading literals: a diagnostic string is not an
    // import, but a relative/raw path or concat! inside include! can load a fixture.
    let mut module_paths = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.text == "#" && tokens.get(i + 1).is_some_and(|t| t.text == "[") {
            let end = group_end(&tokens, i + 1);
            for attribute in tokens[i + 2..end - 1].windows(3) {
                if attribute[0].text == "path" && attribute[1].text == "=" && attribute[2].literal {
                    module_paths.push(&attribute[2]);
                }
            }
        }
        if token.text == "include"
            && tokens.get(i + 1).is_some_and(|t| t.text == "!")
            && tokens.get(i + 2).is_some_and(|t| t.text == "(")
        {
            module_paths.extend(
                tokens[i + 3..group_end(&tokens, i + 2) - 1]
                    .iter()
                    .filter(|t| t.literal),
            );
        }
    }
    for token in module_paths {
        for (root, _, _) in CLI_TEST_ROOTS {
            let basename = Path::new(root)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if token.text.contains(basename) {
                problems.push(format!(
                    "crates/orch-cli/src/{file}:{}: cfg-test path `{root}` loaded in production",
                    token.line
                ));
            }
        }
    }
    problems
}

fn cli_source_sites(repo_root: &Path) -> Result<Vec<String>> {
    let src = repo_root.join("crates/orch-cli/src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files)?;
    let mut problems = Vec::new();
    for (root, parent, declaration) in CLI_TEST_ROOTS {
        let text = std::fs::read_to_string(src.join(parent))?;
        let tokens = rust_tokens(&text);
        let expected = rust_tokens(declaration);
        if !src.join(root).is_file()
            || !tokens
                .windows(expected.len())
                .any(|w| same_tokens(w, &expected))
        {
            problems.push(format!("crates/orch-cli/src/{parent}: cfg-test root `{root}` must retain its independent guarded declaration"));
        }
    }
    let mut found = BTreeSet::new();
    for path in files {
        let file = path
            .strip_prefix(&src)?
            .to_string_lossy()
            .replace('\\', "/");
        if CLI_TEST_ROOTS.iter().any(|(f, _, _)| *f == file) {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        problems.extend(cli_file_problems(&file, &text));
        if production_tokens(&rust_tokens(&text))
            .iter()
            .any(|t| !t.literal && t.text == "orch_daemon")
        {
            found.insert(file);
        }
    }
    for (file, _) in CLI_DAEMON_SITES {
        if !found.contains(*file) {
            problems.push(format!(
                "stale CLI_DAEMON_SITES entry `{file}`; remove standing permission"
            ));
        }
    }
    Ok(problems)
}

/// Pin the facade's type boundary, not its implementation. An arbitrary
/// closure or returned store is an API change requiring an explicit review.
fn bootstrap_contracts(repo_root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for (file, contract) in [
        (
            "bootstrap.rs",
            "pub async fn initialize(options: InitOptions) -> std::result::Result<InitOutcome, DaemonError>",
        ),
        (
            "bootstrap.rs",
            "pub async fn is_initialized(project_root: &Path) -> bool",
        ),
        (
            "lifecycle.rs",
            "pub async fn start(options: StartOptions) -> Result<DaemonExit, DaemonError>",
        ),
        (
            "bootstrap.rs",
            "pub struct InitOutcome { pub already_initialized: bool, pub adapter: String, }",
        ),
        (
            "lifecycle.rs",
            "pub fn prepare_start(options: StartOptions) -> Result<PreparedStart, DaemonError>",
        ),
        (
            "lifecycle.rs",
            "pub async fn run(mut self) -> Result<DaemonExit, DaemonError>",
        ),
        (
            "lifecycle.rs",
            "pub struct DaemonExit { pub code: i32, pub clean: bool, }",
        ),
        (
            "discovery.rs",
            "pub fn select_project_root(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf, DaemonError>",
        ),
    ] {
        let text = std::fs::read_to_string(repo_root.join("crates/orch-daemon/src").join(file))?;
        let tokens = production_tokens(&rust_tokens(&text));
        let expected = rust_tokens(contract);
        let matched = tokens.windows(expected.len()).enumerate().any(|(i, w)| {
            same_tokens(w, &expected)
                && (contract.starts_with("pub struct")
                    || tokens
                        .get(i + expected.len())
                        .is_some_and(|t| t.text == "{"))
        });
        if !matched {
            problems.push(format!("crates/orch-daemon/src/{file}: bootstrap type boundary changed: `{contract}`; stores/engines and generic execution closures must not escape"));
        }
    }
    for (file, names) in [
        ("bootstrap.rs", &["initialize", "is_initialized"][..]),
        ("lifecycle.rs", &["start", "prepare_start", "run"][..]),
        ("discovery.rs", &["select_project_root"][..]),
    ] {
        let text = std::fs::read_to_string(repo_root.join("crates/orch-daemon/src").join(file))?;
        let tokens = production_tokens(&rust_tokens(&text));
        for (i, token) in tokens.iter().enumerate().filter(|(_, t)| t.text == "pub") {
            let mut next = i + 1;
            if tokens.get(next).is_some_and(|t| t.text == "async") {
                next += 1;
            }
            if tokens.get(next).is_some_and(|t| t.text == "fn")
                && tokens
                    .get(next + 1)
                    .is_none_or(|t| !names.contains(&t.text))
            {
                problems.push(format!("crates/orch-daemon/src/{file}:{}: unreviewed public bootstrap function or method; no store/engine escape or generic execution facade", token.line));
            }
        }
    }
    Ok(problems)
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
    fn edge_fixture() -> Edges {
        let mut actual: Edges = ALLOWED
            .iter()
            .map(|(name, deps)| {
                (
                    (*name).to_owned(),
                    deps.iter()
                        .map(|dep| Edge {
                            name: (*dep).to_owned(),
                            kind: "normal".into(),
                            rename: None,
                            target: None,
                            optional: false,
                            features: vec![],
                        })
                        .collect(),
                )
            })
            .collect();
        for (name, deps) in DEV_ALLOWED {
            for dep in *deps {
                actual.get_mut(*name).unwrap().push(Edge {
                    name: (*dep).to_owned(),
                    kind: "dev".into(),
                    rename: None,
                    target: None,
                    optional: false,
                    features: match *dep {
                        "orch-core" => vec!["test-support".into()],
                        "orch-daemon" => vec!["testing".into()],
                        _ => vec![],
                    },
                });
            }
        }
        assert!(graph_problems(&actual).is_empty());
        actual
    }

    #[test]
    fn declared_kinds_aliases_targets_and_optional_edges_cannot_bypass_policy() {
        for owner in ["orch-cli", "orch-client", "orch-tui"] {
            for dep in ["orch-engine", "orch-infra", "orch-peer"] {
                for kind in ["normal", "build"] {
                    for variant in 0..4 {
                        let mut actual = edge_fixture();
                        actual.get_mut(owner).unwrap().push(Edge {
                            name: dep.into(),
                            kind: kind.into(),
                            rename: (variant == 1).then(|| "harmless".into()),
                            target: (variant == 2).then(|| "cfg(target_os = \"none\")".into()),
                            optional: variant == 3,
                            features: vec![],
                        });
                        let problems = graph_problems(&actual).join("\n");
                        assert!(
                            problems.contains("declared-edge policy"),
                            "{owner} {dep} {kind} {variant}: {problems}"
                        );
                    }
                }
            }
        }
        // Even an otherwise allowed edge cannot quietly change its identity.
        for variant in 0..3 {
            let mut actual = edge_fixture();
            let edge = &mut actual.get_mut("orch-cli").unwrap()[0];
            match variant {
                0 => edge.rename = Some("alias".into()),
                1 => edge.target = Some("cfg(unix)".into()),
                _ => edge.optional = true,
            }
            assert!(
                graph_problems(&actual)
                    .iter()
                    .any(|p| p.contains("declared-edge policy"))
            );
        }
    }

    #[test]
    fn metadata_keeps_inactive_declared_edge_identity_and_unprefixed_members() {
        let metadata = serde_json::json!({"workspace_members":["cli", "helper"], "packages":[
            {"id":"cli", "name":"orch-cli", "dependencies":[{"name":"helper", "kind":"build", "rename":"innocent", "target":"cfg(no_such_target)", "optional":true, "features":["testing"]}]},
            {"id":"helper", "name":"helper", "dependencies":[]},
            {"id":"external", "name":"external", "dependencies":[]}
        ]});
        let actual = workspace_edges(&metadata).unwrap();
        let edge = &actual["orch-cli"][0];
        assert_eq!(edge.kind, "build");
        assert_eq!(edge.rename.as_deref(), Some("innocent"));
        assert_eq!(edge.target.as_deref(), Some("cfg(no_such_target)"));
        assert!(edge.optional);
        assert_eq!(edge.features, ["testing"]);
        assert!(!actual.contains_key("external"));
        assert!(
            graph_problems(&actual)
                .iter()
                .any(|p| p.contains("unclassified workspace crate"))
        );
    }

    #[test]
    fn pure_clients_cannot_reach_state_through_an_indirect_helper() {
        let mut actual = edge_fixture();
        actual.get_mut("orch-client").unwrap().push(Edge {
            name: "helper".into(),
            kind: "normal".into(),
            rename: None,
            target: None,
            optional: false,
            features: vec![],
        });
        actual.insert(
            "helper".into(),
            vec![Edge {
                name: "orch-infra".into(),
                kind: "build".into(),
                rename: None,
                target: None,
                optional: false,
                features: vec![],
            }],
        );
        assert!(graph_problems(&actual).iter().any(|p| {
            p.contains("pure client `orch-client` reaches state capability `orch-infra`")
        }));
        assert!(
            !graph_problems(&edge_fixture())
                .iter()
                .any(|p| p.contains("pure client")),
            "combined CLI bootstrap is intentional"
        );
    }

    #[test]
    fn stale_kind_allowances_and_promoted_test_features_fail() {
        let mut missing_owner = edge_fixture();
        missing_owner.remove("orch-infra");
        assert!(
            graph_problems(&missing_owner)
                .iter()
                .any(|p| p.contains("stale dev allowance owner `orch-infra`"))
        );
        let mut actual = edge_fixture();
        actual
            .get_mut("orch-cli")
            .unwrap()
            .retain(|e| !(e.kind == "dev" && e.name == "orch-infra"));
        assert!(
            graph_problems(&actual)
                .iter()
                .any(|p| p.contains("stale dev allowance `orch-cli` -> `orch-infra`"))
        );
        let edge = actual
            .get_mut("orch-cli")
            .unwrap()
            .iter_mut()
            .find(|e| e.name == "orch-daemon" && e.kind == "normal")
            .unwrap();
        edge.features.push("testing".into());
        assert!(
            graph_problems(&actual)
                .iter()
                .any(|p| p.contains("promotes a test API"))
        );
        let metadata = serde_json::json!({"packages":[{"name":"orch-cli", "features":{"escape":["owner?/testing"]}}]});
        assert!(
            promoted_test_features(&metadata)
                .join("\n")
                .contains("owner?/testing")
        );
    }

    #[test]
    fn production_imports_aliases_and_reexports_fail_in_every_cli_frontend() {
        for file in [
            "commands/task.rs",
            "mcp/tools/task.rs",
            "tui/backend.rs",
            "web/mod.rs",
        ] {
            for text in [
                "use orch_infra::storage::FileTaskStore;",
                "fn f() { orch_engine::Container::build(); }",
                "extern crate orch_peer as innocent;",
                "pub use orch_infra as innocent;",
                "pub use orch_infra::{storage as innocent};",
                "use r#orch_infra :: storage;",
                "#[cfg(test)] mod tests { use orch_infra::Paths; } use orch_infra::Paths;",
            ] {
                let problems = cli_file_problems(file, text).join("\n");
                assert!(
                    problems.contains(file) && problems.contains("CLI_SOURCE_SITES"),
                    "{text}: {problems}"
                );
            }
        }
    }

    #[test]
    fn daemon_sites_permit_only_named_bootstrap_symbols_and_never_testing() {
        for (file, names) in CLI_DAEMON_SITES {
            for name in *names {
                assert!(cli_file_problems(file, &format!("orch_daemon::{name}")).is_empty());
            }
            for text in [
                "orch_daemon::testing::DaemonHarness",
                "use orch_daemon as owner;",
                "pub use orch_daemon::*;",
                "orch_daemon::execute(|stores| stores)",
                "orch_daemon::Container",
                "orch_daemon::InitOutcome::execute(|stores| stores)",
            ] {
                assert!(!cli_file_problems(file, text).is_empty(), "{text}");
            }
        }
        assert!(!cli_file_problems("lib.rs", "orch_daemon::initialize(options)").is_empty());
        assert!(
            cli_file_problems(
                "commands/init.rs",
                "#[cfg(test)] fn fixture() { orch_daemon::testing::DaemonHarness; }"
            )
            .is_empty()
        );
    }

    #[test]
    fn cfg_classification_is_item_local_and_strings_are_not_imports() {
        for text in [
            "#[cfg(test)] mod tests { use orch_infra::Paths; }",
            "#[cfg(all(test, target_os = \"macos\"))] fn f() { orch_infra::Paths; }",
            "let target = \"orch_infra::process::spawn\"; /* orch_engine /* nested */ */",
            "let s = r###\"orch_infra::Paths\"###; let c = '\"'; // orch_peer",
            "tokio::spawn(async {}); tokio::task::spawn_blocking(f);",
        ] {
            assert!(cli_file_problems("lib.rs", text).is_empty(), "{text}");
        }
        for text in [
            "#[cfg(any(test, unix))] fn f() { orch_infra::Paths; }",
            "#[cfg(not(test))] fn f() { orch_infra::Paths; }",
            "#[cfg(test)] mod tests {} use orch_infra::Paths;",
        ] {
            assert!(!cli_file_problems("lib.rs", text).is_empty(), "{text}");
        }
    }

    struct SourceFixture(std::path::PathBuf);
    impl SourceFixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = loop {
                let candidate = std::env::temp_dir().join(format!(
                    "crate-graph-{}-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                match std::fs::create_dir(&candidate) {
                    Ok(()) => break candidate,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create fixture: {error}"),
                }
            };
            let fixture = Self(path);
            for (root, parent, declaration) in CLI_TEST_ROOTS {
                fixture.write(root, "use orch_infra::Paths;");
                fixture.write(parent, declaration);
            }
            for (file, _) in CLI_DAEMON_SITES {
                fixture.write(file, "orch_daemon::DaemonError");
            }
            fixture
        }
        fn write(&self, file: &str, text: &str) {
            let path = self.0.join("crates/orch-cli/src").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
    impl Drop for SourceFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn each_dedicated_test_root_needs_its_own_guard_and_no_production_route() {
        let fixture = SourceFixture::new();
        assert!(cli_source_sites(&fixture.0).unwrap().is_empty());
        for (root, parent, declaration) in CLI_TEST_ROOTS {
            let unguarded = declaration.replacen("#[cfg(test)]", "", 1).replacen(
                "#[cfg(all(test, target_os = \"macos\"))]",
                "",
                1,
            );
            fixture.write(parent, &unguarded);
            let problems = cli_source_sites(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains(root) && problems.contains("guarded declaration"),
                "{problems}"
            );
            fixture.write(parent, declaration);
            assert!(cli_source_sites(&fixture.0).unwrap().is_empty());
        }
        fixture.write(
            "web/escape.rs",
            "#[path = \"remote_tests.rs\"] mod innocent;",
        );
        assert!(
            cli_source_sites(&fixture.0)
                .unwrap()
                .iter()
                .any(|p| p.contains("loaded in production"))
        );
    }

    #[test]
    fn stale_bootstrap_source_sites_fail_bidirectionally() {
        let fixture = SourceFixture::new();
        fixture.write(
            "commands/init.rs",
            "// orch_daemon is gone\nlet label = \"orch_daemon::initialize\";",
        );
        assert!(
            cli_source_sites(&fixture.0)
                .unwrap()
                .iter()
                .any(|p| p.contains("stale CLI_DAEMON_SITES entry `commands/init.rs`"))
        );
    }

    #[test]
    fn bootstrap_facade_cannot_return_a_store_or_accept_an_execution_closure() {
        let fixture = SourceFixture::new();
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("crates/orch-daemon/src");
        let dest = fixture.0.join("crates/orch-daemon/src");
        std::fs::create_dir_all(&dest).unwrap();
        for file in ["bootstrap.rs", "lifecycle.rs", "discovery.rs"] {
            std::fs::copy(src.join(file), dest.join(file)).unwrap();
        }
        assert!(bootstrap_contracts(&fixture.0).unwrap().is_empty());
        for (file, old, new) in [
            (
                "bootstrap.rs",
                "std::result::Result<InitOutcome, DaemonError>",
                "std::result::Result<orch_infra::storage::FileTaskStore, DaemonError>",
            ),
            (
                "bootstrap.rs",
                "pub adapter: String,",
                "pub adapter: orch_engine::Container,",
            ),
            (
                "bootstrap.rs",
                "pub struct InitOutcome {",
                "pub fn execute<F: FnOnce()>(f: F) { f() } pub struct InitOutcome {",
            ),
            (
                "bootstrap.rs",
                "initialize(options: InitOptions)",
                "initialize<F: FnOnce()>(options: InitOptions, execute: F)",
            ),
            (
                "lifecycle.rs",
                "pub async fn run(mut self) -> Result<DaemonExit, DaemonError>",
                "pub async fn run(mut self) -> Result<orch_engine::Container, DaemonError>",
            ),
        ] {
            let original = std::fs::read_to_string(dest.join(file)).unwrap();
            assert!(original.contains(old), "mutation must apply");
            std::fs::write(dest.join(file), original.replacen(old, new, 1)).unwrap();
            let problems = bootstrap_contracts(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains(file)
                    && (problems.contains("bootstrap type boundary changed")
                        || problems.contains("unreviewed public bootstrap")),
                "{problems}"
            );
            std::fs::write(dest.join(file), original).unwrap();
        }
    }

    #[test]
    fn client_process_features_include_build_target_alias_and_feature_forwarding() {
        let fixture = SourceFixture::new();
        std::fs::create_dir_all(fixture.0.join("crates/orch-client/src")).unwrap();
        for kind in [Value::Null, Value::String("build".into())] {
            for (dependency, capability) in
                [("tokio", "process"), ("tokio", "full"), ("nix", "process")]
            {
                let metadata = serde_json::json!({"packages":[{"name":"orch-client", "dependencies":[
                    {"name":dependency,"rename":"alias","kind":kind,"target":"cfg(no_such_target)","features":[capability]}
                ]}]});
                assert!(
                    !client_process_capabilities(&fixture.0, &metadata)
                        .unwrap()
                        .is_empty()
                );
                let metadata = serde_json::json!({"packages":[{"name":"orch-client", "dependencies":[
                    {"name":dependency,"rename":"alias","kind":kind,"features":[]}
                ],"features":{"escape":[format!("alias?/{capability}")]}}]});
                assert!(
                    !client_process_capabilities(&fixture.0, &metadata)
                        .unwrap()
                        .is_empty()
                );
            }
        }
        let metadata = serde_json::json!({"packages":[{"name":"orch-client", "dependencies":[{"name":"tokio","features":["rt","sync","net"]}]}]});
        assert!(
            client_process_capabilities(&fixture.0, &metadata)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn relative_fixture_paths_must_not_promote_test_code() {
        for text in [
            "#[path = \"../mcp/tools/remote_tests.rs\"] mod innocent;",
            "#[path = r\"../commands/org_testing.rs\"] mod innocent;",
            "include!(\"../mcp/testing.rs\");",
            "include!(concat!(\"..\", \"/mcp/testing.rs\"));",
        ] {
            let problems = cli_file_problems("web/escape.rs", text).join("\n");
            assert!(
                problems.contains("web/escape.rs") && problems.contains("loaded in production"),
                "{text}: {problems}"
            );
        }
        for text in [
            "let diagnosis = \"remote_tests.rs\";",
            "let path = \"../mcp/testing.rs\";",
            "#[cfg(test)] include!(\"../mcp/testing.rs\");",
        ] {
            assert!(
                cli_file_problems("web/escape.rs", text).is_empty(),
                "{text}"
            );
        }
    }
}
