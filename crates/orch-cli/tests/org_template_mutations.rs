//! Template `update` / `patch` (root and member) through the real `orch` binary.
//!
//! Companion to `org_templates.rs`, which owns sources, parsing, rules, deploy
//! and the shop. This file pins the mutation contract of
//! `docs/plans/2026-10-17-org-templates.md` (design decisions D1, D3, D9, D11
//! and the R1 amendments):
//!
//! * D1  `skills` is presence-preserving: absent inherits, `[]` disables,
//!   `[x]` replaces; authoring output never materialises defaults.
//! * D3  rule references, cycles and the extraction header survive mutation.
//! * D9  `update` replaces, `patch` is RFC 7396; null deletes in a patch and
//!   is invalid in a stored definition or a full replacement.
//! * D11 everything is validated as a whole candidate before one atomic,
//!   locked, single-file publication; failures and no-ops write nothing.
//! * R1  a bounded malformed target can be repaired by a full update, but
//!   never a symlink, an oversized file, an unrelated corruption or a
//!   parseable root whose identity would change.
//!
//! Every test owns a sandbox: throwaway `HOME`, throwaway global config (and
//! so user template root), throwaway project that is never initialised unless
//! a test deploys, and a `PATH` that holds no agent CLI. Nothing here reads or
//! writes outside the sandbox and no agent is ever started.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use assert_cmd::prelude::*;
use serde_json::{Value, json};

// ── Sandbox ────────────────────────────────────────────────────────────────

/// Upper bound for any single `orch` invocation.
const RUN_BUDGET: Duration = Duration::from_secs(60);

struct Sandbox {
    dir: tempfile::TempDir,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::Builder::new()
        .prefix("orch-template-mutations-")
        .tempdir()
        .expect("tempdir");
    assert!(
        !dir.path()
            .ancestors()
            .any(|d| d.join(".orchestry").is_dir()),
        "the tmpdir has an .orchestry ancestor; point TMPDIR outside any ORCH project"
    );
    for sub in ["home", "project", "work"] {
        fs::create_dir_all(dir.path().join(sub)).expect("sandbox dirs");
    }
    Sandbox { dir }
}

impl Sandbox {
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// The user template root: `<global-config-parent>/templates`.
    fn user_templates(&self) -> PathBuf {
        self.home().join("templates")
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("orch").expect("the orch binary");
        command
            .current_dir(self.project())
            .env_clear()
            .env("HOME", self.home())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.home().join("global.yml"),
            )
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            // No agent CLI exists on this PATH.
            .env("PATH", "/usr/bin:/bin");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        run_bounded(self.command().args(args), None)
    }

    fn run_stdin(&self, args: &[&str], input: &[u8]) -> Output {
        run_bounded(self.command().args(args), Some(input.to_vec()))
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "orch {args:?} exited {:?}\n{}",
            output.status.code(),
            transcript(&output)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        parse_json(&self.ok(&all))
    }

    fn init_project(&self) {
        self.ok(&["init", "--name", "proj", "--adapter", "claude"]);
    }

    /// `show` as authoring data (never resolved).
    fn authored(&self, root: &Path) -> Value {
        self.json(&["org", "template", "show", "--template-dir", path_arg(root)])
    }

    /// `show --resolved`: effective deployment inputs by member name.
    fn resolved(&self, root: &Path) -> BTreeMap<String, Value> {
        self.json(&[
            "org",
            "template",
            "show",
            "--template-dir",
            path_arg(root),
            "--resolved",
        ])["resolved_agents"]
            .as_array()
            .expect("resolved_agents")
            .iter()
            .map(|a| (a["name"].as_str().expect("name").to_owned(), a.clone()))
            .collect()
    }
}

/// Run to completion under `RUN_BUDGET`, feeding `stdin` (closed immediately
/// when `None`); a hang kills the child and fails the test.
fn run_bounded(command: &mut Command, stdin: Option<Vec<u8>>) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orch");
    let mut sink = child.stdin.take().expect("stdin");
    let feeder = std::thread::spawn(move || {
        if let Some(bytes) = stdin {
            // A command that exits before reading closes the pipe.
            let _ = sink.write_all(&bytes);
        }
    });
    let mut out = child.stdout.take().expect("stdout");
    let mut err = child.stderr.take().expect("stderr");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + RUN_BUDGET;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("orch outlived its {RUN_BUDGET:?} budget: {command:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    feeder.join().expect("stdin feeder");
    Output {
        status,
        stdout: out_thread.join().expect("stdout reader"),
        stderr: err_thread.join().expect("stderr reader"),
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn transcript(output: &Output) -> String {
    format!("stdout:\n{}\nstderr:\n{}", stdout(output), stderr(output))
}

fn combined(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn parse_json(output: &Output) -> Value {
    serde_json::from_str(&stdout(output))
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}", transcript(output)))
}

fn parse_yaml(output: &Output) -> Value {
    serde_yaml_ng::from_str(&stdout(output))
        .unwrap_or_else(|e| panic!("stdout is not YAML ({e}):\n{}", transcript(output)))
}

/// A clean, loud failure: exit 1-3, never a panic (101) or a signal, and not
/// merely a command line the binary does not know (which would also "reject").
fn assert_rejected(output: &Output, what: &str) {
    assert!(
        matches!(output.status.code(), Some(1..=3)),
        "[{what}] should fail cleanly (exit 1-3), got {:?}\n{}",
        output.status.code(),
        transcript(output)
    );
    let text = combined(output);
    assert!(
        ![
            "unknown command",
            "unexpected argument",
            "unrecognized subcommand"
        ]
        .iter()
        .any(|usage| text.contains(usage)),
        "[{what}] was refused as an unknown command line, not by the mutation\n{}",
        transcript(output)
    );
}

fn assert_mentions(output: &Output, needle: &str, what: &str) {
    assert!(
        combined(output).contains(needle),
        "[{what}] expected the output to mention {needle:?}\n{}",
        transcript(output)
    );
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

// ── Template fixtures ──────────────────────────────────────────────────────

/// A directory template under construction. Files are written in pretty JSON
/// syntax (which is YAML), so the stored bytes can never equal what the
/// mutation writer emits: a rewrite is therefore always visible.
struct Bundle {
    org: Value,
    agents: Vec<(String, Value)>,
    rules: Vec<(String, String)>,
}

impl Bundle {
    fn new(name: &str, lead: &str) -> Self {
        Bundle {
            org: json!({
                "schema_version": 1,
                "name": name,
                "description": "fixture team",
                "lead": lead,
                "rules": [],
            }),
            agents: Vec::new(),
            rules: Vec::new(),
        }
    }

    fn agent(mut self, name: &str, extra: Value) -> Self {
        let mut doc = json!({"name": name, "role": format!("{name} role")});
        for (key, value) in extra.as_object().expect("object") {
            doc[key] = value.clone();
        }
        self.agents.push((name.to_owned(), doc));
        self
    }

    fn rule(mut self, id: &str, text: &str) -> Self {
        self.rules.push((id.to_owned(), text.to_owned()));
        self.org["rules"]
            .as_array_mut()
            .expect("rules array")
            .push(json!(id));
        self
    }

    fn defaults(mut self, defaults: Value) -> Self {
        self.org["defaults"] = defaults;
        self
    }

    fn write(&self, root: &Path) -> PathBuf {
        fs::create_dir_all(root.join("agents")).expect("agents dir");
        fs::create_dir_all(root.join("rules")).expect("rules dir");
        fs::write(root.join("org.yml"), pretty(&self.org)).expect("org.yml");
        for (name, doc) in &self.agents {
            fs::write(root.join("agents").join(format!("{name}.yml")), pretty(doc))
                .expect("agent file");
        }
        for (id, text) in &self.rules {
            fs::write(root.join("rules").join(format!("{id}.md")), text).expect("rule file");
        }
        root.to_path_buf()
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("json")
}

/// Org defaults carry `skills: [review]`; members are
/// `boss` (skills absent, the lead), `empty` (`[]`) and `own` (`[qa]`).
fn skills_team(sb: &Sandbox) -> PathBuf {
    Bundle::new("skills-team", "boss")
        .defaults(json!({
            "adapter": "claude",
            "skills": ["review"],
            "max_turns": 7,
            "env": {"A": "1", "B": "2"},
        }))
        .agent("boss", json!({}))
        .agent("empty", json!({"skills": []}))
        .agent("own", json!({"skills": ["qa"]}))
        .write(&sb.work().join("skills-team"))
}

/// Three members, no defaults: `lead`, `worker`, `reviewer`.
fn small_team(sb: &Sandbox, dir: &str) -> PathBuf {
    Bundle::new(dir, "lead")
        .agent("lead", json!({}))
        .agent("worker", json!({}))
        .agent("reviewer", json!({}))
        .write(&sb.work().join(dir))
}

/// A user-root template `name` with the same three members.
fn user_team(sb: &Sandbox, name: &str) -> PathBuf {
    Bundle::new(name, "lead")
        .agent("lead", json!({}))
        .agent("worker", json!({}))
        .write(&sb.user_templates().join(name))
}

/// Every file (and symlink, and directory) under `root`, by relative path,
/// with its bytes. The advisory `.template.lock` is the one artifact a
/// mutating attempt may leave behind; it carries no template data.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("read_dir").flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            if rel.ends_with(".template.lock") {
                continue;
            }
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("link");
                out.insert(rel, format!("-> {}", target.display()).into_bytes());
            } else if meta.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(&path, root, out);
            } else {
                out.insert(rel, fs::read(&path).expect("read"));
            }
        }
    }
    walk(root, root, &mut out);
    out
}

/// Modification times of the regular files under `root`.
fn mtimes(root: &Path) -> BTreeMap<String, SystemTime> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, SystemTime>) {
        for entry in fs::read_dir(dir).expect("read_dir").flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.is_dir() {
                walk(&path, root, out);
            } else if meta.is_file() && !path.ends_with(".template.lock") {
                out.insert(
                    path.strip_prefix(root).unwrap().display().to_string(),
                    meta.modified().expect("mtime"),
                );
            }
        }
    }
    walk(root, root, &mut out);
    out
}

/// Push every regular file under `root` far into the past, so any rewrite
/// (even of identical bytes) shows up as a changed modification time without
/// the test ever sleeping.
fn age(root: &Path) {
    let epoch_plus = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for rel in mtimes(root).keys() {
        fs::OpenOptions::new()
            .write(true)
            .open(root.join(rel))
            .expect("open for age")
            .set_modified(epoch_plus)
            .expect("set mtime");
    }
}

/// Bytes and modification times, for "nothing was written" assertions.
struct Frozen {
    bytes: BTreeMap<String, Vec<u8>>,
    times: BTreeMap<String, SystemTime>,
}

fn freeze(root: &Path) -> Frozen {
    age(root);
    Frozen {
        bytes: snapshot(root),
        times: mtimes(root),
    }
}

fn assert_untouched(root: &Path, frozen: &Frozen, what: &str) {
    assert_eq!(snapshot(root), frozen.bytes, "[{what}] bytes changed");
    assert_eq!(mtimes(root), frozen.times, "[{what}] a file was rewritten");
}

fn member_path(root: &Path, name: &str) -> PathBuf {
    root.join("agents").join(format!("{name}.yml"))
}

/// A stored definition parsed as data.
fn stored(path: &Path) -> Value {
    serde_yaml_ng::from_str(&fs::read_to_string(path).expect("read stored definition"))
        .expect("stored definition is YAML")
}

fn names(agents: &Value) -> Vec<String> {
    agents
        .as_array()
        .expect("agent array")
        .iter()
        .map(|a| a["name"].as_str().expect("name").to_owned())
        .collect()
}

