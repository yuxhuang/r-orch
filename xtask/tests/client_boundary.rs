//! Exercise the real crate-graph binary against disposable repository copies.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CLI: &str = "crates/orch-cli/Cargo.toml";
const CLIENT: &str = "crates/orch-client/Cargo.toml";
const INIT: &str = "crates/orch-cli/src/commands/init.rs";
const DAEMON: &str = "crates/orch-cli/src/commands/daemon.rs";
const BOOTSTRAP: &str = "crates/orch-daemon/src/bootstrap.rs";
const LIFECYCLE: &str = "crates/orch-daemon/src/lifecycle.rs";
const DISCOVERY: &str = "crates/orch-daemon/src/discovery.rs";
const ERROR: &str = "crates/orch-daemon/src/error.rs";
const EXTRA: &str = "crates/orch-daemon/src/boundary_fixture.rs";
const ROW: &str = "docs/ARCHITECTURE.md";

struct Repository {
    temporary: PathBuf,
    root: PathBuf,
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            // Refuse symlinks rather than letting a fixture write outside its copy.
            assert!(
                kind.is_file(),
                "unexpected fixture input: {:?}",
                entry.path()
            );
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl Repository {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The package has no temporary-directory dependency. Atomic creation and
        // a Drop owner suffice for this small Unix-only test fixture.
        let temporary = loop {
            let path = Path::new("/tmp").join(format!(
                "client-boundary-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private fixture: {error}"),
            }
        };
        let repository = Self {
            root: temporary.join("repo"),
            temporary,
        };
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        fs::create_dir_all(&repository.root).unwrap();
        for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
            fs::copy(source.join(name), repository.root.join(name)).unwrap();
        }
        for name in ["crates", "xtask"] {
            copy_tree(&source.join(name), &repository.root.join(name));
        }
        fs::create_dir_all(repository.root.join("docs")).unwrap();
        fs::copy(source.join(ROW), repository.root.join(ROW)).unwrap();
        for name in ["home", "tmp", "bin"] {
            fs::create_dir(repository.temporary.join(name)).unwrap();
        }
        for name in ["claude", "codex", "opencode", "gemini", "cursor-agent"] {
            let path = repository.temporary.join("bin").join(name);
            fs::write(
                &path,
                "#!/bin/sh\nprintf '%s\\n' \"$0\" >> \"$HOME/blocked-cli.log\"\nexit 127\n",
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        repository.assert_passes("unmodified repository control");
        repository
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.root.join(path)).unwrap()
    }

    fn append(&self, path: &str, source: &str) -> (String, Vec<u8>) {
        let mut bytes = fs::read(self.root.join(path)).unwrap_or_default();
        bytes.extend_from_slice(b"\n");
        bytes.extend_from_slice(source.as_bytes());
        (path.into(), bytes)
    }

    fn replace(&self, path: &str, before: &str, after: &str) -> (String, Vec<u8>) {
        let original = self.read(path);
        assert!(original.contains(before), "plant anchor absent in {path}");
        (
            path.into(),
            original.replacen(before, after, 1).into_bytes(),
        )
    }

    fn dependency(&self, manifest: &str, kind: &str, declaration: &str) -> (String, Vec<u8>) {
        if kind == "normal" {
            self.replace(
                manifest,
                "[dependencies]\n",
                &format!("[dependencies]\n{declaration}\n"),
            )
        } else {
            self.append(manifest, &format!("[build-dependencies]\n{declaration}\n"))
        }
    }

