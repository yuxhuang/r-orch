//! Miniature suites drive the built gate; no host gate or AI CLI is used.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;
use witness::fixture::{AGENTS, SESSION, Sandbox, backdate, field, row, rust_literal};

const PASS: &str = "#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n";
const FAIL: &str = "#[test]\nfn planted_failure() { assert_eq!(2 + 2, 5); }\n";

fn passing() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.mini(PASS, true);
    let output = sandbox.suite();
    output.code(0);
    let record = sandbox.result();
    for step in ["build", "test", "home-hygiene", "no-real-cli", "real-home"] {
        assert_eq!(row(&record, step)[1], "pass", "{record}");
    }
    sandbox
}

fn step_status(sandbox: &Sandbox, name: &str, status: &str) {
    let record = sandbox.result();
    assert_eq!(row(&record, name)[1], status, "{record}");
}

#[test]
fn passing_suite_evaluates_hygiene_and_cli_and_keeps_named_inventory() {
    let sandbox = passing();
    let record = sandbox.result();
    let run_dir = std::path::Path::new(field(&record, "run-dir"));
    assert!(run_dir.starts_with(sandbox.root.join("target/gate-runs")));
    let inventory = fs::read_to_string(run_dir.join("inventory.tsv")).unwrap();
    assert!(
        inventory.contains("lib\tfixture\tfixture\tsample\tok\n"),
        "{inventory}"
    );
}

#[test]
fn assertion_failure_is_judged_while_clean_protections_are_still_evaluated() {
    let sandbox = passing();
    sandbox.write("fixture/src/lib.rs", FAIL);
    let output = sandbox.suite();
    output.code(1);
    step_status(&sandbox, "test", "FAIL");
    step_status(&sandbox, "home-hygiene", "pass");
    step_status(&sandbox, "no-real-cli", "pass");
    let record = sandbox.result();
    assert!(record.contains("planted_failure"), "{record}");
    let log = fs::read_to_string(
        std::path::Path::new(field(&record, "run-dir")).join(row(&record, "test")[4]),
    )
    .unwrap();
    assert!(log.contains("planted_failure"), "{log}");
}

#[test]
fn inherited_and_workspace_redirects_cannot_hide_a_sentinel_home_writer() {
    let sandbox = passing();
    sandbox.write(
        "fixture/src/lib.rs",
        r#"
#[test]
fn redirect_sensitive_writer() {
    assert!(std::env::var_os("ORCHESTRY_GLOBAL_CONFIG_PATH").is_none());
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    std::fs::create_dir_all(home.join(".orchestry")).unwrap();
    std::fs::write(home.join(".orchestry/global.yml"), "{}\n").unwrap();
}
"#,
    );
    let output = sandbox.suite();
    output.code(1);
    step_status(&sandbox, "test", "pass");
    step_status(&sandbox, "home-hygiene", "FAIL");
    assert!(sandbox.result().contains("<HOME>/.orchestry"));
}