/// Names of everything under `dir` that is not a `.yml` file: a leftover
/// temporary file would show up here.
fn stray_entries(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".yml") && !n.ends_with(".md") && n != ".template.lock")
        .collect();
    out.sort();
    out
}

// ── Command helpers ────────────────────────────────────────────────────────

/// A deferred command: (label, how to run it, expected description).
type Run<'a> = (&'a str, Box<dyn Fn() -> Output + 'a>, &'a str);

/// Where a mutation reads its document from.
enum Doc<'a> {
    File(&'a Path),
    Stdin(&'a str),
}

/// `selector` is the template (and for members the agent) selection.
fn mutate(
    sb: &Sandbox,
    members: bool,
    verb: &str,
    selector: &[&str],
    doc: &Doc,
    extra: &[&str],
) -> Output {
    let mut args: Vec<&str> = vec!["org", "template"];
    if members {
        args.push("agent");
    }
    args.push(verb);
    args.extend(selector);
    args.push("-f");
    match doc {
        Doc::File(path) => {
            args.push(path_arg(path));
            args.extend(extra);
            sb.run(&args)
        }
        Doc::Stdin(text) => {
            args.push("-");
            args.extend(extra);
            sb.run_stdin(&args, text.as_bytes())
        }
    }
}

fn org_sel(root: &Path) -> Vec<&str> {
    vec!["--template-dir", path_arg(root)]
}

fn agent_sel<'a>(root: &'a Path, agent: &'a str) -> Vec<&'a str> {
    vec!["--template-dir", path_arg(root), "--agent", agent]
}

#[test]
fn adding_a_member_from_json_file_preserves_surrogate_pairs() {
    assert_added_surrogate_pair(false);
}

#[test]
fn adding_a_member_from_json_stdin_preserves_surrogate_pairs() {
    assert_added_surrogate_pair(true);
}

fn assert_added_surrogate_pair(stdin: bool) {
    let sb = sandbox();
    for explicit_format in [false, true] {
        let root = small_team(
            &sb,
            if explicit_format {
                "explicit"
            } else {
                "inferred"
            },
        );
        let input = br#"{"name":"extra","role":"\ud83e\udd80"}"#;
        let file = sb.work().join("member.json");
        fs::write(&file, input).unwrap();
        let mut args = vec![
            "org",
            "template",
            "agent",
            "add",
            "--template-dir",
            path_arg(&root),
            "--json",
            "-f",
            if stdin { "-" } else { path_arg(&file) },
        ];
        if explicit_format {
            args.extend(["--input-format", "json"]);
        }
        let output = if stdin {
            sb.run_stdin(&args, input)
        } else {
            sb.run(&args)
        };
        assert!(output.status.success(), "{}", transcript(&output));
        assert_eq!(parse_json(&output)["role"], "🦀");
        assert_eq!(stored(&member_path(&root, "extra"))["role"], "🦀");
    }
}

fn patch_org(sb: &Sandbox, root: &Path, doc: &Value) -> Output {
    mutate(
        sb,
        false,
        "patch",
        &org_sel(root),
        &Doc::Stdin(&doc.to_string()),
        &["--json"],
    )
}

fn update_org(sb: &Sandbox, root: &Path, doc: &Value) -> Output {
    mutate(
        sb,
        false,
        "update",
        &org_sel(root),
        &Doc::Stdin(&doc.to_string()),
        &["--json"],
    )
}

fn patch_agent(sb: &Sandbox, root: &Path, agent: &str, doc: &Value) -> Output {
    mutate(
        sb,
        true,
        "patch",
        &agent_sel(root, agent),
        &Doc::Stdin(&doc.to_string()),
        &["--json"],
    )
}

fn update_agent(sb: &Sandbox, root: &Path, agent: &str, doc: &Value) -> Output {
    mutate(
        sb,
        true,
        "update",
        &agent_sel(root, agent),
        &Doc::Stdin(&doc.to_string()),
        &["--json"],
    )
}

/// A successful mutation, returning its echoed JSON.
fn echoed(output: Output) -> Value {
    assert!(output.status.success(), "{}", transcript(&output));
    parse_json(&output)
}

/// The authoring member `name` as `show` prints it.
fn shown_member(sb: &Sandbox, root: &Path, name: &str) -> Value {
    sb.authored(root)["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("{name} not shown"))
        .clone()
}

fn without(doc: &Value, key: &str) -> Value {
    let mut copy = doc.clone();
    copy.as_object_mut().expect("object").remove(key);
    copy
}

// ═══ D1 — skills: absent / [] / [value] through show → update → resolve ═════

fn skills_of(resolved: &BTreeMap<String, Value>, name: &str) -> Option<Value> {
    resolved[name].get("skills").cloned()
}

#[test]
fn authoring_show_keeps_absent_empty_and_listed_skills_distinct() {
    let sb = sandbox();
    let root = skills_team(&sb);
    assert_eq!(
        (
            shown_member(&sb, &root, "boss").get("skills").cloned(),
            shown_member(&sb, &root, "empty").get("skills").cloned(),
            shown_member(&sb, &root, "own").get("skills").cloned(),
        ),
        (None, Some(json!([])), Some(json!(["qa"])))
    );
}

#[test]
fn resolution_inherits_the_default_replaces_it_and_lets_an_empty_list_disable_it() {
    let sb = sandbox();
    let resolved = sb.resolved(&skills_team(&sb));
    assert_eq!(
        (
            skills_of(&resolved, "boss"),
            skills_of(&resolved, "empty"),
            skills_of(&resolved, "own"),
        ),
        (
            Some(json!(["review"])),
            Some(json!([])),
            Some(json!(["qa"]))
        )
    );
}

#[test]
fn updating_every_member_from_its_own_show_echo_writes_nothing() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    for name in ["boss", "empty", "own"] {
        let echo = shown_member(&sb, &root, name);
        echoed(update_agent(&sb, &root, name, &echo));
    }
    assert_untouched(&root, &frozen, "member show echo");
}

#[test]
fn updating_the_org_from_its_own_show_echo_writes_nothing() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let echo = sb.authored(&root)["org"].clone();
    echoed(update_org(&sb, &root, &echo));
    assert_untouched(&root, &frozen, "org show echo");
}

#[test]
fn a_show_echo_update_leaves_the_authoring_output_and_resolution_as_they_were() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let before = (sb.authored(&root)["agents"].clone(), sb.resolved(&root));
    for name in ["boss", "empty", "own"] {
        let echo = shown_member(&sb, &root, name);
        echoed(update_agent(&sb, &root, name, &echo));
    }
    assert_eq!(
        (sb.authored(&root)["agents"].clone(), sb.resolved(&root)),
        before
    );
}

#[test]
fn a_changed_member_update_rewrites_only_that_file_and_keeps_skills_presence() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let mut echo = shown_member(&sb, &root, "empty");
    echo["description"] = json!("now described");
    echoed(update_agent(&sb, &root, "empty", &echo));
    let changed: Vec<String> = snapshot(&root)
        .into_iter()
        .filter(|(path, bytes)| frozen.bytes.get(path) != Some(bytes))
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        (
            changed,
            stored(&member_path(&root, "empty"))["skills"].clone()
        ),
        (vec!["agents/empty.yml".to_owned()], json!([]))
    );
}

#[test]
fn show_update_resolve_deploy_agree_for_absent_empty_and_listed_skills() {
    let sb = sandbox();
    sb.init_project();
    let root = skills_team(&sb);
    for name in ["boss", "empty", "own"] {
        let echo = shown_member(&sb, &root, name);
        echoed(update_agent(&sb, &root, name, &echo));
    }
    let resolved = sb.resolved(&root);
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    let deployed: BTreeMap<String, Option<Value>> = sb
        .json(&["agent", "list"])
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_owned(),
                a["config"].get("skills").cloned(),
            )
        })
        .filter(|(name, _)| ["boss", "empty", "own"].contains(&name.as_str()))
        .collect();
    let want: BTreeMap<String, Option<Value>> = ["boss", "empty", "own"]
        .iter()
        .map(|n| ((*n).to_owned(), skills_of(&resolved, n)))
        .collect();
    assert_eq!(deployed, want);
}

#[test]
fn deployed_skills_after_updates_are_inherit_disable_and_replace() {
    let sb = sandbox();
    sb.init_project();
    let root = skills_team(&sb);
    for name in ["boss", "empty", "own"] {
        let echo = shown_member(&sb, &root, name);
        echoed(update_agent(&sb, &root, name, &echo));
    }
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    let agents = sb.json(&["agent", "list"]);
    let skills = |name: &str| {
        agents
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .unwrap()["config"]["skills"]
            .clone()
    };
    assert_eq!(
        (skills("boss"), skills("empty"), skills("own")),
        (json!(["review"]), json!([]), json!(["qa"]))
    );
}

#[test]
fn patching_a_skills_override_to_null_makes_the_member_inherit_again() {
    let sb = sandbox();
    let root = skills_team(&sb);
    for name in ["empty", "own"] {
        echoed(patch_agent(&sb, &root, name, &json!({"skills": null})));
    }
    let resolved = sb.resolved(&root);
    assert_eq!(
        (
            stored(&member_path(&root, "empty")).get("skills").cloned(),
            stored(&member_path(&root, "own")).get("skills").cloned(),
            skills_of(&resolved, "empty"),
            skills_of(&resolved, "own"),
        ),
        (None, None, Some(json!(["review"])), Some(json!(["review"])))
    );
}

#[test]
fn patching_skills_null_on_a_member_that_has_none_changes_nothing() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    echoed(patch_agent(&sb, &root, "boss", &json!({"skills": null})));
    assert_untouched(&root, &frozen, "null of an absent override");
}

#[test]
fn a_full_update_with_null_skills_is_rejected_and_nothing_changes() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let mut doc = shown_member(&sb, &root, "own");
    doc["skills"] = Value::Null;
    let output = update_agent(&sb, &root, "own", &doc);
    assert_rejected(&output, "full update with null skills");
    assert_mentions(&output, "skills", "full update with null skills");
    assert_untouched(&root, &frozen, "full update with null skills");
}

#[test]
fn a_full_update_with_null_org_default_skills_is_rejected_and_nothing_changes() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let mut doc = sb.authored(&root)["org"].clone();
    doc["defaults"]["skills"] = Value::Null;
    let output = update_org(&sb, &root, &doc);
    assert_rejected(&output, "full org update with null defaults.skills");
    assert_untouched(&root, &frozen, "full org update with null defaults.skills");
}

#[test]
fn a_full_update_that_omits_skills_restores_inheritance() {
    let sb = sandbox();
    let root = skills_team(&sb);
    for name in ["empty", "own"] {
        let doc = without(&shown_member(&sb, &root, name), "skills");
        echoed(update_agent(&sb, &root, name, &doc));
    }
    let resolved = sb.resolved(&root);
    assert_eq!(
        (
            stored(&member_path(&root, "empty")).get("skills").cloned(),
            stored(&member_path(&root, "own")).get("skills").cloned(),
            skills_of(&resolved, "empty"),
            skills_of(&resolved, "own"),
        ),
        (None, None, Some(json!(["review"])), Some(json!(["review"])))
    );
}

#[test]
fn patching_a_skills_list_replaces_it_wholesale_and_an_empty_list_disables_inheritance() {
    let sb = sandbox();
    let root = skills_team(&sb);
    echoed(patch_agent(
        &sb,
        &root,
        "own",
        &json!({"skills": ["x", "y"]}),
    ));
    echoed(patch_agent(&sb, &root, "boss", &json!({"skills": []})));
    let resolved = sb.resolved(&root);
    assert_eq!(
        (skills_of(&resolved, "own"), skills_of(&resolved, "boss")),
        (Some(json!(["x", "y"])), Some(json!([])))
    );
}

