//! `agent|task update|patch` and `config update|patch`: replace or merge-patch
//! a live definition from a JSON or YAML file (or stdin).
//!
//! Everything here drives the real `orch` binary in a throwaway project with a
//! throwaway `HOME`, `PATH=/usr/bin:/bin` (no agent CLIs) and `shell` agents
//! that are never run. Failure cases assert the strongest "nothing happened"
//! evidence the store offers: the entity file, the index and `config.yml` are
//! byte-identical afterwards.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use assert_cmd::prelude::*;
use serde_json::{Value, json};

// ── Harness ────────────────────────────────────────────────────────────────

struct Project {
    dir: tempfile::TempDir,
}

/// Restores write permission on drop so a failed assertion cannot leave an
/// undeletable directory behind.
struct Locked(PathBuf);
impl Drop for Locked {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn project() -> Project {
    let dir = tempfile::Builder::new()
        .prefix("orch-update-patch-")
        .tempdir()
        .expect("tempdir");
    assert!(
        !dir.path()
            .ancestors()
            .any(|d| d.join(".orchestry").is_dir()),
        "the tmpdir has an .orchestry ancestor; point TMPDIR outside any ORCH project"
    );
    fs::create_dir_all(dir.path().join("home")).expect("home");
    fs::create_dir_all(dir.path().join("p")).expect("project dir");
    let p = Project { dir };
    p.ok(&["init", "--name", "up", "--adapter", "shell"]);
    p
}

impl Project {
    fn root(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn store(&self) -> PathBuf {
        self.root().join(".orchestry")
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("orch").expect("the orch binary");
        command
            .current_dir(self.root())
            .env("HOME", self.dir.path().join("home"))
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.dir.path().join("home/global.yml"),
            )
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin")
            .env_remove("RUST_LOG")
            .env_remove("ORCHESTRY_DEBUG");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run orch")
    }

    fn run_stdin(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn orch");
        // A command that rejects its input may exit before reading all of an
        // oversized body; a broken pipe there is expected.
        let _ = child.stdin.take().expect("stdin").write_all(stdin);
        child.wait_with_output().expect("wait orch")
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert_success(&out, args);
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        serde_json::from_slice(&self.ok(&all).stdout).expect("json output")
    }

    /// Write `body` under the project dir (outside `.orchestry`) and return its path.
    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::write(&path, body).expect("write input");
        path
    }

    fn agent(&self, name: &str) -> String {
        self.json(&[
            "agent",
            "add",
            name,
            "--adapter",
            "shell",
            "--command",
            "true",
            "--role",
            "old role",
        ])["id"]
            .as_str()
            .expect("agent id")
            .to_owned()
    }

    fn task(&self, title: &str) -> String {
        self.json(&[
            "task",
            "add",
            title,
            "--description",
            "old body",
            "--priority",
            "2",
            "--labels",
            "a,b",
        ])["id"]
            .as_str()
            .expect("task id")
            .to_owned()
    }

    fn entity_path(&self, kind: &str, id: &str) -> PathBuf {
        self.store().join(kind).join(format!("{id}.yml"))
    }

    /// Bytes and mtimes of every file an update could touch for `kind`.
    fn snapshot(&self, kind: &str) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
        let mut files: Vec<PathBuf> = fs::read_dir(self.store().join(kind))
            .expect("read store dir")
            .map(|e| e.expect("entry").path())
            .collect();
        files.sort();
        files
            .into_iter()
            .map(|p| {
                let bytes = fs::read(&p).expect("read");
                let mtime = fs::metadata(&p).expect("stat").modified().expect("mtime");
                (p, bytes, mtime)
            })
            .collect()
    }

    fn config_bytes(&self) -> Vec<u8> {
        fs::read(self.store().join("config.yml")).expect("config.yml")
    }

    fn config_value(&self) -> Value {
        serde_yaml_ng::from_slice(&self.config_bytes()).expect("config yaml")
    }

    /// Rewrite an entity file in place (a stand-in for state the CLI cannot
    /// reach from outside: a running agent, an imported unnormalised name).
    fn edit_entity(&self, kind: &str, id: &str, from: &str, to: &str) {
        let path = self.entity_path(kind, id);
        let text = fs::read_to_string(&path).expect("read entity");
        assert!(text.contains(from), "{from:?} not in {text}");
        fs::write(&path, text.replacen(from, to, 1)).expect("write entity");
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_success(out: &Output, args: &[&str]) {
    assert!(
        out.status.success(),
        "{args:?} exited {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        stdout(out),
        stderr(out)
    );
}

/// A rejection is a usage/validation failure (exit 2) that says nothing was updated.
fn assert_rejected(out: &Output, needle: &str) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2\nstdout: {}\nstderr: {}",
        stdout(out),
        stderr(out)
    );
    assert!(
        stderr(out).contains(needle),
        "stderr lacks {needle:?}: {}",
        stderr(out)
    );
    assert!(
        !stdout(out).contains("Updated"),
        "a rejected command reported success: {}",
        stdout(out)
    );
}

// ── agent update ───────────────────────────────────────────────────────────

#[test]
fn agent_update_from_json_file_replaces_the_definition() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file(
        "def.json",
        r#"{"name":"alpha2","adapter":"shell","role":"new role","config":{"command":"echo hi"}}"#,
    );
    p.ok(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["agent", "show", &id]);
    assert_eq!(shown["name"], "alpha2");
    assert_eq!(shown["role"], "new role");
    assert_eq!(shown["config"], json!({"command": "echo hi"}));
}

#[test]
fn agent_update_from_yaml_file_replaces_the_definition() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file(
        "def.yaml",
        "name: from-yaml\nadapter: shell\nconfig:\n  command: echo yaml\n  max_turns: 7\n",
    );
    p.ok(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["agent", "show", &id]);
    assert_eq!(shown["name"], "from-yaml");
    assert_eq!(shown["config"]["max_turns"], 7);
}

