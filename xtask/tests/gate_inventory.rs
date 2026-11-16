//! Prepared Cargo/log inputs exercise only the public inventory command.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use witness::fixture::{Capture, Sandbox};

fn executable(package: &str, kind: &str, target: &str, path: &str) -> String {
    serde_json::json!({
        "reason": "compiler-artifact", "package_id": format!("path+file:///fixture/{package}#0.0.0"),
        "manifest_path": format!("/fixture/{package}/Cargo.toml"),
        "target": {"kind": [kind], "crate_types": [if kind == "lib" {"lib"} else {"bin"}],
            "name": target, "src_path": format!("/fixture/{package}/tests/{target}.rs"),
            "edition": "2024", "doc": true, "doctest": true, "test": true},
        "profile": {"opt_level": "0", "debuginfo": 0, "debug_assertions": true,
            "overflow_checks": true, "test": true},
        "features": [], "filenames": [path], "executable": path, "fresh": false
    }).to_string() + "\n"
}

fn section(label: &str, path: &str, lines: &str, summary: &str) -> String {
    let count = lines
        .lines()
        .filter(|line| line.starts_with("test "))
        .count();
    format!(
        "     Running {label} ({path})\n\nrunning {count} test{}\n{lines}\n\ntest result: {summary}; 0 measured; 0 filtered out; finished in 0.00s\n\n",
        if count == 1 { "" } else { "s" }
    )
}

fn passing() -> (String, String) {
    (
        executable(
            "fixture",
            "test",
            "probe",
            "/fixture/target/debug/deps/probe-123",
        ),
        section(
            "tests/probe.rs",
            "/fixture/target/debug/deps/probe-123",
            "test actual ... ok",
            "ok. 1 passed; 0 failed; 0 ignored",
        ),
    )
}

fn inventory(sandbox: &Sandbox, map: &str, log: &str, exit: Option<i32>) -> Capture {
    let rows = sandbox.root.join("rows.tsv");
    if rows.exists() {
        fs::remove_file(rows).unwrap();
    }
    sandbox.write("executables.jsonl", map);
    sandbox.write("suite.log", log);
    let mut command = sandbox.gate(&[
        "inventory",
        "--log",
        "suite.log",
        "--executables",
        "executables.jsonl",
        "--out",
        "rows.tsv",
    ]);
    if let Some(exit) = exit {
        command.args(["--exit", &exit.to_string()]);
    }
    sandbox.run(&mut command)
}

fn require_positive(sandbox: &Sandbox) {
    let (map, log) = passing();
    inventory(sandbox, &map, &log, Some(0)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        "test\tfixture\tprobe\tactual\tok\n"
    );
}

#[test]
fn passing_inventory_preserves_exact_named_rows_and_default_success_exit() {
    let sandbox = Sandbox::new();
    let (map, log) = passing();
    inventory(&sandbox, &map, &log, None).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        "test\tfixture\tprobe\tactual\tok\n"
    );
}

#[test]
fn failed_named_test_with_successful_process_exit_is_inconsistent() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (map, log) = passing();
    let log = log
        .replace("test actual ... ok", "test actual ... FAILED")
        .replace("ok. 1 passed; 0 failed", "FAILED. 0 passed; 1 failed");
    let output = inventory(&sandbox, &map, &log, Some(0));
    output.code(1);
    output.contains("inconsistent");
}

#[test]
fn unsuccessful_process_without_failed_test_is_inconsistent() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (map, log) = passing();
    let output = inventory(&sandbox, &map, &log, Some(101));
    output.code(1);
    output.contains("inconsistent");
}

#[test]
fn mapped_executable_without_completion_is_incomplete() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (mut map, log) = passing();
    map.push_str(&executable(
        "other",
        "test",
        "probe",
        "/fixture/target/debug/deps/probe-456",
    ));
    let output = inventory(&sandbox, &map, &log, Some(0));
    output.code(1);
    output.contains("incomplete");
}

