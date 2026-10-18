//! The `--yaml` contract: wherever `--json` is accepted, `--yaml` prints the
//! same value as YAML.
//!
//! Two layers, because they fail differently:
//!
//! 1. **The command tree** (no process): every command that accepts `--json`
//!    also accepts `--yaml`, as a value-less flag that conflicts with
//!    `--json`. This is a walk over `Cli::command()`, so a command added
//!    later that gets `--json` but not `--yaml` fails here without anyone
//!    remembering to extend a list.
//! 2. **The binary** (throwaway project, throwaway `HOME`, a `shell` agent
//!    running `true`): for a representative command per output family,
//!    `--yaml` parses to exactly the value `--json` parses to. Equality of
//!    the *parsed* values is the contract; the byte layout is not.
//!
//! Errors are part of the contract: `--yaml` must fail the way `--json` does
//! (same exit code, same stdout, a parseable stderr), not print a half-built
//! document.

use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::prelude::*;
use clap::{CommandFactory, Parser};
use orch_cli::tree::Cli;
use serde_json::Value;

// ── Layer 1: the clap command tree ─────────────────────────────────────────

/// Every command in the tree as `("task list", command)`, root included as
/// `""`, after `build()` so global flags are propagated to each subcommand.
fn walk(cmd: &clap::Command, path: &str, out: &mut Vec<(String, clap::Command)>) {
    out.push((path.to_owned(), cmd.clone()));
    for sub in cmd.get_subcommands() {
        // `help` is clap's generated subcommand; it takes no output flags.
        if sub.get_name() == "help" {
            continue;
        }
        let next = if path.is_empty() {
            sub.get_name().to_owned()
        } else {
            format!("{path} {}", sub.get_name())
        };
        walk(sub, &next, out);
    }
}

fn every_command() -> Vec<(String, clap::Command)> {
    let mut root = Cli::command();
    root.build();
    let mut all = Vec::new();
    walk(&root, "", &mut all);
    all
}

fn has_arg(cmd: &clap::Command, id: &str) -> bool {
    cmd.get_arguments().any(|a| a.get_id() == id)
}

#[test]
fn the_walk_reaches_a_realistic_number_of_commands_with_json() {
    // Guards the two tests below against passing vacuously (a walk that
    // stopped at the root would "prove" nothing).
    let with_json = every_command()
        .iter()
        .filter(|(_, c)| has_arg(c, "json"))
        .count();
    assert!(with_json >= 40, "only {with_json} commands accept --json");
}

#[test]
fn every_command_that_accepts_json_also_accepts_yaml() {
    let missing: Vec<String> = every_command()
        .into_iter()
        .filter(|(_, c)| has_arg(c, "json") && !has_arg(c, "yaml"))
        .map(|(path, _)| format!("orch {path}").trim_end().to_owned())
        .collect();
    assert!(missing.is_empty(), "no --yaml on: {missing:#?}");
}

#[test]
fn yaml_is_a_valueless_flag_on_every_command_that_has_it() {
    let bad: Vec<String> = every_command()
        .into_iter()
        .filter_map(|(path, c)| {
            let arg = c.get_arguments().find(|a| a.get_id() == "yaml")?;
            let takes_value = arg.get_action().takes_values();
            takes_value.then(|| format!("orch {path}"))
        })
        .collect();
    assert!(bad.is_empty(), "--yaml takes a value on: {bad:#?}");
}

#[test]
fn yaml_conflicts_with_json_on_every_command_that_accepts_both() {
    let unguarded: Vec<String> = every_command()
        .into_iter()
        .filter(|(_, c)| has_arg(c, "json") && has_arg(c, "yaml"))
        .filter(|(_, c)| {
            let yaml = c.get_arguments().find(|a| a.get_id() == "yaml").unwrap();
            let json = c.get_arguments().find(|a| a.get_id() == "json").unwrap();
            let yaml_blocks_json = c
                .get_arg_conflicts_with(yaml)
                .iter()
                .any(|a| a.get_id() == "json");
            let json_blocks_yaml = c
                .get_arg_conflicts_with(json)
                .iter()
                .any(|a| a.get_id() == "yaml");
            !(yaml_blocks_json || json_blocks_yaml)
        })
        .map(|(path, _)| format!("orch {path}"))
        .collect();
    assert!(
        unguarded.is_empty(),
        "--json and --yaml may be combined on: {unguarded:#?}"
    );
}