#[test]
fn cargo_owned_empty_home_directories_are_not_hygiene_findings() {
    let sandbox = passing();
    sandbox.write("fixture/src/lib.rs", r#"
#[test]
fn empty_cargo_directory() {
    std::fs::create_dir_all(std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo")).unwrap();
}
"#);
    sandbox.suite().code(0);
    step_status(&sandbox, "home-hygiene", "pass");
}

#[test]
fn every_agent_name_is_intercepted_by_gate_shims_and_not_by_outer_blockers() {
    let sandbox = passing();
    for name in AGENTS {
        sandbox.write("fixture/src/lib.rs", format!(r#"
#[test]
fn execs_one_fixture_cli() {{
    let status = std::process::Command::new({name:?}).args(["--fixture-canary", "argument with spaces"]).status().unwrap();
    assert_eq!(status.code(), Some(127));
}}
"#));
        let output = sandbox.suite();
        output.code(1);
        step_status(&sandbox, "test", "pass");
        step_status(&sandbox, "no-real-cli", "FAIL");
        let record = sandbox.result();
        assert!(
            row(&record, "no-real-cli")
                .join("\t")
                .split_whitespace()
                .any(
                    |token| token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                        == name
                )
                && row(&record, "no-real-cli")
                    .join("\t")
                    .contains("--fixture-canary"),
            "{record}"
        );
    }
}

#[test]
fn zero_executed_tests_cannot_report_evaluated_protections() {
    let sandbox = Sandbox::new();
    sandbox.mini("pub fn no_tests() {}\n", false);
    let output = sandbox.suite();
    output.code(1);
    let record = sandbox.result();
    assert!(
        row(&record, "test").join("\t").contains("no test executed"),
        "{record}"
    );
    for step in ["home-hygiene", "no-real-cli"] {
        let fields = row(&record, step);
        assert_eq!(fields[1], "FAIL");
        assert!(fields.join("\t").contains("not evaluated"), "{record}");
    }
}

#[test]
fn unavailable_toolchain_does_not_turn_an_unstarted_suite_into_clean_hygiene() {
    let sandbox = passing();
    let empty = sandbox.temporary.join("empty-rustup");
    fs::create_dir(&empty).unwrap();
    let mut command = sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]);
    command.env("RUSTUP_HOME", empty);
    sandbox.run(&mut command).code(1);
    step_status(&sandbox, "test", "FAIL");
    let record = sandbox.result();
    for step in ["home-hygiene", "no-real-cli"] {
        assert!(
            row(&record, step).join("\t").contains("not evaluated"),
            "{record}"
        );
    }
}

#[test]
fn missing_witness_fails_instead_of_certifying_suite_environment() {
    let sandbox = passing();
    fs::remove_file(sandbox.root.join("fixture/tests/gate_witness.rs")).unwrap();
    sandbox.suite().code(1);
    assert!(
        row(&sandbox.result(), "test")
            .join("\t")
            .contains("witness missing")
    );
}

#[test]
fn wrong_witness_redirect_is_a_witness_mismatch() {
    let sandbox = passing();
    let source = fs::read_to_string(sandbox.root.join("fixture/tests/gate_witness.rs")).unwrap();
    sandbox.write(
        "fixture/tests/gate_witness.rs",
        source.replace("\"unset\"", "\"set\""),
    );
    sandbox.suite().code(1);
    assert!(
        row(&sandbox.result(), "test")
            .join("\t")
            .contains("witness mismatch")
    );
}

#[test]
fn backdated_source_stale_control_passes_but_cold_gate_rebuilds_and_fails() {
    let sandbox = Sandbox::new();
    sandbox.mini(PASS, true);
    let target = sandbox.root.join("target/gate");
    let mut plain = sandbox.command("cargo");
    plain
        .args(["test", "--workspace", "--no-fail-fast"])
        .env("CARGO_TARGET_DIR", &target);
    sandbox.run(&mut plain).code(0);
    sandbox.write("fixture/src/lib.rs", FAIL);
    backdate(&sandbox.root.join("fixture/src/lib.rs"));
    let stale = sandbox.run(&mut plain);
    stale.code(0); // If Cargo rebuilds here the plant is invalid, not a gate success.
    assert!(
        !stale.text().contains("planted_failure"),
        "invalid stale plant: {}",
        stale.text()
    );
    fs::write(target.join("planted-stale-canary"), "must be deleted\n").unwrap();
    sandbox
        .run(&mut sandbox.gate(&["full", "--part", "suite", "--keep", "--lease-wait", "0"]))
        .code(1);
    step_status(&sandbox, "test", "FAIL");
    assert!(sandbox.result().contains("planted_failure"));
    assert!(!target.join("planted-stale-canary").exists());
}

#[test]
fn backdated_warm_content_resets_before_failing_new_source_is_run() {
    let sandbox = passing();
    sandbox.write("fixture/src/lib.rs", FAIL);
    backdate(&sandbox.root.join("fixture/src/lib.rs"));
    let mut plain = sandbox.command("cargo");
    plain
        .args(["test", "--workspace", "--no-fail-fast"])
        .env("CARGO_TARGET_DIR", sandbox.root.join("target/gate-warm"));
    let stale = sandbox.run(&mut plain);
    stale.code(0);
    assert!(
        !stale.text().contains("planted_failure"),
        "invalid stale plant"
    );
    sandbox.suite().code(1);
    let record = sandbox.result();
    assert_eq!(
        field(&record, "target"),
        "reset: content changed without a newer modification time"
    );
    assert!(record.contains("planted_failure"));
}

#[test]
fn relocated_warm_tree_resets_its_record_for_another_worktree() {
    let mut sandbox = passing();
    let moved = sandbox.temporary.join("moved-repo");
    fs::rename(&sandbox.root, &moved).unwrap();
    sandbox.root = moved;
    sandbox.suite().code(0);
    assert_eq!(
        field(&sandbox.result(), "target"),
        "reset: another worktree"
    );
}

#[test]
fn warm_target_symlink_is_refused_without_touching_outside_canary() {
    let sandbox = passing();
    let target = sandbox.root.join("target/gate-warm");
    fs::remove_dir_all(&target).unwrap();
    let outside = sandbox.temporary.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("canary"), "untouched\n").unwrap();
    symlink(&outside, target).unwrap();
    let output = sandbox.suite();
    output.code(1);
    output.contains("REFUSED");
    assert_eq!(
        fs::read_to_string(outside.join("canary")).unwrap(),
        "untouched\n"
    );
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        1,
        "refusal wrote new outside entries"
    );
}