#[test]
fn same_named_targets_keep_distinct_package_identities() {
    let sandbox = Sandbox::new();
    let (mut map, mut log) = passing();
    map.push_str(&executable(
        "other",
        "test",
        "probe",
        "/fixture/target/debug/deps/probe-456",
    ));
    log.push_str(&section(
        "tests/probe.rs",
        "/fixture/target/debug/deps/probe-456",
        "test actual ... ok",
        "ok. 1 passed; 0 failed; 0 ignored",
    ));
    inventory(&sandbox, &map, &log, Some(0)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        "test\tfixture\tprobe\tactual\tok\ntest\tother\tprobe\tactual\tok\n"
    );
}

#[test]
fn failure_body_result_lookalikes_do_not_create_inventory_rows() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (map, _) = passing();
    let log = "     Running tests/probe.rs (/fixture/target/debug/deps/probe-123)\n\nrunning 1 test\ntest actual ... FAILED\n\nfailures:\n\n---- actual stdout ----\ntest forged ... ok\ntest result: ok. 100 passed; 0 failed; 0 ignored\n\nthread 'actual' panicked at fixture/tests/probe.rs:1:1:\nplanted failure\n\n\nfailures:\n    actual\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\nerror: test failed, to rerun pass `-p fixture --test probe`\n";
    // Captured stdout belongs to the failed actual test. It cannot add rows.
    inventory(&sandbox, &map, log, Some(101)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        "test\tfixture\tprobe\tactual\tfailed\n"
    );
}

#[test]
fn ignored_and_doc_tests_have_sorted_named_identities() {
    let sandbox = Sandbox::new();
    let map = executable(
        "fixture",
        "lib",
        "fixture",
        "/fixture/target/debug/deps/fixture-123",
    );
    let log = section(
        "unittests src/lib.rs",
        "/fixture/target/debug/deps/fixture-123",
        "test sample ... ok\ntest skipped ... ignored",
        "ok. 1 passed; 0 failed; 1 ignored",
    ) + "   Doc-tests fixture\n\nrunning 1 test\ntest fixture/src/lib.rs - sample (line 1) ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    inventory(&sandbox, &map, &log, Some(0)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        "doc\tfixture\tfixture\tfixture/src/lib.rs - sample (line 1)\tok\nlib\tfixture\tfixture\tsample\tok\nlib\tfixture\tfixture\tskipped\tignored\n"
    );
}

#[test]
fn merged_child_output_cannot_inflate_passing_inventory() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (map, log) = passing();
    let log = log.replace(
        "test actual ... ok",
        "test actual ... ok\ntest forged ... ok",
    );
    let output = inventory(&sandbox, &map, &log, Some(0));
    output.code(1);
    output.contains("inconsistent");
}

#[test]
fn split_result_line_cannot_be_accepted_as_complete_inventory() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (map, log) = passing();
    let log = log.replace("test actual ... ok", "test actual child-output\n ... ok");
    let output = inventory(&sandbox, &map, &log, Some(0));
    output.code(1);
    output.contains("inconsistent");
}

#[test]
fn malformed_executable_map_cannot_be_a_passing_empty_inventory() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (_, log) = passing();
    inventory(&sandbox, "{not cargo json}\n", &log, Some(0)).code(1);
}

#[test]
fn consistent_failed_inventory_parses_successfully_while_real_folded_suite_stays_failed() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    sandbox.suite().code(0);
    sandbox.write(
        "fixture/src/lib.rs",
        "#[test]\nfn planted_failure() { panic!(\"required failure\"); }\n",
    );
    sandbox.suite().code(1);
    let record = sandbox.result();
    assert!(witness::fixture::field(&record, "verdict").starts_with("FAILED"));
    assert_eq!(witness::fixture::row(&record, "test")[1], "FAIL");
    let run = std::path::Path::new(witness::fixture::field(&record, "run-dir"));
    let failed_rows = fs::read_to_string(run.join("inventory.tsv")).unwrap();
    assert!(failed_rows.contains("lib\tfixture\tfixture\tplanted_failure\tfailed\n"));
    // Structural parser success is deliberately independent from suite success.
    let (map, log) = passing();
    let log = log
        .replace("test actual ... ok", "test actual ... FAILED")
        .replace("ok. 1 passed; 0 failed", "FAILED. 0 passed; 1 failed");
    inventory(&sandbox, &map, &log, Some(101)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        "test\tfixture\tprobe\tactual\tfailed\n"
    );
    assert!(witness::fixture::field(&sandbox.result(), "verdict").starts_with("FAILED"));
}

