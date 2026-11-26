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
/// The workspace's path inside a job container, on GitHub-hosted and
/// self-hosted runners alike. The `github.workspace` context holds the host's
/// path instead, which does not exist inside the container.
const CONTAINER_WORKSPACE: &str = "/__w/r-orch/r-orch";
const AMBIENT_TARGET: &str = "/__w/r-orch/r-orch/target/ci-ambient";
/// Every job runs in this container, so hosted and self-hosted runs match.
const JOB_IMAGE: &str = "      image: docker.io/library/rust:1-trixie@sha256:e84068000361bf3d4a5ba2b7647f62893528b1eb9f847d7a23b493f5ce8ea254";
/// Trusted events and same-repo pull requests use the self-hosted runner the
/// repository variable names; fork pull requests, and everything while the
/// variable is unset, stay on GitHub-hosted runners.
const ROUTED: &str = "    runs-on: ${{ (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && vars.CI_SELF_HOSTED_RUNNER || 'ubuntu-latest' }}";
/// `ambient` runs on pushes to main only, so it never sees fork code.
const AMBIENT_ROUTED: &str = "    runs-on: ${{ vars.CI_SELF_HOSTED_RUNNER || 'ubuntu-latest' }}";
/// cargo-deny 0.20.2, the release `cargo-deny-action` v2.1.1 ran, by digest.
const DENY_FETCH: &str = "          curl -fsSL --proto '=https' -o \"$archive\" https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz";
const DENY_VERIFY: &str = "          echo \"9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f  $archive\" | sha256sum -c -";
const DENY_RUN: &str =
    "          \"$RUNNER_TEMP/cargo-deny\" --log-level warn --all-features check";
/// `runner.environment` is `github-hosted` or `self-hosted`, so hosted
/// receipts keep their `github-hosted-linux` label.
/// The job container runs as root over a workspace the runner's user owns;
/// git and the gate both require the checkout's owner to be the job's user.
const OWN_WORKSPACE: &str = "      - run: chown -R \"$(id -u):$(id -g)\" \"$GITHUB_WORKSPACE\"";
/// Steps that run tests drop to the suite user and every capability, under
/// tini as a subreaper: as root a read-only directory never refuses a write,
/// and the container's PID 1 reaps no orphans.
const UNPRIVILEGED: &str = "        shell: setpriv --reuid=1001 --regid=1001 --init-groups -- env HOME=/home/ci tini -s -- bash --noprofile --norc -eo pipefail {0}";
const SUITE_USER: &str =
    "          useradd --uid 1001 --user-group --create-home --home-dir /home/ci ci";
const SUITE_OWNS: &str =
    "          chown -R ci:ci \"$GITHUB_WORKSPACE\" \"$CARGO_HOME\" \"$RUSTUP_HOME\"";
