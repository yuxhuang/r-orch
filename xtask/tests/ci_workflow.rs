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
    assert!(ambient.contains("- run: cargo test --workspace --no-fail-fast"));
    assert!(!ambient.contains("./xtask/gate.sh"));
    let suite = step(test, "Folded suite");
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
    assert!(test.contains("timeout-minutes: 30"));
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
    for (old, new) in [
        ("if: always()", "if: '!cancelled()'"),
        (
            "- name: Require valid classification",
            "- name: Removed guard",
        ),
        ("--part suite", ""),
        ("cache-targets: false", "cache-targets: true"),
        ("cache-bin: false", "cache-bin: true"),
        ("if-no-files-found: error", "if-no-files-found: warn"),
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