    fn gate(&self) -> Output {
        // Execute the built binary, not cargo run: metadata is the only child
        // command and never builds the deliberately changed Rust sources.
        let cargo = Path::new(env!("CARGO"));
        let path = std::env::join_paths([
            self.temporary.join("bin"),
            cargo.parent().unwrap().to_path_buf(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("crate-graph")
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.temporary.join("home"))
            .env("TMPDIR", self.temporary.join("tmp"))
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.temporary.join("home/global.yml"),
            )
            .env("CARGO", cargo)
            .env("CARGO_HOME", self.temporary.join("home/.cargo"))
            .env("CARGO_TARGET_DIR", self.temporary.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            !self.temporary.join("home/blocked-cli.log").exists(),
            "gate reached a blocked AI CLI"
        );
        output
    }

    fn assert_passes(&self, scenario: &str) {
        let output = self.gate();
        let diagnostic = diagnostic(&output);
        assert!(
            output.status.success() && diagnostic.contains("layering matches ARCHITECTURE §3.1"),
            "{scenario}: {diagnostic}"
        );
    }

    fn rejects(&self, scenario: &str, edits: Vec<(String, Vec<u8>)>, site: &str, rule: &str) {
        let restore = Restore::apply(&self.root, edits);
        let output = self.gate();
        let diagnostic = diagnostic(&output);
        drop(restore);
        self.assert_passes("exactly restored repository control");
        assert!(
            !output.status.success() && diagnostic.contains(site) && diagnostic.contains(rule),
            "{scenario}: expected gate diagnostic {site:?} / {rule:?}; got {diagnostic}"
        );
    }

    fn permits(&self, scenario: &str, edits: Vec<(String, Vec<u8>)>) {
        let restore = Restore::apply(&self.root, edits);
        let output = self.gate();
        let diagnostic = diagnostic(&output);
        drop(restore);
        self.assert_passes("exactly restored repository control");
        assert!(
            output.status.success() && diagnostic.contains("layering matches ARCHITECTURE §3.1"),
            "{scenario}: {diagnostic}"
        );
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temporary);
    }
}

