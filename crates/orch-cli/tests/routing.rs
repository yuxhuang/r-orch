//! The routing contract, exercised through the real binary.
//!
//! Ported from `test/unit/cli/cli-lazy-routing.test.ts` plus the verified
//! tables in `docs/parity/cli.md` §3.5–§3.7. The TS suite asserts these by
//! mocking the container builders; here the binary is run, because the
//! property under test is *what happens before parsing*, and that is only
//! observable from outside.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

/// A directory with no `.orchestry` anywhere above it, which is what makes
/// the exit-3 assertions mean anything — `findProjectRoot` walks up.
fn empty_project() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("orch-routing-")
        .tempdir()
        .expect("tempdir");
    assert!(
        !has_orchestry_ancestor(dir.path()),
        "the tmpdir has an .orchestry ancestor; point TMPDIR outside any ORCH project"
    );
    dir
}

fn has_orchestry_ancestor(start: &Path) -> bool {
    start.ancestors().any(|dir| dir.join(".orchestry").is_dir())
}

fn orch(cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env_remove("ORCHESTRY_DEBUG");
    command
}

/// `orch init` in a fresh directory, so the initialized-project cases have
/// something to run against.
fn initialized_project() -> tempfile::TempDir {
    let dir = empty_project();
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "shell"])
        .assert()
        .success();
    dir
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ── Uninitialized: the container is built before parsing ───────────────────

#[test]
fn every_guarded_command_exits_three_in_an_uninitialized_directory() {
    let dir = empty_project();
    for argv in [
        vec!["task", "list"],
        vec!["agent", "list"],
        vec!["status"],
        vec!["logs", "--task", "tsk_1"],
        vec!["config", "get", "a.b"],
        vec!["context", "list"],
        vec!["msg", "list"],
        vec!["goal", "list"],
        vec!["team", "list"],
        vec!["org", "list"],
        vec!["run", "tsk_1"],
        vec!["serve", "--once"],
    ] {
        let output = orch(dir.path()).args(&argv).output().expect("run");
        assert_eq!(
            output.status.code(),
            Some(3),
            "orch {}: {}",
            argv.join(" "),
            stderr_of(&output)
        );
        assert_eq!(
            stderr_of(&output),
            "  ✕ Not initialized\n    Run: orch init\n",
            "orch {}",
            argv.join(" ")
        );
    }
}

#[test]
fn help_on_a_subcommand_also_exits_three_because_the_container_comes_first() {
    let dir = empty_project();
    for argv in [
        vec!["logs", "--help"],
        vec!["task", "--help"],
        vec!["task", "list", "--help"],
    ] {
        let output = orch(dir.path()).args(&argv).output().expect("run");
        assert_eq!(
            output.status.code(),
            Some(3),
            "orch {} should not print help",
            argv.join(" ")
        );
        assert!(stdout_of(&output).is_empty());
    }
}

#[test]
fn an_unknown_command_or_option_exits_three_before_the_usage_error() {
    let dir = empty_project();
    for argv in [vec!["frobnicate"], vec!["--nope"]] {
        let output = orch(dir.path()).args(&argv).output().expect("run");
        assert_eq!(
            output.status.code(),
            Some(3),
            "orch {}: {}",
            argv.join(" "),
            stderr_of(&output)
        );
        assert!(stderr_of(&output).contains("Not initialized"));
    }
}