#[test]
fn removing_the_org_default_skills_leaves_absent_members_with_no_skills_key() {
    let sb = sandbox();
    let root = skills_team(&sb);
    echoed(patch_org(
        &sb,
        &root,
        &json!({"defaults": {"skills": null}}),
    ));
    let resolved = sb.resolved(&root);
    assert_eq!(
        (
            skills_of(&resolved, "boss"),
            skills_of(&resolved, "empty"),
            skills_of(&resolved, "own"),
        ),
        (None, Some(json!([])), Some(json!(["qa"])))
    );
}

// ═══ D1 — the shop installs what deploy would, after the same mutations ════

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const PTY_WAIT: Duration = Duration::from_secs(20);
const DOWN: &[u8] = b"\x1b[B";
const ENTER: &[u8] = b"\r";

/// `orch` on a real pty, with every wait a `recv_timeout`, so a stuck picker
/// fails the test instead of hanging the suite.
struct Pty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    chunks: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

fn pty_orch(sb: &Sandbox, args: &[&str]) -> Pty {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut command = CommandBuilder::new(assert_cmd::cargo::cargo_bin("orch"));
    command.env_clear();
    command.args(args);
    command.cwd(sb.project());
    command.env("HOME", sb.home());
    command.env("ORCHESTRY_GLOBAL_CONFIG_PATH", sb.home().join("global.yml"));
    command.env("TERM", "xterm-256color");
    command.env("PATH", "/usr/bin:/bin");
    command.env("NO_COLOR", "1");
    let child = pair.slave.spawn_command(command).expect("spawn on a pty");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let writer = pair.master.take_writer().expect("pty writer");
    let (tx, chunks) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    Pty {
        child,
        writer,
        chunks,
        seen: Vec::new(),
    }
}

impl Pty {
    fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + PTY_WAIT;
        while !String::from_utf8_lossy(&self.seen).contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(_) => panic!(
                    "never saw {needle:?}; screen so far:\n{:?}",
                    String::from_utf8_lossy(&self.seen)
                ),
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    fn finish(&mut self) -> u32 {
        let deadline = Instant::now() + PTY_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = self.child.kill();
                    panic!("orch did not exit");
                }
            }
        }
        self.child.wait().expect("wait").exit_code()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn shop_installs_equal_the_resolved_preview_after_skills_were_mutated() {
    let sb = sandbox();
    sb.init_project();
    let root = skills_team(&sb);
    // Cover all three cases after real mutations: absent → patched list,
    // [] → null (inherit), [qa] → full update omitting skills (inherit).
    echoed(patch_agent(
        &sb,
        &root,
        "boss",
        &json!({"skills": ["boss-only"]}),
    ));
    echoed(patch_agent(&sb, &root, "empty", &json!({"skills": null})));
    let own = without(&shown_member(&sb, &root, "own"), "skills");
    echoed(update_agent(&sb, &root, "own", &own));
    let resolved = sb.resolved(&root);

    for (index, name) in ["boss", "empty", "own"].iter().enumerate() {
        let mut pty = pty_orch(&sb, &["agent", "shop", "--template-dir", path_arg(&root)]);
        pty.expect("AGENT SHOP");
        for _ in 0..index {
            pty.send(DOWN);
        }
        pty.expect(&format!("▸ {name}"));
        pty.send(ENTER);
        assert_eq!(pty.finish(), 0, "shop install of {name}");
    }

    let installed = sb.json(&["agent", "list"]);
    let compare = |name: &str| {
        let have = installed
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .unwrap_or_else(|| panic!("{name} not installed"));
        (
            have["role"].clone(),
            have["config"].get("skills").cloned(),
            resolved[name]["role"].clone(),
            resolved[name].get("skills").cloned(),
        )
    };
    let rows: Vec<_> = ["boss", "empty", "own"]
        .iter()
        .map(|n| compare(n))
        .collect();
    assert!(
        rows.iter().all(
            |(role, skills, want_role, want_skills)| role == want_role && skills == want_skills
        ),
        "shop installs differ from the resolved preview: {rows:?}"
    );
}

// ═══ D9 — patch is RFC 7396; update replaces ═══════════════════════════════

#[test]
fn a_patch_merges_nested_env_deletes_null_members_and_keeps_the_rest() {
    let sb = sandbox();
    let root = skills_team(&sb);
    echoed(patch_org(
        &sb,
        &root,
        &json!({"defaults": {"env": {"B": null, "C": "3"}, "max_turns": null}}),
    ));
    assert_eq!(
        stored(&root.join("org.yml"))["defaults"],
        json!({"adapter": "claude", "skills": ["review"], "env": {"A": "1", "C": "3"}})
    );
}

#[test]
fn a_patch_with_null_defaults_removes_the_whole_block() {
    let sb = sandbox();
    let root = skills_team(&sb);
    echoed(patch_org(&sb, &root, &json!({"defaults": null})));
    assert_eq!(stored(&root.join("org.yml")).get("defaults"), None);
}

#[test]
fn a_patch_replaces_the_rules_array_wholesale() {
    let sb = sandbox();
    let root = Bundle::new("rule-team", "solo")
        .rule("alpha", "Alpha body.")
        .rule("beta", "Beta body.")
        .agent("solo", json!({}))
        .write(&sb.work().join("rule-team"));
    echoed(patch_org(&sb, &root, &json!({"rules": ["beta", "alpha"]})));
    assert_eq!(
        stored(&root.join("org.yml"))["rules"],
        json!(["beta", "alpha"])
    );
}

#[test]
fn a_patch_with_a_non_string_env_value_is_rejected_and_nothing_changes() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let output = patch_org(&sb, &root, &json!({"defaults": {"env": {"C": 3}}}));
    assert_rejected(&output, "numeric env value");
    assert_untouched(&root, &frozen, "numeric env value");
}

#[test]
fn a_patch_removing_a_required_field_is_rejected_and_nothing_changes() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let role = patch_agent(&sb, &root, "own", &json!({"role": null}));
    let lead = patch_org(&sb, &root, &json!({"lead": null}));
    assert_rejected(&role, "role: null");
    assert_rejected(&lead, "lead: null");
    assert_untouched(&root, &frozen, "required field removal");
}

#[test]
fn a_full_update_drops_every_omitted_optional_field() {
    let sb = sandbox();
    let root = small_team(&sb, "drop");
    echoed(patch_agent(
        &sb,
        &root,
        "worker",
        &json!({"model": "m", "skills": ["a"]}),
    ));
    echoed(update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    ));
    assert_eq!(
        stored(&member_path(&root, "worker")),
        json!({"name": "worker", "role": "r"})
    );
}

#[test]
fn an_empty_patch_writes_nothing_and_returns_the_current_definition() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let agent = echoed(patch_agent(&sb, &root, "own", &json!({})));
    let org = echoed(patch_org(&sb, &root, &json!({})));
    assert_untouched(&root, &frozen, "empty patch");
    assert_eq!(
        (agent["skills"].clone(), org["lead"].clone()),
        (json!(["qa"]), json!("boss"))
    );
}

#[test]
fn a_patch_that_sets_a_value_it_already_has_writes_nothing() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    echoed(patch_agent(&sb, &root, "own", &json!({"skills": ["qa"]})));
    echoed(patch_org(
        &sb,
        &root,
        &json!({"defaults": {"max_turns": 7}}),
    ));
    assert_untouched(&root, &frozen, "idempotent patch");
}

#[test]
fn an_identical_full_update_preserves_comments_formatting_and_file_time() {
    let sb = sandbox();
    let root = small_team(&sb, "comments");
    let commented = "# keep me\nname: worker\nrole: worker role   # trailing\n";
    fs::write(member_path(&root, "worker"), commented).unwrap();
    let frozen = freeze(&root);
    echoed(update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "worker role"}),
    ));
    assert_untouched(&root, &frozen, "semantically equal replacement");
}

#[test]
fn a_changing_update_publishes_one_valid_file_and_leaves_no_stray_entry() {
    let sb = sandbox();
    let root = small_team(&sb, "tidy");
    echoed(update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "new"}),
    ));
    echoed(patch_org(&sb, &root, &json!({"description": "new"})));
    assert_eq!(
        (
            stray_entries(&root.join("agents")),
            stray_entries(&root),
            stored(&member_path(&root, "worker"))["role"].clone(),
            sb.authored(&root)["org"]["description"].clone(),
        ),
        (
            Vec::<String>::new(),
            vec!["agents".to_owned(), "rules".to_owned()],
            json!("new"),
            json!("new"),
        )
    );
}

#[test]
fn yaml_and_json_echoes_of_the_same_mutation_describe_the_same_value() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let sel = agent_sel(&root, "own");
    let as_json = mutate(&sb, true, "patch", &sel, &Doc::Stdin("{}"), &["--json"]);
    let as_yaml = mutate(&sb, true, "patch", &sel, &Doc::Stdin("{}"), &["--yaml"]);
    assert_eq!(parse_json(&as_json), parse_yaml(&as_yaml));
}

// ═══ Forbidden changes and unknown keys ═════════════════════════════════════

#[test]
fn identity_and_unknown_field_changes_are_rejected_with_nothing_written() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let org = sb.authored(&root)["org"].clone();
    let own = shown_member(&sb, &root, "own");
    let with = |base: &Value, key: &str, value: Value| {
        let mut copy = base.clone();
        copy[key] = value;
        copy
    };
    // (label, command, expected fragment)
    let cases: Vec<(&str, Output, &str)> = vec![
        (
            "org rename (update)",
            update_org(&sb, &root, &with(&org, "name", json!("renamed"))),
            "name",
        ),
        (
            "org rename (patch)",
            patch_org(&sb, &root, &json!({"name": "renamed"})),
            "name",
        ),
        (
            "org schema 2 (update)",
            update_org(&sb, &root, &with(&org, "schema_version", json!(2))),
            "schema",
        ),
        (
            "org schema 2 (patch)",
            patch_org(&sb, &root, &json!({"schema_version": 2})),
            "schema",
        ),
        (
            "org schema as string",
            patch_org(&sb, &root, &json!({"schema_version": "1"})),
            "org.yml",
        ),
        (
            "org name removed (patch)",
            patch_org(&sb, &root, &json!({"name": null})),
            "name",
        ),
        (
            "lead points nowhere",
            patch_org(&sb, &root, &json!({"lead": "nobody"})),
            "nobody",
        ),
        (
            "unregistered rule",
            patch_org(&sb, &root, &json!({"rules": ["zzz"]})),
            "zzz",
        ),
        (
            "org unknown key",
            patch_org(&sb, &root, &json!({"typo": 1})),
            "typo",
        ),
        (
            "org unknown key null",
            patch_org(&sb, &root, &json!({"typo": null})),
            "typo",
        ),
        (
            "org unknown key (update)",
            update_org(&sb, &root, &with(&org, "typo", json!(1))),
            "typo",
        ),
        (
            "org unknown default",
            patch_org(&sb, &root, &json!({"defaults": {"typo": 1}})),
            "defaults.typo",
        ),
        (
            "org unknown default null",
            patch_org(&sb, &root, &json!({"defaults": {"typo": null}})),
            "defaults.typo",
        ),
        (
            "org default role not allowed",
            patch_org(&sb, &root, &json!({"defaults": {"role": "x"}})),
            "defaults.role",
        ),
        (
            "member rename (update)",
            update_agent(&sb, &root, "own", &with(&own, "name", json!("elsewhere"))),
            "name",
        ),
        (
            "member rename (patch)",
            patch_agent(&sb, &root, "own", &json!({"name": "elsewhere"})),
            "name",
        ),
        (
            "member takes a sibling's name",
            patch_agent(&sb, &root, "own", &json!({"name": "boss"})),
            "name",
        ),
        (
            "member unknown key",
            patch_agent(&sb, &root, "own", &json!({"typo": 1})),
            "typo",
        ),
        (
            "member unknown key null",
            patch_agent(&sb, &root, "own", &json!({"typo": null})),
            "typo",
        ),
        (
            "member unknown key (update)",
            update_agent(&sb, &root, "own", &with(&own, "typo", json!(1))),
            "typo",
        ),
        (
            "member blank role",
            patch_agent(&sb, &root, "own", &json!({"role": "  "})),
            "role",
        ),
        (
            "member unregistered rule",
            patch_agent(&sb, &root, "own", &json!({"rules": ["zzz"]})),
            "zzz",
        ),
        (
            "member runtime-only field",
            patch_agent(&sb, &root, "own", &json!({"status": "idle"})),
            "status",
        ),
    ];
    for (what, output, fragment) in &cases {
        assert_rejected(output, what);
        assert_mentions(output, fragment, what);
    }
    assert_untouched(&root, &frozen, "forbidden changes");
}

