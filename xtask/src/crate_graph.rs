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
    problems.extend(boundary_build_targets(&metadata));
    problems.extend(boundary_source_loading(repo_root)?);
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
    uses_default_features: bool,
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
                    uses_default_features: dep["uses_default_features"].as_bool().unwrap_or(true),
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
                    .any(|f| TEST_API_FEATURES.iter().any(|(_, feature)| *feature == f))
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

/// Test APIs are permitted through dev edges only.
const TEST_API_FEATURES: &[(&str, &str)] =
    &[("orch-daemon", "testing"), ("orch-core", "test-support")];

/// Cargo's local aliases and dependency forwarding form a finite fixed point.
/// Resolve declared edges rather than resolved nodes (which unify dev features).
fn selected_workspace_features(
    metadata: &Value,
    edges: &Edges,
    root: &Edge,
) -> BTreeMap<String, BTreeSet<String>> {
    let packages: BTreeMap<_, _> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["name"].as_str()?, p)))
        .collect();
    let mut selected = BTreeMap::<String, BTreeSet<String>>::new();
    let mut active = BTreeSet::<(String, usize)>::new();
    select_edge_features(&mut selected, root);
    loop {
        let before = (selected.clone(), active.clone());
        for (name, features) in &before.0 {
            let deps = edges.get(name).map_or(&[][..], Vec::as_slice);
            for (i, dep) in deps.iter().enumerate().filter(|(_, d)| d.kind != "dev") {
                if !dep.optional {
                    active.insert((name.clone(), i));
                }
            }
            let Some(package) = packages.get(name.as_str()) else {
                continue;
            };
            for feature in features {
                for value in package["features"][feature]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if let Some((alias, forwarded)) = value.split_once('/') {
                        let weak = alias.ends_with('?');
                        let alias = alias.trim_end_matches('?');
                        for (i, dep) in deps.iter().enumerate().filter(|(_, d)| {
                            d.kind != "dev" && d.rename.as_deref().unwrap_or(&d.name) == alias
                        }) {
                            if !weak {
                                active.insert((name.clone(), i));
                            }
                            if active.contains(&(name.clone(), i)) {
                                selected
                                    .entry(dep.name.clone())
                                    .or_default()
                                    .insert(forwarded.to_owned());
                            }
                        }
                    } else if let Some(alias) = value.strip_prefix("dep:") {
                        for (i, _) in deps.iter().enumerate().filter(|(_, d)| {
                            d.kind != "dev" && d.rename.as_deref().unwrap_or(&d.name) == alias
                        }) {
                            active.insert((name.clone(), i));
                        }
                    } else {
                        selected
                            .entry(name.clone())
                            .or_default()
                            .insert(value.to_owned());
                        // Older Cargo's implicit optional-dependency feature.
                        for (i, _) in deps.iter().enumerate().filter(|(_, d)| {
                            d.kind != "dev"
                                && d.optional
                                && d.rename.as_deref().unwrap_or(&d.name) == value
                        }) {
                            active.insert((name.clone(), i));
                        }
                    }
                }
            }
        }
        for (name, i) in &active {
            select_edge_features(&mut selected, &edges[name][*i]);
        }
        if before == (selected.clone(), active.clone()) {
            break;
        }
    }
    selected
}

