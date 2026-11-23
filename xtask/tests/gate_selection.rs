//! Independent black-box R5/R6 oracles, authored from TV20–38 and D31 at F0.
//! No selector source is imported. An all-stub is not a passing classification.
//! Miniatures prove only their stated behavior, never a tier/timing/equivalence gate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn private_capture(path: &Path) -> File {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
}

const BASE: &str = "selector-fixture-base";
const LEAF: &[&str] = &["orch-leaf"];
const MID: &[&str] = &["orch-build-user", "orch-dev-user", "orch-mid"];
const INFRA: &[&str] = &["infra-dependent-sentinel", "orch-cli", "orch-infra"];
const DOMAIN: &[&str] = &[
    "infra-dependent-sentinel",
    "orch-build-user",
    "orch-cli",
    "orch-core",
    "orch-dev-user",
    "orch-domain",
    "orch-infra",
    "orch-leaf",
    "orch-mid",
];
// The sentinel depends on infra normally, but must not be selected merely because
// a CLI change adds infra via D31. Dependencies are deliberately not repo-shaped.
type FixtureDependency = (&'static str, Option<&'static str>);
type FixturePackage = (&'static str, &'static [FixtureDependency]);
const GRAPH: &[FixturePackage] = &[
    ("orch-domain", &[]),
    ("orch-core", &[("orch-domain", None)]),
    ("orch-infra", &[("orch-core", None)]),
    ("orch-cli", &[("orch-infra", None)]),
    ("infra-dependent-sentinel", &[("orch-infra", None)]),
    ("orch-leaf", &[("orch-domain", None)]),
    ("orch-mid", &[("orch-domain", None)]),
    ("orch-build-user", &[("orch-mid", Some("build"))]),
    ("orch-dev-user", &[("orch-build-user", Some("dev"))]),
    ("xtask", &[]),
];

struct Fixture {
    temporary: PathBuf,
    root: PathBuf,
    next: AtomicU64,
}

#[test]
fn selector_captures_are_private_at_creation() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let out = f.run(f.command("/usr/bin/true"), Duration::from_secs(5));
    assert!(out.status.success());
    let logs = f.temporary.join("logs");
    assert_eq!(fs::metadata(&logs).unwrap().mode() & 0o777, 0o700);
    let files: Vec<_> = fs::read_dir(logs).unwrap().collect();
    assert_eq!(files.len(), 2);
    for file in files {
        let m = file.unwrap().metadata().unwrap();
        assert!(m.is_file());
        assert_eq!(m.nlink(), 1);
        assert_eq!(m.mode() & 0o777, 0o600);
    }
}