#[test]
fn equal_identity_echoes_are_accepted() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let mut org = sb.authored(&root)["org"].clone();
    org["description"] = json!("changed through an echo that repeats name and schema");
    let echoed_org = echoed(update_org(&sb, &root, &org));
    let mut own = shown_member(&sb, &root, "own");
    own["description"] = json!("member echo repeating its own name");
    let echoed_own = echoed(update_agent(&sb, &root, "own", &own));
    assert_eq!(
        (
            echoed_org["description"].clone(),
            echoed_own["description"].clone()
        ),
        (org["description"].clone(), own["description"].clone())
    );
}

#[test]
fn the_lead_may_be_changed_to_another_existing_member() {
    let sb = sandbox();
    let root = skills_team(&sb);
    echoed(patch_org(&sb, &root, &json!({"lead": "own"})));
    assert_eq!(sb.authored(&root)["org"]["lead"], json!("own"));
}

#[test]
fn a_member_that_does_not_exist_cannot_be_updated_or_patched_into_existence() {
    let sb = sandbox();
    let root = small_team(&sb, "absent");
    let frozen = freeze(&root);
    let patched = patch_agent(&sb, &root, "ghost", &json!({"description": "x"}));
    let updated = update_agent(&sb, &root, "ghost", &json!({"name": "ghost", "role": "r"}));
    assert_rejected(&patched, "patch ghost");
    assert_rejected(&updated, "update ghost");
    assert_untouched(&root, &frozen, "missing member");
}

#[test]
fn an_unsafe_member_selector_is_rejected_before_any_file_access() {
    let sb = sandbox();
    let root = small_team(&sb, "unsafe");
    let frozen = freeze(&root);
    for bad in ["../x", "/tmp/x", "a/b", "CON", "a..b"] {
        let output = mutate(
            &sb,
            true,
            "patch",
            &["--template-dir", path_arg(&root), "--agent", bad],
            &Doc::Stdin("{}"),
            &[],
        );
        assert_rejected(&output, bad);
    }
    assert_untouched(&root, &frozen, "unsafe selector");
}

// ═══ Selectors and input modes, root and member ═════════════════════════════

fn input_file(sb: &Sandbox, name: &str, text: &str) -> PathBuf {
    let path = sb.work().join(name);
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn every_input_mode_applies_a_member_patch() {
    let sb = sandbox();
    let root = small_team(&sb, "modes");
    let sel = agent_sel(&root, "worker");
    let json_file = input_file(&sb, "p.json", r#"{"description":"json file"}"#);
    let yml_file = input_file(&sb, "p.yml", "description: yml file\n");
    let yaml_file = input_file(&sb, "p.yaml", "description: yaml file\n");
    let json_in_yaml_ext = input_file(&sb, "q.yaml", r#"{"description":"json in yaml ext"}"#);
    let bare_json = input_file(&sb, "bare", r#"{"description":"bare json"}"#);
    let bare_yaml = input_file(&sb, "bare2", "description: bare yaml\n");
    let runs: Vec<Run<'_>> = vec![
        (
            "json file",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::File(&json_file),
                    &["--json"],
                )
            }),
            "json file",
        ),
        (
            "yml file",
            Box::new(|| mutate(&sb, true, "patch", &sel, &Doc::File(&yml_file), &["--json"])),
            "yml file",
        ),
        (
            "yaml file",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::File(&yaml_file),
                    &["--json"],
                )
            }),
            "yaml file",
        ),
        (
            "json in a .yaml file",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::File(&json_in_yaml_ext),
                    &["--json"],
                )
            }),
            "json in yaml ext",
        ),
        (
            "extension-less json",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::File(&bare_json),
                    &["--json"],
                )
            }),
            "bare json",
        ),
        (
            "extension-less yaml",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::File(&bare_yaml),
                    &["--json"],
                )
            }),
            "bare yaml",
        ),
        (
            "stdin json",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::Stdin(r#"{"description":"stdin json"}"#),
                    &["--json"],
                )
            }),
            "stdin json",
        ),
        (
            "stdin yaml",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::Stdin("description: stdin yaml\n"),
                    &["--json"],
                )
            }),
            "stdin yaml",
        ),
        (
            "stdin forced json",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::Stdin(r#"{"description":"forced json"}"#),
                    &["--json", "--input-format", "json"],
                )
            }),
            "forced json",
        ),
        (
            "stdin forced yaml",
            Box::new(|| {
                mutate(
                    &sb,
                    true,
                    "patch",
                    &sel,
                    &Doc::Stdin("{\"description\": \"forced yaml\"}"),
                    &["--json", "--input-format", "yaml"],
                )
            }),
            "forced yaml",
        ),
    ];
    for (what, run, want) in runs {
        let output = run();
        assert!(output.status.success(), "[{what}] {}", transcript(&output));
        assert_eq!(parse_json(&output)["description"], json!(want), "[{what}]");
        assert_eq!(
            stored(&member_path(&root, "worker"))["description"],
            json!(want),
            "[{what}] stored"
        );
    }
}

#[test]
fn every_input_mode_applies_a_root_update_and_patch() {
    let sb = sandbox();
    let root = small_team(&sb, "rootmodes");
    let sel = org_sel(&root);
    let full_json = input_file(
        &sb,
        "full.json",
        r#"{"schema_version":1,"name":"rootmodes","lead":"lead","description":"full json"}"#,
    );
    let full_yaml = "schema_version: 1\nname: rootmodes\nlead: lead\ndescription: full yaml\n";
    let runs: Vec<Run<'_>> = vec![
        (
            "update json file",
            Box::new(|| {
                mutate(
                    &sb,
                    false,
                    "update",
                    &sel,
                    &Doc::File(&full_json),
                    &["--json"],
                )
            }),
            "full json",
        ),
        (
            "update stdin yaml",
            Box::new(|| {
                mutate(
                    &sb,
                    false,
                    "update",
                    &sel,
                    &Doc::Stdin(full_yaml),
                    &["--json"],
                )
            }),
            "full yaml",
        ),
        (
            "patch stdin json",
            Box::new(|| {
                mutate(
                    &sb,
                    false,
                    "patch",
                    &sel,
                    &Doc::Stdin(r#"{"description":"patch json"}"#),
                    &["--json"],
                )
            }),
            "patch json",
        ),
        (
            "patch stdin yaml",
            Box::new(|| {
                mutate(
                    &sb,
                    false,
                    "patch",
                    &sel,
                    &Doc::Stdin("description: patch yaml\n"),
                    &["--json"],
                )
            }),
            "patch yaml",
        ),
    ];
    for (what, run, want) in runs {
        let output = run();
        assert!(output.status.success(), "[{what}] {}", transcript(&output));
        assert_eq!(parse_json(&output)["description"], json!(want), "[{what}]");
        assert_eq!(
            stored(&root.join("org.yml"))["description"],
            json!(want),
            "[{what}] stored"
        );
    }
}

#[test]
fn the_selected_parser_is_final_and_a_wrong_language_fails_unchanged() {
    let sb = sandbox();
    let root = small_team(&sb, "parser");
    let frozen = freeze(&root);
    let yaml_in_json = input_file(&sb, "wrong.json", "description: not json\n");
    let yaml_forced_json = input_file(&sb, "wrong2", "description: not json\n");
    let sel = agent_sel(&root, "worker");
    let a = mutate(&sb, true, "patch", &sel, &Doc::File(&yaml_in_json), &[]);
    let b = mutate(
        &sb,
        true,
        "patch",
        &sel,
        &Doc::File(&yaml_forced_json),
        &["--input-format", "json"],
    );
    assert_rejected(&a, "yaml content in a .json file");
    assert_rejected(&b, "yaml content forced to json");
    assert_mentions(&a, "invalid JSON", "no fallback to YAML");
    assert_untouched(&root, &frozen, "wrong language");
}