#[test]
fn yaml_is_accepted_before_and_after_the_subcommand() {
    for argv in [
        ["orch", "--yaml", "task", "list"],
        ["orch", "task", "--yaml", "list"],
        ["orch", "task", "list", "--yaml"],
    ] {
        Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} failed to parse: {e}"));
    }
}

// ── Layer 2: the binary ────────────────────────────────────────────────────

/// Strings a YAML emitter must quote or escape to survive a round trip: they
/// are YAML scalars of another type, or syntax, when written bare.
const TRICKY_TITLES: &[&str] = &[
    "null",
    "yes",
    "no",
    "true",
    "123",
    "1e3",
    "0x1F",
    "~",
    "[a, b]",
    "{a: b}",
    "| pipe",
    "> fold",
    "%dir",
    "@at",
    "? question",
    "key: value",
    "# not a comment",
    "line1\nline2",
    "inner  double  spaces",
    "tab\there",
    "emoji 🚀 ünï",
    "'single' \"double\"",
    "*star &anchor !tag",
];

struct Project {
    dir: tempfile::TempDir,
    agent: String,
    other_agent: String,
    task: String,
    goal: String,
    team: String,
    run: Option<String>,
}

fn orch_in(dir: &Path, cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(cwd)
        .env("HOME", dir.join("home"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", dir.join("home/global.yml"))
        .env("NO_COLOR", "1")
        // No agent CLIs on PATH: the only adapter used is `shell` running
        // `true`, and `doctor`-style probes find nothing real to exec.
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG");
    command
}

fn scratch() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("orch-yaml-contract-")
        .tempdir()
        .expect("tempdir");
    assert!(
        !dir.path()
            .ancestors()
            .any(|d| d.join(".orchestry").is_dir()),
        "the tmpdir has an .orchestry ancestor; point TMPDIR outside any ORCH project"
    );
    std::fs::create_dir_all(dir.path().join("home")).expect("home");
    std::fs::create_dir_all(dir.path().join("p")).expect("project dir");
    dir
}

impl Project {
    fn root(&self) -> std::path::PathBuf {
        self.dir.path().join("p")
    }

    fn orch(&self) -> Command {
        orch_in(self.dir.path(), &self.root())
    }

    /// `orch <args> --json`, parsed.
    fn json(&self, args: &[&str]) -> Value {
        parse_json(&success(self.orch().args(args).arg("--json")))
    }

    /// `orch <args> --yaml`, parsed.
    fn yaml(&self, args: &[&str]) -> Value {
        parse_yaml(&success(self.orch().args(args).arg("--yaml")))
    }
}

fn success(command: &mut Command) -> Output {
    let output = command.output().expect("run orch");
    assert!(
        output.status.success(),
        "{:?} exited {:?}\nstdout: {}\nstderr: {}",
        command,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn parse_json(output: &Output) -> Value {
    serde_json::from_str(&stdout(output))
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}", stdout(output)))
}

fn parse_yaml(output: &Output) -> Value {
    serde_yaml_ng::from_str(&stdout(output))
        .unwrap_or_else(|e| panic!("stdout is not YAML ({e}):\n{}", stdout(output)))
}

fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("an id").to_owned()
}

/// A project with two agents, a task per tricky title plus one plain task
/// assigned to an agent, a goal, a team, a context entry and a message. With
/// `with_run`, the assigned task is also run once so `logs` has data.
fn project(with_run: bool) -> Project {
    let dir = scratch();
    let root = dir.path().join("p");
    let mut p = Project {
        dir,
        agent: String::new(),
        other_agent: String::new(),
        task: String::new(),
        goal: String::new(),
        team: String::new(),
        run: None,
    };
    success(
        p.orch()
            .args(["init", "--name", "contract", "--adapter", "shell"]),
    );
    let agent = |name: &str, role: &str| {
        id_of(&parse_json(&success(orch_in(p.dir.path(), &root).args([
            "agent",
            "add",
            name,
            "--adapter",
            "shell",
            "--command",
            "true",
            "--role",
            role,
            "--json",
        ]))))
    };
    p.agent = agent("alpha", "plain role");
    p.other_agent = agent("beta", "colon: role\nwith a second line");
    for title in TRICKY_TITLES {
        success(p.orch().args(["task", "add", title, "--json"]));
    }
    p.task = id_of(&p.json(&["task", "add", "plain task", "--assignee", &p.agent.clone()]));
    p.goal = id_of(&p.json(&["goal", "add", "yes", "--description", "a: b"]));
    p.team = id_of(&p.json(&["team", "create", "crew", "--lead", &p.agent.clone()]));
    success(p.orch().args(["context", "set", "k", "null"]));
    success(p.orch().args([
        "msg",
        "send",
        &p.other_agent.clone(),
        "hello: world",
        "-s",
        "subj",
    ]));
    if with_run {
        success(p.orch().args(["run", &p.task.clone()]));
        let runs = p.json(&["logs", "--task", &p.task.clone()]);
        p.run = Some(id_of(&runs[0]));
    }
    p
}

/// One test per command: `--yaml` parses to the value `--json` parses to.
macro_rules! yaml_equals_json {
    ($name:ident, $with_run:expr, |$p:ident| $args:expr) => {
        #[test]
        fn $name() {
            let $p = project($with_run);
            let args: Vec<String> = $args;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let json = $p.json(&args);
            let yaml = $p.yaml(&args);
            assert_eq!(yaml, json);
            // Equality of two empty-ish values proves little; make sure
            // there was something to compare.
            assert!(!json.is_null(), "{args:?} printed null");
        }
    };
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

yaml_equals_json!(agent_list_yaml_equals_json, false, |p| words(&[
    "agent", "list"
]));
yaml_equals_json!(agent_show_yaml_equals_json, false, |p| {
    let mut a = words(&["agent", "show"]);
    a.push(p.other_agent.clone());
    a
});
yaml_equals_json!(task_list_yaml_equals_json, false, |p| words(&[
    "task", "list"
]));
yaml_equals_json!(task_show_yaml_equals_json, false, |p| {
    let mut a = words(&["task", "show"]);
    a.push(p.task.clone());
    a
});
yaml_equals_json!(goal_list_yaml_equals_json, false, |p| words(&[
    "goal", "list"
]));
yaml_equals_json!(goal_show_yaml_equals_json, false, |p| {
    let mut a = words(&["goal", "show"]);
    a.push(p.goal.clone());
    a
});
yaml_equals_json!(team_list_yaml_equals_json, false, |p| words(&[
    "team", "list"
]));
yaml_equals_json!(team_show_yaml_equals_json, false, |p| {
    let mut a = words(&["team", "show"]);
    a.push(p.team.clone());
    a
});
yaml_equals_json!(org_list_yaml_equals_json, false, |p| words(&[
    "org", "list"
]));
yaml_equals_json!(config_get_yaml_equals_json, false, |p| words(&[
    "config",
    "get",
    "project.name"
]));
yaml_equals_json!(config_global_show_yaml_equals_json, false, |p| words(&[
    "config", "global", "show"
]));
yaml_equals_json!(status_yaml_equals_json, false, |p| words(&["status"]));
yaml_equals_json!(context_list_yaml_equals_json, false, |p| words(&[
    "context", "list"
]));
yaml_equals_json!(context_get_yaml_equals_json, false, |p| words(&[
    "context", "get", "k"
]));
yaml_equals_json!(msg_inbox_yaml_equals_json, false, |p| {
    let mut a = words(&["msg", "inbox"]);
    a.push(p.other_agent.clone());
    a
});
yaml_equals_json!(remote_list_yaml_equals_json_when_empty, false, |p| words(
    &["remote", "list"]
));
// `logs --task` prints Run[] (run metadata); `logs <run-id>` prints
// RunEvent[] (the event stream of one run). Different payload families.
yaml_equals_json!(logs_by_task_yaml_equals_json, true, |p| words(&[
    "logs", "--task", &p.task
]));
yaml_equals_json!(logs_run_events_yaml_equals_json, true, |p| {
    let mut a = words(&["logs"]);
    a.push(p.run.clone().expect("a run"));
    a
});

#[test]
fn empty_collections_round_trip_to_empty_sequences() {
    let dir = scratch();
    let p = Project {
        dir,
        agent: String::new(),
        other_agent: String::new(),
        task: String::new(),
        goal: String::new(),
        team: String::new(),
        run: None,
    };
    success(
        p.orch()
            .args(["init", "--name", "empty", "--adapter", "shell"]),
    );
    for args in [
        ["agent", "list"],
        ["task", "list"],
        ["goal", "list"],
        ["team", "list"],
    ] {
        assert_eq!(p.yaml(&args), Value::Array(Vec::new()), "{args:?}");
    }
}

#[test]
fn tricky_task_titles_survive_a_yaml_round_trip_unchanged() {
    let p = project(false);
    let tasks = p.yaml(&["task", "list"]);
    let titles: Vec<&str> = tasks
        .as_array()
        .expect("a list")
        .iter()
        .map(|t| t["title"].as_str().expect("string title (not coerced)"))
        .collect();
    for want in TRICKY_TITLES {
        assert!(
            titles.contains(want),
            "{want:?} lost or coerced in {titles:?}"
        );
    }
}

#[test]
fn a_context_value_that_looks_like_null_stays_a_string_in_yaml() {
    let p = project(false);
    let entry = p.yaml(&["context", "get", "k"]);
    assert_eq!(entry["value"], Value::String("null".into()));
}

#[test]
fn yaml_stdout_is_one_document_with_no_decoration() {
    let p = project(false);
    let out = success(p.orch().args(["task", "list", "--yaml"]));
    let text = stdout(&out);
    // Bounded: on malformed input the deserializer can keep yielding errors.
    let documents = serde_yaml_ng::Deserializer::from_str(&text)
        .take(100)
        .count();
    assert_eq!(documents, 1, "expected exactly one YAML document:\n{text}");
    assert!(!text.contains('\u{1b}'), "ANSI escapes in --yaml output");
}

#[test]
fn global_yaml_before_the_subcommand_prints_the_same_value() {
    let p = project(false);
    let after = p.yaml(&["task", "list"]);
    let before = parse_yaml(&success(p.orch().args(["--yaml", "task", "list"])));
    assert_eq!(before, after);
}

#[test]
fn yaml_with_quiet_behaves_like_json_with_quiet() {
    // Whatever `--json --quiet` prints, `--yaml --quiet` prints the same value
    // (compared parsed when `--json --quiet` is a document, as text if not).
    let p = project(false);
    let json = success(p.orch().args(["task", "list", "--json", "--quiet"]));
    let yaml = success(p.orch().args(["task", "list", "--yaml", "--quiet"]));
    match serde_json::from_str::<Value>(&stdout(&json)) {
        Ok(json_value) => assert_eq!(parse_yaml(&yaml), json_value),
        Err(_) => assert_eq!(stdout(&yaml), stdout(&json)),
    }
}

/// Write commands cannot be run twice and compared (ids and timestamps move),
/// so compare the *shape*: same keys, same value types, recursively.
fn skeleton(value: &Value) -> Value {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Array(items) => Value::Array(items.iter().map(skeleton).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), skeleton(v))).collect())
        }
    }
}