// Independent controls use invented names and literal expected rows. They guard
// reconciliation, rather than reproducing the retained workspace's identities.
const INTERLEAVED: &str = include_str!("fixtures/real_inventory/interleaved.log");
const INTERLEAVED_ROWS: &str = include_str!("fixtures/real_inventory/interleaved.tsv");

fn interleaved_map() -> String {
    executable(
        "fixture",
        "lib",
        "fixture-lib",
        "/fixture/target/debug/deps/library-abc",
    )
}

fn reject_interleaved(map: &str, log: &str) {
    let sandbox = Sandbox::new();
    // Pair every refusal with the complete positive so unrelated setup failures
    // cannot satisfy a negative. Refused input must not publish an inventory.
    inventory(&sandbox, &interleaved_map(), INTERLEAVED, Some(0)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        INTERLEAVED_ROWS
    );
    inventory(&sandbox, map, log, Some(0)).code(1);
    assert!(!sandbox.root.join("rows.tsv").exists());
}

#[test]
fn interleaved_helpers_progress_and_split_docs_preserve_literal_outer_rows() {
    let sandbox = Sandbox::new();
    inventory(&sandbox, &interleaved_map(), INTERLEAVED, Some(0)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        INTERLEAVED_ROWS
    );
}

#[test]
fn nested_helper_without_redundant_identity_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replacen("test zeta ... ok", "test stranger ... ok", 1),
    );
}

#[test]
fn nested_helper_with_conflicting_terminal_status_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replacen("test zeta ... ok", "test zeta ... FAILED", 1),
    );
}

#[test]
fn duplicate_outer_row_without_matching_helper_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace("test skipped ...", "test zeta ... ok\ntest skipped ..."),
    );
}

#[test]
fn nested_helper_missing_announcement_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replacen("running 1 test\n", "", 1),
    );
}

#[test]
fn nested_helper_missing_summary_is_refused() {
    reject_interleaved(&interleaved_map(), &INTERLEAVED.replace("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.02s\n", ""));
}

#[test]
fn nested_helper_filtered_complement_must_match_outer_count() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replacen("2 filtered out", "1 filtered out", 1),
    );
}

#[test]
fn nested_multi_test_announcement_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replacen("running 1 test\n", "running 2 tests\n", 1),
    );
}

#[test]
fn nonterminal_notice_without_named_terminal_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace("test alpha has been", "test absent has been"),
    );
}

#[test]
fn repeated_nonterminal_notice_is_refused() {
    reject_interleaved(&interleaved_map(), &INTERLEAVED.replace("test alpha has been running for over 60 seconds\n", "test alpha has been running for over 60 seconds\ntest alpha has been running for over 60 seconds\n"));
}

#[test]
fn arbitrary_text_cannot_replace_standard_nonterminal_notice() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace(
            "has been running for over 60 seconds",
            "is probably still working",
        ),
    );
}

#[test]
fn notice_after_its_terminal_row_is_refused() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace(
            "test alpha has been running for over 60 seconds",
            "test zeta has been running for over 60 seconds",
        ),
    );
}

#[test]
fn split_doc_group_cannot_repeat_a_previous_identity() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace(
            "src/lib.rs - rejected (line 19) - compile fail",
            "src/lib.rs - example (line 8)",
        ),
    );
}