// Subprocess deadlines are failure bounds, not synchronization or timing claims.
// These owned, dependency-free fixtures do not daemonize or detach. Every child
// gets a separate group; timeout/unwind kills that group before reaping its
// still-owned leader. A reaped process number is never signalled.
// Library-first §4.8: the binding std-only/no-manifest test interface rules
// out adding the tokio process-wrap runtime; std creates the group and the
// existing system kill tool supplies only its bounded termination operation.
struct Running {
    child: Child,
    reaped: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.reaped {
            // A reaped PID/group number may be reused. Never signal it. The
            // fixed fixtures wait for their synchronous child commands.
            return;
        }
        // Use only the group created by CommandExt::process_group(0), never a
        // process-name/PID census or a group inherited from the test runner.
        let mut kill = Command::new("/bin/kill")
            .args(["-KILL", &format!("-{}", self.child.id())])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start owned process-group cleanup");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if kill.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "process-group cleanup did not exit"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        }
        if !self.reaped {
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if self.child.try_wait().unwrap().is_some() {
                    break;
                }
                assert!(Instant::now() < deadline, "owned child could not be reaped");
                std::thread::park_timeout(Duration::from_millis(10));
            }
        }
    }
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // xtask has no tempfile dependency. This mirrors client_boundary's
        // atomic private-directory owner; no general temp/path parser is added.
        let temporary = loop {
            let path = Path::new("/tmp").join(format!(
                "gate-selection-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("private fixture directory: {error}"),
            }
        };
        let fixture = Self {
            root: temporary.join("repo"),
            temporary,
            next: AtomicU64::new(0),
        };
        for path in [
            "repo",
            "home",
            "tmp",
            "cargo-home",
            "rustup-home",
            "lease",
            "logs",
            "git-template",
        ] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(fixture.temporary.join(path))
                .unwrap();
        }
        fs::write(fixture.temporary.join("gitconfig"), "").unwrap();
        fixture
    }

    fn command(&self, executable: impl AsRef<Path>) -> Command {
        // CARGO is supplied by Cargo to the integration-test compilation, not
        // inherited from an agent session. Require its pinned sibling rustc.
        let cargo = Path::new(env!("CARGO"));
        assert!(
            cargo.is_absolute(),
            "Cargo must supply an absolute tool path"
        );
        let tools = cargo.parent().unwrap();
        assert!(
            tools.join("rustc").is_file(),
            "pinned rustc must accompany cargo"
        );
        let mut command = Command::new(executable.as_ref());
        command
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.temporary.join("home"))
            .env("TMPDIR", self.temporary.join("tmp"))
            .env("TMP", self.temporary.join("tmp"))
            .env("TEMP", self.temporary.join("tmp"))
            .env("CARGO", cargo)
            .env("RUSTC", tools.join("rustc"))
            .env("RUSTDOC", tools.join("rustdoc"))
            .env("CARGO_HOME", self.temporary.join("cargo-home"))
            .env("RUSTUP_HOME", self.temporary.join("rustup-home"))
            .env("CARGO_TARGET_DIR", self.temporary.join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_BUILD_JOBS", "1")
            .env("CARGO_TERM_COLOR", "never")
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.temporary.join("home/global.yml"),
            )
            .env("XTASK_GATE_LEASE_DIR", self.temporary.join("lease"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", self.temporary.join("gitconfig"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Selector Fixture")
            .env("GIT_AUTHOR_EMAIL", "selector@example.invalid")
            .env("GIT_COMMITTER_NAME", "Selector Fixture")
            .env("GIT_COMMITTER_EMAIL", "selector@example.invalid")
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
            .env("LC_ALL", "C")
            .env(
                "PATH",
                std::env::join_paths([tools, Path::new("/usr/bin"), Path::new("/bin")]).unwrap(),
            );
        command
    }

    fn run(&self, mut command: Command, limit: Duration) -> Output {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let stdout = self.temporary.join(format!("logs/{n}.stdout"));
        let stderr = self.temporary.join(format!("logs/{n}.stderr"));
        command
            .stdin(Stdio::null())
            .stdout(private_capture(&stdout))
            .stderr(private_capture(&stderr))
            .process_group(0);
        let mut running = Running {
            child: command.spawn().expect("spawn fixture command"),
            reaped: false,
        };
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                running.reaped = true;
                break status;
            }
            assert!(
                fs::metadata(&stdout).unwrap().len() + fs::metadata(&stderr).unwrap().len()
                    < 4 * 1024 * 1024,
                "fixture output exceeded 4 MiB"
            );
            assert!(
                Instant::now() < deadline,
                "fixture subprocess deadline: {command:?}"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        };
        drop(running); // finished leaders are reaped; failures kill before reap
        assert!(
            fs::metadata(&stdout).unwrap().len() + fs::metadata(&stderr).unwrap().len()
                < 4 * 1024 * 1024,
            "completed fixture output exceeded 4 MiB"
        );
        Output {
            status,
            stdout: fs::read(stdout).unwrap(),
            stderr: fs::read(stderr).unwrap(),
        }
    }

    fn write(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn metadata(&self) -> String {
        let packages: Vec<_> = GRAPH.iter().map(|(name, dependencies)| {
            let directory = self.root.join(if *name == "xtask" { "xtask".into() } else { format!("crates/{name}") });
            let dependencies: Vec<_> = dependencies.iter().map(|(dependency, kind)| json!({
                "name": dependency, "source": null, "req": "*", "kind": kind,
                "rename": null, "optional": false, "uses_default_features": true,
                "features": [], "target": null, "registry": null,
                "path": self.root.join(format!("crates/{dependency}")),
            })).collect();
            json!({
                "name": name, "version": "0.0.0",
                "id": format!("path+file://{}#{name}@0.0.0", directory.display()),
                "manifest_path": directory.join("Cargo.toml"), "source": null,
                "dependencies": dependencies, "features": {},
                "targets": [{"name": name.replace('-', "_"), "kind": ["lib"], "crate_types": ["lib"],
                    "src_path": directory.join("src/lib.rs"), "edition": "2024", "test": true, "doctest": true, "doc": true}],
                "edition": "2024", "publish": [], "license": "Apache-2.0",
            })
        }).collect();
        json!({
            "workspace_members": packages.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(),
            "workspace_default_members": packages.iter().map(|p| p["id"].clone()).collect::<Vec<_>>(),
            "packages": packages, "resolve": null, "workspace_root": self.root,
            "target_directory": self.temporary.join("target"), "version": 1, "metadata": null,
        }).to_string()
    }

    fn selection(&self, changes: &[u8], metadata: &[u8], format: &str) -> Output {
        let changed = self.temporary.join("changes.nul");
        let graph = self.temporary.join("metadata.json");
        fs::write(&changed, changes).unwrap();
        fs::write(&graph, metadata).unwrap();
        let mut command = self.command(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["gate", "select", "--base", BASE, "--format", format])
            .arg("--changed-from")
            .arg(changed)
            .arg("--metadata-from")
            .arg(graph);
        self.run(command, Duration::from_secs(15))
    }

    fn select(&self, changes: &[u8]) -> Value {
        parse(self.selection(changes, self.metadata().as_bytes(), "json"))
    }

    fn git(&self, args: &[&str]) -> String {
        let mut command = self.command("/usr/bin/git");
        command.args(args);
        let output = self.run(command, Duration::from_secs(15));
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            diagnostic(&output)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    fn repository(&self) {
        self.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/leaf\", \"crates/other\"]\nresolver = \"3\"\n",
        );
        for name in ["leaf", "other"] {
            self.write(
                format!("crates/{name}/Cargo.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"),
            );
            self.write(format!("crates/{name}/src/lib.rs"), "pub fn marker() {}\n");
            // Keep metadata valid when the inferred library is deleted/renamed.
            self.write(format!("crates/{name}/src/main.rs"), "fn main() {}\n");
        }
        self.write("README.md", "fixture\n");
        self.write("docs/guide.md", "guide\n");
        // Predeclare the dependency-free lockfile so metadata cannot introduce
        // an unrelated untracked Cargo.lock during selection.
        self.write("Cargo.lock", "version = 4\n\n[[package]]\nname = \"leaf\"\nversion = \"0.0.0\"\n\n[[package]]\nname = \"other\"\nversion = \"0.0.0\"\n");
        self.git(&[
            "init",
            "--quiet",
            &format!(
                "--template={}",
                self.temporary.join("git-template").display()
            ),
        ]);
        self.git(&["config", "commit.gpgsign", "false"]);
        self.git(&["add", "--", "."]);
        self.git(&["commit", "--quiet", "-m", "fixture baseline"]);
        self.git(&["branch", BASE]);
    }

    fn select_git(&self, base: &str) -> Value {
        let mut command = self.command(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["gate", "select", "--format", "json"])
            .arg(format!("--base={base}"));
        parse(self.run(command, Duration::from_secs(20)))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.temporary)
            .expect("remove private fixture after all children exit");
    }
}

fn diagnostic(output: &Output) -> String {
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn parse(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "classification (including errors) exits zero: {}",
        diagnostic(&output)
    );
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("selection must be schema-1 JSON");
    assert_eq!(value["schema"], 1);
    for reason in value["reasons"].as_array().expect("reason array") {
        for key in ["path", "class", "selects", "why"] {
            assert!(
                reason[key].is_string(),
                "missing reason field {key}: {reason}"
            );
        }
        assert!(!reason["selects"].as_str().unwrap().is_empty());
        assert!(!reason["why"].as_str().unwrap().is_empty());
    }
    value
}

fn selected(value: &Value, mode: &str, packages: &[&str]) {
    assert_eq!(value["mode"], mode, "{value}");
    assert_eq!(
        value["packages"],
        json!(packages),
        "exact sorted package set: {value}"
    );
}