#[test]
fn write_commands_print_the_same_shape_in_yaml_as_in_json() {
    let p = project(false);
    let agent = p.agent.clone();
    let cases: Vec<Vec<String>> = vec![
        words(&["task", "add", "shape one"]),
        words(&["goal", "add", "shape goal"]),
        words(&["context", "set", "shape-key", "shape-value"]),
        vec!["msg".into(), "send".into(), agent, "shape body".into()],
        words(&[
            "agent",
            "add",
            "gamma",
            "--adapter",
            "shell",
            "--command",
            "true",
        ]),
    ];
    for args in cases {
        // The agent name must be unique per call; vary it for the second run.
        let second: Vec<String> = args
            .iter()
            .map(|a| {
                if a == "gamma" {
                    "delta".to_owned()
                } else {
                    a.clone()
                }
            })
            .collect();
        let json_out = success(p.orch().args(&args).arg("--json"));
        let yaml_out = success(p.orch().args(&second).arg("--yaml"));
        assert_eq!(
            skeleton(&parse_yaml(&yaml_out)),
            skeleton(&parse_json(&json_out)),
            "{args:?}"
        );
    }
}

// ── Errors ─────────────────────────────────────────────────────────────────

/// The failing invocation `args` behaves the same with `--yaml` as with
/// `--json`: same exit code and same stdout, and `--yaml` is not rejected as
/// an unknown flag.
fn assert_error_parity(cwd: &Path, home: &Path, args: &[&str]) {
    let json = orch_in(home, cwd)
        .args(args)
        .arg("--json")
        .output()
        .expect("run");
    let yaml = orch_in(home, cwd)
        .args(args)
        .arg("--yaml")
        .output()
        .expect("run");
    assert!(!json.status.success(), "{args:?} should fail under --json");
    assert_eq!(yaml.status.code(), json.status.code(), "{args:?} exit code");
    assert_eq!(stdout(&yaml), stdout(&json), "{args:?} stdout");
    let err = stderr(&yaml);
    assert!(!err.trim().is_empty(), "{args:?} failed silently");
    assert!(
        !err.contains("unknown option") && !err.contains("unexpected argument"),
        "--yaml was rejected as a flag:\n{err}"
    );
}

