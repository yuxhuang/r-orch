//! A capability opt-in may appear only at reviewed definitions/forwarders or
//! in the explicitly non-forking test fixtures. No production file wildcard.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SELECTORS: &[&str] = &[
    "for_synthetic_processes",
    "with_spawn_domain",
    "isolated_fake_spawns",
    "build_registry_with_clock_and_spawn_domain",
    "build_full_with_children_and_spawn_domain",
    "bring_up_with_spawn_domain",
    "run_with_spawn_domain",
    "run_for_code_with_spawn_domain",
];
const TEST_FILES: &[&str] = &[
    "crates/orch-daemon/tests/daemon_dispatch_wake.rs",
    "crates/orch-daemon/tests/daemon_spawn_gate_isolation.rs",
    "crates/orch-daemon/tests/support/spawn_gate_isolation.rs",
    "crates/orch-daemon/tests/daemon_isolated_spawns_contract.rs",
    "crates/orch-infra/src/process/gate/synthetic_tests.rs",
];

// Exact approved production lines, including definitions, imports and default
// forwarding. A duplicate on another line, even in these files, is rejected.
const PRODUCTION: &[(&str, &[&str])] = &[
    (
        "crates/orch-daemon/src/lifecycle.rs",
        &[
            "Self::bring_up_with_spawn_domain(spec, maintenance_timer, None).await",
            "pub(crate) async fn bring_up_with_spawn_domain(",
            "orch_engine::container::build_full_with_children_and_spawn_domain(",
        ],
    ),
    (
        "crates/orch-daemon/src/testing/mod.rs",
        &[
            "isolated_fake_spawns: bool,",
            "pub fn isolated_fake_spawns(mut self) -> Self {",
            "self.isolated_fake_spawns = true;",
            "if self.isolated_fake_spawns && self.processes.is_none() {",
            ".isolated_fake_spawns",
            ".then(orch_infra::process::gate::SpawnDomain::for_synthetic_processes);",
            "let mut running = Running::bring_up_with_spawn_domain(",
        ],
    ),
    (
        "crates/orch-engine/src/container.rs",
        &[
            "use orch_infra::adapters::registry::build_registry_with_clock_and_spawn_domain;",
            "build_full_with_children_and_spawn_domain(",
            "pub async fn build_full_with_children_and_spawn_domain(",
            "let adapters = Arc::new(build_registry_with_clock_and_spawn_domain(",
            ".with_spawn_domain(spawn_domain.clone()),",
        ],
    ),
    (
        "crates/orch-infra/src/adapters/opencode.rs",
        &["pub fn with_spawn_domain(mut self, domain: SpawnDomain) -> Self {"],
    ),
    (
        "crates/orch-infra/src/adapters/registry.rs",
        &[
            "build_registry_with_clock_and_spawn_domain(process_manager, clock, Default::default())",
            "pub fn build_registry_with_clock_and_spawn_domain(",
            ".with_spawn_domain(domain),",
        ],
    ),
    (
        "crates/orch-infra/src/process/gate.rs",
        &["pub fn for_synthetic_processes() -> Self {"],
    ),
    (
        "crates/orch-infra/src/workspace/manager.rs",
        &[
            "use super::{MergeStrategy, run_for_code, run_for_code_with_spawn_domain, run_with_spawn_domain};",
            "pub fn with_spawn_domain(mut self, domain: SpawnDomain) -> Self {",
            "run_for_code_with_spawn_domain(",
            "let Ok(base) = run_with_spawn_domain(",
            "let Ok(diff) = run_with_spawn_domain(",
        ],
    ),
    (
        "crates/orch-infra/src/workspace/mod.rs",
        &[
            "run_with_spawn_domain(pm, cwd, command, args, &Default::default()).await",
            "pub(crate) async fn run_with_spawn_domain(",
            "run_for_code_with_spawn_domain(pm, cwd, command, args, &Default::default()).await",
            "pub(crate) async fn run_for_code_with_spawn_domain(",
            "match run_with_spawn_domain(pm, cwd, command, &args, domain).await {",
        ],
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
fn references(source: &str) -> BTreeMap<String, usize> {
    let mut lines = BTreeMap::new();
    // Conservative text scan also rejects references in comments and literals.
    // It sees aliases/imports/function values, not only direct method calls.
    for line in source
        .lines()
        .filter(|line| SELECTORS.iter().any(|name| line.contains(name)))
    {
        *lines.entry(line.trim().to_owned()).or_default() += 1;
    }
    lines
}

#[test]
fn synthetic_capability_references_stay_within_exact_reviewed_sites() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut paths = Vec::new();
    sources(&root.join("crates"), &mut paths);
    sources(&root.join("xtask/src"), &mut paths);
    paths.sort();
    let mut seen = Vec::new();
    let strings = regex::Regex::new(r#""(?:\\.|[^"\\])*""#).unwrap();
    for path in paths {
        let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
        // This guard may name selectors only as pattern strings above.
        if relative == "crates/orch-infra/tests/synthetic_spawn_domain_guard.rs" {
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                references(&strings.replace_all(&text, "")).is_empty(),
                "guard contains a selector outside pattern strings"
            );
            continue;
        }
        if TEST_FILES.contains(&relative) {
            continue;
        }
        let actual = references(&std::fs::read_to_string(&path).unwrap());
        if let Some((_, lines)) = PRODUCTION.iter().find(|(file, _)| *file == relative) {
            let expected = references(&lines.join("\n"));
            assert_eq!(actual, expected, "unreviewed selector site in {relative}");
            seen.push(relative.to_owned());
        } else {
            assert!(
                actual.is_empty(),
                "synthetic capability escaped allow-list in {relative}: {actual:?}"
            );
        }
    }
    assert_eq!(
        seen.len(),
        PRODUCTION.len(),
        "reviewed production file disappeared"
    );
}