fn real_home_writer(sandbox: &Sandbox, path: &std::path::Path, create: bool) {
    sandbox.write(
        "fixture/src/lib.rs",
        format!(
            r#"
#[test]
fn mutates_only_supplied_standin_home() {{
    let path = std::path::Path::new({path});
    if {create} {{ std::fs::create_dir_all(path.parent().unwrap()).unwrap(); }}
    std::fs::write(path, "changed-size-and-content\n").unwrap();
}}
"#,
            path = rust_literal(path)
        ),
    );
}

#[test]
fn unchanged_complete_standin_home_is_reported_as_unchanged() {
    let sandbox = passing();
    fs::create_dir_all(sandbox.temporary.join("home/.orchestry")).unwrap();
    fs::write(sandbox.temporary.join("home/.orchestry/global.yml"), "{}\n").unwrap();
    sandbox.suite().code(0);
    assert!(
        row(&sandbox.result(), "real-home")
            .join("\t")
            .contains("unchanged")
    );
    assert_eq!(field(&sandbox.result(), "real-home-coverage"), "complete");
}

#[test]
fn changed_existing_standin_real_home_file_is_listed() {
    let sandbox = passing();
    let path = sandbox.temporary.join("home/.orchestry/global.yml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{}\n").unwrap();
    real_home_writer(&sandbox, &path, false);
    sandbox.suite().code(1);
    let detail = row(&sandbox.result(), "real-home").join("\t");
    assert!(detail.contains("global.yml"), "{detail}");
}

#[test]
fn absent_standin_real_home_created_during_suite_is_reported() {
    let sandbox = passing();
    let path = sandbox.temporary.join("home/.orchestry/global.yml");
    assert!(!path.parent().unwrap().exists());
    real_home_writer(&sandbox, &path, true);
    sandbox.suite().code(1);
    assert!(
        row(&sandbox.result(), "real-home")
            .join("\t")
            .contains("created")
    );
}

#[test]
fn count_limited_unchanged_home_reports_partial_and_still_detects_top_level_mutation() {
    let sandbox = passing();
    let directory = sandbox.temporary.join("home/.orchestry");
    fs::create_dir(&directory).unwrap();
    for index in 0..5001 {
        fs::write(directory.join(format!("entry-{index:05}")), "x").unwrap();
    }
    sandbox.suite().code(0);
    let record = sandbox.result();
    let detail = row(&record, "real-home").join("\t");
    assert!(
        detail.contains("partial")
            && detail
                .split(|c: char| !c.is_ascii_digit())
                .any(|n| n.parse::<usize>().is_ok_and(|n| n > 0 && n <= 5000))
            && !detail.contains("unchanged"),
        "{detail}"
    );
    assert_eq!(field(&record, "real-home-coverage"), "partial");
    real_home_writer(&sandbox, &directory.join("entry-00000"), false);
    sandbox.suite().code(1);
    assert!(
        row(&sandbox.result(), "real-home")
            .join("\t")
            .contains("entry-00000")
    );
}