/// The error text a failing `--yaml` invocation prints is itself valid YAML,
/// so a consumer piping the whole stream into a YAML parser never chokes.
fn assert_stderr_is_yaml(cwd: &Path, home: &Path, args: &[&str]) {
    let out = orch_in(home, cwd)
        .args(args)
        .arg("--yaml")
        .output()
        .expect("run");
    assert!(!out.status.success(), "{args:?} should fail");
    let err = stderr(&out);
    serde_yaml_ng::from_str::<Value>(&err)
        .unwrap_or_else(|e| panic!("{args:?} stderr is not valid YAML ({e}):\n{err}"));
}

#[test]
fn a_missing_agent_fails_under_yaml_like_under_json() {
    let p = project(false);
    assert_error_parity(&p.root(), p.dir.path(), &["agent", "show", "agt_nope"]);
}

#[test]
fn a_missing_task_fails_under_yaml_like_under_json() {
    let p = project(false);
    assert_error_parity(&p.root(), p.dir.path(), &["task", "show", "tsk_nope"]);
}

#[test]
fn an_uninitialized_project_fails_under_yaml_like_under_json() {
    let dir = scratch();
    assert_error_parity(&dir.path().join("p"), dir.path(), &["task", "list"]);
}

#[test]
fn a_missing_agent_prints_valid_yaml_on_stderr_under_yaml() {
    let p = project(false);
    assert_stderr_is_yaml(&p.root(), p.dir.path(), &["agent", "show", "agt_nope"]);
}

