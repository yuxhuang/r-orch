//! Directory org templates, the embedded `a-team`, template-backed deployment
//! and the template-backed agent shop, through the real `orch` binary.
//!
//! Every test runs in its own sandbox: a throwaway `HOME`, a throwaway
//! `ORCHESTRY_GLOBAL_CONFIG_PATH` (which is also where the user template root
//! lives), a throwaway project, and a `PATH` that holds only the system
//! directories, so no agent CLI can be found, let alone run. Agents are
//! created but never started.
//!
//! Layers, by the decision in `docs/decisions/0006-org-templates.md` they pin:
//!
//! * D1  one file per member, the embedded 27-member `a-team`
//! * D2  whole-bundle validation: identifiers, containment, budgets, parsing
//! * D3  shared rules embedded once into each deployed role
//! * D4  source precedence, shadowing and explicit escape from it
//! * D5  list/show/init/edit/agent CRUD, builtins read-only, nothing clobbered
//! * D6  deployment: preflight, overrides, skills, collisions, partial failure
//! * D7  the template shop: list, install exactly one agent, cancel, bad input
//! * D11 mutation safety: unchanged bytes on failure, locking, no-clobber
//!
//! Template update/patch belong to the later mutation slice and are not
//! exercised here. Nothing in this file reads a path outside its sandbox.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use serde_json::{Value, json};

// ── Sandbox ────────────────────────────────────────────────────────────────

/// Upper bound for any single `orch` invocation. Nothing here legitimately
/// takes more than a couple of seconds except the 5 s lock-timeout test.
const RUN_BUDGET: Duration = Duration::from_secs(60);

struct Sandbox {
    dir: tempfile::TempDir,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::Builder::new()
        .prefix("orch-org-templates-")
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

    /// Scratch space for explicit `--template-dir` roots and input files.
    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// The user template root: `<global-config-parent>/templates`.
    fn user_templates(&self) -> PathBuf {
        self.home().join("templates")
    }

    fn command_in(&self, cwd: &Path) -> Command {
        let mut command = Command::cargo_bin("orch").expect("the orch binary");
        command
            .current_dir(cwd)
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

    fn command(&self) -> Command {
        self.command_in(&self.project())
    }

    /// Run to completion under `RUN_BUDGET`; a hang fails the test instead of
    /// the suite.
    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        run_bounded(self.command_in(cwd).args(args), RUN_BUDGET)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.project(), args)
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

    /// A clean, loud failure: exit 1 or 2, never a panic (101) or a signal.
    fn fail(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert_clean_failure(&output, args);
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        parse_json(&self.ok(&all))
    }

    fn yaml(&self, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--yaml");
        let output = self.ok(&all);
        serde_yaml_ng::from_str(&stdout(&output))
            .unwrap_or_else(|e| panic!("stdout is not YAML ({e}):\n{}", stdout(&output)))
    }

    /// `orch init` with a real adapter name. Agents are created, never run.
    fn init_project(&self) {
        self.ok(&["init", "--name", "proj", "--adapter", "claude"]);
    }

    /// Names of the agents in the project, sorted.
    fn agent_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .json(&["agent", "list"])
            .as_array()
            .expect("agent array")
            .iter()
            .map(|a| a["name"].as_str().expect("name").to_owned())
            .collect();
        names.sort();
        names
    }

    fn team_count(&self) -> usize {
        self.json(&["team", "list"])
            .as_array()
            .expect("teams")
            .len()
    }

    fn goal_count(&self) -> usize {
        self.json(&["goal", "list"])
            .as_array()
            .expect("goals")
            .len()
    }

    /// Every file and directory under the project's `.orchestry`, by relative
    /// path only: what exists, not what it says (timestamps vary).
    fn orchestry_entries(&self) -> Vec<String> {
        let root = self.project().join(".orchestry");
        let mut out = Vec::new();
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                out.push(path.strip_prefix(root).unwrap().display().to_string());
                if path.is_dir() {
                    walk(&path, root, out);
                }
            }
        }
        walk(&root, &root, &mut out);
        out.sort();
        out
    }
}

/// Run `command` with output captured, killing it if it outlives `budget`.
fn run_bounded(command: &mut Command, budget: Duration) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orch");
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
    let deadline = Instant::now() + budget;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("orch outlived its {budget:?} budget: {command:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
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

/// stdout and stderr together: where a message prints is not the contract,
/// that it names the offender is.
fn combined(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn parse_json(output: &Output) -> Value {
    serde_json::from_str(&stdout(output))
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}", transcript(output)))
}

fn assert_clean_failure(output: &Output, args: &[&str]) {
    let code = output.status.code();
    assert!(
        matches!(code, Some(1..=3)),
        "orch {args:?} should fail cleanly (exit 1-3), got {code:?}\n{}",
        transcript(output)
    );
}

fn assert_mentions(output: &Output, needle: &str) {
    let text = combined(output);
    assert!(
        text.contains(needle),
        "expected the output to mention {needle:?}\n{}",
        transcript(output)
    );
}

// ── Template fixtures ──────────────────────────────────────────────────────

/// A directory template under construction. Files are written in JSON syntax,
/// which is YAML, so the bytes are unambiguous.
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

    /// Add a member; `extra` fields are merged over `{name, role}`.
    fn agent(mut self, name: &str, extra: Value) -> Self {
        let mut doc = json!({"name": name, "role": format!("{name} role")});
        if let Some(map) = extra.as_object() {
            for (key, value) in map {
                doc[key] = value.clone();
            }
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

    fn write(&self, root: &Path) {
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
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("json")
}

/// A three-member team: `lead`, `worker`, `reviewer`.
fn small_team(name: &str) -> Bundle {
    Bundle::new(name, "lead")
        .agent("lead", json!({}))
        .agent("worker", json!({}))
        .agent("reviewer", json!({}))
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

fn names(agents: &Value) -> Vec<String> {
    agents
        .as_array()
        .expect("agent array")
        .iter()
        .map(|a| a["name"].as_str().expect("name").to_owned())
        .collect()
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

/// The roster the embedded `a-team` must keep: 27 members in name order.
const A_TEAM: [&str; 27] = [
    "ai-eng",
    "architect",
    "backend-eng",
    "backend-eng-2",
    "code-reviewer",
    "data-eng",
    "fact-checker",
    "fact-checker-2",
    "frontend-eng",
    "frontend-eng-2",
    "orchestrator",
    "perf-eng",
    "product-manager",
    "qa-engineer",
    "red-team-guy",
    "release-eng",
    "sec-eng",
    "sec-reviewer",
    "sec-reviewer-codex",
    "senior-eng",
    "senior-eng-2",
    "sre",
    "strategist",
    "tech-writer",
    "test-eng-2",
    "test-engineer",
    "triage",
];

// ═══ D1 — one file per member; the embedded a-team ═════════════════════════

#[test]
fn builtin_a_team_lists_the_27_member_roster_in_name_order() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team"]);
    assert_eq!(names(&shown["agents"]), A_TEAM);
}

#[test]
fn builtin_a_team_leads_with_the_orchestrator_and_registers_three_rules() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team"]);
    let mut rules: Vec<&str> = shown["rules"]
        .as_object()
        .expect("rules object")
        .keys()
        .map(String::as_str)
        .collect();
    rules.sort();
    assert_eq!(
        (shown["org"]["lead"].as_str(), rules),
        (
            Some("orchestrator"),
            vec!["collaboration-protocol", "seniority", "stance"]
        )
    );
}

#[test]
fn builtin_a_team_has_no_creator_and_no_remote_members() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team"]);
    let offenders: Vec<String> = names(&shown["agents"])
        .into_iter()
        .filter(|n| n.contains('@') || n.contains("creator"))
        .collect();
    assert!(offenders.is_empty(), "unexpected members: {offenders:?}");
}

#[test]
fn both_backend_roles_stay_exactly_the_one_line_role() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team"]);
    let roles: Vec<&str> = shown["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["name"].as_str().unwrap().starts_with("backend-eng"))
        .map(|a| a["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["Backend developer", "Backend developer"]);
}

#[test]
fn builtin_roles_keep_the_instructions_that_define_them() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team"]);
    let role = |name: &str| -> String {
        shown["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"))["role"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // One distinctive instruction per role family that carries long prose;
    // losing any of them means extraction dropped content.
    let expected = [
        ("orchestrator", "Phase 3: approval gate"),
        (
            "red-team-guy",
            "**Disagreement** section was written for you",
        ),
        (
            "sec-reviewer-codex",
            "Independent read-only security review",
        ),
        ("qa-engineer", "Reproduce reported bugs with minimal steps"),
        (
            "release-eng",
            "Only release, deploy, promote, tag, publish or push",
        ),
        ("fact-checker", "Verified"),
        ("test-engineer", "A test that can't fail proves nothing"),
    ];
    let missing: Vec<String> = expected
        .iter()
        .filter(|(name, needle)| !role(name).contains(needle))
        .map(|(name, needle)| format!("{name}: {needle:?}"))
        .collect();
    assert!(missing.is_empty(), "roles lost instructions: {missing:#?}");
}

#[test]
fn builtin_bundle_carries_no_personal_paths_or_identities() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    let text = shown.to_string();
    let found: Vec<&str> = [
        "/Users/",
        "/home/",
        "192.168.",
        "10.0.",
        "@gmail",
        "ssh://",
        "git@github.com:",
    ]
    .into_iter()
    .filter(|needle| text.contains(needle))
    .collect();
    assert!(
        found.is_empty(),
        "personal data in the a-team bundle: {found:?}"
    );
}

#[test]
fn init_from_builtin_writes_org_yml_one_file_per_member_and_the_rules() {
    let sb = sandbox();
    let dest = sb.work().join("copy");
    sb.ok(&[
        "org",
        "template",
        "init",
        "copy",
        "--from",
        "builtin:a-team",
        "--template-dir",
        path_arg(&dest),
    ]);
    let files: Vec<String> = snapshot(&dest)
        .into_keys()
        .filter(|k| !k.ends_with('/'))
        .collect();
    let mut expected: Vec<String> = A_TEAM.iter().map(|n| format!("agents/{n}.yml")).collect();
    expected.extend(
        [
            "org.yml",
            "rules/collaboration-protocol.md",
            "rules/seniority.md",
            "rules/stance.md",
        ]
        .map(String::from),
    );
    expected.sort();
    assert_eq!(files, expected);
}

#[test]
fn init_copy_renames_the_org_and_keeps_every_member_definition() {
    let sb = sandbox();
    let dest = sb.work().join("copy");
    sb.ok(&[
        "org",
        "template",
        "init",
        "renamed",
        "--from",
        "builtin:a-team",
        "--template-dir",
        path_arg(&dest),
    ]);
    let copy = sb.json(&["org", "template", "show", "--template-dir", path_arg(&dest)]);
    let builtin = sb.json(&["org", "template", "show", "builtin:a-team"]);
    assert_eq!(
        (&copy["org"]["name"], &copy["agents"], &copy["rules"]),
        (&json!("renamed"), &builtin["agents"], &builtin["rules"])
    );
}

#[test]
fn init_without_from_creates_a_valid_single_lead_team() {
    let sb = sandbox();
    let dest = sb.work().join("minimal");
    sb.ok(&[
        "org",
        "template",
        "init",
        "minimal",
        "--template-dir",
        path_arg(&dest),
    ]);
    let shown = sb.json(&["org", "template", "show", "--template-dir", path_arg(&dest)]);
    assert_eq!(
        (names(&shown["agents"]), shown["org"]["lead"].clone()),
        (vec!["lead".to_owned()], json!("lead"))
    );
}

#[test]
fn init_by_name_creates_the_template_under_the_user_root() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "mine"]);
    assert!(sb.user_templates().join("mine/org.yml").is_file());
}

// ═══ D2 — whole-bundle validation ══════════════════════════════════════════

/// A directory template that is valid until a test breaks it.
fn valid_dir(sb: &Sandbox, name: &str) -> PathBuf {
    let root = sb.work().join(name);
    small_team(name).write(&root);
    root
}

