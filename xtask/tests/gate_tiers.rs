//! Tier/part/result contract. These miniature runs are never acceptance evidence.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use std::path::{Path, PathBuf};
use witness::fixture::{STEPS, Sandbox, field, row, rust_literal};

#[test]
fn help_names_default_release_goal_and_merge_tiers() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&mut sandbox.gate(&["--help"]));
    output.code(0);
    for text in [
        "full",
        "pr",
        "fast",
        "default",
        "merge condition",
        "goal",
        "release gate",
    ] {
        output.contains(text);
    }
    // Line wrapping and padding belong to clap. Associate tier tokens with
    // their description in a bounded word window, independent of display lines.
    let text = output.text();
    let words: Vec<_> = text
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .collect();
    for (tier, description) in [("pr", "merge"), ("full", "default"), ("full", "release")] {
        assert!(
            words
                .iter()
                .enumerate()
                .any(|(index, word)| *word == description
                    && words[index.saturating_sub(12)..(index + 13).min(words.len())]
                        .contains(&tier)),
            "help does not associate {tier} with {description}: {text}"
        );
    }
}

#[test]
fn default_and_pr_plans_have_ordered_steps_targets_and_exclusive_lease_without_running_tools() {
    let sandbox = Sandbox::new();
    let marker = sandbox.temporary.join("tool-started");
    sandbox.script(
        &sandbox.temporary.join("bin/cargo"),
        &format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
    );
    for (args, tier, target) in [
        (vec!["--plan"], "full", "target/gate"),
        (vec!["full", "--plan"], "full", "target/gate"),
        (vec!["pr", "--plan"], "pr", "target/gate-warm"),
    ] {
        let output = sandbox.run(&mut sandbox.gate(&args));
        output.code(0);
        output.contains(tier);
        output.contains(target);
        output.contains("exclusive");
        let text = output.text();
        let mut cursor = 0;
        for step in STEPS {
            cursor += text[cursor..]
                .find(step)
                .unwrap_or_else(|| panic!("missing ordered {step}: {text}"))
                + step.len();
        }
        assert!(!marker.exists(), "--plan ran cargo");
        assert!(
            !sandbox.root.join("target").exists(),
            "--plan mutated target"
        );
        assert!(
            !sandbox.temporary.join("lease/r-orch-gate.lease").exists(),
            "--plan acquired lease"
        );
    }
}

#[test]
fn unknown_tier_unknown_option_and_pr_warm_are_usage_errors() {
    let sandbox = Sandbox::new();
    for (args, offending) in [
        (vec!["unknown-tier"], "unknown-tier"),
        (vec!["--unknown-option"], "--unknown-option"),
        (vec!["pr", "--warm"], "--warm"),
    ] {
        let output = sandbox.run(&mut sandbox.gate(&args));
        output.code(2);
        assert!(
            output.stderr.contains(offending),
            "missing usage attribution {offending}: {}",
            output.text()
        );
    }
    // The pre-PR2 `fast` unavailable observation is a frozen-stage validation
    // command in the handoff, not a permanent assertion about implemented fast.
}

#[test]
fn passing_miniature_has_timed_step_rows_total_and_partial_result_identity() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n", true);
    sandbox.suite().code(0);
    let record = sandbox.result();
    assert_eq!(field(&record, "tier"), "pr");
    assert_eq!(field(&record, "part"), "suite");
    assert!(!field(&record, "verdict").starts_with("PASSED (tier"));
    assert!(
        Path::new(field(&record, "run-dir")).starts_with(sandbox.root.join("target/gate-runs"))
    );
    let mut sum = 0_u64;
    for step in ["build", "test", "home-hygiene", "no-real-cli", "real-home"] {
        let fields = row(&record, step);
        assert_eq!(fields.len(), 6, "{record}");
        assert_eq!(fields[1], "pass");
        assert_eq!(fields[2], "0");
        sum += fields[3].parse::<u64>().expect("nonnegative step wall-ms");
        assert!(!fields[5].is_empty());
    }
    assert_eq!(field(&record, "total-wall-ms").parse::<u64>().unwrap(), sum);
}

#[test]
fn legacy_warm_uses_developer_target_and_never_claims_a_gate() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() { assert_eq!(2 + 2, 4); }\n", true);
    sandbox
        .run(&mut sandbox.gate(&["full", "--warm", "--part", "suite", "--lease-wait", "0"]))
        .code(0);
    let record = sandbox.result();
    assert_eq!(field(&record, "target"), "developer (not a gate)");
    assert!(record.contains("not a gate"));
    assert!(field(&record, "qualifying").starts_with("no"));
}

