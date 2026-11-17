//! Synthetic historical evidence input, never an assertion that a gate ran.
//! Acceptance/rejection always comes from the real public xtask verifier.

#[path = "gate_witness.rs"]
mod witness;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use witness::fixture::{Capture, STEPS, Sandbox};

struct Evidence {
    sandbox: Sandbox,
    commit: String,
    judge: String,
}

// Independent Git blob identities use the public F4 raw UTF-8 path encoding.
// Control-character paths are exercised separately as explicit refusal cases.
impl Evidence {
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let files = [
            (".gitignore", "/target/\n/xtask/tracked-ignored.txt\n"),
            (
                "Cargo.toml",
                "[workspace]\nresolver = \"3\"\nmembers = [\"fixture\", \"xtask\"]\n",
            ),
            (
                "Cargo.lock",
                "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.0.0\"\n\n[[package]]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
            ),
            (
                "rust-toolchain.toml",
                "[toolchain]\nchannel = \"1.99.1\"\ncomponents = [\"rustfmt\", \"clippy\"]\nprofile = \"minimal\"\n",
            ),
            (
                ".cargo/config.toml",
                "[env]\nORCHESTRY_GLOBAL_CONFIG_PATH = { value = \"target/test-global/global.yml\", relative = true }\n",
            ),
            (
                "fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            (
                "fixture/src/lib.rs",
                "#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n",
            ),
            (
                "xtask/Cargo.toml",
                "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            (
                "xtask/src/main.rs",
                "fn main() {} // synthetic source, no gate implementation\n",
            ),
            (
                "xtask/gate.sh",
                "#!/bin/sh\n# Synthetic source only; never execute.\nexit 2\n",
            ),
            ("xtask/space name.txt", "space\n"),
            ("xtask/quote\"slash\\.txt", "quote and slash\n"),
            ("xtask/-leading.txt", "leading dash\n"),
            ("xtask/tracked-ignored.txt", "tracked even though ignored\n"),
        ];
        let mut blobs = BTreeMap::new();
        for (path, content) in files {
            sandbox.write(path, content);
            blobs.insert(path, sandbox.blob(content.as_bytes()));
        }
        let commit = sandbox.commit();
        let tree = sandbox.git(&["rev-parse", "HEAD^{tree}"]);
        let identity = |judge_only: bool| {
            let mut records = String::new();
            for (path, blob) in &blobs {
                if !judge_only
                    || path.starts_with("xtask/")
                    || [
                        "Cargo.toml",
                        "Cargo.lock",
                        "rust-toolchain.toml",
                        ".cargo/config.toml",
                    ]
                    .contains(path)
                {
                    records.push_str(&format!("{blob} {}\n", path));
                }
            }
            sandbox.blob(records.as_bytes())
        };
        let source = identity(false);
        let judge = identity(true);
        let values = BTreeMap::from([
            ("verdict", "PASSED (tier pr)"),
            ("commit", commit.as_str()),
            ("tree", tree.as_str()),
            ("dirty", "no"),
            ("moved", "no"),
            ("mode", "warm"),
            ("started", "2000-01-01T00:00:00Z"),
            ("finished", "2000-01-01T00:00:09Z"),
            ("tier", "pr"),
            ("part", "-"),
            ("run-dir", "<REPO>/target/gate-runs/synthetic-pr"),
            ("target", "reused"),
            ("lease", "exclusive"),
            ("foreign-build-activity", "no"),
            ("real-home-coverage", "complete"),
            ("judge-source-id", judge.as_str()),
            ("qualifying", "yes"),
            ("schema", "1"),
            ("head-start", commit.as_str()),
            ("head-end", commit.as_str()),
            ("tree-start", tree.as_str()),
            ("tree-end", tree.as_str()),
            ("dirty-start", "no"),
            ("dirty-end", "no"),
            ("source-start", source.as_str()),
            ("source-end", source.as_str()),
            ("entrypoint", "gate.sh"),
            ("legacy-warm", "no"),
            ("lease-default", "yes"),
            ("pinned-toolchain", "yes"),
            ("target-finished", "yes"),
            ("lease-path", "default"),
            ("lease-override", "no"),
            ("lease-held-through-teardown", "yes"),
            ("lease-lost", "no"),
            ("lease-holder-live", "yes"),
            ("lease-token-match", "yes"),
            ("lease-authority", "kernel-exclusive"),
            ("host-os", "linux"),
            ("host-arch", "x86_64"),
            ("host-label", "synthetic-lab"),
            ("runner", "cargo"),
            ("exit", "0"),
            ("total-wall-ms", "9000"),
        ]);
        let mut result = values
            .iter()
            .map(|(key, value)| format!("{key}: {value}\n"))
            .collect::<String>();
        // Keep historical table aliases for contradiction controls, and supply
        // the public F4 scalar representation independently.
        for step in STEPS {
            result.push_str(&format!(
                "step-{step}-status: pass\nstep-{step}-exit: 0\nstep-{step}-wall: 1.0\n"
            ));
        }
        result.push_str("\nstep\tstatus\texit\twall-ms\tlog\tdetail\n");
        for step in STEPS {
            result.push_str(&format!(
                "{step}\tpass\t0\t1000\tsteps/{step}.log\tsynthetic fixture\n"
            ));
            sandbox.write(format!("bundle/steps/{step}.log"), if step == "test" {
                "     Running unittests src/lib.rs (<REPO>/target/gate-warm/debug/deps/fixture-012345)\n\nrunning 1 test\ntest sample ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n   Doc-tests fixture\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n".to_owned()
            } else { format!("synthetic fixture: {step} passed\n") });
        }
        sandbox.write("bundle/steps/build.log", serde_json::json!({
            "reason": "compiler-artifact", "package_id": "path+file:///fixture#fixture@0.0.0",
            "profile": {"test": true}, "executable": "<REPO>/target/gate-warm/debug/deps/fixture-012345",
            "target": {"kind": ["lib"], "name": "fixture"}
        }).to_string() + "\n");
        sandbox.write("bundle/result.txt", result);
        sandbox.write(
            "bundle/inventory.tsv",
            "lib\tfixture\tfixture\tsample\tok\n",
        );
        sandbox.write("bundle/environment.txt", "removed ORCHESTRY_GLOBAL_CONFIG_PATH\nset CARGO_HOME\nset CARGO_TARGET_DIR\nset HOME\nset PATH\nset RUSTUP_HOME\nset RUSTUP_TOOLCHAIN\nset XTASK_GATE_WITNESS_DIR\nset XTASK_GATE_WITNESS_WRITER\n");
        let tools = BTreeMap::from([
            ("schema", "1"),
            ("object-format", "sha1"),
            ("toolchain", "1.99.1"),
            ("rustc", "rustc 1.99.1 (synthetic fixture)"),
            ("cargo", "cargo 1.99.1 (synthetic fixture)"),
            ("rustfmt", "rustfmt 1.8.0-stable (synthetic fixture)"),
            ("clippy", "clippy 0.1.99 (synthetic fixture)"),
            ("cargo-deny", "cargo-deny 0.18.3 (synthetic fixture)"),
            ("git", "git version 2.50.0 (synthetic fixture)"),
            ("cargo-nextest", "not-used"),
            ("lock-blob", blobs["Cargo.lock"].as_str()),
            ("cargo-config-blob", blobs[".cargo/config.toml"].as_str()),
            ("toolchain-file-blob", blobs["rust-toolchain.toml"].as_str()),
            ("source-id", source.as_str()),
            ("target-scope", "worktree"),
            ("target-owner", "<REPO>"),
            ("target-path", "<REPO>/target/gate-warm"),
        ]);
        sandbox.write(
            "bundle/tools.txt",
            tools
                .iter()
                .map(|(key, value)| format!("{key}: {value}\n"))
                .collect::<String>(),
        );
        let fixture = Self {
            sandbox,
            commit,
            judge,
        };
        fixture.rehash();
        fixture
    }

    fn verify(&self) -> Capture {
        self.verify_commit(&self.commit)
    }
    fn verify_commit(&self, commit: &str) -> Capture {
        self.sandbox.run(
            &mut self
                .sandbox
                .gate(&["evidence", "verify", "bundle", "--commit", commit]),
        )
    }
    fn positive(&self) {
        self.verify().code(0);
    }
    fn key(&self, name: &str, value: &str) {
        let path = self.sandbox.root.join("bundle/result.txt");
        let text = fs::read_to_string(&path).unwrap();
        let prefix = format!("{name}: ");
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with(&prefix))
                .count(),
            1,
            "fixture key"
        );
        fs::write(
            path,
            text.lines()
                .map(|line| {
                    if line.starts_with(&prefix) {
                        format!("{name}: {value}\n")
                    } else {
                        format!("{line}\n")
                    }
                })
                .collect::<String>(),
        )
        .unwrap();
    }
    fn step(&self, name: &str, column: usize, value: &str) {
        let suffix = match column {
            1 => "status",
            2 => "exit",
            _ => panic!("fixture step column"),
        };
        self.key(&format!("step-{name}-{suffix}"), value);
        let path = self.sandbox.root.join("bundle/result.txt");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            text.lines()
                .map(|line| {
                    if line.starts_with(&format!("{name}\t")) {
                        let mut fields: Vec<_> = line.split('\t').collect();
                        fields[column] = value;
                        fields.join("\t") + "\n"
                    } else {
                        line.to_owned() + "\n"
                    }
                })
                .collect::<String>(),
        )
        .unwrap();
    }
    fn rehash(&self) {
        let mut names = vec![
            "environment.txt".to_owned(),
            "inventory.tsv".to_owned(),
            "result.txt".to_owned(),
            "tools.txt".to_owned(),
        ];
        names.extend(STEPS.map(|step| format!("steps/{step}.log")));
        names.sort();
        let manifest = names
            .iter()
            .map(|name| {
                let bytes = fs::read(self.sandbox.root.join("bundle").join(name)).unwrap();
                format!("{}  {name}\n", self.sandbox.blob(&bytes))
            })
            .collect::<String>();
        self.sandbox.write("bundle/MANIFEST", manifest);
    }
    fn reject(&self, reason: &str) {
        self.rehash();
        let output = self.verify();
        output.code(1);
        output.contains(reason);
    }
    fn inherited(&self) {
        self.key("lease", "inherited");
        self.key("lease-authority", "live-exclusive-holder");
        self.rehash();
        self.positive();
    }
}