#[test]
fn agent_update_omitting_an_optional_field_clears_it() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file(
        "def.json",
        r#"{"name":"alpha","adapter":"shell","config":{"command":"true"}}"#,
    );
    p.ok(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["agent", "show", &id]);
    // `role` and the `agent add` defaults (max_turns, ...) were not restated.
    assert!(shown.get("role").is_none(), "role survived: {shown}");
    assert!(shown["config"].get("max_turns").is_none(), "{shown}");
}

#[test]
fn agent_update_reads_yaml_from_stdin_when_the_file_is_dash() {
    let p = project();
    let id = p.agent("alpha");
    let body = b"name: via-stdin\nadapter: shell\nconfig:\n  command: echo stdin\n";
    let out = p.run_stdin(&["agent", "update", &id, "-f", "-"], body);
    assert_success(&out, &["agent", "update", "-f", "-"]);
    assert_eq!(p.json(&["agent", "show", &id])["name"], "via-stdin");
}

#[test]
fn agent_update_stdin_starting_with_a_brace_is_parsed_as_json() {
    let p = project();
    let id = p.agent("alpha");
    let body = br#"{"name":"braced","adapter":"shell","config":{"command":"x"}}"#;
    let out = p.run_stdin(&["agent", "update", &id, "-f", "-"], body);
    assert_success(&out, &["agent", "update", "-f", "-"]);
    assert_eq!(p.json(&["agent", "show", &id])["name"], "braced");
}

