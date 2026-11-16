//! Tier/part/result contract. These miniature runs are never acceptance evidence.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use std::path::{Path, PathBuf};
use witness::fixture::{Capture, STEPS, Sandbox, field, row, rust_literal};

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
    output.code(3);
    assert!(export.join("MANIFEST").is_file());
    for step in ["build", "test", "home-hygiene", "no-real-cli", "real-home"] {
        assert_eq!(row(&sandbox.result(), step)[1], "pass");
    }
    assert!(!output.text().contains("SECRET_"), "terminal secret leak");
    for file in exported_files(&export) {
        let text = fs::read_to_string(file).unwrap();
        for value in canaries {
            assert!(!text.contains(value));
        }
    }
    assert!(
        field(
            &fs::read_to_string(export.join("result.txt")).unwrap(),
            "qualifying"
        )
        .starts_with("no")
    );
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

// Actual native Cargo build/map/test output, never a synthetic success bundle.
fn sensitive_export_case(secret: &str, failed: bool, refused: bool) {
    sensitive_export_payload_case(secret, failed, refused, None);
}

fn sensitive_export_payload_case(
    secret: &str,
    failed: bool,
    refused: bool,
    payload: Option<String>,
) {
    let sandbox = Sandbox::new();
    let failure = if failed {
        "panic!(\"intentional suite failure\");"
    } else {
        ""
    };
    sandbox.mini(&format!(r#"
#[test]
fn sensitive_child_output() {{
    let secret = std::env::var("SYNTHETIC_API_KEY").unwrap();
    assert!(std::process::Command::new("/usr/bin/printf").args(["%s\\n", &secret]).status().unwrap().success());
    assert!(std::process::Command::new("/bin/sh").args(["-c", "printf '%s\\n' \"$SYNTHETIC_API_KEY\" >&2"]).status().unwrap().success());
    {failure}
}}
"#), true);
    // Supply the real judge's required source set, without pretending this
    // direct miniature invocation is a content-stamped bootstrap.
    sandbox.write(
        "xtask/gate.sh",
        "#!/bin/sh\n# synthetic source only; never execute\nexit 2\n",
    );
    sandbox.commit();
    let inputs: std::collections::BTreeMap<_, _> = [
        ".cargo/config.toml",
        "Cargo.lock",
        "Cargo.toml",
        "rust-toolchain.toml",
        "xtask/gate.sh",
    ]
    .into_iter()
    .map(|name| {
        (
            name,
            sandbox.blob(&fs::read(sandbox.root.join(name)).unwrap()),
        )
    })
    .collect();
    let expected_judge = sandbox.blob(
        inputs
            .into_iter()
            .map(|(name, blob)| format!("{blob} {name}\n"))
            .collect::<String>()
            .as_bytes(),
    );
    let real = sandbox.cargo_forwarder();
    sandbox.script(&sandbox.temporary.join("bin/cargo"), &format!(r#"#!/bin/sh
case " $* " in
  *" build "*) printf 'build stdout: %s\n' "$SYNTHETIC_API_KEY"; printf 'build stderr: %s\n' "$SYNTHETIC_API_KEY" >&2; printf '%s\n' "$SYNTHETIC_ENCODED_VALUE" >&2 ;;
  *" --no-run "*) printf 'map stderr: %s\n' "$SYNTHETIC_API_KEY" >&2 ;;
esac
exec '{}' "$@"
"#, real.display()));
    let export = sandbox.temporary.join("export");
    let mut command = sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0", "--evidence"]);
    command
        .arg(&export)
        .args(["--host-label", secret])
        .env("SYNTHETIC_API_KEY", secret);
    if let Some(payload) = payload {
        command.env("SYNTHETIC_ENCODED_VALUE", payload);
    } else if secret.starts_with("nested-") || secret.starts_with("deep-") {
        let json = serde_json::json!({"nested": serde_json::json!({"value": secret}).to_string()})
            .to_string();
        let json = if secret.starts_with("deep-") {
            format!("{}{json}{}", "[".repeat(130), "]".repeat(130))
        } else {
            json
        };
        command.env("SYNTHETIC_ENCODED_VALUE", json);
    }
    command.env("XTASK_GATE_JUDGE_ID", &expected_judge);
    let output = sandbox.run(&mut command);
    retain_export_case(&sandbox, &command, &output, &export);
    // Prove the canary actually reached each production encoding boundary.
    let record = sandbox.result();
    assert_eq!(field(&record, "target-finished"), "yes");
    let judge = field(&record, "judge-source-id");
    assert_eq!(judge, expected_judge);
    assert_eq!(judge.len(), 40);
    assert!(judge.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(row(&record, "build")[1], "pass");
    assert_eq!(
        row(&record, "test")[1],
        if failed { "FAIL" } else { "pass" }
    );
    assert_eq!(row(&record, "test")[2], if failed { "101" } else { "0" });
    let run = Path::new(field(&record, "run-dir"));
    let build = fs::read_to_string(run.join("steps/build.log")).unwrap();
    for reason in ["gate-build-log", "gate-map-stderr"] {
        let value: serde_json::Value = build
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|v| v["reason"] == reason)
            .unwrap();
        assert!(
            value["text"].as_str().unwrap().contains(secret),
            "canary missed {reason}"
        );
        if reason == "gate-build-log" {
            assert!(value["text"].as_str().unwrap().contains("build stdout:"));
            assert!(value["text"].as_str().unwrap().contains("build stderr:"));
        }
    }
    assert!(
        fs::read_to_string(run.join("steps/test.log"))
            .unwrap()
            .matches(secret)
            .count()
            >= 2,
        "both test stdout and stderr must reach the real log"
    );
    assert!(
        record.contains(
            &secret
                .chars()
                .flat_map(char::escape_default)
                .collect::<String>()
        )
    );
    assert_sensitive_absent(&output.text(), secret);
    if refused {
        output.code(1);
        output.contains("evidence redaction refused");
        assert!(!export.exists(), "refusal left a partial export");
    } else {
        output.code(if failed { 1 } else { 3 });
        assert!(export.join("MANIFEST").is_file());
        let copied = fs::read_to_string(export.join("result.txt")).unwrap();
        assert_eq!(
            field(&copied, "judge-source-id"),
            field(&record, "judge-source-id")
        );
        assert_eq!(
            field(&copied, "step-test-exit"),
            if failed { "101" } else { "0" }
        );
        assert!(field(&copied, "qualifying").starts_with("no"));
        assert_eq!(field(&copied, "host-label"), "<REDACTED>");
        for file in exported_files(&export) {
            assert_sensitive_absent(&fs::read_to_string(file).unwrap(), secret);
        }
        let manifest = fs::read_to_string(export.join("MANIFEST")).unwrap();
        for line in manifest.lines() {
            let (digest, name) = line.split_once("  ").unwrap();
            assert_eq!(digest, sandbox.blob(&fs::read(export.join(name)).unwrap()));
        }
        let verify = sandbox.run(
            sandbox
                .gate(&["evidence", "verify"])
                .arg(&export)
                .args(["--commit", &sandbox.git(&["rev-parse", "HEAD"])]),
        );
        verify.code(1);
        verify.contains("nonqualifying");
    }
}

fn assert_sensitive_absent(text: &str, secret: &str) {
    assert!(!text.contains(secret), "raw secret leak");
    let diagnostic: String = secret.chars().flat_map(char::escape_default).collect();
    assert!(!text.contains(&diagnostic), "diagnostic secret leak");
    fn decoded(value: &serde_json::Value, secret: &str) {
        match value {
            serde_json::Value::String(s) => assert!(!s.contains(secret), "decoded secret leak"),
            serde_json::Value::Array(items) => items.iter().for_each(|v| decoded(v, secret)),
            serde_json::Value::Object(items) => {
                for (key, value) in items {
                    assert!(!key.contains(secret));
                    decoded(value, secret);
                }
            }
            _ => {}
        }
    }
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str(line) {
            decoded(&value, secret);
        }
    }
}

#[test]
fn escaped_known_secret_is_redacted_across_actual_output_and_export() {
    sensitive_export_case("known-\"secret\\canary", false, false);
}
#[test]
fn failed_suite_export_keeps_failure_and_redacts_encoded_secret() {
    sensitive_export_case("failed-\"secret\\canary", true, false);
}
#[test]
fn one_byte_known_secret_refuses_without_partial_export() {
    sensitive_export_case("~", false, true);
}
#[test]
fn two_byte_known_secret_refuses_without_partial_export() {
    sensitive_export_case("~!", false, true);
}
#[test]
fn three_byte_known_secret_refuses_without_partial_export() {
    sensitive_export_case("q7!", false, true);
}

fn cargo_configuration_case(location: &str) {
    let sandbox = Sandbox::new();
    sandbox.mini("#[test] fn sample() {}", true);
    let config = "[env]\nXDG_CONFIG_HOME = { value = \"synthetic-redirect\", force = true }\nCODEX_HOME = { value = \"synthetic-codex\", force = true }\nCUSTOM_REMOVED = { value = \"synthetic-custom\", force = true }\n";
    // The source declares the names to strip, regardless of which Cargo
    // configuration attempts to put them back into the actual test process.
    sandbox.write(".cargo/config.toml", format!("{config}ORCHESTRY_GLOBAL_CONFIG_PATH = \"unused-global.yml\"\nXTASK_GATE_WITNESS_DIR = \"unused-witness\"\n"));
    if location != "source" {
        let path = if location == "cargo-home" {
            sandbox.temporary.join("cargo-home/config.toml")
        } else {
            sandbox.temporary.join("tmp/.cargo/config.toml")
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, config).unwrap();
    }
    sandbox.commit();
    let mut command = sandbox.gate(&["pr", "--part", "suite", "--lease-wait", "0"]);
    let output = sandbox.run(&mut command);
    retain_export_case(
        &sandbox,
        &command,
        &output,
        &sandbox.temporary.join("no-export"),
    );
    let record = sandbox.result();
    assert_eq!(row(&record, "build")[1], "pass", "{location}: {record}");
    assert_eq!(row(&record, "test")[2], "0", "{location}: {record}");
    if location == "source" {
        output.code(0);
        assert_eq!(row(&record, "test")[1], "pass");
    } else {
        output.code(1);
        assert_eq!(row(&record, "test")[1], "FAIL");
        assert!(
            row(&record, "test").join(" ").contains("witness mismatch"),
            "{record}"
        );
        let run = Path::new(field(&record, "run-dir"));
        let environment = fs::read_to_string(run.join("environment.txt")).unwrap();
        for name in ["XDG_CONFIG_HOME", "CODEX_HOME", "CUSTOM_REMOVED"] {
            assert!(environment.contains(&format!("removed {name}\n")));
        }
    }
}

#[test]
fn source_config_removed_names_do_not_override_required_gate_values() {
    cargo_configuration_case("source");
}
#[test]
fn cargo_home_config_reintroduced_removed_names_fail_witness() {
    cargo_configuration_case("cargo-home");
}
#[test]
fn cwd_ancestor_config_reintroduced_removed_names_fail_witness() {
    cargo_configuration_case("ancestor");
}

// Retain actual inner argv/environment/streams/records when the existing private
// fixture root is supplied. Assertions never depend on this optional recorder.
fn retain_export_case(
    sandbox: &Sandbox,
    command: &std::process::Command,
    output: &Capture,
    export: &Path,
) {
    let Some(root) = std::env::var_os("XTASK_F2_PRIVATE_ROOT") else {
        return;
    };
    let destination = PathBuf::from(root).join(sandbox.temporary.file_name().unwrap());
    fs::create_dir(&destination).unwrap();
    let environment: std::collections::BTreeMap<_, _> = command
        .get_envs()
        .map(|(key, value)| (key.to_string_lossy(), value.map(|s| s.to_string_lossy())))
        .collect();
    let invocation = serde_json::json!({
        "program": command.get_program().to_string_lossy(),
        "argv": command.get_args().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),
        "cwd": command.get_current_dir().unwrap(), "environment": environment,
        "status": output.status.code(), "synthetic_secrets_only": true,
    });
    fs::write(
        destination.join("invocation.json"),
        serde_json::to_vec_pretty(&invocation).unwrap(),
    )
    .unwrap();
    fs::write(destination.join("stdout"), &output.stdout_bytes).unwrap();
    fs::write(destination.join("stderr"), &output.stderr_bytes).unwrap();
    for (name, path) in [
        (
            "source-config.toml",
            sandbox.root.join(".cargo/config.toml"),
        ),
        (
            "cargo-home-config.toml",
            sandbox.temporary.join("cargo-home/config.toml"),
        ),
        (
            "ancestor-config.toml",
            sandbox.temporary.join("tmp/.cargo/config.toml"),
        ),
        (
            "miniature-source.rs",
            sandbox.root.join("fixture/src/lib.rs"),
        ),
    ] {
        if path.is_file() {
            fs::copy(path, destination.join(name)).unwrap();
        }
    }
    let record = sandbox.result();
    let run = Path::new(field(&record, "run-dir"));
    for file in exported_files(run) {
        let path = destination
            .join("run")
            .join(file.strip_prefix(run).unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(file, path).unwrap();
    }
    if export.exists() {
        for file in exported_files(export) {
            let path = destination
                .join("export")
                .join(file.strip_prefix(export).unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::copy(file, path).unwrap();
        }
    }
}

#[test]
fn nested_json_known_secret_refuses_without_partial_export() {
    sensitive_export_case("nested-\"secret\\canary", false, true);
}

#[test]
fn json_decode_budget_cannot_hide_a_known_secret_in_export() {
    sensitive_export_case("deep-\"secret\\canary", false, true);
}

#[test]
fn prefixed_double_encoded_secret_refuses_without_partial_export() {
    let secret = "known-\"secret\\canary";
    let payload = serde_json::json!({"nested": serde_json::json!({"value": secret}).to_string()});
    sensitive_export_payload_case(
        secret,
        false,
        true,
        Some(format!("build-script diagnostic: {payload}")),
    );
}

#[test]
fn deeper_prefixed_escaped_keys_and_values_refuse_without_partial_export() {
    let secret = "known-\"secret\\canary";
    // Alternate Unicode escaping prevents fixed literal/escape substitutions.
    let inner = serde_json::json!({secret: [secret]})
        .to_string()
        .replace("known-", "known\\u002d");
    let mut payload = inner;
    for _ in 0..4 {
        payload = serde_json::json!({"nested": payload}).to_string();
    }
    sensitive_export_payload_case(
        secret,
        false,
        true,
        Some(format!(
            "first {{not json}} then {payload} trailing diagnostic"
        )),
    );
}

#[test]
fn prefixed_json_depth_refuses_without_partial_export() {
    sensitive_export_payload_case(
        "known-\"secret\\canary",
        false,
        true,
        Some(format!(
            "diagnostic: {}0{}",
            "[".repeat(130),
            "]".repeat(130)
        )),
    );
}

#[test]
fn malformed_fragment_work_budget_refuses_without_partial_export() {
    sensitive_export_payload_case(
        "known-\"secret\\canary",
        false,
        true,
        Some("{x".repeat(20_000)),
    );
}