#[test]
fn hostile_agent_names_are_rejected_without_writing_anything() {
    let long = "a".repeat(65);
    let hostile = [
        "../escape",
        "/tmp/x",
        "a/b",
        "a\\\\b",
        ".",
        "a..b",
        "C:evil",
        "CON",
        "com1",
        "lpt9",
        "Upper",
        "ünï",
        "-lead",
        "with space",
        "",
        long.as_str(),
    ];
    let sb = sandbox();
    let root = valid_dir(&sb, "target");
    // The input lives outside `work/`, so the snapshot covers exactly the
    // template and its siblings: an escape such as `../escape` would show up.
    let input = sb.dir.path().join("hostile.json");
    let before = snapshot(&sb.work());
    for name in hostile {
        fs::write(&input, json!({"name": name, "role": "x"}).to_string()).unwrap();
        let output = sb.fail(&[
            "org",
            "template",
            "agent",
            "add",
            "--template-dir",
            path_arg(&root),
            "-f",
            path_arg(&input),
        ]);
        assert_eq!(
            snapshot(&sb.work()),
            before,
            "name {name:?} changed the filesystem\n{}",
            transcript(&output)
        );
    }
    assert!(!sb.dir.path().join("escape").exists());
}

#[test]
fn org_name_with_a_path_separator_cannot_create_a_template() {
    let sb = sandbox();
    let output = sb.fail(&["org", "template", "init", "../evil"]);
    assert!(
        !sb.home().join("evil").exists() && !sb.user_templates().join("../evil").exists(),
        "an escape directory was created\n{}",
        transcript(&output)
    );
}

/// Break a valid three-member directory template in one way and require that
/// it is rejected everywhere it could be consumed, naming the offender.
struct Corruption {
    case: &'static str,
    offender: &'static str,
    apply: fn(&Path),
}

fn corruptions() -> Vec<Corruption> {
    vec![
        Corruption {
            case: "member file name differs from its name field",
            offender: "wrongname.yml",
            apply: |root| {
                fs::copy(
                    root.join("agents/worker.yml"),
                    root.join("agents/wrongname.yml"),
                )
                .unwrap();
            },
        },
        Corruption {
            case: "unsupported schema_version",
            offender: "schema_version",
            apply: |root| {
                let mut org: Value =
                    serde_json::from_slice(&fs::read(root.join("org.yml")).unwrap()).unwrap();
                org["schema_version"] = json!(2);
                fs::write(root.join("org.yml"), pretty(&org)).unwrap();
            },
        },
        Corruption {
            case: "lead names no member",
            offender: "ghost",
            apply: |root| {
                let mut org: Value =
                    serde_json::from_slice(&fs::read(root.join("org.yml")).unwrap()).unwrap();
                org["lead"] = json!("ghost");
                fs::write(root.join("org.yml"), pretty(&org)).unwrap();
            },
        },
        Corruption {
            case: "duplicate rule id in the registry",
            offender: "dup",
            apply: |root| {
                let mut org: Value =
                    serde_json::from_slice(&fs::read(root.join("org.yml")).unwrap()).unwrap();
                org["rules"] = json!(["dup", "dup"]);
                fs::write(root.join("org.yml"), pretty(&org)).unwrap();
                fs::write(root.join("rules/dup.md"), "dup").unwrap();
            },
        },
        Corruption {
            case: "registered rule has no rules/<id>.md",
            offender: "absent-rule",
            apply: |root| {
                let mut org: Value =
                    serde_json::from_slice(&fs::read(root.join("org.yml")).unwrap()).unwrap();
                org["rules"] = json!(["absent-rule"]);
                fs::write(root.join("org.yml"), pretty(&org)).unwrap();
            },
        },
        Corruption {
            case: "member selects an unregistered rule",
            offender: "nope",
            apply: |root| {
                let doc = json!({"name": "worker", "role": "x", "rules": ["nope"]});
                fs::write(root.join("agents/worker.yml"), pretty(&doc)).unwrap();
            },
        },
        Corruption {
            case: "unknown field in a member",
            offender: "typo",
            apply: |root| {
                let doc = json!({"name": "worker", "role": "x", "typo": 1});
                fs::write(root.join("agents/worker.yml"), pretty(&doc)).unwrap();
            },
        },
        Corruption {
            case: "explicit null for an optional member field",
            offender: "null",
            apply: |root| {
                let doc = json!({"name": "worker", "role": "x", "skills": null});
                fs::write(root.join("agents/worker.yml"), pretty(&doc)).unwrap();
            },
        },
        Corruption {
            case: "blank role",
            offender: "role",
            apply: |root| {
                let doc = json!({"name": "worker", "role": "   "});
                fs::write(root.join("agents/worker.yml"), pretty(&doc)).unwrap();
            },
        },
        Corruption {
            case: "malformed YAML member",
            offender: "worker.yml",
            apply: |root| {
                fs::write(root.join("agents/worker.yml"), "broken: [").unwrap();
            },
        },
        Corruption {
            case: "member written as .yaml would be silently dropped",
            offender: "extra.yaml",
            apply: |root| {
                fs::copy(
                    root.join("agents/worker.yml"),
                    root.join("agents/extra.yaml"),
                )
                .unwrap();
            },
        },
        Corruption {
            case: "nested directory in agents/",
            offender: "nested",
            apply: |root| fs::create_dir(root.join("agents/nested")).unwrap(),
        },
        Corruption {
            case: "symlinked member pointing outside the root",
            offender: "linked.yml",
            apply: |root| {
                let outside = root.parent().unwrap().join("outside.yml");
                fs::write(&outside, json!({"name": "linked", "role": "x"}).to_string()).unwrap();
                symlink(&outside, root.join("agents/linked.yml")).unwrap();
            },
        },
        Corruption {
            case: "rules/ is a symlink",
            offender: "rules",
            apply: |root| {
                let real = root.parent().unwrap().join("real-rules");
                fs::create_dir(&real).unwrap();
                fs::remove_dir_all(root.join("rules")).unwrap();
                symlink(&real, root.join("rules")).unwrap();
            },
        },
        Corruption {
            case: "role references an unregistered rule path",
            offender: "unknown.md",
            apply: |root| {
                let doc =
                    json!({"name": "worker", "role": "Follow .claude/agent-rules/unknown.md now."});
                fs::write(root.join("agents/worker.yml"), pretty(&doc)).unwrap();
            },
        },
        Corruption {
            case: "bare registered-rule basename in role prose",
            offender: "stance.md",
            apply: |root| {
                let mut org: Value =
                    serde_json::from_slice(&fs::read(root.join("org.yml")).unwrap()).unwrap();
                org["rules"] = json!(["stance"]);
                fs::write(root.join("org.yml"), pretty(&org)).unwrap();
                fs::write(root.join("rules/stance.md"), "# Stance\n").unwrap();
                let doc = json!({"name": "worker", "role": "Read stance.md carefully."});
                fs::write(root.join("agents/worker.yml"), pretty(&doc)).unwrap();
            },
        },
        Corruption {
            case: "shorthand rule reference inside a rule text",
            offender: "stance.md",
            apply: |root| {
                let mut org: Value =
                    serde_json::from_slice(&fs::read(root.join("org.yml")).unwrap()).unwrap();
                org["rules"] = json!(["stance", "seniority"]);
                fs::write(root.join("org.yml"), pretty(&org)).unwrap();
                fs::write(root.join("rules/stance.md"), "# Stance\n").unwrap();
                fs::write(root.join("rules/seniority.md"), "See stance.md for more.").unwrap();
            },
        },
    ]
}

#[test]
fn every_invalid_bundle_is_rejected_by_show_and_by_deploy_with_nothing_created() {
    for corruption in corruptions() {
        let sb = sandbox();
        sb.init_project();
        let root = valid_dir(&sb, "bad");
        (corruption.apply)(&root);
        let agents_before = sb.agent_names();
        let entries_before = sb.orchestry_entries();

        let shown = sb.fail(&["org", "template", "show", "--template-dir", path_arg(&root)]);
        assert!(
            combined(&shown).contains(corruption.offender),
            "show for [{}] does not name {:?}\n{}",
            corruption.case,
            corruption.offender,
            transcript(&shown)
        );
        sb.fail(&["org", "deploy", "--template-dir", path_arg(&root)]);

        assert_eq!(
            (sb.agent_names(), sb.orchestry_entries()),
            (agents_before, entries_before),
            "[{}] deployed or wrote something",
            corruption.case
        );
    }
}

#[test]
fn a_symlinked_template_root_is_rejected() {
    let sb = sandbox();
    let real = valid_dir(&sb, "real");
    let link = sb.work().join("link");
    symlink(&real, &link).unwrap();
    let output = sb.fail(&["org", "template", "show", "--template-dir", path_arg(&link)]);
    assert_mentions(&output, "symlink");
}

#[test]
fn readme_and_dotfiles_beside_members_are_ignored_not_deployed() {
    let sb = sandbox();
    let root = valid_dir(&sb, "plain");
    fs::write(root.join("agents/README.md"), "notes").unwrap();
    fs::write(root.join("agents/.hidden"), "x").unwrap();
    let shown = sb.json(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    assert_eq!(names(&shown["agents"]), ["lead", "reviewer", "worker"]);
}

/// Documents the guarded input path must refuse before anything is written,
/// each with the fragment that proves it was refused for *that* reason (not
/// merely because the document has a stray field).
fn hostile_documents() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let deep = format!("{}1{}", "[".repeat(80), "]".repeat(80));
    vec![
        ("empty file", Vec::new(), "bytes"),
        (
            "two YAML documents",
            b"name: extra\nrole: x\n---\nname: extra\nrole: y\n".to_vec(),
            "more than one document",
        ),
        (
            "duplicate YAML keys",
            b"name: extra\nrole: x\nrole: y\n".to_vec(),
            "duplicate entry with key \"role\"",
        ),
        (
            "duplicate JSON keys",
            br#"{"name":"extra","role":"x","role":"y"}"#.to_vec(),
            "duplicate entry with key \"role\"",
        ),
        (
            "explicit YAML tag",
            b"name: extra\nrole: !!binary aGk=\n".to_vec(),
            "tag",
        ),
        (
            "non-string mapping key",
            b"name: extra\nrole: x\n1: two\n".to_vec(),
            "keys must be strings",
        ),
        (
            "nonfinite number",
            b"name: extra\nrole: x\nmax_turns: .nan\n".to_vec(),
            "nonfinite",
        ),
        (
            "nesting deeper than 64",
            format!("name: extra\nrole: x\nskills: {deep}\n").into_bytes(),
            "depth",
        ),
        (
            "oversized document",
            {
                let mut doc = b"name: extra\nrole: ".to_vec();
                doc.extend(std::iter::repeat_n(b'x', 1024 * 1024 + 1));
                doc
            },
            "1 MiB",
        ),
        (
            "alias expansion bomb",
            {
                let mut doc = String::from(
                    "name: extra\nrole: x\nbomb:\n  - &a0 [x, x, x, x, x, x, x, x, x, x]\n",
                );
                for level in 1..=9 {
                    let refs = vec![format!("*a{}", level - 1); 10].join(", ");
                    doc.push_str(&format!("  - &a{level} [{refs}]\n"));
                }
                doc.into_bytes()
            },
            "alias",
        ),
    ]
}

#[test]
fn hostile_member_documents_are_rejected_promptly_and_leave_the_template_unchanged() {
    for (case, bytes, reason) in hostile_documents() {
        let sb = sandbox();
        let root = valid_dir(&sb, "target");
        let input = sb.dir.path().join("input.yml");
        fs::write(&input, &bytes).unwrap();
        let before = snapshot(&root);
        let started = Instant::now();
        let output = sb.fail(&[
            "org",
            "template",
            "agent",
            "add",
            "--template-dir",
            path_arg(&root),
            "-f",
            path_arg(&input),
        ]);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "[{case}] took {:?}",
            started.elapsed()
        );
        assert!(
            combined(&output).contains(path_arg(&input)),
            "[{case}] did not name the input path\n{}",
            transcript(&output)
        );
        assert!(
            combined(&output).contains(reason),
            "[{case}] was not refused for the expected reason {reason:?}\n{}",
            transcript(&output)
        );
        assert_eq!(snapshot(&root), before, "[{case}] changed the template");
    }
}