#[test]
fn the_three_container_less_commands_run_in_an_uninitialized_directory() {
    let dir = empty_project();

    // `doctor` probes adapters and always exits 0.
    let output = orch(dir.path()).arg("doctor").output().expect("run");
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(stdout_of(&output).contains("orch doctor · checking adapters and dependencies"));
    assert!(
        stdout_of(&output).contains("of 5 adapters ready"),
        "container-less doctor probes five adapters: {}",
        stdout_of(&output)
    );
    assert!(stdout_of(&output).contains("not found — run: orch init"));

    // `update` reports the version and exits 0.
    let output = orch(dir.path()).arg("update").output().expect("run");
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_of(&output).contains("orch update · version"));

    let output = orch(dir.path())
        .args(["update", "--check"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn help_and_version_work_without_a_project() {
    let dir = empty_project();

    let output = orch(dir.path()).arg("--help").output().expect("run");
    assert_eq!(output.status.code(), Some(0));
    let help = stdout_of(&output);
    // Every one of the sixteen commands is listed.
    for command in orch_cli::tree::ALL_KNOWN_COMMANDS {
        assert!(help.contains(command), "--help omits {command}");
    }

    let output = orch(dir.path()).arg("--version").output().expect("run");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_of(&output).trim(), "1.0.34");

    let output = orch(dir.path()).arg("-V").output().expect("run");
    assert_eq!(stdout_of(&output).trim(), "1.0.34");
}

// ── Initialized: Commander's own failures ──────────────────────────────────

#[test]
fn a_group_with_no_subcommand_prints_help_to_stdout_and_exits_one() {
    let dir = initialized_project();
    for group in [
        "task", "agent", "config", "context", "msg", "goal", "team", "org",
    ] {
        let output = orch(dir.path()).arg(group).output().expect("run");
        assert_eq!(output.status.code(), Some(1), "orch {group}");
        assert!(
            stdout_of(&output).contains("Usage:"),
            "orch {group} must print help on stdout"
        );
        assert!(
            stderr_of(&output).is_empty(),
            "orch {group} must not print to stderr: {}",
            stderr_of(&output)
        );
    }
}

#[test]
fn the_nested_config_global_group_behaves_the_same() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["config", "global"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout_of(&output).contains("Usage:"));
}

#[test]
fn usage_errors_exit_one_with_commanders_wording() {
    let dir = initialized_project();

    let output = orch(dir.path()).arg("frobnicate").output().expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr_of(&output), "error: unknown command 'frobnicate'\n");

    let output = orch(dir.path()).arg("--nope").output().expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr_of(&output), "error: unknown option '--nope'\n");

    let output = orch(dir.path())
        .args(["task", "list", "--bogus"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr_of(&output), "error: unknown option '--bogus'\n");

    let output = orch(dir.path())
        .args(["agent", "add", "shelly"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr_of(&output),
        "error: required option '--adapter <adapter>' not specified\n"
    );
}

#[test]
fn help_on_a_subcommand_works_once_the_project_exists() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["logs", "--help"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));
    let help = stdout_of(&output);
    for flag in ["--agent", "--task", "--follow", "--since"] {
        assert!(help.contains(flag), "logs --help omits {flag}");
    }
}

// ── Per-command exit codes from §3.7 ───────────────────────────────────────

#[test]
fn logs_with_no_selector_exits_two() {
    let dir = initialized_project();
    let output = orch(dir.path()).arg("logs").output().expect("run");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr_of(&output),
        "  ✕ Specify a run ID, --task, --agent, or --since <duration>\n"
    );
}

#[test]
fn logs_with_a_bad_since_exits_two() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["logs", "--since", "banana"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr_of(&output),
        "  ✕ Invalid duration: \"banana\". Use format: 5m, 1h, 30s, 1d\n"
    );
}

#[test]
fn logs_for_a_missing_run_is_not_an_error() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["logs", "run_missing"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_of(&output), "\n  No events for run run_missing\n\n");
}

#[test]
fn logs_json_emits_an_empty_array_for_a_missing_run() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["--json", "logs", "run_missing"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_of(&output), "[]\n");
}

#[test]
fn logs_for_a_task_and_an_agent_report_no_runs() {
    let dir = initialized_project();

    let output = orch(dir.path())
        .args(["logs", "--task", "tsk_missing"])
        .output()
        .expect("run");
    assert_eq!(stdout_of(&output), "\n  No runs for task tsk_missing\n\n");

    let output = orch(dir.path())
        .args(["logs", "--agent", "agt_missing"])
        .output()
        .expect("run");
    assert_eq!(stdout_of(&output), "\n  No runs for agent agt_missing\n\n");

    let output = orch(dir.path())
        .args(["logs", "--since", "1h"])
        .output()
        .expect("run");
    assert_eq!(
        stdout_of(&output),
        "\n  No runs in the specified time window\n\n"
    );
}