fn reasons(value: &Value, expected: &[(&str, &str)]) {
    let actual: BTreeSet<_> = value["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| {
            (
                reason["path"].as_str().unwrap().to_owned(),
                reason["class"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected: BTreeSet<_> = expected
        .iter()
        .map(|(path, class)| (path.to_string(), class.to_string()))
        .collect();
    assert_eq!(
        actual, expected,
        "every changed path must have its class: {value}"
    );
}

fn modified(paths: &[&str]) -> Vec<u8> {
    // This is an emitter for the frozen Git name-status -z fixture protocol,
    // not an independently implemented selector/parser (library-first §4.8).
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(b"M\0");
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
    }
    bytes
}

fn classified(value: &Value) {
    // Error and empty-input projections can match F0's all stub. Require an
    // implemented reason there; narrowing/path tests assert their own contract
    // first so failures identify the missing package or classification.
    for reason in value["reasons"].as_array().unwrap() {
        assert!(
            !reason["why"]
                .as_str()
                .unwrap()
                .contains("not available yet"),
            "F0 stub is dispatch evidence, not a classifier: {reason}"
        );
    }
}

fn fail_closed(value: &Value, class: &str) {
    selected(value, "all", &[]);
    assert!(
        value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["class"] == class),
        "fail-closed needs an attributable {class} reason: {value}"
    );
    classified(value);
}

#[test]
fn leaf_source_selects_only_its_owner_tv20() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-leaf/src/lib.rs"]));
    selected(&value, "packages", LEAF);
    reasons(&value, &[("crates/orch-leaf/src/lib.rs", "crate")]);
}

#[test]
fn root_documents_select_no_tests_tv21() {
    let fixture = Fixture::new();
    let paths = [
        "docs/RUNBOOK.md",
        "docs/specs/unrelated.md",
        "README.md",
        "CHANGELOG.md",
        "LICENSE",
        "NOTICE",
    ];
    let value = fixture.select(&modified(&paths));
    selected(&value, "docs-only", &[]);
    reasons(
        &value,
        &paths.iter().map(|p| (*p, "doc")).collect::<Vec<_>>(),
    );
}

const SQLITE_SPEC: &str = "docs/specs/2026-11-20-sqlite-storage.md";

#[test]
fn sqlite_spec_selects_importer_and_its_cli_runtime() {
    let fixture = Fixture::new();
    let changes = modified(&[SQLITE_SPEC]);
    let value = fixture.select(&changes);
    selected(&value, "packages", &["orch-cli", "orch-infra"]);
    reasons(&value, &[(SQLITE_SPEC, "coupled")]);
    let reason = &value["reasons"][0];
    assert_eq!(reason["selects"], "orch-cli,orch-infra");
    assert!(
        reason["why"]
            .as_str()
            .unwrap()
            .contains("import_index_reference")
    );
    // The unrelated infra-dependent sentinel must remain excluded.
    let output = fixture.selection(&changes, fixture.metadata().as_bytes(), "github");
    assert!(output.status.success(), "{}", diagnostic(&output));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "mode={}\npackages={}\n",
            value["mode"].as_str().unwrap(),
            value["packages"]
        )
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains(SQLITE_SPEC)
    );
}

#[test]
fn sqlite_spec_unions_with_prose_and_source_closure() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&[SQLITE_SPEC, "docs/RUNBOOK.md"]));
    selected(&value, "packages", &["orch-cli", "orch-infra"]);
    reasons(
        &value,
        &[(SQLITE_SPEC, "coupled"), ("docs/RUNBOOK.md", "doc")],
    );
    let value = fixture.select(&modified(&[SQLITE_SPEC, "crates/orch-mid/src/lib.rs"]));
    selected(
        &value,
        "packages",
        &[
            "orch-build-user",
            "orch-cli",
            "orch-dev-user",
            "orch-infra",
            "orch-mid",
        ],
    );
    reasons(
        &value,
        &[
            (SQLITE_SPEC, "coupled"),
            ("crates/orch-mid/src/lib.rs", "crate"),
        ],
    );
    for global in ["xtask/src/main.rs", "unknown-input"] {
        let value = fixture.select(&modified(&[SQLITE_SPEC, global]));
        selected(&value, "all", &[]);
    }
}