#[test]
fn a_template_with_more_than_128_members_is_rejected() {
    let sb = sandbox();
    let root = sb.work().join("big");
    let mut bundle = Bundle::new("big", "m000");
    for i in 0..129 {
        bundle = bundle.agent(&format!("m{i:03}"), json!({}));
    }
    bundle.write(&root);
    sb.fail(&["org", "template", "show", "--template-dir", path_arg(&root)]);
}

#[test]
fn a_template_with_exactly_128_members_is_accepted() {
    let sb = sandbox();
    let root = sb.work().join("max");
    let mut bundle = Bundle::new("max", "m000");
    for i in 0..128 {
        bundle = bundle.agent(&format!("m{i:03}"), json!({}));
    }
    bundle.write(&root);
    let shown = sb.json(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    assert_eq!(shown["agents"].as_array().unwrap().len(), 128);
}

#[test]
fn a_member_file_may_use_json_syntax_or_yaml_syntax() {
    let sb = sandbox();
    let root = valid_dir(&sb, "mixed");
    fs::write(
        root.join("agents/worker.yml"),
        "name: worker\nrole: |\n  multi\n  line\nskills: [qa]\n",
    )
    .unwrap();
    let shown = sb.json(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    let worker = shown["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "worker")
        .unwrap();
    assert_eq!(
        (worker["role"].clone(), worker["skills"].clone()),
        (json!("multi\nline\n"), json!(["qa"]))
    );
}

// ═══ D3 — shared rules, embedded once ══════════════════════════════════════

/// `resolved_agents` of a `show --resolved`, by member name.
fn resolved_roles(shown: &Value) -> BTreeMap<String, String> {
    shown["resolved_agents"]
        .as_array()
        .expect("resolved_agents")
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_owned(),
                a["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// How many whole lines of `role` equal `heading`. A role may legitimately
/// contain `## Stance` of its own, so a substring count would over-report a
/// rule whose first line is `# Stance`.
fn heading_lines(role: &str, heading: &str) -> usize {
    role.lines().filter(|line| *line == heading).count()
}

#[test]
fn every_a_team_role_embeds_each_rule_it_selects_exactly_once() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    let roles = resolved_roles(&shown);
    // A rule's first line is its identity in the deployed text.
    let headings: BTreeMap<&str, &str> = shown["rules"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, text)| (id.as_str(), text.as_str().unwrap().lines().next().unwrap()))
        .collect();
    assert!(
        headings.values().all(|h| !h.is_empty()),
        "blank rule heading"
    );

    let mut wrong = Vec::new();
    for agent in shown["agents"].as_array().unwrap() {
        let name = agent["name"].as_str().unwrap();
        for rule in agent["rules"].as_array().unwrap() {
            let id = rule.as_str().unwrap();
            let count = heading_lines(&roles[name], headings[id]);
            if count != 1 {
                wrong.push(format!("{name}: rule {id} appears {count} times"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn no_deployed_a_team_role_points_at_a_rule_file() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    let offenders: Vec<String> = resolved_roles(&shown)
        .into_iter()
        .filter(|(_, role)| role.contains(".claude/agent-rules") || role.contains("same folder"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "roles still look up rule files: {offenders:?}"
    );
}

#[test]
fn red_team_and_orchestrator_resolve_all_three_rules_whether_or_not_they_are_selected() {
    // Their source headers name the rules by shorthand; normalisation turns
    // that into canonical references, so the closure is the same three rules
    // even for a copy that drops the explicit `rules` selection.
    let sb = sandbox();
    let copy = sb.work().join("copy");
    sb.ok(&[
        "org",
        "template",
        "init",
        "copy",
        "--from",
        "builtin:a-team",
        "--template-dir",
        path_arg(&copy),
    ]);
    for member in ["orchestrator", "red-team-guy"] {
        let file = copy.join("agents").join(format!("{member}.yml"));
        let mut doc: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert!(
            doc.as_object_mut().unwrap().remove("rules").is_some(),
            "{member} had no rules"
        );
        fs::write(&file, pretty(&doc)).unwrap();
    }
    let shown = sb.json(&[
        "org",
        "template",
        "show",
        "--template-dir",
        path_arg(&copy),
        "--resolved",
    ]);
    let roles = resolved_roles(&shown);
    let headings: Vec<&str> = shown["rules"]
        .as_object()
        .unwrap()
        .values()
        .map(|t| t.as_str().unwrap().lines().next().unwrap())
        .collect();
    let counts: Vec<(String, Vec<usize>)> = ["orchestrator", "red-team-guy"]
        .iter()
        .map(|m| {
            (
                (*m).to_owned(),
                headings
                    .iter()
                    .map(|h| heading_lines(&roles[*m], h))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        counts,
        vec![
            ("orchestrator".to_owned(), vec![1, 1, 1]),
            ("red-team-guy".to_owned(), vec![1, 1, 1])
        ]
    );
}

#[test]
fn red_team_role_keeps_its_disagreement_instruction_after_rule_embedding() {
    let sb = sandbox();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    assert!(
        resolved_roles(&shown)["red-team-guy"]
            .contains("**Disagreement** section was written for you")
    );
}

fn rule_team(sb: &Sandbox, registry_order: &[&str], role: &str, member_rules: Value) -> PathBuf {
    let root = sb.work().join("rules-team");
    let mut bundle = Bundle::new("rules-team", "solo");
    for id in registry_order {
        bundle = bundle.rule(id, &format!("{id} body."));
    }
    bundle = bundle.agent("solo", json!({"role": role, "rules": member_rules}));
    bundle.write(&root);
    root
}

fn solo_role(sb: &Sandbox, root: &Path) -> String {
    let shown = sb.json(&[
        "org",
        "template",
        "show",
        "--template-dir",
        path_arg(root),
        "--resolved",
    ]);
    resolved_roles(&shown).remove("solo").expect("solo")
}

#[test]
fn a_role_that_references_a_rule_gets_it_even_with_an_explicit_empty_selection() {
    let sb = sandbox();
    let root = rule_team(
        &sb,
        &["alpha"],
        "Obey .claude/agent-rules/alpha.md.",
        json!([]),
    );
    assert_eq!(occurrences(&solo_role(&sb, &root), "alpha body."), 1);
}

#[test]
fn rules_that_reference_each_other_are_each_embedded_once() {
    let sb = sandbox();
    let root = sb.work().join("cycle");
    Bundle::new("cycle", "solo")
        .rule("alpha", "Alpha body. See .claude/agent-rules/beta.md.")
        .rule("beta", "Beta body. See .claude/agent-rules/alpha.md.")
        .agent(
            "solo",
            json!({"role": "Obey .claude/agent-rules/alpha.md."}),
        )
        .write(&root);
    let role = solo_role(&sb, &root);
    assert_eq!(
        (
            occurrences(&role, "Alpha body."),
            occurrences(&role, "Beta body.")
        ),
        (1, 1)
    );
}

#[test]
fn rules_are_emitted_in_registry_order_not_reference_order() {
    let sb = sandbox();
    let root = rule_team(
        &sb,
        &["beta", "alpha", "gamma"],
        "Use .claude/agent-rules/alpha.md then .claude/agent-rules/gamma.md then .claude/agent-rules/beta.md.",
        json!([]),
    );
    let role = solo_role(&sb, &root);
    let positions: Vec<usize> = ["beta body.", "alpha body.", "gamma body."]
        .iter()
        .map(|b| role.find(b).unwrap_or_else(|| panic!("{b} missing")))
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "order was {positions:?}"
    );
}

#[test]
fn a_rule_nobody_selects_or_references_is_not_embedded() {
    let sb = sandbox();
    let root = rule_team(&sb, &["alpha", "beta"], "Plain role.", json!(["alpha"]));
    let role = solo_role(&sb, &root);
    assert_eq!(
        (
            occurrences(&role, "alpha body."),
            occurrences(&role, "beta body.")
        ),
        (1, 0)
    );
}

#[test]
fn the_authored_role_text_survives_resolution_unchanged() {
    let sb = sandbox();
    let authored = "First line.\n\n## Heading\n- bullet with `code` and \"quotes\"\n";
    let root = rule_team(&sb, &[], authored, json!([]));
    assert!(solo_role(&sb, &root).contains(authored));
}

#[test]
fn deploying_embeds_rules_without_touching_the_workspace_rule_files() {
    let sb = sandbox();
    sb.init_project();
    let rules_dir = sb.project().join(".claude/agent-rules");
    fs::create_dir_all(&rules_dir).unwrap();
    fs::write(rules_dir.join("stance.md"), "LOCAL OVERRIDE stance").unwrap();
    let before = snapshot(&sb.project().join(".claude"));

    sb.ok(&["org", "deploy", "--template", "builtin:a-team", "--json"]);

    let orchestrator = sb
        .json(&["agent", "list"])
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "orchestrator")
        .unwrap()["role"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        (
            snapshot(&sb.project().join(".claude")) == before,
            orchestrator.contains("LOCAL OVERRIDE"),
        ),
        (true, false)
    );
}

// ═══ D4 — sources, precedence, escape from shadowing ═══════════════════════

fn rows<'a>(listing: &'a Value, name: &str) -> Vec<&'a Value> {
    listing
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["name"] == name)
        .collect()
}

#[test]
fn with_no_user_templates_the_listing_is_a_team_plus_ten_legacy_presets() {
    let sb = sandbox();
    let listing = sb.json(&["org", "template", "list"]);
    let kinds: Vec<(&str, &str)> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["source"]["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        (
            kinds.len(),
            kinds.iter().filter(|(_, k)| *k == "builtin").count(),
            kinds.iter().filter(|(_, k)| *k == "legacy").count()
        ),
        (11, 1, 10)
    );
}

#[test]
fn a_user_template_shadows_the_builtin_of_the_same_name() {
    let sb = sandbox();
    sb.ok(&[
        "org",
        "template",
        "init",
        "a-team",
        "--from",
        "builtin:a-team",
    ]);
    let by_name = sb.json(&["org", "template", "show", "a-team"]);
    let explicit = sb.json(&["org", "template", "show", "builtin:a-team"]);
    assert_eq!(
        (
            by_name["source"]["kind"].clone(),
            explicit["source"]["kind"].clone()
        ),
        (json!("user"), json!("builtin"))
    );
}

#[test]
fn the_listing_marks_the_shadowed_builtin() {
    let sb = sandbox();
    sb.ok(&[
        "org",
        "template",
        "init",
        "a-team",
        "--from",
        "builtin:a-team",
    ]);
    let listing = sb.json(&["org", "template", "list"]);
    let state: Vec<(String, bool)> = rows(&listing, "a-team")
        .iter()
        .map(|r| {
            (
                r["source"]["kind"].as_str().unwrap().to_owned(),
                r["shadowed"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        state,
        [("user".to_owned(), false), ("builtin".to_owned(), true)]
    );
}

#[test]
fn a_user_template_named_like_a_legacy_preset_shadows_it_and_the_legacy_stays_reachable() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "startup-mvp"]);
    let plain = sb.json(&["org", "template", "show", "startup-mvp"]);
    let legacy = sb.json(&["org", "template", "show", "builtin:startup-mvp"]);
    assert_eq!(
        (
            plain["source"]["kind"].clone(),
            legacy["source"]["kind"].clone()
        ),
        (json!("user"), json!("legacy"))
    );
}

#[test]
fn a_corrupt_user_template_is_an_error_row_and_the_listing_exits_nonzero() {
    let sb = sandbox();
    sb.ok(&[
        "org",
        "template",
        "init",
        "a-team",
        "--from",
        "builtin:a-team",
    ]);
    fs::write(sb.user_templates().join("a-team/org.yml"), "x: [").unwrap();
    let output = sb.run(&["org", "template", "list", "--json"]);
    assert_clean_failure(&output, &["org", "template", "list"]);
    let listing = parse_json(&output);
    let errored: Vec<bool> = rows(&listing, "a-team")
        .iter()
        .map(|r| r.get("error").is_some())
        .collect();
    assert_eq!(errored, [true, false]);
}

#[test]
fn a_corrupt_user_template_is_never_replaced_by_the_builtin_it_shadows() {
    let sb = sandbox();
    sb.init_project();
    sb.ok(&[
        "org",
        "template",
        "init",
        "a-team",
        "--from",
        "builtin:a-team",
    ]);
    fs::write(sb.user_templates().join("a-team/org.yml"), "x: [").unwrap();
    let agents_before = sb.agent_names();
    sb.fail(&["org", "template", "show", "a-team"]);
    sb.fail(&["org", "deploy", "--template", "a-team"]);
    sb.fail(&["agent", "shop", "--template", "a-team", "--list"]);
    assert_eq!(sb.agent_names(), agents_before);
}

#[test]
fn the_builtin_selector_still_works_while_the_user_copy_is_corrupt() {
    let sb = sandbox();
    sb.ok(&[
        "org",
        "template",
        "init",
        "a-team",
        "--from",
        "builtin:a-team",
    ]);
    fs::write(sb.user_templates().join("a-team/org.yml"), "x: [").unwrap();
    let shown = sb.json(&["org", "template", "show", "builtin:a-team"]);
    assert_eq!(shown["agents"].as_array().unwrap().len(), 27);
}

#[test]
fn a_dangling_user_template_symlink_does_not_fall_back_to_the_legacy_preset() {
    let sb = sandbox();
    sb.init_project();
    fs::create_dir_all(sb.user_templates()).unwrap();
    symlink(
        sb.home().join("nowhere"),
        sb.user_templates().join("startup-mvp"),
    )
    .unwrap();
    let agents_before = sb.agent_names();
    sb.fail(&["org", "deploy", "startup-mvp"]);
    assert_eq!(sb.agent_names(), agents_before);
}

#[test]
fn qualified_selectors_only_look_in_their_own_source() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "mine"]);
    let user_of_builtin = sb.run(&["org", "template", "show", "user:a-team"]);
    let builtin_of_user = sb.run(&["org", "template", "show", "builtin:mine"]);
    assert_clean_failure(&user_of_builtin, &["show", "user:a-team"]);
    assert_clean_failure(&builtin_of_user, &["show", "builtin:mine"]);
}

#[test]
fn a_user_template_whose_org_name_differs_from_its_directory_is_rejected() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "first"]);
    fs::rename(
        sb.user_templates().join("first"),
        sb.user_templates().join("second"),
    )
    .unwrap();
    let output = sb.fail(&["org", "template", "show", "second"]);
    assert_mentions(&output, "name");
}