#[test]
fn complete_synthetic_bundle_verifies_for_its_independent_commit() {
    Evidence::new().positive();
}

#[test]
fn changed_log_without_rehash_is_rejected_as_digest_corruption() {
    let evidence = Evidence::new();
    evidence.positive();
    let log = evidence.sandbox.root.join("bundle/steps/test.log");
    let mut bytes = fs::read(&log).unwrap();
    bytes.push(b'x'); // TV41 changes exactly one byte, leaving MANIFEST stale.
    fs::write(log, bytes).unwrap();
    let output = evidence.verify();
    output.code(1);
    output.contains("digest");
}

#[test]
fn different_existing_expected_commit_is_rejected_as_commit_mismatch() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.sandbox.git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "--allow-empty",
        "--no-gpg-sign",
        "-m",
        "different expected commit",
    ]);
    let other = evidence.sandbox.git(&["rev-parse", "HEAD"]);
    assert_ne!(other, evidence.commit);
    evidence.rehash();
    let output = evidence.verify_commit(&other);
    output.code(1);
    output.contains("commit");
}

#[test]
fn explicit_dirty_end_cannot_be_masked_by_clean_aliases() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.key("dirty-end", "yes");
    evidence.reject("dirty");
}

#[test]
fn explicit_moved_end_cannot_be_masked_by_original_commit_alias() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.sandbox.git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "--allow-empty",
        "--no-gpg-sign",
        "-m",
        "moved end",
    ]);
    evidence.key("head-end", &evidence.sandbox.git(&["rev-parse", "HEAD"]));
    evidence.rehash();
    let output = evidence.verify();
    output.code(1);
    assert!(
        output.text().contains("head-end") || output.text().contains("moved"),
        "missing moved-head attribution: {}",
        output.text()
    );
}