/// The gate exports its receipt into a new directory under `$RUNNER_TEMP`.
const SUITE_TEMP: &str = "          chown ci:ci \"$RUNNER_TEMP\"";
const HOST_LABEL: &str = "          HOST_LABEL: ${{ runner.environment }}-linux";
/// The suite runner, pinned. A job that runs tests installs exactly this.
const INSTALL: &str = "      - run: cargo install cargo-nextest --locked --version 0.9.152 --target-dir \"$RUNNER_TEMP/nextest-install\" --root \"$HOME/.nextest-bin\"";
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
    for lane in [test, ambient] {
        let restore = step(lane, "Restore nextest executable");
        assert!(restore.contains("id: nextest-cache\n"));
        assert!(restore.contains("actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830"));
        assert!(restore.contains("path: ~/.nextest-bin/bin/cargo-nextest\n"));
        assert!(restore.contains("key: nextest-${{ runner.os }}-${{ runner.arch }}-0.9.152-${{ hashFiles('rust-toolchain.toml') }}"));
        assert!(!restore.contains("restore-keys:"));
        assert!(lane.contains(&format!(
            "{INSTALL}\n        if: steps.nextest-cache.outputs.cache-hit != 'true'\n"
        )));
        let activate = step(lane, "Activate pinned nextest");
        assert!(activate.contains("echo \"$HOME/.nextest-bin/bin\" >> \"$GITHUB_PATH\""));
        assert!(
            activate.contains("test \"$program\" = cargo-nextest && test \"$version\" = 0.9.152")
        );
        let save = step(lane, "Save nextest executable");
        assert!(save.contains("if: github.event_name == 'push' && github.ref == 'refs/heads/main' && steps.nextest-cache.outputs.cache-hit != 'true'\n"));
        assert!(save.contains("actions/cache/save@0057852bfaa89a56745cba8c7296529d2fc39830"));
        assert!(save.contains("path: ~/.nextest-bin/bin/cargo-nextest\n"));
        assert!(save.contains("key: ${{ steps.nextest-cache.outputs.cache-primary-key }}"));
    }
    assert!(!check.contains("cargo install"));
    assert!(!changes.contains("cargo install"));
    assert_eq!(ci.matches("cargo install cargo-nextest").count(), 2);
    assert_eq!(ci.matches("--version 0.9.152").count(), 2);
    assert_ambient_target_contract(ci);
    assert!(!ambient.contains("./xtask/gate.sh"));
    let suite = step(test, "Folded suite");
    assert!(
        script(suite).lines().any(|line| line == "exec ./xtask/gate.sh pr --part suite --receipt \"$RUNNER_TEMP/pr3-suite-receipt\" --lease-wait 5 --host-label \"$HOST_LABEL\""),
        "folded producer must replace the step shell with unchanged argv"
    );
    assert!(suite.contains("--lease-wait 5 --host-label \"$HOST_LABEL\""));
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
    // The runner.temp context names the host's directory; this is $RUNNER_TEMP
    // inside the job container.
    assert!(upload.contains("path: /__w/_temp/pr3-suite-receipt\n"));
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
    // cargo-deny runs in the job: a container action would start a pod of its
    // own on a Kubernetes-mode self-hosted runner, outside the job's hardening.
    let deny = step(check, "cargo deny");
    for line in [DENY_FETCH, DENY_VERIFY, DENY_RUN] {
        assert!(
            deny.lines().any(|l| l == line),
            "cargo deny step lost {line}"
        );
    }
    let fetch = deny.find(DENY_FETCH).unwrap();
    let verify = deny.find(DENY_VERIFY).unwrap();
    let run = deny.find(DENY_RUN).unwrap();
    assert!(
        fetch < verify && verify < run,
        "verify the archive before running it"
    );
    assert!(script(deny).starts_with("set -euo pipefail\n"));
    assert!(
        !ci.lines()
            .any(|l| l.contains("uses:") && l.contains("cargo-deny-action"))
    );
    assert!(!ci.contains("uses: docker://"));
    // Runners: one container for every job, routing only through the variable.
    for (name, job) in [("changes", changes), ("check", check), ("test", test)] {
        assert!(
            job.lines().any(|l| l == ROUTED),
            "{name} must use the fork-aware route"
        );
        assert!(
            job.contains(&format!("\n    container:\n{JOB_IMAGE}\n")),
            "{name} must run in the pinned job container"
        );
    }
    assert!(ambient.lines().any(|l| l == AMBIENT_ROUTED));
    assert!(ambient.contains(&format!("\n    container:\n{JOB_IMAGE}\n")));
    assert_eq!(ci.matches("    runs-on: ").count(), 4);
    assert_eq!(ci.matches("\n    container:\n").count(), 4);
    assert_eq!(ci.matches(ROUTED).count(), 3);
    // Untranslated in a job container: these name the host's paths.
    assert!(!ci.contains("${{ github.workspace }}"));
    assert!(!ci.contains("${{ runner.temp }}"));
    assert!(!ci.contains("sudo "));
    assert!(step(test, "Folded suite").lines().any(|l| l == HOST_LABEL));
    assert!(!CONTAINER_WORKSPACE.ends_with('/'));
    // Every job's checkout is followed by taking ownership of it, before any
    // other step can run git or the gate.
    for (name, job) in [
        ("changes", changes),
        ("check", check),
        ("test", test),
        ("ambient", ambient),
    ] {
        let checkout = job.find("persist-credentials: false\n").expect("checkout");
        let next = job[checkout..]
            .find("\n      - ")
            .map(|n| checkout + n + 1)
            .unwrap();
        let safe = job
            .find(OWN_WORKSPACE)
            .unwrap_or_else(|| panic!("{name} lacks {OWN_WORKSPACE}"));
        let between = &job[next..safe];
        assert!(
            between.lines().all(|l| l.trim_start().starts_with('#')),
            "{name}: taking ownership must directly follow checkout"
        );
    }
    assert_eq!(ci.matches(OWN_WORKSPACE).count(), 4);
    // Test-running steps are unprivileged; the user is set up after everything
    // root installs and before the first of them.
    for (name, lane, steps) in [
        (
            "test",
            test,
            &["Folded suite", "Verify diagnostic receipt"][..],
        ),
        ("ambient", ambient, &[][..]),
    ] {
        let prep = step(lane, "Hand the checkout to the suite user");
        assert!(prep.lines().any(|l| l == SUITE_USER), "{name}: suite user");
        assert!(
            prep.lines().any(|l| l == SUITE_OWNS),
            "{name}: suite ownership"
        );
        assert!(
            prep.lines().any(|l| l == SUITE_TEMP),
            "{name}: receipt directory"
        );
        let at = lane
            .find("      - name: Hand the checkout to the suite user")
            .unwrap();
        assert!(lane.find("      - name: Save nextest executable").unwrap() < at);
        assert!(
            at < lane.find(UNPRIVILEGED).unwrap(),
            "{name}: user before use"
        );
        for s in steps {
            assert!(
                step(lane, s).lines().any(|l| l == UNPRIVILEGED),
                "{name}: {s} must run unprivileged"
            );
        }
        assert!(lane.contains("      - run: apt-get update && apt-get install -y tmux tini\n"));
    }
    assert_eq!(test.matches(UNPRIVILEGED).count(), 2);
    for pass in [AMBIENT_PASSES[0], AMBIENT_PASSES[1], AMBIENT_DOC] {
        assert!(
            ambient.contains(&format!("{pass}\n{UNPRIVILEGED}\n")),
            "ambient: {pass} must run unprivileged"
        );
    }
    assert_eq!(ci.matches(UNPRIVILEGED).count(), 5);
    assert!(check.contains("test -s Cargo.lock"));
    assert!(check.contains("! grep -qE '^name = \"(terminfo|termwiz)\"$' Cargo.lock"));
    assert!(test.contains("timeout-minutes: 50"));
    assert!(ambient.contains("timeout-minutes: 50"));
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
        let changed = original.replacen("timeout-minutes: 50", "timeout-minutes: 30", 1);
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
        // Routing: dropping the fork check, or hard-coding either runner.
        (
            "(github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && ",
            "",
        ),
        (
            "vars.CI_SELF_HOSTED_RUNNER || 'ubuntu-latest' }}",
            "'ubuntu-latest' }}",
        ),
        (
            "    container:\n      image: docker.io/library/rust:1-trixie@",
            "    container:\n      image: docker.io/library/rust:1-bookworm@",
        ),
        (
            "path: /__w/_temp/pr3-suite-receipt",
            "path: /home/runner/work/_temp/pr3-suite-receipt",
        ),
        (
            "      - run: apt-get update",
            "      - run: sudo apt-get update",
        ),
        (
            "--host-label \"$HOST_LABEL\"",
            "--host-label github-hosted-linux",
        ),
        // cargo-deny: unverified archive, or a different release.
        (
            "9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f  $archive",
            "0000000000000000000000000000000000000000000000000000000000000000  $archive",
        ),
        ("releases/download/0.20.2/", "releases/download/0.20.1/"),
        ("--all-features check", "check"),
        // Ownership: only the top directory, or a git-only exception, is not enough.
        ("chown -R \"$(id -u)", "chown \"$(id -u)"),
        // The suite as root, or without a subreaper, or as another user.
        (
            "--init-groups -- env HOME=/home/ci tini -s -- bash",
            "--init-groups -- env HOME=/home/ci bash",
        ),
        ("--reuid=1001 --regid=1001", "--reuid=0 --regid=0"),
        ("apt-get install -y tmux tini", "apt-get install -y tmux"),
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
fn nextest_activation_checks_hits_and_misses_before_use() {
    for name in ["test", "ambient"] {
        let f = Fixture::new();
        for relative in ["home/.nextest-bin", "home/.nextest-bin/bin"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(f.0.join(relative))
                .unwrap();
        }
        f.executable(
            "home/.nextest-bin/bin/cargo-nextest",
            "#!/bin/sh\nprintf '%s\\n' \"$RUNNER_VERSION\"\nexit \"$RUNNER_STATUS\"\n",
        );
        let body = script(step(job(CI, name), "Activate pinned nextest"));
        let output = f.0.join("path");
        for (version, status, expected) in [
            (
                "cargo-nextest 0.9.152 (26ebeda79 2026-11-13)\nrelease: 0.9.152",
                "0",
                true,
            ),
            ("cargo-nextest 0.9.153", "0", false),
            ("cargo-nextest 0.9.1520", "0", false),
            ("other 0.9.152", "0", false),
            ("", "0", false),
            ("cargo-nextest 0.9.152", "1", false),
        ] {
            assert_eq!(
                f.run(
                    &body,
                    &[
                        ("GITHUB_PATH", output.to_str().unwrap()),
                        ("RUNNER_VERSION", version),
                        ("RUNNER_STATUS", status)
                    ]
                ),
                expected,
                "{name}: {version}/{status}"
            );
        }
        assert!(
            fs::read_to_string(output)
                .unwrap()
                .lines()
                .all(|line| line == f.0.join("home/.nextest-bin/bin").to_str().unwrap())
        );
    }
}