#[test]
fn an_explicit_directory_may_have_any_name_and_its_org_name_stays_the_key() {
    let sb = sandbox();
    sb.init_project();
    let root = sb.work().join("whatever-dir");
    small_team("keyed").write(&root);
    let deployed = sb.json(&["org", "deploy", "--template-dir", path_arg(&root)]);
    assert_eq!(deployed["team"]["name"], "keyed");
}

#[test]
fn an_unknown_template_name_fails_and_names_it() {
    let sb = sandbox();
    let output = sb.fail(&["org", "template", "show", "no-such-team"]);
    assert_mentions(&output, "no-such-team");
}

#[test]
fn a_template_dir_conflicts_with_a_name_selector_and_with_the_legacy_positional() {
    let sb = sandbox();
    sb.init_project();
    let root = valid_dir(&sb, "dir");
    let dir = path_arg(&root);
    let agents_before = sb.agent_names();
    let attempts: [&[&str]; 3] = [
        &["org", "template", "show", "a-team", "--template-dir", dir],
        &["org", "deploy", "a-team", "--template-dir", dir],
        &[
            "org",
            "deploy",
            "--template",
            "a-team",
            "--template-dir",
            dir,
        ],
    ];
    for args in attempts {
        let output = sb.run(args);
        assert_clean_failure(&output, args);
        assert_mentions(&output, "cannot be used with");
    }
    assert_eq!(sb.agent_names(), agents_before);
}

// ═══ D5 — grammar, CRUD, read-only builtins, nothing clobbered ═════════════

#[test]
fn listing_showing_and_agent_listing_need_no_project_and_create_nothing() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let work_before = snapshot(&sb.work());
    sb.ok(&["org", "template", "list"]);
    sb.ok(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    sb.ok(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    sb.ok(&["org", "template", "agent", "list", "builtin:a-team"]);
    let untouched = (
        snapshot(&sb.project()).is_empty(),
        snapshot(&sb.home()).is_empty(),
        snapshot(&sb.work()) == work_before,
    );
    assert_eq!(untouched, (true, true, true));
}

#[test]
fn global_output_flags_may_precede_the_template_subgroup_without_a_project() {
    let sb = sandbox();
    let placements: [&[&str]; 3] = [
        &["--json", "org", "template", "list"],
        &["org", "--json", "template", "list"],
        &["org", "template", "--json", "list"],
    ];
    let ok: Vec<bool> = placements
        .iter()
        .map(|a| sb.run(a).status.success())
        .collect();
    assert_eq!(ok, [true; 3]);
}

#[test]
fn deploy_and_install_need_an_initialized_project_and_create_nothing_without_one() {
    let sb = sandbox();
    let deploy = sb.run(&["org", "deploy", "--template", "builtin:a-team"]);
    let shop = sb.run(&["agent", "shop", "--template", "builtin:a-team", "--list"]);
    assert_eq!(
        (
            deploy.status.code(),
            shop.status.code(),
            snapshot(&sb.project()).is_empty()
        ),
        (Some(3), Some(3), true)
    );
}

#[test]
fn builtins_are_read_only_and_the_error_points_at_init() {
    let sb = sandbox();
    let input = sb.dir.path().join("member.json");
    fs::write(&input, json!({"name": "extra", "role": "x"}).to_string()).unwrap();
    let attempts: Vec<Vec<&str>> = vec![
        vec!["org", "template", "edit", "builtin:a-team"],
        vec![
            "org",
            "template",
            "agent",
            "add",
            "builtin:a-team",
            "-f",
            path_arg(&input),
        ],
        vec!["org", "template", "agent", "edit", "builtin:a-team", "sre"],
        vec![
            "org",
            "template",
            "agent",
            "delete",
            "builtin:a-team",
            "sre",
        ],
    ];
    for args in attempts {
        let output = sb.fail(&args);
        assert!(
            combined(&output).contains("init"),
            "{args:?} should suggest `template init`\n{}",
            transcript(&output)
        );
    }
    assert!(
        !sb.user_templates().exists(),
        "a mutation attempt created a user template"
    );
}

#[test]
fn init_never_overwrites_an_existing_destination() {
    let sb = sandbox();
    let root = valid_dir(&sb, "taken");
    let before = snapshot(&root);
    sb.fail(&[
        "org",
        "template",
        "init",
        "taken",
        "--template-dir",
        path_arg(&root),
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn init_of_an_existing_user_name_leaves_it_untouched() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "mine"]);
    let before = snapshot(&sb.user_templates());
    sb.fail(&[
        "org",
        "template",
        "init",
        "mine",
        "--from",
        "builtin:a-team",
    ]);
    assert_eq!(snapshot(&sb.user_templates()), before);
}

#[test]
fn a_failed_init_leaves_no_partial_destination_behind() {
    let sb = sandbox();
    let before = snapshot(&sb.work());
    sb.fail(&[
        "org",
        "template",
        "init",
        "ghost",
        "--from",
        "user:does-not-exist",
        "--template-dir",
        path_arg(&sb.work().join("ghost")),
    ]);
    sb.fail(&[
        "org",
        "template",
        "init",
        "Bad_Name",
        "--template-dir",
        path_arg(&sb.work().join("badname")),
    ]);
    assert_eq!(snapshot(&sb.work()), before);
}

#[test]
fn show_and_agent_list_do_not_write_into_a_template_directory() {
    let sb = sandbox();
    let root = valid_dir(&sb, "readonly");
    let before = snapshot_with_hidden(&root);
    sb.ok(&[
        "org",
        "template",
        "show",
        "--template-dir",
        path_arg(&root),
        "--resolved",
    ]);
    sb.ok(&[
        "org",
        "template",
        "agent",
        "list",
        "--template-dir",
        path_arg(&root),
    ]);
    assert_eq!(snapshot_with_hidden(&root), before);
}

/// Like `snapshot`, but the lock file counts too.
fn snapshot_with_hidden(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = snapshot(root);
    let lock = root.join(".template.lock");
    if lock.exists() {
        out.insert(".template.lock".into(), fs::read(lock).unwrap());
    }
    out
}

fn write_input(sb: &Sandbox, name: &str, doc: &Value) -> PathBuf {
    let path = sb.dir.path().join(name);
    fs::write(&path, doc.to_string()).unwrap();
    path
}

#[test]
fn adding_a_member_from_a_json_file_publishes_exactly_that_one_file() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let input = write_input(
        &sb,
        "new.json",
        &json!({"name": "extra", "role": "added", "skills": []}),
    );
    let added = sb.json(&[
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        path_arg(&input),
    ]);
    let after = snapshot(&root);
    let new_keys: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    let changed: Vec<&String> = before.keys().filter(|k| before[*k] != after[*k]).collect();
    assert_eq!(
        (new_keys, changed, added["skills"].clone()),
        (vec![&"agents/extra.yml".to_string()], vec![], json!([]))
    );
}

#[test]
fn adding_a_member_from_a_yaml_file_works_too() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let input = sb.dir.path().join("new.yaml");
    fs::write(
        &input,
        "name: extra\nrole: |\n  line one\n  line two\nadapter: codex\n",
    )
    .unwrap();
    let added = sb.json(&[
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        path_arg(&input),
    ]);
    assert_eq!(
        (added["role"].clone(), added["adapter"].clone()),
        (json!("line one\nline two\n"), json!("codex"))
    );
}

#[test]
fn adding_a_member_from_stdin_works() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let mut command = sb.command();
    command.args([
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        "-",
        "--json",
    ]);
    let output = run_with_stdin(&mut command, br#"{"name":"piped","role":"from stdin"}"#);
    assert!(output.status.success(), "{}", transcript(&output));
    assert_eq!(parse_json(&output)["role"], "from stdin");
}

fn run_with_stdin(command: &mut Command, input: &[u8]) -> Output {
    use std::io::Write;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().expect("output")
}

#[test]
fn adding_a_member_whose_file_exists_is_refused_and_keeps_the_original() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let input = write_input(
        &sb,
        "dup.json",
        &json!({"name": "worker", "role": "replacement"}),
    );
    sb.fail(&[
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        path_arg(&input),
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn deleting_a_member_by_name_and_by_flag_removes_only_that_file() {
    let sb = sandbox();
    sb.ok(&["org", "template", "init", "named"]);
    let named = sb.user_templates().join("named");
    let extra = write_input(&sb, "extra.json", &json!({"name": "extra", "role": "x"}));
    sb.ok(&[
        "org",
        "template",
        "agent",
        "add",
        "named",
        "-f",
        path_arg(&extra),
    ]);
    let by_name = sb.json(&["org", "template", "agent", "delete", "named", "extra"]);

    let dir = valid_dir(&sb, "dir");
    let by_flag = sb.json(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&dir),
        "--agent",
        "worker",
    ]);

    let left: (bool, bool) = (
        named.join("agents/extra.yml").exists(),
        dir.join("agents/worker.yml").exists(),
    );
    assert_eq!(
        (by_name, by_flag, left),
        (
            json!({"deleted": "extra"}),
            json!({"deleted": "worker"}),
            (false, false)
        )
    );
}

