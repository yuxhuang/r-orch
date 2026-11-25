//! Fixed workflow-shape controls and execution of its actual classification shell.
//! These do not emulate GitHub scheduling/required-check semantics (TV50–55).
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const CI: &str = include_str!("../../.github/workflows/ci.yml");
const TEST_IF: &str = "github.event_name != 'schedule' && needs.changes.result == 'success' && (needs.changes.outputs.mode == 'packages' || needs.changes.outputs.mode == 'all')";
const AMBIENT_IF: &str = "github.event_name == 'push' && github.ref == 'refs/heads/main'";
const AMBIENT_TARGET: &str = "${{ github.workspace }}/target/ci-ambient";
/// The suite runner, pinned. A job that runs tests installs exactly this.
const INSTALL: &str = "      - run: cargo install cargo-nextest --locked --version 0.9.152 --target-dir \"$RUNNER_TEMP/nextest-install\"";
/// One step per pass, in `nextest::PROFILES` order. The pass split itself is
/// defined only in `.config/nextest.toml`; this pins that the ambient job runs
/// both halves of it, and `xtask/tests/nextest_isolation.rs` pins that the
/// three readers of the split agree.
///
/// Every step after the first carries `if: ${{ !cancelled() }}`, which is this
/// job's `--no-fail-fast` across steps: a failing pass must not hide the other
/// pass's result or the doc-tests'. That is why these are the `run:` halves of
/// a two-line step, and why [`GUARD`] is pinned beside them.
const AMBIENT_PASSES: [&str; 2] = [
    "      - run: cargo nextest run --profile default --workspace --no-fail-fast --no-tests fail",
    "        run: cargo nextest run --profile alone --workspace --no-fail-fast --no-tests fail",
];
const AMBIENT_SUITE: &str = AMBIENT_PASSES[0];
const AMBIENT_DOC: &str = "        run: cargo test --workspace --doc --no-fail-fast";
/// The step guard that keeps a failed pass from skipping what follows it.
const GUARD: &str = "      - if: ${{ !cancelled() }}";