#[test]
fn agent_update_accepts_its_own_full_show_output_as_input() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let shown = p.ok(&["agent", "show", &id, "--json"]).stdout;
    let file = p.file("show.json", shown);
    p.ok(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    // Equal read-only echoes (id, status, stats, ...) are ignored; the
    // definition did not change, so nothing was written.
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_from_yaml_show_output_round_trips() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let shown = p.ok(&["agent", "show", &id, "--yaml"]).stdout;
    let file = p.file("show.yaml", shown);
    p.ok(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_rejects_a_changed_read_only_echo() {
    let p = project();
    let id = p.agent("alpha");
    let mut shown: Value = p.json(&["agent", "show", &id]);
    shown["status"] = json!("running");
    let file = p.file("show.json", shown.to_string());
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "status: read-only field");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_rejects_explicit_nulls() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file(
        "def.json",
        r#"{"name":"a","adapter":"shell","role":null,"config":{"command":"x"}}"#,
    );
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "null is not allowed");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_requires_name_adapter_and_config() {
    for (case, body) in [
        ("no name", r#"{"adapter":"shell","config":{"command":"x"}}"#),
        ("no adapter", r#"{"name":"n","config":{"command":"x"}}"#),
        ("no config", r#"{"name":"n","adapter":"shell"}"#),
        (
            "blank name",
            r#"{"name":"  ","adapter":"shell","config":{"command":"x"}}"#,
        ),
    ] {
        let p = project();
        let id = p.agent("alpha");
        let file = p.file("def.json", body);
        let before = p.snapshot("agents");
        let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, "required");
        assert_eq!(p.snapshot("agents"), before, "{case}");
    }
}

#[test]
fn agent_update_to_shell_without_a_command_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file(
        "def.json",
        r#"{"name":"alpha","adapter":"shell","config":{}}"#,
    );
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "Shell agents require a command");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_to_another_agents_name_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    p.agent("beta");
    let file = p.file(
        "def.json",
        r#"{"name":"beta","adapter":"shell","config":{"command":"x"}}"#,
    );
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "another agent already has this name");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_with_zero_runtime_limit_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file(
        "def.json",
        r#"{"name":"alpha","adapter":"shell","config":{"command":"x","max_turns":0}}"#,
    );
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "runtime limits must be positive");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_of_a_running_agent_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    p.edit_entity("agents", &id, "status: idle", "status: running");
    let file = p.file(
        "def.json",
        r#"{"name":"renamed","adapter":"shell","config":{"command":"x"}}"#,
    );
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "running or remote");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_patch_of_a_running_agent_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    p.edit_entity("agents", &id, "status: idle", "status: running");
    let file = p.file("patch.json", r#"{"role":"new"}"#);
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "running or remote");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_mutation_of_a_remote_id_is_refused_without_reading_the_store() {
    let p = project();
    p.agent("alpha");
    let file = p.file("patch.json", r#"{"role":"new"}"#);
    let before = p.snapshot("agents");
    for verb in ["update", "patch"] {
        let out = p.run(&["agent", verb, "agt_x@peer", "-f", file.to_str().unwrap()]);
        assert!(out.status.code() == Some(2), "{verb}: {}", stderr(&out));
    }
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_update_of_an_unknown_id_fails_without_creating_it() {
    let p = project();
    let file = p.file(
        "def.json",
        r#"{"name":"ghost","adapter":"shell","config":{"command":"x"}}"#,
    );
    let out = p.run(&["agent", "update", "agt_nope", "-f", file.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!p.entity_path("agents", "agt_nope").exists());
}

#[test]
fn agent_update_that_changes_nothing_keeps_imported_whitespace_and_timestamps() {
    let p = project();
    let id = p.agent("alpha");
    // An imported definition the normal write path would trim.
    p.edit_entity("agents", &id, "name: alpha", "name: '  padded  '");
    let before = p.snapshot("agents");
    let file = p.file(
        "def.json",
        json!({
            "name": "  padded  ",
            "adapter": "shell",
            "role": "old role",
            "config": p.json(&["agent", "show", &id])["config"],
        })
        .to_string(),
    );
    let out = p.run(&["agent", "update", &id, "-f", file.to_str().unwrap()]);
    assert_success(&out, &["agent", "update"]);
    assert_eq!(p.snapshot("agents"), before);
}

// ── agent patch ────────────────────────────────────────────────────────────

#[test]
fn agent_patch_changes_only_the_named_field() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.json(&["agent", "show", &id]);
    let file = p.file("patch.json", r#"{"role":"patched"}"#);
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    let mut expected = before;
    expected["role"] = json!("patched");
    assert_eq!(p.json(&["agent", "show", &id]), expected);
}

#[test]
fn agent_patch_null_removes_an_optional_field() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.json", r#"{"role":null,"config":{"max_turns":null}}"#);
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["agent", "show", &id]);
    assert!(shown.get("role").is_none(), "{shown}");
    assert!(shown["config"].get("max_turns").is_none(), "{shown}");
    assert_eq!(shown["config"]["command"], "true", "sibling kept");
}

#[test]
fn agent_patch_replaces_arrays_instead_of_merging_them() {
    let p = project();
    let id = p.agent("alpha");
    let first = p.file("one.json", r#"{"config":{"skills":["a","b","c"]}}"#);
    p.ok(&["agent", "patch", &id, "-f", first.to_str().unwrap()]);
    let second = p.file("two.json", r#"{"config":{"skills":["z"]}}"#);
    p.ok(&["agent", "patch", &id, "-f", second.to_str().unwrap()]);
    assert_eq!(
        p.json(&["agent", "show", &id])["config"]["skills"],
        json!(["z"])
    );
}

#[test]
fn agent_patch_merges_nested_env_objects_recursively() {
    let p = project();
    let id = p.agent("alpha");
    let first = p.file("one.json", r#"{"config":{"env":{"A":"1","B":"2"}}}"#);
    p.ok(&["agent", "patch", &id, "-f", first.to_str().unwrap()]);
    let second = p.file("two.json", r#"{"config":{"env":{"B":null,"C":"3"}}}"#);
    p.ok(&["agent", "patch", &id, "-f", second.to_str().unwrap()]);
    assert_eq!(
        p.json(&["agent", "show", &id])["config"]["env"],
        json!({"A": "1", "C": "3"})
    );
}

#[test]
fn agent_patch_from_yaml_file_applies() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.yml", "role: yaml role\nconfig:\n  max_turns: 9\n");
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["agent", "show", &id]);
    assert_eq!(shown["role"], "yaml role");
    assert_eq!(shown["config"]["max_turns"], 9);
}

#[test]
fn agent_patch_from_stdin_applies() {
    let p = project();
    let id = p.agent("alpha");
    let out = p.run_stdin(&["agent", "patch", &id, "-f", "-"], b"role: from stdin\n");
    assert_success(&out, &["agent", "patch", "-f", "-"]);
    assert_eq!(p.json(&["agent", "show", &id])["role"], "from stdin");
}

#[test]
fn agent_patch_rejects_read_only_keys_even_when_equal() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    for key in ["id", "status", "stats", "capabilities"] {
        let current = p.json(&["agent", "show", &id]);
        let patch = json!({ key: current[key] });
        let file = p.file("patch.json", patch.to_string());
        let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, &format!("{key}: read-only field"));
    }
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_patch_rejects_an_unknown_null_instead_of_dropping_it() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    for body in [r#"{"typo":null}"#, r#"{"config":{"typo":null}}"#] {
        let file = p.file("patch.json", body);
        let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, "unknown field");
    }
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_patch_cannot_remove_required_fields() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    for body in [
        r#"{"name":null}"#,
        r#"{"adapter":null}"#,
        r#"{"config":null}"#,
    ] {
        let file = p.file("patch.json", body);
        let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, "required");
    }
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_patch_with_a_wrongly_typed_config_field_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    for body in [
        r#"{"config":{"max_turns":"many"}}"#,
        r#"{"config":{"approval_policy":"sometimes"}}"#,
        r#"{"role":5}"#,
    ] {
        let file = p.file("patch.json", body);
        let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, "");
    }
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_empty_patch_changes_nothing() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let file = p.file("patch.json", "{}");
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn agent_patch_preserves_runtime_statistics() {
    let p = project();
    let id = p.agent("alpha");
    p.edit_entity("agents", &id, "tasks_completed: 0", "tasks_completed: 7");
    let file = p.file("patch.json", r#"{"role":"x"}"#);
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(
        p.json(&["agent", "show", &id])["stats"]["tasks_completed"],
        7
    );
}

#[test]
fn agent_mutation_prints_the_updated_entity_in_the_requested_output_format() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.yaml", "role: out\n");
    let as_json = p.ok(&[
        "agent",
        "patch",
        &id,
        "-f",
        file.to_str().unwrap(),
        "--json",
    ]);
    let as_yaml = p.ok(&[
        "agent",
        "patch",
        &id,
        "-f",
        file.to_str().unwrap(),
        "--yaml",
    ]);
    let json: Value = serde_json::from_slice(&as_json.stdout).expect("json");
    let yaml: Value = serde_yaml_ng::from_slice(&as_yaml.stdout).expect("yaml");
    assert_eq!(json, yaml);
}

#[test]
fn agent_mutation_output_format_flag_does_not_choose_the_input_parser() {
    let p = project();
    let id = p.agent("alpha");
    // YAML input with --json output, and JSON input with --yaml output.
    let y = p.file("p.yaml", "role: from yaml\n");
    p.ok(&["agent", "patch", &id, "-f", y.to_str().unwrap(), "--json"]);
    let j = p.file("p.json", r#"{"role":"from json"}"#);
    p.ok(&["agent", "patch", &id, "-f", j.to_str().unwrap(), "--yaml"]);
    assert_eq!(p.json(&["agent", "show", &id])["role"], "from json");
}

// ── input selection and rejection ──────────────────────────────────────────

#[test]
fn input_format_flag_overrides_the_file_extension() {
    let p = project();
    let id = p.agent("alpha");
    // Block-style YAML in a `.json` file: only the override can parse it.
    let file = p.file("patch.json", "role: forced yaml\n");
    p.ok(&[
        "agent",
        "patch",
        &id,
        "-f",
        file.to_str().unwrap(),
        "--input-format",
        "yaml",
    ]);
    assert_eq!(p.json(&["agent", "show", &id])["role"], "forced yaml");
}

#[test]
fn json_extension_selects_the_json_parser_with_no_yaml_fallback() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.json", "role: not json\n");
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "invalid JSON");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn explicit_json_input_format_does_not_fall_back_to_yaml() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.yaml", "role: yaml only\n");
    let before = p.snapshot("agents");
    let out = p.run(&[
        "agent",
        "patch",
        &id,
        "-f",
        file.to_str().unwrap(),
        "--input-format",
        "json",
    ]);
    assert_rejected(&out, "invalid JSON");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn unknown_extension_with_leading_brace_is_sniffed_as_json() {
    let p = project();
    let id = p.agent("alpha");
    // A surrogate-pair escape is valid JSON but not valid YAML, so only the
    // JSON parser accepts this document.
    let file = p.file("patch.txt", "  \n{\"role\":\"\\ud83d\\ude00 sniffed\"}");
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.json(&["agent", "show", &id])["role"], "😀 sniffed");
}