#[test]
fn depth_limited_home_detects_metadata_change_in_directory_at_bound() {
    let sandbox = passing();
    let boundary = sandbox.temporary.join("home/.orchestry/a/b/c/d");
    fs::create_dir_all(boundary.join("e")).unwrap();
    sandbox.suite().code(0);
    let record = sandbox.result();
    let detail = row(&record, "real-home").join("\t");
    assert!(
        detail.contains("partial") && !detail.contains("unchanged"),
        "{detail}"
    );
    real_home_writer(&sandbox, &boundary.join("created"), true);
    // Explicit old metadata prevents a same-tick directory update from hiding the plant.
    backdate(&boundary);
    sandbox.suite().code(1);
    assert!(
        row(&sandbox.result(), "real-home")
            .join("\t")
            .contains("a/b/c/d")
    );
}

fn assert_no_value_in_files(root: &std::path::Path, value: &[u8]) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            assert_no_value_in_files(&entry.path(), value);
        } else if kind.is_file() {
            let bytes = fs::read(entry.path()).unwrap();
            assert!(
                !bytes.windows(value.len()).any(|part| part == value),
                "secret leaked in {}",
                entry.path().display()
            );
        }
    }
}

#[test]
fn optins_unknown_and_empty_product_variables_refuse_before_any_suite_start() {
    let sandbox = Sandbox::new();
    let marker = sandbox.temporary.join("suite-started");
    sandbox.mini(
        &format!(
            "#[test]\nfn marks_start() {{ std::fs::write({}, \"started\").unwrap(); }}\n",
            rust_literal(&marker)
        ),
        true,
    );
    let secret = "SECRET_environment_value_do_not_export";
    for names in [
        vec!["ORCH_T1_REQUIRE_OLD"],
        vec!["ORCH_T1_OLD_BIN"],
        vec!["ORCH_T1_REQUIRE_OLD", "ORCH_T1_OLD_BIN"],
        vec!["ORCH_BLESS_FIXTURES"],
        vec!["ORCH_STRESS_SECS"],
        vec!["ORCH_INDEX_PROBE_ROOT"],
        vec!["ORCH_INDEX_PROBE_WRITER"],
        vec!["ORCH_INDEX_PROBE_ITERATIONS"],
        vec!["ORCH_SOME_NEW_KNOB"],
    ] {
        for value in [secret, ""] {
            let evidence = sandbox.temporary.join("refused-evidence");
            if evidence.exists() {
                fs::remove_dir_all(&evidence).unwrap();
            }
            let mut command = sandbox.gate(&["pr", "--part", "suite", "--evidence"]);
            command.arg(&evidence);
            for name in &names {
                command.env(name, value);
            }
            let output = sandbox.run(&mut command);
            output.code(1);
            output.contains("REFUSED");
            output.contains("inherited variable");
            for name in &names {
                output.contains(name);
            }
            assert!(!output.text().contains(secret));
            assert!(!marker.exists(), "refused suite started");
            assert!(
                !sandbox.temporary.join("lease/r-orch-gate.lease").exists(),
                "refusal acquired lease"
            );
            assert_no_value_in_files(&sandbox.root.join("target"), secret.as_bytes());
            if evidence.exists() {
                assert_no_value_in_files(&evidence, secret.as_bytes());
            }
        }
    }
}

#[test]
fn invalid_utf8_prefixed_environment_name_is_refused_without_value_leak() {
    let sandbox = Sandbox::new();
    sandbox.mini(PASS, true);
    let mut command = sandbox.gate(&["pr", "--part", "suite"]);
    command.env(
        std::ffi::OsString::from_vec(b"ORCH_BAD_\xff".to_vec()),
        "SECRET_nonutf8_value",
    );
    let output = sandbox.run(&mut command);
    output.code(1);
    output.contains("REFUSED");
    assert!(!output.text().contains("SECRET_nonutf8_value"));
}