#[test]
fn the_lead_cannot_be_deleted() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    sb.fail(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "lead",
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn the_final_member_cannot_be_deleted() {
    let sb = sandbox();
    let root = sb.work().join("solo");
    Bundle::new("solo", "only")
        .agent("only", json!({}))
        .write(&root);
    let before = snapshot(&root);
    sb.fail(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "only",
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn deleting_a_member_that_does_not_exist_fails_and_names_it() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let output = sb.fail(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "phantom",
    ]);
    assert_mentions(&output, "phantom");
}

/// An `$EDITOR` that replaces the file it is given with `content` and records
/// the path it was given.
fn fake_editor(sb: &Sandbox, content: &str, exit: i32) -> (PathBuf, PathBuf) {
    let script = sb.dir.path().join("editor.sh");
    let record = sb.dir.path().join("editor-path.txt");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\ncat > \"$1\" <<'ORCH_EDITOR_EOF'\n{content}\nORCH_EDITOR_EOF\nexit {exit}\n",
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
    run_bounded(&mut command, RUN_BUDGET)
}

#[test]
fn editing_a_member_runs_the_editor_on_a_temporary_copy_and_publishes_the_result() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let (editor, record) = fake_editor(&sb, r#"{"name":"worker","role":"edited role"}"#, 0);
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
            "worker",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", transcript(&output));
    let edited_path = PathBuf::from(fs::read_to_string(record).unwrap());
    let live = fs::canonicalize(root.join("agents/worker.yml")).unwrap();
    assert_eq!(
        (
            parse_json(&output)["role"].clone(),
            edited_path.starts_with(&root),
            fs::canonicalize(&edited_path).ok() == Some(live)
        ),
        (json!("edited role"), false, false)
    );
}

#[test]
fn an_edit_that_breaks_the_bundle_is_rejected_and_the_file_is_unchanged() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let (editor, _) = fake_editor(&sb, r#"{"name":"worker","role":"x","typo":true}"#, 0);
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
            "worker",
        ],
    );
    assert_clean_failure(&output, &["agent", "edit"]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn an_edit_that_renames_the_member_is_rejected_and_the_file_is_unchanged() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let (editor, _) = fake_editor(&sb, r#"{"name":"renamed","role":"x"}"#, 0);
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
            "worker",
        ],
    );
    assert_clean_failure(&output, &["agent", "edit"]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn an_editor_that_exits_nonzero_publishes_nothing() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let (editor, _) = fake_editor(&sb, r#"{"name":"worker","role":"would be valid"}"#, 1);
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
            "worker",
        ],
    );
    assert_clean_failure(&output, &["agent", "edit"]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn editing_the_org_changes_org_yml_only() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let (editor, _) = fake_editor(
        &sb,
        r#"{"schema_version":1,"name":"dir","lead":"lead","description":"edited description","rules":[]}"#,
        0,
    );
    let output = with_editor(
        &sb,
        &editor,
        &[
            "org",
            "template",
            "edit",
            "--template-dir",
            path_arg(&root),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", transcript(&output));
    let after = snapshot(&root);
    let changed: Vec<&String> = after
        .keys()
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    assert_eq!(
        (changed, parse_json(&output)["description"].clone()),
        (vec![&"org.yml".to_string()], json!("edited description"))
    );
}

#[test]
fn an_org_edit_that_renames_the_org_or_points_the_lead_nowhere_is_rejected() {
    for (case, doc) in [
        (
            "rename",
            r#"{"schema_version":1,"name":"other","lead":"lead","rules":[]}"#,
        ),
        (
            "lead",
            r#"{"schema_version":1,"name":"dir","lead":"nobody","rules":[]}"#,
        ),
    ] {
        let sb = sandbox();
        let root = valid_dir(&sb, "dir");
        let before = snapshot(&root);
        let (editor, _) = fake_editor(&sb, doc, 0);
        let output = with_editor(
            &sb,
            &editor,
            &["org", "template", "edit", "--template-dir", path_arg(&root)],
        );
        assert_clean_failure(&output, &["edit"]);
        assert_eq!(snapshot(&root), before, "[{case}] changed the template");
    }
}

#[test]
fn json_and_yaml_describe_the_same_value_for_every_template_read_command() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let dir = path_arg(&root);
    let commands: Vec<Vec<&str>> = vec![
        vec!["org", "template", "list"],
        vec!["org", "template", "show", "builtin:a-team"],
        vec!["org", "template", "show", "builtin:a-team", "--resolved"],
        vec!["org", "template", "show", "--template-dir", dir],
        vec!["org", "template", "agent", "list", "builtin:a-team"],
    ];
    for args in commands {
        assert_eq!(sb.yaml(&args), sb.json(&args), "{args:?}");
    }
}

#[test]
fn json_output_of_a_failure_is_not_machine_data() {
    // A rejected command prints nothing on stdout: scripts must be able to
    // test the exit status instead of parsing a half-formed document.
    let sb = sandbox();
    let output = sb.fail(&["org", "template", "show", "no-such-team", "--json"]);
    assert_eq!(stdout(&output), "");
}

// ═══ D6 — deployment ═══════════════════════════════════════════════════════

/// A fixture whose org defaults and members exercise adapter, model, skills
/// and the command rules at once:
///
/// | member    | own fields                           |
/// |-----------|--------------------------------------|
/// | `boss`    | none (inherits everything)           |
/// | `cx`      | adapter codex, model gpt-x           |
/// | `empty-sk`| skills `[]`                          |
/// | `own-sk`  | skills `[qa]`                        |
/// | `shellie` | adapter shell, command `true`        |
///
/// org defaults: adapter claude, model claude-opus-x, skills `[review]`.
fn mix_team(sb: &Sandbox) -> PathBuf {
    let root = sb.work().join("mix");
    Bundle::new("mix", "boss")
        .defaults(json!({"adapter": "claude", "model": "claude-opus-x", "skills": ["review"], "max_turns": 7}))
        .agent("boss", json!({}))
        .agent("cx", json!({"adapter": "codex", "model": "gpt-x"}))
        .agent("empty-sk", json!({"skills": []}))
        .agent("own-sk", json!({"skills": ["qa"]}))
        .agent("shellie", json!({"adapter": "shell", "command": "true"}))
        .write(&root);
    root
}

/// Deployed agent by name: `(adapter, model, skills)`; the two options are
/// `None` when the config carries no such key.
#[derive(Debug, PartialEq)]
struct Deployed {
    adapter: String,
    model: Option<String>,
    skills: Option<Vec<String>>,
}

fn deployed(sb: &Sandbox, name: &str) -> Deployed {
    let agents = sb.json(&["agent", "list"]);
    let agent = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("{name} was not deployed"));
    Deployed {
        adapter: agent["adapter"].as_str().unwrap().to_owned(),
        model: agent["config"]["model"].as_str().map(str::to_owned),
        skills: agent["config"]["skills"]
            .as_array()
            .map(|s| s.iter().map(|v| v.as_str().unwrap().to_owned()).collect()),
    }
}

fn dep(adapter: &str, model: Option<&str>, skills: Option<&[&str]>) -> Deployed {
    Deployed {
        adapter: adapter.into(),
        model: model.map(Into::into),
        skills: skills.map(|s| s.iter().map(|x| (*x).to_owned()).collect()),
    }
}

#[test]
fn deploying_a_template_creates_one_agent_per_member_and_a_team_led_by_the_lead() {
    let sb = sandbox();
    sb.init_project();
    let root = valid_dir(&sb, "trio");
    let before = sb.agent_names();
    let out = sb.json(&["org", "deploy", "--template-dir", path_arg(&root)]);

    let lead_id = out["team"]["lead_agent_id"].as_str().unwrap().to_owned();
    let agents = sb.json(&["agent", "list"]);
    let lead_name = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == lead_id.as_str())
        .map(|a| a["name"].clone());
    let mut expected = before;
    expected.extend(["lead", "reviewer", "worker"].map(String::from));
    expected.sort();
    assert_eq!(
        (
            sb.agent_names(),
            lead_name,
            out["team"]["name"].clone(),
            out["agentIds"].as_array().unwrap().len()
        ),
        (expected, Some(json!("lead")), json!("trio"), 3)
    );
}

#[test]
fn deploying_with_a_goal_creates_it_and_without_a_goal_creates_none() {
    let sb = sandbox();
    sb.init_project();
    let root = valid_dir(&sb, "trio");
    let with_goal = sb.json(&[
        "org",
        "deploy",
        "--template-dir",
        path_arg(&root),
        "--goal",
        "Ship it",
    ]);
    assert_eq!(
        (with_goal["goalId"].is_string(), sb.goal_count()),
        (true, 1)
    );

    let other = sb.work().join("duo");
    Bundle::new("duo", "a")
        .agent("a", json!({}))
        .agent("b", json!({}))
        .write(&other);
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&other)]);
    assert_eq!(sb.goal_count(), 1);
}

#[test]
fn deployed_agents_equal_the_resolved_preview_for_every_member() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    let preview = sb.json(&[
        "org",
        "template",
        "show",
        "--template-dir",
        path_arg(&root),
        "--resolved",
    ]);
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    let agents = sb.json(&["agent", "list"]);
    let mut mismatches = Vec::new();
    for want in preview["resolved_agents"].as_array().unwrap() {
        let have = agents
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == want["name"])
            .expect("deployed");
        let same = have["role"] == want["role"]
            && have["adapter"] == want["adapter"]
            && have["config"]["model"] == want["model"]
            && have["config"]["skills"] == want["skills"]
            && have["config"]["max_turns"] == want["max_turns"];
        if !same {
            mismatches.push(want["name"].to_string());
        }
    }
    assert!(
        mismatches.is_empty(),
        "preview and deployment differ for {mismatches:?}"
    );
}

#[test]
fn without_overrides_each_member_takes_its_own_adapter_and_inherits_the_rest() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    let got = [
        deployed(&sb, "boss"),
        deployed(&sb, "cx"),
        deployed(&sb, "shellie"),
    ];
    assert_eq!(
        got,
        [
            dep("claude", Some("claude-opus-x"), Some(&["review"])),
            dep("codex", Some("gpt-x"), Some(&["review"])),
            dep("shell", None, Some(&["review"])),
        ]
    );
}

#[test]
fn an_adapter_override_drops_models_bound_to_the_old_adapter_and_keeps_skills() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    sb.ok(&[
        "org",
        "deploy",
        "--template-dir",
        path_arg(&root),
        "--adapter",
        "codex",
    ]);
    // `boss` inherited a claude-bound model: it must not follow to codex.
    // `cx` already was codex, so its own model stays valid.
    let got = [
        deployed(&sb, "boss"),
        deployed(&sb, "cx"),
        deployed(&sb, "own-sk"),
    ];
    assert_eq!(
        got,
        [
            dep("codex", None, Some(&["review"])),
            dep("codex", Some("gpt-x"), Some(&["review"])),
            dep("codex", None, Some(&["qa"])),
        ]
    );
}

#[test]
fn an_adapter_override_drops_a_members_own_model_when_it_belonged_to_another_adapter() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    // `cx` is codex with a codex-only model. Forcing claude must not hand
    // `gpt-x` to claude; the claude-bound org model is what applies instead.
    sb.ok(&[
        "org",
        "deploy",
        "--template-dir",
        path_arg(&root),
        "--adapter",
        "claude",
    ]);
    assert_eq!(
        deployed(&sb, "cx"),
        dep("claude", Some("claude-opus-x"), Some(&["review"]))
    );
}

#[test]
fn an_explicit_model_override_wins_for_every_member_that_takes_a_model() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    sb.ok(&[
        "org",
        "deploy",
        "--template-dir",
        path_arg(&root),
        "--adapter",
        "codex",
        "--model",
        "gpt-z",
    ]);
    let got = [
        deployed(&sb, "boss"),
        deployed(&sb, "cx"),
        deployed(&sb, "empty-sk"),
    ];
    assert_eq!(
        got,
        [
            dep("codex", Some("gpt-z"), Some(&["review"])),
            dep("codex", Some("gpt-z"), Some(&["review"])),
            dep("codex", Some("gpt-z"), Some(&[])),
        ]
    );
}

