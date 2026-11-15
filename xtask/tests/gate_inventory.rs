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