#[test]
fn all_session_variables_are_removed_and_witness_sees_no_product_names() {
    let sandbox = passing();
    sandbox.write(
        "fixture/src/lib.rs",
        r#"
#[test]
fn observes_no_product_or_gate_control_variables() {
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        assert!(!name.starts_with("ORCH_") && !name.starts_with("ORCHESTRY_"));
        assert!(!name.starts_with("XTASK_GATE_") || name == "XTASK_GATE_WITNESS_DIR");
    }
}
"#,
    );
    let mut command = sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]);
    for name in SESSION {
        command.env(name, "SECRET_session_canary");
    }
    command.env("XTASK_GATE_UNUSED_CONTROL", "SECRET_control_canary");
    let output = sandbox.run(&mut command);
    output.code(0);
    for name in SESSION {
        output.contains(name);
    }
    for secret in ["SECRET_session_canary", "SECRET_control_canary"] {
        assert!(!output.text().contains(secret));
        assert_no_value_in_files(&sandbox.root.join("target"), secret.as_bytes());
    }
    step_status(&sandbox, "test", "pass");
}

#[test]
fn failed_compile_allows_recovery_and_reuse_of_unchanged_source() {
    let sandbox = Sandbox::new();
    sandbox.mini("this is not Rust\n", true);
    sandbox.suite().code(1);
    let first = sandbox.result();
    assert_eq!(field(&first, "target"), "reset: no record");
    // With the same failed source and no live writer, the next run can reuse
    // its failed-build record. No private record/marker name is guessed.
    sandbox.suite().code(1);
    assert_eq!(field(&sandbox.result(), "target"), "reused");
}

#[test]
fn source_changed_during_build_then_backdated_revert_cannot_reuse_racing_artifact() {
    use std::time::Duration;
    use witness::fixture::wait_for;
    let sandbox = passing();
    let ready = sandbox.temporary.join("build-script-ready");
    let release = sandbox.temporary.join("build-script-release");
    sandbox.write(
        "fixture/build.rs",
        format!(
            r#"
fn main() {{
    std::fs::write({ready}, "ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !std::path::Path::new({release}).exists() {{
        assert!(std::time::Instant::now() < deadline, "fixture handshake deadline");
        std::thread::park_timeout(std::time::Duration::from_millis(10));
    }}
}}
"#,
            ready = rust_literal(&ready),
            release = rust_literal(&release)
        ),
    );
    let child = sandbox.spawn(&mut sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]));
    wait_for(&ready, Duration::from_secs(30));
    sandbox.write(
        "fixture/src/lib.rs",
        "#[test]\nfn different_source_still_passes() { assert_eq!(3 + 3, 6); }\n",
    );
    fs::write(&release, "release").unwrap();
    let output = child.finish(Duration::from_secs(30));
    // Identity instability may fail/refuse this run. It must never bless a
    // record that allows an old-mtime revert to reuse the changed artifact.
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "{}",
        output.text()
    );
    assert!(
        !sandbox.root.join("target/.gate-state/warm.record").exists(),
        "changed source published a reusable record"
    );
    if output.status.success() {
        let record = sandbox.result();
        assert_eq!(field(&record, "publication"), "no record: source changed");
        assert_eq!(field(&record, "target-finished"), "yes");
        assert!(!sandbox.root.join("target/.gate-state/warm.use").exists());
    }
    sandbox.write("fixture/src/lib.rs", PASS);
    backdate(&sandbox.root.join("fixture/src/lib.rs"));
    sandbox.suite().code(0);
    assert!(
        field(&sandbox.result(), "target").starts_with("reset:"),
        "racing source record was reused"
    );
}

// C9 / RC16, RC20: real matching publication precedes each damaged-state input.
#[test]
fn missing_truncated_corrupt_and_oversized_warm_records_reset_actual_artifacts() {
    for damage in ["missing", "truncated", "corrupt", "oversized"] {
        let sandbox = passing();
        let record = sandbox.root.join("target/.gate-state/warm.record");
        let original = fs::read(&record).unwrap();
        assert!(!original.is_empty());
        let canary = sandbox.root.join("target/gate-warm/stale-artifact");
        fs::write(&canary, "previous build\n").unwrap();
        match damage {
            "missing" => fs::remove_file(&record).unwrap(),
            "truncated" => fs::write(&record, &original[..original.len() / 2]).unwrap(),
            "corrupt" => fs::write(&record, b"not a completed record\n").unwrap(),
            "oversized" => fs::write(&record, vec![b'x'; 8 * 1024 * 1024 + 1]).unwrap(),
            _ => unreachable!(),
        }
        sandbox.suite().code(0);
        assert_eq!(
            field(&sandbox.result(), "target"),
            if damage == "missing" {
                "reset: no record"
            } else {
                "reset: unreadable record"
            }
        );
        assert!(!canary.exists(), "{damage} retained stale artifact");
        assert!(record.is_file());
        assert!(!sandbox.root.join("target/.gate-state/warm.use").exists());
        sandbox.suite().code(0);
        assert_eq!(field(&sandbox.result(), "target"), "reused");
    }
}