fn select_edge_features(selected: &mut BTreeMap<String, BTreeSet<String>>, edge: &Edge) {
    let features = selected.entry(edge.name.clone()).or_default();
    features.extend(edge.features.iter().cloned());
    if edge.uses_default_features {
        features.insert("default".to_owned());
    }
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
                if feature.split_once('/').is_some_and(|(_, name)| {
                    TEST_API_FEATURES
                        .iter()
                        .any(|(_, feature)| *feature == name)
                }) {
                    problems.push(format!(
                        "{} feature `{feature}` promotes a dependency test API into production",
                        package["name"]
                    ));
                }
            }
        }
    }
    if let Ok(edges) = workspace_edges(metadata) {
        for (owner, deps) in &edges {
            // A package's default may forward an alias into a dependency even
            // when that dependency has default-features=false on its edge.
            let default_root = Edge {
                name: owner.clone(),
                kind: "normal".to_owned(),
                rename: None,
                target: None,
                optional: false,
                features: Vec::new(),
                uses_default_features: true,
            };
            let defaults = selected_workspace_features(metadata, &edges, &default_root);
            for (package, feature) in TEST_API_FEATURES {
                if *package != owner && defaults.get(*package).is_some_and(|s| s.contains(*feature))
                {
                    problems.push(format!("`{owner}` default feature closure selects dependency test API `{package}/{feature}` (TEST_API_FEATURES)"));
                }
            }
            for edge in deps.iter().filter(|e| e.kind != "dev") {
                let selected = selected_workspace_features(metadata, &edges, edge);
                for (package, feature) in TEST_API_FEATURES {
                    if selected.get(*package).is_some_and(|s| s.contains(*feature)) {
                        problems.push(format!("`{owner}` -> `{}` selects test API `{package}/{feature}` through its {} feature closure (TEST_API_FEATURES)", edge.name, edge.kind));
                    }
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

const BOUNDARY_CRATES: &[&str] = &["orch-cli", "orch-client", "orch-proto", "orch-tui"];
/// Exact (crate, source path) approvals. No boundary crate has a build script.
const BOUNDARY_BUILD_TARGETS: &[(&str, &str)] = &[];

fn boundary_build_targets(metadata: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let name = package["name"].as_str().unwrap_or("");
        if !BOUNDARY_CRATES.contains(&name) {
            continue;
        }
        for target in package["targets"].as_array().into_iter().flatten() {
            if target["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "custom-build"))
            {
                let path = target["src_path"].as_str().unwrap_or("<missing source>");
                if !BOUNDARY_BUILD_TARGETS.contains(&(name, path)) {
                    problems.push(format!("`{name}` custom-build target `{path}` is unreviewed (BOUNDARY_BUILD_TARGETS)"));
                }
            }
        }
    }
    problems
}

/// Plain string paths only. Escaped/computed paths fail closed; data-loading
/// macros do not compile Rust and are intentionally outside this rule.
fn source_path_literal<'a>(token: &Token<'a>) -> Option<&'a str> {
    if !token.literal {
        return None;
    }
    if let Some(path) = token
        .text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
    {
        return (!path.contains('\\')).then_some(path);
    }
    let raw = token.text.strip_prefix('r')?;
    let hashes = raw.bytes().take_while(|c| *c == b'#').count();
    raw.get(hashes..)?
        .strip_prefix('"')?
        .strip_suffix(&format!("\"{}", "#".repeat(hashes)))
}

fn boundary_source_loading(repo_root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    // Bootstrap aliases/impls must also stay inside the enumerated daemon
    // root; otherwise an ordinary include/path could hide their declarations.
    for name in BOUNDARY_CRATES.iter().chain([&"orch-daemon"]) {
        let src = repo_root.join("crates").join(name).join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_rust_files(&src, &mut files)?;
        let enumerated: BTreeSet<_> = files
            .iter()
            .map(std::fs::canonicalize)
            .collect::<std::io::Result<_>>()?;
        let canonical_src = src.canonicalize()?;
        for file in files {
            let relative = file
                .strip_prefix(&src)?
                .to_string_lossy()
                .replace('\\', "/");
            if *name == "orch-cli" && CLI_TEST_ROOTS.iter().any(|(root, _, _)| *root == relative) {
                continue;
            }
            let text = std::fs::read_to_string(&file)?;
            let tokens = production_tokens(&rust_tokens(&text));
            let mut attribute_end = 0;
            for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.literal) {
                if token.text == "use" {
                    let end = (i + 1..tokens.len())
                        .find(|&j| tokens[j].text == ";")
                        .unwrap_or(tokens.len());
                    if tokens[i + 1..end]
                        .iter()
                        .any(|t| !t.literal && t.text == "include")
                    {
                        problems.push(format!("crates/{name}/src/{relative}:{}: importing or renaming the include macro is forbidden (BOUNDARY_SOURCE_LOADING)", token.line));
                    }
                }
                if token.text == "#" {
                    let start =
                        i + 1 + usize::from(tokens.get(i + 1).is_some_and(|t| t.text == "!"));
                    if tokens.get(start).is_some_and(|t| t.text == "[") {
                        attribute_end = group_end(&tokens, start);
                    }
                }
                let arg = if token.text == "include"
                    && tokens.get(i + 1).is_some_and(|t| t.text == "!")
                    && tokens
                        .get(i + 2)
                        .is_some_and(|t| matches!(t.text, "(" | "[" | "{"))
                {
                    Some(&tokens[i + 3..group_end(&tokens, i + 2).saturating_sub(1)])
                } else if i < attribute_end
                    && token.text == "path"
                    && tokens.get(i + 1).is_some_and(|t| t.text == "=")
                {
                    // cfg_attr (including nested cfg_attr) can set the same
                    // module path as a direct attribute. Stop at its value's
                    // delimiter, not the end of the enclosing attribute.
                    let mut end = i + 2;
                    while end < attribute_end && !matches!(tokens[end].text, "," | ")" | "]" | "}")
                    {
                        end = group_end(&tokens, end);
                    }
                    Some(&tokens[i + 2..end])
                } else {
                    None
                };
                let Some(arg) = arg else { continue };
                let arg = if arg.last().is_some_and(|t| t.text == ",") {
                    &arg[..arg.len() - 1]
                } else {
                    arg
                };
                let target = (arg.len() == 1)
                    .then(|| source_path_literal(&arg[0]))
                    .flatten()
                    .and_then(|path| file.parent()?.join(path).canonicalize().ok());
                if !target.is_some_and(|p| {
                    p.starts_with(&canonical_src) && p.is_file() && enumerated.contains(&p)
                }) {
                    problems.push(format!("crates/{name}/src/{relative}:{}: production code loading must be a plain literal naming a file inside the enumerated crate source root (BOUNDARY_SOURCE_LOADING)", token.line));
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
    raw_identifier: bool,
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
                        raw_identifier: true,
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
                raw_identifier: false,
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
                // Fields/arms end at a comma or their enclosing closer. A
                // block-ended arm/statement/macro needs no comma. Items can
                // contain generic commas (Result<(), E>) before the body.
                let mut prefix = i;
                if tokens.get(prefix).is_some_and(|t| t.text == "pub") {
                    prefix += 1;
                    if tokens.get(prefix).is_some_and(|t| t.text == "(") {
                        prefix = group_end(tokens, prefix);
                    }
                }
                let item = tokens.get(prefix).is_some_and(|t| {
                    matches!(
                        t.text,
                        "fn" | "mod"
                            | "use"
                            | "impl"
                            | "struct"
                            | "enum"
                            | "const"
                            | "static"
                            | "type"
                            | "trait"
                            | "extern"
                            | "async"
                            | "unsafe"
                            | "macro_rules"
                    )
                });
                while i < tokens.len() {
                    let text = tokens[i].text;
                    if matches!(text, ")" | "]" | "}") {
                        break;
                    }
                    i = group_end(tokens, i);
                    if text == ";" || (item && text == "{") || (!item && text == ",") {
                        break;
                    }
                    // Neither `else`, postfix `?`, nor a single postfix `.`
                    // can begin a new arm/statement (`r#else!` is an identifier,
                    // not the else keyword). A range CAN begin with
                    // `..`, so keep only a dot not followed by another dot.
                    // An early stop otherwise only exposes tokens.
                    if !item
                        && text == "{"
                        && !tokens.get(i).is_some_and(|t| {
                            (t.text == "else" && !t.raw_identifier)
                                || t.text == "?"
                                || (t.text == "."
                                    && tokens.get(i + 1).is_none_or(|t| t.text != "."))
                        })
                    {
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
        if token.text == "use" {
            let end = (i + 1..tokens.len())
                .find(|&j| tokens[j].text == ";")
                .unwrap_or(tokens.len());
            let import = &tokens[i + 1..end];
            let mut begin = i;
            while begin > 0 && !matches!(tokens[begin - 1].text, ";" | "{" | "}") {
                begin -= 1;
            }
            let visible = tokens[begin..i].iter().any(|t| t.text == "pub");
            let renamed = import.iter().any(|t| t.text == "as");
            let exception = file == "commands/init.rs"
                && same_tokens(
                    &tokens[begin..end],
                    &rust_tokens("pub use orch_daemon::InitOutcome"),
                );
            if import.iter().any(|t| t.text == "orch_daemon") && (visible || renamed) && !exception
            {
                problems.push(format!("crates/orch-cli/src/{file}:{}: bootstrap use visibility or alias is unreviewed (CLI_DAEMON_SITES)", token.line));
            }
        }
        if ["orch_infra", "orch_engine", "orch_peer"].contains(&token.text) {
            problems.push(format!("crates/orch-cli/src/{file}:{}: production owner import `{}` is forbidden (CLI_DAEMON_SITES: no production owner imports)", token.line, token.text));
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
            "pub struct InitOptions { pub project_root: PathBuf, pub name: Option<String>, pub adapter: Option<String>, pub prompt_mode: Option<orch_domain::PromptMode>, pub interactive: bool, }",
        ),
        (
            "lifecycle.rs",
            "pub struct StartOptions { pub project_root: PathBuf, pub socket: Option<PathBuf>, pub detach: bool, pub dispatch: DispatchMode, pub from_agent: bool, pub listen: Option<String>, pub peer_name: Option<String>, }",
        ),
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
    problems.extend(bootstrap_enums(repo_root)?);
    problems.extend(bootstrap_impls(repo_root)?);
    Ok(problems)
}

/// Pin variants and payloads, while leaving derive/error/default attributes
/// free to change. A new payload is just as much an API change as a field.
fn bootstrap_enums(repo_root: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for (file, name, contract) in [
        (
            "error.rs",
            "DaemonError",
            "pub enum DaemonError {
                AlreadyRunning(Option<u32>), LegacyLockHeld(u32),
                RefusedInAgentContext, InvalidArguments(String), NotInitialized,
                Core(orch_core::error::Error),
                Io { context: String, source: std::io::Error, },
                Operation(Box<ErrorBody>), StopIncomplete,
                Unimplemented { what: &'static str, owner: &'static str, },
            }",
        ),
        (
            "lifecycle.rs",
            "DispatchMode",
            "pub enum DispatchMode { Off, On, }",
        ),
    ] {
        let text = std::fs::read_to_string(repo_root.join("crates/orch-daemon/src").join(file))?;
        let tokens = rust_tokens(&text);
        // Drop attributes only, including those on tuple payload fields.
        // Do not grant cfg(test) an exemption from the exact variant list.
        let mut plain = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            if tokens[i].text == "#" && tokens.get(i + 1).is_some_and(|t| t.text == "[") {
                i = group_end(&tokens, i + 1);
            } else {
                plain.push(tokens[i].clone());
                i += 1;
            }
        }
        let expected = rust_tokens(contract);
        if !plain
            .windows(expected.len())
            .any(|w| same_tokens(w, &expected))
        {
            problems.push(format!("crates/orch-daemon/src/{file}: bootstrap enum boundary changed: {name} variants and payloads must match the reviewed list (BOOTSTRAP_TYPES)"));
        }
    }
    Ok(problems)
}

/// Every daemon type named by CLI_DAEMON_SITES, plus PreparedStart returned
/// by prepare_start. No source module may add an unreviewed implementation.
const BOOTSTRAP_TYPES: &[&str] = &[
    "InitOutcome",
    "InitOptions",
    "StartOptions",
    "DaemonExit",
    "PreparedStart",
    "DispatchMode",
    "DaemonError",
];
const BOOTSTRAP_IMPLS: &[(&str, &str, &[&str])] = &[
    (
        "lifecycle.rs",
        "impl DaemonExit",
        &["pub const CLEAN: DaemonExit"],
    ),
    (
        "lifecycle.rs",
        "impl PreparedStart",
        &[
            "pub async fn run(mut self) -> Result<DaemonExit, DaemonError>",
            "fn bring_up(self) -> BringUp",
        ],
    ),
    (
        "error.rs",
        "impl DaemonError",
        &["pub fn exit_code(&self) -> i32"],
    ),
    (
        "error.rs",
        "impl From<&DaemonError> for ErrorBody",
        &["fn from(error: &DaemonError) -> Self"],
    ),
];

/// Locate a top-level separator without mistaking generic defaults, const
/// blocks or function arrows for the end of a declaration/header. This is a
/// lexical tripwire (no parser dependency is permitted for this gate).
fn declaration_separator(tokens: &[Token<'_>], start: usize, separators: &[&str]) -> usize {
    let mut angle = 0usize;
    let mut i = start;
    while i < tokens.len() {
        let text = tokens[i].text;
        if angle == 0 && separators.contains(&text) {
            return i;
        }
        match text {
            "<" => angle += 1,
            ">" if i == 0 || tokens[i - 1].text != "-" => angle = angle.saturating_sub(1),
            _ => {}
        }
        i = group_end(tokens, i);
    }
    tokens.len()
}

// The one existing blanket impl has no members and lives in the test API.
// Any other type-parameter impl could apply to a bootstrap type without ever
// spelling its name. Lifetime-only impls on fixed receivers are not blanket.
const BOOTSTRAP_BLANKET_IMPLS: &[(&str, &str)] = &[(
    "testing/mod.rs",
    "impl<T: AsyncRead + AsyncWrite + Send + Unpin> ClientStream for T",
)];

fn impl_has_type_parameters(header: &[Token<'_>]) -> bool {
    if header.get(1).is_none_or(|t| t.text != "<") {
        return false;
    }
    let mut angle = 1usize;
    let mut start = true;
    let mut i = 2;
    while i < header.len() && angle > 0 {
        let text = header[i].text;
        if start && !matches!(text, "'" | "const" | ">") {
            return true;
        }
        start = false;
        match text {
            "<" => angle += 1,
            ">" if header[i - 1].text != "-" => angle -= 1,
            "," if angle == 1 => start = true,
            _ => {}
        }
        i = group_end(header, i);
    }
    false
}

/// Follow names globally and conservatively, including nested/cfg-gated code.
/// A wrapper/projection is tainted too: it is rejected below, never mistaken
/// for a successfully resolved identity alias. Names are not scope-resolved;
/// shadowing can cause a rejection, not grant an unreviewed implementation.
fn bootstrap_type_aliases(sources: &[(String, String)]) -> BTreeMap<String, String> {
    let mut aliases: BTreeMap<_, _> = BOOTSTRAP_TYPES
        .iter()
        .map(|t| (t.to_string(), t.to_string()))
        .collect();
    loop {
        let before = aliases.clone();
        for (_, text) in sources {
            let tokens = rust_tokens(text);
            for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.literal) {
                if token.text == "type"
                    && let Some(name) = tokens.get(i + 1)
                {
                    let end = declaration_separator(&tokens, i + 2, &[";"]);
                    if let Some(target) = tokens[i + 2..end]
                        .iter()
                        .filter(|t| !t.literal)
                        .find_map(|t| before.get(t.text))
                    {
                        aliases
                            .entry(name.text.to_owned())
                            .or_insert_with(|| target.clone());
                    }
                }
                if token.text == "as"
                    && i > 0
                    && let Some(target) = before.get(tokens[i - 1].text)
                    && let Some(alias) = tokens.get(i + 1)
                {
                    aliases
                        .entry(alias.text.to_owned())
                        .or_insert_with(|| target.clone());
                }
            }
        }
        if before == aliases {
            return aliases;
        }
    }
}

/// An identity alias is a path ending in a tracked name, with optional generic
/// arguments. `Option<T>`, `(T,)`, `&T` and associated projections are not T.
fn bootstrap_alias_target<'a>(
    tokens: &'a [Token<'a>],
    aliases: &BTreeMap<String, String>,
) -> Option<&'a str> {
    let mut i = 0;
    if tokens.first().is_some_and(|t| t.text == ":") {
        if tokens.get(1).is_none_or(|t| t.text != ":") {
            return None;
        }
        i = 2;
    }
    loop {
        let name = tokens.get(i)?;
        if name.literal
            || !name
                .text
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            return None;
        }
        i += 1;
        if tokens.get(i).is_some_and(|t| t.text == ":")
            && tokens.get(i + 1).is_some_and(|t| t.text == ":")
        {
            i += 2;
            continue;
        }
        if !aliases.contains_key(name.text) {
            return None;
        }
        if i == tokens.len() {
            return Some(name.text);
        }
        if tokens[i].text == "<" {
            // Balanced generic arguments must occupy the entire suffix.
            // Count them separately to reject `Alias<T>::Associated`.
            let mut depth = 0usize;
            let mut j = i;
            while j < tokens.len() {
                match tokens[j].text {
                    "<" => depth += 1,
                    ">" if j == 0 || tokens[j - 1].text != "-" => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return (j + 1 == tokens.len()).then_some(name.text);
                        }
                    }
                    _ => {}
                }
                j = group_end(tokens, j);
            }
        }
        return None;
    }
}