#[test]
fn a_model_override_alone_is_not_forced_onto_a_shell_member() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    sb.ok(&[
        "org",
        "deploy",
        "--template-dir",
        path_arg(&root),
        "--model",
        "m2",
    ]);
    assert_eq!(
        [deployed(&sb, "boss"), deployed(&sb, "shellie")],
        [
            dep("claude", Some("m2"), Some(&["review"])),
            dep("shell", None, Some(&["review"]))
        ]
    );
}

#[test]
fn skills_absent_empty_and_nonempty_deploy_as_inherit_disable_and_replace() {
    let sb = sandbox();
    sb.init_project();
    let root = mix_team(&sb);
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    let got = [
        deployed(&sb, "boss").skills,
        deployed(&sb, "empty-sk").skills,
        deployed(&sb, "own-sk").skills,
    ];
    assert_eq!(
        got,
        [
            Some(vec!["review".to_owned()]),
            Some(vec![]),
            Some(vec!["qa".to_owned()])
        ]
    );
}

#[test]
fn skills_absent_at_both_levels_deploy_without_a_skills_key() {
    let sb = sandbox();
    sb.init_project();
    let root = sb.work().join("plain");
    Bundle::new("plain", "a").agent("a", json!({})).write(&root);
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    assert_eq!(deployed(&sb, "a").skills, None);
}

#[test]
fn a_second_deploy_of_the_same_template_fails_and_adds_nothing() {
    let sb = sandbox();
    sb.init_project();
    let root = valid_dir(&sb, "trio");
    sb.ok(&["org", "deploy", "--template-dir", path_arg(&root)]);
    let (agents, entries) = (sb.agent_names(), sb.orchestry_entries());
    let output = sb.fail(&["org", "deploy", "--template-dir", path_arg(&root)]);
    assert_eq!(
        (sb.agent_names(), sb.orchestry_entries()),
        (agents, entries),
        "{}",
        transcript(&output)
    );
}

#[test]
fn a_name_collision_with_a_later_member_creates_nothing_at_all() {
    let sb = sandbox();
    sb.init_project();
    // `sec-reviewer` sorts mid-roster: 17 members are earlier in order.
    sb.ok(&[
        "agent",
        "add",
        "sec-reviewer",
        "--adapter",
        "shell",
        "--command",
        "true",
    ]);
    let (agents, entries) = (sb.agent_names(), sb.orchestry_entries());
    let output = sb.fail(&["org", "deploy", "--template", "builtin:a-team"]);
    assert_mentions(&output, "sec-reviewer");
    assert_eq!(
        (sb.agent_names(), sb.orchestry_entries()),
        (agents, entries)
    );
}

#[test]
fn an_invalid_member_input_fails_preflight_before_the_first_agent_is_created() {
    let cases: [(&str, Value); 4] = [
        (
            "shell member without a command",
            json!({"adapter": "shell"}),
        ),
        ("unsupported adapter", json!({"adapter": "emacs"})),
        ("zero timeout", json!({"timeout_ms": 0})),
        ("zero max_turns", json!({"max_turns": 0})),
    ];
    for (case, extra) in cases {
        let sb = sandbox();
        sb.init_project();
        let root = sb.work().join("pre");
        // The offender is the *last* member: earlier ones would be created
        // first by a deploy that validated lazily.
        Bundle::new("pre", "a")
            .agent("a", json!({}))
            .agent("b", json!({}))
            .agent("c", json!({}))
            .agent("zz-bad", extra)
            .write(&root);
        let (agents, entries) = (sb.agent_names(), sb.orchestry_entries());
        let output = sb.run(&["org", "deploy", "--template-dir", path_arg(&root)]);
        assert_clean_failure(&output, &["org", "deploy"]);
        assert_eq!(
            (sb.agent_names(), sb.orchestry_entries()),
            (agents, entries),
            "[{case}] created something\n{}",
            transcript(&output)
        );
    }
}

#[test]
fn an_unusable_cli_override_fails_before_anything_is_created() {
    let cases: [&[&str]; 2] = [
        // `a-team` members declare no command, so a shell override is invalid.
        &[
            "org",
            "deploy",
            "--template",
            "builtin:a-team",
            "--adapter",
            "shell",
        ],
        &[
            "org",
            "deploy",
            "--template",
            "builtin:a-team",
            "--adapter",
            "emacs",
        ],
    ];
    for args in cases {
        let sb = sandbox();
        sb.init_project();
        let (agents, entries) = (sb.agent_names(), sb.orchestry_entries());
        sb.fail(args);
        assert_eq!(
            (sb.agent_names(), sb.orchestry_entries()),
            (agents, entries),
            "{args:?}"
        );
    }
}

/// Ids named in a deployment-failure message, from `agentIds` of its JSON
/// survivors report.
fn survivor_report(message: &str) -> Value {
    let start = message
        .find("Surviving entities: ")
        .expect("survivor report")
        + "Surviving entities: ".len();
    let json_part = &message[start..];
    let end = json_part.rfind('}').expect("end of report") + 1;
    serde_json::from_str(&json_part[..end]).expect("survivor report is JSON")
}

#[test]
fn a_failure_creating_the_team_reports_every_surviving_agent_id_and_the_stage() {
    let sb = sandbox();
    sb.init_project();
    let ids = |sb: &Sandbox| -> Vec<String> {
        let mut ids: Vec<String> = sb
            .json(&["agent", "list"])
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    };
    let before = ids(&sb);
    fs::write(sb.project().join(".orchestry/teams"), "not a directory").unwrap();
    let output = sb.fail(&["org", "deploy", "--template", "builtin:a-team"]);
    let message = stderr(&output);

    let mut reported: Vec<String> = survivor_report(&message)["agentIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    reported.sort();
    let created: Vec<String> = ids(&sb)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(
        (
            message.contains("failed at team"),
            reported.len(),
            reported == created
        ),
        (true, 27, true),
        "the report must name exactly the agents that exist now:\n{message}"
    );
}

#[test]
fn a_failure_creating_the_goal_reports_the_agents_and_the_team_that_survive() {
    let sb = sandbox();
    sb.init_project();
    let goals = sb.project().join(".orchestry/goals");
    fs::remove_dir(&goals).unwrap();
    fs::write(&goals, "not a directory").unwrap();
    let root = valid_dir(&sb, "trio");
    let output = sb.fail(&[
        "org",
        "deploy",
        "--template-dir",
        path_arg(&root),
        "--goal",
        "G",
    ]);
    let message = stderr(&output);
    let report = survivor_report(&message);
    assert_eq!(
        (
            message.contains("failed at goal"),
            report["agentIds"].as_array().unwrap().len(),
            report["teamId"].is_string(),
            sb.team_count()
        ),
        (true, 3, true, 1)
    );
}

#[test]
fn legacy_positional_deploy_still_deploys_the_legacy_preset() {
    let sb = sandbox();
    sb.init_project();
    let out = sb.json(&["org", "deploy", "startup-mvp"]);
    assert_eq!(out["agentIds"].as_array().unwrap().len(), 6);
}

#[test]
fn legacy_positional_deploy_of_an_unknown_preset_fails_and_creates_nothing() {
    let sb = sandbox();
    sb.init_project();
    let entries = sb.orchestry_entries();
    sb.fail(&["org", "deploy", "no-such-preset"]);
    assert_eq!(sb.orchestry_entries(), entries);
}

#[test]
fn org_list_keeps_the_ten_legacy_records_in_order_and_appends_a_team() {
    let sb = sandbox();
    sb.init_project();
    let listing = sb.json(&["org", "list"]);
    let keys: Vec<&str> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "startup-mvp",
            "pr-review-corp",
            "migration-squad",
            "security-dept",
            "test-factory",
            "content-agency",
            "data-lab",
            "sales-machine",
            "bugfix-dept",
            "docs-team",
            "a-team",
        ]
    );
}

#[test]
fn the_a_team_record_in_org_list_namespaces_its_shop_keys_and_points_at_the_lead() {
    let sb = sandbox();
    sb.init_project();
    let listing = sb.json(&["org", "list"]);
    let record = listing.as_array().unwrap().last().unwrap();
    let keys: Vec<&str> = record["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["shop_key"].as_str().unwrap())
        .collect();
    assert_eq!(
        (
            keys.len(),
            keys.iter().all(|k| k.starts_with("a-team/")),
            record["lead_index"].clone()
        ),
        (
            27,
            true,
            json!(A_TEAM.iter().position(|n| *n == "orchestrator").unwrap())
        )
    );
}

#[test]
fn a_legacy_preset_selected_by_the_new_flag_deploys_what_its_preview_shows() {
    let sb = sandbox();
    sb.init_project();
    let preview = sb.json(&["org", "template", "show", "builtin:data-lab", "--resolved"]);
    sb.ok(&["org", "deploy", "--template", "builtin:data-lab"]);
    let agents = sb.json(&["agent", "list"]);
    let mut mismatches = Vec::new();
    for want in preview["resolved_agents"].as_array().unwrap() {
        let have = agents
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == want["name"]);
        if have.map(|a| (&a["role"], &a["adapter"])) != Some((&want["role"], &want["adapter"])) {
            mismatches.push(want["name"].to_string());
        }
    }
    assert!(
        mismatches.is_empty(),
        "preview and deployment differ for {mismatches:?}"
    );
}

#[test]
fn deploying_the_a_team_creates_27_agents_and_an_orchestrator_led_team() {
    let sb = sandbox();
    sb.init_project();
    let out = sb.json(&["org", "deploy", "--template", "builtin:a-team"]);
    let lead_id = out["team"]["lead_agent_id"].as_str().unwrap();
    let agents = sb.json(&["agent", "list"]);
    let lead = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == lead_id)
        .unwrap();
    assert_eq!(
        (
            out["agentIds"].as_array().unwrap().len(),
            out["team"]["name"].clone(),
            lead["name"].clone()
        ),
        (27, json!("a-team"), json!("orchestrator"))
    );
}

// ═══ D7 — the template shop ════════════════════════════════════════════════

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// `orch` on a real pty, run through `sh` so the terminal's settings can be
/// recorded before and after: a picker that leaves the terminal in raw mode
/// fails `terminal_restored`. A reader thread forwards the screen bytes and
/// every wait is a `recv_timeout`, so there are no sleeps and a stuck picker
/// fails the test instead of hanging the suite.
struct Pty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    chunks: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
    tty_before: PathBuf,
    tty_after: PathBuf,
}

/// How long the picker may take to show something, or to exit. Generous for a
/// loaded machine; a hang fails after this rather than hanging the suite.
const PTY_WAIT: Duration = Duration::from_secs(20);