#[test]
fn run_with_no_selector_exits_two() {
    let dir = initialized_project();
    let output = orch(dir.path()).arg("run").output().expect("run");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr_of(&output),
        "  ✕ Specify a task ID, --all, or --watch\n"
    );
}

#[test]
fn serve_with_an_unknown_log_format_exits_two() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["serve", "--log-format", "yaml"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr_of(&output),
        "  ✕ Unknown --log-format \"yaml\". Valid: json, text\n"
    );
}

#[test]
fn init_with_an_unknown_adapter_exits_two_and_writes_nothing() {
    let dir = empty_project();
    let output = orch(dir.path())
        .args(["init", "--adapter", "banana"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_of(&output).contains("Unknown adapter \"banana\""));
    assert!(
        !dir.path().join(".orchestry").exists(),
        "a rejected adapter must not leave a project behind"
    );
}

#[test]
fn init_is_idempotent_and_exits_zero_the_second_time() {
    let dir = initialized_project();
    let output = orch(dir.path()).arg("init").output().expect("run");
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_of(&output).contains("Already initialized"));
}

#[test]
fn doctor_with_a_project_probes_eight_adapters_and_still_exits_zero() {
    let dir = initialized_project();
    let output = orch(dir.path()).arg("doctor").output().expect("run");
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_of(&output);
    assert!(text.contains("of 8 adapters ready"), "{text}");
    assert!(text.contains(".orchestry/   exists ·"), "{text}");
}

#[test]
fn doctor_json_prints_the_two_header_lines_before_the_json() {
    let dir = initialized_project();
    let output = orch(dir.path())
        .args(["--json", "doctor"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));

    let text = stdout_of(&output);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(""));
    assert_eq!(
        lines.next(),
        Some("  orch doctor · checking adapters and dependencies")
    );
    assert_eq!(lines.next(), Some(""));

    // The rest is the report, so `doctor --json` stdout is not pure JSON.
    let json_start = text.find('{').expect("a JSON object");
    let report: serde_json::Value =
        serde_json::from_str(&text[json_start..]).expect("the report parses");
    assert_eq!(report["adaptersTotal"], 8);
    assert!(report["checks"].as_array().expect("checks").len() >= 12);
}

#[test]
fn every_declared_command_is_wired() {
    // The ratchet that replaced `… is not wired yet`: there is no such
    // command any more, so the assertion is that no command *claims* to be
    // unwired. `orch tui` was the last one, and it needs a TTY, so it is
    // checked for the message rather than for success.
    let dir = initialized_project();
    for argv in [vec!["tui"], vec!["run", "--all"], vec!["org", "list"]] {
        let output = orch(dir.path()).args(&argv).output().expect("run");
        let stderr = stderr_of(&output);
        assert!(
            !stderr.contains("not wired yet"),
            "orch {}: {stderr}",
            argv.join(" ")
        );
    }
}

#[test]
fn status_reports_the_project_and_emits_claimed_as_an_array() {
    // PARITY §14.1's fix, from outside: the oracle emits `claimed: {}`.
    let dir = initialized_project();

    let output = orch(dir.path()).arg("status").output().expect("run");
    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        stdout_of(&output).starts_with("\norch · "),
        "{}",
        stdout_of(&output)
    );

    let output = orch(dir.path())
        .args(["--json", "status"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&output)).expect("pure JSON");
    assert!(report["tasks"].is_array());
    assert!(report["agents"].is_array());
    assert!(
        report["state"]["claimed"].is_array(),
        "claimed must be an array, got {}",
        report["state"]["claimed"]
    );
}

#[test]
fn the_three_binary_names_are_the_same_program() {
    let dir = empty_project();
    for binary in ["orch", "orchestry", "ao"] {
        let output = Command::cargo_bin(binary)
            .expect("binary")
            .current_dir(dir.path())
            .arg("--version")
            .output()
            .expect("run");
        assert_eq!(stdout_of(&output).trim(), "1.0.34", "{binary}");
    }
}