#[test]
fn a_user_template_is_addressable_by_name_qualifier_and_positional_member() {
    let sb = sandbox();
    let root = user_team(&sb, "mine");
    let by_flag = mutate(
        &sb,
        true,
        "patch",
        &["mine", "--agent", "worker"],
        &Doc::Stdin(r#"{"description":"by flag"}"#),
        &["--json"],
    );
    let by_position = mutate(
        &sb,
        true,
        "patch",
        &["mine", "worker"],
        &Doc::Stdin(r#"{"description":"by position"}"#),
        &["--json"],
    );
    let qualified = mutate(
        &sb,
        true,
        "patch",
        &["user:mine", "--agent", "worker"],
        &Doc::Stdin(r#"{"description":"qualified"}"#),
        &["--json"],
    );
    let org = mutate(
        &sb,
        false,
        "patch",
        &["mine"],
        &Doc::Stdin(r#"{"description":"org by name"}"#),
        &["--json"],
    );
    let wants = [
        (by_flag, "by flag"),
        (by_position, "by position"),
        (qualified, "qualified"),
    ];
    for (output, want) in wants {
        assert!(output.status.success(), "{}", transcript(&output));
        assert_eq!(parse_json(&output)["description"], json!(want));
    }
    assert!(org.status.success(), "{}", transcript(&org));
    assert_eq!(
        (
            stored(&member_path(&root, "worker"))["description"].clone(),
            stored(&root.join("org.yml"))["description"].clone()
        ),
        (json!("qualified"), json!("org by name"))
    );
}

#[test]
fn a_name_selector_and_a_template_dir_together_are_a_usage_error_that_writes_nothing() {
    let sb = sandbox();
    let root = user_team(&sb, "mine");
    let other = small_team(&sb, "other");
    let frozen = freeze(&root);
    let other_frozen = freeze(&other);
    let output = mutate(
        &sb,
        true,
        "patch",
        &[
            "mine",
            "--template-dir",
            path_arg(&other),
            "--agent",
            "worker",
        ],
        &Doc::Stdin(r#"{"description":"x"}"#),
        &[],
    );
    assert_rejected(&output, "both selectors");
    assert_mentions(&output, "cannot be used with", "both selectors");
    assert_untouched(&root, &frozen, "user template");
    assert_untouched(&other, &other_frozen, "explicit directory");
}

#[test]
fn builtin_and_unknown_templates_are_read_only_for_update_and_patch() {
    let sb = sandbox();
    let runs = [
        (
            "builtin member patch",
            mutate(
                &sb,
                true,
                "patch",
                &["a-team", "--agent", "orchestrator"],
                &Doc::Stdin("{}"),
                &[],
            ),
        ),
        (
            "qualified builtin member update",
            mutate(
                &sb,
                true,
                "update",
                &["builtin:a-team", "--agent", "orchestrator"],
                &Doc::Stdin(r#"{"name":"orchestrator","role":"r"}"#),
                &[],
            ),
        ),
        (
            "builtin root patch",
            mutate(
                &sb,
                false,
                "patch",
                &["builtin:a-team"],
                &Doc::Stdin("{}"),
                &[],
            ),
        ),
        (
            "builtin root update",
            mutate(
                &sb,
                false,
                "update",
                &["a-team"],
                &Doc::Stdin(r#"{"schema_version":1,"name":"a-team","lead":"orchestrator"}"#),
                &[],
            ),
        ),
        (
            "legacy preset patch",
            mutate(
                &sb,
                false,
                "patch",
                &["builtin:startup"],
                &Doc::Stdin("{}"),
                &[],
            ),
        ),
        (
            "unknown template",
            mutate(
                &sb,
                false,
                "patch",
                &["no-such-team"],
                &Doc::Stdin("{}"),
                &[],
            ),
        ),
    ];
    for (what, output) in &runs {
        assert_rejected(output, what);
    }
    assert!(
        runs[..5]
            .iter()
            .all(|(_, o)| combined(o).contains("read-only") || combined(o).contains("Builtins")),
        "builtin refusals must explain themselves: {:?}",
        runs.iter().map(|(_, o)| transcript(o)).collect::<Vec<_>>()
    );
    assert!(
        !sb.user_templates().exists(),
        "a refused builtin mutation must not create a shadow copy"
    );
}

#[test]
fn mutations_need_no_project_and_create_no_project_state() {
    let sb = sandbox();
    let root = small_team(&sb, "noproj");
    echoed(patch_agent(
        &sb,
        &root,
        "worker",
        &json!({"description": "x"}),
    ));
    echoed(update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    ));
    echoed(patch_org(&sb, &root, &json!({"description": "x"})));
    echoed(update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "noproj", "lead": "lead"}),
    ));
    assert!(!sb.project().join(".orchestry").exists());
}

#[test]
fn a_missing_input_file_or_a_directory_as_input_fails_unchanged() {
    let sb = sandbox();
    let root = small_team(&sb, "inputs");
    let frozen = freeze(&root);
    let sel = agent_sel(&root, "worker");
    let missing = sb.work().join("nope.json");
    let work = sb.work();
    let a = mutate(&sb, true, "patch", &sel, &Doc::File(&missing), &[]);
    let b = mutate(&sb, true, "update", &sel, &Doc::File(&work), &[]);
    assert_rejected(&a, "missing input");
    assert_rejected(&b, "directory input");
    assert_untouched(&root, &frozen, "bad input path");
}

// ═══ Hostile input documents ════════════════════════════════════════════════

fn hostile_inputs() -> Vec<(&'static str, &'static str, Vec<u8>, &'static str)> {
    // (label, extension, bytes, expected reason fragment)
    let deep = format!("description: {}1{}", "[".repeat(65), "]".repeat(65));
    let mut bomb = String::from("description: x\nbomb:\n  - &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..=9 {
        bomb.push_str(&format!(
            "  - &a{level} [{}]\n",
            vec![format!("*a{}", level - 1); 10].join(", ")
        ));
    }
    let mut big = b"description: ".to_vec();
    big.extend(std::iter::repeat_n(b'x', 1024 * 1024 + 1));
    vec![
        ("empty document", "yml", Vec::new(), "expected an object"),
        ("null root", "yml", b"null\n".to_vec(), "expected an object"),
        ("array root", "json", b"[1]".to_vec(), "expected an object"),
        (
            "scalar root",
            "yml",
            b"just text\n".to_vec(),
            "expected an object",
        ),
        (
            "two YAML documents",
            "yml",
            b"description: a\n---\ndescription: b\n".to_vec(),
            "exactly one YAML document",
        ),
        (
            "duplicate YAML keys",
            "yml",
            b"description: a\ndescription: b\n".to_vec(),
            "invalid YAML",
        ),
        (
            "duplicate JSON keys",
            "json",
            br#"{"description":"a","description":"b"}"#.to_vec(),
            "invalid JSON",
        ),
        (
            "explicit YAML tag",
            "yml",
            b"description: !!binary aGk=\n".to_vec(),
            "tags are not allowed",
        ),
        (
            "non-string key",
            "yml",
            b"description: a\n1: two\n".to_vec(),
            "keys must be strings",
        ),
        (
            "nonfinite number",
            "yml",
            b"max_turns: .nan\n".to_vec(),
            "nonfinite",
        ),
        ("nesting deeper than 64", "yml", deep.into_bytes(), "depth"),
        (
            "alias expansion bomb",
            "yml",
            bomb.into_bytes(),
            "invalid YAML",
        ),
        ("document over 1 MiB", "yml", big, "1 MiB"),
    ]
}

#[test]
fn hostile_documents_are_refused_by_every_mutation_and_leave_the_template_unchanged() {
    let sb = sandbox();
    let root = small_team(&sb, "hostile");
    let frozen = freeze(&root);
    for (label, ext, bytes, reason) in hostile_inputs() {
        let file = sb.work().join(format!("hostile.{ext}"));
        fs::write(&file, &bytes).unwrap();
        let started = Instant::now();
        for (members, verb, sel) in [
            (true, "update", agent_sel(&root, "worker")),
            (true, "patch", agent_sel(&root, "worker")),
            (false, "update", org_sel(&root)),
            (false, "patch", org_sel(&root)),
        ] {
            let what = format!(
                "{label} / {verb} {}",
                if members { "member" } else { "org" }
            );
            let output = mutate(&sb, members, verb, &sel, &Doc::File(&file), &[]);
            assert_rejected(&output, &what);
            assert_mentions(&output, path_arg(&file), &what);
            assert_mentions(&output, reason, &what);
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "[{label}] was not prompt"
        );
    }
    assert_untouched(&root, &frozen, "hostile documents");
}

#[test]
fn hostile_documents_on_stdin_are_refused_and_name_stdin() {
    let sb = sandbox();
    let root = small_team(&sb, "hostile-stdin");
    let frozen = freeze(&root);
    for (label, text) in [
        ("two documents", "description: a\n---\ndescription: b\n"),
        ("duplicate keys", "description: a\ndescription: b\n"),
        ("tag", "description: !!str x\n"),
        ("array", "[1, 2]"),
        ("empty", ""),
    ] {
        let output = mutate(
            &sb,
            true,
            "patch",
            &agent_sel(&root, "worker"),
            &Doc::Stdin(text),
            &[],
        );
        assert_rejected(&output, label);
        assert_mentions(&output, "<stdin>", label);
    }
    assert_untouched(&root, &frozen, "hostile stdin");
}

#[test]
fn explicit_null_members_in_a_full_replacement_are_rejected_at_every_level() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let org = sb.authored(&root)["org"].clone();
    let own = shown_member(&sb, &root, "own");
    let mut cases: Vec<(String, Output)> = Vec::new();
    for key in ["description", "rules", "defaults"] {
        let mut doc = org.clone();
        doc[key] = Value::Null;
        cases.push((format!("org {key}"), update_org(&sb, &root, &doc)));
    }
    let mut doc = org.clone();
    doc["defaults"]["adapter"] = Value::Null;
    cases.push(("org defaults.adapter".into(), update_org(&sb, &root, &doc)));
    for key in ["role", "description", "rules", "model", "skills"] {
        let mut doc = own.clone();
        doc[key] = Value::Null;
        cases.push((
            format!("member {key}"),
            update_agent(&sb, &root, "own", &doc),
        ));
    }
    for (what, output) in &cases {
        assert_rejected(output, what);
        assert_mentions(output, "null", what);
    }
    assert_untouched(&root, &frozen, "null in full replacement");
}

// ═══ R1 — repair of a bounded malformed target ═════════════════════════════

const BROKEN: &str = "broken: [\n";

fn corrupt_member(root: &Path, name: &str) {
    fs::write(member_path(root, name), BROKEN).unwrap();
}

fn changed_paths(root: &Path, before: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let after = snapshot(root);
    let mut paths: Vec<String> = after
        .iter()
        .filter(|(p, b)| before.get(*p) != Some(*b))
        .map(|(p, _)| p.clone())
        .chain(before.keys().filter(|p| !after.contains_key(*p)).cloned())
        .collect();
    paths.sort();
    paths
}

#[test]
fn a_full_update_repairs_a_malformed_member_and_touches_only_that_file() {
    let sb = sandbox();
    let root = small_team(&sb, "repair");
    corrupt_member(&root, "worker");
    let before = snapshot(&root);
    let shown = sb.run(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    assert_rejected(&shown, "show of a broken template");
    let repaired = echoed(update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "repaired role"}),
    ));
    assert_eq!(
        (
            repaired["role"].clone(),
            changed_paths(&root, &before),
            names(&sb.authored(&root)["agents"]),
        ),
        (
            json!("repaired role"),
            vec!["agents/worker.yml".to_owned()],
            vec![
                "lead".to_owned(),
                "reviewer".to_owned(),
                "worker".to_owned()
            ],
        )
    );
}

#[test]
fn a_repair_with_the_wrong_member_name_is_rejected_and_the_corrupt_bytes_stay() {
    let sb = sandbox();
    let root = small_team(&sb, "repair-name");
    corrupt_member(&root, "worker");
    let frozen = freeze(&root);
    let output = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "reviewer", "role": "r"}),
    );
    assert_rejected(&output, "name of another member");
    assert_untouched(&root, &frozen, "wrong replacement name");
}

#[test]
fn member_identity_comes_from_the_filename_never_from_the_corrupt_old_content() {
    let sb = sandbox();
    let root = small_team(&sb, "repair-identity");
    // Parseable, claims another name, but invalid (unknown field).
    fs::write(
        member_path(&root, "worker"),
        "name: reviewer\nrole: x\nbogus: 1\n",
    )
    .unwrap();
    let frozen = freeze(&root);
    let claimed = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "reviewer", "role": "r"}),
    );
    assert_rejected(&claimed, "adopting the corrupt content's name");
    assert_untouched(&root, &frozen, "corrupt content identity");
    echoed(update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    ));
    assert_eq!(
        stored(&member_path(&root, "worker"))["name"],
        json!("worker")
    );
}

#[test]
fn a_repair_is_refused_while_another_member_is_also_malformed() {
    let sb = sandbox();
    let root = small_team(&sb, "repair-two");
    corrupt_member(&root, "worker");
    corrupt_member(&root, "reviewer");
    let frozen = freeze(&root);
    let output = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    );
    assert_rejected(&output, "repair beside unrelated corruption");
    assert_mentions(&output, "reviewer", "names the unrelated corruption");
    assert_untouched(&root, &frozen, "unrelated corruption");
}

#[test]
fn a_repair_is_refused_while_the_org_is_malformed() {
    let sb = sandbox();
    let root = small_team(&sb, "repair-org-bad");
    corrupt_member(&root, "worker");
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    let frozen = freeze(&root);
    let output = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    );
    assert_rejected(&output, "member repair beside a broken org");
    assert_untouched(&root, &frozen, "broken org");
}