#[test]
fn unknown_extension_without_a_brace_is_sniffed_as_yaml() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.txt", "role: sniffed yaml\n");
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.json(&["agent", "show", &id])["role"], "sniffed yaml");
}

#[test]
fn mutation_commands_require_the_file_flag() {
    let p = project();
    let id = p.agent("alpha");
    for args in [
        vec!["agent", "update", id.as_str()],
        vec!["agent", "patch", id.as_str()],
        vec!["task", "update", "tsk_x"],
        vec!["task", "patch", "tsk_x"],
        vec!["config", "update"],
        vec!["config", "patch"],
    ] {
        let out = p.run(&args);
        assert!(!out.status.success(), "{args:?}: {}", stdout(&out));
        assert!(
            stderr(&out).contains("--file"),
            "{args:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_missing_input_file_is_an_io_failure_that_changes_nothing() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let out = p.run(&["agent", "patch", &id, "-f", "/nonexistent/nope.json"]);
    assert!(!out.status.success());
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn a_directory_given_as_the_input_file_is_refused() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let dir = p.dir.path().to_str().unwrap().to_owned();
    let out = p.run(&["agent", "patch", &id, "-f", &dir]);
    assert_rejected(&out, "regular input file");
    assert_eq!(p.snapshot("agents"), before);
}

/// Inputs every mutation command must reject before touching any state.
fn hostile_inputs() -> Vec<(&'static str, &'static str, Vec<u8>, &'static str)> {
    let deep_json = format!("{}1{}", "[".repeat(80), "]".repeat(80));
    let deep_object = format!("{{\"x\":{}1{}}}", "{\"a\":".repeat(70), "}".repeat(70));
    let oversized = format!("{{\"role\":\"{}\"}}", "x".repeat(1024 * 1024 + 16));
    vec![
        ("empty json", "json", Vec::new(), "invalid JSON"),
        ("truncated json", "json", b"{\"role\":".to_vec(), "invalid JSON"),
        (
            "duplicate json key",
            "json",
            br#"{"role":"a","role":"b"}"#.to_vec(),
            "invalid JSON",
        ),
        (
            "two json documents",
            "json",
            br#"{"role":"a"}{"role":"b"}"#.to_vec(),
            "exactly one JSON document",
        ),
        (
            "json root array",
            "json",
            br#"[{"role":"a"}]"#.to_vec(),
            "expected an object",
        ),
        (
            "json root scalar",
            "json",
            b"\"role\"".to_vec(),
            "expected an object",
        ),
        (
            "json nested too deep",
            "json",
            deep_object.into_bytes(),
            "",
        ),
        ("json array root too deep", "json", deep_json.into_bytes(), ""),
        (
            "oversized json",
            "json",
            oversized.into_bytes(),
            "exceeds 1 MiB",
        ),
        (
            "duplicate yaml key",
            "yaml",
            b"role: a\nrole: b\n".to_vec(),
            "",
        ),
        (
            "explicit yaml tag",
            "yaml",
            b"role: !!str x\n".to_vec(),
            "tags are not allowed",
        ),
        (
            "two yaml documents",
            "yaml",
            b"role: a\n---\nrole: b\n".to_vec(),
            "",
        ),
        (
            "yaml non-string key",
            "yaml",
            b"1: a\n".to_vec(),
            "",
        ),
        (
            "yaml infinity",
            "yaml",
            b"config:\n  max_turns: .inf\n".to_vec(),
            "",
        ),
        ("yaml root sequence", "yaml", b"- role\n".to_vec(), ""),
        (
            "yaml alias bomb",
            "yaml",
            b"a: &a [x, x, x, x]\nb: &b [*a, *a, *a, *a, *a, *a, *a, *a]\nc: &c [*b, *b, *b, *b, *b, *b, *b, *b]\nd: &d [*c, *c, *c, *c, *c, *c, *c, *c]\ne: &e [*d, *d, *d, *d, *d, *d, *d, *d]\nf: &f [*e, *e, *e, *e, *e, *e, *e, *e]\ng: &g [*f, *f, *f, *f, *f, *f, *f, *f]\nh: &h [*g, *g, *g, *g, *g, *g, *g, *g]\nrole: x\n"
                .to_vec(),
            "",
        ),
    ]
}

#[test]
fn hostile_input_is_rejected_by_agent_patch_with_no_state_change() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    for (case, format, body, needle) in hostile_inputs() {
        let file = p.file(&format!("hostile.{format}"), &body);
        let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{case}: {}", stderr(&out));
        assert!(stderr(&out).contains(needle), "{case}: {}", stderr(&out));
        assert_eq!(p.snapshot("agents"), before, "{case} mutated the store");
    }
}

#[test]
fn hostile_input_is_rejected_by_task_update_with_no_state_change() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    for (case, format, body, _) in hostile_inputs() {
        let file = p.file(&format!("hostile.{format}"), &body);
        let out = p.run(&["task", "update", &id, "-f", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{case}: {}", stderr(&out));
        assert_eq!(p.snapshot("tasks"), before, "{case} mutated the store");
    }
}

#[test]
fn hostile_input_is_rejected_by_config_patch_with_no_state_change() {
    let p = project();
    let before = p.config_bytes();
    for (case, format, body, _) in hostile_inputs() {
        let file = p.file(&format!("hostile.{format}"), &body);
        let out = p.run(&["config", "patch", "-f", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{case}: {}", stderr(&out));
        assert_eq!(p.config_bytes(), before, "{case} mutated config.yml");
    }
}

#[test]
fn oversized_stdin_is_rejected_without_reading_it_all_into_a_mutation() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let body = format!("role: {}\n", "x".repeat(2 * 1024 * 1024));
    let out = p.run_stdin(&["agent", "patch", &id, "-f", "-"], body.as_bytes());
    assert_rejected(&out, "<stdin>: document exceeds 1 MiB");
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn a_document_at_the_size_limit_is_not_rejected_for_size() {
    let p = project();
    let id = p.agent("alpha");
    // Exactly 1 MiB of valid JSON: accepted by the reader, then refused later
    // by the persisted-size guard — the point is the error is not "exceeds 1 MiB".
    let prefix = r#"{"role":""#;
    let suffix = r#""}"#;
    let fill = "x".repeat(1024 * 1024 - prefix.len() - suffix.len());
    let file = p.file("edge.json", format!("{prefix}{fill}{suffix}"));
    let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert!(
        !stderr(&out).contains("document exceeds 1 MiB"),
        "exact-limit input rejected as oversized: {}",
        stderr(&out)
    );
}

#[test]
fn error_messages_name_the_input_file_and_never_echo_its_content() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("secret.json", r#"{"role":"s3cr3t-token","role":"again"}"#);
    let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert!(stderr(&out).contains("secret.json"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("s3cr3t-token"), "{}", stderr(&out));
}

// ── publication failure and concurrency ────────────────────────────────────

#[test]
fn agent_patch_that_cannot_publish_reports_failure_and_leaves_the_store_intact() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    let dir = p.store().join("agents");
    let _restore = Locked(dir.clone());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");
    let file = p.file("patch.json", r#"{"role":"blocked"}"#);
    let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(!stdout(&out).contains("Updated"), "{}", stdout(&out));
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn task_update_that_cannot_publish_reports_failure_and_leaves_the_store_intact() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    let dir = p.store().join("tasks");
    let _restore = Locked(dir.clone());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");
    let file = p.file(
        "def.json",
        r#"{"title":"x","description":"y","priority":1}"#,
    );
    let out = p.run(&["task", "update", &id, "-f", file.to_str().unwrap()]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(!stdout(&out).contains("Updated"), "{}", stdout(&out));
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn config_patch_that_cannot_publish_reports_failure_and_leaves_config_intact() {
    let p = project();
    let before = p.config_bytes();
    let dir = p.store();
    let _restore = Locked(dir.clone());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");
    let file = p.file("patch.json", r#"{"project":{"description":"blocked"}}"#);
    let out = p.run(&["config", "patch", "-f", file.to_str().unwrap()]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(!stdout(&out).contains("Updated"), "{}", stdout(&out));
    assert_eq!(p.config_bytes(), before);
}

#[test]
fn a_held_definition_lock_makes_a_mutation_fail_cleanly_without_changes() {
    let p = project();
    let id = p.agent("alpha");
    let before = p.snapshot("agents");
    // The same advisory lock the commands take; held for the whole run.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let guard = runtime
        .block_on(orch_infra::storage::fs_utils::mutation_lock(
            &p.store().join(".definition.lock"),
        ))
        .expect("take the lock");
    let file = p.file("patch.json", r#"{"role":"locked out"}"#);
    // The product bounds its wait (5 s); no timing is asserted here.
    let out = p.run(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    drop(guard);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(!stdout(&out).contains("Updated"), "{}", stdout(&out));
    assert_eq!(p.snapshot("agents"), before);
}

#[test]
fn a_mutation_succeeds_once_the_lock_is_released() {
    let p = project();
    let id = p.agent("alpha");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let guard = runtime
        .block_on(orch_infra::storage::fs_utils::mutation_lock(
            &p.store().join(".definition.lock"),
        ))
        .expect("take the lock");
    drop(guard);
    let file = p.file("patch.json", r#"{"role":"after release"}"#);
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.json(&["agent", "show", &id])["role"], "after release");
}

#[test]
fn definition_lock_file_is_owner_only() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.json", r#"{"role":"x"}"#);
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    let mode = fs::metadata(p.store().join(".definition.lock"))
        .expect("lock file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn updated_agent_file_is_owner_only() {
    let p = project();
    let id = p.agent("alpha");
    let file = p.file("patch.json", r#"{"role":"x"}"#);
    p.ok(&["agent", "patch", &id, "-f", file.to_str().unwrap()]);
    let mode = fs::metadata(p.entity_path("agents", &id))
        .expect("agent file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

// ── task update / patch ────────────────────────────────────────────────────

#[test]
fn task_update_replaces_the_definition_and_clears_omitted_labels() {
    let p = project();
    let id = p.task("old title");
    let file = p.file(
        "def.json",
        r#"{"title":"new title","description":"new body","priority":1}"#,
    );
    p.ok(&["task", "update", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["task", "show", &id]);
    assert_eq!(shown["title"], "new title");
    assert_eq!(shown["description"], "new body");
    assert_eq!(shown["priority"], 1);
    assert_eq!(shown["labels"], json!([]));
}

#[test]
fn task_update_from_yaml_stdin_applies() {
    let p = project();
    let id = p.task("t");
    let body =
        b"title: yaml title\ndescription: |\n  line one\n  line two\npriority: 4\nlabels: [x, y]\n";
    let out = p.run_stdin(&["task", "update", &id, "-f", "-"], body);
    assert_success(&out, &["task", "update", "-f", "-"]);
    let shown = p.json(&["task", "show", &id]);
    // The task service trims descriptions on every write path.
    assert_eq!(shown["description"], "line one\nline two");
    assert_eq!(shown["labels"], json!(["x", "y"]));
}

#[test]
fn task_update_requires_title_description_and_priority() {
    for (case, body) in [
        ("no title", r#"{"description":"d","priority":2}"#),
        ("no description", r#"{"title":"t","priority":2}"#),
        ("no priority", r#"{"title":"t","description":"d"}"#),
        (
            "blank title",
            r#"{"title":" ","description":"d","priority":2}"#,
        ),
    ] {
        let p = project();
        let id = p.task("t");
        let before = p.snapshot("tasks");
        let file = p.file("def.json", body);
        let out = p.run(&["task", "update", &id, "-f", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{case}: {}", stderr(&out));
        assert_eq!(p.snapshot("tasks"), before, "{case}");
    }
}

#[test]
fn task_mutation_rejects_priority_outside_one_to_four() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    for priority in ["0", "5", "-1", "2.5", "\"high\""] {
        let body = format!(r#"{{"priority":{priority}}}"#);
        let file = p.file("patch.json", body);
        let out = p.run(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, "priority");
    }
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_mutation_rejects_non_string_labels() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    for body in [
        r#"{"labels":[1]}"#,
        r#"{"labels":"a"}"#,
        r#"{"labels":{"a":1}}"#,
    ] {
        let file = p.file("patch.json", body);
        let out = p.run(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, "labels");
    }
    assert_eq!(p.snapshot("tasks"), before);
}

fn assert_task_mutation_rejects_reserved_label(verb: &str, label: &str) {
    let p = project();
    let id = p.task("local task");
    let before = p.snapshot("tasks");
    let config_before = p.config_bytes();
    let mut body = if verb == "update" {
        json!({"title": "changed", "description": "changed body", "priority": 4})
    } else {
        json!({"title": "changed"})
    };
    body["labels"] = json!(["benign", label]);
    let out = p.run_stdin(&["task", verb, &id, "-f", "-"], body.to_string().as_bytes());
    assert_rejected(&out, "labels");
    assert_eq!(p.snapshot("tasks"), before);
    assert_eq!(p.config_bytes(), config_before);
}

#[test]
fn task_patch_rejects_remote_control_labels_without_changing_the_store() {
    for label in ["remote-ctl:x", "remote-ctl:"] {
        assert_task_mutation_rejects_reserved_label("patch", label);
    }
}

#[test]
fn task_patch_rejects_execution_labels_without_changing_the_store() {
    for label in ["exec:x", "exec:"] {
        assert_task_mutation_rejects_reserved_label("patch", label);
    }
}

#[test]
fn task_update_rejects_remote_control_labels_without_changing_the_store() {
    for label in ["remote-ctl:x", "remote-ctl:"] {
        assert_task_mutation_rejects_reserved_label("update", label);
    }
}

#[test]
fn task_update_rejects_execution_labels_without_changing_the_store() {
    for label in ["exec:x", "exec:"] {
        assert_task_mutation_rejects_reserved_label("update", label);
    }
}

#[test]
fn task_mutations_accept_benign_labels_including_similar_names() {
    let p = project();
    let id = p.task("local task");
    let labels = json!([
        "ordinary",
        "remote-ctl",
        "exec",
        "remote-ctl-other:x",
        "my-exec:x"
    ]);
    let patch = p.file("patch.json", json!({"labels": labels}).to_string());
    p.ok(&["task", "patch", &id, "-f", patch.to_str().unwrap()]);
    assert_eq!(p.json(&["task", "show", &id])["labels"], labels);

    let update = p.file(
        "definition.yaml",
        "title: updated\ndescription: body\npriority: 2\nlabels: [ordinary, remote-ctl, exec, remote-ctl-other:x, my-exec:x]\n",
    );
    p.ok(&["task", "update", &id, "-f", update.to_str().unwrap()]);
    assert_eq!(p.json(&["task", "show", &id])["labels"], labels);
}

#[test]
fn task_update_accepts_its_own_full_show_output() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    let shown = p.ok(&["task", "show", &id, "--json"]).stdout;
    let file = p.file("show.json", shown);
    p.ok(&["task", "update", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_update_rejects_a_changed_status_echo() {
    let p = project();
    let id = p.task("t");
    let mut shown = p.json(&["task", "show", &id]);
    shown["status"] = json!("done");
    let file = p.file("show.json", shown.to_string());
    let before = p.snapshot("tasks");
    let out = p.run(&["task", "update", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "status: read-only field");
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_patch_rejects_read_only_keys_even_when_equal() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    for key in ["id", "status", "attempts", "created_at", "depends_on"] {
        let current = p.json(&["task", "show", &id]);
        let file = p.file("patch.json", json!({ key: current[key] }).to_string());
        let out = p.run(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_rejected(&out, &format!("{key}: read-only field"));
    }
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_patch_rejects_unknown_nulls_and_nulls_on_required_fields() {
    let p = project();
    let id = p.task("t");
    let before = p.snapshot("tasks");
    for body in [
        r#"{"typo":null}"#,
        r#"{"title":null}"#,
        r#"{"description":null}"#,
        r#"{"priority":null}"#,
    ] {
        let file = p.file("patch.json", body);
        let out = p.run(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{body}: {}", stderr(&out));
    }
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_patch_changes_one_field_and_keeps_the_rest() {
    let p = project();
    let id = p.task("keep me");
    let file = p.file("patch.json", r#"{"priority":4}"#);
    p.ok(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    let shown = p.json(&["task", "show", &id]);
    assert_eq!(shown["priority"], 4);
    assert_eq!(shown["title"], "keep me");
    assert_eq!(shown["labels"], json!(["a", "b"]));
}

#[test]
fn task_patch_replaces_labels_wholesale() {
    let p = project();
    let id = p.task("t");
    let file = p.file("patch.yaml", "labels: [only]\n");
    p.ok(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.json(&["task", "show", &id])["labels"], json!(["only"]));
}

#[test]
fn task_patch_null_labels_clears_them() {
    let p = project();
    let id = p.task("t");
    let file = p.file("patch.json", r#"{"labels":null}"#);
    p.ok(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.json(&["task", "show", &id])["labels"], json!([]));
}

#[test]
fn task_patch_does_not_append_or_touch_attachments() {
    let p = project();
    let id = p.task("t");
    let file = p.file("patch.json", r#"{"attachments":[]}"#);
    let before = p.snapshot("tasks");
    let out = p.run(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "attachments: read-only field");
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_mutation_of_an_in_progress_task_is_refused() {
    let p = project();
    let id = p.task("t");
    p.edit_entity("tasks", &id, "status: todo", "status: in_progress");
    let file = p.file("patch.json", r#"{"title":"changed"}"#);
    let before = p.snapshot("tasks");
    for verb in ["patch", "update"] {
        let out = p.run(&["task", verb, &id, "-f", file.to_str().unwrap()]);
        assert!(!out.status.success(), "{verb}: {}", stdout(&out));
    }
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_mutation_of_a_remote_controlled_task_is_refused() {
    let p = project();
    let id = p.task("t");
    p.edit_entity("tasks", &id, "- a\n", "- remote-ctl:controller\n");
    let file = p.file("patch.json", r#"{"title":"changed"}"#);
    let before = p.snapshot("tasks");
    let out = p.run(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "remote-controlled");
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_mutation_that_changes_nothing_keeps_bytes_and_updated_at() {
    let p = project();
    let id = p.task("t");
    p.edit_entity(
        "tasks",
        &id,
        "description: old body",
        "description: '  padded body  '",
    );
    let before = p.snapshot("tasks");
    let current = p.json(&["task", "show", &id]);
    let same = json!({
        "title": current["title"],
        "description": current["description"],
        "priority": current["priority"],
        "labels": current["labels"],
    });
    let file = p.file("same.json", same.to_string());
    p.ok(&["task", "update", &id, "-f", file.to_str().unwrap()]);
    assert_eq!(p.snapshot("tasks"), before);
}

#[test]
fn task_mutation_that_changes_something_advances_updated_at() {
    let p = project();
    let id = p.task("t");
    let before = p.json(&["task", "show", &id])["updated_at"].clone();
    let file = p.file("patch.json", r#"{"title":"changed"}"#);
    p.ok(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    let after = p.json(&["task", "show", &id])["updated_at"].clone();
    assert_ne!(before, after);
}

#[test]
fn task_mutation_of_an_unknown_id_fails_without_creating_it() {
    let p = project();
    let file = p.file(
        "def.json",
        r#"{"title":"x","description":"y","priority":1}"#,
    );
    let out = p.run(&["task", "update", "tsk_nope", "-f", file.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!p.entity_path("tasks", "tsk_nope").exists());
}

#[test]
fn task_mutation_keeps_unicode_and_yaml_special_text_intact() {
    let p = project();
    let id = p.task("t");
    for title in ["null", "yes", "key: value", "emoji 🚀 ünï", "line1\nline2"] {
        let body = json!({"title": title, "description": "d", "priority": 2});
        let file = p.file("def.json", body.to_string());
        p.ok(&["task", "update", &id, "-f", file.to_str().unwrap()]);
        assert_eq!(p.json(&["task", "show", &id])["title"], title);
    }
}

// ── config update / patch ──────────────────────────────────────────────────

#[test]
fn config_patch_sets_a_nested_key_and_keeps_the_others() {
    let p = project();
    let before = p.config_value();
    let file = p.file(
        "patch.json",
        r#"{"scheduling":{"max_concurrent_agents":3}}"#,
    );
    p.ok(&["config", "patch", "-f", file.to_str().unwrap()]);
    let after = p.config_value();
    assert_eq!(after["scheduling"]["max_concurrent_agents"], 3);
    assert_eq!(
        after["scheduling"]["poll_interval_ms"],
        before["scheduling"]["poll_interval_ms"]
    );
    assert_eq!(after["project"], before["project"]);
}

#[test]
fn config_patch_from_yaml_stdin_applies() {
    let p = project();
    let out = p.run_stdin(
        &["config", "patch", "-f", "-"],
        b"project:\n  description: from stdin\n",
    );
    assert_success(&out, &["config", "patch", "-f", "-"]);
    assert_eq!(p.config_value()["project"]["description"], "from stdin");
}

#[test]
fn config_patch_null_on_a_default_backed_key_removes_it_and_get_shows_the_default() {
    let p = project();
    let set = p.file("set.json", r#"{"scheduling":{"max_concurrent_agents":3}}"#);
    p.ok(&["config", "patch", "-f", set.to_str().unwrap()]);
    let clear = p.file(
        "clear.json",
        r#"{"scheduling":{"max_concurrent_agents":null}}"#,
    );
    // Complete-ness is only required of `update`; a patch may drop a key whose
    // default then shows through.
    let out = p.run(&["config", "patch", "-f", clear.to_str().unwrap()]);
    assert_success(&out, &["config", "patch"]);
    assert!(
        p.config_value()["scheduling"]
            .get("max_concurrent_agents")
            .is_none()
    );
    let got = p.json(&["config", "get", "scheduling.max_concurrent_agents"]);
    assert_eq!(got["value"], json!(6));
}

#[test]
fn config_patch_preserves_unknown_extension_keys() {
    let p = project();
    let ext = p.file("ext.json", r#"{"x-team":{"flag":true,"note":null}}"#);
    p.ok(&["config", "patch", "-f", ext.to_str().unwrap()]);
    let other = p.file("other.json", r#"{"project":{"description":"d"}}"#);
    p.ok(&["config", "patch", "-f", other.to_str().unwrap()]);
    assert_eq!(p.config_value()["x-team"], json!({"flag": true}));
}

#[test]
fn config_update_with_the_current_full_config_changes_nothing() {
    let p = project();
    let before = p.config_bytes();
    let file = p.file("full.json", p.config_value().to_string());
    let out = p.run(&["config", "update", "-f", file.to_str().unwrap()]);
    assert_success(&out, &["config", "update"]);
    assert_eq!(p.config_bytes(), before);
}

#[test]
fn config_update_replaces_the_whole_document_and_drops_omitted_extensions() {
    let p = project();
    let ext = p.file("ext.json", r#"{"x-team":{"flag":true}}"#);
    p.ok(&["config", "patch", "-f", ext.to_str().unwrap()]);
    let mut full = p.config_value();
    full.as_object_mut().unwrap().remove("x-team");
    full["project"]["name"] = json!("renamed");
    let file = p.file("full.yaml", serde_yaml_ng::to_string(&full).unwrap());
    p.ok(&["config", "update", "-f", file.to_str().unwrap()]);
    let after = p.config_value();
    assert_eq!(after["project"]["name"], "renamed");
    assert!(after.get("x-team").is_none(), "{after}");
}

#[test]
fn config_update_keeps_a_literal_null_in_an_extension_value() {
    let p = project();
    let mut full = p.config_value();
    full["x-ext"] = json!({"explicit": null});
    let file = p.file("full.json", full.to_string());
    p.ok(&["config", "update", "-f", file.to_str().unwrap()]);
    assert_eq!(p.config_value()["x-ext"], json!({"explicit": null}));
}

#[test]
fn config_update_missing_a_required_section_is_refused() {
    let p = project();
    let before = p.config_bytes();
    let file = p.file("partial.json", r#"{"project":{"name":"only"}}"#);
    let out = p.run(&["config", "update", "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "required field");
    assert_eq!(p.config_bytes(), before);
}

#[test]
fn config_update_with_an_empty_object_is_refused() {
    let p = project();
    let before = p.config_bytes();
    let file = p.file("empty.json", "{}");
    let out = p.run(&["config", "update", "-f", file.to_str().unwrap()]);
    assert_rejected(&out, "required field");
    assert_eq!(p.config_bytes(), before);
}

#[test]
fn config_patch_with_a_wrongly_typed_known_field_is_refused() {
    let p = project();
    let before = p.config_bytes();
    for body in [
        r#"{"scheduling":{"max_concurrent_agents":"six"}}"#,
        r#"{"scheduling":{"max_concurrent_agents":-1}}"#,
        r#"{"scheduling":{"poll_interval_ms":1.5}}"#,
        r#"{"scheduling":5}"#,
        r#"{"project":{"description":7}}"#,
        r#"{"defaults":{"agent":{"approval_policy":"sometimes"}}}"#,
    ] {
        let file = p.file("patch.json", body);
        let out = p.run(&["config", "patch", "-f", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{body}: {}", stderr(&out));
        assert_eq!(p.config_bytes(), before, "{body}");
    }
}

#[test]
fn empty_config_patch_does_not_rewrite_config_yml() {
    let p = project();
    let path = p.store().join("config.yml");
    let before = (
        p.config_bytes(),
        fs::metadata(&path).unwrap().modified().unwrap(),
    );
    let file = p.file("patch.json", "{}");
    p.ok(&["config", "patch", "-f", file.to_str().unwrap()]);
    let after = (
        p.config_bytes(),
        fs::metadata(&path).unwrap().modified().unwrap(),
    );
    assert_eq!(after, before);
}

#[test]
fn config_patch_that_restates_current_values_does_not_rewrite_config_yml() {
    let p = project();
    // Hand-formatted but equivalent content must survive a no-op byte for byte.
    let path = p.store().join("config.yml");
    let mut text = String::from_utf8(p.config_bytes()).unwrap();
    text.push_str("# hand comment\n");
    fs::write(&path, &text).unwrap();
    let file = p.file("patch.json", r#"{"project":{"name":"up"}}"#);
    p.ok(&["config", "patch", "-f", file.to_str().unwrap()]);
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn config_file_stays_owner_only_after_a_patch() {
    let p = project();
    let file = p.file("patch.json", r#"{"project":{"description":"perm"}}"#);
    p.ok(&["config", "patch", "-f", file.to_str().unwrap()]);
    let mode = fs::metadata(p.store().join("config.yml"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn config_mutation_prints_the_resulting_config_in_the_requested_format() {
    let p = project();
    let file = p.file("patch.json", r#"{"project":{"description":"shown"}}"#);
    let out = p.ok(&["config", "patch", "-f", file.to_str().unwrap(), "--yaml"]);
    let printed: Value = serde_yaml_ng::from_slice(&out.stdout).expect("yaml");
    assert_eq!(printed["project"]["description"], "shown");
}

#[test]
fn config_mutation_outside_a_project_fails_and_creates_no_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        !dir.path()
            .ancestors()
            .any(|d| d.join(".orchestry").is_dir())
    );
    fs::create_dir_all(dir.path().join("home")).unwrap();
    let file = dir.path().join("patch.json");
    fs::write(&file, r#"{"project":{"description":"x"}}"#).unwrap();
    let out = Command::cargo_bin("orch")
        .unwrap()
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            dir.path().join("home/global.yml"),
        )
        .env("PATH", "/usr/bin:/bin")
        .env("NO_COLOR", "1")
        .args(["config", "patch", "-f"])
        .arg(&file)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!Path::new(&dir.path().join(".orchestry")).exists());
}

#[test]
fn config_patch_null_for_a_whole_default_backed_section_falls_back_to_defaults() {
    let p = project();
    let file = p.file("patch.json", r#"{"project":null}"#);
    p.ok(&["config", "patch", "-f", file.to_str().unwrap()]);
    assert!(p.config_value().get("project").is_none());
    // The project still loads: the default name shows through.
    let got = p.json(&["config", "get", "project.name"]);
    assert_eq!(got["value"], json!("my-project"));
}

#[test]
fn updated_task_file_is_owner_only() {
    let p = project();
    let id = p.task("t");
    let file = p.file("patch.json", r#"{"title":"changed"}"#);
    p.ok(&["task", "patch", &id, "-f", file.to_str().unwrap()]);
    let mode = fs::metadata(p.entity_path("tasks", &id))
        .expect("task file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