#[test]
fn nextest_cache_boundary_regressions_are_rejected_in_both_lanes() {
    for name in ["test", "ambient"] {
        let original = job(CI, name);
        for (old, new) in [
            ("--root \"$HOME/.nextest-bin\"", ""),
            (
                "if: steps.nextest-cache.outputs.cache-hit != 'true'",
                "if: false",
            ),
            (
                "path: ~/.nextest-bin/bin/cargo-nextest",
                "path: ~/.cargo/bin",
            ),
            (
                "key: nextest-${{ runner.os }}",
                "restore-keys: nextest-${{ runner.os }}",
            ),
            (
                "github.event_name == 'push' && github.ref == 'refs/heads/main' && steps.nextest-cache",
                "steps.nextest-cache",
            ),
            ("test \"$version\" = 0.9.152", "test true"),
            (
                "key: ${{ steps.nextest-cache.outputs.cache-primary-key }}",
                "key: unrelated",
            ),
        ] {
            let changed = original.replacen(old, new, 1);
            assert_ne!(original, changed, "{name}: {old}");
            let mutant = CI.replacen(original, &changed, 1);
            assert!(
                std::panic::catch_unwind(|| contract(&mutant)).is_err(),
                "{name}: {old}"
            );
        }
    }
}

#[test]
fn suite_failure_cannot_be_replaced_by_verifier_success() {
    let f = Fixture::new();
    f.executable("xtask/gate.sh", "#!/bin/sh\nexit \"$PRODUCER_STATUS\"\n");
    let suite = script(step(job(CI, "test"), "Folded suite"));
    for status in ["1", "2", "3", "75", "143"] {
        assert!(!f.run(
            &suite,
            &[("PRODUCER_STATUS", status), ("HOST_LABEL", "self-hosted")]
        ));
    }
    assert!(f.run(
        &suite,
        &[("PRODUCER_STATUS", "0"), ("HOST_LABEL", "self-hosted")]
    ));
    // An unset label fails the step rather than recording an empty one.
    assert!(!f.run(&suite, &[("PRODUCER_STATUS", "0")]));
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
    assert!(!f.run(
        &suite,
        &[("PRODUCER_STATUS", "1"), ("HOST_LABEL", "self-hosted")]
    ));
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
    let target = match raw.strip_prefix(&format!("{CONTAINER_WORKSPACE}/")) {
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
    // The lines the split occupies: the first pass, then the guard and the
    // second pass as one step, each pass with its unprivileged shell.
    let split = format!(
        "{}\n{UNPRIVILEGED}\n{}\n{}\n{UNPRIVILEGED}\n",
        AMBIENT_PASSES[0], GUARD, AMBIENT_PASSES[1]
    );
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
        CI.replacen(AMBIENT_TARGET, "/__w/r-orch/r-orch/target/wrong", 1),
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