#[test]
fn preflight_refusal_retains_refused_result_and_table() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    let mut command = sandbox.gate(&["full", "--part", "suite"]);
    command.env("ORCH_SOME_NEW_KNOB", "SECRET_refused_value");
    let output = sandbox.run(&mut command);
    output.code(1);
    output.contains("REFUSED");
    let record = sandbox.result();
    assert!(field(&record, "verdict").starts_with("REFUSED"));
    // F4 renders the public human-readable verdict/time/status table.
    for column in ["status", "step", "wall(s)", "exit", "detail"] {
        assert!(
            record.contains(column),
            "missing result column {column}: {record}"
        );
    }
    assert!(!record.contains("SECRET_refused_value"));
}

#[test]
fn missing_advisories_tool_is_env_limited_with_install_guidance() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    // Forward native version/identity queries; stub only the static boundary.
    let real = sandbox.cargo_forwarder();
    sandbox.script(
        &sandbox.temporary.join("bin/cargo"),
        &format!(
            r#"#!/bin/sh
case "$*" in
  *fmt*|*clippy*|*crate-graph*) exit 0 ;;
  *) exec '{}' "$@" ;;
esac
"#,
            real.display()
        ),
    );
    let tools = sandbox.temporary.join("static-tools");
    fs::create_dir(&tools).unwrap();
    for name in ["git", "df", "ps", "sh", "env", "sysctl"] {
        let source = [
            PathBuf::from("/usr/bin").join(name),
            PathBuf::from("/bin").join(name),
            PathBuf::from("/usr/sbin").join(name),
        ]
        .into_iter()
        .find(|path| path.is_file());
        if let Some(source) = source {
            std::os::unix::fs::symlink(source, tools.join(name)).unwrap();
        }
    }
    std::os::unix::fs::symlink(sandbox.temporary.join("bin/cargo"), tools.join("cargo")).unwrap();
    std::os::unix::fs::symlink(sandbox.temporary.join("bin/rustc"), tools.join("rustc")).unwrap();
    let deny = tools.join("cargo-deny");
    sandbox.script(
        &deny,
        "#!/bin/sh\nprintf '%s\\n' 'cargo-deny synthetic-static-boundary'\n",
    );
    let mut positive = sandbox.gate(&["pr", "--part", "static", "--lease-wait", "0"]);
    positive.env("PATH", &tools);
    sandbox.run(&mut positive).code(0);
    for step in ["clippy", "fmt", "advisories", "crate-graph"] {
        assert_eq!(row(&sandbox.result(), step)[1], "pass");
    }
    // Remove just the advisory executable after actual native admission.
    fs::remove_file(&deny).unwrap();
    let mut command = sandbox.gate(&["pr", "--part", "static", "--lease-wait", "0"]);
    command.env("PATH", &tools);
    let output = sandbox.run(&mut command);
    output.code(3);
    let record = sandbox.result();
    for step in ["clippy", "fmt", "crate-graph"] {
        assert_eq!(row(&record, step)[1], "pass", "{record}");
    }
    let fields = row(&record, "advisories");
    assert_eq!(fields[1], "ENV-LIMITED");
    assert!(
        fields
            .join("\t")
            .contains("cargo install cargo-deny --locked")
    );
    assert!(field(&record, "verdict").starts_with("ENV-LIMITED"));
}

fn exported_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink(), "export contains symlink");
        if kind.is_dir() {
            files.extend(exported_files(&entry.path()));
        } else {
            assert!(kind.is_file());
            files.push(entry.path());
        }
    }
    files
}

#[test]
fn otherwise_passing_partial_export_exits_three_and_redacts_private_paths() {
    let sandbox = Sandbox::new();
    sandbox.mini(&format!(r#"
#[test]
fn emits_private_paths_through_a_child() {{
    assert!(std::process::Command::new("/bin/echo").args([{root}, {home}, {tmp}]).status().unwrap().success());
}}
"#, root = rust_literal(&sandbox.root), home = rust_literal(&sandbox.temporary.join("home")),
        tmp = rust_literal(&sandbox.temporary.join("tmp"))), true);
    let export = sandbox.temporary.join("export");
    let mut command = sandbox.gate(&[
        "pr",
        "--part",
        "suite",
        "--lease-wait",
        "0",
        "--host-label",
        "synthetic-lab",
        "--evidence",
    ]);
    command.arg(&export);
    sandbox.run(&mut command).code(3);
    let record = sandbox.result();
    for step in ["build", "test", "home-hygiene", "no-real-cli", "real-home"] {
        assert_eq!(row(&record, step)[1], "pass", "{record}");
    }
    for file in exported_files(&export) {
        let bytes = fs::read(&file).unwrap();
        for private in [
            &sandbox.root,
            &sandbox.temporary.join("home"),
            &sandbox.temporary.join("tmp"),
        ] {
            let needle = private.as_os_str().as_encoded_bytes();
            assert!(
                !bytes.windows(needle.len()).any(|part| part == needle),
                "path leak in {}",
                file.display()
            );
        }
    }
    let copied = fs::read_to_string(export.join("result.txt")).unwrap();
    assert_eq!(field(&copied, "host-label"), "synthetic-lab");
    assert!(field(&copied, "qualifying").starts_with("no"));
}