#[test]
fn changed_end_tree_is_rejected_with_the_original_expected_commit() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.sandbox.write("new-source.txt", "different tree\n");
    evidence.sandbox.commit();
    evidence.key(
        "tree-end",
        &evidence.sandbox.git(&["rev-parse", "HEAD^{tree}"]),
    );
    evidence.reject("tree");
}

#[test]
fn partial_run_is_rejected_even_with_qualifying_yes() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.key("part", "suite");
    evidence.reject("part");
}

#[test]
fn env_limited_status_alone_prevents_qualification() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.step("advisories", 1, "ENV-LIMITED");
    evidence.reject("advisories");
}

#[test]
fn failed_status_alone_prevents_qualification() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.step("test", 1, "FAIL");
    evidence.rehash();
    let output = evidence.verify();
    output.code(1);
    assert!(output.text().lines().any(|line| line.contains("test") && (line.contains("status") || line.contains("FAIL"))), "missing test-status attribution: {}", output.text());
}

#[test]
fn nonzero_step_exit_alone_prevents_qualification() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.step("test", 2, "101");
    evidence.rehash();
    let output = evidence.verify();
    output.code(1);
    assert!(
        output
            .text()
            .lines()
            .any(|line| line.contains("test") && (line.contains("exit") || line.contains("101"))),
        "missing test-exit attribution: {}",
        output.text()
    );
}