// These are expression/data macros used with bootstrap values today. Their
// arguments are still scanned; this is not an exemption for nested impls.
// Macro name binding and procedural expansion remain source-review limits.
const BOOTSTRAP_VALUE_MACROS: &[&str] = &[
    "matches",
    "assert",
    "assert_eq",
    "assert_ne",
    "format",
    "write",
    "writeln",
    "vec",
];

fn bootstrap_declarations(
    sources: &[(String, String)],
    aliases: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut declarations: BTreeMap<&str, Vec<String>> = BOOTSTRAP_TYPES
        .iter()
        .map(|&name| (name, Vec::new()))
        .collect();
    for (file, text) in sources {
        let tokens = rust_tokens(text);
        let production = production_tokens(&tokens);
        for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.literal) {
            if matches!(token.text, "struct" | "enum")
                && let Some(name) = tokens.get(i + 1)
                && let Some(sites) = declarations.get_mut(name.text)
            {
                sites.push(format!("{file}:{}", token.line));
            }
            if token.text == "type"
                && let Some(name) = tokens.get(i + 1)
            {
                let end = declaration_separator(&tokens, i + 2, &[";"]);
                let eq = declaration_separator(&tokens, i + 2, &["=", ";"]);
                let mentioned = tokens[i + 2..end]
                    .iter()
                    .filter(|t| !t.literal)
                    .find_map(|t| aliases.get(t.text));
                if let Some(target) = mentioned {
                    let rhs = if eq < end { &tokens[eq + 1..end] } else { &[] };
                    if bootstrap_alias_target(rhs, aliases).is_none() || name.text == "$" {
                        problems.push(format!("crates/orch-daemon/src/{file}:{}: unresolved bootstrap alias {} mentioning {target} (BOOTSTRAP_IMPLS)", token.line, name.text));
                    }
                }
            }
            // Prevent a payload token in an exact contract being re-bound to
            // an unrelated type/module in the same pinned source file.
            let payloads: &[&str] = match file.as_str() {
                "bootstrap.rs" => &[
                    "PathBuf",
                    "Option",
                    "String",
                    "orch_domain",
                    "PromptMode",
                    "bool",
                ],
                "lifecycle.rs" => &["PathBuf", "Option", "String", "bool", "i32"],
                "error.rs" => &[
                    "Option",
                    "u32",
                    "String",
                    "orch_core",
                    "Error",
                    "std",
                    "io",
                    "Box",
                    "ErrorBody",
                    "str",
                ],
                _ => &[],
            };
            if matches!(
                token.text,
                "type" | "as" | "struct" | "enum" | "union" | "trait" | "mod"
            ) && let Some(name) = tokens.get(i + 1)
                && payloads.contains(&name.text)
            {
                problems.push(format!("crates/orch-daemon/src/{file}:{}: pinned payload name {} cannot be rebound by a declaration/rename (BOOTSTRAP_TYPES)", token.line, name.text));
            }
            if token.text == "use" && tokens.get(i + 1).is_none_or(|t| t.text != "<") {
                let end = declaration_separator(&tokens, i + 1, &[";"]);
                let imports = &tokens[i..end];
                let pinned = !payloads.is_empty()
                    && imports.iter().enumerate().any(|(j, t)| {
                        t.text == "*"
                            || (payloads.contains(&t.text)
                                && imports.get(j + 1).is_none_or(|next| next.text != ":"))
                    });
                let reviewed = match file.as_str() {
                    "bootstrap.rs" | "lifecycle.rs" => "use std::path::{Path, PathBuf}",
                    "error.rs" => "use orch_proto::error::{ErrorBody, exit}",
                    _ => "",
                };
                if pinned
                    && !same_tokens(imports, &rust_tokens(reviewed))
                    && !(same_tokens(imports, &rust_tokens("use super::*"))
                        && !production
                            .iter()
                            .any(|t| t.text.as_ptr() == token.text.as_ptr()))
                {
                    problems.push(format!("crates/orch-daemon/src/{file}:{}: unreviewed import binding of a pinned payload name (BOOTSTRAP_TYPES)", token.line));
                }
                if imports
                    .iter()
                    .any(|t| BOOTSTRAP_VALUE_MACROS.contains(&t.text))
                {
                    problems.push(format!("crates/orch-daemon/src/{file}:{}: reviewed value macro names cannot be imported/rebound (BOOTSTRAP_IMPLS)", token.line));
                }
            }
            if token.text == "macro_rules"
                && tokens
                    .get(i + 3)
                    .is_some_and(|t| matches!(t.text, "(" | "[" | "{"))
            {
                let end = group_end(&tokens, i + 3);
                if let Some(target) = tokens[i + 4..end.saturating_sub(1)]
                    .iter()
                    .filter(|t| !t.literal)
                    .find_map(|t| aliases.get(t.text))
                {
                    problems.push(format!("crates/orch-daemon/src/{file}:{}: local macro definition carries bootstrap type {target}; expansion requires review (BOOTSTRAP_IMPLS)", token.line));
                }
            }
            if (token.text == "macro_rules"
                && tokens
                    .get(i + 2)
                    .is_some_and(|t| BOOTSTRAP_VALUE_MACROS.contains(&t.text)))
                || (token.text == "as"
                    && tokens
                        .get(i + 1)
                        .is_some_and(|t| BOOTSTRAP_VALUE_MACROS.contains(&t.text)))
            {
                problems.push(format!("crates/orch-daemon/src/{file}:{}: reviewed value macro names cannot be rebound (BOOTSTRAP_IMPLS)", token.line));
            }
            if tokens.get(i + 1).is_some_and(|t| t.text == "!")
                && tokens
                    .get(i + 2)
                    .is_some_and(|t| matches!(t.text, "(" | "[" | "{"))
                && !BOOTSTRAP_VALUE_MACROS.contains(&token.text)
            {
                let end = group_end(&tokens, i + 2);
                if let Some(target) = tokens[i + 3..end.saturating_sub(1)]
                    .iter()
                    .filter(|t| !t.literal)
                    .find_map(|t| aliases.get(t.text))
                {
                    problems.push(format!("crates/orch-daemon/src/{file}:{}: macro {} carries bootstrap type {target}; expansion requires review (BOOTSTRAP_IMPLS)", token.line, token.text));
                }
            }
        }
    }
    for (name, sites) in declarations {
        if sites.len() != 1 {
            problems.push(format!("crates/orch-daemon/src: bootstrap type {name} requires exactly one raw struct/enum declaration, found {} [{}] (BOOTSTRAP_TYPES)", sites.len(), sites.join(", ")));
        }
    }
    problems
}

