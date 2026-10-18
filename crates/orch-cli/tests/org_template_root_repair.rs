//! Repair of a parseable template root that lacks `name` or `schema_version`,
//! through the real `orch` binary.
//!
//! Regression for code-review finding F-1 of the org-template mutation slice:
//! a full `update` of `org.yml` is the documented way to repair a root the
//! strict loader rejects, but an absent `name` or `schema_version` used to be
//! reported as an "immutable" change, so the repair could never succeed.
//!
//! Contract pinned here (design decisions D2 / D11 / R1):
//!
//! * an *absent* `name` / `schema_version` is repaired by a full `update`,
//!   in a named user root and in an explicit `--template-dir`;
//! * an identity field that is *present* (including `null` or an unsupported
//!   schema) is still immutable, and a user root still pins `name` to its
//!   directory key;
//! * every other file is left byte- and mtime-identical, and a rejected
//!   attempt writes nothing at all.
//!
//! Every test owns a sandbox: throwaway `HOME`, throwaway global config (and
//! so user template root), a `PATH` that holds no agent CLI. No agent is ever
//! started and nothing outside the sandbox is read or written.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
        .prefix("orch-template-root-repair-")
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
            .current_dir(self.dir.path().join("project"))
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

    /// `show` as authoring data; the strict loader must accept the template.
    fn show(&self, selector: &[&str]) -> Value {
        let mut args = vec!["org", "template", "show"];
        args.extend(selector);
        args.push("--json");
        let output = self.run(&args);
        assert!(output.status.success(), "{}", transcript(&output));
        parse_json(&output)
    }
}

/// Run to completion under `RUN_BUDGET`, feeding `stdin`; a hang kills the
/// child and fails the test.
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

fn parse_json(output: &Output) -> Value {
    serde_json::from_str(&stdout(output))
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}", transcript(output)))
}

/// A clean, loud rejection by the mutation itself: exit 1-3 (never a panic or
/// a signal), the expected reason in the output, and not merely a command line
/// the binary does not know.
fn assert_rejected(output: &Output, reason: &str, what: &str) {
    assert!(
        matches!(output.status.code(), Some(1..=3)),
        "[{what}] should fail cleanly (exit 1-3), got {:?}\n{}",
        output.status.code(),
        transcript(output)
    );
    let text = format!("{}{}", stdout(output), stderr(output));
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
    assert!(
        text.contains(reason),
        "[{what}] expected the refusal to mention {reason:?}\n{}",
        transcript(output)
    );
}

// ── Template fixtures ──────────────────────────────────────────────────────

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("json")
}

/// A complete directory template: `lead`, `worker`, one rule file. Files are
/// written as pretty JSON (which is YAML), so the stored bytes can never equal
/// what the mutation writer emits and a rewrite is always visible.
fn write_team(root: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(root.join("agents")).expect("agents dir");
    fs::create_dir_all(root.join("rules")).expect("rules dir");
    fs::write(
        root.join("org.yml"),
        pretty(&org_doc(name, &json!({"rules": ["style"]}))),
    )
    .expect("org.yml");
    fs::write(root.join("rules").join("style.md"), "Keep it short.\n").expect("rule");
    for member in ["lead", "worker"] {
        fs::write(
            root.join("agents").join(format!("{member}.yml")),
            pretty(&json!({"name": member, "role": format!("{member} role")})),
        )
        .expect("agent file");
    }
    root.to_path_buf()
}

fn org_doc(name: &str, extra: &Value) -> Value {
    let mut doc = json!({
        "schema_version": 1,
        "name": name,
        "description": "fixture team",
        "lead": "lead",
    });
    for (key, value) in extra.as_object().expect("object") {
        doc[key] = value.clone();
    }
    doc
}