#[test]
fn unusable_lease_provenance_cannot_be_overridden_by_qualifying_yes() {
    for (key, value) in [
        ("lease", "none: fixture"),
        ("lease", "shared"),
        ("lease", "lost"),
        ("lease-override", "yes"),
        ("lease-lost", "yes"),
        ("lease-token-match", "no"),
        ("lease-held-through-teardown", "no"),
    ] {
        let evidence = Evidence::new();
        evidence.positive();
        evidence.key(key, value);
        evidence.reject("lease");
    }
}

#[test]
fn live_inherited_exclusive_provenance_verifies() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.inherited();
}

#[test]
fn inherited_dead_holder_is_rejected_after_valid_inherited_positive() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.inherited();
    evidence.key("lease-holder-live", "no");
    evidence.reject("lease");
}

#[test]
fn inherited_diagnostic_only_authority_is_rejected_after_valid_inherited_positive() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.inherited();
    evidence.key("lease-authority", "diagnostic-only");
    evidence.reject("lease");
}

#[test]
fn wrong_and_absent_judge_are_rejected_with_correct_commit_and_rehashed_bytes() {
    for value in ["wrong", "absent"] {
        let evidence = Evidence::new();
        evidence.positive();
        let wrong = evidence.sandbox.blob(b"different judge source");
        evidence.key(
            "judge-source-id",
            if value == "wrong" { &wrong } else { "absent" },
        );
        evidence.reject("judge");
        evidence.key("judge-source-id", &evidence.judge);
        evidence.rehash();
        evidence.positive();
    }
}