#[test]
fn sqlite_spec_deletion_rename_and_copy_retain_the_consumer() {
    let fixture = Fixture::new();
    for change in [
        format!("D\0{SQLITE_SPEC}\0"),
        format!("R100\0{SQLITE_SPEC}\0docs/old-spec.md\0"),
        format!("R100\0docs/new-spec.md\0{SQLITE_SPEC}\0"),
        format!("C100\0{SQLITE_SPEC}\0docs/copied-spec.md\0"),
        format!("C100\0docs/new-spec.md\0{SQLITE_SPEC}\0"),
    ] {
        let value = fixture.select(change.as_bytes());
        selected(&value, "packages", &["orch-cli", "orch-infra"]);
        assert!(
            value["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == SQLITE_SPEC && r["class"] == "coupled")
        );
    }
}

#[test]
fn sqlite_spec_missing_consumer_fails_closed() {
    let fixture = Fixture::new();
    let mut metadata: Value = serde_json::from_str(&fixture.metadata()).unwrap();
    let packages = metadata["packages"].as_array_mut().unwrap();
    packages.retain(|p| p["name"] != "orch-infra");
    for package in packages.iter_mut() {
        package["dependencies"]
            .as_array_mut()
            .unwrap()
            .retain(|d| d["name"] != "orch-infra");
    }
    let members: Vec<_> = packages.iter().map(|p| p["id"].clone()).collect();
    metadata["workspace_members"] = json!(members);
    metadata["workspace_default_members"] = json!(members);
    let value = parse(fixture.selection(
        &modified(&[SQLITE_SPEC]),
        metadata.to_string().as_bytes(),
        "json",
    ));
    fail_closed(&value, "error");
    reasons(&value, &[(SQLITE_SPEC, "error")]);
    assert!(
        value["reasons"][0]["why"]
            .as_str()
            .unwrap()
            .contains("missing orch-infra member")
    );
}
#[test]
fn sqlite_spec_missing_cli_runtime_fails_closed() {
    let fixture = Fixture::new();
    let mut metadata: Value = serde_json::from_str(&fixture.metadata()).unwrap();
    let packages = metadata["packages"].as_array_mut().unwrap();
    packages.retain(|p| p["name"] != "orch-cli");
    for package in packages.iter_mut() {
        package["dependencies"]
            .as_array_mut()
            .unwrap()
            .retain(|d| d["name"] != "orch-cli");
    }
    let members: Vec<_> = packages.iter().map(|p| p["id"].clone()).collect();
    metadata["workspace_members"] = json!(members);
    metadata["workspace_default_members"] = json!(members);
    let value = parse(fixture.selection(
        &modified(&[SQLITE_SPEC]),
        metadata.to_string().as_bytes(),
        "json",
    ));
    fail_closed(&value, "error");
    reasons(&value, &[(SQLITE_SPEC, "error")]);
    assert!(
        value["reasons"][0]["why"]
            .as_str()
            .unwrap()
            .contains("missing orch-cli member")
    );
}

#[test]
fn git_current_base_spec_only_commit_selects_importer_and_prose_does_not() {
    let fixture = Fixture::new();
    fixture.repository();
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n",
    );
    for name in ["orch-infra", "orch-cli", "infra-dependent-sentinel"] {
        let dependency = if name == "infra-dependent-sentinel" {
            "[dependencies]\norch-infra = { path = \"../orch-infra\" }\n"
        } else {
            ""
        };
        fixture.write(format!("crates/{name}/Cargo.toml"), format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{dependency}"));
        fixture.write(format!("crates/{name}/src/lib.rs"), "// fixture\n");
    }
    fixture.write(SQLITE_SPEC, "# SQLite spec fixture\n");
    // Resolve the dependency-free workspace before freezing the clean base.
    let mut metadata = fixture.command(env!("CARGO"));
    metadata.args(["metadata", "--offline", "--format-version", "1"]);
    let output = fixture.run(metadata, Duration::from_secs(20));
    assert!(output.status.success(), "{}", diagnostic(&output));
    fixture.git(&["add", "--", "."]);
    fixture.git(&["commit", "--quiet", "-m", "consumer baseline"]);
    let base = fixture.git(&["rev-parse", "HEAD"]);
    fixture.write(
        SQLITE_SPEC,
        "# SQLite spec fixture\n\nHarmless prose edit.\n",
    );
    fixture.git(&["add", "--", SQLITE_SPEC]);
    fixture.git(&["commit", "--quiet", "-m", "spec prose only"]);
    assert_eq!(fixture.git(&["rev-parse", "HEAD^"]), base);
    assert_eq!(
        fixture.git(&["diff", "--name-status", &base, "HEAD"]),
        format!("M\t{SQLITE_SPEC}")
    );
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    let value = fixture.select_git(&base);
    selected(&value, "packages", &["orch-cli", "orch-infra"]);
    reasons(&value, &[(SQLITE_SPEC, "coupled")]);
    let base = fixture.git(&["rev-parse", "HEAD"]);
    fixture.write("README.md", "Unrelated prose edit.\n");
    fixture.git(&["add", "--", "README.md"]);
    fixture.git(&["commit", "--quiet", "-m", "unrelated prose only"]);
    assert_eq!(
        fixture.git(&["diff", "--name-status", &base, "HEAD"]),
        "M\tREADME.md"
    );
    let value = fixture.select_git(&base);
    selected(&value, "docs-only", &[]);
    reasons(&value, &[("README.md", "doc")]);
}

#[test]
fn domain_source_selects_all_transitive_dependents_but_not_independent_xtask_tv22() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-domain/src/lib.rs"]));
    selected(&value, "packages", DOMAIN);
    reasons(&value, &[("crates/orch-domain/src/lib.rs", "crate")]);
}

#[test]
fn midgraph_source_follows_normal_build_and_development_edges_tv23_tv24() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-mid/src/lib.rs"]));
    selected(&value, "packages", MID);
    reasons(&value, &[("crates/orch-mid/src/lib.rs", "crate")]);
}

#[test]
fn development_only_dependent_is_selected_tv24() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-build-user/src/lib.rs"]));
    selected(&value, "packages", &["orch-build-user", "orch-dev-user"]);
    reasons(&value, &[("crates/orch-build-user/src/lib.rs", "crate")]);
}

#[test]
fn document_does_not_narrow_a_source_selection_tv25() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["README.md", "crates/orch-leaf/src/lib.rs"]));
    selected(&value, "packages", LEAF);
    reasons(
        &value,
        &[
            ("README.md", "doc"),
            ("crates/orch-leaf/src/lib.rs", "crate"),
        ],
    );
}

#[test]
fn global_inputs_select_everything_with_each_path_explained_tv26_through_tv30() {
    let fixture = Fixture::new();
    for path in [
        "Cargo.lock",
        "Cargo.toml",
        "crates/orch-leaf/Cargo.toml",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        "clippy.toml",
        "deny.toml",
        "xtask/src/main.rs",
        ".github/workflows/ci.yml",
        ".config/nextest.toml",
        "fixtures/shared.json",
        "skills/sample/SKILL.md",
        "examples/demo.rs",
        "crates/orch-leaf/tests/common/mod.rs",
        "crates/orch-core/src/test_support/mod.rs",
        ".gitignore",
        ".gitattributes",
    ] {
        let value = fixture.select(&modified(&[path]));
        selected(&value, "all", &[]);
        reasons(&value, &[(path, "global")]);
    }
}

#[test]
fn unknown_paths_are_not_documents_or_unowned_crates_tv31_tv33() {
    let fixture = Fixture::new();
    for path in [
        "Makefile",
        "newdir/file",
        "newdir/notes.md",
        "crates/not-a-member/src/lib.rs",
        "crates/orch-leaf-evil/src/lib.rs",
    ] {
        let value = fixture.select(&modified(&[path]));
        selected(&value, "all", &[]);
        reasons(&value, &[(path, "unknown")]);
    }
}

#[test]
fn embedded_markdown_is_owned_code_tv32() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-mid/BACKEND.md"]));
    selected(&value, "packages", MID);
    reasons(&value, &[("crates/orch-mid/BACKEND.md", "crate")]);
}