#[test]
fn split_doc_group_requires_its_own_complete_summary() {
    let truncated = INTERLEAVED.rsplit_once("test result:").unwrap().0;
    reject_interleaved(&interleaved_map(), truncated);
}

#[test]
fn split_doc_group_count_cannot_be_borrowed_from_another_group() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace(
            "running 1 test\ntest src/lib.rs - rejected",
            "running 2 tests\ntest src/lib.rs - rejected",
        ),
    );
}

#[test]
fn duplicate_doc_header_is_refused_even_for_disjoint_groups() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace(
            "running 1 test\ntest src/lib.rs - rejected",
            "Doc-tests fixture_lib\nrunning 1 test\ntest src/lib.rs - rejected",
        ),
    );
}

#[test]
fn ambiguous_normalized_doc_target_is_refused() {
    let map = interleaved_map()
        + &executable(
            "other",
            "lib",
            "fixture_lib",
            "/fixture/target/debug/deps/other-def",
        );
    let other = section(
        "unittests src/lib.rs",
        "/fixture/target/debug/deps/other-def",
        "test other ... ok",
        "ok. 1 passed; 0 failed; 0 ignored",
    );
    let log = INTERLEAVED.replace("Doc-tests fixture_lib", &(other + "Doc-tests fixture_lib"));
    reject_interleaved(&map, &log);
}

#[test]
fn unknown_executable_cannot_supply_a_completed_group() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace("library-abc)", "unknown-def)"),
    );
}

#[test]
fn duplicate_executable_map_entry_is_refused() {
    reject_interleaved(&(interleaved_map() + &interleaved_map()), INTERLEAVED);
}

#[test]
fn unknown_executable_kind_is_refused() {
    let sandbox = Sandbox::new();
    require_positive(&sandbox);
    let (_, log) = passing();
    let map = executable(
        "fixture",
        "unknown-kind",
        "probe",
        "/fixture/target/debug/deps/probe-123",
    );
    inventory(&sandbox, &map, &log, Some(0)).code(1);
    assert!(!sandbox.root.join("rows.tsv").exists());
}

#[test]
fn missing_mapped_zero_test_executable_is_refused() {
    let map = interleaved_map()
        + &executable(
            "other",
            "test",
            "empty",
            "/fixture/target/debug/deps/empty-def",
        );
    reject_interleaved(&map, INTERLEAVED);
}

#[test]
fn completed_zero_test_executable_does_not_erase_other_named_rows() {
    let sandbox = Sandbox::new();
    let map = interleaved_map()
        + &executable(
            "other",
            "test",
            "empty",
            "/fixture/target/debug/deps/empty-def",
        );
    let log = INTERLEAVED.to_owned()
        + &section(
            "tests/empty.rs",
            "/fixture/target/debug/deps/empty-def",
            "",
            "ok. 0 passed; 0 failed; 0 ignored",
        );
    inventory(&sandbox, &map, &log, Some(0)).code(0);
    assert_eq!(
        fs::read_to_string(sandbox.root.join("rows.tsv")).unwrap(),
        INTERLEAVED_ROWS
    );
}

#[test]
fn outer_summary_status_cannot_disagree_with_failed_count() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace("test result: ok. 2 passed", "test result: FAILED. 2 passed"),
    );
}

#[test]
fn result_duration_and_field_labels_require_standard_grammar() {
    for log in [
        INTERLEAVED.replace("finished in 60.01s", "finished in NaNs"),
        INTERLEAVED.replace("2 passed;", "2 successes;"),
        INTERLEAVED.replace("0 measured;", "1 measured;"),
    ] {
        reject_interleaved(&interleaved_map(), &log);
    }
}

#[test]
fn unknown_terminal_status_cannot_be_treated_as_success() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace(" ... ok", " ... success"),
    );
}

#[test]
fn noninteger_announced_count_cannot_be_normalized_into_an_integer() {
    reject_interleaved(
        &interleaved_map(),
        &INTERLEAVED.replace("running 3 tests", "running 3.0 tests"),
    );
}