#[test]
fn malformed_duplicate_and_traversing_manifest_paths_are_rejected() {
    for variant in [
        "duplicate",
        "parent",
        "absolute",
        "dot-parent",
        "double-slash",
        "dot",
    ] {
        let evidence = Evidence::new();
        evidence.positive();
        let (original, invalid) = match variant {
            "duplicate" => ("environment.txt", "environment.txt".to_owned()),
            "parent" => ("steps/test.log", "../outside-log".to_owned()),
            "absolute" => (
                "steps/test.log",
                evidence
                    .sandbox
                    .temporary
                    .join("absolute-log")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
            "dot-parent" => ("result.txt", "steps/../result.txt".to_owned()),
            "double-slash" => ("steps/test.log", "steps//test.log".to_owned()),
            "dot" => ("result.txt", "./result.txt".to_owned()),
            _ => unreachable!(),
        };
        let bytes = fs::read(evidence.sandbox.root.join("bundle").join(original)).unwrap();
        // Every spelling resolves to existing bytes with the matching digest.
        // Absolute/traversal plants stay inside this owned disposable root.
        if variant == "parent" {
            evidence.sandbox.write("outside-log", &bytes);
        }
        if variant == "absolute" {
            fs::write(&invalid, &bytes).unwrap();
        }
        assert_eq!(
            fs::read(evidence.sandbox.root.join("bundle").join(&invalid)).unwrap(),
            bytes
        );
        let manifest = evidence.sandbox.root.join("bundle/MANIFEST");
        let text = fs::read_to_string(&manifest).unwrap();
        let old = format!("{}  {original}\n", evidence.sandbox.blob(&bytes));
        assert_eq!(text.matches(&old).count(), 1);
        let changed = format!("{}  {invalid}\n", evidence.sandbox.blob(&bytes));
        let text = if variant == "duplicate" {
            text + &changed
        } else {
            text.replace(&old, &changed)
        };
        let mut lines: Vec<_> = text.lines().collect();
        lines.sort_by_key(|line| line.split_once("  ").unwrap().1);
        fs::write(manifest, lines.join("\n") + "\n").unwrap();
        let output = evidence.verify();
        output.code(1);
        let diagnostic = output.text();
        assert!(
            diagnostic.lines().any(|line| {
                if variant == "duplicate" {
                    line.contains("duplicate")
                        && (line.contains("manifest") || line.contains("MANIFEST"))
                } else {
                    (line.contains("path")
                        || line.contains("canonical")
                        || line.contains("travers"))
                        && (line.contains("manifest")
                            || line.contains("MANIFEST")
                            || line.contains(&invalid))
                }
            }),
            "missing manifest path attribution for {variant}: {diagnostic}"
        );
        assert!(
            !diagnostic.contains("No such file"),
            "invalid missing-file plant: {diagnostic}"
        );
    }
}

#[test]
fn bundle_root_steps_and_file_symlinks_are_rejected_without_writing_canaries() {
    for component in ["root", "steps", "file"] {
        let evidence = Evidence::new();
        evidence.positive();
        let root = &evidence.sandbox.root;
        if component == "root" {
            fs::rename(root.join("bundle"), root.join("saved-bundle")).unwrap();
            symlink("saved-bundle", root.join("bundle")).unwrap();
        } else if component == "steps" {
            fs::rename(root.join("bundle/steps"), root.join("saved-steps")).unwrap();
            symlink("../saved-steps", root.join("bundle/steps")).unwrap();
        } else {
            fs::rename(
                root.join("bundle/steps/test.log"),
                root.join("outside-canary"),
            )
            .unwrap();
            symlink("../../outside-canary", root.join("bundle/steps/test.log")).unwrap();
        }
        evidence.verify().code(1);
    }
}

#[test]
fn missing_extra_and_oversized_structured_files_are_rejected() {
    for mutation in ["missing", "extra", "oversize", "nul", "duplicate-key"] {
        let evidence = Evidence::new();
        evidence.positive();
        let (file, reasons): (&str, &[&str]) = match mutation {
            "missing" => {
                fs::remove_file(evidence.sandbox.root.join("bundle/steps/test.log")).unwrap();
                ("test.log", &["missing", "No such file"])
            }
            "extra" => {
                evidence.sandbox.write("bundle/extra.txt", "unmanifested\n");
                ("extra.txt", &["extra", "unmanifested", "unexpected"])
            }
            "oversize" => {
                let path = evidence.sandbox.root.join("bundle/tools.txt");
                let original = fs::read_to_string(&path).unwrap();
                let padded = original.replace(
                    "git: git version ",
                    &format!(
                        "git: git version {}",
                        "x".repeat(1024 * 1024 - original.len())
                    ),
                );
                assert_eq!(padded.len(), 1024 * 1024);
                fs::write(&path, &padded).unwrap();
                evidence.rehash();
                evidence.positive(); // Grammar-valid exactly-at-bound control.
                let oversized = padded.replacen("git: git version ", "git: git version x", 1);
                assert_eq!(oversized.len(), 1024 * 1024 + 1);
                fs::write(path, oversized).unwrap();
                evidence.rehash();
                ("tools.txt", &["bound", "large", "size", "limit", "budget"])
            }
            "nul" => {
                // Keep every required key and value predicate intact: a free
                // observed Git version value isolates structured-text NUL
                // validation from missing/unknown environment-name checks.
                let path = evidence.sandbox.root.join("bundle/tools.txt");
                let text = fs::read_to_string(&path).unwrap();
                assert!(text.contains("git: git version "));
                fs::write(
                    path,
                    text.replacen("git: git version ", "git: git version \0", 1),
                )
                .unwrap();
                evidence.rehash();
                ("tools.txt", &["NUL", "nul", "null", "zero byte"])
            }
            "duplicate-key" => {
                let path = evidence.sandbox.root.join("bundle/result.txt");
                let text = fs::read_to_string(&path).unwrap();
                fs::write(path, format!("schema: 1\n{text}")).unwrap();
                evidence.rehash();
                ("schema", &["duplicate"])
            }
            _ => unreachable!(),
        };
        let output = evidence.verify();
        output.code(1);
        assert!(
            output
                .text()
                .lines()
                .any(|line| line.contains(file) && reasons.iter().any(|r| line.contains(r))),
            "missing {mutation}/{file} attribution: {}",
            output.text()
        );
    }
}

#[test]
fn failed_empty_unsorted_and_duplicate_inventory_do_not_qualify() {
    for rows in [
        "",
        "lib\tfixture\tfixture\tsample\tfailed\n",
        "lib\tfixture\tfixture\tz\tok\nlib\tfixture\tfixture\ta\tok\n",
        "lib\tfixture\tfixture\tsample\tok\nlib\tfixture\tfixture\tsample\tok\n",
    ] {
        let evidence = Evidence::new();
        evidence.positive();
        evidence.sandbox.write("bundle/inventory.tsv", rows);
        evidence.rehash();
        evidence.verify().code(1);
    }
}

#[test]
fn judge_control_character_paths_are_explicitly_refused() {
    for name in ["xtask/tab\tname.txt", "xtask/new\nline.txt"] {
        let evidence = Evidence::new();
        evidence.positive();
        evidence.sandbox.write(name, "unsupported judge path\n");
        let commit = evidence.sandbox.commit();
        evidence.key("commit", &commit);
        evidence.key("head-end", &commit);
        evidence.rehash();
        let output = evidence.verify_commit(&commit);
        output.code(1);
        output.contains("judge source path unsupported");
    }
}

#[test]
fn kept_target_cannot_qualify_even_when_aliases_claim_finished() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.key("target", "kept (direct child was not reaped)");
    // Deliberately retain qualifying=yes and target-finished=yes. C8 requires
    // the explicit kept outcome to override contradictory success aliases.
    evidence.reject("target");
}