#[test]
fn renames_and_copies_classify_both_source_and_destination_tv33() {
    let fixture = Fixture::new();
    for status in ["R100", "C100"] {
        let changes =
            format!("{status}\0crates/orch-leaf/src/old.rs\0crates/orch-mid/src/new.rs\0");
        let value = fixture.select(changes.as_bytes());
        selected(
            &value,
            "packages",
            &["orch-build-user", "orch-dev-user", "orch-leaf", "orch-mid"],
        );
        reasons(
            &value,
            &[
                ("crates/orch-leaf/src/old.rs", "crate"),
                ("crates/orch-mid/src/new.rs", "crate"),
            ],
        );
    }
}

#[test]
fn document_renamed_into_crate_selects_destination_owner_tv33() {
    let fixture = Fixture::new();
    let value = fixture.select(b"R100\0docs/old.md\0crates/orch-leaf/new.md\0");
    selected(&value, "packages", LEAF);
    reasons(
        &value,
        &[("docs/old.md", "doc"), ("crates/orch-leaf/new.md", "crate")],
    );
}

#[test]
fn deleted_source_keeps_its_package_owner_tv33() {
    let fixture = Fixture::new();
    let value = fixture.select(b"D\0crates/orch-mid/src/deleted.rs\0");
    selected(&value, "packages", MID);
    reasons(&value, &[("crates/orch-mid/src/deleted.rs", "crate")]);
}

#[test]
fn architecture_document_selects_xtask_tv34() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["docs/ARCHITECTURE.md"]));
    selected(&value, "packages", &["xtask"]);
    reasons(&value, &[("docs/ARCHITECTURE.md", "coupled")]);
}

#[test]
fn malformed_and_memberless_metadata_fail_closed_tv35() {
    let fixture = Fixture::new();
    for metadata in [
        b"{".as_slice(),
        b"{}",
        b"{\"packages\":[],\"workspace_members\":[]}",
        b"\xff",
    ] {
        let value = parse(fixture.selection(
            &modified(&["crates/orch-leaf/src/lib.rs"]),
            metadata,
            "json",
        ));
        fail_closed(&value, "error");
    }
}

#[test]
fn malformed_status_and_arity_never_narrow_tv35() {
    let fixture = Fixture::new();
    for changes in [
        b"M\0".as_slice(),
        b"R100\0docs/a.md\0",
        b"C100\0docs/a.md\0",
        b"BOGUS\0README.md\0",
        b"\0README.md\0",
        b"M\0\0",
        b"M\0README.md",
        b"Rno-score\0docs/a.md\0docs/b.md\0",
    ] {
        fail_closed(&fixture.select(changes), "error");
    }
}

#[test]
fn raw_type_change_cannot_take_the_documentation_only_path_tv37() {
    let fixture = Fixture::new();
    let value = fixture.select(b"T\0README.md\0");
    selected(&value, "all", &[]);
    assert!(
        value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "README.md"
                && matches!(r["class"].as_str(), Some("error" | "unknown"))),
        "{value}"
    );
}

#[test]
fn non_utf8_path_bytes_fail_closed_without_lossy_document_classification_tv35() {
    let fixture = Fixture::new();
    // Raw data needs no unsupported non-UTF-8 filename on macOS.
    fail_closed(&fixture.select(b"M\0docs/\xff.md\0"), "error");
}

#[test]
fn empty_change_set_does_not_silently_skip_tests_tv36() {
    let fixture = Fixture::new();
    let value = fixture.select(b"");
    selected(&value, "all", &[]);
    assert!(
        !value["reasons"].as_array().unwrap().is_empty(),
        "empty changes need a reason"
    );
    classified(&value);
}

#[test]
fn every_valid_path_is_explained_even_when_a_global_input_dominates() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&[
        "README.md",
        "Cargo.lock",
        "crates/orch-leaf/src/lib.rs",
        "newdir/file",
    ]));
    selected(&value, "all", &[]);
    reasons(
        &value,
        &[
            ("README.md", "doc"),
            ("Cargo.lock", "global"),
            ("crates/orch-leaf/src/lib.rs", "crate"),
            ("newdir/file", "unknown"),
        ],
    );
}

#[test]
fn cli_change_adds_only_coupled_infra_not_its_extra_dependent_tv38() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-cli/src/main.rs"]));
    selected(&value, "packages", &["orch-cli", "orch-infra"]);
    reasons(
        &value,
        &[
            ("crates/orch-cli/src/main.rs", "crate"),
            ("crates/orch-cli/src/main.rs", "coupled"),
        ],
    );
    let coupled = value["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["class"] == "coupled")
        .unwrap();
    assert!(
        coupled["selects"].as_str().unwrap().contains("orch-infra"),
        "{coupled}"
    );
}

#[test]
fn upstream_cli_only_change_couples_infra_after_closure_tv38() {
    let fixture = Fixture::new();
    // Make leaf an upstream of CLI but not infra. The independent sentinel still
    // depends only on infra, and must not leak into the added coupling closure.
    let mut metadata: Value = serde_json::from_str(&fixture.metadata()).unwrap();
    let cli = metadata["packages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p["name"] == "orch-cli")
        .unwrap();
    cli["dependencies"].as_array_mut().unwrap().push(json!({
        "name": "orch-leaf", "kind": null, "source": null,
        "path": fixture.root.join("crates/orch-leaf"),
    }));
    let path = "crates/orch-leaf/src/lib.rs";
    let value =
        parse(fixture.selection(&modified(&[path]), metadata.to_string().as_bytes(), "json"));
    selected(&value, "packages", &["orch-cli", "orch-infra", "orch-leaf"]);
    reasons(&value, &[(path, "crate"), (path, "coupled")]);
    assert!(
        value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["class"] == "coupled" && r["selects"] == "orch-infra")
    );
}

#[test]
fn mixed_cli_and_infra_changes_use_normal_closure_without_coupling_tv38() {
    let fixture = Fixture::new();
    let paths = [
        "crates/orch-cli/src/main.rs",
        "crates/orch-infra/src/lib.rs",
    ];
    let value = fixture.select(&modified(&paths));
    selected(&value, "packages", INFRA);
    reasons(&value, &[(paths[0], "crate"), (paths[1], "crate")]);
}