fn bootstrap_impls(repo_root: &Path) -> Result<Vec<String>> {
    let src = repo_root.join("crates/orch-daemon/src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files)?;
    let sources: Vec<_> = files
        .iter()
        .map(|file| {
            Ok((
                file.strip_prefix(&src)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                std::fs::read_to_string(file)?,
            ))
        })
        .collect::<Result<_>>()?;
    let aliases = bootstrap_type_aliases(&sources);
    let mut seen = BTreeSet::new();
    let mut seen_blankets = BTreeSet::new();
    let mut problems = bootstrap_declarations(&sources, &aliases);
    for (relative, text) in &sources {
        let tokens = rust_tokens(text);
        for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.literal) {
            if token.text == "struct"
                && tokens.get(i + 1).is_some_and(|t| t.text == "PreparedStart")
            {
                let body = (i + 2..tokens.len()).find(|&j| tokens[j].text == "{");
                if let Some(body) = body
                    && tokens[body + 1..group_end(&tokens, body).saturating_sub(1)]
                        .iter()
                        .any(|t| !t.literal && t.text == "pub")
                {
                    problems.push(format!("crates/orch-daemon/src/{relative}:{}: bootstrap type boundary changed: PreparedStart must have no pub field", token.line));
                }
            }
            if token.text != "impl" {
                continue;
            }
            // Do not skip unfamiliar positions (including macro arguments).
            // A bootstrap-bearing impl Trait/header we cannot classify fails
            // closed. Skip const-generic groups while locating the body.
            let body = declaration_separator(&tokens, i + 1, &["{", ";", ",", ")", "]", "="]);
            if body == tokens.len() {
                continue;
            }
            let header = &tokens[i..body];
            let types: BTreeSet<_> = header
                .iter()
                .filter(|t| !t.literal)
                .filter_map(|t| aliases.get(t.text))
                .collect();
            if types.is_empty() {
                if impl_has_type_parameters(header) {
                    let entry =
                        BOOTSTRAP_BLANKET_IMPLS
                            .iter()
                            .enumerate()
                            .find(|(_, (file, expected))| {
                                file == relative && same_tokens(header, &rust_tokens(expected))
                            });
                    if let Some((index, _)) = entry
                        && tokens[body].text == "{"
                        && group_end(&tokens, body) == body + 2
                        && seen_blankets.insert(index)
                    {
                        continue;
                    }
                    problems.push(format!("crates/orch-daemon/src/{relative}:{}: unreviewed type-parameter impl could apply to a bootstrap type (BOOTSTRAP_IMPLS)", token.line));
                }
                if header.iter().any(|t| t.text == "$") {
                    problems.push(format!("crates/orch-daemon/src/{relative}:{}: unresolved macro impl receiver; it could name a bootstrap type (BOOTSTRAP_IMPLS)", token.line));
                }
                continue;
            }
            let end = group_end(&tokens, body).saturating_sub(1);
            let mut members = Vec::new();
            let mut signatures = Vec::new();
            let mut next = body + 1;
            while next < end {
                let begin = next;
                let mut member = None;
                let mut signature_end = None;
                while next < end {
                    if matches!(tokens[next].text, "fn" | "const" | "type")
                        && let Some(name) = tokens.get(next + 1)
                    {
                        member = Some(format!("{} {}", tokens[next].text, name.text));
                    }
                    let text = tokens[next].text;
                    if matches!(text, "{" | ";" | "=") && signature_end.is_none() {
                        signature_end = Some(next);
                    }
                    next = group_end(&tokens, next);
                    if matches!(text, "{" | ";") {
                        break;
                    }
                }
                members.push(
                    member.unwrap_or_else(|| format!("unreviewed member {}", tokens[begin].text)),
                );
                signatures.push(&tokens[begin..signature_end.unwrap_or(next)]);
                if next < end && tokens[next].text == ";" {
                    next += 1;
                }
            }
            let entry = BOOTSTRAP_IMPLS
                .iter()
                .enumerate()
                .find(|(_, (file, expected, _))| {
                    file == relative && same_tokens(header, &rust_tokens(expected))
                });
            if let Some((index, (_, _, expected))) = entry
                && seen.insert(index)
                && signatures.len() == expected.len()
                && signatures
                    .iter()
                    .zip(*expected)
                    .all(|(got, expected)| same_tokens(got, &rust_tokens(expected)))
            {
                continue;
            }
            problems.push(format!("crates/orch-daemon/src/{relative}:{}: unreviewed impl/method on bootstrap type(s) {}: [{}] (BOOTSTRAP_IMPLS)", token.line, types.into_iter().map(String::as_str).collect::<Vec<_>>().join(", "), members.join(", ")));
        }
    }
    for (i, (file, header)) in BOOTSTRAP_BLANKET_IMPLS.iter().enumerate() {
        if !seen_blankets.contains(&i) {
            problems.push(format!(
                "crates/orch-daemon/src/{file}: stale BOOTSTRAP_BLANKET_IMPLS entry `{header}`"
            ));
        }
    }
    for (i, (file, header, _)) in BOOTSTRAP_IMPLS.iter().enumerate() {
        if !seen.contains(&i) {
            problems.push(format!(
                "crates/orch-daemon/src/{file}: stale BOOTSTRAP_IMPLS entry `{header}`"
            ));
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
                            uses_default_features: true,
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
                    uses_default_features: true,
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
                            uses_default_features: true,
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
            {"id":"cli", "name":"orch-cli", "dependencies":[{"name":"helper", "kind":"build", "rename":"innocent", "target":"cfg(no_such_target)", "optional":true, "uses_default_features":false, "features":["testing"]}]},
            {"id":"helper", "name":"helper", "dependencies":[]},
            {"id":"external", "name":"external", "dependencies":[]}
        ]});
        let actual = workspace_edges(&metadata).unwrap();
        let edge = &actual["orch-cli"][0];
        assert_eq!(edge.kind, "build");
        assert_eq!(edge.rename.as_deref(), Some("innocent"));
        assert_eq!(edge.target.as_deref(), Some("cfg(no_such_target)"));
        assert!(edge.optional);
        assert!(!edge.uses_default_features);
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
            uses_default_features: true,
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
                uses_default_features: true,
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
                    problems.contains(file)
                        && problems.contains("CLI_DAEMON_SITES: no production owner imports"),
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
        let fixture = daemon_fixture();
        let dest = fixture.0.join("crates/orch-daemon/src");
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

    // Reverting the element bound must expose each following production use.
    #[test]
    fn cfg_test_fields_and_match_arms_do_not_hide_production_references() {
        let missed: Vec<_> = [
            "struct S { #[cfg(test)] hidden: u8, value: u8 } fn f() { orch_daemon::initialize(o); }",
            "struct S { #[cfg(test)] pub hidden: u8, value: u8 } fn f() { orch_daemon::initialize(o); }",
            "fn f() { S { #[cfg(test)] hidden: 1, value: { orch_infra::Paths; 2 } }; }",
            "fn f() { match n { #[cfg(test)] 0 => 0, _ => { orch_infra::Paths; 1 } } }",
            "struct S { #[cfg(test)] hidden: u8 } fn f() { orch_daemon::initialize(o); }",
            "fn f() { match n { #[cfg(test)] 0 => { 0 } _ => { orch_daemon::initialize(o); 1 } } }",
            "fn f() { match n { #[cfg(test)] 0 => if n > 1 { 0 } else { 2 } _ => { orch_infra::Paths; 1 } } }",
            "fn f() { #[cfg(test)] { let _ = 1; } orch_daemon::initialize(o); }",
            "#[cfg(test)] thread_local! { static N: u8 = 0; } fn f() { orch_infra::Paths; }",
            "fn f() { match n { #[cfg(test)] 9 => { 0 } ..=5 => { orch_daemon::prepare_start(o); 1 } _ => 2 } }",
            "fn f() { #[cfg(test)] { let _ = 1; } ..orch_infra::Paths::new(p); }",
            "fn f() { #[cfg(test)] {} r#else! { orch_daemon::prepare_start(o); } }",
        ]
        .into_iter()
        .filter(|text| {
            let problems = cli_file_problems("commands/task.rs", text).join("\n");
            !problems.contains("production owner import") && !problems.contains("CLI_DAEMON_SITES")
        })
        .collect();
        assert!(
            missed.is_empty(),
            "hidden production references: {missed:#?}"
        );
        for text in [
            "#[cfg(test)] pub async fn f() -> Result<(), E> { orch_infra::Paths; }",
            "#[cfg(test)] impl<T, E> Trait<T, E> for S { fn f() { orch_infra::Paths; } }",
            "fn f() { match n { #[cfg(test)] 0 => if n > 1 { orch_infra::Paths; 0 } else { orch_infra::Paths; 2 } _ => 1 } }",
            "fn f() { #[cfg(test)] { orch_infra::Paths }.method(orch_infra::Paths); }",
            "fn f() { #[cfg(test)] { orch_infra::Paths }?; }",
        ] {
            assert!(cli_file_problems("lib.rs", text).is_empty(), "{text}");
        }
    }

    #[test]
    fn site_imports_cannot_reexport_or_rename_the_bootstrap_api() {
        for text in [
            "pub use orch_daemon::initialize;",
            "pub(crate) use orch_daemon::initialize;",
            "pub(in crate::commands) use orch_daemon::initialize;",
            "use orch_daemon::initialize as local_init;",
            "pub use orch_daemon::InitOutcome as Outcome;",
            "pub use ::orch_daemon::initialize as local_init;",
        ] {
            let problems = cli_file_problems("commands/init.rs", text).join("\n");
            assert!(problems.contains("CLI_DAEMON_SITES"), "{text}: {problems}");
        }
        assert!(
            cli_file_problems("commands/init.rs", "pub use orch_daemon::InitOutcome;").is_empty()
        );
        assert!(cli_file_problems("commands/init.rs", "use orch_daemon::initialize;").is_empty());
    }

    fn daemon_fixture() -> SourceFixture {
        let fixture = SourceFixture::new();
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("crates/orch-daemon/src");
        let dest = fixture.0.join("crates/orch-daemon/src");
        std::fs::create_dir_all(&dest).unwrap();
        for file in ["bootstrap.rs", "lifecycle.rs", "discovery.rs", "error.rs"] {
            std::fs::copy(src.join(file), dest.join(file)).unwrap();
        }
        std::fs::create_dir_all(dest.join("testing")).unwrap();
        std::fs::copy(src.join("testing/mod.rs"), dest.join("testing/mod.rs")).unwrap();
        fixture
    }

    #[test]
    fn bootstrap_impls_are_checked_across_the_entire_daemon_source_tree() {
        let fixture = daemon_fixture();
        assert!(bootstrap_contracts(&fixture.0).unwrap().is_empty());
        let extra = fixture.0.join("crates/orch-daemon/src/nested");
        std::fs::create_dir(&extra).unwrap();
        for text in [
            "impl InitOutcome { pub fn local_tasks() -> Store { todo!() } }",
            "impl crate::InitOutcome { pub fn local_tasks() -> Store { todo!() } }",
            "impl<T> AsRef<T> for InitOutcome { fn local_tasks(&self) -> &T { todo!() } }",
            "impl InitOutcome {}",
            "#[cfg(test)] impl InitOutcome { fn local_tasks() {} }",
            "type Outcome = InitOutcome; impl Outcome { fn local_tasks() -> Store { todo!() } }",
            "use crate::InitOutcome as Outcome; impl Outcome { fn local_tasks() -> Store { todo!() } }",
            "impl StartOptions { fn local_tasks() -> Store { todo!() } }",
            "impl InitOptions { fn local_tasks() -> Store { todo!() } }",
            "impl DispatchMode { fn local_tasks() -> Store { todo!() } }",
            "impl DaemonExit { fn local_tasks() -> Store { todo!() } }",
            "impl DaemonError { fn local_tasks() -> Store { todo!() } }",
            "impl PreparedStart { fn local_tasks() -> Store { todo!() } }",
        ] {
            std::fs::write(extra.join("escape.rs"), text).unwrap();
            let problems = bootstrap_contracts(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("BOOTSTRAP_IMPLS") && problems.contains("escape.rs"),
                "{text}: {problems}"
            );
            if text.contains("local_tasks") {
                assert!(problems.contains("local_tasks"), "{problems}");
            }
        }
        std::fs::remove_file(extra.join("escape.rs")).unwrap();
        let file = fixture.0.join("crates/orch-daemon/src/lifecycle.rs");
        let original = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            &file,
            original.replacen(
                "impl PreparedStart {",
                "impl PreparedStart { pub fn local_tasks() {}",
                1,
            ),
        )
        .unwrap();
        assert!(
            bootstrap_contracts(&fixture.0)
                .unwrap()
                .join("\n")
                .contains("local_tasks")
        );
    }

    #[test]
    fn bootstrap_receiver_spellings_fail_closed() {
        let fixture = daemon_fixture();
        let src = fixture.0.join("crates/orch-daemon/src");
        let mut missed = Vec::new();
        // First case is the const-parameter identity alias. Keep each spelling
        // independent: a rejection of one alias must not mask another case.
        for (label, declarations, implementation) in [
            (
                "const generic",
                "type Outcome<const N: usize> = InitOutcome;",
                "impl Outcome<0> { fn local_tasks() {} }",
            ),
            (
                "type generic identity",
                "type Outcome<T> = T;",
                "impl Outcome<InitOutcome> { fn local_tasks() {} }",
            ),
            (
                "type generic",
                "trait Identity { type Out; } impl<T> Identity for T { type Out = InitOutcome; } type Outcome<T> = <T as Identity>::Out;",
                "impl Outcome<()> { fn local_tasks() {} }",
            ),
            (
                "lifetime generic",
                "type Outcome<'a> = InitOutcome;",
                "impl Outcome<'static> { fn local_tasks() {} }",
            ),
            (
                "where clause",
                "type Outcome<const N: usize> where [(); N]: Sized = InitOutcome;",
                "impl Outcome<0> { fn local_tasks() {} }",
            ),
            (
                "generic default",
                "type Outcome<T = InitOutcome> = T;",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "const expression",
                "type Outcome<const N: usize = { let n = 0; n }> = InitOutcome;",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "function arrow",
                "type Outcome<T = fn() -> InitOutcome> = T;",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "plain alias",
                "type Outcome = InitOutcome;",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "alias chain",
                "type Outcome = Middle; type Middle = InitOutcome;",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "renamed import",
                "use crate::InitOutcome as Outcome;",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "grouped raw rename",
                "use crate::{InitOutcome as r#Outcome};",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "cross module",
                "mod aliases { pub type Outcome = crate::InitOutcome; }",
                "impl crate::aliases::Outcome { fn local_tasks() {} }",
            ),
            (
                "blanket trait",
                "",
                "impl<T> Local for T { fn local_tasks() {} }",
            ),
            (
                "lifetime and type blanket",
                "",
                "impl<'a, T: 'a> Local for T { fn local_tasks() {} }",
            ),
            ("direct", "", "impl InitOutcome { fn local_tasks() {} }"),
            (
                "crate path",
                "",
                "impl crate::bootstrap::InitOutcome { fn local_tasks() {} }",
            ),
            (
                "self path",
                "",
                "impl self::InitOutcome { fn local_tasks() {} }",
            ),
            (
                "super path",
                "",
                "mod inner { impl super::InitOutcome { fn local_tasks() {} } }",
            ),
            (
                "absolute path",
                "",
                "impl ::owner::InitOutcome { fn local_tasks() {} }",
            ),
            (
                "function",
                "",
                "fn scoped() { type Outcome = InitOutcome; impl Outcome { fn local_tasks() {} } }",
            ),
            (
                "block",
                "",
                "fn scoped() { { type Outcome = InitOutcome; impl Outcome { fn local_tasks() {} } } }",
            ),
            (
                "const scope",
                "",
                "const _: () = { type Outcome = InitOutcome; impl Outcome { fn local_tasks() {} } };",
            ),
            (
                "nested module",
                "",
                "mod nested { type Outcome = InitOutcome; impl Outcome { fn local_tasks() {} } }",
            ),
            (
                "parenthesized",
                "",
                "impl (InitOutcome) { fn local_tasks() {} }",
            ),
            (
                "tuple trait",
                "",
                "impl Local for (InitOutcome,) { fn local_tasks() {} }",
            ),
            (
                "option trait",
                "",
                "impl Local for Option<InitOutcome> { fn local_tasks() {} }",
            ),
            (
                "pointer trait",
                "",
                "impl Local for *const InitOutcome { fn local_tasks() {} }",
            ),
            (
                "slice trait",
                "",
                "impl Local for [InitOutcome] { fn local_tasks() {} }",
            ),
            (
                "parenthesized alias",
                "type Outcome = (InitOutcome);",
                "impl Outcome { fn local_tasks() {} }",
            ),
            (
                "turbofish alias",
                "type Outcome<const N: usize> = InitOutcome;",
                "impl Outcome::<0> { fn local_tasks() {} }",
            ),
            (
                "trait reference",
                "",
                "impl Local for &InitOutcome { fn local_tasks() {} }",
            ),
            (
                "trait generic",
                "",
                "impl<const N: usize> Local<N> for InitOutcome { fn local_tasks() {} }",
            ),
            (
                "const generic header",
                "",
                "impl Local<{ 0 }> for InitOutcome { fn local_tasks() {} }",
            ),
            (
                "unsafe trait",
                "",
                "unsafe impl Local for InitOutcome { fn local_tasks() {} }",
            ),
            (
                "raw identifier",
                "type r#Outcome = r#InitOutcome;",
                "impl r#Outcome { fn local_tasks() {} }",
            ),
            (
                "unicode identifier",
                "type OutcomeΩ = InitOutcome;",
                "impl OutcomeΩ { fn local_tasks() {} }",
            ),
            (
                "trait projection",
                "trait Identity { type Out; } impl Identity for () { type Out = InitOutcome; }",
                "impl Local for <() as Identity>::Out { fn local_tasks() {} }",
            ),
            (
                "projection",
                "trait Identity { type Out; } impl Identity for () { type Out = InitOutcome; }",
                "impl <() as Identity>::Out { fn local_tasks() {} }",
            ),
            (
                "macro literal",
                "",
                "macro_rules! add { () => { impl InitOutcome { fn local_tasks() {} } } }",
            ),
            (
                "macro receiver",
                "",
                "macro_rules! add { ($t:ty) => { impl $t { fn local_tasks() {} } } }",
            ),
            (
                "macro alias RHS",
                "",
                "macro_rules! base { () => { InitOutcome } } type Outcome = base!(); impl Outcome { fn local_tasks() {} }",
            ),
            ("macro argument", "", "add!(InitOutcome);"),
            ("macro keyword argument", "", "add!(impl, InitOutcome);"),
            (
                "macro alias argument",
                "type Outcome = InitOutcome;",
                "add!(Outcome);",
            ),
            (
                "macro shadow",
                "",
                "macro_rules! matches { ($t:ty) => { add!($t); } } matches!(InitOutcome);",
            ),
            (
                "macro import shadow",
                "",
                "use producer::add as matches; matches!(InitOutcome);",
            ),
            (
                "unfamiliar impl position",
                "",
                "add!(impl InitOutcome { fn local_tasks() {} });",
            ),
        ] {
            std::fs::write(src.join("aliases.rs"), declarations).unwrap();
            std::fs::write(src.join("receiver.rs"), implementation).unwrap();
            let problems = bootstrap_impls(&fixture.0).unwrap().join("\n");
            if !problems.contains("BOOTSTRAP_IMPLS") || !problems.contains("receiver.rs") {
                missed.push(format!("{label}: {problems}"));
            }
        }
        assert!(
            missed.is_empty(),
            "unreviewed receivers accepted: {missed:#?}"
        );
    }

    #[test]
    fn the_reviewed_blanket_impl_cannot_grow_or_go_stale() {
        let fixture = daemon_fixture();
        let path = fixture.0.join("crates/orch-daemon/src/testing/mod.rs");
        let original = std::fs::read_to_string(&path).unwrap();
        let old = "impl<T: AsyncRead + AsyncWrite + Send + Unpin> ClientStream for T {}";
        assert!(original.contains(old));
        for replacement in [
            "impl<T: AsyncRead + AsyncWrite + Send + Unpin> ClientStream for T { fn local_tasks() {} }",
            "impl<T> ClientStream for T {}",
            "",
        ] {
            std::fs::write(&path, original.replacen(old, replacement, 1)).unwrap();
            let problems = bootstrap_impls(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("BOOTSTRAP_IMPLS")
                    || problems.contains("BOOTSTRAP_BLANKET_IMPLS"),
                "{problems}"
            );
        }
    }

    #[test]
    fn bootstrap_alias_resolution_and_unresolved_forms_are_explicit() {
        let fixture = daemon_fixture();
        let file = fixture.0.join("crates/orch-daemon/src/alias.rs");
        for declaration in [
            "type Outcome<const N: usize> = crate::InitOutcome;",
            "type Outcome<'a> = self::InitOutcome;",
            "type Outcome<const N: usize> where [(); N]: Sized = InitOutcome;",
            "type Middle<const N: usize> = InitOutcome; type Outcome = Middle<0>;",
        ] {
            let sources = vec![("alias.rs".to_owned(), declaration.to_owned())];
            assert_eq!(bootstrap_type_aliases(&sources)["Outcome"], "InitOutcome");
            std::fs::write(&file, declaration).unwrap();
            assert!(
                bootstrap_impls(&fixture.0).unwrap().is_empty(),
                "{declaration}"
            );
        }
        for declaration in [
            "type Outcome<T> = Option<InitOutcome>;",
            "type Outcome = (InitOutcome,);",
            "type Outcome<'a> = &'a InitOutcome;",
            "type Outcome = <InitOutcome as Identity>::Out;",
            "type Outcome<T = InitOutcome> = T;",
            "type Outcome where InitOutcome: Identity = Unknown;",
            "type Outcome: Identity<InitOutcome>;",
            "type Outcome<const N: usize> = InitOutcome where [(); N]: Sized;",
            "type Outcome<const N: usize = { 0 }> = [InitOutcome; N];",
        ] {
            std::fs::write(&file, declaration).unwrap();
            let problems = bootstrap_impls(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("alias.rs:1")
                    && problems.contains("Outcome")
                    && problems.contains("InitOutcome")
                    && problems.contains("BOOTSTRAP_IMPLS"),
                "{declaration}: {problems}"
            );
        }
    }

    #[test]
    fn bootstrap_pins_require_one_raw_declaration_per_type() {
        let fixture = daemon_fixture();
        let file = fixture.0.join("crates/orch-daemon/src/duplicate.rs");
        for name in BOOTSTRAP_TYPES {
            std::fs::write(&file, format!("#[cfg(test)] pub struct {name} {{}}")).unwrap();
            let problems = bootstrap_contracts(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains(name)
                    && problems.contains("exactly one raw")
                    && problems.contains("duplicate.rs:1")
                    && problems.contains("BOOTSTRAP_TYPES"),
                "{name}: {problems}"
            );
        }
        std::fs::remove_file(&file).unwrap();
    }

    // The exact reviewed copy cannot authorize a changed live definition.
    fn assert_duplicate_bootstrap_definition_rejected(old: &str, live: &str, reviewed: &str) {
        let fixture = daemon_fixture();
        let path = fixture.0.join("crates/orch-daemon/src/lifecycle.rs");
        let original = std::fs::read_to_string(&path).unwrap();
        assert!(original.contains(old));
        std::fs::write(
            &path,
            format!("{}\n{reviewed}", original.replacen(old, live, 1)),
        )
        .unwrap();
        let problems = bootstrap_contracts(&fixture.0).unwrap().join("\n");
        assert!(
            problems.contains("exactly one raw") && problems.contains("BOOTSTRAP_TYPES"),
            "{problems}"
        );
    }

    #[test]
    fn a_cfg_gated_enum_copy_cannot_authorize_a_changed_live_enum() {
        assert_duplicate_bootstrap_definition_rejected(
            "pub enum DispatchMode {",
            "pub enum DispatchMode { Hook(fn(&Path)),",
            "#[cfg(test)] pub enum DispatchMode { Off, On, }",
        );
    }

    #[test]
    fn a_cfg_gated_struct_copy_cannot_authorize_a_changed_live_struct() {
        assert_duplicate_bootstrap_definition_rejected(
            "pub struct StartOptions {",
            "pub struct StartOptions { pub hook: fn(),",
            "#[cfg(test)] pub struct StartOptions { pub project_root: PathBuf, pub socket: Option<PathBuf>, pub detach: bool, pub dispatch: DispatchMode, pub from_agent: bool, pub listen: Option<String>, pub peer_name: Option<String>, }",
        );
    }

    #[test]
    fn bootstrap_aliases_cannot_be_loaded_from_unenumerated_sources() {
        let fixture = daemon_fixture();
        let src = fixture.0.join("crates/orch-daemon/src");
        std::fs::write(
            fixture.0.join("outside.rs"),
            "type Outcome = crate::InitOutcome;",
        )
        .unwrap();
        std::fs::write(src.join("aliases.rs"), "type Outcome = crate::InitOutcome;").unwrap();
        for text in [
            r#"include!(concat!("../", "outside.rs"));"#,
            r#"include!("../../../outside.rs");"#,
            r#"#[path = "../../../outside.rs"] mod aliases;"#,
            r#"use std::include as load; load!("../../../outside.rs");"#,
        ] {
            std::fs::write(src.join("lib.rs"), text).unwrap();
            let problems = boundary_source_loading(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("orch-daemon/src/lib.rs:1")
                    && problems.contains("BOUNDARY_SOURCE_LOADING"),
                "{text}: {problems}"
            );
        }
        for text in [
            r#"include!("aliases.rs");"#,
            r#"#[path = "aliases.rs"] mod aliases;"#,
        ] {
            std::fs::write(src.join("lib.rs"), text).unwrap();
            assert!(
                boundary_source_loading(&fixture.0).unwrap().is_empty(),
                "{text}"
            );
        }
    }

    #[test]
    fn bootstrap_payload_tokens_cannot_be_rebound_in_pinned_files() {
        let fixture = daemon_fixture();
        for (file, declaration) in [
            ("lifecycle.rs", "use orch_engine::Container as PathBuf;"),
            ("lifecycle.rs", "type PathBuf = orch_engine::Container;"),
            ("bootstrap.rs", "use orch_engine::Container as String;"),
            ("error.rs", "type ErrorBody = orch_engine::Container;"),
            ("error.rs", "use owner as orch_core;"),
            ("lifecycle.rs", "struct String;"),
            ("lifecycle.rs", "use owner::String;"),
            ("lifecycle.rs", "use owner::*;"),
            ("lifecycle.rs", "use super::*;"),
        ] {
            let path = fixture.0.join("crates/orch-daemon/src").join(file);
            let original = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{original}\n{declaration}")).unwrap();
            let problems = bootstrap_impls(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("pinned payload name") && problems.contains("BOOTSTRAP_TYPES"),
                "{declaration}: {problems}"
            );
            std::fs::write(&path, original).unwrap();
        }
    }

    #[test]
    fn bootstrap_options_are_exact_and_prepared_start_fields_stay_private() {
        let fixture = daemon_fixture();
        for (file, old, new) in [
            (
                "lifecycle.rs",
                "pub struct PreparedStart {",
                "pub struct PreparedStart { pub engine: Arc<orch_engine::Container>,",
            ),
            (
                "lifecycle.rs",
                "pub struct StartOptions {",
                "pub struct StartOptions { pub on_ready: Option<Box<dyn FnOnce(&dyn Any) + Send>>,",
            ),
            (
                "bootstrap.rs",
                "pub struct InitOptions {",
                "pub struct InitOptions { pub store: Store,",
            ),
        ] {
            let path = fixture.0.join("crates/orch-daemon/src").join(file);
            let original = std::fs::read_to_string(&path).unwrap();
            assert!(original.contains(old));
            std::fs::write(&path, original.replacen(old, new, 1)).unwrap();
            let problems = bootstrap_contracts(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("bootstrap type boundary changed"),
                "{new}: {problems}"
            );
            std::fs::write(&path, original).unwrap();
        }
    }

    #[test]
    fn bootstrap_enum_variants_and_payloads_are_exact() {
        let fixture = daemon_fixture();
        assert!(bootstrap_enums(&fixture.0).unwrap().is_empty());
        for (file, old, new, name) in [
            (
                "error.rs",
                "pub enum DaemonError {",
                "pub enum DaemonError { Leak(Arc<orch_engine::Container>),",
                "DaemonError",
            ),
            (
                "lifecycle.rs",
                "pub enum DispatchMode {",
                "pub enum DispatchMode { Hook(fn(&Path)),",
                "DispatchMode",
            ),
            (
                "error.rs",
                "InvalidArguments(String)",
                "InvalidArguments(orch_engine::Container)",
                "DaemonError",
            ),
            (
                "lifecycle.rs",
                "    On,",
                "    On(fn(&Path)),",
                "DispatchMode",
            ),
        ] {
            let path = fixture.0.join("crates/orch-daemon/src").join(file);
            let original = std::fs::read_to_string(&path).unwrap();
            assert!(original.contains(old), "fixture must apply: {old}");
            std::fs::write(&path, original.replacen(old, new, 1)).unwrap();
            let problems = bootstrap_contracts(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("BOOTSTRAP_TYPES") && problems.contains(name),
                "{new}: {problems}"
            );
            std::fs::write(&path, original).unwrap();
        }
    }

    fn feature_fixture(kind: &str, defaults: bool, daemon_features: Value) -> Value {
        serde_json::json!({"workspace_members":["cli", "daemon", "core"], "packages":[
            {"id":"cli", "name":"orch-cli", "features":{}, "dependencies":[
                {"name":"orch-daemon", "kind":kind, "uses_default_features":defaults, "features":[]}]},
            {"id":"daemon", "name":"orch-daemon", "features":daemon_features, "dependencies":[
                {"name":"orch-core", "kind":null, "uses_default_features":true, "features":[]}]},
            {"id":"core", "name":"orch-core", "features":{"test-support":[]}, "dependencies":[]}
        ]})
    }

    #[test]
    fn selected_default_and_local_alias_features_cannot_enable_test_apis() {
        for kind in ["normal", "build"] {
            for features in [
                serde_json::json!({"default":["testing"], "testing":[]}),
                serde_json::json!({"default":["fixture-alias"], "fixture-alias":["testing"], "testing":[]}),
            ] {
                let metadata = feature_fixture(kind, true, features);
                let problems = promoted_test_features(&metadata).join("\n");
                assert!(
                    problems.contains("orch-cli") && problems.contains("orch-daemon/testing"),
                    "{kind}: {problems}"
                );
            }
        }
        // Inactive optional declarations still grant a production capability;
        // the edge check must not rely on an owner's active default closure.
        for kind in ["normal", "build"] {
            let mut metadata = feature_fixture(
                kind,
                true,
                serde_json::json!({"default":["testing"], "testing":[]}),
            );
            metadata["packages"][0]["dependencies"][0]["optional"] = serde_json::json!(true);
            assert!(
                promoted_test_features(&metadata)
                    .join("\n")
                    .contains("orch-daemon/testing")
            );
        }
        assert!(
            promoted_test_features(&feature_fixture(
                "dev",
                true,
                serde_json::json!({"default":["testing"], "testing":[]})
            ))
            .is_empty()
        );
        let mut metadata = feature_fixture(
            "normal",
            false,
            serde_json::json!({"default":["testing"], "fixture-alias":["testing"], "testing":[]}),
        );
        assert!(promoted_test_features(&metadata).is_empty());
        metadata["packages"][0]["dependencies"][0]["features"] =
            serde_json::json!(["fixture-alias"]);
        assert!(
            promoted_test_features(&metadata)
                .join("\n")
                .contains("orch-daemon/testing")
        );
        metadata["packages"][0]["dependencies"][0]["features"] = serde_json::json!([]);
        metadata["packages"][2]["features"]["default"] = serde_json::json!(["test-support"]);
        assert!(
            promoted_test_features(&metadata)
                .join("\n")
                .contains("orch-core/test-support")
        );
    }

    #[test]
    fn owner_default_forwarding_cannot_select_a_dependency_test_alias() {
        let mut metadata = feature_fixture(
            "normal",
            false,
            serde_json::json!({"bridge":["testing"], "testing":[]}),
        );
        metadata["packages"][0]["features"] =
            serde_json::json!({"default":["forward"], "forward":["orch-daemon/bridge"]});
        let problems = promoted_test_features(&metadata).join("\n");
        assert!(
            problems.contains("orch-cli") && problems.contains("orch-daemon/testing"),
            "{problems}"
        );
        metadata["packages"][0]["dependencies"][0]["kind"] = serde_json::json!("dev");
        assert!(promoted_test_features(&metadata).is_empty());
    }

    #[test]
    fn selected_feature_closure_follows_aliases_and_weak_dependency_forwarding() {
        for forwarding in ["ports/bridge", "ports?/bridge"] {
            let mut metadata = feature_fixture(
                "normal",
                true,
                serde_json::json!({
                    "default":["cycle"], "cycle":["next"], "next":["cycle", forwarding]
                }),
            );
            metadata["packages"][1]["dependencies"][0]["rename"] = serde_json::json!("ports");
            metadata["packages"][2]["features"]["bridge"] = serde_json::json!(["test-support"]);
            let problems = promoted_test_features(&metadata).join("\n");
            assert!(
                problems.contains("orch-core/test-support"),
                "{forwarding}: {problems}"
            );
            let edges = workspace_edges(&metadata).unwrap();
            let selected = selected_workspace_features(&metadata, &edges, &edges["orch-cli"][0]);
            assert!(selected["orch-core"].contains("test-support"));
        }
        let mut metadata = feature_fixture(
            "normal",
            true,
            serde_json::json!({"default":["ports?/bridge"]}),
        );
        metadata["packages"][1]["dependencies"][0]["rename"] = serde_json::json!("ports");
        metadata["packages"][1]["dependencies"][0]["optional"] = serde_json::json!(true);
        metadata["packages"][2]["features"]["bridge"] = serde_json::json!(["test-support"]);
        let edges = workspace_edges(&metadata).unwrap();
        let selected = selected_workspace_features(&metadata, &edges, &edges["orch-cli"][0]);
        assert!(
            !selected.contains_key("orch-core"),
            "weak forwarding must not activate an optional dependency"
        );
        metadata["packages"][1]["features"]["default"] =
            serde_json::json!(["ports?/bridge", "dep:ports"]);
        let edges = workspace_edges(&metadata).unwrap();
        let selected = selected_workspace_features(&metadata, &edges, &edges["orch-cli"][0]);
        assert!(selected["orch-core"].contains("test-support"));
    }

    #[test]
    fn boundary_code_loading_is_literal_and_inside_the_scanned_source_root() {
        let fixture = SourceFixture::new();
        fixture.write("shared.rs", "fn shared() {}");
        fixture.write("shared.txt", "fn unscanned() {}");
        std::fs::write(fixture.0.join("crates/outside.rs"), "fn outside() {}").unwrap();
        for text in [
            "include!(concat!(env!(\"OUT_DIR\"), \"/client_generated.rs\"));",
            "include!(concat!(\"../mcp/test\", \"ing.rs\"));",
            "include!(\"../../outside.rs\");",
            "#[path = concat!(env!(\"OUT_DIR\"), \"/client.rs\")] mod generated;",
            "#[path = \"../../outside.rs\"] mod generated;",
            "include!(\"missing.rs\");",
            "include!(\"shared.txt\");",
        ] {
            fixture.write("generated.rs", text);
            let problems = boundary_source_loading(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("BOUNDARY_SOURCE_LOADING") && problems.contains("generated.rs"),
                "{text}: {problems}"
            );
        }
        for text in [
            "include!(\"shared.rs\");",
            "#[path = \"shared.rs\"] mod shared;",
            "include!(r#\"shared.rs\"#);",
            "include_str!(concat!(env!(\"OUT_DIR\"), \"/asset\")); include_bytes!(\"asset\");",
            "#[cfg(test)] mod tests { include!(concat!(env!(\"OUT_DIR\"), \"/test.rs\")); }",
        ] {
            fixture.write("generated.rs", text);
            assert!(
                boundary_source_loading(&fixture.0).unwrap().is_empty(),
                "{text}"
            );
        }
        for name in ["orch-client", "orch-proto", "orch-tui"] {
            let src = fixture.0.join("crates").join(name).join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(
                src.join("lib.rs"),
                "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));",
            )
            .unwrap();
            assert!(
                boundary_source_loading(&fixture.0)
                    .unwrap()
                    .join("\n")
                    .contains(name)
            );
            std::fs::remove_dir_all(src).unwrap();
        }
    }

    #[test]
    fn boundary_include_macro_imports_are_rejected() {
        let fixture = SourceFixture::new();
        for text in [
            "use std::include as load_source; load_source!(concat!(env!(\"OUT_DIR\"), \"/client.rs\"));",
            "pub use core::{include as load_source, include_str};",
            "use ::std::include;",
        ] {
            fixture.write("generated.rs", &format!("\n{text}"));
            let problems = boundary_source_loading(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("BOUNDARY_SOURCE_LOADING")
                    && problems.contains("generated.rs:2:")
                    && problems.contains("include macro"),
                "{text}: {problems}"
            );
        }
        for text in [
            "use std::{include_str as text, include_bytes as bytes};",
            "#[cfg(test)] use std::include as load_test_source;",
            "let label = \"use std::include as load_source;\";",
        ] {
            fixture.write("generated.rs", text);
            assert!(
                boundary_source_loading(&fixture.0).unwrap().is_empty(),
                "{text}"
            );
        }
    }

    #[test]
    fn boundary_nested_path_attributes_stay_inside_the_scanned_source_root() {
        let fixture = SourceFixture::new();
        fixture.write("shared.rs", "fn shared() {}");
        for text in [
            "#[cfg_attr(all(), path = \"../../outside.rs\")] mod generated;",
            "#[cfg_attr(all(), cfg_attr(unix, path = \"../../outside.rs\"))] mod generated;",
            "#[cfg_attr(all(), path = concat!(\"shared\", \".rs\"))] mod generated;",
            "#[other(path = \"../../outside.rs\")] mod generated;",
        ] {
            fixture.write("generated.rs", &format!("\n{text}"));
            let problems = boundary_source_loading(&fixture.0).unwrap().join("\n");
            assert!(
                problems.contains("BOUNDARY_SOURCE_LOADING")
                    && problems.contains("generated.rs:2:"),
                "{text}: {problems}"
            );
        }
        for text in [
            "#[cfg_attr(all(), path = \"shared.rs\")] mod shared;",
            "#[cfg_attr(all(), path = \"shared.rs\", allow(dead_code))] mod shared;",
            "#[cfg_attr(all(), cfg_attr(unix, path = \"shared.rs\"), allow(dead_code))] mod shared;",
            "#[other(path = r#\"shared.rs\"#)] mod shared;",
            "#[doc = \"path = unscanned\"] mod shared;",
        ] {
            fixture.write("generated.rs", text);
            assert!(
                boundary_source_loading(&fixture.0).unwrap().is_empty(),
                "{text}"
            );
        }
    }

    #[test]
    fn boundary_custom_build_targets_require_an_exact_reviewed_entry() {
        for name in ["orch-cli", "orch-client", "orch-proto", "orch-tui"] {
            let metadata = serde_json::json!({"packages":[{"name":name, "targets":[{"kind":["custom-build"], "src_path":"/fixture/build.rs"}]}]});
            let problems = boundary_build_targets(&metadata).join("\n");
            assert!(
                problems.contains(name) && problems.contains("BOUNDARY_BUILD_TARGETS"),
                "{problems}"
            );
        }
        assert!(
            boundary_build_targets(
                &serde_json::json!({"packages":[{"name":"orch-cli", "targets":[{"kind":["lib"]}]}]})
            )
            .is_empty()
        );
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