#[test]
fn unfinished_target_cannot_qualify_even_when_qualifying_yes() {
    let evidence = Evidence::new();
    evidence.positive();
    evidence.key("target-finished", "no");
    evidence.reject("target-finished");
}

// Partial diagnostics deliberately reuse the independent historical synthetic
// fixture, not a claim that this source/stamped judge executed a native gate.
impl Evidence {
    fn partial_receipt() -> Self {
        let e = Self::new();
        e.key("part", "suite");
        e.key("verdict", "PASSED (part suite)");
        e.key("qualifying", "no: partial run");
        let p = e.sandbox.root.join("bundle/result.txt");
        let original = fs::read_to_string(&p).unwrap();
        fs::write(p, format!("receipt: diagnostic\n{original}")).unwrap();
        e.rehash();
        e
    }
    fn verify_receipt(&self, commit: &str) -> Capture {
        self.sandbox.run(&mut self.sandbox.gate(&[
            "evidence",
            "verify-receipt",
            "bundle",
            "--commit",
            commit,
        ]))
    }
}

#[test]
fn partial_receipt_integrity_is_diagnostic_and_strict_verifier_stays_strict() {
    let e = Evidence::partial_receipt();
    let out = e.verify_receipt(&e.commit);
    out.code(0);
    out.contains("completeness=complete; outcome=passed");
    out.contains("qualifying=no; acceptance=not-granted");
    // Missing bootstrap/publication is honestly reported by this historical
    // fixture, even though its inventory and executed steps are consistent.
    out.contains("safety=failed-or-unestablished");
    e.verify().code(1);
    e.key("qualifying", "yes");
    e.rehash();
    e.verify().code(1); // part still refuses, even if qualification is forged.
    e.verify_receipt(&e.commit).code(1);
}