#[test]
fn infra_change_uses_normal_closure_without_a_coupled_reason_tv38() {
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&["crates/orch-infra/src/lib.rs"]));
    selected(&value, "packages", INFRA);
    reasons(&value, &[("crates/orch-infra/src/lib.rs", "crate")]);
}

#[test]
fn permutation_and_duplicate_paths_keep_sorted_exact_selection_and_reason_sets() {
    let fixture = Fixture::new();
    let first = fixture.select(&modified(&[
        "crates/orch-mid/src/lib.rs",
        "README.md",
        "crates/orch-leaf/src/lib.rs",
    ]));
    let second = fixture.select(&modified(&[
        "crates/orch-leaf/src/lib.rs",
        "crates/orch-mid/src/lib.rs",
        "README.md",
        "README.md",
    ]));
    for value in [&first, &second] {
        selected(
            value,
            "packages",
            &["orch-build-user", "orch-dev-user", "orch-leaf", "orch-mid"],
        );
        reasons(
            value,
            &[
                ("crates/orch-mid/src/lib.rs", "crate"),
                ("README.md", "doc"),
                ("crates/orch-leaf/src/lib.rs", "crate"),
            ],
        );
    }
    // Byte-for-byte deterministic ordering for the same input is part of the
    // wire output; the order of different input permutations is not prescribed.
    assert_eq!(
        first,
        fixture.select(&modified(&[
            "crates/orch-mid/src/lib.rs",
            "README.md",
            "crates/orch-leaf/src/lib.rs"
        ]))
    );
}

#[test]
fn github_projection_matches_json_selection_and_keeps_reasons_visible() {
    let fixture = Fixture::new();
    let output = fixture.selection(
        &modified(&["crates/orch-leaf/src/lib.rs"]),
        fixture.metadata().as_bytes(),
        "github",
    );
    assert_eq!(output.status.code(), Some(0), "{}", diagnostic(&output));
    let fields: BTreeMap<_, _> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| line.split_once('=').expect("github key=value output"))
        .collect();
    assert_eq!(fields.get("mode"), Some(&"packages"));
    assert_eq!(
        serde_json::from_str::<Value>(fields["packages"]).unwrap(),
        json!(LEAF)
    );
    assert!(
        diagnostic(&output).contains("crates/orch-leaf/src/lib.rs"),
        "reasons remain observable"
    );
}

#[test]
fn missing_fixture_inputs_fail_closed() {
    let fixture = Fixture::new();
    let mut missing = fixture.command(env!("CARGO_BIN_EXE_xtask"));
    missing
        .args(["gate", "select", "--format", "json", "--changed-from"])
        .arg(fixture.temporary.join("absent-changes"))
        .arg("--metadata-from")
        .arg(fixture.temporary.join("absent-metadata"));
    fail_closed(
        &parse(fixture.run(missing, Duration::from_secs(15))),
        "error",
    );
}

#[test]
fn half_fixture_pair_is_a_usage_error() {
    let fixture = Fixture::new();
    let mut half = fixture.command(env!("CARGO_BIN_EXE_xtask"));
    half.args(["gate", "select", "--changed-from"])
        .arg(fixture.temporary.join("unused"));
    assert_eq!(
        fixture.run(half, Duration::from_secs(15)).status.code(),
        Some(2)
    );
}

#[test]
fn unknown_base_is_an_error_classification_not_a_usage_failure_tv37() {
    let fixture = Fixture::new();
    fixture.repository();
    fail_closed(&fixture.select_git("refs/heads/absent-base"), "error");
}

#[test]
fn git_includes_committed_staged_unstaged_and_untracked_changes_tv37() {
    for state in ["committed", "staged", "unstaged", "untracked"] {
        let fixture = Fixture::new();
        fixture.repository();
        let path = if state == "untracked" {
            "crates/leaf/src/new.rs"
        } else {
            "crates/leaf/src/lib.rs"
        };
        fixture.write(path, "pub fn changed() {}\n");
        if matches!(state, "committed" | "staged") {
            fixture.git(&["add", "--", path]);
        }
        if state == "committed" {
            fixture.git(&["commit", "--quiet", "-m", "source change"]);
        }
        let value = fixture.select_git(BASE);
        selected(&value, "packages", &["leaf"]);
        reasons(&value, &[(path, "crate")]);
    }
}

#[test]
fn git_rename_retains_old_and_new_owners_tv33() {
    let fixture = Fixture::new();
    fixture.repository();
    fixture.git(&[
        "mv",
        "--",
        "crates/leaf/src/lib.rs",
        "crates/other/src/moved.rs",
    ]);
    let value = fixture.select_git(BASE);
    selected(&value, "packages", &["leaf", "other"]);
    reasons(
        &value,
        &[
            ("crates/leaf/src/lib.rs", "crate"),
            ("crates/other/src/moved.rs", "crate"),
        ],
    );
}

#[test]
fn git_deletion_keeps_owner_even_when_the_file_no_longer_exists_tv33() {
    let fixture = Fixture::new();
    fixture.repository();
    fixture.git(&["rm", "--", "crates/leaf/src/lib.rs"]);
    let value = fixture.select_git(BASE);
    selected(&value, "packages", &["leaf"]);
    reasons(&value, &[("crates/leaf/src/lib.rs", "crate")]);
}

#[test]
fn git_symlink_and_type_changes_select_all_without_following_the_target_tv37() {
    for replacement in [false, true] {
        let fixture = Fixture::new();
        fixture.repository();
        let outside = fixture.temporary.join("outside-canary");
        fs::write(&outside, b"outside unchanged").unwrap();
        let path = if replacement {
            "crates/leaf/src/lib.rs"
        } else {
            "docs/linked.md"
        };
        if replacement {
            fs::remove_file(fixture.root.join(path)).unwrap();
        }
        symlink(&outside, fixture.root.join(path)).unwrap();
        let value = fixture.select_git(BASE);
        selected(&value, "all", &[]);
        assert!(
            value["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == path
                    && matches!(r["class"].as_str(), Some("error" | "unknown"))),
            "{value}"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"outside unchanged");
    }
}