#[test]
fn a_patch_of_a_malformed_member_is_refused_with_repair_guidance_and_changes_nothing() {
    let sb = sandbox();
    let root = small_team(&sb, "patch-bad");
    corrupt_member(&root, "worker");
    let frozen = freeze(&root);
    let output = patch_agent(&sb, &root, "worker", &json!({"description": "x"}));
    assert_rejected(&output, "patch of a malformed member");
    assert_mentions(&output, "template update", "repair guidance");
    let empty = patch_agent(&sb, &root, "worker", &json!({}));
    assert_rejected(&empty, "empty patch of a malformed member");
    assert_untouched(&root, &frozen, "patch of a malformed member");
}

#[test]
fn a_patch_of_a_malformed_org_is_refused_with_repair_guidance_and_changes_nothing() {
    let sb = sandbox();
    let root = small_team(&sb, "patch-org-bad");
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    let frozen = freeze(&root);
    let output = patch_org(&sb, &root, &json!({"description": "x"}));
    assert_rejected(&output, "patch of a malformed org");
    assert_mentions(&output, "template update", "repair guidance");
    assert_untouched(&root, &frozen, "patch of a malformed org");
}

fn fake_editor(sb: &Sandbox, content: &str) -> (PathBuf, PathBuf) {
    let script = sb.dir.path().join("editor.sh");
    let record = sb.dir.path().join("editor-ran.txt");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\ncat > \"$1\" <<'ORCH_EDITOR_EOF'\n{content}\nORCH_EDITOR_EOF\n",
            record.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, record)
}

fn with_editor(sb: &Sandbox, editor: &Path, args: &[&str]) -> Output {
    let mut command = sb.command();
    command
        .args(args)
        .env("EDITOR", editor)
        .env("VISUAL", editor);
    run_bounded(&mut command, None)
}

#[test]
fn editing_a_malformed_member_or_org_gives_repair_guidance_and_never_starts_the_editor() {
    let sb = sandbox();
    let root = small_team(&sb, "edit-bad");
    corrupt_member(&root, "worker");
    let (editor, record) = fake_editor(&sb, r#"{"name":"worker","role":"edited"}"#);
    let member = with_editor(
        &sb,
        &editor,
        &[
            "org",
            "template",
            "agent",
            "edit",
            "--template-dir",
            path_arg(&root),
            "--agent",
            "worker",
        ],
    );
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    let org = with_editor(
        &sb,
        &editor,
        &["org", "template", "edit", "--template-dir", path_arg(&root)],
    );
    assert_rejected(&member, "edit of a malformed member");
    assert_rejected(&org, "edit of a malformed org");
    assert_mentions(&member, "template update", "member edit guidance");
    assert_mentions(&org, "template update", "org edit guidance");
    assert!(
        !record.exists(),
        "the editor must not start without a parseable base"
    );
}

#[test]
fn an_edit_that_stores_an_explicit_null_is_rejected_and_the_member_is_unchanged() {
    let sb = sandbox();
    let root = skills_team(&sb);
    let frozen = freeze(&root);
    let (editor, record) = fake_editor(&sb, r#"{"name":"own","role":"r","skills":null}"#);
    let output = with_editor(
        &sb,
        &editor,
        &[
            "org",
            "template",
            "agent",
            "edit",
            "--template-dir",
            path_arg(&root),
            "--agent",
            "own",
        ],
    );
    assert_rejected(&output, "edit storing null");
    assert!(record.exists(), "the editor did run on a parseable member");
    assert_untouched(&root, &frozen, "edit storing null");
}

#[test]
fn malformed_org_repair_in_a_user_root_must_keep_the_directory_key() {
    let sb = sandbox();
    let root = user_team(&sb, "mine");
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    let frozen = freeze(&root);
    let renamed = mutate(
        &sb,
        false,
        "update",
        &["mine"],
        &Doc::Stdin(&json!({"schema_version": 1, "name": "other", "lead": "lead"}).to_string()),
        &["--json"],
    );
    assert_rejected(&renamed, "user-root repair under another name");
    assert_untouched(&root, &frozen, "user-root repair under another name");
    let repaired = mutate(
        &sb,
        false,
        "update",
        &["mine"],
        &Doc::Stdin(&json!({"schema_version": 1, "name": "mine", "lead": "lead"}).to_string()),
        &["--json"],
    );
    assert!(repaired.status.success(), "{}", transcript(&repaired));
    assert_eq!(stored(&root.join("org.yml"))["name"], json!("mine"));
}

#[test]
fn malformed_org_repair_in_an_explicit_directory_accepts_the_replacement_name() {
    let sb = sandbox();
    let root = small_team(&sb, "dir-name");
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    echoed(update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "chosen", "lead": "lead"}),
    ));
    assert_eq!(sb.authored(&root)["org"]["name"], json!("chosen"));
}

#[test]
fn malformed_org_repair_cannot_install_an_unsupported_schema_version() {
    let sb = sandbox();
    let root = small_team(&sb, "repair-schema");
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    let frozen = freeze(&root);
    let output = update_org(
        &sb,
        &root,
        &json!({"schema_version": 2, "name": "x", "lead": "lead"}),
    );
    assert_rejected(&output, "repair to schema 2");
    assert_untouched(&root, &frozen, "repair to schema 2");
}

#[test]
fn a_malformed_org_repair_validates_every_retained_member() {
    let sb = sandbox();
    let root = small_team(&sb, "repair-org-members");
    fs::write(root.join("org.yml"), BROKEN).unwrap();
    corrupt_member(&root, "worker");
    let frozen = freeze(&root);
    let output = update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "repair-org-members", "lead": "lead"}),
    );
    assert_rejected(&output, "org repair beside a malformed member");
    assert_untouched(&root, &frozen, "org repair beside a malformed member");
}

#[test]
fn a_parseable_root_with_an_unsupported_schema_cannot_be_downgraded_or_renamed() {
    let sb = sandbox();
    let root = user_team(&sb, "mine");
    fs::write(
        root.join("org.yml"),
        "schema_version: 2\nname: mine\nlead: lead\n",
    )
    .unwrap();
    let frozen = freeze(&root);
    let attempts = [
        (
            "update to schema 1",
            json!({"schema_version": 1, "name": "mine", "lead": "lead"}),
            "immutable",
        ),
        (
            "update to another name",
            json!({"schema_version": 1, "name": "other", "lead": "lead"}),
            "name",
        ),
    ];
    for (what, doc, fragment) in attempts {
        let output = mutate(
            &sb,
            false,
            "update",
            &["mine"],
            &Doc::Stdin(&doc.to_string()),
            &[],
        );
        assert_rejected(&output, what);
        assert_mentions(&output, fragment, what);
    }
    let patched = mutate(
        &sb,
        false,
        "patch",
        &["mine"],
        &Doc::Stdin(r#"{"schema_version":1}"#),
        &[],
    );
    assert_rejected(&patched, "patch to schema 1");
    assert_untouched(&root, &frozen, "unsupported parseable schema");
}

#[test]
fn a_parseable_root_with_a_bad_field_cannot_use_repair_to_change_its_identity() {
    let sb = sandbox();
    // An explicit directory has no directory key to pin the name, so only the
    // old root's own identity can stop a rename here.
    let root = small_team(&sb, "bad-field");
    // Parseable and carries an explicit null, which the strict loader rejects:
    // it needs repair, but its name must still not be changeable.
    fs::write(
        root.join("org.yml"),
        "schema_version: 1\nname: bad-field\nlead: lead\ndescription: null\n",
    )
    .unwrap();
    let frozen = freeze(&root);
    let renamed = update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "other", "lead": "lead"}),
    );
    assert_rejected(&renamed, "rename through a nullable-field repair");
    assert_mentions(
        &renamed,
        "immutable",
        "rename through a nullable-field repair",
    );
    assert_untouched(&root, &frozen, "rename through a nullable-field repair");
    let repaired = update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "bad-field", "lead": "lead"}),
    );
    assert!(repaired.status.success(), "{}", transcript(&repaired));
}

// ── Safety failures are never repair exceptions ─────────────────────────────

#[test]
fn a_symlinked_member_is_refused_and_the_outside_file_is_untouched() {
    let sb = sandbox();
    let root = small_team(&sb, "link-member");
    let outside = sb.work().join("outside.yml");
    fs::write(&outside, "name: worker\nrole: outside\n").unwrap();
    fs::remove_file(member_path(&root, "worker")).unwrap();
    symlink(&outside, member_path(&root, "worker")).unwrap();
    let frozen = freeze(&root);
    let outside_before = fs::read(&outside).unwrap();
    let on_target = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    );
    let on_sibling = patch_agent(&sb, &root, "reviewer", &json!({"description": "x"}));
    assert_rejected(&on_target, "update of a symlinked member");
    assert_rejected(&on_sibling, "patch beside a symlinked member");
    assert_eq!(fs::read(&outside).unwrap(), outside_before);
    assert_eq!(snapshot(&root), frozen.bytes);
}

#[test]
fn a_symlinked_agents_directory_or_root_is_refused() {
    let sb = sandbox();
    let real = small_team(&sb, "real");
    let via_agents = small_team(&sb, "via-agents");
    let moved = sb.work().join("moved-agents");
    fs::rename(via_agents.join("agents"), &moved).unwrap();
    symlink(&moved, via_agents.join("agents")).unwrap();
    let link = sb.work().join("root-link");
    symlink(&real, &link).unwrap();
    let real_frozen = freeze(&real);
    let moved_before = snapshot(&moved);
    let agents_link = patch_agent(&sb, &via_agents, "worker", &json!({"description": "x"}));
    let root_link = patch_agent(&sb, &link, "worker", &json!({"description": "x"}));
    assert_rejected(&agents_link, "symlinked agents directory");
    assert_rejected(&root_link, "symlinked template root");
    assert_untouched(&real, &real_frozen, "real directory behind a root link");
    assert_eq!(snapshot(&moved), moved_before);
}

#[test]
fn a_symlinked_user_template_is_refused_by_name_too() {
    let sb = sandbox();
    let real = small_team(&sb, "real-user");
    fs::create_dir_all(sb.user_templates()).unwrap();
    symlink(&real, sb.user_templates().join("linked")).unwrap();
    let frozen = freeze(&real);
    let output = mutate(
        &sb,
        true,
        "patch",
        &["linked", "--agent", "worker"],
        &Doc::Stdin("{}"),
        &[],
    );
    assert_rejected(&output, "symlinked user template");
    assert_untouched(&real, &frozen, "symlink target");
}

#[test]
fn an_oversized_existing_member_cannot_be_repaired_through_update() {
    let sb = sandbox();
    let root = small_team(&sb, "oversized");
    let mut huge = b"name: worker\nrole: x\n# ".to_vec();
    huge.extend(std::iter::repeat_n(b'x', 1024 * 1024));
    fs::write(member_path(&root, "worker"), &huge).unwrap();
    let frozen = freeze(&root);
    let on_target = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    );
    let on_sibling = patch_agent(&sb, &root, "reviewer", &json!({"description": "x"}));
    assert_rejected(&on_target, "update of an oversized member");
    assert_mentions(&on_target, "1 MiB", "names the limit");
    assert_rejected(&on_sibling, "patch beside an oversized member");
    assert_untouched(&root, &frozen, "oversized member");
}

#[test]
fn an_oversized_candidate_is_refused_before_anything_is_written() {
    let sb = sandbox();
    let root = small_team(&sb, "oversized-candidate");
    let frozen = freeze(&root);
    let doc = json!({"name": "worker", "role": "x".repeat(1024 * 1024)});
    let file = input_file(&sb, "huge.json", &doc.to_string());
    let output = mutate(
        &sb,
        true,
        "update",
        &agent_sel(&root, "worker"),
        &Doc::File(&file),
        &[],
    );
    assert_rejected(&output, "candidate over 1 MiB");
    assert_untouched(&root, &frozen, "candidate over 1 MiB");
}