#[test]
fn partial_receipt_rejects_identity_tamper_missing_and_unsafe_artifacts() {
    let e = Evidence::partial_receipt();
    e.verify_receipt(&"f".repeat(40)).code(1);
    for (field, bad, original) in [
        ("judge-source-id", "bad-id", e.judge.as_str()),
        (
            "tree-start",
            "bad-tree",
            e.sandbox.git(&["rev-parse", "HEAD^{tree}"]).as_str(),
        ),
    ] {
        e.key(field, bad);
        e.rehash();
        e.verify_receipt(&e.commit).code(1);
        e.key(field, original);
        e.rehash();
        e.verify_receipt(&e.commit).code(0);
    }
    let p = e.sandbox.root.join("bundle/steps/test.log");
    let original = fs::read(&p).unwrap();
    fs::write(&p, b"tampered\n").unwrap();
    e.verify_receipt(&e.commit).code(1);
    fs::write(&p, &original).unwrap();
    fs::remove_file(&p).unwrap();
    e.verify_receipt(&e.commit).code(1);
    let outside = e.sandbox.temporary.join("outside-log");
    fs::write(&outside, &original).unwrap();
    symlink(&outside, &p).unwrap();
    e.verify_receipt(&e.commit).code(1);
    fs::remove_file(&p).unwrap();
    fs::write(&p, original).unwrap();
    e.verify_receipt(&e.commit).code(0);
    let inventory = e.sandbox.root.join("bundle/inventory.tsv");
    let original_inventory = fs::read(&inventory).unwrap();
    fs::remove_file(&inventory).unwrap();
    e.verify_receipt(&e.commit).code(1);
    fs::write(inventory, original_inventory).unwrap();
    e.verify_receipt(&e.commit).code(0);
    // Rehashed mismatching inventory cannot borrow valid log integrity.
    e.sandbox.write(
        "bundle/inventory.tsv",
        "lib\tfixture\tfixture\tforged\tok\n",
    );
    e.rehash();
    e.verify_receipt(&e.commit).code(1);
}

#[test]
fn failed_and_interrupted_receipts_do_not_become_passing_evidence() {
    let e = Evidence::partial_receipt();
    e.key("verdict", "FAILED (part suite)");
    e.step("test", 1, "FAIL");
    e.step("test", 2, "101");
    let p = e.sandbox.root.join("bundle/steps/test.log");
    let log = fs::read_to_string(&p)
        .unwrap()
        .replace("test sample ... ok", "test sample ... FAILED")
        .replace(
            "test result: ok. 1 passed; 0 failed",
            "test result: FAILED. 0 passed; 1 failed",
        );
    fs::write(p, log).unwrap();
    e.sandbox.write(
        "bundle/inventory.tsv",
        "lib\tfixture\tfixture\tsample\tfailed\n",
    );
    e.rehash();
    let out = e.verify_receipt(&e.commit);
    out.code(0);
    out.contains("completeness=complete; outcome=failed");
    out.contains("acceptance=not-granted");
    e.verify().code(1);
    e.key("verdict", "INTERRUPTED (part suite)");
    e.step("test", 2, "unknown");
    e.sandbox.write("bundle/inventory.tsv", "");
    e.rehash();
    let out = e.verify_receipt(&e.commit);
    out.code(0);
    out.contains("completeness=incomplete; outcome=interrupted");
    e.verify().code(1);
    e.key("verdict", "PASSED (part suite)");
    e.rehash();
    e.verify_receipt(&e.commit).code(1);
}