// This extracts only this file's fixed indentation, not arbitrary YAML. Shape
// changes require deliberate updates; do not silently accept an unknown form.
fn job<'a>(ci: &'a str, name: &str) -> &'a str {
    let start = ci.find(&format!("\n  {name}:\n")).expect("named job");
    let tail = &ci[start + 1..];
    let end = tail
        .lines()
        .skip(1)
        .find(|line| line.starts_with("  ") && !line.starts_with("   ") && line.ends_with(':'));
    end.map_or(tail, |line| {
        &tail[..tail.find(&format!("\n{line}\n")).unwrap()]
    })
}
fn step<'a>(job: &'a str, name: &str) -> &'a str {
    let start = job
        .find(&format!("      - name: {name}\n"))
        .expect("named step");
    let tail = &job[start..];
    let end = tail[1..]
        .find("\n      - ")
        .map(|n| n + 1)
        .unwrap_or(tail.len());
    &tail[..end]
}
fn script(step: &str) -> String {
    let (_, body) = step.split_once("        run: |\n").expect("block shell");
    body.lines()
        .map_while(|line| line.strip_prefix("          "))
        .map(|line| format!("{line}\n"))
        .collect()
}
fn contract(ci: &str) {
    let changes = job(ci, "changes");
    let check = job(ci, "check");
    let test = job(ci, "test");
    let ambient = job(ci, "ambient");
    assert!(changes.contains("fetch-depth: 0"));
    assert!(changes.contains("--base \"$PR_BASE\" --format github"));
    assert!(changes.contains("printf 'mode=all\\n'"));
    assert!(check.contains("    needs: changes\n    if: always()\n"));
    assert!(check.contains("- name: Require valid classification\n"));
    assert!(check.contains("[[ \"$CHANGES_RESULT\" == success ]] || exit 1"));
    assert!(check.contains("[[ \"$CHANGES_RESULT\" == skipped && -z \"$MODE\" ]] || exit 1"));
    assert!(check.contains(
        "if: needs.changes.outputs.mode == 'docs-only'\n        run: cargo build --workspace"
    ));
    assert!(test.contains(&format!("    if: {TEST_IF}\n")));
    assert!(ambient.contains(&format!("    if: {AMBIENT_IF}\n")));
    assert_eq!(
        ci.matches("./xtask/gate.sh pr --part suite --receipt")
            .count(),
        1
    );
    assert!(!check.contains("cargo test"));
    assert!(!changes.contains("cargo test"));
    // Every job that runs tests installs the pinned runner, and no job that
    // does not run tests pays for it.
    for (name, job) in [("test", test), ("ambient", ambient)] {
        assert!(
            job.lines().any(|line| line == INSTALL),
            "{name} must install the pinned runner"
        );
    }
    assert!(!check.contains("cargo install"));
    assert!(!changes.contains("cargo install"));
    assert_eq!(ci.matches("cargo install cargo-nextest").count(), 2);
    assert_eq!(ci.matches("--version 0.9.152").count(), 2);
    assert_ambient_target_contract(ci);
    assert!(!ambient.contains("./xtask/gate.sh"));
    let suite = step(test, "Folded suite");
    assert!(
        script(suite).lines().any(|line| line == "exec ./xtask/gate.sh pr --part suite --receipt \"$RUNNER_TEMP/pr3-suite-receipt\" --lease-wait 5 --host-label github-hosted-linux"),
        "folded producer must replace the step shell with unchanged argv"
    );
    assert!(suite.contains("--lease-wait 5 --host-label github-hosted-linux"));
    assert!(!ci.contains("--no-lease"));
    assert!(!ci.contains("XTASK_GATE_LEASE_DIR"));
    assert!(!ci.contains("continue-on-error:"));
    assert!(!suite.contains("||"));
    let verify = step(test, "Verify diagnostic receipt");
    assert!(verify.contains("if: always()\n"));
    assert!(verify.contains(
        "verify-receipt \"$RUNNER_TEMP/pr3-suite-receipt\" --commit \"$(git rev-parse HEAD)\""
    ));
    let upload = step(test, "Retain verified redacted receipt");
    assert!(upload.contains("if: always() && steps.receipt.outcome == 'success'"));
    assert!(upload.contains("path: ${{ runner.temp }}/pr3-suite-receipt\n"));
    assert!(upload.contains("if-no-files-found: error"));
    assert!(upload.contains("include-hidden-files: false"));
    assert_eq!(ci.matches("Swatinem/rust-cache@").count(), 4);
    assert_eq!(ci.matches("          cache-targets: false\n").count(), 4);
    assert_eq!(ci.matches("          cache-bin: false\n").count(), 4);
    for forbidden in [
        "cache-directories:",
        "workspaces:",
        "path: target",
        "paths-ignore:",
        "pull_request_target:",
        "contents: write",
        "persist-credentials: true",
    ] {
        assert!(!ci.contains(forbidden), "unexpected {forbidden}");
    }
    assert!(ci.contains("permissions:\n  contents: read\n"));
    assert_eq!(ci.matches("persist-credentials: false").count(), 4);
    for line in ci.lines().filter(|l| l.contains("uses:")) {
        let pin = line
            .split('@')
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert_eq!(pin.len(), 40, "action must be commit-pinned");
        assert!(pin.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert!(
        check.contains("EmbarkStudios/cargo-deny-action@3c6349835b2b7b196a839186cb8b78e02f7b5f25")
    );
    assert!(check.contains("test -s Cargo.lock"));
    assert!(check.contains("! grep -qE '^name = \"(terminfo|termwiz)\"$' Cargo.lock"));
    assert!(test.contains("timeout-minutes: 40"));
    assert!(ambient.contains("timeout-minutes: 40"));
    // Event data belongs in env, never in shell source.
    for body in ci.split("        run: |\n").skip(1) {
        for line in body
            .lines()
            .take_while(|line| line.starts_with("          "))
        {
            assert!(!line.contains("${{"), "event expression in shell");
        }
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "pr3-ci-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        for name in ["home", "tmp", "xtask", "target", "bin"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(name))
                .unwrap();
        }
        Self(root)
    }
    fn command(&self) -> Command {
        let mut c = Command::new("/bin/bash");
        c.args(["--noprofile", "--norc", "-s"])
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("TMPDIR", self.0.join("tmp"))
            .env("RUNNER_TEMP", self.0.join("tmp"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            );
        c
    }
    fn run(&self, body: &str, vars: &[(&str, &str)]) -> bool {
        let mut c = self.command();
        for (key, value) in vars {
            c.env(key, value);
        }
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        child.wait().unwrap().success()
    }
    fn executable(&self, relative: &str, bytes: &str) {
        let path = self.0.join(relative);
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(path)
            .unwrap();
        f.write_all(bytes.as_bytes()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn workflow_preserves_routing_receipts_and_cache_boundaries() {
    contract(CI);
}

#[test]
fn classification_guard_executes_fail_closed_truth_table() {
    let f = Fixture::new();
    let body = script(step(job(CI, "check"), "Require valid classification"));
    for (event, reference) in [
        ("pull_request", "refs/pull/1/merge"),
        ("push", "refs/heads/main"),
        ("push", "refs/heads/other"),
        ("schedule", "refs/heads/main"),
        ("workflow_dispatch", "refs/heads/main"),
    ] {
        for result in ["success", "failure", "cancelled", "skipped", "", "unknown"] {
            for mode in [
                "docs-only",
                "packages",
                "all",
                "",
                "unknown",
                "unknown:all",
                "all\ndocs-only",
            ] {
                let expected = if event == "schedule" {
                    result == "skipped" && mode.is_empty()
                } else {
                    result == "success"
                        && ((event == "pull_request"
                            && ["docs-only", "packages", "all"].contains(&mode))
                            || (event == "push" && reference == "refs/heads/main" && mode == "all"))
                };
                assert_eq!(
                    f.run(
                        &body,
                        &[
                            ("EVENT", event),
                            ("REF", reference),
                            ("CHANGES_RESULT", result),
                            ("MODE", mode)
                        ]
                    ),
                    expected,
                    "{event}/{reference}/{result}/{mode}"
                );
            }
        }
    }
    // Removing the refusal really changes a failed PR's execution result.
    let mutant = body.replace("[[ \"$CHANGES_RESULT\" == success ]] || exit 1", ":");
    assert!(f.run(
        &mutant,
        &[
            ("EVENT", "pull_request"),
            ("REF", "refs/pull/1/merge"),
            ("CHANGES_RESULT", "failure"),
            ("MODE", "all")
        ]
    ));
}

#[test]
fn workflow_mutants_are_discriminated() {
    contract(CI);
    for name in ["test", "ambient"] {
        let original = job(CI, name);
        let changed = original.replacen("timeout-minutes: 40", "timeout-minutes: 30", 1);
        assert_ne!(original, changed);
        let mutant = CI.replacen(original, &changed, 1);
        assert!(
            std::panic::catch_unwind(|| contract(&mutant)).is_err(),
            "survived {name} timeout regression"
        );
    }
    for (old, new) in [
        ("if: always()", "if: '!cancelled()'"),
        (
            "- name: Require valid classification",
            "- name: Removed guard",
        ),
        ("--part suite", ""),
        ("exec ./xtask/gate.sh", "./xtask/gate.sh"),
        ("cache-targets: false", "cache-targets: true"),
        ("cache-bin: false", "cache-bin: true"),
        ("if-no-files-found: error", "if-no-files-found: warn"),
        ("--version 0.9.152", "--version 0.9.153"),
        (
            "cargo nextest run --profile default --workspace",
            "cargo test --workspace",
        ),
        (
            "cargo nextest run --profile alone --workspace",
            "cargo nextest run --workspace",
        ),
        ("--no-tests fail", "--no-tests warn"),
        ("cargo test --workspace --doc", "cargo test --workspace"),
        (
            "steps.receipt.outcome == 'success'",
            "steps.suite.outcome == 'success'",
        ),
    ] {
        let mutant = CI.replacen(old, new, 1);
        assert_ne!(mutant, CI);
        assert!(
            std::panic::catch_unwind(|| contract(&mutant)).is_err(),
            "survived {old}"
        );
    }
}

#[test]
fn suite_failure_cannot_be_replaced_by_verifier_success() {
    let f = Fixture::new();
    f.executable("xtask/gate.sh", "#!/bin/sh\nexit \"$PRODUCER_STATUS\"\n");
    let suite = script(step(job(CI, "test"), "Folded suite"));
    for status in ["1", "2", "3", "75", "143"] {
        assert!(!f.run(&suite, &[("PRODUCER_STATUS", status)]));
    }
    assert!(f.run(&suite, &[("PRODUCER_STATUS", "0")]));
    // Verification is independent: even valid diagnostics for failed suites
    // cannot replace the producer step's nonzero status.
    let verify = script(step(job(CI, "test"), "Verify diagnostic receipt"));
    f.executable("bin/git", "#!/bin/sh\nprintf '%040d\\n' 0\n");
    assert!(!f.run(&verify, &[])); // absent final artifact/executable fails
    for relative in ["target/gate-xtask", "target/gate-xtask/debug"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(f.0.join(relative))
            .unwrap();
    }
    f.executable("target/gate-xtask/debug/xtask", "#!/bin/sh\nexit 0\n");
    assert!(f.run(&verify, &[]));
    assert!(!f.run(&suite, &[("PRODUCER_STATUS", "1")]));
}

#[test]
fn main_forces_all_without_running_selector() {
    let f = Fixture::new();
    f.executable("bin/git", "#!/bin/sh\nexit 0\n");
    f.executable("bin/cargo", "#!/bin/sh\nexit 99\n");
    let output = f.0.join("output");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .unwrap();
    assert!(f.run(
        &script(step(job(CI, "changes"), "Classify changes")),
        &[
            ("EVENT", "push"),
            ("REF", "refs/heads/main"),
            ("GITHUB_OUTPUT", output.to_str().unwrap())
        ]
    ));
    assert_eq!(fs::read_to_string(output).unwrap(), "mode=all\n");
}

#[test]
fn pr_base_is_passed_as_data_not_shell() {
    let f = Fixture::new();
    f.executable("bin/git", "#!/bin/sh\nexit 0\n");
    f.executable(
        "bin/cargo",
        "#!/bin/sh\n[ \"$9\" = \"$PR_BASE\" ] || exit 1\nprintf 'mode=all\\npackages=\\n'\n",
    );
    let output = f.0.join("output");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .unwrap();
    assert!(f.run(
        &script(step(job(CI, "changes"), "Classify changes")),
        &[
            ("EVENT", "pull_request"),
            ("REF", "refs/pull/1/merge"),
            ("PR_BASE", "$(touch injected); ' ; mode=docs-only"),
            ("GITHUB_OUTPUT", output.to_str().unwrap())
        ]
    ));
    assert!(!Path::new(&f.0.join("injected")).exists());
    assert_eq!(fs::read_to_string(output).unwrap(), "mode=all\npackages=\n");
}

// Evaluate only the committed job-env workspace expression, not arbitrary
// Actions syntax. Relative values stay relative so the package-cwd negative
// control exercises the same filesystem resolution as concurrent_index.
fn ambient_target_value(ci: &str) -> &str {
    job(ci, "ambient")
        .lines()
        .find_map(|line| line.strip_prefix("      CARGO_TARGET_DIR: "))
        .expect("ambient job target")
}

fn assert_ambient_target_contract(ci: &str) {
    let ambient = job(ci, "ambient");
    assert_eq!(ambient_target_value(ci), AMBIENT_TARGET);
    assert_eq!(ambient.matches("CARGO_TARGET_DIR:").count(), 1);
    assert!(!ambient.contains("working-directory:"));
    // The two suite passes provision the CLI through orch-cli's test targets.
    // No separate build or step-level target/cwd override is needed.
    let cargo_steps: Vec<_> = ambient
        .lines()
        .filter(|line| line.contains("cargo ") && !line.trim_start().starts_with('#'))
        .collect();
    // The runner install, then one step per pass in order, then the doc-tests
    // nextest does not run. Nothing else, and no step-level target or cwd
    // override.
    assert_eq!(
        cargo_steps,
        [INSTALL, AMBIENT_PASSES[0], AMBIENT_PASSES[1], AMBIENT_DOC]
    );
    // The second pass and the doc-tests each run even when the step before
    // them failed, and nothing else in the job is guarded that way.
    for step in [AMBIENT_PASSES[1], AMBIENT_DOC] {
        assert!(
            ambient.contains(&format!("{GUARD}\n{step}\n")),
            "{step} must be guarded by {GUARD}, or a failed pass hides it"
        );
    }
    assert_eq!(ambient.matches(GUARD).count(), 2);
    assert!(!ambient.contains("Build ambient CLI"));
}

fn ambient_target_resolves_from_package(ci: &str) -> bool {
    let f = Fixture::new();
    for relative in [
        "crates",
        "crates/orch-infra",
        "target/ci-ambient",
        "target/ci-ambient/debug",
    ] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(f.0.join(relative))
            .unwrap();
    }
    // Marker for Cargo's workspace artifact, not a fake Cargo executable or
    // an emulation of binary provisioning. Native Cargo proof is separate.
    let artifact = f.0.join("target/ci-ambient/debug/orch");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&artifact)
        .unwrap();
    let raw = ambient_target_value(ci);
    let target = match raw.strip_prefix("${{ github.workspace }}/") {
        Some(suffix) => f.0.join(suffix),
        None => PathBuf::from(raw),
    };
    assert!(artifact.is_file(), "workspace artifact must exist");
    assert!(f.run("test -f target/ci-ambient/debug/orch\n", &[],));
    // Cargo runs this integration test from its package root. Shell test uses
    // actual OS path resolution from that cwd, including the relative failure.
    f.run(
        "set -eu\ncd crates/orch-infra\ntest -f \"$CARGO_TARGET_DIR/debug/orch\"\n",
        &[("CARGO_TARGET_DIR", target.to_str().unwrap())],
    )
}

#[test]
fn ambient_workspace_target_resolves_from_package_cwd() {
    assert!(
        ambient_target_resolves_from_package(CI),
        "concurrent_index must find the workspace artifact from its package cwd"
    );
    assert_ambient_target_contract(CI);
}

/// The ambient job must run both passes, in order, each reached even when the
/// step before it failed, and nothing else.
#[test]
fn an_ambient_job_that_drops_reorders_or_merges_a_pass_is_rejected() {
    // The three lines the split occupies: the first pass, then the guard and
    // the second pass as one step.
    let split = format!("{}\n{}\n{}\n", AMBIENT_PASSES[0], GUARD, AMBIENT_PASSES[1]);
    assert!(CI.contains(&split));
    for (why, mutant) in [
        (
            "the second pass dropped",
            CI.replacen(&format!("{GUARD}\n{}\n", AMBIENT_PASSES[1]), "", 1),
        ),
        (
            "the first pass dropped",
            CI.replacen(&format!("{}\n", AMBIENT_PASSES[0]), "", 1),
        ),
        (
            "the passes reordered",
            CI.replace(
                "--profile default --workspace",
                "--profile reordered --workspace",
            )
            .replace(
                "--profile alone --workspace",
                "--profile default --workspace",
            )
            .replace(
                "--profile reordered --workspace",
                "--profile alone --workspace",
            ),
        ),
        (
            // One unsplit run is what the split replaced.
            "the two passes merged into one unsplit run",
            CI.replacen(
                &split,
                "      - run: cargo nextest run --workspace --no-fail-fast --no-tests fail\n",
                1,
            ),
        ),
        (
            "a pass that drops the empty-run failure",
            CI.replacen(
                AMBIENT_PASSES[1],
                &AMBIENT_PASSES[1].replace(" --no-tests fail", ""),
                1,
            ),
        ),
        (
            "the doc-tests dropped",
            CI.replacen(&format!("{GUARD}\n{AMBIENT_DOC}\n"), "", 1),
        ),
        (
            // Without the guard a failing first pass skips the rest, and the
            // job reports one pass's failure instead of every result.
            "the second pass's guard dropped, so a failed first pass skips it",
            CI.replacen(
                &format!("{GUARD}\n{}", AMBIENT_PASSES[1]),
                &AMBIENT_PASSES[1].replacen("        run:", "      - run:", 1),
                1,
            ),
        ),
        (
            "the doc-tests' guard dropped",
            CI.replacen(
                &format!("{GUARD}\n{AMBIENT_DOC}"),
                &AMBIENT_DOC.replacen("        run:", "      - run:", 1),
                1,
            ),
        ),
    ] {
        assert_ne!(mutant, CI, "{why}");
        assert!(
            std::panic::catch_unwind(|| assert_ambient_target_contract(&mutant)).is_err(),
            "{why} must be rejected: {mutant}"
        );
    }
}

#[test]
fn ambient_relative_target_and_override_controls_are_discriminated() {
    let relative = CI.replacen(AMBIENT_TARGET, "target/ci-ambient", 1);
    assert_ne!(relative, CI);
    assert!(
        !ambient_target_resolves_from_package(&relative),
        "the original relative target must fail despite the workspace artifact"
    );
    let suite = AMBIENT_SUITE;
    for mutant in [
        relative,
        CI.replacen(AMBIENT_TARGET, "${{ github.workspace }}/target/wrong", 1),
        CI.replacen(
            suite,
            &format!("{suite}\n        env:\n          CARGO_TARGET_DIR: target/wrong"),
            1,
        ),
        CI.replacen(
            suite,
            &format!("{suite}\n        working-directory: crates/orch-infra"),
            1,
        ),
    ] {
        assert_ne!(mutant, CI);
        assert!(
            std::panic::catch_unwind(|| assert_ambient_target_contract(&mutant)).is_err(),
            "target/cwd mutation must be rejected"
        );
    }
}