#[test]
fn git_submodule_index_change_selects_all_without_fetching_tv37() {
    let fixture = Fixture::new();
    fixture.repository();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    // Install a local gitlink directly: no submodule clone, URL or network.
    fixture.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},docs/submodule"),
    ]);
    let value = fixture.select_git(BASE);
    selected(&value, "all", &[]);
    assert!(
        value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "docs/submodule"
                && matches!(r["class"].as_str(), Some("error" | "unknown"))),
        "{value}"
    );
}

#[test]
fn hostile_names_are_lossless_data_and_never_shell_code_tv37() {
    let fixture = Fixture::new();
    fixture.repository();
    let paths = [
        "crates/leaf/src/space name.rs",
        "crates/leaf/src/line\nbreak.rs",
        "crates/leaf/src/$(touch INJECTED).rs",
        "crates/leaf/src/quote'\";touch INJECTED;.rs",
        "crates/leaf/src/-leading.rs",
    ];
    for path in paths {
        fixture.write(path, "fixture\n");
    }
    let value = fixture.select_git(BASE);
    selected(&value, "packages", &["leaf"]);
    let expected: Vec<_> = paths.iter().map(|p| (*p, "crate")).collect();
    reasons(&value, &expected);
    assert!(
        !fixture.root.join("INJECTED").exists(),
        "shell interpretation of filename"
    );
    fixture.write("-leading-root.rs", "untracked unknown root\n");
    let value = fixture.select_git(BASE);
    selected(&value, "all", &[]);
    let mut expected = expected;
    expected.push(("-leading-root.rs", "unknown"));
    reasons(&value, &expected);
}

#[test]
fn hostile_base_argument_cannot_become_a_git_option_or_shell_program_tv37() {
    let fixture = Fixture::new();
    fixture.repository();
    for base in [
        "--help",
        "--output=INJECTED",
        "$(touch INJECTED)",
        "HEAD;touch INJECTED",
    ] {
        fail_closed(&fixture.select_git(base), "error");
        assert!(!fixture.root.join("INJECTED").exists());
    }
}

#[test]
fn selected_packages_build_current_cli_before_infra_path_exec() {
    build_current_cli_before_infra_path_exec("crates/orch-cli/src/main.rs");
}

#[test]
fn sqlite_spec_selection_builds_current_cli_before_infra_path_exec_on_cold_target() {
    build_current_cli_before_infra_path_exec(SQLITE_SPEC);
}

fn build_current_cli_before_infra_path_exec(changed_path: &str) {
    // D31 thin probe only. No product orch, real AI CLI, registry or shared
    // helper is used. Mac/native Linux and actual concurrent_index are separate
    // validation obligations. No gate/timing claim follows from this fixture.
    let fixture = Fixture::new();
    let value = fixture.select(&modified(&[changed_path]));
    selected(&value, "packages", &["orch-cli", "orch-infra"]);
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/orch-cli\", \"crates/orch-infra\"]\nresolver = \"3\"\n",
    );
    fixture.write("crates/orch-cli/Cargo.toml", "[package]\nname = \"orch-cli\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[[bin]]\nname = \"orch\"\npath = \"src/main.rs\"\n");
    fixture.write("crates/orch-cli/tests/representative.rs", "#[test]\nfn cli_target() { assert!(std::path::Path::new(env!(\"CARGO_BIN_EXE_orch\")).is_file()); }\n");
    fixture.write(
        "crates/orch-infra/Cargo.toml",
        "[package]\nname = \"orch-infra\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    fixture.write(
        "crates/orch-infra/src/lib.rs",
        "// dependency-free fixture\n",
    );
    fixture.write("crates/orch-infra/tests/by_path.rs", r#"
#[test]
fn executes_current_cli_marker() {
    let executable = std::path::Path::new(&std::env::var_os("CARGO_TARGET_DIR").unwrap()).join("debug/orch");
    let output = std::process::Command::new(executable).env_clear().output().unwrap();
    assert!(output.status.success());
    let marker = String::from_utf8(output.stdout).unwrap();
    assert_eq!(marker.trim(), std::env::var("EXPECTED_MARKER").unwrap());
    println!("path-exec-marker={}", marker.trim());
}
"#);
    assert!(
        !fixture.temporary.join("target").exists(),
        "probe starts from an empty unique target"
    );
    for marker in ["first-current-source", "second-current-source"] {
        fixture.write(
            "crates/orch-cli/src/main.rs",
            format!("fn main() {{ println!(\"{marker}\"); }}\n"),
        );
        let mut command = fixture.command(env!("CARGO"));
        command.args(["test", "--offline", "--no-fail-fast"]);
        for package in value["packages"].as_array().unwrap() {
            command.arg("-p").arg(package.as_str().unwrap());
        }
        command
            .args(["--", "--nocapture"])
            .env("EXPECTED_MARKER", marker);
        eprintln!(
            "D31 marker={marker} target={} command={command:?}",
            fixture.temporary.join("target").display()
        );
        let output = fixture.run(command, Duration::from_secs(45));
        eprintln!("D31 marker={marker}: {}", diagnostic(&output));
        assert!(
            output.status.success(),
            "D31 build-before-path-exec ({marker}): {}",
            diagnostic(&output)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("path-exec-marker={marker}")),
            "the path-exec test must actually run: {}",
            diagnostic(&output)
        );
    }
}