/// Every file under `root` by relative path, with its bytes.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("read_dir").flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            // The advisory lock carries no template data.
            if rel.ends_with(".template.lock") {
                continue;
            }
            if fs::symlink_metadata(&path).expect("metadata").is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                walk(&path, root, out);
            } else {
                out.insert(rel, fs::read(&path).expect("read"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Modification times of the regular files under `root`.
fn mtimes(root: &Path) -> BTreeMap<String, SystemTime> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, SystemTime>) {
        for entry in fs::read_dir(dir).expect("read_dir").flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.is_dir() {
                walk(&path, root, out);
            } else if !path.ends_with(".template.lock") {
                out.insert(
                    path.strip_prefix(root).unwrap().display().to_string(),
                    meta.modified().expect("mtime"),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Bytes and modification times, for "nothing was written" assertions.
struct Frozen {
    bytes: BTreeMap<String, Vec<u8>>,
    times: BTreeMap<String, SystemTime>,
}

/// Push every file far into the past (so any rewrite, even of identical bytes,
/// changes its mtime without the test sleeping), then record the state.
fn freeze(root: &Path) -> Frozen {
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for rel in mtimes(root).keys() {
        fs::OpenOptions::new()
            .write(true)
            .open(root.join(rel))
            .expect("open for age")
            .set_modified(past)
            .expect("set mtime");
    }
    Frozen {
        bytes: snapshot(root),
        times: mtimes(root),
    }
}

fn assert_untouched(root: &Path, frozen: &Frozen, what: &str) {
    assert_eq!(snapshot(root), frozen.bytes, "[{what}] bytes changed");
    assert_eq!(mtimes(root), frozen.times, "[{what}] a file was rewritten");
}

/// Everything except `org.yml` is unchanged, byte for byte and mtime for mtime.
fn assert_only_root_changed(root: &Path, frozen: &Frozen, what: &str) {
    let strip = |mut m: BTreeMap<String, Vec<u8>>| {
        m.remove("org.yml");
        m
    };
    assert_eq!(
        strip(snapshot(root)),
        strip(frozen.bytes.clone()),
        "[{what}] bytes"
    );
    let mut times = mtimes(root);
    let mut before = frozen.times.clone();
    times.remove("org.yml");
    before.remove("org.yml");
    assert_eq!(times, before, "[{what}] a sibling file was rewritten");
}

// ── Selection ──────────────────────────────────────────────────────────────

/// How a test addresses the template whose root it repairs.
#[derive(Clone, Copy, Debug)]
enum Select {
    /// `orch org template update <name>`: pinned to the user directory key.
    UserKey,
    /// `orch org template update --template-dir <dir>`: the root names itself.
    Directory,
}

/// Which identity field the damaged `org.yml` lacks.
#[derive(Clone, Copy)]
enum Missing {
    Name,
    Schema,
}

impl Missing {
    fn field(self) -> &'static str {
        match self {
            Missing::Name => "name",
            Missing::Schema => "schema_version",
        }
    }
}

struct Case {
    sb: Sandbox,
    root: PathBuf,
    select: Select,
    /// The name the template is selected by (user key) or created under.
    key: &'static str,
}

impl Case {
    fn new(select: Select) -> Self {
        let sb = sandbox();
        let key = "mine";
        let root = match select {
            Select::UserKey => write_team(&sb.user_templates().join(key), key),
            Select::Directory => write_team(&sb.work().join("some-dir"), key),
        };
        Case {
            sb,
            root,
            select,
            key,
        }
    }

    /// Replace `org.yml` with a parseable document and return the frozen tree.
    fn damage(&self, doc: &Value) -> Frozen {
        fs::write(self.root.join("org.yml"), pretty(doc)).expect("damage org.yml");
        freeze(&self.root)
    }

    fn selector(&self) -> Vec<&str> {
        match self.select {
            Select::UserKey => vec![self.key],
            Select::Directory => vec!["--template-dir", self.root.to_str().expect("utf-8")],
        }
    }

    fn mutate(&self, verb: &str, doc: &Value) -> Output {
        let mut args = vec!["org", "template", verb];
        args.extend(self.selector());
        args.extend(["-f", "-", "--json"]);
        self.sb.run_stdin(&args, doc.to_string().as_bytes())
    }

    fn update(&self, doc: &Value) -> Output {
        self.mutate("update", doc)
    }

    fn show(&self) -> Value {
        self.sb.show(&self.selector())
    }

    fn stored(&self) -> Value {
        serde_yaml_ng::from_str(&fs::read_to_string(self.root.join("org.yml")).expect("read"))
            .expect("stored org.yml is YAML")
    }
}

/// The valid full root document an author submits to repair the template.
fn repair_doc(name: &str) -> Value {
    org_doc(name, &json!({"rules": ["style"]}))
}

// ── Absent identity is repaired by a full update ───────────────────────────

fn assert_absent_field_repaired(select: Select, missing: Missing) {
    let case = Case::new(select);
    let mut damaged = org_doc(case.key, &json!({"rules": ["style"]}));
    damaged.as_object_mut().unwrap().remove(missing.field());
    let frozen = case.damage(&damaged);
    // A bystander user template must never be touched by the repair.
    let bystander = write_team(&case.sb.user_templates().join("bystander"), "bystander");
    let bystander_frozen = freeze(&bystander);

    // Only an absent name may be chosen freely, and only in an explicit
    // directory; otherwise the present name (the user directory key) stays.
    let name = match (select, missing) {
        (Select::Directory, Missing::Name) => "chosen-name",
        _ => case.key,
    };
    let output = case.update(&repair_doc(name));
    assert!(output.status.success(), "{}", transcript(&output));

    let echoed = parse_json(&output);
    let echoed_org = echoed.get("org").unwrap_or(&echoed);
    assert_eq!(echoed_org["name"], json!(name), "echoed name: {echoed}");
    assert_eq!(
        echoed_org["schema_version"],
        json!(1),
        "echoed schema: {echoed}"
    );
    assert_eq!(case.stored()["name"], json!(name));
    assert_eq!(case.stored()["schema_version"], json!(1));
    assert_eq!(case.show()["org"]["name"], json!(name), "strict reload");
    assert_only_root_changed(&case.root, &frozen, "repaired root");
    assert_untouched(&bystander, &bystander_frozen, "bystander template");
}

#[test]
fn full_update_repairs_an_absent_name_in_a_named_user_root() {
    assert_absent_field_repaired(Select::UserKey, Missing::Name);
}

#[test]
fn full_update_repairs_an_absent_name_in_an_explicit_template_dir() {
    assert_absent_field_repaired(Select::Directory, Missing::Name);
}

#[test]
fn full_update_repairs_an_absent_schema_version_in_a_named_user_root() {
    assert_absent_field_repaired(Select::UserKey, Missing::Schema);
}

#[test]
fn full_update_repairs_an_absent_schema_version_in_an_explicit_template_dir() {
    assert_absent_field_repaired(Select::Directory, Missing::Schema);
}

#[test]
fn a_repair_that_restates_the_same_document_twice_writes_once() {
    let case = Case::new(Select::UserKey);
    let mut damaged = repair_doc(case.key);
    damaged.as_object_mut().unwrap().remove("name");
    case.damage(&damaged);
    assert!(case.update(&repair_doc(case.key)).status.success());
    let repaired = freeze(&case.root);
    let again = case.update(&repair_doc(case.key));
    assert!(again.status.success(), "{}", transcript(&again));
    assert_untouched(&case.root, &repaired, "second identical update");
}

// ── Present identity stays immutable ───────────────────────────────────────

fn assert_present_identity_refused(
    select: Select,
    damaged: Value,
    submit: Value,
    reason: &str,
    what: &str,
) {
    let case = Case::new(select);
    let frozen = case.damage(&damaged);
    let output = case.update(&submit);
    assert_rejected(&output, reason, what);
    assert_untouched(&case.root, &frozen, what);
}

#[test]
fn absent_name_does_not_license_changing_a_present_unsupported_schema() {
    for select in [Select::UserKey, Select::Directory] {
        let mut damaged = repair_doc("mine");
        damaged.as_object_mut().unwrap().remove("name");
        damaged["schema_version"] = json!(2);
        assert_present_identity_refused(
            select,
            damaged,
            repair_doc("mine"),
            "immutable",
            &format!("{select:?}: absent name beside schema 2"),
        );
    }
}

#[test]
fn absent_name_does_not_license_changing_a_present_null_schema() {
    for select in [Select::UserKey, Select::Directory] {
        let mut damaged = repair_doc("mine");
        damaged.as_object_mut().unwrap().remove("name");
        damaged["schema_version"] = Value::Null;
        assert_present_identity_refused(
            select,
            damaged,
            repair_doc("mine"),
            "immutable",
            &format!("{select:?}: absent name beside null schema"),
        );
    }
}

#[test]
fn absent_schema_does_not_license_renaming_a_present_name() {
    for select in [Select::UserKey, Select::Directory] {
        let mut damaged = repair_doc("mine");
        damaged.as_object_mut().unwrap().remove("schema_version");
        assert_present_identity_refused(
            select,
            damaged,
            repair_doc("other"),
            // A user root is pinned to its key before identity is compared.
            match select {
                Select::UserKey => "user directory key",
                Select::Directory => "immutable",
            },
            &format!("{select:?}: absent schema with a renamed name"),
        );
    }
}

#[test]
fn absent_schema_does_not_license_replacing_a_present_null_name() {
    for select in [Select::UserKey, Select::Directory] {
        let mut damaged = repair_doc("mine");
        damaged.as_object_mut().unwrap().remove("schema_version");
        damaged["name"] = Value::Null;
        assert_present_identity_refused(
            select,
            damaged,
            repair_doc("mine"),
            "immutable",
            &format!("{select:?}: absent schema beside null name"),
        );
    }
}

#[test]
fn absent_name_in_a_user_root_cannot_be_repaired_to_a_different_directory_key() {
    let case = Case::new(Select::UserKey);
    let mut damaged = repair_doc(case.key);
    damaged.as_object_mut().unwrap().remove("name");
    let frozen = case.damage(&damaged);
    let output = case.update(&repair_doc("different"));
    assert_rejected(
        &output,
        "user directory key",
        "wrong key for an absent name",
    );
    assert_untouched(&case.root, &frozen, "wrong key for an absent name");
}

#[test]
fn absent_schema_in_a_user_root_cannot_be_repaired_to_a_different_directory_key() {
    let case = Case::new(Select::UserKey);
    let mut damaged = repair_doc(case.key);
    damaged.as_object_mut().unwrap().remove("schema_version");
    let frozen = case.damage(&damaged);
    let output = case.update(&repair_doc("different"));
    assert_rejected(
        &output,
        "user directory key",
        "wrong key for an absent schema",
    );
    assert_untouched(&case.root, &frozen, "wrong key for an absent schema");
}

#[test]
fn a_repair_cannot_install_an_unsupported_schema_in_place_of_an_absent_one() {
    for select in [Select::UserKey, Select::Directory] {
        let case = Case::new(select);
        let mut damaged = repair_doc(case.key);
        damaged.as_object_mut().unwrap().remove("schema_version");
        let frozen = case.damage(&damaged);
        let mut submit = repair_doc(case.key);
        submit["schema_version"] = json!(2);
        let output = case.update(&submit);
        assert!(
            !output.status.success(),
            "{select:?}: {}",
            transcript(&output)
        );
        assert!(
            matches!(output.status.code(), Some(1..=3)),
            "{select:?}: {}",
            transcript(&output)
        );
        assert_untouched(&case.root, &frozen, "unsupported schema replacing absent");
    }
}

// ── Retained members still gate the repair ─────────────────────────────────

#[test]
fn a_repair_of_an_absent_name_is_refused_beside_a_malformed_member() {
    for select in [Select::UserKey, Select::Directory] {
        let case = Case::new(select);
        let mut damaged = repair_doc(case.key);
        damaged.as_object_mut().unwrap().remove("name");
        case.damage(&damaged);
        fs::write(case.root.join("agents").join("worker.yml"), "broken: [\n").unwrap();
        let frozen = freeze(&case.root);
        let output = case.update(&repair_doc(case.key));
        assert_rejected(&output, "worker", "repair beside a malformed member");
        assert_untouched(&case.root, &frozen, "repair beside a malformed member");
    }
}

// ── A partial patch may repair the same parseable base ─────────────────────

#[test]
fn patch_repairs_an_absent_name_under_either_selection() {
    for select in [Select::UserKey, Select::Directory] {
        let case = Case::new(select);
        let mut damaged = repair_doc(case.key);
        damaged.as_object_mut().unwrap().remove("name");
        let frozen = case.damage(&damaged);
        let output = case.mutate("patch", &json!({"name": case.key}));
        assert!(
            output.status.success(),
            "{select:?}: {}",
            transcript(&output)
        );
        assert_eq!(parse_json(&output)["name"], json!(case.key), "{select:?}");
        assert_eq!(case.show()["org"]["name"], json!(case.key), "{select:?}");
        assert_only_root_changed(&case.root, &frozen, "patched root");
    }
}

#[test]
fn patch_repairs_an_absent_schema_version_under_either_selection() {
    for select in [Select::UserKey, Select::Directory] {
        let case = Case::new(select);
        let mut damaged = repair_doc(case.key);
        damaged.as_object_mut().unwrap().remove("schema_version");
        let frozen = case.damage(&damaged);
        let output = case.mutate("patch", &json!({"schema_version": 1}));
        assert!(
            output.status.success(),
            "{select:?}: {}",
            transcript(&output)
        );
        assert_eq!(case.stored()["schema_version"], json!(1), "{select:?}");
        assert_eq!(case.show()["org"]["schema_version"], json!(1), "{select:?}");
        assert_only_root_changed(&case.root, &frozen, "patched root");
    }
}

#[test]
fn patch_cannot_rename_a_present_name_beside_an_absent_schema_version() {
    let case = Case::new(Select::Directory);
    let mut damaged = repair_doc(case.key);
    damaged.as_object_mut().unwrap().remove("schema_version");
    let frozen = case.damage(&damaged);
    let output = case.mutate("patch", &json!({"name": "other", "schema_version": 1}));
    assert_rejected(&output, "immutable", "patch renaming beside absent schema");
    assert_untouched(&case.root, &frozen, "patch renaming beside absent schema");
}