fn diagnostic(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

struct Restore(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Restore {
    fn apply(root: &Path, edits: Vec<(String, Vec<u8>)>) -> Self {
        let mut restore = Self(Vec::new());
        for (name, bytes) in edits {
            let path = root.join(name);
            let original = match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("read original {}: {error}", path.display()),
            };
            restore.0.push((path.clone(), original));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        restore
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        for (path, original) in self.0.iter().rev() {
            if let Some(bytes) = original {
                fs::write(path, bytes).unwrap();
                assert_eq!(fs::read(path).unwrap(), *bytes, "restore exact bytes");
            } else {
                fs::remove_file(path).unwrap();
            }
        }
    }
}

#[test]
fn normal_build_target_renamed_and_optional_owner_dependencies_are_rejected() {
    let repo = Repository::new();
    for owner in ["orch-cli", "orch-client", "orch-tui"] {
        let manifest = format!("crates/{owner}/Cargo.toml");
        for dependency in ["orch-infra", "orch-engine", "orch-peer"] {
            for kind in ["normal", "build"] {
                repo.rejects(
                    &format!("{owner} {kind} dependency on {dependency}"),
                    vec![repo.dependency(
                        &manifest,
                        kind,
                        &format!("{dependency}.workspace = true"),
                    )],
                    &format!("`{owner}` -> `{dependency}`"),
                    &format!("violates declared-edge policy: kind={kind}"),
                );
            }
        }
        repo.rejects(
            "renamed owner dependency",
            vec![repo.dependency(
                &manifest,
                "normal",
                "boundary-owner = { package = \"orch-infra\", path = \"../orch-infra\" }",
            )],
            &format!("`{owner}` -> `orch-infra`"),
            "rename=Some(\"boundary-owner\")",
        );
        repo.rejects(
            "inactive target owner dependency",
            vec![repo.append(
                &manifest,
                "[target.'cfg(target_os = \"none\")'.dependencies]\norch-infra.workspace = true",
            )],
            &format!("`{owner}` -> `orch-infra`"),
            "target=Some(\"cfg(target_os = \\\"none\\\")\")",
        );
        repo.rejects(
            "inactive optional owner dependency",
            vec![repo.dependency(
                &manifest,
                "normal",
                "orch-infra = { workspace = true, optional = true }",
            )],
            &format!("`{owner}` -> `orch-infra`"),
            "optional=true",
        );
    }
}

#[test]
fn storage_imports_paths_aliases_and_reexports_are_rejected_in_every_frontend() {
    let repo = Repository::new();
    for file in [
        "crates/orch-cli/src/commands/task.rs",
        "crates/orch-cli/src/mcp/tools/task.rs",
        "crates/orch-cli/src/tui/backend.rs",
        "crates/orch-cli/src/web/mod.rs",
    ] {
        for source in [
            "use orch_infra::storage::FileTaskStore;",
            "fn boundary_probe() { let _ = orch_infra::storage::FileTaskStore; }",
            "use orch_infra::storage::FileTaskStore as BoundaryStore;",
            "pub use orch_infra::storage::FileTaskStore;",
        ] {
            repo.rejects(
                "frontend storage access",
                vec![repo.append(file, source)],
                file,
                "production owner import `orch_infra` is forbidden",
            );
        }
    }
}

#[test]
fn daemon_references_outside_wrappers_are_rejected() {
    let repo = Repository::new();
    for file in [
        "crates/orch-cli/src/commands/task.rs",
        "crates/orch-cli/src/mcp/tools/task.rs",
        "crates/orch-cli/src/tui/backend.rs",
        "crates/orch-cli/src/web/mod.rs",
    ] {
        repo.rejects(
            "daemon import outside wrappers",
            vec![repo.append(file, "use orch_daemon::InitOptions;")],
            file,
            "daemon reference is outside the exact CLI_DAEMON_SITES bootstrap API",
        );
    }
}

#[test]
fn public_and_renamed_direct_daemon_imports_are_rejected_in_both_wrappers() {
    let repo = Repository::new();
    for (file, symbol) in [(DAEMON, "StartOptions"), (INIT, "InitOptions")] {
        for source in [
            format!("pub use orch_daemon::{symbol};"),
            format!("pub(crate) use orch_daemon::{symbol};"),
            format!("pub(super) use orch_daemon::{symbol};"),
            format!("pub(self) use orch_daemon::{symbol};"),
            format!("pub(in crate::commands) use orch_daemon::{symbol};"),
            format!("#[allow(unused_imports)] pub use orch_daemon::{symbol};"),
            format!("use orch_daemon::{symbol} as BoundaryOptions;"),
            format!("fn boundary_probe() {{ use orch_daemon::{symbol} as BoundaryOptions; }}"),
            format!("use orch_daemon::{{{symbol} as BoundaryOptions}};"),
            format!("use {{orch_daemon::{symbol} as BoundaryOptions}};"),
            format!("pub use orch_daemon::{{{symbol}}};"),
            format!("use ::orch_daemon::{symbol} as BoundaryOptions;"),
            format!("pub use ::orch_daemon::{symbol};"),
            format!("use r#orch_daemon::r#{symbol} as BoundaryOptions;"),
            format!("pub use r#orch_daemon::r#{symbol};"),
            "pub use orch_daemon::*;".into(),
        ] {
            repo.rejects(
                "direct daemon import changes visibility or binding",
                vec![repo.append(file, &source)],
                file,
                "bootstrap use visibility or alias is unreviewed",
            );
        }
        for source in [
            "use orch_daemon as boundary_owner;",
            "use orch_daemon::{self as boundary_owner};",
            "extern crate orch_daemon as boundary_owner;",
            "use orch_daemon::*;",
        ] {
            repo.rejects(
                "crate-level daemon binding is outside the listed symbols",
                vec![repo.append(file, source)],
                file,
                "daemon reference is outside the exact CLI_DAEMON_SITES bootstrap API",
            );
        }
    }
}

#[test]
fn private_direct_imports_and_two_step_private_renames_are_outside_the_visibility_sentence() {
    let repo = Repository::new();
    // A use that does not itself name the daemon crate is outside that sentence's scope.
    for (file, symbol) in [(DAEMON, "StartOptions"), (INIT, "InitOptions")] {
        repo.permits(
            "plain private daemon import",
            vec![repo.append(file, &format!("use orch_daemon::{symbol};"))],
        );
        repo.permits(
            "two-step private alias rename",
            vec![repo.append(file, &format!("mod boundary_scope {{ use orch_daemon::{symbol}; use {symbol} as BoundaryOptions; }}"))],
        );
    }
}

#[test]
fn associated_daemon_apis_outside_the_reviewed_symbols_are_rejected() {
    let repo = Repository::new();
    repo.rejects(
        "associated method is not part of the bootstrap wrapper API",
        vec![repo.append(
            DAEMON,
            "fn boundary_probe() { orch_daemon::StartOptions::boundary_state(); }",
        )],
        DAEMON,
        "unreviewed associated bootstrap API",
    );
}

#[test]
fn dedicated_test_roots_need_independent_guards_and_no_production_route() {
    let repo = Repository::new();
    let file = "crates/orch-cli/src/mcp.rs";
    repo.rejects(
        "fixture declaration loses its own test guard",
        vec![repo.replace(
            file,
            "#[cfg(test)]\npub(crate) mod testing;",
            "pub(crate) mod testing;",
        )],
        file,
        "must retain its independent guarded declaration",
    );
    repo.rejects(
        "a second module declaration loads a fixture in production",
        vec![repo.append(
            "crates/orch-cli/src/boundary_fixture.rs",
            "mod spawn_subscriber_tests;",
        )],
        "crates/orch-cli/src/boundary_fixture.rs",
        "cfg-test root `spawn_subscriber_tests.rs` loaded in production",
    );
}

#[test]
fn bootstrap_store_returns_engine_fields_and_execution_closures_are_rejected() {
    let repo = Repository::new();
    for (file, before, after) in [
        (
            BOOTSTRAP,
            "std::result::Result<InitOutcome, DaemonError>",
            "std::result::Result<orch_infra::storage::FileTaskStore, DaemonError>",
        ),
        (
            BOOTSTRAP,
            "pub adapter: String,",
            "pub adapter: orch_engine::Container,",
        ),
        (
            BOOTSTRAP,
            "initialize(options: InitOptions)",
            "initialize<F: FnOnce()>(options: InitOptions, execute: F)",
        ),
        (
            LIFECYCLE,
            "pub async fn run(mut self) -> Result<DaemonExit, DaemonError>",
            "pub async fn run(mut self) -> Result<orch_engine::Container, DaemonError>",
        ),
    ] {
        repo.rejects(
            "bootstrap surface exposes state or execution",
            vec![repo.replace(file, before, after)],
            file,
            "bootstrap type boundary changed",
        );
    }
    repo.rejects(
        "new generic execution facade",
        vec![repo.append(
            BOOTSTRAP,
            "pub fn boundary_execute<F: FnOnce()>(execute: F) { execute() }",
        )],
        BOOTSTRAP,
        "unreviewed public bootstrap function or method",
    );
}

#[test]
fn bootstrap_enum_payloads_duplicate_declarations_and_rebindings_are_rejected() {
    let repo = Repository::new();
    for (file, before, after) in [
        (
            ERROR,
            "pub enum DaemonError {",
            "pub enum DaemonError { BoundaryState(usize),",
        ),
        (
            LIFECYCLE,
            "pub enum DispatchMode {",
            "pub enum DispatchMode { BoundaryState(usize),",
        ),
    ] {
        repo.rejects(
            "new enum payload",
            vec![repo.replace(file, before, after)],
            file,
            "bootstrap enum boundary changed",
        );
    }
    for (file, source, rule) in [
        (
            LIFECYCLE,
            "#[cfg(test)] enum DispatchMode {}",
            "requires exactly one raw struct/enum declaration",
        ),
        (
            LIFECYCLE,
            "#[cfg(test)] struct StartOptions;",
            "requires exactly one raw struct/enum declaration",
        ),
        (
            EXTRA,
            "type InitOutcome = ();",
            "bootstrap type name InitOutcome cannot be rebound",
        ),
        (
            EXTRA,
            "mod InitOutcome {}",
            "bootstrap type name InitOutcome cannot be rebound",
        ),
        (
            EXTRA,
            "use crate::InitOptions as InitOutcome;",
            "bootstrap type name InitOutcome cannot be rebound",
        ),
        (
            LIFECYCLE,
            "type Result = ();",
            "pinned payload name Result cannot be rebound",
        ),
        (
            BOOTSTRAP,
            "type Path = ();",
            "pinned payload name Path cannot be rebound",
        ),
        (
            DISCOVERY,
            "type PathBuf = ();",
            "pinned payload name PathBuf cannot be rebound",
        ),
        (
            DISCOVERY,
            "type Result = ();",
            "pinned payload name Result cannot be rebound",
        ),
        (
            ERROR,
            "use crate::InitOptions as ErrorBody;",
            "pinned payload name ErrorBody cannot be rebound",
        ),
        (
            LIFECYCLE,
            "use crate::*;",
            "unreviewed import binding of a pinned payload name",
        ),
    ] {
        repo.rejects(
            "declaration or payload binding changed",
            vec![repo.append(file, source)],
            "crates/orch-daemon/src",
            rule,
        );
    }
    repo.rejects(
        "bootstrap declaration moved out of production",
        vec![repo.replace(
            BOOTSTRAP,
            "pub struct InitOutcome {",
            "#[cfg(test)] pub struct InitOutcome {",
        )],
        BOOTSTRAP,
        "must be declared once at the top level",
    );
}

#[test]
fn bootstrap_impls_aliases_macro_arguments_and_unclosed_groups_are_rejected() {
    let repo = Repository::new();
    for source in [
        "impl InitOutcome { pub fn boundary_state(&self) {} }",
        "type BoundaryAlias<const N: usize> = InitOutcome; impl BoundaryAlias<1> { pub fn boundary_state(&self) {} }",
        "use crate::InitOutcome as BoundaryAlias; impl BoundaryAlias { pub fn boundary_state(&self) {} }",
        "type BoundaryAlias = InitOutcome; type BoundaryNext = BoundaryAlias; impl BoundaryNext { pub fn boundary_state(&self) {} }",
        "impl<T> BoundaryTrait for T { fn boundary_state(&self) {} }",
        "boundary_tokens!(impl InitOutcome {});",
        "macro_rules! boundary_tokens { () => { impl InitOutcome {} } }",
        "impl InitOutcome { pub fn boundary_state(&self) -> impl Iterator<Item = u8> {} }",
    ] {
        repo.rejects(
            "unreviewed bootstrap implementation",
            vec![repo.append(EXTRA, source)],
            EXTRA,
            "BOOTSTRAP_IMPLS",
        );
    }
    for (source, rule) in [
        ("boundary_tokens!(", "unclosed macro invocation group"),
        (
            "macro_rules! boundary_tokens {",
            "unclosed macro definition group",
        ),
    ] {
        repo.rejects(
            "unclosed macro group",
            vec![repo.append(EXTRA, source)],
            EXTRA,
            rule,
        );
    }
}

#[test]
fn cfg_test_items_do_not_hide_following_fields_arms_statements_or_macro_arguments() {
    let repo = Repository::new();
    for source in [
        "#[cfg(test)] mod boundary_tests {} fn boundary_probe() { orch_daemon::prepare_start(o); }",
        "struct Boundary { #[cfg(test)] test: u8, live: u8 } fn boundary_probe() { orch_daemon::prepare_start(o); }",
        "fn boundary_probe() { Boundary { #[cfg(test)] test: 0, live: { orch_daemon::prepare_start(o); 1 } }; }",
        "fn boundary_probe() { match n { #[cfg(test)] 0 => 0, _ => { orch_daemon::prepare_start(o); 1 } } }",
        "fn boundary_probe() { match n { #[cfg(test)] 0 => { 0 } _ => { orch_daemon::prepare_start(o); 1 } } }",
        "fn boundary_probe() { #[cfg(test)] {} orch_daemon::prepare_start(o); }",
        "fn boundary_probe() { #[cfg(test)] boundary_empty! {} orch_daemon::prepare_start(o); }",
        "fn boundary_probe() { match n { #[cfg(test)] 0 => if b { 0 } else { 1 } _ => { orch_daemon::prepare_start(o); 1 } } }",
        "fn boundary_probe() { match n { #[cfg(test)] 0 => 0, ..=5 => { orch_daemon::prepare_start(o); 1 } _ => 2 } }",
        "fn boundary_probe() { #[cfg(test)] {} ..orch_daemon::prepare_start(o); }",
        "fn boundary_probe() { #[cfg(test)] let _ = value; orch_daemon::prepare_start(o); }",
        "fn boundary_probe() { #[cfg(test)] {} r#else! { orch_daemon::prepare_start(o); } }",
        "boundary_tokens!(#[cfg(test)] orch_daemon::prepare_start(o));",
        "boundary_tokens!{#[cfg(test)] {} ? orch_daemon::prepare_start(o)}",
    ] {
        repo.rejects(
            "production reference after a test-only element",
            vec![repo.append("crates/orch-cli/src/mcp/tools/task.rs", source)],
            "crates/orch-cli/src/mcp/tools/task.rs",
            "daemon reference is outside the exact CLI_DAEMON_SITES bootstrap API",
        );
    }
}

fn helper(repo: &Repository, owner: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let mut edits = vec![
        repo.replace("Cargo.toml", "members = [", "members = [\n    \"crates/boundary-helper\","),
        ("crates/boundary-helper/Cargo.toml".into(), b"[package]\nname = \"boundary-helper\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\norch-infra = { path = \"../orch-infra\" }\n".to_vec()),
        ("crates/boundary-helper/src/lib.rs".into(), Vec::new()),
    ];
    if let Some(owner) = owner {
        edits.push(repo.dependency(
            &format!("crates/{owner}/Cargo.toml"),
            "normal",
            "boundary-helper = { path = \"../boundary-helper\" }",
        ));
    }
    edits
}

#[test]
fn pure_clients_cannot_reach_state_through_a_new_workspace_helper() {
    let repo = Repository::new();
    for owner in ["orch-client", "orch-proto", "orch-tui"] {
        repo.rejects(
            "indirect helper reaches state",
            helper(&repo, Some(owner)),
            &format!("pure client `{owner}`"),
            "reaches state capability `orch-infra` through normal/build workspace edges",
        );
    }
    // The combined CLI's intentional owner-bootstrap route is the normal control.
    repo.assert_passes("CLI owner-bootstrap transitive route remains allowed");
}

#[test]
fn stale_edges_stale_source_sites_and_unclassified_members_are_rejected() {
    let repo = Repository::new();
    repo.rejects(
        "removed allowed crate edge",
        vec![repo.replace(
            "crates/orch-tui/Cargo.toml",
            "orch-domain.workspace = true",
            "",
        )],
        "`orch-tui` -> `orch-domain`",
        "stale normal allowance",
    );
    repo.rejects(
        "removed allowed CLI source site",
        vec![(INIT.into(), Vec::new())],
        "commands/init.rs",
        "stale CLI_DAEMON_SITES entry",
    );
    let engine_site = "crates/orch-engine/src/prompt_mode.rs";
    repo.rejects(
        "removed allowed engine source site",
        vec![(
            engine_site.into(),
            repo.read(engine_site)
                .replace("orch_infra", "boundary_unused")
                .into_bytes(),
        )],
        "prompt_mode.rs",
        "which no longer names `orch_infra`",
    );
    repo.rejects(
        "new member has no policy",
        helper(&repo, None),
        "boundary-helper",
        "is an unclassified workspace crate",
    );
}

#[test]
fn external_dev_fixtures_are_permitted_but_production_test_apis_are_rejected() {
    let repo = Repository::new();
    repo.permits(
        "external daemon test harness on the declared dev edge",
        vec![(
            "crates/orch-cli/tests/boundary_fixture.rs".into(),
            b"use orch_daemon::testing::DaemonHarness;\n".to_vec(),
        )],
    );
    repo.permits(
        "inline test import has only item-local immunity",
        vec![repo.append(
            "crates/orch-cli/src/commands/task.rs",
            "#[cfg(test)] mod boundary_tests { use orch_daemon::testing::DaemonHarness; }",
        )],
    );
    repo.rejects(
        "test harness moved into production source",
        vec![repo.append(
            "crates/orch-cli/src/commands/task.rs",
            "use orch_daemon::testing::DaemonHarness;",
        )],
        "crates/orch-cli/src/commands/task.rs",
        "daemon reference is outside the exact CLI_DAEMON_SITES bootstrap API",
    );
    repo.rejects(
        "dev feature promoted to a normal edge",
        vec![repo.replace(
            CLI,
            "orch-daemon.workspace = true",
            "orch-daemon = { workspace = true, features = [\"testing\"] }",
        )],
        "`orch-cli` -> `orch-daemon`",
        "promotes a test API into normal dependencies",
    );
    for features in [
        "default = [\"boundary-testing\"]\nboundary-testing = [\"orch-daemon/testing\"]",
        "default = [\"boundary-first\"]\nboundary-first = [\"boundary-second\"]\nboundary-second = [\"orch-daemon/testing\"]",
    ] {
        repo.rejects(
            "default feature closure forwards a test API",
            vec![repo.replace(CLI, "[features]", &format!("[features]\n{features}"))],
            "orch-cli",
            "TEST_API_FEATURES",
        );
    }
    repo.rejects(
        "weak optional forwarding still selects a test API",
        vec![(CLI.into(), repo.read(CLI).replacen("[features]", "[features]\ndefault = [\"dep:orch-core\", \"boundary-testing\"]\nboundary-testing = [\"orch-core?/test-support\"]", 1).replacen("orch-core.workspace = true", "orch-core = { workspace = true, optional = true }", 1).into_bytes())],
        "orch-cli",
        "TEST_API_FEATURES",
    );
}

#[test]
fn normal_workspace_feature_closures_cannot_enable_daemon_testing() {
    let repo = Repository::new();
    let manifest = "crates/orch-daemon/Cargo.toml";
    for features in [
        "default = [\"testing\"]",
        "default = [\"boundary-first\"]\nboundary-first = [\"boundary-second\"]\nboundary-second = [\"testing\"]",
    ] {
        repo.rejects(
            "normal dependency selects a test API through default feature closure",
            vec![repo.replace(manifest, "[features]", &format!("[features]\n{features}"))],
            "`orch-cli` -> `orch-daemon`",
            "selects test API `orch-daemon/testing` through its normal feature closure",
        );
    }
}

#[test]
fn client_process_paths_aliases_and_enabled_features_are_rejected_but_tasks_are_permitted() {
    let repo = Repository::new();
    let file = "crates/orch-client/src/boundary_fixture.rs";
    for (source, token) in [
        (
            "use std::process::Command as BoundaryChild;",
            "std::process",
        ),
        (
            "use tokio::process::Command as BoundaryChild;",
            "tokio::process",
        ),
        ("use libc::fork as boundary_split;", "fork"),
        ("use nix::unistd::execvp as boundary_replace;", "execvp"),
        ("use libc::posix_spawn as boundary_launch;", "posix_spawn"),
    ] {
        repo.rejects(
            "client process alias",
            vec![repo.append(file, source)],
            file,
            &format!("names `{token}`"),
        );
    }
    repo.rejects(
        "Tokio process feature",
        vec![repo.replace(
            CLIENT,
            "tokio = { workspace = true, features = [",
            "tokio = { workspace = true, features = [\"process\",",
        )],
        "orch-client enables `tokio/process`",
        "a process capability",
    );
    repo.rejects(
        "aliased nix process feature",
        vec![repo.dependency(
            CLIENT,
            "normal",
            "boundary-nix = { package = \"nix\", version = \"0.31\", features = [\"process\"] }",
        )],
        "orch-client enables `nix/process`",
        "a process capability",
    );
    repo.permits(
        "Tokio tasks have no process capability",
        vec![repo.append(
            file,
            "fn boundary_tasks() { tokio::spawn(async {}); tokio::task::spawn_blocking(|| 1); }",
        )],
    );
}

#[test]
fn reviewed_wrappers_state_free_helpers_and_frontend_effects_are_permitted() {
    let repo = Repository::new();
    // The unmodified copy exercises the explicit init/bootstrap/start wrappers.
    repo.permits(
        "state-free helpers and frontend editor effects",
        vec![repo.append("crates/orch-cli/src/boundary_fixture.rs", "use orch_domain::TaskStatus; use orch_core::clock::Clock; fn boundary_editor() { let _ = std::process::Command::new(\"editor-placeholder\"); }")],
    );
}

#[test]
fn frontend_helper_reexports_cannot_name_an_owner_crate() {
    let repo = Repository::new();
    repo.rejects(
        "frontend helper re-exports storage",
        vec![repo.append(
            "crates/orch-cli/src/boundary_fixture.rs",
            "pub use orch_infra::storage::FileTaskStore as BoundaryClipboard;",
        )],
        "crates/orch-cli/src/boundary_fixture.rs",
        "production owner import `orch_infra` is forbidden",
    );
}

#[test]
fn pure_source_owner_references_and_unreviewed_build_scripts_are_rejected() {
    let repo = Repository::new();
    for owner in ["orch-client", "orch-proto", "orch-tui"] {
        let file = format!("crates/{owner}/src/boundary_fixture.rs");
        repo.rejects(
            "pure source cannot name an owner",
            vec![repo.append(&file, "use orch_infra::storage::FileTaskStore;")],
            &file,
            "pure client source names state capability",
        );
        repo.rejects(
            "boundary build script needs review",
            vec![(
                format!("crates/{owner}/build.rs"),
                b"fn main() {}\n".to_vec(),
            )],
            &format!("`{owner}` custom-build target"),
            "BOUNDARY_BUILD_TARGETS",
        );
    }
}

#[test]
fn computed_outside_and_renamed_source_loading_are_rejected() {
    let repo = Repository::new();
    let file = "crates/orch-cli/src/web/mod.rs";
    for source in [
        "include!(concat!(\"boundary\", \".rs\"));",
        "#[path = \"../../boundary-outside.rs\"] mod boundary_loaded;",
        "#[cfg_attr(all(), cfg_attr(all(), path = \"../../boundary-outside.rs\"))] mod boundary_loaded;",
        "use std::include as boundary_include;",
    ] {
        repo.rejects(
            "unreviewed code loading",
            vec![repo.append(file, source)],
            file,
            "BOUNDARY_SOURCE_LOADING",
        );
    }
    repo.rejects(
        "test-only fixture path loaded in production",
        vec![repo.append(file, "include!(\".././mcp/testing.rs\");")],
        file,
        "cfg-test path `mcp/testing.rs` loaded in production",
    );
}

#[test]
fn non_ascii_identifiers_in_plain_generic_and_import_alias_forms_are_rejected() {
    let repo = Repository::new();
    for source in [
        "type Café = InitOutcome; impl Cafe\u{301} { fn boundary_method() {} }",
        "type Café<const N: usize> = InitOutcome; impl Cafe\u{301}<0> { fn boundary_method() {} }",
        "use crate::InitOutcome as Café; impl Cafe\u{301} { fn boundary_method() {} }",
        "fn boundary_lifetime<'é>() {}",
        "boundary_tokens!(\"plain\", é);",
    ] {
        repo.rejects(
            "non-ASCII emitted token",
            vec![repo.append(EXTRA, source)],
            EXTRA,
            "SOURCE_SPELLING",
        );
    }
}

fn architecture_row(text: &str) -> &str {
    text.lines()
        .find(|line| line.starts_with("| `orch-cli → orch-daemon`"))
        .expect("architecture must retain the client boundary row")
}

fn assert_documented_limit(text: &str, words: &str) {
    assert!(
        architecture_row(text).contains(words),
        "documented source-scan limit missing: {words}"
    );
}

fn limit_at_head(words: &str) {
    let repo = Repository::new();
    let text = repo.read(ROW);
    assert_documented_limit(&text, words);
}

#[test]
fn raw_declaration_keyword_macro_cfg_looking_attribute_limit_is_documented() {
    // A cfg-looking attribute in the arguments of a macro invocation whose raw name is the declaration keyword can hide an explicitly written CLI daemon reference.
    limit_at_head(
        "A cfg-looking attribute in the arguments of a macro invocation whose raw name is the declaration keyword can hide an explicitly written CLI daemon reference.",
    );
}

#[test]
fn test_only_raw_let_keyword_macro_following_production_statement_limit_is_documented() {
    // A test-only raw let-keyword macro invocation can make the text scan omit the following production statement.
    limit_at_head(
        "A test-only raw let-keyword macro invocation can make the text scan omit the following production statement.",
    );
}

#[test]
fn daemon_reexport_same_named_outside_type_limit_is_documented() {
    // A daemon re-export can bind a bootstrap name to a same-named type declared outside the daemon source root, leaving additional methods outside the checked bootstrap surface.
    limit_at_head(
        "A daemon re-export can bind a bootstrap name to a same-named type declared outside the daemon source root, leaving additional methods outside the checked bootstrap surface.",
    );
}

#[test]
fn adjacent_suffixed_literal_tokenizer_limit_with_all_consumers_is_documented() {
    // Adjacent suffixed literals in macro input can make the tokenizer treat subsequent source as literal content.
    limit_at_head(
        "Adjacent suffixed literals in macro input can make the tokenizer treat subsequent source as literal content, concealing non-ASCII identifiers, explicit CLI daemon references, bootstrap impl headers and aliased receivers, pinned payload-name bindings, and source-loading attributes.",
    );
}

#[test]
fn procedural_attribute_derive_and_token_synthesizing_expansion_limit_is_documented() {
    // Not caught: procedural, attribute, derive and token-synthesizing expansion.
    limit_at_head("procedural, attribute, derive and token-synthesizing expansion");
}

#[test]
fn macro_generated_include_limit_is_documented() {
    // Not caught: macro-generated include/path attributes (macro fragments are not expanded).
    limit_at_head("macro-generated include/path attributes (macro fragments are not expanded)");
}

#[test]
fn macro_generated_path_attribute_limit_is_documented() {
    // Not caught: macro-generated include/path attributes (macro fragments are not expanded).
    limit_at_head("macro-generated include/path attributes (macro fragments are not expanded)");
}