#[test]
fn a_symlinked_org_file_is_refused_and_the_outside_file_is_untouched() {
    let sb = sandbox();
    let root = small_team(&sb, "link-org");
    let outside = sb.work().join("outside-org.yml");
    fs::rename(root.join("org.yml"), &outside).unwrap();
    symlink(&outside, root.join("org.yml")).unwrap();
    let frozen = freeze(&root);
    let outside_before = fs::read(&outside).unwrap();
    let org = update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "link-org", "lead": "lead"}),
    );
    let member = patch_agent(&sb, &root, "worker", &json!({"description": "x"}));
    assert_rejected(&org, "update of a symlinked org.yml");
    assert_rejected(&member, "member patch beside a symlinked org.yml");
    assert_eq!(fs::read(&outside).unwrap(), outside_before);
    assert_eq!(snapshot(&root), frozen.bytes);
}

#[test]
fn an_oversized_existing_org_file_cannot_be_repaired_through_update() {
    let sb = sandbox();
    let root = small_team(&sb, "huge-org");
    let mut huge = b"schema_version: 1\nname: huge-org\nlead: lead\n# ".to_vec();
    huge.extend(std::iter::repeat_n(b'x', 1024 * 1024));
    fs::write(root.join("org.yml"), &huge).unwrap();
    let frozen = freeze(&root);
    let output = update_org(
        &sb,
        &root,
        &json!({"schema_version": 1, "name": "huge-org", "lead": "lead"}),
    );
    assert_rejected(&output, "update of an oversized org.yml");
    assert_mentions(&output, "1 MiB", "names the limit");
    assert_untouched(&root, &frozen, "oversized org.yml");
}

#[test]
fn nested_or_misnamed_entries_make_every_mutation_fail_unchanged() {
    let sb = sandbox();
    let nested = small_team(&sb, "nested");
    fs::create_dir(nested.join("agents/sub")).unwrap();
    let yaml_ext = small_team(&sb, "yaml-ext");
    fs::write(yaml_ext.join("agents/extra.yaml"), "name: extra\nrole: x\n").unwrap();
    for (what, root) in [
        ("nested directory", &nested),
        ("agents/*.yaml member", &yaml_ext),
    ] {
        let frozen = freeze(root);
        let patch = patch_agent(&sb, root, "worker", &json!({"description": "x"}));
        let update = update_org(
            &sb,
            root,
            &json!({"schema_version": 1, "name": root.file_name().unwrap().to_str().unwrap(), "lead": "lead"}),
        );
        assert_rejected(&patch, what);
        assert_rejected(&update, what);
        assert_untouched(root, &frozen, what);
    }
}

// ═══ Complete-bundle validation, even for no-ops ════════════════════════════

fn broken_bundles(sb: &Sandbox) -> Vec<(&'static str, PathBuf)> {
    let malformed = small_team(sb, "b-malformed");
    corrupt_member(&malformed, "reviewer");
    let stem = small_team(sb, "b-stem");
    fs::write(
        member_path(&stem, "reviewer"),
        "name: someone-else\nrole: x\n",
    )
    .unwrap();
    let no_lead = small_team(sb, "b-no-lead");
    fs::remove_file(member_path(&no_lead, "lead")).unwrap();
    let orphan_rule = small_team(sb, "b-orphan-rule");
    fs::write(orphan_rule.join("rules/stray.md"), "unregistered").unwrap();
    let unknown_key = small_team(sb, "b-unknown-key");
    fs::write(
        member_path(&unknown_key, "reviewer"),
        r#"{"name":"reviewer","role":"x","typo":1}"#,
    )
    .unwrap();
    vec![
        ("an unrelated malformed member", malformed),
        ("a member whose name differs from its filename", stem),
        ("a lead that does not exist", no_lead),
        ("a rule asset nobody registered", orphan_rule),
        ("an unrelated member with an unknown key", unknown_key),
    ]
}

#[test]
fn an_empty_patch_still_validates_the_whole_bundle() {
    let sb = sandbox();
    for (what, root) in broken_bundles(&sb) {
        let frozen = freeze(&root);
        let member = patch_agent(&sb, &root, "worker", &json!({}));
        let org = patch_org(&sb, &root, &json!({}));
        assert_rejected(&member, &format!("empty member patch beside {what}"));
        assert_rejected(&org, &format!("empty org patch beside {what}"));
        assert_untouched(&root, &frozen, what);
    }
}

#[test]
fn an_identical_full_update_still_validates_the_whole_bundle() {
    let sb = sandbox();
    for (what, root) in broken_bundles(&sb) {
        let frozen = freeze(&root);
        let echo = stored(&member_path(&root, "worker"));
        let output = update_agent(&sb, &root, "worker", &echo);
        assert_rejected(&output, &format!("identical update beside {what}"));
        assert_untouched(&root, &frozen, what);
    }
}

// ═══ D3 — references, cycles and the extraction header survive mutation ═════

fn rule_team(sb: &Sandbox, name: &str) -> PathBuf {
    Bundle::new(name, "solo")
        .rule("alpha", "Alpha body. See .claude/agent-rules/beta.md.")
        .rule("beta", "Beta body. See .claude/agent-rules/alpha.md.")
        .agent("solo", json!({"role": "Plain role."}))
        .write(&sb.work().join(name))
}