#[test]
fn a_missing_task_prints_valid_yaml_on_stderr_under_yaml() {
    let p = project(false);
    assert_stderr_is_yaml(&p.root(), p.dir.path(), &["task", "show", "tsk_nope"]);
}

#[test]
fn an_uninitialized_project_prints_valid_yaml_on_stderr_under_yaml() {
    // Its message spans two lines ("Not initialized" / "Run: orch init"),
    // which is not a YAML document when printed bare.
    let dir = scratch();
    assert_stderr_is_yaml(&dir.path().join("p"), dir.path(), &["task", "list"]);
}

#[test]
fn yaml_error_output_names_the_problem() {
    let p = project(false);
    let out = p
        .orch()
        .args(["agent", "show", "agt_nope", "--yaml"])
        .output()
        .expect("run");
    assert!(stderr(&out).contains("Agent not found"), "{}", stderr(&out));
}

#[test]
fn json_with_yaml_is_refused_with_a_usage_error_and_no_stdout_whichever_level_each_flag_is_on() {
    // clap's own `conflicts_with` on a global flag only sees two flags at the
    // same subcommand level, so the split-level spellings are the ones that
    // slip through.
    let p = project(false);
    for argv in [
        ["task", "list", "--json", "--yaml"],
        ["task", "list", "--yaml", "--json"],
        ["--json", "task", "list", "--yaml"],
        ["--yaml", "task", "list", "--json"],
        ["--json", "task", "--yaml", "list"],
    ] {
        let out = p.orch().args(argv).output().expect("run");
        assert!(!out.status.success(), "{argv:?} was accepted");
        assert_eq!(stdout(&out), "", "{argv:?} printed on a usage error");
        let err = stderr(&out);
        assert!(
            err.contains("--json") && err.contains("--yaml"),
            "{argv:?}: {err}"
        );
    }
}