/// macOS `stty -g` (`gfmt1:...`) with the `PENDIN` bit of `lflag` cleared.
/// The kernel sets `PENDIN` whenever a tty returns to canonical mode and
/// clears it on the next read, so it is driver state rather than a mode a
/// program can restore: `tcsetattr` neither sets nor clears it. Other
/// formats (Linux) are compared as is.
fn without_pendin(stty: &str) -> String {
    const PENDIN: u64 = 0x2000_0000;
    if !stty.starts_with("gfmt1:") {
        return stty.to_owned();
    }
    stty.trim_end()
        .split(':')
        .map(|field| match field.strip_prefix("lflag=") {
            Some(hex) => match u64::from_str_radix(hex, 16) {
                Ok(lflag) => format!("lflag={:x}", lflag & !PENDIN),
                Err(_) => field.to_owned(),
            },
            None => field.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(":")
}

const PTY_WRAPPER: &str = r#"stty -g > "$1"; after="$2"; orch="$3"; shift 3; "$orch" "$@"; code=$?; stty -g > "$after"; exit $code"#;

fn pty_orch(sb: &Sandbox, args: &[&str], rows: u16, cols: u16) -> Pty {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let tty_before = sb.dir.path().join("tty-before.txt");
    let tty_after = sb.dir.path().join("tty-after.txt");
    let mut command = CommandBuilder::new("/bin/sh");
    command.env_clear();
    command.args(["-c", PTY_WRAPPER, "sh"]);
    command.arg(&tty_before);
    command.arg(&tty_after);
    command.arg(assert_cmd::cargo::cargo_bin("orch"));
    command.args(args);
    command.cwd(sb.project());
    command.env("HOME", sb.home());
    command.env("ORCHESTRY_GLOBAL_CONFIG_PATH", sb.home().join("global.yml"));
    command.env("TERM", "xterm-256color");
    command.env("PATH", "/usr/bin:/bin");
    command.env("NO_COLOR", "1");
    let child = pair
        .slave
        .spawn_command(command)
        .expect("spawn orch on a pty");
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
        master: pair.master,
        chunks,
        seen: Vec::new(),
        tty_before,
        tty_after,
    }
}

impl Pty {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// Read until `needle` appears in everything seen so far.
    fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + PTY_WAIT;
        while !self.text().contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(_) => panic!("never saw {needle:?}; screen so far:\n{:?}", self.text()),
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        use std::io::Write;
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    /// Drain to the end of the session and return the exit code.
    fn finish(&mut self) -> u32 {
        let deadline = Instant::now() + PTY_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = self.child.kill();
                    panic!("orch did not exit; screen:\n{:?}", self.text());
                }
            }
        }
        self.child.wait().expect("wait").exit_code()
    }

    /// Read until the latest picker frame satisfies `wanted`, so a test that
    /// changes the terminal size waits for the redraw instead of racing it.
    fn expect_frame(&mut self, wanted: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + PTY_WAIT;
        loop {
            if self.text().contains("AGENT SHOP") && wanted(&self.last_frame()) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(_) => panic!(
                    "no picker frame satisfied the wait; last frame:\n{:#?}",
                    self.last_frame()
                ),
            }
        }
    }

    /// The first complete picker frame drawn after `mark` bytes of output
    /// that shows `selected` and `last_row` (the footer). A frame is complete once
    /// its last row is on screen. Unlike `last_frame`, it ignores everything
    /// drawn before `mark`, so it names the frame an input produced rather
    /// than any frame that happens to look right later.
    fn first_frame_after(&mut self, mark: usize, selected: &str, last_row: &str) -> Vec<String> {
        let deadline = Instant::now() + PTY_WAIT;
        loop {
            let text =
                String::from_utf8_lossy(&self.seen[mark.min(self.seen.len())..]).into_owned();
            // The first piece is the tail of a frame drawn before `mark`.
            for piece in text.split("\x1b[2J").skip(1) {
                if piece.contains(selected) && piece.contains(last_row) {
                    return piece
                        .replace("\x1b[H", "")
                        .split("\r\n")
                        .map(str::to_owned)
                        .collect();
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(_) => panic!("no frame showed {selected:?} after the input:\n{text:?}"),
            }
        }
    }

    fn terminal_restored(&self) -> bool {
        let before = fs::read_to_string(&self.tty_before).expect("recorded tty before");
        let after = fs::read_to_string(&self.tty_after).expect("recorded tty after");
        !before.trim().is_empty() && without_pendin(&before) == without_pendin(&after)
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize");
    }

    /// The picker redraws the whole screen after `ESC[2J`; frames are what
    /// lies between clears. Returns the last frame that is a picker frame.
    fn last_frame(&self) -> Vec<String> {
        let text = self.text();
        let frame = text
            .split("\x1b[2J")
            .filter(|f| f.contains("AGENT SHOP"))
            .last()
            .expect("a picker frame");
        frame
            .replace("\x1b[H", "")
            .split("\r\n")
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";
const ENTER: &[u8] = b"\r";

/// Terminal cells a string occupies, treating East Asian wide characters as
/// two. Enough for the fixtures used here.
fn cells(s: &str) -> usize {
    s.chars()
        .map(|c| match c as u32 {
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6 => 2,
            _ => 1,
        })
        .sum()
}

fn shop_dir(sb: &Sandbox) -> PathBuf {
    valid_dir(sb, "shop")
}

#[test]
fn selecting_a_row_installs_exactly_that_agent_as_its_resolved_input() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let preview = sb.json(&[
        "org",
        "template",
        "show",
        "--template-dir",
        path_arg(&root),
        "--resolved",
    ]);
    let before = sb.agent_names();

    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    pty.expect("AGENT SHOP");
    pty.send(DOWN);
    pty.expect("▸ reviewer");
    pty.send(ENTER);
    let code = pty.finish();

    let mut expected = before;
    expected.push("reviewer".into());
    expected.sort();
    let installed = sb.json(&["agent", "list"]);
    let reviewer = installed
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "reviewer")
        .unwrap();
    let want = preview["resolved_agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "reviewer")
        .unwrap();
    assert_eq!(
        (
            code,
            sb.agent_names(),
            reviewer["role"].clone(),
            reviewer["adapter"].clone(),
            pty.terminal_restored()
        ),
        (
            0,
            expected,
            want["role"].clone(),
            want["adapter"].clone(),
            true
        )
    );
}

#[test]
fn selecting_from_a_template_shop_creates_no_team_and_no_goal() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    pty.expect("AGENT SHOP");
    pty.send(ENTER);
    assert_eq!(pty.finish(), 0);
    assert_eq!((sb.team_count(), sb.goal_count()), (0, 0));
}

#[test]
fn cancelling_the_template_shop_writes_nothing_and_restores_the_terminal() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let (agents, entries, tree) = (sb.agent_names(), sb.orchestry_entries(), snapshot(&root));
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    pty.expect("AGENT SHOP");
    pty.send(DOWN);
    pty.send(b"q");
    let code = pty.finish();
    assert_eq!(
        (
            code,
            sb.agent_names(),
            sb.orchestry_entries(),
            snapshot(&root),
            pty.terminal_restored()
        ),
        (0, agents, entries, tree, true)
    );
}

#[test]
fn installing_from_the_a_team_shop_creates_the_resolved_member_and_only_that_member() {
    let sb = sandbox();
    sb.init_project();
    let preview = sb.json(&["org", "template", "show", "builtin:a-team", "--resolved"]);
    let before = sb.agent_names();
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template", "builtin:a-team"],
        30,
        100,
    );
    pty.expect("AGENT SHOP");
    pty.send(DOWN);
    pty.send(DOWN);
    pty.expect("▸ backend-eng");
    pty.send(ENTER);
    assert_eq!(pty.finish(), 0);
    let agents = sb.json(&["agent", "list"]);
    let backend = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "backend-eng")
        .unwrap();
    let want = preview["resolved_agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "backend-eng")
        .unwrap();
    assert_eq!(
        (
            sb.agent_names().len() - before.len(),
            backend["role"].clone(),
            sb.team_count()
        ),
        (1, want["role"].clone(), 0)
    );
}

#[test]
fn installing_a_name_that_already_exists_fails_clearly_and_adds_nothing() {
    let sb = sandbox();
    sb.init_project();
    sb.ok(&[
        "agent",
        "add",
        "reviewer",
        "--adapter",
        "shell",
        "--command",
        "true",
    ]);
    let root = shop_dir(&sb);
    let agents = sb.agent_names();
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    pty.expect("AGENT SHOP");
    pty.send(DOWN);
    pty.send(ENTER);
    let code = pty.finish();
    assert_eq!(
        (
            code != 0,
            pty.text().contains("reviewer"),
            sb.agent_names(),
            pty.terminal_restored()
        ),
        (true, true, agents, true)
    );
}

#[test]
fn a_narrow_terminal_gets_rows_clipped_to_its_width_and_height() {
    let sb = sandbox();
    sb.init_project();
    let root = sb.work().join("hostile");
    let long = "界".repeat(60);
    Bundle::new("hostile", "lead")
        .agent(
            "lead",
            json!({"description": format!("{long}\u{1b}[31mRED\u{1b}[2J\nSECOND\r\u{7}")}),
        )
        .agent("worker", json!({"description": "short"}))
        .write(&root);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        8,
        40,
    );
    pty.expect("AGENT SHOP");
    pty.send(b"q");
    pty.finish();
    let frame = pty.last_frame();
    let widest = frame.iter().map(|l| cells(l)).max().unwrap();
    assert!(
        widest <= 40 && frame.len() <= 8,
        "frame is {} lines, widest {widest} cells:\n{frame:#?}",
        frame.len()
    );
}

#[test]
fn control_characters_in_a_stored_description_never_reach_the_terminal() {
    let sb = sandbox();
    sb.init_project();
    let root = sb.work().join("hostile");
    Bundle::new("hostile", "lead")
        .agent(
            "lead",
            json!({"description": "before\u{1b}[31mRED\u{1b}[0m\u{7}after"}),
        )
        .agent("worker", json!({}))
        .write(&root);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    pty.expect("AGENT SHOP");
    pty.send(b"q");
    pty.finish();
    let screen = pty.text();
    assert_eq!(
        (screen.contains("\x1b[31m"), screen.contains('\u{7}')),
        (false, false)
    );
}

#[test]
fn picker_rows_start_with_a_carriage_return_so_raw_mode_never_staircases() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    pty.expect("AGENT SHOP");
    pty.send(b"q");
    pty.finish();
    let bytes = pty.seen.clone();
    let bare_lf = bytes
        .iter()
        .enumerate()
        .filter(|(i, b)| **b == b'\n' && (*i == 0 || bytes[i - 1] != b'\r'))
        .count();
    assert_eq!(bare_lf, 0, "bare LF in raw-mode output:\n{:?}", pty.text());
}

#[test]
fn a_tall_template_stays_within_the_terminal_height_with_the_cursor_row_visible() {
    let sb = sandbox();
    sb.init_project();
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template", "builtin:a-team"],
        12,
        80,
    );
    pty.expect("AGENT SHOP");
    for _ in 0..20 {
        pty.send(DOWN);
    }
    // senior-eng-2 is the 21st member in name order.
    pty.expect("▸ senior-eng-2");
    pty.send(b"q");
    pty.finish();
    let frame = pty.last_frame();
    assert_eq!(
        (
            frame.len() <= 12,
            frame.iter().any(|l| l.contains("▸ senior-eng-2"))
        ),
        (true, true),
        "{frame:#?}"
    );
}

#[test]
fn resizing_narrower_redraws_within_the_new_width() {
    let sb = sandbox();
    sb.init_project();
    let root = sb.work().join("wide");
    Bundle::new("wide", "lead")
        .agent("lead", json!({"description": "x".repeat(200)}))
        .agent("worker", json!({}))
        .write(&root);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        100,
    );
    pty.expect("AGENT SHOP");
    // A keypress that is answered proves the picker is reading events, i.e.
    // is listening for the resize; one sent before that would be lost.
    pty.send(DOWN);
    pty.expect("▸ worker");
    // Resize with no key in flight: a key written in the same instant as the
    // resize can swallow it (see the hand-off), which is not what this test
    // is about.
    pty.resize(10, 40);
    pty.expect_frame(|f| {
        f.iter().any(|l| l.contains("▸ worker")) && f.iter().all(|l| cells(l) <= 40)
    });
    pty.send(b"q");
    pty.finish();
    let frame = pty.last_frame();
    let widest = frame.iter().map(|l| cells(l)).max().unwrap();
    assert!(
        widest <= 40 && frame.len() <= 10,
        "{widest} cells, {} lines:\n{frame:#?}",
        frame.len()
    );
}

/// A lead with a 200-character description and `members` more agents, so the
/// frame's width and its page length both depend on the terminal size.
fn crowded_template(sb: &Sandbox, members: usize) -> PathBuf {
    let root = sb.work().join("crowded");
    let mut bundle =
        Bundle::new("crowded", "lead").agent("lead", json!({"description": "x".repeat(200)}));
    for n in 0..members {
        bundle = bundle.agent(&format!("w{n:02}"), json!({}));
    }
    bundle.write(&root);
    root
}