#[test]
fn added_and_removed_source_entries_reset_a_previously_completed_warm_target() {
    let sandbox = passing();
    for added in [true, false] {
        let canary = sandbox.root.join("target/gate-warm/stale-artifact");
        fs::write(&canary, "old set\n").unwrap();
        if added {
            sandbox.write("fixture/src/new_source.rs", "// source inventory entry\n");
            // RC20: Cargo cannot repair an omitted old-mtime input by freshness.
            backdate(&sandbox.root.join("fixture/src/new_source.rs"));
        } else {
            fs::remove_file(sandbox.root.join("fixture/src/new_source.rs")).unwrap();
        }
        sandbox.suite().code(0);
        assert!(field(&sandbox.result(), "target").starts_with("reset:"));
        assert!(!canary.exists());
        sandbox.suite().code(0);
        assert_eq!(field(&sandbox.result(), "target"), "reused");
    }
}

#[test]
fn failed_warm_publication_keeps_marker_and_target_without_touching_outside_canary() {
    let sandbox = passing();
    let canary = sandbox.temporary.join("outside-publication-canary");
    fs::write(&canary, "untouched\n").unwrap();
    let temporary = sandbox.root.join("target/.gate-state/warm.record.tmp");
    sandbox.write(
        "fixture/src/lib.rs",
        format!(
            r#"
#[test]
fn plants_publication_link_after_native_admission() {{
    std::os::unix::fs::symlink({canary}, {temporary}).unwrap();
}}
"#,
            canary = rust_literal(&canary),
            temporary = rust_literal(&temporary)
        ),
    );
    sandbox.suite().code(1);
    let record = sandbox.result();
    assert!(field(&record, "target").starts_with("kept ("), "{record}");
    assert_eq!(field(&record, "target-finished"), "no");
    assert!(field(&record, "qualifying").starts_with("no"));
    assert!(sandbox.root.join("target/gate-warm").is_dir());
    assert!(sandbox.root.join("target/.gate-state/warm.use").is_file());
    assert!(!sandbox.root.join("target/.gate-state/warm.record").exists());
    assert!(
        fs::symlink_metadata(temporary)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(canary).unwrap(), "untouched\n");
    assert!(
        record.contains("warm.record.tmp"),
        "missing fault path: {record}"
    );
}

#[test]
fn existing_regular_publication_temporary_causes_real_finish_failure_without_replacement() {
    let sandbox = passing();
    let temporary = sandbox.root.join("target/.gate-state/warm.record.tmp");
    sandbox.write(
        "fixture/src/lib.rs",
        format!(
            r#"
#[test]
fn plants_regular_temporary_during_an_admitted_use() {{
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Write;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open({temporary}).unwrap().write_all(b"occupied temporary\n").unwrap();
}}
"#,
            temporary = rust_literal(&temporary)
        ),
    );
    sandbox.suite().code(1);
    let record = sandbox.result();
    assert_eq!(
        row(&record, "test")[1],
        "pass",
        "suite failed before publication: {record}"
    );
    assert!(field(&record, "target").starts_with("kept ("));
    assert!(
        record.contains("warm.record.tmp"),
        "publication failure lacks its path: {record}"
    );
    assert_eq!(field(&record, "target-finished"), "no");
    assert_eq!(
        fs::read_to_string(temporary).unwrap(),
        "occupied temporary\n"
    );
    assert!(sandbox.root.join("target/.gate-state/warm.use").exists());
    assert!(!sandbox.root.join("target/.gate-state/warm.record").exists());
}