fn solo_role(sb: &Sandbox, root: &Path) -> String {
    sb.resolved(root)["solo"]["role"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn a_role_updated_to_reference_a_cyclic_rule_embeds_each_rule_once() {
    let sb = sandbox();
    let root = rule_team(&sb, "cycle");
    echoed(update_agent(
        &sb,
        &root,
        "solo",
        &json!({"name": "solo", "role": "Obey .claude/agent-rules/alpha.md."}),
    ));
    let role = solo_role(&sb, &root);
    assert_eq!(
        (
            role.matches("Alpha body.").count(),
            role.matches("Beta body.").count()
        ),
        (1, 1)
    );
}

#[test]
fn an_explicit_empty_rule_selection_does_not_suppress_a_referenced_rule() {
    let sb = sandbox();
    let root = rule_team(&sb, "empty-sel");
    echoed(update_agent(
        &sb,
        &root,
        "solo",
        &json!({"name": "solo", "role": "Obey .claude/agent-rules/alpha.md.", "rules": []}),
    ));
    assert!(solo_role(&sb, &root).contains("Alpha body."));
}

#[test]
fn selecting_a_rule_by_patch_embeds_its_whole_cyclic_closure_in_registry_order() {
    let sb = sandbox();
    let root = rule_team(&sb, "select");
    echoed(patch_agent(&sb, &root, "solo", &json!({"rules": ["beta"]})));
    let role = solo_role(&sb, &root);
    let (alpha, beta) = (
        role.find("Alpha body.").unwrap(),
        role.find("Beta body.").unwrap(),
    );
    assert!(alpha < beta, "registry order is alpha then beta:\n{role}");
}

#[test]
fn a_patch_that_does_not_touch_rules_leaves_the_resolved_role_byte_identical() {
    let sb = sandbox();
    let root = rule_team(&sb, "stable");
    echoed(patch_agent(
        &sb,
        &root,
        "solo",
        &json!({"rules": ["alpha"]}),
    ));
    let before = solo_role(&sb, &root);
    echoed(patch_agent(
        &sb,
        &root,
        "solo",
        &json!({"description": "new words", "skills": ["x"]}),
    ));
    echoed(patch_org(&sb, &root, &json!({"description": "also new"})));
    assert_eq!(solo_role(&sb, &root), before);
}

#[test]
fn bare_rule_names_and_unknown_rule_paths_are_refused_by_update_and_patch() {
    let sb = sandbox();
    let root = rule_team(&sb, "refs");
    let frozen = freeze(&root);
    let cases = [
        (
            "bare registered name",
            "See alpha.md for details.",
            "use .claude/agent-rules/alpha.md",
        ),
        (
            "bare built-in name",
            "See stance.md for details.",
            "use .claude/agent-rules/stance.md",
        ),
        (
            "unknown rule path",
            "See .claude/agent-rules/nope.md.",
            "nope",
        ),
    ];
    for (what, role, fragment) in cases {
        let patched = patch_agent(&sb, &root, "solo", &json!({"role": role}));
        let updated = update_agent(&sb, &root, "solo", &json!({"name": "solo", "role": role}));
        assert_rejected(&patched, what);
        assert_rejected(&updated, what);
        assert_mentions(&patched, fragment, what);
        assert_mentions(&updated, fragment, what);
    }
    assert_untouched(&root, &frozen, "ambiguous rule references");
}

#[test]
fn the_extraction_header_is_rewritten_and_the_disagreement_clause_survives_an_update() {
    let sb = sandbox();
    let root = rule_team(&sb, "header");
    let role = "Senior practitioner.\n\nRules (relative to your workspace root): .claude/agent-rules/alpha.md and .claude/agent-rules/beta.md (its **Disagreement** section was written for you).\n";
    echoed(update_agent(
        &sb,
        &root,
        "solo",
        &json!({"name": "solo", "role": role}),
    ));
    let resolved = solo_role(&sb, &root);
    assert!(resolved.contains("Rules (embedded in this role): embedded rule «alpha» and embedded rule «beta» (its **Disagreement** section was written for you)."), "{resolved}");
    assert!(!resolved.contains(".claude/agent-rules/"), "{resolved}");
    assert!(
        !resolved.contains("relative to your workspace root"),
        "{resolved}"
    );
}

fn context_team(sb: &Sandbox, name: &str) -> PathBuf {
    Bundle::new(name, "solo")
        .rule("marked", "Put output under <workspace root>/out.")
        .rule("plain", "Be plain.")
        .agent("solo", json!({"role": "Plain role."}))
        .write(&sb.work().join(name))
}

const CONTEXT_SENTENCE: &str = "means the owning orch project root";

#[test]
fn an_empty_closure_returns_the_authored_role_bytes_even_with_a_placeholder() {
    let sb = sandbox();
    let root = context_team(&sb, "ctx-empty");
    let authored = "Write to <workspace root>/x and nothing else.";
    echoed(update_agent(
        &sb,
        &root,
        "solo",
        &json!({"name": "solo", "role": authored}),
    ));
    assert_eq!(solo_role(&sb, &root), authored);
}

#[test]
fn a_registered_but_unselected_placeholder_rule_adds_only_the_generic_prefix() {
    let sb = sandbox();
    let root = context_team(&sb, "ctx-unselected");
    echoed(patch_agent(
        &sb,
        &root,
        "solo",
        &json!({"rules": ["plain"]}),
    ));
    let role = solo_role(&sb, &root);
    assert!(
        role.starts_with(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required."
        ) && !role.contains(CONTEXT_SENTENCE)
            && !role.contains("Put output under"),
        "{role}"
    );
}

#[test]
fn selecting_or_authoring_the_placeholder_defines_the_root_in_a_renamed_team() {
    let sb = sandbox();
    let root = context_team(&sb, "ctx-selected");
    echoed(patch_agent(
        &sb,
        &root,
        "solo",
        &json!({"rules": ["marked"]}),
    ));
    let by_rule = solo_role(&sb, &root);
    echoed(update_agent(
        &sb,
        &root,
        "solo",
        &json!({"name": "solo", "role": "Use <workspace root>/y.", "rules": ["plain"]}),
    ));
    let by_role = solo_role(&sb, &root);
    assert_eq!(
        (
            by_rule.contains(CONTEXT_SENTENCE),
            by_role.contains(CONTEXT_SENTENCE)
        ),
        (true, true)
    );
}

#[test]
fn a_renamed_a_team_copy_resolves_exactly_like_the_builtin_and_survives_echo_updates() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "my-team", "--from", "a-team"]);
    let copy = sb.user_templates().join("my-team");
    let builtin = sb.json(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    let renamed = sb.json(&["org", "template", "show", "my-team", "--resolved"]);
    // The root-definition sentence is added to every a-team member of the
    // builtin, but to a renamed copy only where a role or a selected rule
    // mentions the placeholder (F1). Everything else must be identical.
    let by_name = |shown: &Value| -> BTreeMap<String, String> {
        shown["resolved_agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["name"].as_str().unwrap().to_owned(),
                    a["role"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let (want, have) = (by_name(&builtin), by_name(&renamed));
    let sentence_start = want["orchestrator"]
        .find(" <workspace root> means")
        .expect("builtin sentence");
    let sentence_end = want["orchestrator"][sentence_start..]
        .find("other layouts.")
        .unwrap()
        + sentence_start
        + "other layouts.".len();
    let sentence = want["orchestrator"][sentence_start..sentence_end].to_owned();
    let mut equal = Vec::new();
    for (name, role) in &want {
        let stripped = role.replacen(&sentence, "", 1);
        assert!(
            have[name] == *role || have[name] == stripped,
            "{name}: the renamed copy differs from the builtin by more than the context sentence"
        );
        if have[name] == *role {
            equal.push(name.clone());
        }
    }
    assert_eq!(want.len(), 27);
    assert!(
        equal.contains(&"orchestrator".to_owned()) && equal.contains(&"tech-writer".to_owned()),
        "the placeholder-using roles must keep the root definition in a renamed copy; equal: {equal:?}"
    );
    assert_eq!(
        builtin["agents"], renamed["agents"],
        "authoring data of the copy equals the builtin"
    );
    let orchestrator = have["orchestrator"].clone();
    assert!(orchestrator.contains(CONTEXT_SENTENCE), "{orchestrator}");

    let frozen = freeze(&copy);
    let members = names(&renamed["agents"]);
    assert_eq!(members.len(), 27);
    for name in &members {
        let echo = renamed["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == *name)
            .unwrap()
            .clone();
        let output = mutate(
            &sb,
            true,
            "update",
            &["my-team", "--agent", name],
            &Doc::Stdin(&echo.to_string()),
            &["--json"],
        );
        assert!(output.status.success(), "{name}: {}", transcript(&output));
    }
    let org = renamed["org"].clone();
    let org_out = mutate(
        &sb,
        false,
        "update",
        &["my-team"],
        &Doc::Stdin(&org.to_string()),
        &["--json"],
    );
    assert!(org_out.status.success(), "{}", transcript(&org_out));
    assert_untouched(&copy, &frozen, "27 member echoes and the org echo");
    let after = sb.json(&["org", "template", "show", "my-team", "--resolved"]);
    assert_eq!(after["resolved_agents"], renamed["resolved_agents"]);
}

#[test]
fn the_red_team_member_keeps_its_disagreement_clause_after_a_patch_of_another_field() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "my-team", "--from", "a-team"]);
    let patched = mutate(
        &sb,
        true,
        "patch",
        &["my-team", "--agent", "red-team-guy"],
        &Doc::Stdin(r#"{"description":"retitled"}"#),
        &["--json"],
    );
    assert!(patched.status.success(), "{}", transcript(&patched));
    let shown = sb.json(&["org", "template", "show", "my-team", "--resolved"]);
    let role = shown["resolved_agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "red-team-guy")
        .unwrap()["role"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        role.contains("Disagreement")
            && role.contains("Rules (embedded in this role):")
            && !role.contains("Rules (relative to your workspace root):"),
        "{role}"
    );
}

// ═══ D11 — locking, concurrency and atomic failure ══════════════════════════

fn hold_lock(root: &Path) -> fs::File {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".template.lock"))
        .unwrap();
    lock.lock().expect("hold the template lock");
    lock
}

#[test]
fn update_and_patch_fail_after_the_lock_budget_while_another_writer_holds_the_lock() {
    let sb = sandbox();
    let root = small_team(&sb, "locked");
    let frozen = freeze(&root);
    let lock = hold_lock(&root);
    // A reader is never blocked by the writers' lock.
    let read_started = Instant::now();
    sb.ok(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    let read_in = read_started.elapsed();

    let started = Instant::now();
    let attempts: Vec<(&str, Output)> = std::thread::scope(|scope| {
        let handles = [
            (
                "member patch",
                scope.spawn(|| patch_agent(&sb, &root, "worker", &json!({"description": "x"}))),
            ),
            (
                "member update",
                scope.spawn(|| {
                    update_agent(
                        &sb,
                        &root,
                        "worker",
                        &json!({"name": "worker", "role": "r"}),
                    )
                }),
            ),
            (
                "org patch",
                scope.spawn(|| patch_org(&sb, &root, &json!({"description": "x"}))),
            ),
            (
                "org update",
                scope.spawn(|| {
                    update_org(
                        &sb,
                        &root,
                        &json!({"schema_version": 1, "name": "locked", "lead": "lead"}),
                    )
                }),
            ),
        ];
        handles.map(|(what, h)| (what, h.join().unwrap())).into()
    });
    let waited = started.elapsed();
    for (what, output) in &attempts {
        assert_rejected(output, what);
        assert_mentions(output, "lock", what);
    }
    assert_untouched(&root, &frozen, "lock timeout");
    assert!(
        waited >= Duration::from_secs(4) && waited < Duration::from_secs(40),
        "lock wait was {waited:?}"
    );
    assert!(
        read_in < Duration::from_secs(4),
        "a reader waited {read_in:?}"
    );

    drop(lock);
    let after = patch_agent(&sb, &root, "worker", &json!({"description": "released"}));
    assert!(after.status.success(), "{}", transcript(&after));
}

#[test]
fn concurrent_patches_of_different_keys_all_survive() {
    let sb = sandbox();
    let root = small_team(&sb, "concurrent");
    let results: Vec<bool> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let (sb, root) = (&sb, &root);
                scope.spawn(move || {
                    patch_agent(
                        sb,
                        root,
                        "worker",
                        &json!({"env": {format!("K{i}"): format!("v{i}")}}),
                    )
                    .status
                    .success()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let env = stored(&member_path(&root, "worker"))["env"].clone();
    let want: serde_json::Map<String, Value> = (0..6)
        .map(|i| (format!("K{i}"), json!(format!("v{i}"))))
        .collect();
    assert_eq!((results, env), (vec![true; 6], Value::Object(want)));
}

#[test]
fn concurrent_root_and_member_patches_all_land_and_leave_no_stray_entry() {
    let sb = sandbox();
    let root = small_team(&sb, "mixed");
    let results: Vec<bool> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for i in 0..3 {
            let (sb, root) = (&sb, &root);
            handles.push(scope.spawn(move || {
                patch_agent(
                    sb,
                    root,
                    "reviewer",
                    &json!({"env": {format!("M{i}"): "x"}}),
                )
                .status
                .success()
            }));
            handles.push(scope.spawn(move || {
                patch_org(
                    sb,
                    root,
                    &json!({"defaults": {"env": {format!("O{i}"): "y"}}}),
                )
                .status
                .success()
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let member_env = stored(&member_path(&root, "reviewer"))["env"]
        .as_object()
        .unwrap()
        .len();
    let org_env = stored(&root.join("org.yml"))["defaults"]["env"]
        .as_object()
        .unwrap()
        .len();
    assert_eq!(
        (
            results,
            member_env,
            org_env,
            stray_entries(&root.join("agents"))
        ),
        (vec![true; 6], 3, 3, Vec::<String>::new())
    );
}

#[test]
fn concurrent_patches_of_the_same_key_leave_exactly_one_winners_value() {
    let sb = sandbox();
    let root = small_team(&sb, "same-key");
    let results: Vec<bool> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let (sb, root) = (&sb, &root);
                scope.spawn(move || {
                    patch_agent(
                        sb,
                        root,
                        "worker",
                        &json!({"description": format!("from {i}")}),
                    )
                    .status
                    .success()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let description = stored(&member_path(&root, "worker"))["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        results.iter().all(|ok| *ok)
            && (0..5).any(|i| description == format!("from {i}"))
            && sb.authored(&root)["agents"].as_array().unwrap().len() == 3,
        "results {results:?}, description {description:?}"
    );
}

/// Readers take no lock (decision D11), so a read during a write must either
/// succeed or see a consistent bundle; it must never error on a writer's own
/// temporary file. `RawSnapshot::capture` lists `agents/` and a writer may
/// rename its `.<name>.yml.<hex>.tmp` away before the reader stats it, so this
/// pins that a vanished member is tolerated instead of surfacing
/// `Template I/O ...: No such file or directory`.
#[test]
fn a_reader_never_sees_a_truncated_member_while_writers_publish() {
    let sb = sandbox();
    let root = small_team(&sb, "atomic");
    let failures: Vec<String> = std::thread::scope(|scope| {
        let writers: Vec<_> = (0..6)
            .map(|i| {
                let (sb, root) = (&sb, &root);
                scope.spawn(move || {
                    (0..15).all(|round| {
                        patch_agent(
                            sb,
                            root,
                            "worker",
                            &json!({"description": "y".repeat(2000 + i * 10 + round)}),
                        )
                        .status
                        .success()
                    })
                })
            })
            .collect();
        let mut failures = Vec::new();
        for _ in 0..80 {
            let output = sb.run(&[
                "org",
                "template",
                "show",
                "--template-dir",
                path_arg(&root),
                "--json",
            ]);
            if !output.status.success() {
                failures.push(transcript(&output));
            }
        }
        for writer in writers {
            assert!(writer.join().unwrap(), "a writer failed");
        }
        failures
    });
    assert!(
        failures.is_empty(),
        "readers failed during publication: {failures:#?}"
    );
}

/// Restores permissions on drop so a failed assertion cannot leave an
/// undeletable directory behind.
struct Writable(PathBuf);

impl Drop for Writable {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn a_publish_that_cannot_write_fails_loudly_and_keeps_the_old_bytes() {
    let sb = sandbox();
    let root = small_team(&sb, "readonly");
    // Create the advisory lock while the directory is still writable.
    echoed(patch_agent(&sb, &root, "worker", &json!({})));
    let frozen = freeze(&root);
    let agents = root.join("agents");
    let _guard = Writable(agents.clone());
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(agents.join(".probe"), b"x").is_ok() {
        // Running as a user that ignores directory permissions.
        let _ = fs::remove_file(agents.join(".probe"));
        return;
    }
    let patched = patch_agent(&sb, &root, "worker", &json!({"description": "x"}));
    let updated = update_agent(
        &sb,
        &root,
        "worker",
        &json!({"name": "worker", "role": "r"}),
    );
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o755)).unwrap();
    assert_rejected(&patched, "patch into a read-only directory");
    assert_rejected(&updated, "update into a read-only directory");
    assert_untouched(&root, &frozen, "read-only agents directory");
    assert_eq!(stray_entries(&agents), Vec::<String>::new());
}

#[test]
fn a_dead_writers_lock_does_not_block_the_next_writer() {
    let sb = sandbox();
    let root = small_team(&sb, "stale-lock");
    // A leftover lock file from a killed process holds no OS lock.
    fs::write(root.join(".template.lock"), b"").unwrap();
    let output = patch_agent(
        &sb,
        &root,
        "worker",
        &json!({"description": "after a crash"}),
    );
    assert!(output.status.success(), "{}", transcript(&output));
    assert_eq!(
        stored(&member_path(&root, "worker"))["description"],
        json!("after a crash")
    );
}