#[test]
fn password_prompt_and_control_canaries_are_redacted_or_export_is_refused() {
    let sandbox = Sandbox::new();
    let canaries = [
        "SECRET_password_canary",
        "SECRET_prompt_canary",
        "SECRET_token_canary",
    ];
    sandbox.mini(
        &format!(
            r#"
#[test]
fn emits_known_sensitive_diagnostic_values() {{
    assert!(std::process::Command::new("/bin/echo").args({canaries:?}).status().unwrap().success());
}}
"#
        ),
        true,
    );
    let export = sandbox.temporary.join("export");
    let mut command = sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0", "--evidence"]);
    command
        .arg(&export)
        .env("ORCH_WEB_PASSWORD", canaries[0])
        .env("ORCHESTRY_TASK_PROMPT", canaries[1])
        .env("XTASK_GATE_UNUSED_CONTROL", canaries[2]);
    let output = sandbox.run(&mut command);
    assert!(
        matches!(output.status.code(), Some(1 | 3)),
        "{}",
        output.text()
    );
    assert!(
        !output.text().contains("SECRET_"),
        "secret leaked in terminal output"
    );
    if output.status.code() == Some(3) {
        assert!(export.is_dir(), "exit 3 requires an actual evidence export");
        assert!(!exported_files(&export).is_empty(), "empty evidence export");
        assert!(export.join("result.txt").is_file(), "export missing result");
    }
    if export.exists() {
        for file in exported_files(&export) {
            let bytes = fs::read(file).unwrap();
            for value in canaries {
                assert!(
                    !bytes
                        .windows(value.len())
                        .any(|part| part == value.as_bytes())
                );
            }
        }
    }
    if output.status.code() == Some(1) {
        output.contains("REFUSED");
    }
}

#[test]
fn explicitly_kept_cold_export_is_finished_but_cannot_qualify_or_damage_warm_sibling() {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test]\nfn sample() {}\n", true);
    sandbox.suite().code(0);
    let warm_record = sandbox.root.join("target/.gate-state/warm.record");
    let warm_bytes = fs::read(&warm_record).unwrap();
    let canary = sandbox.root.join("target/gate-warm/warm-sibling");
    fs::write(&canary, "warm\n").unwrap();
    sandbox
        .run(&mut sandbox.gate(&["full", "--part", "suite", "--lease-wait", "0"]))
        .code(0);
    assert_eq!(field(&sandbox.result(), "target"), "deleted");
    assert!(!sandbox.root.join("target/gate").exists());
    let export = sandbox.temporary.join("kept-export");
    sandbox
        .run(
            sandbox
                .gate(&[
                    "full",
                    "--part",
                    "suite",
                    "--keep",
                    "--lease-wait",
                    "0",
                    "--evidence",
                ])
                .arg(&export),
        )
        .code(3);
    let record = sandbox.result();
    assert_eq!(field(&record, "target"), "kept by request");
    assert_eq!(field(&record, "target-finished"), "yes");
    assert!(field(&record, "qualifying").contains("target kept by request"));
    assert!(sandbox.root.join("target/gate").is_dir());
    assert!(!sandbox.root.join("target/.gate-state/cold.use").exists());
    assert_eq!(fs::read(&warm_record).unwrap(), warm_bytes);
    assert_eq!(fs::read_to_string(canary).unwrap(), "warm\n");
    let copied = fs::read_to_string(export.join("result.txt")).unwrap();
    assert_eq!(field(&copied, "target-finished"), "yes");
    assert_eq!(field(&copied, "target"), "kept by request");
    let commit = sandbox.git(&["rev-parse", "HEAD"]);
    sandbox
        .run(
            sandbox
                .gate(&["evidence", "verify"])
                .arg(&export)
                .args(["--commit", &commit]),
        )
        .code(1);
}
