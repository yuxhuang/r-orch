//! PR2 miniature controls: real selection and native Cargo; no product workload.
#[path = "gate_witness.rs"]
mod witness;
use std::fs;
use std::path::Path;
use witness::fixture::{Sandbox, field, row};

fn miniature() -> Sandbox {
    let s = Sandbox::new();
    s.mini("", false);
    fs::remove_dir_all(s.root.join("fixture")).unwrap();
    s.write("Cargo.toml", "[workspace]\nresolver=\"3\"\nmembers=[\"crates/orch-cli\",\"crates/orch-infra\",\"crates/unselected\",\"xtask\"]\n");
    for name in ["orch-cli", "orch-infra", "unselected"] {
        s.write(
            format!("crates/{name}/Cargo.toml"),
            format!("[package]\nname=\"{name}\"\nversion=\"0.0.0\"\nedition=\"2024\"\n"),
        );
    }
    s.write("crates/orch-cli/Cargo.toml", "[package]\nname=\"orch-cli\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[dependencies]\norch-infra={path=\"../orch-infra\"}\n");
    s.write(
        "crates/orch-cli/src/main.rs",
        "fn main() { println!(\"first\"); }\n",
    );
    s.write("crates/orch-infra/src/lib.rs", "pub fn marker() {}\n");
    s.write(
        "crates/unselected/src/lib.rs",
        "#[test] fn must_not_run() { panic!(\"unselected ran\"); }\n",
    );
    s.write("xtask/gate.sh", include_str!("../gate.sh"));
    s.write(
        "xtask/Cargo.toml",
        "[package]\nname=\"xtask\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    );
    s.write(
        "xtask/src/main.rs",
        "fn main() { assert_eq!(std::env::args().nth(1).as_deref(), Some(\"crate-graph\")); }\n",
    );
    s.write(
        "xtask/tests/gate_witness.rs",
        format!("{}\n#[test] fn witness_target_extra_a() {{}}\n#[test] fn witness_target_extra_b() {{}}\n", include_str!("gate_witness.rs")
            .split("// FIXTURE SUPPORT:")
            .next()
            .unwrap()),
    );
    s.write("crates/orch-infra/tests/path_exec.rs", r#"#[test]
fn current_cli() {
    let target = std::env::var_os("CARGO_TARGET_DIR").unwrap();
    let out = std::process::Command::new(std::path::Path::new(&target).join("debug/orch-cli")).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), include_str!("../expected.txt").trim());
}
"#);
    s.write("crates/orch-infra/expected.txt", "first\n");
    s.run(s.command("cargo").args(["generate-lockfile", "--offline"]))
        .code(0);
    // Native rustfmt is part of fast; make these synthetic sources canonical.
    s.run(s.command("cargo").args(["fmt", "--all"])).code(0);
    s.commit();
    s
}
fn assert_private_tree(path: &Path) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        if meta.is_dir() { 0o700 } else { 0o600 },
        "{}",
        path.display()
    );
    if meta.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            assert_private_tree(&entry.unwrap().path());
        }
    } else {
        assert!(meta.is_file());
        assert_eq!(meta.nlink(), 1);
    }
}
fn fast(s: &Sandbox, base: &str) -> witness::fixture::Capture {
    s.run(&mut s.gate(&["fast", "--base", base, "--lease-wait", "0"]))
}
#[test]
fn fast_plan_is_shared_and_runs_no_tools_tv72() {
    let s = Sandbox::new();
    let marker = s.temporary.join("cargo-called");
    s.script(
        &s.temporary.join("bin/cargo"),
        &format!("#!/bin/sh\ntouch '{}'\nexit 99\n", marker.display()),
    );
    let out = s.run(&mut s.gate(&["fast", "--plan"]));
    out.code(0);
    for text in [
        "shared",
        "iteration",
        "never merge evidence",
        "fmt",
        "clippy",
        "crate-graph",
        "target/gate-warm",
    ] {
        out.contains(text);
    }
    assert!(!out.text().contains("advisories"));
    assert!(!marker.exists());
    assert!(!s.root.join("target").exists());
    for args in [
        vec!["fast", "--warm"],
        vec!["fast", "--part", "suite"],
        vec!["fast", "--part", "static", "--plan"],
    ] {
        s.run(&mut s.gate(&args)).code(2);
    }
}
#[test]
fn selected_fast_builds_current_cli_on_empty_and_changed_warm_target_d40() {
    let s = miniature();
    for marker in ["second", "third"] {
        s.write(
            "crates/orch-cli/src/main.rs",
            format!("fn main() {{\n    println!(\"{marker}\");\n}}\n"),
        );
        // Include text is an unknown input and would rightly select all: amend
        // expected as part of the fixture base, then change only CLI Rust source.
        s.write("crates/orch-infra/expected.txt", format!("{marker}\n"));
        s.git(&[
            "add",
            "--",
            "crates/orch-cli/src/main.rs",
            "crates/orch-infra/expected.txt",
        ]);
        s.git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "test: next marker",
        ]);
        let expected_base = s.git(&["rev-parse", "HEAD"]);
        assert!(s.git(&["ls-files", "target"]).is_empty());
        s.write(
            "crates/orch-cli/src/main.rs",
            format!("fn main() {{\n    println!(\"{marker}\"); // changed source\n}}\n"),
        );
        let out = fast(&s, &expected_base);
        out.code(0);
        let record = s.result();
        assert_eq!(field(&record, "lease"), "shared");
        assert!(field(&record, "qualifying").contains("never merge evidence"));
        assert_eq!(row(&record, "test")[1], "pass", "{record}");
        out.contains("coupled");
        out.contains("orch-infra");
        let run = Path::new(field(&record, "run-dir"));
        let inventory = fs::read_to_string(run.join("inventory.tsv")).unwrap();
        assert!(inventory.contains("current_cli\tok"), "{inventory}");
        for name in [
            "gate_environment_witness",
            "witness_target_extra_a",
            "witness_target_extra_b",
        ] {
            assert_eq!(
                inventory
                    .lines()
                    .filter(|l| l.ends_with(&format!("\t{name}\tok")))
                    .count(),
                1,
                "{inventory}"
            );
        }
        assert!(!inventory.contains("must_not_run"));
        assert!(!record.contains("step-advisories-status"));
        assert_eq!(row(&record, "home-hygiene")[1], "pass");
        assert_eq!(row(&record, "no-real-cli")[1], "pass");
        assert_eq!(row(&record, "real-home")[1], "pass");
    }
}
#[test]
fn docs_only_fast_builds_and_lints_without_running_tests() {
    let s = miniature();
    let base = s.git(&["rev-parse", "HEAD"]);
    s.write("README.md", "documentation only\n");
    let out = fast(&s, &base);
    out.code(0);
    out.contains("docs-only");
    let record = s.result();
    for name in ["build", "fmt", "clippy", "crate-graph"] {
        assert_eq!(row(&record, name)[1], "pass", "{record}");
    }
    for name in [
        "test",
        "advisories",
        "home-hygiene",
        "no-real-cli",
        "real-home",
    ] {
        assert!(!record.contains(&format!("step-{name}-status")), "{record}");
    }
    let run = Path::new(field(&record, "run-dir"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(run.join("steps"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_private_tree(run);
    assert_private_tree(&s.root.join("target/.gate-state"));
    assert_private_tree(&s.root.join("target/gate-result.txt"));
    assert!(fs::read(run.join("inventory.tsv")).unwrap().is_empty());
    assert!(field(&record, "qualifying").contains("never merge evidence"));
}

#[test]
fn selected_fast_retains_home_shim_and_failure_negatives() {
    for (case, body, failed_step) in [
        (
            "home",
            "#[test] fn planted() { std::fs::write(std::path::Path::new(&std::env::var_os(\"HOME\").unwrap()).join(\"planted\"), b\"bad\").unwrap(); }",
            "home-hygiene",
        ),
        (
            "cli",
            "#[test] fn planted() { assert_eq!(std::process::Command::new(\"claude\").arg(\"--private-fixture\").status().unwrap().code(), Some(127)); }",
            "no-real-cli",
        ),
        (
            "failure",
            "#[test] fn planted() { panic!(\"intentional selected failure\"); }",
            "test",
        ),
    ] {
        let s = miniature();
        let base = s.git(&["rev-parse", "HEAD"]);
        s.write("crates/orch-infra/src/lib.rs", body);
        s.run(s.command("cargo").args(["fmt", "--all"])).code(0);
        let out = fast(&s, &base);
        out.code(1);
        let record = s.result();
        assert_eq!(row(&record, failed_step)[1], "FAIL", "{case}: {record}");
        assert_eq!(row(&record, "environment-witness")[1], "pass", "{record}");
        if case == "failure" {
            assert_eq!(row(&record, "test")[2], "101");
        }
        assert!(
            Path::new(field(&record, "run-dir"))
                .join("steps/test.log")
                .is_file()
        );
    }
}
#[test]
fn selected_fast_zero_tests_and_missing_witness_never_pass() {
    for case in ["zero", "missing-witness"] {
        let s = miniature();
        if case == "zero" {
            fs::remove_file(s.root.join("crates/orch-infra/tests/path_exec.rs")).unwrap();
            s.commit();
        } else {
            fs::remove_file(s.root.join("xtask/tests/gate_witness.rs")).unwrap();
            s.commit();
        }
        let base = s.git(&["rev-parse", "HEAD"]);
        s.write(
            "crates/orch-cli/src/main.rs",
            "fn main() {\n    println!(\"first\"); // selected\n}\n",
        );
        let out = fast(&s, &base);
        out.code(1);
        let record = s.result();
        if case == "zero" {
            assert_eq!(row(&record, "environment-witness")[1], "pass");
            assert_eq!(row(&record, "test")[1], "FAIL");
            out.contains("no test executed");
        } else {
            assert_eq!(row(&record, "environment-witness")[1], "FAIL");
            assert!(!record.contains("step-test-status"));
        }
    }
}
#[test]
fn failed_current_build_never_executes_a_planted_stale_cli_d40() {
    let s = miniature();
    let base = s.git(&["rev-parse", "HEAD"]);
    s.write(
        "crates/orch-cli/src/main.rs",
        "this is an intentional compiler failure\n",
    );
    let marker = s.temporary.join("stale-cli-executed");
    // A gate reset must not trust this pre-existing executable.
    fs::create_dir_all(s.root.join("target/gate-warm/debug")).unwrap();
    s.script(
        &s.root.join("target/gate-warm/debug/orch-cli"),
        &format!("#!/bin/sh\ntouch '{}'\necho first\n", marker.display()),
    );
    fast(&s, &base).code(1);
    let record = s.result();
    assert_eq!(row(&record, "build")[1], "FAIL");
    assert_eq!(row(&record, "test")[1], "FAIL");
    assert!(!marker.exists());
    assert!(
        fs::read_to_string(Path::new(field(&record, "run-dir")).join("inventory.tsv"))
            .unwrap()
            .lines()
            .all(|l| !l.contains("current_cli"))
    );
}

#[test]
fn fast_export_remains_nonqualifying_and_verifier_rejects_it() {
    let s = miniature();
    let base = s.git(&["rev-parse", "HEAD"]);
    s.write("README.md", "docs-only\n");
    let destination = s.temporary.join("fast-export");
    let out = s.run(&mut s.gate(&[
        "fast",
        "--base",
        &base,
        "--lease-wait",
        "0",
        "--evidence",
        destination.to_str().unwrap(),
    ]));
    out.code(3);
    out.contains("never merge evidence");
    use std::os::unix::fs::PermissionsExt;
    for path in [&destination, &destination.join("steps")] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_private_tree(&destination);
    s.run(&mut s.gate(&[
        "evidence",
        "verify",
        destination.to_str().unwrap(),
        "--commit",
        &base,
    ]))
    .code(1);
}

// A real metadata result is saved first; only then does the ordinary editor save
// happen. Git's changed-path queries are already complete at this boundary.
fn metadata_boundary_save(case: &str) {
    let s = miniature();
    let base = s.git(&["rev-parse", "HEAD"]);
    if case == "docs" {
        s.write("README.md", "documentation only\n");
    } else {
        s.write(
            "crates/orch-cli/src/main.rs",
            "fn main() {\n    println!(\"first\"); // selected\n}\n",
        );
    }
    let real = s.cargo_forwarder();
    let changed = s.root.join("crates/unselected/src/lib.rs");
    let saved = s.temporary.join("metadata-save");
    let trace = s.temporary.join("metadata-calls.jsonl");
    let armed = s.temporary.join("selection-paths-complete");
    // Only this exact selector Git command arms the save, after its real result
    // is returned. Earlier admission/source inventories cannot arm it.
    s.script(&s.temporary.join("bin/git"), &format!(r#"#!/usr/bin/python3 -B
import os, pathlib, subprocess, sys
args=sys.argv[1:]
result=subprocess.run(['/usr/bin/git']+args, stdout=subprocess.PIPE)
if args[-4:] == ['ls-files','--others','--exclude-standard','-z'] and '--no-optional-locks' in args and result.returncode == 0:
    with os.fdopen(os.open({armed:?}, os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW, 0o600), 'w') as f: f.write('selection paths complete\n')
sys.stdout.buffer.write(result.stdout)
sys.exit(result.returncode)
"#, armed=armed.to_str().unwrap()));
    s.script(&s.temporary.join("bin/cargo"), &format!(r#"#!/usr/bin/python3 -B
import json, os, pathlib, subprocess, sys
if sys.argv[1:2] == ['metadata']:
    exact=sys.argv[1:] == ['metadata','--no-deps','--format-version','1','--offline']
    armed=pathlib.Path({armed:?}).is_file()
    result=subprocess.run([{real:?}]+sys.argv[1:], stdout=subprocess.PIPE)
    trigger=exact and armed and result.returncode == 0 and not pathlib.Path({saved:?}).exists()
    if trigger:
        pathlib.Path({changed:?}).write_text('#[test]\nfn introduced_failure() {{\n    panic!("metadata save must not pass narrow selection");\n}}\n')
        with os.fdopen(os.open({saved:?}, os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW, 0o600), 'w') as f: f.write('saved at explicit selection metadata boundary\n')
    with os.fdopen(os.open({trace:?}, os.O_WRONLY|os.O_CREAT|os.O_APPEND|os.O_NOFOLLOW, 0o600), 'a') as f: f.write(json.dumps(dict(argv=sys.argv[1:],armed=armed,trigger=trigger,status=result.returncode))+'\n')
    sys.stdout.buffer.write(result.stdout)
    sys.exit(result.returncode)
os.execv({real:?}, [{real:?}] + sys.argv[1:])
"#, real=real.to_str().unwrap(), changed=changed.to_str().unwrap(), saved=saved.to_str().unwrap(), trace=trace.to_str().unwrap(), armed=armed.to_str().unwrap()));
    let out = fast(&s, &base);
    assert!(saved.is_file(), "metadata save boundary was not reached");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(&s.temporary).unwrap().mode() & 0o777, 0o700);
    for path in [&trace, &saved, &armed] {
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
    }
    let calls = fs::read_to_string(&trace).unwrap();
    let calls: Vec<serde_json::Value> = calls
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(calls.iter().filter(|c| c["trigger"] == true).count(), 1);
    assert!(
        calls
            .iter()
            .any(|c| c["trigger"] == true && c["armed"] == true)
    );
    println!(
        "METADATA_BOUNDARY case={case} calls={calls:?} actual-fast-exit={:?}\n{}",
        out.status.code(),
        out.text()
    );
    out.code(1);
    out.contains("source changed during fast selection");
    let record = s.result();
    assert!(!record.contains("PASSED"), "{record}");
    assert!(
        !record.contains("publication: record published"),
        "{record}"
    );
    assert!(!record.contains("step-build-status"), "{record}");
    assert!(
        fs::read_to_string(&changed)
            .unwrap()
            .contains("introduced_failure")
    );
    assert_ne!(field(&record, "judge-source-id"), "absent");
    assert!(
        !record.contains("source/head changed"),
        "judge must stay stable: {record}"
    );
    let independent = s.run(
        s.command("cargo")
            .args(["test", "-p", "unselected", "--lib"]),
    );
    independent.code(101);
    independent.contains("introduced_failure ... FAILED");
    println!(
        "INDEPENDENT_OMITTED_PACKAGE case={case} actual-exit={:?}\n{}",
        independent.status.code(),
        independent.text()
    );
    s.run(&mut s.gate(&[
        "lease",
        "exec",
        "--purpose",
        "selection-mismatch-close-check",
        "--lease-wait",
        "0",
        "--",
        "/usr/bin/true",
    ]))
    .code(0);
}

#[test]
fn metadata_boundary_save_cannot_pass_docs_only() {
    metadata_boundary_save("docs");
}

#[test]
fn metadata_boundary_save_cannot_omit_another_package() {
    metadata_boundary_save("package");
}
#[test]
fn fast_linked_warm_target_refuses_without_touching_outside_canary() {
    let s = miniature();
    let base = s.git(&["rev-parse", "HEAD"]);
    s.write("README.md", "docs-only\n");
    let outside = s.temporary.join("outside-warm");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("canary"), "unchanged").unwrap();
    fs::create_dir(s.root.join("target")).unwrap();
    std::os::unix::fs::symlink(&outside, s.root.join("target/gate-warm")).unwrap();
    fast(&s, &base).code(1);
    assert_eq!(
        fs::read_to_string(outside.join("canary")).unwrap(),
        "unchanged"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert!(!s.result().contains("step-build-status: pass"));
}