#[test]
fn a_shrink_delivered_together_with_a_key_still_draws_the_next_frame_at_the_new_size() {
    let sb = sandbox();
    sb.init_project();
    let root = crowded_template(&sb, 30);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        100,
    );
    pty.expect("AGENT SHOP");
    // An answered key proves the picker is reading events.
    pty.send(DOWN);
    pty.expect("▸ w00");
    let mark = pty.seen.len();
    // Back to back, no wait between them: the resize and the key reach the
    // picker in the same wakeup.
    pty.resize(10, 40);
    pty.send(UP);
    let frame = pty.first_frame_after(mark, "▸ lead", "of 31");
    pty.send(b"q");
    pty.finish();
    let widest = frame.iter().map(|l| cells(l)).max().unwrap();
    assert!(
        widest <= 40 && frame.len() <= 10,
        "{widest} cells, {} lines:\n{frame:#?}",
        frame.len()
    );
}

#[test]
fn a_grow_delivered_together_with_a_key_still_draws_the_next_frame_at_the_new_size() {
    let sb = sandbox();
    sb.init_project();
    let root = crowded_template(&sb, 30);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        10,
        40,
    );
    pty.expect("AGENT SHOP");
    pty.send(DOWN);
    pty.expect("▸ w00");
    let mark = pty.seen.len();
    pty.resize(24, 100);
    pty.send(UP);
    let frame = pty.first_frame_after(mark, "▸ lead", "of 31");
    pty.send(b"q");
    pty.finish();
    let widest = frame.iter().map(|l| cells(l)).max().unwrap();
    assert!(
        widest > 40 && widest <= 100 && frame.len() > 10 && frame.len() <= 24,
        "{widest} cells, {} lines:\n{frame:#?}",
        frame.len()
    );
}

/// A key that lands in the same wakeup as SIGWINCH is still read. crossterm's
/// default (mio) source is edge-triggered and dropped the second readiness, so
/// the key sat unread until the next one; on Linux about one round in five lost
/// a key. Many rounds make that loss near-certain rather than occasional.
#[test]
fn a_key_sent_with_every_resize_is_never_left_unread() {
    let sb = sandbox();
    sb.init_project();
    let root = crowded_template(&sb, 30);
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        10,
        40,
    );
    pty.expect("AGENT SHOP");
    for round in 0..12u16 {
        let mark = pty.seen.len();
        let (rows, cols) = if round % 2 == 0 { (24, 100) } else { (10, 40) };
        pty.resize(rows, cols);
        pty.send(DOWN);
        pty.first_frame_after(mark, &format!("▸ w{round:02}"), "of 31");
    }
    pty.send(b"q");
    assert_eq!(pty.finish(), 0);
}

#[test]
fn an_invalid_template_fails_before_the_picker_starts() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    fs::write(root.join("agents/worker.yml"), "broken: [").unwrap();
    let agents = sb.agent_names();
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root)],
        24,
        80,
    );
    let code = pty.finish();
    let screen = pty.text();
    assert_eq!(
        (
            code != 0,
            screen.contains("AGENT SHOP"),
            screen.contains("\x1b[2J"),
            screen.contains("worker.yml"),
            sb.agent_names()
        ),
        (true, false, false, true, agents)
    );
}

#[test]
fn shop_list_on_a_terminal_prints_the_table_without_entering_the_picker() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let agents = sb.agent_names();
    let mut pty = pty_orch(
        &sb,
        &["agent", "shop", "--template-dir", path_arg(&root), "--list"],
        24,
        80,
    );
    let code = pty.finish();
    let screen = pty.text();
    assert_eq!(
        (
            code,
            screen.contains("reviewer"),
            screen.contains("AGENT SHOP"),
            sb.agent_names()
        ),
        (0, true, false, agents)
    );
}

#[test]
fn shop_list_as_json_serializes_the_authoring_members_and_creates_nothing() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let (agents, entries) = (sb.agent_names(), sb.orchestry_entries());
    let listing = sb.json(&["agent", "shop", "--template-dir", path_arg(&root), "--list"]);
    let authoring = sb.json(&[
        "org",
        "template",
        "agent",
        "list",
        "--template-dir",
        path_arg(&root),
    ]);
    assert_eq!(
        (listing, sb.agent_names(), sb.orchestry_entries()),
        (authoring, agents, entries)
    );
}

#[test]
fn shop_list_yaml_and_json_describe_the_same_members() {
    let sb = sandbox();
    sb.init_project();
    let args = ["agent", "shop", "--template", "builtin:a-team", "--list"];
    assert_eq!(sb.yaml(&args), sb.json(&args));
}

#[test]
fn without_a_terminal_the_template_shop_prints_a_table_and_installs_nothing() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    let agents = sb.agent_names();
    let output = sb.ok(&["agent", "shop", "--template-dir", path_arg(&root)]);
    assert_eq!(
        (stdout(&output).contains("reviewer"), sb.agent_names()),
        (true, agents)
    );
}

#[test]
fn a_template_shop_rejects_a_broken_template_without_a_terminal_too() {
    let sb = sandbox();
    sb.init_project();
    let root = shop_dir(&sb);
    fs::write(root.join("org.yml"), "x: [").unwrap();
    let output = sb.fail(&["agent", "shop", "--template-dir", path_arg(&root), "--list"]);
    assert_mentions(&output, "org.yml");
}

#[test]
fn the_shop_without_a_selector_still_lists_the_legacy_catalogue() {
    let sb = sandbox();
    sb.init_project();
    let output = sb.ok(&["agent", "shop", "--list"]);
    let text = stdout(&output);
    assert_eq!(
        ["backend-dev", "growth-hacker", "fullstack-dev"].map(|k| text.contains(k)),
        [true; 3]
    );
}

// ═══ D11 — mutation safety ═════════════════════════════════════════════════

fn corrupt(root: &Path, member: &str) {
    fs::write(
        root.join("agents").join(format!("{member}.yml")),
        "broken: [",
    )
    .unwrap();
}

#[test]
fn a_malformed_non_lead_member_can_be_deleted_even_though_show_refuses_the_template() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    corrupt(&root, "worker");
    sb.fail(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    sb.ok(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "worker",
    ]);
    let shown = sb.json(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    assert_eq!(names(&shown["agents"]), ["lead", "reviewer"]);
}

#[test]
fn deleting_a_malformed_member_is_refused_while_another_member_is_also_malformed() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    corrupt(&root, "worker");
    corrupt(&root, "reviewer");
    let before = snapshot(&root);
    sb.fail(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "worker",
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn deleting_a_valid_member_is_refused_while_an_unrelated_member_is_malformed() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    corrupt(&root, "reviewer");
    let before = snapshot(&root);
    sb.fail(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "worker",
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn a_malformed_lead_cannot_be_deleted_even_though_it_is_never_parsed() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    corrupt(&root, "lead");
    let before = snapshot(&root);
    sb.fail(&[
        "org",
        "template",
        "agent",
        "delete",
        "--template-dir",
        path_arg(&root),
        "--agent",
        "lead",
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn adding_a_member_to_a_template_with_a_malformed_member_is_refused() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    corrupt(&root, "reviewer");
    let before = snapshot(&root);
    let input = write_input(&sb, "new.json", &json!({"name": "extra", "role": "x"}));
    sb.fail(&[
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        path_arg(&input),
    ]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn an_edit_cannot_repair_a_malformed_member_because_it_has_no_base_to_edit() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    corrupt(&root, "worker");
    let before = snapshot(&root);
    let (editor, _) = fake_editor(&sb, r#"{"name":"worker","role":"repaired"}"#, 0);
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
            "worker",
        ],
    );
    assert_clean_failure(&output, &["agent", "edit"]);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn a_mutation_fails_after_the_lock_budget_when_another_writer_holds_the_lock() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let before = snapshot(&root);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".template.lock"))
        .unwrap();
    lock.lock().expect("hold the template lock");
    let input = write_input(&sb, "new.json", &json!({"name": "extra", "role": "x"}));
    let args = [
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        path_arg(&input),
    ];

    // A reader is never blocked by the writers' lock.
    let shown_started = Instant::now();
    sb.ok(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    let shown_in = shown_started.elapsed();

    let started = Instant::now();
    let output = sb.run(&args);
    let waited = started.elapsed();
    assert_clean_failure(&output, &args);
    let intact = snapshot(&root) == before;

    // Released, the same command goes through.
    drop(lock);
    let retried = sb.run(&args);
    assert_eq!(
        (
            waited >= Duration::from_secs(4) && waited < Duration::from_secs(30),
            shown_in < Duration::from_secs(4),
            intact,
            retried.status.success(),
        ),
        (true, true, true, true),
        "waited {waited:?}; {}",
        transcript(&output)
    );
}

#[test]
fn two_writers_adding_the_same_member_leave_exactly_one_winner_intact() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let inputs: Vec<PathBuf> = ["first", "second"]
        .iter()
        .map(|who| {
            write_input(
                &sb,
                &format!("{who}.json"),
                &json!({"name": "contested", "role": format!("role from {who}")}),
            )
        })
        .collect();
    let handles: Vec<_> = inputs
        .iter()
        .map(|input| {
            let mut command = sb.command();
            command.args([
                "org",
                "template",
                "agent",
                "add",
                "--template-dir",
                path_arg(&root),
                "-f",
                path_arg(input),
            ]);
            std::thread::spawn(move || run_bounded(&mut command, RUN_BUDGET))
        })
        .collect();
    let results: Vec<Output> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, o)| o.status.success())
        .map(|(i, _)| i)
        .collect();
    let stored = fs::read_to_string(root.join("agents/contested.yml")).unwrap();
    assert_eq!(
        winners.len(),
        1,
        "{}",
        results.iter().map(transcript).collect::<String>()
    );
    assert!(
        stored.contains(["role from first", "role from second"][winners[0]]),
        "the stored file is not the winner's:\n{stored}"
    );
}

#[test]
fn concurrent_writers_adding_different_members_all_succeed_and_the_template_stays_valid() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let inputs: Vec<PathBuf> = (0..4)
        .map(|i| {
            write_input(
                &sb,
                &format!("m{i}.json"),
                &json!({"name": format!("extra-{i}"), "role": "x"}),
            )
        })
        .collect();
    let handles: Vec<_> = inputs
        .iter()
        .map(|input| {
            let mut command = sb.command();
            command.args([
                "org",
                "template",
                "agent",
                "add",
                "--template-dir",
                path_arg(&root),
                "-f",
                path_arg(input),
            ]);
            std::thread::spawn(move || run_bounded(&mut command, RUN_BUDGET))
        })
        .collect();
    let ok: Vec<bool> = handles
        .into_iter()
        .map(|h| h.join().unwrap().status.success())
        .collect();
    let shown = sb.json(&["org", "template", "show", "--template-dir", path_arg(&root)]);
    assert_eq!(
        (ok, names(&shown["agents"])),
        (
            vec![true; 4],
            [
                "extra-0", "extra-1", "extra-2", "extra-3", "lead", "reviewer", "worker"
            ]
            .map(String::from)
            .to_vec()
        )
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
fn a_publish_that_cannot_write_fails_loudly_and_leaves_the_template_as_it_was() {
    let sb = sandbox();
    let root = valid_dir(&sb, "dir");
    let agents_dir = root.join("agents");
    let guard = Writable(agents_dir.clone());
    fs::set_permissions(&agents_dir, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(agents_dir.join(".probe"), b"x").is_ok() {
        // Running as a user that ignores directory permissions: the failure
        // cannot be provoked this way.
        let _ = fs::remove_file(agents_dir.join(".probe"));
        return;
    }
    let before = snapshot(&root);
    let input = write_input(&sb, "new.json", &json!({"name": "extra", "role": "x"}));
    let output = sb.fail(&[
        "org",
        "template",
        "agent",
        "add",
        "--template-dir",
        path_arg(&root),
        "-f",
        path_arg(&input),
    ]);
    drop(guard);
    assert_eq!(snapshot(&root), before, "{}", transcript(&output));
}