#[test]
fn real_cargo_path_dependencies_include_every_workspace_edge_kind_i2() {
    // Each variant has exactly one edge. A missing normal/build/dev/optional/
    // target-specific or renamed edge therefore cannot hide behind another.
    for dependency in [
        "[dependencies]\nleaf = { path = \"../leaf\" }\n",
        "[build-dependencies]\nleaf = { path = \"../leaf\" }\n",
        "[dev-dependencies]\nleaf = { path = \"../leaf\" }\n",
        "[dependencies]\nleaf = { path = \"../leaf\", optional = true }\n",
        "[target.'cfg(target_os = \"none\")'.dependencies]\nleaf = { path = \"../leaf\" }\n",
        "[dependencies]\nalias = { package = \"leaf\", path = \"../leaf\" }\n",
    ] {
        let fixture = Fixture::new();
        fixture.repository();
        let manifest = fixture.root.join("crates/other/Cargo.toml");
        let mut contents = fs::read_to_string(&manifest).unwrap();
        contents.push_str(dependency);
        fs::write(manifest, contents).unwrap();
        let mut command = fixture.command(env!("CARGO"));
        command.args([
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ]);
        let output = fixture.run(command, Duration::from_secs(20));
        assert!(
            output.status.success(),
            "{dependency}: {}",
            diagnostic(&output)
        );
        let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
        let other = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "other")
            .unwrap();
        assert_eq!(other["dependencies"].as_array().unwrap().len(), 1);
        assert_eq!(other["dependencies"][0]["name"], "leaf");
        let root = Path::new(metadata["workspace_root"].as_str().unwrap());
        assert_eq!(
            other["dependencies"][0]["path"],
            root.join("crates/leaf").to_str().unwrap()
        );
        let path = "crates/leaf/src/lib.rs";
        // Exercise both the fixture parser with unmodified Cargo JSON and the
        // actual Git/Cargo collection (including /tmp canonicalization on Mac).
        let value = parse(fixture.selection(&modified(&[path]), &output.stdout, "json"));
        selected(&value, "packages", &["leaf", "other"]);
        reasons(&value, &[(path, "crate")]);
        fixture.git(&["add", "--", "."]);
        fixture.git(&["commit", "--quiet", "-m", "path dependency baseline"]);
        fixture.git(&["branch", "-f", BASE]);
        fixture.write(path, "pub fn changed_marker() {}\n");
        let value = fixture.select_git(BASE);
        selected(&value, "packages", &["leaf", "other"]);
        reasons(&value, &[(path, "crate")]);
    }
}

#[test]
fn missing_coupling_member_and_ambiguous_dependency_paths_fail_closed() {
    let fixture = Fixture::new();
    let original: Value = serde_json::from_str(&fixture.metadata()).unwrap();
    for corruption in ["missing-member", "path-mismatch", "outside-path"] {
        let mut metadata = original.clone();
        match corruption {
            "missing-member" => {
                let packages = metadata["packages"].as_array_mut().unwrap();
                packages.retain(|p| p["name"] != "orch-infra");
                let members: Vec<_> = packages.iter().map(|p| p["id"].clone()).collect();
                metadata["workspace_members"] = json!(members);
                // No dangling graph edge may preempt the missing coupling row.
                for package in metadata["packages"].as_array_mut().unwrap() {
                    package["dependencies"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|d| d["name"] != "orch-infra");
                }
            }
            _ => {
                let cli = metadata["packages"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|p| p["name"] == "orch-cli")
                    .unwrap();
                cli["dependencies"][0]["path"] = json!(if corruption == "path-mismatch" {
                    fixture.root.join("crates/orch-leaf")
                } else {
                    fixture.temporary.join("outside")
                });
            }
        }
        let value = parse(fixture.selection(
            &modified(&["crates/orch-cli/src/main.rs"]),
            metadata.to_string().as_bytes(),
            "json",
        ));
        fail_closed(&value, "error");
        let why = value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["why"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let expected = match corruption {
            "missing-member" => "missing orch-infra member",
            "path-mismatch" => "dependency name/path mismatch",
            _ => "ambiguous member dependency path",
        };
        assert!(why.contains(expected), "{corruption}: {value}");
    }
}

#[test]
fn command_failures_select_all_with_input_error_reasons() {
    for failing in ["git", "cargo"] {
        let fixture = Fixture::new();
        fixture.repository();
        fixture.write("crates/leaf/src/lib.rs", "pub fn changed() {}\n");
        let shims = fixture.temporary.join("failing-command");
        fs::create_dir(&shims).unwrap();
        let script = shims.join(failing);
        // Static failure shims only; argv forwarding is quoted and no arbitrary
        // command or source input is evaluated as shell code.
        let body = if failing == "git" {
            "#!/bin/sh\ncase \"$*\" in *status*) exit 7;; esac\nexec /usr/bin/git \"$@\"\n"
        } else {
            "#!/bin/sh\nexit 7\n"
        };
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = fixture.command(env!("CARGO_BIN_EXE_xtask"));
        let path = command
            .get_envs()
            .find(|(k, _)| *k == "PATH")
            .unwrap()
            .1
            .unwrap()
            .to_owned();
        let mut paths = vec![shims];
        paths.extend(std::env::split_paths(&path));
        command
            .env("PATH", std::env::join_paths(paths).unwrap())
            .args(["gate", "select", "--base", BASE, "--format", "json"]);
        let value = parse(fixture.run(command, Duration::from_secs(20)));
        fail_closed(&value, "error");
        assert!(
            value["reasons"].as_array().unwrap().iter().any(|r| r["why"]
                .as_str()
                .unwrap()
                .contains(if failing == "git" {
                    "git query failed"
                } else {
                    "cargo metadata failed"
                })),
            "{value}"
        );
    }
    let fixture = Fixture::new();
    fixture.repository();
    fixture.write("Cargo.toml", "invalid Cargo TOML {\n");
    let value = fixture.select_git(BASE);
    fail_closed(&value, "error");
    assert!(
        value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["why"].as_str().unwrap().contains("cargo metadata failed")),
        "{value}"
    );
}

#[test]
fn text_and_github_escape_hostile_reason_bytes_without_new_commands() {
    let fixture = Fixture::new();
    let path = "crates/orch-leaf/src/\n::error::injected\u{1b}[31m.rs";
    let changes = modified(&[path]);
    for format in ["text", "github"] {
        let output = fixture.selection(&changes, fixture.metadata().as_bytes(), format);
        assert_eq!(output.status.code(), Some(0), "{}", diagnostic(&output));
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let reasons = if format == "text" { &stdout } else { &stderr };
        assert!(reasons.contains(&serde_json::to_string(path).unwrap()));
        assert!(!reasons.contains('\u{1b}'));
        assert!(!reasons.lines().any(|line| line.starts_with("::")));
        if format == "github" {
            assert_eq!(stdout, "mode=packages\npackages=[\"orch-leaf\"]\n");
        } else {
            assert!(stdout.starts_with("mode: packages\npackages: [\"orch-leaf\"]\n"));
        }
    }
}
