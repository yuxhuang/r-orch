//! Per-tool behaviour of `orch mcp`, through the real binary (design D16, T2).
//!
//! Black-box, like `mcp_stdio`: a hand-rolled JSON-RPC client talks to the
//! `orch` binary, and every expectation is typed out from the design and the
//! lead's rulings, never read back from the registry. What is covered:
//!
//! * every one of the 50 tools, once with a valid call and once with a call
//!   that must fail, and — where the CLI has `--json` — the tool result against
//!   the parsed CLI JSON of the same operation on the same project;
//! * the † projections (the entity re-read after a CLI command that prints no
//!   JSON) and the `null` results of deletions;
//! * the pause/force gate (decision-2 E1) with a second goal sharing the
//!   assignee, and its destructive-mode counterpart;
//! * the concurrency and recovery evidence decision-2 E2 asks for;
//! * the worker matrix of design D19, driven through the real shell adapter;
//! * the serialized `tools/list` size of the three modes (design D20).
//!
//! Hermetic: every project is a temp directory initialised by the real binary,
//! `HOME` and the global config are private (see `mcp_common::orch_command`),
//! `PATH` holds no agent CLI, and no test sleeps to decide an outcome.

mod mcp_common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use mcp_common::{
    DESTRUCTIVE, EXPECTED, McpServer, ProjectDir, READ_ONLY, Rendezvous, error_code,
    initialized_project, initialized_project_unowned, orch_command, output_of, spawn_bounded,
    tool_text,
};
use serde_json::{Value, json};

// ── fixtures ──────────────────────────────────────────────────────────────

/// An initialised project with one running `orch mcp` against it.
struct Fx {
    // Declared first so the server is killed before its directory is removed.
    server: McpServer,
    dir: ProjectDir,
}

impl Fx {
    /// Default mode, no identity.
    fn new() -> Self {
        Self::with(&[], &[])
    }

    /// `--allow-destructive`.
    fn destructive() -> Self {
        Self::with(&["--allow-destructive"], &[])
    }

    fn with(args: &[&str], envs: &[(&str, Option<&str>)]) -> Self {
        Self::on(initialized_project(), args, envs)
    }

    /// A server on an already prepared project, for tests whose identity must
    /// name an agent that has to exist before the server starts.
    fn on(dir: ProjectDir, args: &[&str], envs: &[(&str, Option<&str>)]) -> Self {
        let mut server = McpServer::spawn_with(dir.path(), args, envs);
        server.initialize();
        Self { server, dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The whole JSON-RPC response of one call.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.server.call_tool(tool, arguments)
    }

    /// A successful call's JSON value.
    fn ok(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self.call(tool, arguments.clone());
        assert!(
            response.get("error").is_none(),
            "{tool} {arguments} returned a protocol error: {response}"
        );
        assert_eq!(
            response["result"]["isError"],
            json!(false),
            "{tool} {arguments} returned a tool error: {response}"
        );
        serde_json::from_str(tool_text(&response))
            .unwrap_or_else(|err| panic!("{tool} text is not JSON ({err}): {response}"))
    }

    /// The text of a tool-level failure (`isError: true`); fails the test on a
    /// success or a protocol error.
    fn tool_error(&mut self, tool: &str, arguments: Value) -> String {
        let response = self.call(tool, arguments.clone());
        assert!(
            response.get("error").is_none(),
            "{tool} {arguments} should be a tool error, got a protocol error: {response}"
        );
        assert_eq!(
            response["result"]["isError"],
            json!(true),
            "{tool} {arguments} should have failed: {response}"
        );
        tool_text(&response).to_owned()
    }

    /// The JSON-RPC error code of a call the server refuses before running it.
    fn rpc_code(&mut self, tool: &str, arguments: Value) -> i64 {
        let response = self.call(tool, arguments.clone());
        error_code(&response)
            .unwrap_or_else(|| panic!("{tool} {arguments} should be a protocol error: {response}"))
    }

    /// The full response of a call that must be a protocol error.
    fn rpc_error(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self.call(tool, arguments.clone());
        assert!(
            response.get("error").is_some(),
            "{tool} {arguments} should be a protocol error: {response}"
        );
        response
    }

    /// `orch --json <args>` on this project; the parsed stdout.
    fn cli(&self, args: &[&str]) -> Value {
        cli_json(self.root(), args)
    }

    /// `orch <args>` on this project, asserting success.
    fn cli_plain(&self, args: &[&str]) {
        let output = output_of(orch_command(self.root()).args(args));
        assert!(
            output.status.success(),
            "orch {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A shell agent created through the CLI; returns its id.
    fn agent(&self, name: &str) -> String {
        let created = self.cli(&[
            "agent",
            "add",
            name,
            "--adapter",
            "shell",
            "--command",
            "true",
        ]);
        created["id"].as_str().expect("agent id").to_owned()
    }

    /// A todo task created through the CLI; returns its id.
    fn task(&self, title: &str) -> String {
        self.cli(&["task", "add", title])["id"]
            .as_str()
            .expect("task id")
            .to_owned()
    }

    /// Put a stored task into `status` (and optionally add a label), the way a
    /// finished run would have left it. The index caches status, so it is
    /// dropped and rebuilt from the entity files on the next read.
    fn force_task(&self, id: &str, status: &str, label: Option<&str>) {
        let path = self.root().join(format!(".orchestry/tasks/{id}.yml"));
        let text = std::fs::read_to_string(&path).expect("read task file");
        let mut out = String::new();
        for line in text.lines() {
            if line.starts_with("status: ") {
                out.push_str(&format!("status: {status}\n"));
            } else if line == "labels: []" && label.is_some() {
                out.push_str(&format!("labels:\n- {}\n", label.unwrap_or_default()));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        std::fs::write(&path, out).expect("write task file");
        let _ = std::fs::remove_file(self.root().join(".orchestry/tasks/_index.json"));
    }

    /// Make a stored task's attempt counter `attempts`, the way earlier failed
    /// runs would have left it (the index is rebuilt on the next read).
    fn force_attempts(&self, id: &str, attempts: u32) {
        let path = self.root().join(format!(".orchestry/tasks/{id}.yml"));
        let text = std::fs::read_to_string(&path).expect("read task file");
        let mut replaced = false;
        let mut out = String::new();
        for line in text.lines() {
            if line.starts_with("attempts: ") {
                out.push_str(&format!("attempts: {attempts}\n"));
                replaced = true;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        assert!(replaced, "no `attempts:` line in {text}");
        std::fs::write(&path, out).expect("write task file");
        let _ = std::fs::remove_file(self.root().join(".orchestry/tasks/_index.json"));
    }

    /// Write a definition document inside the project and return its
    /// project-relative name.
    fn doc(&self, name: &str, text: &str) -> String {
        std::fs::write(self.root().join(name), text).expect("write document");
        name.to_owned()
    }
}

/// `orch --json <args>` in `cwd`; the parsed stdout, or a failure with stderr.
fn cli_json(cwd: &Path, args: &[&str]) -> Value {
    let output = output_of(orch_command(cwd).arg("--json").args(args));
    assert!(
        output.status.success(),
        "orch --json {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "orch --json {args:?} did not print JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Remove the fields that differ between two creations of the same thing.
fn without_volatile(mut value: Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["id", "created_at", "updated_at", "expires_at", "joined_at"] {
                    map.remove(key);
                }
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut value);
    value
}

/// Every file under `dir`, relative path to bytes. The evidence that a refused
/// call changed nothing on disk.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(
                    path.strip_prefix(base).expect("under base").to_owned(),
                    bytes,
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .expect("array")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

// ── task ──────────────────────────────────────────────────────────────────

#[test]
fn task_add_creates_a_task_with_the_same_shape_as_the_cli() {
    let mut fx = Fx::new();
    let via_cli = fx.cli(&[
        "task", "add", "T", "-d", "D", "-p", "2", "-l", "a,b", "--scope", "src/**",
    ]);
    let via_mcp = fx.ok(
        "task_add",
        json!({"title": "T", "description": "D", "priority": 2, "labels": "a,b", "scope": "src/**"}),
    );
    assert_eq!(without_volatile(via_mcp), without_volatile(via_cli));
}

#[test]
fn task_add_result_equals_the_stored_task() {
    let mut fx = Fx::new();
    let created = fx.ok("task_add", json!({"title": "stored"}));
    let id = created["id"].as_str().expect("id").to_owned();
    assert_eq!(fx.cli(&["task", "show", &id]), created);
}

#[test]
fn task_add_defaults_priority_to_three() {
    let mut fx = Fx::new();
    assert_eq!(fx.ok("task_add", json!({"title": "p"}))["priority"], 3);
}

#[test]
fn task_add_with_a_blank_title_is_a_tool_error_that_creates_nothing() {
    let mut fx = Fx::new();
    let message = fx.tool_error("task_add", json!({"title": "   "}));
    assert!(message.to_lowercase().contains("title"), "{message}");
    assert_eq!(fx.cli(&["task", "list"]), json!([]));
}

#[test]
fn task_add_with_priority_zero_or_five_is_a_tool_error() {
    let mut fx = Fx::new();
    for priority in [0, 5] {
        let message = fx.tool_error("task_add", json!({"title": "x", "priority": priority}));
        assert!(message.to_lowercase().contains("priority"), "{message}");
    }
    assert_eq!(fx.cli(&["task", "list"]), json!([]));
}

#[test]
fn task_add_with_an_unknown_review_criterion_is_a_tool_error() {
    let mut fx = Fx::new();
    let message = fx.tool_error(
        "task_add",
        json!({"title": "x", "review_criteria": "magic"}),
    );
    assert!(message.contains("magic"), "{message}");
}

#[test]
fn task_add_with_a_string_priority_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.rpc_code("task_add", json!({"title": "x", "priority": "3"})),
        -32602
    );
}

#[test]
fn task_list_equals_the_cli_list_and_filters_by_status() {
    let mut fx = Fx::new();
    let a = fx.task("a");
    let b = fx.task("b");
    fx.force_task(&b, "failed", None);
    assert_eq!(fx.ok("task_list", json!({})), fx.cli(&["task", "list"]));
    let failed = fx.ok("task_list", json!({"status": "failed"}));
    assert_eq!(ids(&failed), vec![b]);
    assert_eq!(
        failed,
        fx.cli(&["task", "list", "--status", "failed"]),
        "filtered list matches the CLI"
    );
    assert_eq!(ids(&fx.ok("task_list", json!({"status": "todo"}))), vec![a]);
}

#[test]
fn task_list_with_an_unknown_status_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("task_list", json!({"status": "bogus"})), -32602);
}

#[test]
fn task_show_equals_the_cli_show() {
    let mut fx = Fx::new();
    let id = fx.task("shown");
    assert_eq!(
        fx.ok("task_show", json!({"id": id})),
        fx.cli(&["task", "show", &id])
    );
}

#[test]
fn task_show_of_a_missing_task_is_a_tool_error_naming_it() {
    let mut fx = Fx::new();
    let message = fx.tool_error("task_show", json!({"id": "tsk_nope"}));
    assert!(message.contains("tsk_nope"), "{message}");
}

#[test]
fn task_show_without_an_id_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("task_show", json!({})), -32602);
}

#[test]
fn task_assign_sets_the_assignee_to_the_agent_id() {
    let mut fx = Fx::new();
    let agent = fx.agent("worker");
    let id = fx.task("t");
    let assigned = fx.ok("task_assign", json!({"task_id": id, "agent_id": "worker"}));
    assert_eq!(assigned["assignee"], json!(agent));
    assert_eq!(fx.cli(&["task", "show", &id])["assignee"], json!(agent));
}

#[test]
fn task_assign_to_an_unknown_agent_is_a_tool_error_and_leaves_the_task_alone() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = fx.cli(&["task", "show", &id]);
    let message = fx.tool_error("task_assign", json!({"task_id": id, "agent_id": "ghost"}));
    assert!(message.contains("ghost"), "{message}");
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

#[test]
fn task_cancel_marks_the_task_cancelled_and_returns_the_reread_task() {
    let mut fx = Fx::destructive();
    let id = fx.task("t");
    let cancelled = fx.ok("task_cancel", json!({"id": id}));
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled, fx.cli(&["task", "show", &id]));
}

#[test]
fn task_cancel_of_a_finished_task_is_a_tool_error_and_keeps_its_status() {
    let mut fx = Fx::destructive();
    let id = fx.task("t");
    fx.force_task(&id, "done", None);
    let message = fx.tool_error("task_cancel", json!({"id": id}));
    assert!(message.to_lowercase().contains("transition"), "{message}");
    assert_eq!(fx.cli(&["task", "show", &id])["status"], "done");
}

#[test]
fn task_cancel_never_starts_an_orchestrator_or_leaves_a_project_lock() {
    let mut fx = Fx::destructive();
    let id = fx.task("t");
    let lock = fx.root().join(".orchestry/orchestrator.lock");
    let state = fx.root().join(".orchestry/state.json");
    let read = |path: &Path| std::fs::read(path).ok();
    let (lock_before, state_before) = (read(&lock), read(&state));
    fx.ok("task_cancel", json!({"id": id}));
    // The project's test daemon owns the lock and state; the cancel must not
    // touch either (it is store-only until the daemon-routed cancel lands).
    assert_eq!(
        read(&lock),
        lock_before,
        "task_cancel changed the project lock"
    );
    assert_eq!(read(&state), state_before, "task_cancel changed state.json");
}

#[test]
fn task_approve_moves_a_review_task_to_done_and_returns_it_reread() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    fx.force_task(&id, "review", None);
    let approved = fx.ok("task_approve", json!({"id": id}));
    assert_eq!(approved["status"], "done");
    assert_eq!(approved, fx.cli(&["task", "show", &id]));
}

#[test]
fn task_approve_releases_a_task_held_for_approval_without_completing_it() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    fx.force_task(&id, "todo", Some("awaiting-approval"));
    let released = fx.ok("task_approve", json!({"id": id}));
    assert_eq!(released["status"], "todo");
    assert_eq!(released["labels"], json!([]));
}

#[test]
fn task_approve_of_a_plain_todo_task_is_a_tool_error() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let message = fx.tool_error("task_approve", json!({"id": id}));
    assert!(message.to_lowercase().contains("transition"), "{message}");
    assert_eq!(fx.cli(&["task", "show", &id])["status"], "todo");
}

#[test]
fn task_reject_returns_a_review_task_to_todo_with_the_feedback() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    fx.force_task(&id, "review", None);
    let rejected = fx.ok("task_reject", json!({"id": id, "reason": "needs tests"}));
    assert_eq!(rejected["status"], "todo");
    assert_eq!(rejected["feedback"], "needs tests");
    assert_eq!(rejected, fx.cli(&["task", "show", &id]));
}

#[test]
fn task_reject_of_a_task_not_in_review_is_a_tool_error() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let message = fx.tool_error("task_reject", json!({"id": id}));
    assert!(message.to_lowercase().contains("transition"), "{message}");
}

#[test]
fn task_retry_resets_a_failed_task_to_todo_with_zero_attempts() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    fx.force_task(&id, "failed", None);
    fx.force_attempts(&id, 2);
    assert_eq!(
        fx.cli(&["task", "show", &id])["attempts"],
        2,
        "the precondition: earlier runs used attempts up"
    );
    let retried = fx.ok("task_retry", json!({"id": id}));
    assert_eq!(retried["status"], "todo");
    assert_eq!(retried["attempts"], 0);
    assert_eq!(retried, fx.cli(&["task", "show", &id]));
}

#[test]
fn task_retry_of_a_todo_task_is_a_tool_error() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let message = fx.tool_error("task_retry", json!({"id": id}));
    assert!(message.to_lowercase().contains("transition"), "{message}");
}

#[test]
fn task_update_replaces_the_authored_definition_from_a_project_file() {
    let mut fx = Fx::new();
    let id = fx.task("old");
    let file = fx.doc(
        "def.json",
        r#"{"title": "new title", "description": "new d", "priority": 1}"#,
    );
    let updated = fx.ok("task_update", json!({"id": id, "file": file}));
    assert_eq!(updated["title"], "new title");
    assert_eq!(updated["priority"], 1);
    assert_eq!(updated, fx.cli(&["task", "show", &id]));
}

#[test]
fn task_update_with_yaml_and_an_explicit_input_format() {
    let mut fx = Fx::new();
    let id = fx.task("old");
    let file = fx.doc("def.txt", "title: from yaml\ndescription: d\npriority: 2\n");
    let updated = fx.ok(
        "task_update",
        json!({"id": id, "file": file, "input_format": "yaml"}),
    );
    assert_eq!(updated["title"], "from yaml");
}

#[test]
fn task_update_with_a_missing_file_is_a_tool_error_and_changes_nothing() {
    let mut fx = Fx::new();
    let id = fx.task("old");
    let before = fx.cli(&["task", "show", &id]);
    let message = fx.tool_error("task_update", json!({"id": id, "file": "nope.json"}));
    // The CLI reports the same text for the same missing file.
    assert!(message.contains("read mutation input"), "{message}");
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

#[test]
fn task_patch_merges_only_the_given_fields() {
    let mut fx = Fx::new();
    let id = fx.cli(&["task", "add", "keep", "-d", "kept", "-p", "2"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let file = fx.doc("patch.json", r#"{"priority": 4}"#);
    let patched = fx.ok("task_patch", json!({"id": id, "file": file}));
    assert_eq!(patched["priority"], 4);
    assert_eq!(patched["title"], "keep");
    assert_eq!(patched["description"], "kept");
}

#[test]
fn task_patch_that_removes_a_required_field_is_a_tool_error() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let file = fx.doc("patch.json", r#"{"title": null}"#);
    fx.tool_error("task_patch", json!({"id": id, "file": file}));
    assert_eq!(fx.cli(&["task", "show", &id])["title"], "t");
}

#[test]
fn task_update_and_patch_with_file_dash_are_invalid_params() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    for tool in ["task_update", "task_patch"] {
        assert_eq!(fx.rpc_code(tool, json!({"id": id, "file": "-"})), -32602);
    }
}

// ── goal ──────────────────────────────────────────────────────────────────

#[test]
fn goal_add_creates_a_goal_with_the_same_shape_as_the_cli() {
    let mut fx = Fx::new();
    let agent = fx.agent("lead");
    let via_cli = fx.cli(&[
        "goal",
        "add",
        "G",
        "--description",
        "why",
        "--assignee",
        &agent,
    ]);
    let via_mcp = fx.ok(
        "goal_add",
        json!({"title": "G", "description": "why", "assignee": agent}),
    );
    assert_eq!(without_volatile(via_mcp), without_volatile(via_cli));
}

#[test]
fn goal_add_with_a_blank_title_is_a_tool_error() {
    let mut fx = Fx::new();
    fx.tool_error("goal_add", json!({"title": " "}));
    assert_eq!(fx.cli(&["goal", "list"]), json!([]));
}

#[test]
fn goal_list_equals_the_cli_and_an_empty_filter_means_all() {
    let mut fx = Fx::new();
    fx.cli(&["goal", "add", "one"]);
    fx.cli(&["goal", "add", "two"]);
    let all = fx.ok("goal_list", json!({}));
    assert_eq!(all, fx.cli(&["goal", "list"]));
    assert_eq!(fx.ok("goal_list", json!({"status": ""})), all);
}

#[test]
fn goal_list_with_an_unknown_status_is_an_empty_list_not_an_error() {
    let mut fx = Fx::new();
    fx.cli(&["goal", "add", "one"]);
    assert_eq!(fx.ok("goal_list", json!({"status": "bogus"})), json!([]));
}

#[test]
fn goal_show_includes_every_linked_task_and_omits_absent_progress() {
    let mut fx = Fx::new();
    let goal = fx.ok("goal_add", json!({"title": "G"}));
    let gid = goal["id"].as_str().expect("goal id").to_owned();
    let none = fx.ok("goal_show", json!({"id": gid}));
    assert_eq!(none["tasks"], json!([]));
    assert!(none.get("progress").is_none(), "progress absent, not null");
    let t1 = fx.ok("task_add", json!({"title": "a", "goal_id": gid}))["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let t2 = fx.ok("task_add", json!({"title": "b", "goal_id": gid}))["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let shown = fx.ok("goal_show", json!({"id": gid}));
    let mut linked = ids(&shown["tasks"]);
    linked.sort();
    let mut want = vec![t1, t2];
    want.sort();
    assert_eq!(linked, want);
}

#[test]
fn goal_show_equals_the_cli_show_fields() {
    let mut fx = Fx::new();
    let gid = fx.cli(&["goal", "add", "G"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_eq!(
        fx.ok("goal_show", json!({"id": gid})),
        fx.cli(&["goal", "show", &gid])
    );
}

#[test]
fn goal_show_of_a_missing_goal_is_a_tool_error() {
    let mut fx = Fx::new();
    let message = fx.tool_error("goal_show", json!({"id": "goal_nope"}));
    assert!(message.contains("goal_nope"), "{message}");
}

#[test]
fn goal_status_abandons_an_active_goal_in_default_mode() {
    let mut fx = Fx::new();
    let gid = fx.cli(&["goal", "add", "G"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let goal = fx.ok("goal_status", json!({"id": gid, "status": "abandoned"}));
    assert_eq!(goal["status"], "abandoned");
    assert_eq!(fx.cli(&["goal", "show", &gid])["status"], "abandoned");
}

#[test]
fn goal_status_with_an_illegal_transition_is_a_tool_error() {
    let mut fx = Fx::new();
    let gid = fx.cli(&["goal", "add", "G"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    fx.ok("goal_status", json!({"id": gid, "status": "abandoned"}));
    let message = fx.tool_error("goal_status", json!({"id": gid, "status": "active"}));
    assert!(message.contains("transition"), "{message}");
}

#[test]
fn goal_status_with_an_unknown_status_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.rpc_code("goal_status", json!({"id": "g", "status": "bogus"})),
        -32602
    );
}

#[test]
fn goal_update_changes_text_and_an_empty_assignee_unassigns() {
    let mut fx = Fx::new();
    let agent = fx.agent("lead");
    let gid = fx.cli(&["goal", "add", "G", "--assignee", &agent])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let renamed = fx.ok("goal_update", json!({"id": gid, "title": "G2"}));
    assert_eq!(renamed["title"], "G2");
    assert_eq!(renamed["assignee"], json!(agent), "omission preserves");
    let cleared = fx.ok("goal_update", json!({"id": gid, "assignee": ""}));
    assert!(
        cleared.get("assignee").is_none_or(Value::is_null),
        "{cleared}"
    );
}

#[test]
fn goal_update_with_a_blank_title_is_a_tool_error_that_keeps_the_goal() {
    let mut fx = Fx::new();
    let gid = fx.cli(&["goal", "add", "G"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    fx.tool_error("goal_update", json!({"id": gid, "title": "  "}));
    assert_eq!(fx.cli(&["goal", "show", &gid])["title"], "G");
}

#[test]
fn goal_delete_removes_the_goal_and_returns_its_id() {
    let mut fx = Fx::destructive();
    let gid = fx.cli(&["goal", "add", "G"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_eq!(
        fx.ok("goal_delete", json!({"id": gid})),
        json!({"deleted": gid})
    );
    assert_eq!(fx.cli(&["goal", "list"]), json!([]));
}

#[test]
fn goal_delete_of_a_missing_goal_is_a_tool_error() {
    let mut fx = Fx::destructive();
    fx.tool_error("goal_delete", json!({"id": "goal_nope"}));
}

// ── msg ───────────────────────────────────────────────────────────────────

#[test]
fn msg_send_delivers_one_message_with_the_same_shape_as_the_cli() {
    let mut fx = Fx::new();
    let to = fx.agent("rx");
    let via_cli = fx.cli(&["msg", "send", &to, "hi", "-s", "subj", "--from", "cli"]);
    let via_mcp = fx.ok(
        "msg_send",
        json!({"to_agent_id": to, "body": "hi", "subject": "subj", "from": "cli"}),
    );
    assert_eq!(
        via_mcp
            .as_array()
            .expect("Message[] even for one recipient")
            .len(),
        1
    );
    assert_eq!(without_volatile(via_mcp), without_volatile(via_cli));
}

/// A project with agents `me` and `rx` (ids returned), no server yet.
fn project_with_sender_and_receiver() -> (ProjectDir, String, String) {
    let dir = initialized_project();
    let make = |name: &str| {
        cli_json(
            dir.path(),
            &[
                "agent",
                "add",
                name,
                "--adapter",
                "shell",
                "--command",
                "true",
            ],
        )["id"]
            .as_str()
            .expect("agent id")
            .to_owned()
    };
    let me = make("me");
    let rx = make("rx");
    (dir, me, rx)
}

#[test]
fn msg_send_without_from_uses_the_environment_identity() {
    let (dir, me, rx) = project_with_sender_and_receiver();
    let mut fx = Fx::on(dir, &[], &[("ORCH_AGENT_ID", Some(&me))]);
    let sent = fx.ok("msg_send", json!({"to_agent_id": rx, "body": "b"}));
    assert_eq!(sent[0]["from_agent_id"], json!(me));
}

#[test]
fn msg_send_identity_precedence_is_explicit_from_then_flag_then_environment() {
    let (dir, me, rx) = project_with_sender_and_receiver();
    let mut fx = Fx::on(
        dir,
        &["--agent", &me],
        &[("ORCH_AGENT_ID", Some("env-agent-that-does-not-exist"))],
    );
    let by_flag = fx.ok("msg_send", json!({"to_agent_id": rx, "body": "b"}));
    assert_eq!(
        by_flag[0]["from_agent_id"],
        json!(me),
        "flag beats environment"
    );
    let explicit = fx.ok(
        "msg_send",
        json!({"to_agent_id": rx, "body": "b", "from": "cli"}),
    );
    assert_eq!(explicit[0]["from_agent_id"], "cli", "explicit beats flag");
}

#[test]
fn msg_send_without_from_and_without_identity_is_invalid_params_and_sends_nothing() {
    let mut fx = Fx::new();
    let to = fx.agent("rx");
    let response = fx.rpc_error("msg_send", json!({"to_agent_id": to, "body": "b"}));
    assert_eq!(error_code(&response), Some(-32602));
    assert_eq!(fx.cli(&["msg", "list"]), json!([]));
}

#[test]
fn msg_send_to_an_unknown_agent_is_a_tool_error() {
    let mut fx = Fx::new();
    let message = fx.tool_error(
        "msg_send",
        json!({"to_agent_id": "ghost", "body": "b", "from": "cli"}),
    );
    assert!(message.contains("ghost"), "{message}");
}

#[test]
fn msg_send_reply_to_threads_the_reply_to_the_original() {
    let mut fx = Fx::new();
    let to = fx.agent("rx");
    let first = fx.ok(
        "msg_send",
        json!({"to_agent_id": to, "body": "q", "from": "cli"}),
    );
    let first_id = first[0]["id"].as_str().expect("id").to_owned();
    let reply = fx.ok(
        "msg_send",
        json!({"to_agent_id": to, "body": "a", "from": "cli", "reply_to": first_id}),
    );
    assert_eq!(reply[0]["reply_to"], json!(first_id));
}

#[test]
fn msg_broadcast_reaches_every_enabled_agent_except_the_sender() {
    let mut fx = Fx::new();
    let sender = fx.agent("sender");
    let a = fx.agent("a");
    let b = fx.agent("b");
    let off = fx.agent("off");
    fx.cli_plain(&["agent", "disable", &off]);
    let sent = fx.ok(
        "msg_broadcast",
        json!({"body": "all hands", "from": sender}),
    );
    let mut recipients: Vec<String> = sent
        .as_array()
        .expect("Message[]")
        .iter()
        .map(|m| m["to_agent_id"].as_str().expect("to").to_owned())
        .collect();
    recipients.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(recipients, want);
}

#[test]
fn msg_broadcast_to_a_team_reaches_only_its_members() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let member = fx.agent("member");
    let _outsider = fx.agent("outsider");
    let team = fx.cli(&[
        "team",
        "create",
        "crew",
        "--lead",
        &lead,
        "--members",
        &member,
    ]);
    let team_id = team["id"].as_str().expect("team id").to_owned();
    let sent = fx.ok(
        "msg_broadcast",
        json!({"body": "x", "from": "cli", "team": team_id}),
    );
    let mut to: Vec<&str> = sent
        .as_array()
        .expect("Message[]")
        .iter()
        .map(|m| m["to_agent_id"].as_str().expect("to"))
        .collect();
    to.sort();
    let mut want = vec![lead.as_str(), member.as_str()];
    want.sort();
    assert_eq!(to, want);
}

#[test]
fn msg_broadcast_with_nobody_to_reach_succeeds_with_an_empty_list() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.ok("msg_broadcast", json!({"body": "x", "from": "cli"})),
        json!([])
    );
}

#[test]
fn msg_broadcast_without_from_and_without_identity_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("msg_broadcast", json!({"body": "x"})), -32602);
}

#[test]
fn msg_inbox_lists_pending_messages_without_consuming_them() {
    let mut fx = Fx::new();
    let to = fx.agent("rx");
    fx.cli(&["msg", "send", &to, "one", "--from", "cli"]);
    let first = fx.ok("msg_inbox", json!({"agent_id": to}));
    assert_eq!(first.as_array().expect("array").len(), 1);
    assert_eq!(
        fx.ok("msg_inbox", json!({"agent_id": to})),
        first,
        "read twice"
    );
    assert_eq!(first, fx.cli(&["msg", "inbox", &to]));
}

#[test]
fn msg_inbox_defaults_to_the_server_identity() {
    let (dir, me, _rx) = project_with_sender_and_receiver();
    let mut fx = Fx::on(dir, &[], &[("ORCH_AGENT_ID", Some(&me))]);
    fx.cli(&["msg", "send", &me, "mine", "--from", "cli"]);
    let inbox = fx.ok("msg_inbox", json!({}));
    assert_eq!(inbox[0]["body"], "mine");
}

#[test]
fn msg_inbox_without_an_agent_and_without_identity_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("msg_inbox", json!({})), -32602);
}

#[test]
fn msg_list_equals_the_cli_and_filters_by_agent() {
    let mut fx = Fx::new();
    let a = fx.agent("a");
    let b = fx.agent("b");
    fx.cli(&["msg", "send", &a, "to a", "--from", "cli"]);
    fx.cli(&["msg", "send", &b, "to b", "--from", "cli"]);
    assert_eq!(fx.ok("msg_list", json!({})), fx.cli(&["msg", "list"]));
    let only_a = fx.ok("msg_list", json!({"agent": a}));
    assert_eq!(only_a.as_array().expect("array").len(), 1);
    assert_eq!(only_a[0]["body"], "to a");
}

#[test]
fn msg_list_does_not_default_its_filter_to_the_server_identity() {
    let mut fx = Fx::with(&["--agent", "someone"], &[]);
    let a = fx.agent("a");
    fx.cli(&["msg", "send", &a, "x", "--from", "cli"]);
    assert_eq!(
        fx.ok("msg_list", json!({}))
            .as_array()
            .expect("array")
            .len(),
        1
    );
}

// ── context ───────────────────────────────────────────────────────────────

/// Make `key` expired on disk without waiting for the real clock.
fn expire_context(root: &Path, key: &str) {
    let path = root.join(format!(".orchestry/context/{key}.json"));
    let mut entry: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read entry")).expect("json");
    entry["expires_at"] = json!("2000-01-01T00:00:00.000Z");
    std::fs::write(&path, serde_json::to_string(&entry).expect("json")).expect("write entry");
    let _ = std::fs::remove_file(root.join(".orchestry/context/_index.json"));
}

#[test]
fn context_set_returns_the_stored_entry_equal_to_the_cli_get() {
    let mut fx = Fx::new();
    let set = fx.ok("context_set", json!({"key": "k", "value": "v"}));
    assert_eq!(set["key"], "k");
    assert_eq!(set["value"], "v");
    assert_eq!(set, fx.cli(&["context", "get", "k"]));
}

#[test]
fn context_set_with_a_ttl_records_the_expiry() {
    let mut fx = Fx::new();
    let set = fx.ok(
        "context_set",
        json!({"key": "k", "value": "v", "ttl": 60000}),
    );
    assert_eq!(set["ttl_ms"], 60000);
    assert!(set["expires_at"].is_string(), "{set}");
}

#[test]
fn context_set_with_ttl_zero_or_beyond_thirty_days_is_refused_and_stores_nothing() {
    let mut fx = Fx::new();
    for ttl in [0_u64, 30 * 24 * 60 * 60 * 1000 + 1] {
        let response = fx.call("context_set", json!({"key": "k", "value": "v", "ttl": ttl}));
        // Design D13 files a TTL range under core tool errors; the tool refuses
        // it as invalid params instead (same message). Either way it is refused
        // before storing; the discrepancy is recorded in the T2 report.
        let refused =
            response.get("error").is_some() || response["result"]["isError"] == json!(true);
        assert!(refused, "ttl {ttl} must be refused: {response}");
        assert!(response.to_string().contains("TTL"), "{response}");
    }
    assert_eq!(fx.ok("context_get", json!({"key": "k"})), Value::Null);
}

#[test]
fn context_get_returns_the_entry_and_null_for_a_missing_key() {
    let mut fx = Fx::new();
    fx.cli(&["context", "set", "k", "v"]);
    assert_eq!(
        fx.ok("context_get", json!({"key": "k"})),
        fx.cli(&["context", "get", "k"])
    );
    assert_eq!(fx.ok("context_get", json!({"key": "missing"})), Value::Null);
}

#[test]
fn context_get_of_an_expired_key_is_null_and_leaves_the_entry_on_disk() {
    let mut fx = Fx::new();
    fx.cli(&["context", "set", "k", "v", "--ttl", "60000"]);
    expire_context(fx.root(), "k");
    assert_eq!(fx.ok("context_get", json!({"key": "k"})), Value::Null);
    // Daemon goal row 5c: client reads filter expiry without deleting files.
    assert!(fx.root().join(".orchestry/context/k.json").exists());
}

#[test]
fn context_get_without_a_key_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("context_get", json!({})), -32602);
}

#[test]
fn context_list_equals_the_cli_list_and_omits_expired_entries() {
    let mut fx = Fx::new();
    fx.cli(&["context", "set", "live", "1"]);
    fx.cli(&["context", "set", "old", "2", "--ttl", "60000"]);
    expire_context(fx.root(), "old");
    let listed = fx.ok("context_list", json!({}));
    assert_eq!(
        listed
            .as_array()
            .expect("array")
            .iter()
            .map(|e| e["key"].clone())
            .collect::<Vec<_>>(),
        vec![json!("live")]
    );
    assert_eq!(listed, fx.cli(&["context", "list"]));
}

#[test]
fn context_list_rejects_an_argument_it_does_not_declare() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("context_list", json!({"key": "k"})), -32602);
}

#[test]
fn context_delete_returns_null_and_deleting_twice_is_still_null() {
    let mut fx = Fx::destructive();
    fx.cli(&["context", "set", "k", "v"]);
    assert_eq!(fx.ok("context_delete", json!({"key": "k"})), Value::Null);
    assert_eq!(fx.ok("context_delete", json!({"key": "k"})), Value::Null);
    assert_eq!(fx.cli(&["context", "list"]), json!([]));
}

// ── agent ─────────────────────────────────────────────────────────────────

#[test]
fn agent_list_equals_the_cli_list() {
    let mut fx = Fx::new();
    fx.agent("one");
    fx.agent("two");
    assert_eq!(fx.ok("agent_list", json!({})), fx.cli(&["agent", "list"]));
}

#[test]
fn agent_show_equals_the_cli_status_and_effective_adds_the_prompt_mode() {
    let mut fx = Fx::new();
    let id = fx.agent("one");
    assert_eq!(
        fx.ok("agent_show", json!({"id": id})),
        fx.cli(&["agent", "status", &id])
    );
    let effective = fx.ok("agent_show", json!({"id": id, "effective": true}));
    assert_eq!(effective, fx.cli(&["agent", "status", &id, "--effective"]));
    assert_eq!(effective["effective_config"]["prompt_mode"], "normal");
}

#[test]
fn agent_show_of_a_missing_agent_is_a_tool_error() {
    let mut fx = Fx::new();
    let message = fx.tool_error("agent_show", json!({"id": "agt_nope"}));
    assert!(message.contains("agt_nope"), "{message}");
}

#[test]
fn agent_add_creates_an_agent_with_the_same_shape_as_the_cli() {
    let mut fx = Fx::new();
    let via_cli = fx.cli(&[
        "agent",
        "add",
        "a",
        "--adapter",
        "shell",
        "--command",
        "true",
        "--role",
        "r",
        "--max-turns",
        "9",
        "--skills",
        "x, y",
    ]);
    let via_mcp = fx.ok(
        "agent_add",
        json!({"name": "b", "adapter": "shell", "command": "true", "role": "r", "max_turns": 9, "skills": "x, y"}),
    );
    let mut cli = without_volatile(via_cli);
    cli["name"] = json!("b");
    assert_eq!(without_volatile(via_mcp), cli);
}

#[test]
fn agent_add_of_a_shell_agent_without_a_command_is_a_tool_error_and_stores_nothing() {
    let mut fx = Fx::new();
    let message = fx.tool_error("agent_add", json!({"name": "a", "adapter": "shell"}));
    assert!(message.to_lowercase().contains("command"), "{message}");
    assert_eq!(fx.cli(&["agent", "list"]), json!([]));
}

#[test]
fn agent_add_with_an_unknown_effort_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.rpc_code(
            "agent_add",
            json!({"name": "a", "adapter": "shell", "command": "true", "effort": "warp"})
        ),
        -32602
    );
}

#[test]
fn agent_add_with_an_effort_a_shell_agent_ignores_succeeds_like_the_cli_does() {
    let mut fx = Fx::new();
    let created = fx.ok(
        "agent_add",
        json!({"name": "a", "adapter": "shell", "command": "true", "effort": "high"}),
    );
    assert_eq!(created["name"], "a");
}

#[test]
fn agent_add_with_a_role_summary_over_192_bytes_is_a_tool_error_and_stores_nothing() {
    let mut fx = Fx::new();
    // 97 two-byte characters: 194 bytes, fewer than 192 characters.
    let summary = "é".repeat(97);
    let message = fx.tool_error(
        "agent_add",
        json!({"name": "a", "adapter": "shell", "command": "true", "role_summary": summary}),
    );
    assert!(message.contains("192"), "{message}");
    assert_eq!(fx.cli(&["agent", "list"]), json!([]));
}

#[test]
fn agent_add_with_an_invalid_prompt_mode_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.rpc_code(
            "agent_add",
            json!({"name": "a", "adapter": "shell", "command": "true", "prompt_mode": "loud"})
        ),
        -32602
    );
    assert_eq!(fx.cli(&["agent", "list"]), json!([]));
}

#[test]
fn agent_update_flags_set_and_clear_the_prompt_mode_and_role_summary() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let eco = fx.ok(
        "agent_update",
        json!({"id": id, "prompt_mode": "eco", "role_summary": "short"}),
    );
    assert_eq!(eco["config"]["prompt_mode"], "eco");
    assert_eq!(eco["config"]["role_summary"], "short");
    let cleared = fx.ok("agent_update", json!({"id": id, "prompt_mode": "inherit"}));
    assert!(cleared["config"].get("prompt_mode").is_none(), "{cleared}");
    assert_eq!(
        cleared["config"]["role_summary"], "short",
        "untouched field"
    );
}

#[test]
fn agent_update_with_a_file_replaces_the_authored_definition() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let mut definition = fx.cli(&["agent", "status", &id]);
    definition["role"] = json!("rewritten");
    let file = fx.doc("agent.json", &definition.to_string());
    let updated = fx.ok("agent_update", json!({"id": id, "file": file}));
    assert_eq!(updated["role"], "rewritten");
}

#[test]
fn agent_update_needs_exactly_one_form() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let file = fx.doc("a.json", "{}");
    for arguments in [
        json!({"id": id}),
        json!({"id": id, "file": file, "prompt_mode": "eco"}),
        json!({"id": id, "input_format": "json", "prompt_mode": "eco"}),
    ] {
        assert_eq!(
            fx.rpc_code("agent_update", arguments.clone()),
            -32602,
            "{arguments}"
        );
    }
    assert_eq!(
        fx.rpc_code("agent_update", json!({"id": id, "file": "-"})),
        -32602
    );
}

#[test]
fn agent_patch_merges_a_config_field_and_keeps_the_rest() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let file = fx.doc("p.json", r#"{"config": {"max_turns": 7}}"#);
    let patched = fx.ok("agent_patch", json!({"id": id, "file": file}));
    assert_eq!(patched["config"]["max_turns"], 7);
    assert_eq!(patched["config"]["command"], "true");
}

#[test]
fn agent_patch_that_edits_a_read_only_field_is_a_tool_error_and_changes_nothing() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let before = fx.cli(&["agent", "status", &id]);
    let file = fx.doc("p.json", r#"{"id": "agt_other"}"#);
    fx.tool_error("agent_patch", json!({"id": id, "file": file}));
    assert_eq!(fx.cli(&["agent", "status", &id]), before);
}

#[test]
fn agent_disable_then_enable_round_trips_the_reread_agent() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let disabled = fx.ok("agent_disable", json!({"id": id}));
    assert_eq!(disabled["status"], "disabled");
    assert_eq!(disabled, fx.cli(&["agent", "status", &id]));
    let enabled = fx.ok("agent_enable", json!({"id": id}));
    assert_ne!(enabled["status"], "disabled");
}

#[test]
fn agent_enable_and_disable_of_a_missing_agent_are_tool_errors() {
    let mut fx = Fx::new();
    for tool in ["agent_enable", "agent_disable"] {
        fx.tool_error(tool, json!({"id": "agt_nope"}));
    }
}

#[test]
fn agent_autonomous_toggles_sets_and_prefers_on_when_both_are_given() {
    let mut fx = Fx::new();
    let id = fx.agent("a");
    let flag = |agent: &Value| agent["autonomous"].as_bool().unwrap_or(false);
    assert!(
        flag(&fx.ok("agent_autonomous", json!({"id": id}))),
        "neither toggles on"
    );
    assert!(
        !flag(&fx.ok("agent_autonomous", json!({"id": id}))),
        "neither toggles off"
    );
    assert!(flag(
        &fx.ok("agent_autonomous", json!({"id": id, "on": true}))
    ));
    assert!(!flag(
        &fx.ok("agent_autonomous", json!({"id": id, "off": true}))
    ));
    assert!(
        flag(&fx.ok(
            "agent_autonomous",
            json!({"id": id, "on": true, "off": true})
        )),
        "on wins"
    );
}

#[test]
fn agent_autonomous_of_a_missing_agent_is_a_tool_error() {
    let mut fx = Fx::new();
    fx.tool_error("agent_autonomous", json!({"id": "agt_nope"}));
}

#[test]
fn agent_remove_returns_null_and_the_agent_is_gone() {
    let mut fx = Fx::destructive();
    let id = fx.agent("a");
    assert_eq!(fx.ok("agent_remove", json!({"id": id})), Value::Null);
    assert_eq!(fx.cli(&["agent", "list"]), json!([]));
}

#[test]
fn agent_remove_of_a_missing_agent_is_a_tool_error() {
    let mut fx = Fx::destructive();
    fx.tool_error("agent_remove", json!({"id": "agt_nope"}));
}

// ── team ──────────────────────────────────────────────────────────────────

/// The agent ids of a team's `members`.
fn members_of(team: &Value) -> Vec<String> {
    team["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|m| m["agent_id"].as_str().expect("agent_id").to_owned())
        .collect()
}

#[test]
fn team_create_makes_a_team_with_the_same_shape_as_the_cli() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let member = fx.agent("member");
    let via_cli = fx.cli(&[
        "team",
        "create",
        "t1",
        "--lead",
        &lead,
        "--members",
        &member,
        "-d",
        "dd",
    ]);
    let via_mcp = fx.ok(
        "team_create",
        json!({"name": "t2", "lead": lead, "members": member, "description": "dd"}),
    );
    let mut cli = without_volatile(via_cli);
    cli["name"] = json!("t2");
    assert_eq!(without_volatile(via_mcp), cli);
}

#[test]
fn team_create_no_auto_claim_turns_auto_claim_off() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let on = fx.ok("team_create", json!({"name": "on", "lead": lead}));
    let off = fx.ok(
        "team_create",
        json!({"name": "off", "lead": lead, "no_auto_claim": true}),
    );
    assert_eq!(on["config"]["auto_claim"], true);
    assert_eq!(off["config"]["auto_claim"], false);
}

#[test]
fn team_create_with_an_unknown_lead_is_a_tool_error_and_stores_nothing() {
    let mut fx = Fx::new();
    let message = fx.tool_error("team_create", json!({"name": "t", "lead": "ghost"}));
    assert!(message.contains("ghost"), "{message}");
    assert_eq!(fx.cli(&["team", "list"]), json!([]));
}

#[test]
fn team_list_and_show_equal_the_cli() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let team = fx.cli(&["team", "create", "t", "--lead", &lead]);
    let id = team["id"].as_str().expect("id").to_owned();
    assert_eq!(fx.ok("team_list", json!({})), fx.cli(&["team", "list"]));
    assert_eq!(
        fx.ok("team_show", json!({"id": id})),
        fx.cli(&["team", "show", &id])
    );
}

#[test]
fn team_show_of_a_missing_team_is_a_tool_error() {
    let mut fx = Fx::new();
    fx.tool_error("team_show", json!({"id": "team_nope"}));
}

#[test]
fn team_join_and_leave_return_the_reread_team() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let member = fx.agent("member");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let joined = fx.ok("team_join", json!({"team_id": id, "agent_id": member}));
    assert!(members_of(&joined).contains(&member), "{joined}");
    assert_eq!(joined, fx.cli(&["team", "show", &id]));
    let left = fx.ok("team_leave", json!({"team_id": id, "agent_id": member}));
    assert!(!members_of(&left).contains(&member), "{left}");
}

#[test]
fn team_join_twice_is_handled_by_the_service_not_duplicated() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let member = fx.agent("member");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    fx.ok("team_join", json!({"team_id": id, "agent_id": member}));
    let second = fx.call("team_join", json!({"team_id": id, "agent_id": member}));
    assert!(second.get("error").is_none(), "{second}");
    let team = fx.cli(&["team", "show", &id]);
    let count = members_of(&team).iter().filter(|m| **m == member).count();
    assert_eq!(count, 1, "{team}");
}

#[test]
fn team_join_with_an_unknown_agent_is_a_tool_error() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    fx.tool_error("team_join", json!({"team_id": id, "agent_id": "ghost"}));
}

#[test]
fn team_add_task_returns_the_reread_team_with_the_task_in_its_pool() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let task = fx.task("pooled");
    let team = fx.ok("team_add_task", json!({"team_id": id, "task_id": task}));
    assert_eq!(team, fx.cli(&["team", "show", &id]));
    assert!(team.to_string().contains(&task), "{team}");
}

#[test]
fn team_add_task_with_a_missing_task_is_a_tool_error() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    fx.tool_error(
        "team_add_task",
        json!({"team_id": id, "task_id": "tsk_nope"}),
    );
}

#[test]
fn team_set_lead_transfers_leadership_to_a_member() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let member = fx.agent("member");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead, "--members", &member])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let team = fx.ok("team_set_lead", json!({"team_id": id, "agent_id": member}));
    assert_eq!(team["lead_agent_id"], json!(member));
    assert_eq!(team, fx.cli(&["team", "show", &id]));
}

#[test]
fn team_set_lead_to_a_non_member_is_a_tool_error_and_keeps_the_lead() {
    let mut fx = Fx::new();
    let lead = fx.agent("lead");
    let outsider = fx.agent("outsider");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    fx.tool_error(
        "team_set_lead",
        json!({"team_id": id, "agent_id": outsider}),
    );
    assert_eq!(fx.cli(&["team", "show", &id])["lead_agent_id"], json!(lead));
}

#[test]
fn team_disband_returns_null_and_the_team_is_gone() {
    let mut fx = Fx::destructive();
    let lead = fx.agent("lead");
    let id = fx.cli(&["team", "create", "t", "--lead", &lead])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_eq!(fx.ok("team_disband", json!({"id": id})), Value::Null);
    assert_eq!(fx.cli(&["team", "show", &id])["status"], "disbanded");
}

#[test]
fn team_disband_of_a_missing_team_is_a_tool_error() {
    let mut fx = Fx::destructive();
    fx.tool_error("team_disband", json!({"id": "team_nope"}));
}

// ── status, doctor ────────────────────────────────────────────────────────

#[test]
fn status_equals_the_cli_status_json() {
    let mut fx = Fx::new();
    fx.task("t");
    fx.agent("a");
    assert_eq!(fx.ok("status", json!({})), fx.cli(&["status"]));
}

#[test]
fn status_rejects_an_argument_it_does_not_declare() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("status", json!({"verbose": true})), -32602);
}

/// The JSON object of `orch doctor --json`, which prints a header first.
fn cli_doctor_json(cwd: &Path) -> Value {
    let output = output_of(orch_command(cwd).args(["--json", "doctor"]));
    let text = String::from_utf8_lossy(&output.stdout);
    let start = text.find("{\n").expect("a JSON object in doctor output");
    serde_json::from_str(&text[start..]).expect("doctor JSON")
}

#[test]
fn doctor_returns_the_cli_report_without_its_header() {
    let mut fx = Fx::new();
    let report = fx.ok("doctor", json!({}));
    assert_eq!(report, cli_doctor_json(fx.root()));
    assert_eq!(report["adaptersTotal"], 8);
    assert_eq!(
        report["adaptersReady"], 1,
        "only `shell` is ready with no agent CLI on PATH"
    );
}

#[test]
fn doctor_reports_failed_adapter_checks_as_data_not_as_a_tool_error() {
    let mut fx = Fx::new();
    let response = fx.call("doctor", json!({}));
    assert_eq!(response["result"]["isError"], json!(false), "{response}");
    let report: Value = serde_json::from_str(tool_text(&response)).expect("JSON");
    let failed = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|c| c["status"] == "fail")
        .count();
    assert!(failed > 0, "{report}");
}

#[test]
fn doctor_rejects_an_argument_it_does_not_declare() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("doctor", json!({"adapter": "claude"})), -32602);
}

// ── logs ──────────────────────────────────────────────────────────────────

/// One stored run, with `events` lines in its event log.
fn seed_run(
    root: &Path,
    id: &str,
    task: &str,
    agent: &str,
    started: &str,
    finished: Option<&str>,
    events: &[&str],
) {
    let mut run = json!({
        "id": id, "task_id": task, "agent_id": agent, "attempt": 1,
        "status": "succeeded", "started_at": started,
        "workspace_path": "/w", "prompt": "p",
    });
    if let Some(finished) = finished {
        run["finished_at"] = json!(finished);
    }
    let runs = root.join(".orchestry/runs");
    std::fs::create_dir_all(&runs).expect("runs dir");
    std::fs::write(runs.join(format!("{id}.json")), run.to_string()).expect("write run");
    let mut log = String::new();
    for (n, at) in events.iter().enumerate() {
        log.push_str(
            &json!({"timestamp": at, "type": "agent_output", "data": format!("line {n}")})
                .to_string(),
        );
        log.push('\n');
    }
    std::fs::write(runs.join(format!("{id}.jsonl")), log).expect("write events");
}

const OLD: &str = "2020-01-01T00:00:00.000Z";
const FUTURE: &str = "2099-01-01T00:00:00.000Z";

#[test]
fn logs_with_a_run_id_returns_its_last_fifty_events_like_the_cli() {
    let mut fx = Fx::new();
    let stamps: Vec<&str> = (0..60).map(|_| OLD).collect();
    seed_run(
        fx.root(),
        "run_a",
        "tsk_1",
        "agt_1",
        OLD,
        Some(OLD),
        &stamps,
    );
    let events = fx.ok("logs", json!({"run_id": "run_a"}));
    assert_eq!(events.as_array().expect("events").len(), 50);
    assert_eq!(events[0]["data"], "line 10");
    assert_eq!(events, fx.cli(&["logs", "run_a"]));
}

#[test]
fn logs_with_a_task_returns_every_run_of_that_task_newest_first() {
    let mut fx = Fx::new();
    seed_run(fx.root(), "run_old", "tsk_1", "agt_1", OLD, Some(OLD), &[]);
    seed_run(fx.root(), "run_new", "tsk_1", "agt_1", FUTURE, None, &[]);
    seed_run(fx.root(), "run_other", "tsk_2", "agt_1", FUTURE, None, &[]);
    let runs = fx.ok("logs", json!({"task": "tsk_1"}));
    assert_eq!(ids(&runs), vec!["run_new", "run_old"]);
    assert_eq!(runs, fx.cli(&["logs", "--task", "tsk_1"]));
}

#[test]
fn logs_with_an_agent_returns_only_its_last_five_runs() {
    let mut fx = Fx::new();
    for n in 0..7 {
        let started = format!("2020-01-0{}T00:00:00.000Z", n + 1);
        seed_run(
            fx.root(),
            &format!("run_{n}"),
            "tsk_1",
            "agt_1",
            &started,
            Some(&started),
            &[],
        );
    }
    let runs = fx.ok("logs", json!({"agent": "agt_1"}));
    assert_eq!(runs.as_array().expect("runs").len(), 5);
    assert_eq!(runs, fx.cli(&["logs", "--agent", "agt_1"]));
}

#[test]
fn logs_with_only_since_returns_the_runs_overlapping_the_window() {
    let mut fx = Fx::new();
    seed_run(fx.root(), "run_old", "tsk_1", "agt_1", OLD, Some(OLD), &[]);
    seed_run(fx.root(), "run_live", "tsk_1", "agt_1", OLD, None, &[]);
    seed_run(fx.root(), "run_future", "tsk_1", "agt_1", FUTURE, None, &[]);
    let runs = fx.ok("logs", json!({"since": "1h"}));
    let mut got = ids(&runs);
    got.sort();
    assert_eq!(
        got,
        vec!["run_future", "run_live"],
        "an unfinished run overlaps"
    );
    assert_eq!(runs, fx.cli(&["logs", "--since", "1h"]));
}

#[test]
fn logs_since_with_a_run_id_drops_events_older_than_the_window() {
    let mut fx = Fx::new();
    seed_run(
        fx.root(),
        "run_a",
        "tsk_1",
        "agt_1",
        OLD,
        None,
        &[OLD, FUTURE],
    );
    let events = fx.ok("logs", json!({"run_id": "run_a", "since": "1h"}));
    assert_eq!(events.as_array().expect("events").len(), 1);
    assert_eq!(events[0]["timestamp"], FUTURE);
}

#[test]
fn logs_selector_precedence_is_run_id_then_task_then_agent() {
    let mut fx = Fx::new();
    seed_run(fx.root(), "run_a", "tsk_1", "agt_1", OLD, Some(OLD), &[OLD]);
    let by_run = fx.ok(
        "logs",
        json!({"run_id": "run_a", "task": "tsk_1", "agent": "agt_1"}),
    );
    assert_eq!(
        by_run[0]["type"], "agent_output",
        "run_id wins: events, not runs"
    );
    let by_task = fx.ok("logs", json!({"task": "tsk_1", "agent": "agt_none"}));
    assert_eq!(ids(&by_task), vec!["run_a"], "task wins over agent");
}

#[test]
fn logs_without_any_selector_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("logs", json!({})), -32602);
}

#[test]
fn logs_with_a_non_ascii_or_overflowing_duration_is_invalid_params_and_the_server_survives() {
    let mut fx = Fx::new();
    for since in [
        "é",
        "1é",
        "99999999999999999999d",
        "9223372036854775807d",
        "-1s",
        "1w",
        "s",
        "",
    ] {
        assert_eq!(
            fx.rpc_code("logs", json!({"agent": "agt_1", "since": since})),
            -32602,
            "since {since:?}"
        );
    }
    assert!(
        fx.ok("status", json!({})).is_object(),
        "server still serves"
    );
}

#[test]
fn an_invalid_since_diagnostic_omits_the_supplied_value() {
    let mut fx = Fx::new();
    for since in [
        "SECRET_SINCE_918273",
        "1é",
        "99999999999999999999d",
        "9223372036854775807d",
        "-1s",
        "1w",
    ] {
        let response = fx.rpc_error("logs", json!({"since": since}));
        assert_eq!(error_code(&response), Some(-32602), "{response}");
        let message = response["error"]["message"].as_str().unwrap();
        assert!(message.contains("since"), "{message}");
        assert!(message.contains("ASCII digits"), "{message}");
        assert!(!message.contains(since), "{message}");
    }
}

#[test]
fn logs_since_is_validated_even_when_another_selector_takes_precedence() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.rpc_code("logs", json!({"run_id": "run_a", "since": "é"})),
        -32602
    );
}

#[test]
fn logs_has_no_follow_argument() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.rpc_code("logs", json!({"agent": "a", "follow": true})),
        -32602
    );
}

#[test]
fn logs_of_an_unknown_run_is_an_empty_event_list() {
    let mut fx = Fx::new();
    assert_eq!(fx.ok("logs", json!({"run_id": "run_missing"})), json!([]));
}

// ── config ────────────────────────────────────────────────────────────────

#[test]
fn config_get_returns_the_value_like_the_cli_and_omits_value_for_a_missing_key() {
    let mut fx = Fx::new();
    let found = fx.ok("config_get", json!({"key": "defaults.task.priority"}));
    assert_eq!(found, json!({"key": "defaults.task.priority", "value": 3}));
    assert_eq!(found, fx.cli(&["config", "get", "defaults.task.priority"]));
    let missing = fx.ok("config_get", json!({"key": "no.such.key"}));
    assert_eq!(missing, json!({"key": "no.such.key"}));
    assert_eq!(missing, fx.cli(&["config", "get", "no.such.key"]));
}

#[test]
fn config_get_without_a_key_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("config_get", json!({})), -32602);
}

#[test]
fn config_set_parses_numbers_and_booleans_and_stores_anything_else_as_a_string() {
    let mut fx = Fx::new();
    let number = fx.ok(
        "config_set",
        json!({"key": "defaults.task.max_attempts", "value": "5000"}),
    );
    assert_eq!(
        number,
        json!({"key": "defaults.task.max_attempts", "value": 5000})
    );
    let boolean = fx.ok("config_set", json!({"key": "custom.flag", "value": "true"}));
    assert_eq!(boolean["value"], json!(true));
    let text = fx.ok(
        "config_set",
        json!({"key": "custom.name", "value": "hello"}),
    );
    assert_eq!(text["value"], json!("hello"));
    assert_eq!(fx.cli(&["config", "get", "custom.name"])["value"], "hello");
}

#[test]
fn config_set_of_an_incompatible_prompt_mode_is_a_tool_error_and_keeps_the_config() {
    let mut fx = Fx::new();
    let before = std::fs::read(fx.root().join(".orchestry/config.yml")).expect("config");
    fx.tool_error(
        "config_set",
        json!({"key": "defaults.agent.prompt_mode", "value": "loud"}),
    );
    assert_eq!(
        std::fs::read(fx.root().join(".orchestry/config.yml")).expect("config"),
        before
    );
}

#[test]
fn config_set_without_a_value_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("config_set", json!({"key": "a.b"})), -32602);
}

#[test]
fn config_update_replaces_the_config_and_returns_the_raw_object() {
    let mut fx = Fx::new();
    let mut config: Value = serde_yaml_ng::from_str(
        &std::fs::read_to_string(fx.root().join(".orchestry/config.yml")).expect("config"),
    )
    .expect("yaml");
    config["project"]["name"] = json!("renamed");
    let file = fx.doc("cfg.json", &config.to_string());
    let updated = fx.ok("config_update", json!({"file": file}));
    assert_eq!(updated["project"]["name"], "renamed");
    assert_eq!(
        fx.cli(&["config", "get", "project.name"])["value"],
        "renamed"
    );
}

#[test]
fn config_update_with_malformed_yaml_is_a_tool_error_and_keeps_the_config() {
    let mut fx = Fx::new();
    let before = std::fs::read(fx.root().join(".orchestry/config.yml")).expect("config");
    let file = fx.doc("bad.yml", "project: [unclosed\n");
    fx.tool_error("config_update", json!({"file": file}));
    assert_eq!(
        std::fs::read(fx.root().join(".orchestry/config.yml")).expect("config"),
        before
    );
}

#[test]
fn config_patch_merges_only_the_given_keys() {
    let mut fx = Fx::new();
    let file = fx.doc("patch.json", r#"{"project": {"name": "patched"}}"#);
    let patched = fx.ok("config_patch", json!({"file": file}));
    assert_eq!(patched["project"]["name"], "patched");
    assert_eq!(
        patched["scheduling"]["max_concurrent_agents"], 6,
        "other keys kept"
    );
}

#[test]
fn config_patch_with_a_duplicate_key_document_is_a_tool_error_and_keeps_the_config() {
    let mut fx = Fx::new();
    let before = std::fs::read(fx.root().join(".orchestry/config.yml")).expect("config");
    let file = fx.doc("dup.json", r#"{"project": {"name": "a", "name": "b"}}"#);
    fx.tool_error("config_patch", json!({"file": file}));
    assert_eq!(
        std::fs::read(fx.root().join(".orchestry/config.yml")).expect("config"),
        before
    );
}

#[test]
fn config_update_and_patch_with_file_dash_are_invalid_params() {
    let mut fx = Fx::new();
    for tool in ["config_update", "config_patch"] {
        assert_eq!(fx.rpc_code(tool, json!({"file": "-"})), -32602);
    }
}

// ── remote ────────────────────────────────────────────────────────────────

#[test]
fn remote_list_and_status_equal_the_cli_for_a_registered_peer() {
    let mut fx = Fx::new();
    fx.cli_plain(&["remote", "add", "peer", "127.0.0.1:9"]);
    let listed = fx.ok("remote_list", json!({}));
    assert_eq!(listed[0]["alias"], "peer");
    assert_eq!(listed, fx.cli(&["remote", "list"]));
    assert_eq!(
        fx.ok("remote_status", json!({})),
        fx.cli(&["remote", "status"])
    );
}

#[test]
fn remote_list_and_status_of_a_project_with_no_peers_are_empty() {
    let mut fx = Fx::new();
    assert_eq!(fx.ok("remote_list", json!({})), json!([]));
    let status = fx.ok("remote_status", json!({}));
    assert_eq!(status["remotes"], json!([]));
    assert_eq!(status["listen"], Value::Null);
}

#[test]
fn remote_list_and_status_read_the_registry_without_connecting_to_a_registered_peer() {
    // The observer is a real listener standing in for the peer. A connection
    // attempt on loopback is queued in its backlog as soon as it is made, so
    // by the time the calls have returned an `accept` sees any dial.
    let observer = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the observer");
    observer
        .set_nonblocking(true)
        .expect("non-blocking observer");
    let address = observer.local_addr().expect("observer address").to_string();
    let mut fx = Fx::new();
    fx.cli_plain(&["remote", "add", "peer", &address]);
    assert_eq!(fx.ok("remote_list", json!({}))[0]["alias"], "peer");
    assert_eq!(
        fx.ok("remote_status", json!({}))["remotes"][0]["alias"],
        "peer"
    );
    let dialled = observer.accept();
    assert!(
        matches!(&dialled, Err(err) if err.kind() == std::io::ErrorKind::WouldBlock),
        "the read-only remote tools dialled the registered peer: {dialled:?}"
    );
    // Control: the observer does see a connection when one is made.
    let _probe = std::net::TcpStream::connect(&address).expect("connect to the observer");
    assert!(
        wait_for_accept(&observer),
        "the observer cannot see connections, so the check above proves nothing"
    );
}

/// Whether `listener` (non-blocking) accepts a connection within the harness
/// deadline; the loopback handshake is complete before `connect` returns, so
/// this normally succeeds on the first poll.
fn wait_for_accept(listener: &std::net::TcpListener) -> bool {
    mcp_common::wait_until(mcp_common::TIMEOUT, || listener.accept().is_ok())
}

#[test]
fn remote_tools_reject_an_argument_they_do_not_declare() {
    let mut fx = Fx::new();
    for tool in ["remote_list", "remote_status"] {
        assert_eq!(fx.rpc_code(tool, json!({"alias": "x"})), -32602);
    }
}

// ── mode gates ────────────────────────────────────────────────────────────

/// Two goals sharing an assignee, each with queued autonomous tasks, plus a
/// goal-less autonomous task and non-autonomous tasks (spec R4/R13: pausing G1
/// cancels G1's own autonomous tasks only).
struct SharedAssignee {
    agent: String,
    g1: String,
    g2: String,
    /// Autonomous, todo, in goal G2.
    autonomous_task: String,
    /// Autonomous, todo, in the paused goal G1 itself.
    autonomous_in_g1: String,
    /// Autonomous, `retrying`, in the paused goal G1 itself.
    retrying_in_g1: String,
    /// Autonomous, `retrying`, in goal G2.
    retrying_autonomous_task: String,
    /// Not autonomous, todo, in goal G2.
    plain_task: String,
    /// Not autonomous, todo, in the paused goal G1 itself.
    plain_in_g1: String,
    /// Autonomous, todo, linked to no goal.
    goalless_autonomous: String,
}

fn shared_assignee(fx: &mut Fx) -> SharedAssignee {
    let agent = fx.agent("lead");
    let g1 = fx.ok("goal_add", json!({"title": "G1", "assignee": agent}));
    let g2 = fx.ok("goal_add", json!({"title": "G2", "assignee": agent}));
    let g1_id = g1["id"].as_str().expect("id").to_owned();
    let g2_id = g2["id"].as_str().expect("id").to_owned();
    let mut add = |title: &str, autonomous: bool, goal: Option<&str>| {
        let mut arguments = json!({"title": title, "assignee": agent});
        if autonomous {
            arguments["labels"] = json!("autonomous");
        }
        if let Some(goal) = goal {
            arguments["goal_id"] = json!(goal);
        }
        fx.ok("task_add", arguments)["id"]
            .as_str()
            .expect("id")
            .to_owned()
    };
    let autonomous_task = add("auto", true, Some(&g2_id));
    let autonomous_in_g1 = add("auto-g1", true, Some(&g1_id));
    let retrying_in_g1 = add("auto-g1-retrying", true, Some(&g1_id));
    let retrying_autonomous_task = add("auto-retrying", true, Some(&g2_id));
    let plain_task = add("plain", false, Some(&g2_id));
    let plain_in_g1 = add("plain-g1", false, Some(&g1_id));
    let goalless_autonomous = add("auto-goalless", true, None);
    fx.force_task(&retrying_in_g1, "retrying", None);
    fx.force_task(&retrying_autonomous_task, "retrying", None);
    SharedAssignee {
        agent,
        g1: g1_id,
        g2: g2_id,
        autonomous_task,
        autonomous_in_g1,
        retrying_in_g1,
        retrying_autonomous_task,
        plain_task,
        plain_in_g1,
        goalless_autonomous,
    }
}

/// After G1 was paused: G1's queued autonomous tasks are cancelled, and
/// everything else of the shared assignee is as it was.
fn assert_only_g1s_autonomous_tasks_were_cancelled(fx: &Fx, s: &SharedAssignee) {
    assert_eq!(fx.cli(&["goal", "show", &s.g1])["status"], "paused");
    assert_eq!(fx.cli(&["goal", "show", &s.g2])["status"], "active");
    for (task, status) in [
        (&s.autonomous_in_g1, "cancelled"),
        (&s.retrying_in_g1, "cancelled"),
        (&s.plain_in_g1, "todo"),
        (&s.autonomous_task, "todo"),
        (&s.retrying_autonomous_task, "retrying"),
        (&s.plain_task, "todo"),
        (&s.goalless_autonomous, "todo"),
    ] {
        assert_eq!(fx.cli(&["task", "show", task])["status"], status, "{task}");
    }
}

#[test]
fn default_mode_pausing_g1_cancels_only_g1s_own_autonomous_tasks() {
    let mut fx = Fx::new();
    let s = shared_assignee(&mut fx);
    let paused = fx.ok("goal_status", json!({"id": s.g1, "status": "paused"}));
    assert_eq!(paused["status"], "paused");
    assert_only_g1s_autonomous_tasks_were_cancelled(&fx, &s);
    let agent = fx.cli(&["agent", "status", &s.agent]);
    assert_eq!(agent["id"], json!(s.agent));
}

#[test]
fn default_mode_refuses_force_even_on_a_status_that_does_not_pause() {
    let mut fx = Fx::new();
    let s = shared_assignee(&mut fx);
    let before = snapshot(&fx.root().join(".orchestry"));
    for arguments in [
        json!({"id": s.g1, "status": "achieved", "force": true}),
        json!({"id": s.g1, "status": "abandoned", "force": true}),
        json!({"id": s.g1, "status": "paused", "force": true}),
    ] {
        let response = fx.rpc_error("goal_status", arguments.clone());
        assert_eq!(error_code(&response), Some(-32602), "{arguments}");
        assert_eq!(
            response["error"]["message"], "force=true requires --allow-destructive",
            "{arguments}"
        );
    }
    assert_eq!(snapshot(&fx.root().join(".orchestry")), before);
    for goal in [&s.g1, &s.g2] {
        assert_eq!(
            fx.cli(&["goal", "show", goal])["status"],
            "active",
            "{goal}"
        );
    }
}

#[test]
fn default_mode_pauses_a_goal_that_has_no_assignee_too() {
    let mut fx = Fx::new();
    let goal = fx.ok("goal_add", json!({"title": "unassigned"}));
    let id = goal["id"].as_str().expect("id").to_owned();
    let paused = fx.ok("goal_status", json!({"id": id, "status": "paused"}));
    assert_eq!(paused["status"], "paused");
    assert_eq!(fx.cli(&["goal", "show", &id])["status"], "paused");
}

#[test]
fn default_mode_still_allows_the_other_goal_transitions_without_force() {
    let mut fx = Fx::new();
    let s = shared_assignee(&mut fx);
    fx.ok("goal_status", json!({"id": s.g1, "status": "abandoned"}));
    assert_eq!(fx.cli(&["goal", "show", &s.g1])["status"], "abandoned");
}

#[test]
fn allow_destructive_pausing_g1_is_scoped_to_g1_as_well() {
    // The mode changes only the gate; the engine's pause is goal-scoped.
    let mut fx = Fx::destructive();
    let s = shared_assignee(&mut fx);
    let paused = fx.ok("goal_status", json!({"id": s.g1, "status": "paused"}));
    assert_eq!(paused["status"], "paused");
    assert_only_g1s_autonomous_tasks_were_cancelled(&fx, &s);
}

#[test]
fn allow_destructive_accepts_force_on_an_achieved_transition() {
    let mut fx = Fx::destructive();
    let goal = fx.ok("goal_add", json!({"title": "G"}));
    let id = goal["id"].as_str().expect("id").to_owned();
    let achieved = fx.ok(
        "goal_status",
        json!({"id": id, "status": "achieved", "force": true}),
    );
    assert_eq!(achieved["status"], "achieved");
}

#[test]
fn read_only_mode_refuses_goal_status_as_an_absent_tool() {
    let mut fx = Fx::with(&["--read-only"], &[]);
    let response = fx.rpc_error("goal_status", json!({"id": "g", "status": "abandoned"}));
    assert_eq!(error_code(&response), Some(-32602));
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("not found"),
        "{response}"
    );
}

#[test]
fn every_destructive_tool_is_not_found_in_default_and_read_only_mode_and_changes_nothing() {
    for args in [&[][..], &["--read-only"][..]] {
        let mut fx = Fx::with(args, &[]);
        let id = fx.task("keep");
        let before = snapshot(&fx.root().join(".orchestry"));
        for tool in DESTRUCTIVE {
            let response = fx.rpc_error(tool, json!({"id": id, "key": "k"}));
            assert_eq!(error_code(&response), Some(-32602), "{tool} {args:?}");
            assert!(
                response["error"]["message"]
                    .as_str()
                    .expect("message")
                    .contains("not found"),
                "{tool}: {response}"
            );
        }
        assert_eq!(snapshot(&fx.root().join(".orchestry")), before, "{args:?}");
    }
}

#[test]
fn every_mutating_tool_is_not_found_in_read_only_mode() {
    let mut fx = Fx::with(&["--read-only"], &[]);
    let read: Vec<&str> = READ_ONLY.to_vec();
    for tool in EXPECTED.iter().filter(|name| !read.contains(name)) {
        let response = fx.rpc_error(tool, json!({}));
        assert_eq!(error_code(&response), Some(-32602), "{tool}");
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("not found"),
            "{tool}: {response}"
        );
    }
}

#[test]
fn read_only_mode_serves_every_read_tool_with_real_entities() {
    // Entities are made through the CLI (a read-only server cannot create
    // them), so every read below finds something and a tool that was
    // registered but broken - or answered with a not-found error - fails.
    let mut fx = Fx::with(&["--read-only"], &[]);
    let agent = fx.agent("reader");
    let task = fx.task("t");
    let goal = fx.cli(&["goal", "add", "G"])["id"]
        .as_str()
        .expect("goal id")
        .to_owned();
    let team = fx.cli(&["team", "create", "crew", "--lead", &agent])["id"]
        .as_str()
        .expect("team id")
        .to_owned();
    fx.cli_plain(&["context", "set", "k", "v"]);
    fx.cli_plain(&["msg", "send", &agent, "hello", "--from", "cli"]);
    seed_run(
        fx.root(),
        "run_a",
        &task,
        &agent,
        OLD,
        Some(OLD),
        &[OLD, OLD],
    );

    let mut covered: Vec<String> = Vec::new();
    let mut read = |tool: &str, arguments: Value| -> Value {
        covered.push(tool.to_owned());
        fx.ok(tool, arguments)
    };
    assert_eq!(read("agent_show", json!({"id": agent}))["id"], json!(agent));
    assert_eq!(read("agent_list", json!({}))[0]["id"], json!(agent));
    assert_eq!(
        read("config_get", json!({"key": "project.name"})),
        json!({"key": "project.name", "value": "p"})
    );
    assert_eq!(read("context_get", json!({"key": "k"}))["value"], "v");
    assert_eq!(read("context_list", json!({}))[0]["key"], "k");
    assert!(read("doctor", json!({})).is_object());
    assert_eq!(read("goal_show", json!({"id": goal}))["id"], json!(goal));
    assert_eq!(read("goal_list", json!({}))[0]["id"], json!(goal));
    assert_eq!(
        read("logs", json!({"run_id": "run_a"}))
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        read("msg_inbox", json!({"agent_id": agent}))[0]["body"],
        "hello"
    );
    assert_eq!(
        read("msg_list", json!({"agent": agent}))[0]["body"],
        "hello"
    );
    assert!(read("remote_list", json!({})).is_array());
    assert!(read("remote_status", json!({})).is_object());
    assert!(read("status", json!({})).is_object());
    assert_eq!(read("task_show", json!({"id": task}))["id"], json!(task));
    assert_eq!(read("task_list", json!({}))[0]["id"], json!(task));
    assert_eq!(read("team_show", json!({"id": team}))["id"], json!(team));
    assert_eq!(read("team_list", json!({}))[0]["id"], json!(team));
    // The loop above is the whole read-only catalogue, no more and no fewer.
    covered.sort();
    let mut expected: Vec<String> = READ_ONLY.iter().map(|tool| (*tool).to_owned()).collect();
    expected.sort();
    assert_eq!(covered, expected);
}

#[test]
fn a_destructive_tool_works_under_allow_destructive_and_not_in_default_mode() {
    let mut denied = Fx::new();
    let id = denied.task("t");
    assert_eq!(denied.rpc_code("task_cancel", json!({"id": id})), -32602);
    assert_eq!(denied.cli(&["task", "show", &id])["status"], "todo");
    let mut allowed = Fx::destructive();
    let id = allowed.task("t");
    allowed.ok("task_cancel", json!({"id": id}));
    assert_eq!(allowed.cli(&["task", "show", &id])["status"], "cancelled");
}

// ── definition-file guards and path resolution ────────────────────────────

#[test]
fn a_file_dash_is_refused_at_once_and_the_next_frame_is_intact() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    assert_eq!(
        fx.rpc_code("task_patch", json!({"id": id, "file": "-"})),
        -32602
    );
    assert!(fx.ok("status", json!({})).is_object());
}

#[test]
fn an_empty_file_name_is_invalid_params() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    assert_eq!(
        fx.rpc_code("task_patch", json!({"id": id, "file": ""})),
        -32602
    );
}

#[test]
fn a_definition_with_duplicate_keys_is_a_tool_error_and_changes_nothing() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = fx.cli(&["task", "show", &id]);
    let file = fx.doc("dup.json", r#"{"title": "a", "title": "b"}"#);
    fx.tool_error("task_patch", json!({"id": id, "file": file}));
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

#[test]
fn a_patch_naming_an_unknown_key_as_null_is_refused_before_anything_is_merged() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = fx.cli(&["task", "show", &id]);
    let file = fx.doc("null.json", r#"{"no_such_field": null}"#);
    fx.tool_error("task_patch", json!({"id": id, "file": file}));
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

#[test]
fn a_patch_that_rewrites_an_immutable_field_is_a_tool_error_and_changes_nothing() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = fx.cli(&["task", "show", &id]);
    let file = fx.doc("imm.json", r#"{"id": "tsk_other", "status": "done"}"#);
    fx.tool_error("task_patch", json!({"id": id, "file": file}));
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

#[test]
fn a_definition_over_one_mebibyte_is_a_tool_error_and_changes_nothing() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = fx.cli(&["task", "show", &id]);
    let big = format!(r#"{{"description": "{}"}}"#, "x".repeat(1024 * 1024));
    let file = fx.doc("big.json", &big);
    let message = fx.tool_error("task_patch", json!({"id": id, "file": file}));
    assert!(message.contains("1 MiB"), "{message}");
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

/// `{"<key>": [[...[1]...]]}` with `levels` nested arrays. The root object is
/// level 0 and the array under `key` level 1, so the scalar sits at depth
/// `levels + 1`; the server allows scalars down to depth 64.
fn nested_document(key: &str, levels: usize) -> String {
    format!(
        r#"{{"{key}": {}1{}}}"#,
        "[".repeat(levels),
        "]".repeat(levels)
    )
}

/// The wording only the depth guard produces (`document_input::parse_json`).
const DEPTH_REFUSAL: &str = "depth <= 64";

#[test]
fn a_definition_nested_deeper_than_sixty_four_levels_is_refused_for_its_depth_and_changes_nothing()
{
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = snapshot(&fx.root().join(".orchestry"));
    // `labels` is a flat list of strings, so this document is invalid for
    // other reasons too: only the depth wording proves the depth guard ran.
    let file = fx.doc("deep.json", &nested_document("labels", 65));
    let message = fx.tool_error("task_patch", json!({"id": id, "file": file}));
    assert!(message.contains(DEPTH_REFUSAL), "{message}");
    assert_eq!(snapshot(&fx.root().join(".orchestry")), before);
}

#[test]
fn a_config_extension_one_level_past_the_depth_limit_is_refused_for_its_depth_and_changes_nothing()
{
    // A document that is valid at every depth the guard allows (config keeps
    // unknown extensions verbatim), so the depth is the only thing wrong.
    let mut fx = Fx::new();
    let before = snapshot(&fx.root().join(".orchestry"));
    let file = fx.doc("too-deep.json", &nested_document("x_ext", 64));
    let message = fx.tool_error("config_patch", json!({"file": file}));
    assert!(message.contains(DEPTH_REFUSAL), "{message}");
    assert_eq!(snapshot(&fx.root().join(".orchestry")), before);
}

#[test]
fn a_config_extension_nested_exactly_to_the_depth_limit_is_accepted_and_stored() {
    // The boundary control for the refusal above: one level shallower passes,
    // so the guard is "deeper than 64", not "nested at all".
    let mut fx = Fx::new();
    let file = fx.doc("at-limit.json", &nested_document("x_ext", 63));
    fx.ok("config_patch", json!({"file": file}));
    let stored =
        std::fs::read_to_string(fx.root().join(".orchestry/config.yml")).expect("read the config");
    assert!(stored.contains("x_ext"), "{stored}");
}

#[test]
fn malformed_yaml_is_a_tool_error_and_changes_nothing() {
    let mut fx = Fx::new();
    let id = fx.task("t");
    let before = fx.cli(&["task", "show", &id]);
    let file = fx.doc("bad.yml", "title: [unclosed\n");
    fx.tool_error("task_update", json!({"id": id, "file": file}));
    assert_eq!(fx.cli(&["task", "show", &id]), before);
}

/// A server whose working directory is `project/sub`, so a relative path means
/// different files depending on what it is resolved against.
fn server_in_subdirectory() -> (ProjectDir, McpServer) {
    let dir = initialized_project();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("subdir");
    let mut server = McpServer::spawn(&sub);
    server.initialize();
    (dir, server)
}

#[test]
fn a_relative_definition_path_resolves_against_the_project_root_not_the_server_cwd() {
    let (dir, mut server) = server_in_subdirectory();
    let id = cli_json(dir.path(), &["task", "add", "t"])["id"]
        .as_str()
        .expect("id")
        .to_owned();
    std::fs::write(dir.path().join("p.json"), r#"{"priority": 1}"#).expect("root doc");
    std::fs::write(dir.path().join("sub/p.json"), r#"{"priority": 4}"#).expect("decoy");
    let response = server.call_tool("task_patch", json!({"id": id, "file": "p.json"}));
    let patched: Value = serde_json::from_str(tool_text(&response)).expect("JSON");
    assert_eq!(
        patched["priority"], 1,
        "project-relative, not cwd-relative: {response}"
    );
}

// Design D12: a relative `attach` path resolves against the fixed project
// root, not the server's working directory.
#[test]
#[ignore = "production bug tsk_ml1V-uw: the MCP bridge sends task_add attachments as plain strings, the wire wants tagged WirePath"]
fn a_relative_attachment_path_resolves_against_the_project_root_not_the_server_cwd() {
    let (dir, mut server) = server_in_subdirectory();
    std::fs::write(dir.path().join("notes.txt"), "from the root").expect("attachment");
    let response = server.call_tool("task_add", json!({"title": "t", "attach": "notes.txt"}));
    assert_eq!(response["result"]["isError"], json!(false), "{response}");
    let task: Value = serde_json::from_str(tool_text(&response)).expect("JSON");
    assert_eq!(task["attachments"], json!(["notes.txt"]), "{task}");
}

// ── remote agents in the agent tools ──────────────────────────────────────

/// A registered peer `peer` with a stored snapshot holding one agent, which is
/// a copy of the local agent `local` renamed. Returns the remote agent's id.
fn project_with_remote_snapshot(fx: &Fx, local: &str) -> String {
    fx.cli_plain(&["remote", "add", "peer", "127.0.0.1:9"]);
    let mut remote_agent = fx.cli(&["agent", "status", local]);
    remote_agent["id"] = json!("agt_remote1");
    remote_agent["name"] = json!("far-away");
    let mut status = fx.cli(&["remote", "status"])["remotes"][0].clone();
    status["agents"] = json!([remote_agent]);
    std::fs::create_dir_all(fx.root().join(".orchestry/remotes")).expect("remotes dir");
    std::fs::write(
        fx.root().join(".orchestry/remotes/peer.json"),
        status.to_string(),
    )
    .expect("write snapshot");
    "agt_remote1".to_owned()
}

#[test]
fn agent_list_includes_stored_remote_snapshots_exactly_as_the_cli_does() {
    let mut fx = Fx::new();
    let local = fx.agent("near");
    project_with_remote_snapshot(&fx, &local);
    let listed = fx.ok("agent_list", json!({}));
    assert_eq!(listed, fx.cli(&["agent", "list"]));
    assert_eq!(listed.as_array().expect("agents").len(), 2, "{listed}");
}

#[test]
fn agent_show_resolves_a_qualified_remote_agent_like_the_cli() {
    let mut fx = Fx::new();
    let local = fx.agent("near");
    let remote = project_with_remote_snapshot(&fx, &local);
    let qualified = format!("{remote}@peer");
    assert_eq!(
        fx.ok("agent_show", json!({"id": qualified})),
        fx.cli(&["agent", "status", &qualified])
    );
}

#[test]
fn a_remote_agent_cannot_be_mutated_through_the_agent_tools() {
    let mut fx = Fx::new();
    let local = fx.agent("near");
    let remote = project_with_remote_snapshot(&fx, &local);
    let qualified = format!("{remote}@peer");
    fx.tool_error("agent_enable", json!({"id": qualified}));
    fx.tool_error(
        "agent_update",
        json!({"id": qualified, "role_summary": "x"}),
    );
}

#[test]
fn remote_status_ignores_a_listener_record_left_by_a_dead_process() {
    let mut fx = Fx::new();
    std::fs::create_dir_all(fx.root().join(".orchestry/remotes")).expect("remotes dir");
    std::fs::write(
        fx.root().join(".orchestry/remotes/_listener.json"),
        r#"{"listen_addr": "127.0.0.1:41234", "controller_name": "ghost", "pid": 1073741823}"#,
    )
    .expect("listener record");
    let status = fx.ok("remote_status", json!({}));
    assert_eq!(status["listen"], Value::Null, "{status}");
    assert_eq!(status["controller"], Value::Null);
    assert_eq!(status, fx.cli(&["remote", "status"]));
}

// ── configuration freshness (a fresh Engine per call) ─────────────────────

#[test]
fn config_set_changes_a_default_that_the_next_task_add_uses() {
    let mut fx = Fx::new();
    fx.ok(
        "config_set",
        json!({"key": "defaults.task.max_attempts", "value": "7"}),
    );
    assert_eq!(fx.ok("task_add", json!({"title": "t"}))["max_attempts"], 7);
}

#[test]
fn an_external_config_edit_between_calls_is_seen_by_the_next_call() {
    let mut fx = Fx::new();
    assert_eq!(
        fx.ok("task_add", json!({"title": "first"}))["max_attempts"],
        3
    );
    fx.cli_plain(&["config", "set", "defaults.task.max_attempts", "9"]);
    assert_eq!(
        fx.ok("task_add", json!({"title": "second"}))["max_attempts"],
        9,
        "the running server must not hold the startup config"
    );
}

#[test]
fn an_external_default_agent_edit_is_seen_by_the_next_agent_add() {
    let mut fx = Fx::new();
    fx.cli_plain(&["config", "set", "defaults.agent.max_turns", "77"]);
    let created = fx.ok(
        "agent_add",
        json!({"name": "a", "adapter": "shell", "command": "true"}),
    );
    assert_eq!(created["config"]["max_turns"], 77);
}

// ── concurrency and recovery evidence (decision-2 E2) ─────────────────────

/// How long a test waits for a helper thread. Longer than the 90 s `orch run`
/// deadline plus its own CLI calls, so a hung worker is reported by its own
/// deadline first and this only catches a thread that never returns.
const WORKER_JOIN_LIMIT: Duration = Duration::from_secs(150);

#[test]
fn two_servers_creating_distinct_tasks_at_once_both_keep_every_index_entry() {
    const PER_SERVER: usize = 8;
    let dir = initialized_project();
    // A cancelable rendezvous: a creator that fails before arriving releases
    // its peer with an error instead of leaving it waiting forever.
    let rendezvous = Rendezvous::new(2);
    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let root = dir.path().to_owned();
            let rendezvous = std::sync::Arc::clone(&rendezvous);
            spawn_bounded("a task-creating server thread", move || {
                let _participant = rendezvous.participant();
                let mut server = McpServer::spawn(&root);
                server.initialize();
                rendezvous
                    .arrive(mcp_common::TIMEOUT)
                    .expect("both servers start creating together");
                (0..PER_SERVER)
                    .map(|n| {
                        let response = server
                            .call_tool("task_add", json!({"title": format!("w{worker}-{n}")}));
                        let task: Value =
                            serde_json::from_str(tool_text(&response)).expect("task JSON");
                        task["id"].as_str().expect("id").to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut created: Vec<String> = workers
        .into_iter()
        .flat_map(|worker| worker.join(WORKER_JOIN_LIMIT))
        .collect();
    created.sort();
    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".orchestry/tasks/_index.json")).expect("index"),
    )
    .expect("index JSON");
    let mut indexed = ids(&index);
    indexed.sort();
    assert_eq!(
        indexed, created,
        "every created task is in the published index"
    );
    assert_eq!(created.len(), 2 * PER_SERVER);
    assert_eq!(
        ids(&cli_json(dir.path(), &["task", "list"])).len(),
        2 * PER_SERVER
    );
}

#[test]
fn a_removed_context_directory_is_recreated_by_the_next_write() {
    // Design D10 residual, closed: the daemon's ensured-directory cache
    // forgets a directory that vanished underneath it, so the next write
    // recreates it instead of failing until the daemon restarts.
    let mut fx = Fx::new();
    fx.ok("context_set", json!({"key": "k", "value": "v"}));
    std::fs::remove_dir_all(fx.root().join(".orchestry/context")).expect("remove context dir");
    fx.ok("context_set", json!({"key": "k2", "value": "v"}));
    let got = fx.ok("context_get", json!({"key": "k2"}));
    assert_eq!(got["value"], json!("v"), "{got}");
}

#[test]
fn a_wrong_typed_known_config_key_is_refused_and_leaves_the_config_and_the_server_working() {
    // The daemon validates `config_set` (it owns the only writer), so the old
    // "accepted, then poisons every later call" state cannot be reached
    // through the tool any more.
    let mut fx = Fx::new();
    let config = fx.root().join(".orchestry/config.yml");
    let before = std::fs::read(&config).expect("config before");
    let response = fx.call(
        "config_set",
        json!({"key": "defaults.task.max_attempts", "value": "not-a-number"}),
    );
    assert_eq!(response["result"]["isError"], json!(true), "{response}");
    assert_eq!(
        std::fs::read(&config).expect("config after"),
        before,
        "a refused config_set must leave config.yml byte-identical"
    );
    for tool in ["task_list", "status", "agent_list"] {
        fx.ok(tool, json!({}));
    }
}

// ── worker matrix (design D19) ────────────────────────────────────────────
//
// A worker is a child the real orchestrator starts through the real shell
// adapter: `orch run <task>` prepares the workspace (shared, worktree or
// isolated), builds `ExecuteParams`/`SpawnOptions`, and spawns the agent's
// command with `ORCH_AGENT_ID` overlaid. The command here is a fake MCP client
// script that records the cwd and environment it was handed and then launches
// `orch mcp` from that cwd, inheriting that environment — exactly what a
// client registered without a project pin does. No AI CLI is ever executed.

/// A project that also works as a git repository, so `worktree` and
/// `isolated` workspaces can be created.
///
/// The project's explicit test daemon is started last, after the git
/// repository exists: workers run through `orch run <id>`, an RPC to that
/// daemon, which prepares the workspace and spawns the worker.
fn git_project(name: &str) -> ProjectDir {
    let mut dir = initialized_project_unowned(name);
    let git = |args: &[&str]| {
        // The fixture's own private HOME and the system directories only: no
        // developer-machine PATH entry, no inherited user git config.
        let output = output_of(
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env_clear()
                .env("HOME", dir.path().join(".test-home"))
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1"),
        );
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);
    dir.start_daemon();
    dir
}

/// What one worker run produced.
struct WorkerRun {
    cwd: PathBuf,
    agent_id_seen: String,
    mcp_exit: String,
    responses: BTreeMap<u64, Value>,
    stderr: String,
}

impl WorkerRun {
    fn instructions(&self) -> &str {
        self.responses[&1]["result"]["instructions"]
            .as_str()
            .expect("initialize instructions")
    }
}

/// How the worker's MCP child is launched.
#[derive(Clone, Copy)]
struct Launch<'a> {
    /// Extra `orch mcp` arguments (a project pin, for the hazard test).
    mcp_args: &'a str,
    /// Drop `ORCH_AGENT_ID` before launching the child (a client that does not
    /// forward the environment).
    forget_identity: bool,
}

const FORWARDING: Launch<'static> = Launch {
    mcp_args: "",
    forget_identity: false,
};

const WORKER_SCRIPT: &str = r#"#!/bin/sh
pwd > "$OUT/cwd"
printf '%s' "${ORCH_AGENT_ID-<unset>}" > "$OUT/agent_id"
if [ -n "$FORGET_IDENTITY" ]; then unset ORCH_AGENT_ID; fi
: > "$OUT/responses.jsonl"
# Like a real client: keep stdin open until every answer has arrived, then EOF.
{
  cat "$OUT/requests.jsonl"
  n=0
  while [ "$(wc -l < "$OUT/responses.jsonl" | tr -d ' ')" -lt 3 ] && [ "$n" -lt 400 ]; do
    sleep 0.05
    n=$((n + 1))
  done
} | "$ORCH_BIN" mcp $MCP_ARGS > "$OUT/responses.jsonl" 2> "$OUT/stderr"
echo $? > "$OUT/exit"
"#;

/// Run one worker of `mode` in `project` (a task assigned to a fresh agent) and
/// return what it recorded. `peer` is the agent its message goes to.
fn run_worker(project: &Path, mode: &str, title: &str, launch: Launch<'_>) -> (WorkerRun, String) {
    let out = tempfile::Builder::new()
        .prefix("orch-worker-")
        .tempdir()
        .expect("worker output dir");
    let script = out.path().join("worker.sh");
    std::fs::write(&script, WORKER_SCRIPT).expect("write worker script");
    let peer = cli_json(
        project,
        &[
            "agent",
            "add",
            &format!("peer-{mode}"),
            "--adapter",
            "shell",
            "--command",
            "true",
        ],
    )["id"]
        .as_str()
        .expect("peer id")
        .to_owned();
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": mcp_common::PROTOCOL_VERSION, "capabilities": {},
            "clientInfo": {"name": "worker", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "task_add", "arguments": {"title": title}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "msg_send", "arguments": {"to_agent_id": peer, "body": title}}}),
    ];
    let lines: String = requests.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(out.path().join("requests.jsonl"), lines).expect("write requests");

    let forget = if launch.forget_identity { "1" } else { "" };
    let command = format!(
        "env OUT='{}' ORCH_BIN='{}' MCP_ARGS='{}' FORGET_IDENTITY='{forget}' sh '{}'",
        out.path().display(),
        env!("CARGO_BIN_EXE_orch"),
        launch.mcp_args,
        script.display(),
    );
    let agent = cli_json(
        project,
        &[
            "agent",
            "add",
            &format!("worker-{mode}"),
            "--adapter",
            "shell",
            "--command",
            &command,
            "--workspace-mode",
            mode,
        ],
    )["id"]
        .as_str()
        .expect("worker id")
        .to_owned();
    let task = cli_json(
        project,
        &["task", "add", &format!("job-{mode}"), "--assignee", &agent],
    )["id"]
        .as_str()
        .expect("task id")
        .to_owned();

    let log = out.path().join("run.log");
    // `orch_command`'s PATH (`/usr/bin:/bin`) stays: the worker, git and the
    // worker's MCP child all see the system directories and nothing else.
    let status = run_with_deadline(
        orch_command(project)
            .args(["run", &task])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1"),
        &log,
        Duration::from_secs(90),
    );
    let read = |name: &str| std::fs::read_to_string(out.path().join(name)).unwrap_or_default();
    assert!(
        status,
        "`orch run` failed or timed out: {}; mcp stderr: {}",
        read("run.log"),
        read("stderr")
    );
    let responses = read("responses.jsonl")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| v["id"].as_u64().map(|id| (id, v)))
        .collect();
    (
        WorkerRun {
            cwd: PathBuf::from(read("cwd").trim()),
            agent_id_seen: read("agent_id"),
            mcp_exit: read("exit").trim().to_owned(),
            responses,
            stderr: read("stderr"),
        },
        agent,
    )
}

/// Run `command` to completion within `limit`, output going to `log`; true on
/// a zero exit. The command leads its own process group, and one that overruns
/// is killed together with every descendant (the worker script, its `orch mcp`
/// child, whatever they started), then reaped.
fn run_with_deadline(command: &mut std::process::Command, log: &Path, limit: Duration) -> bool {
    let sink = std::fs::File::create(log).expect("log file");
    mcp_common::own_process_group(command)
        .stdin(Stdio::null())
        .stdout(sink.try_clone().expect("clone log"))
        .stderr(sink);
    let mut child = command.spawn().expect("spawn orch run");
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("poll orch run") {
            return status.success();
        }
        if Instant::now() >= deadline {
            mcp_common::kill_and_reap(&mut child);
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize")
}

/// The titles of every task stored in `project`.
fn task_titles(project: &Path) -> Vec<String> {
    let mut titles: Vec<String> = cli_json(project, &["task", "list"])
        .as_array()
        .expect("tasks")
        .iter()
        .map(|t| t["title"].as_str().expect("title").to_owned())
        .collect();
    titles.sort();
    titles
}

/// Check one worker of `mode`: where it ran, who it was, and that it served
/// (and wrote to) exactly its own project.
fn assert_worker_served_its_project(
    run: &WorkerRun,
    agent: &str,
    project: &Path,
    name: &str,
    mode: &str,
    title: &str,
) {
    let root = canonical(project);
    // A worktree is removed once the run ends, so `cwd` may no longer exist to
    // canonicalize; compare with the macOS `/private` alias folded away.
    let fold = |path: &Path| path.strip_prefix("/private").unwrap_or(path).to_owned();
    let cwd = fold(&run.cwd);
    match mode {
        "shared" => assert_eq!(cwd, fold(&root), "a shared workspace is the project root"),
        _ => assert!(
            cwd.starts_with(fold(&root).join(".orchestry/workspaces")),
            "a {mode} workspace is nested under the owner's .orchestry/workspaces: {cwd:?}"
        ),
    }
    assert_eq!(
        run.agent_id_seen, agent,
        "the adapter forwards ORCH_AGENT_ID"
    );
    assert_eq!(run.mcp_exit, "0", "mcp stderr: {}", run.stderr);
    let instructions = run.instructions();
    assert!(
        instructions.contains(&root.display().to_string()),
        "{mode}: instructions name the owning root {root:?}: {instructions}"
    );
    assert!(
        instructions.contains(&format!("orch project: \"{name}\"")),
        "{instructions}"
    );
    assert!(
        instructions.contains(&format!("Default agent: \"{agent}\".")),
        "default agent is the inherited id: {instructions}"
    );
    let created = worker_json(run, 2);
    assert_eq!(created["title"], title);
    let sent = worker_json(run, 3);
    assert_eq!(
        sent[0]["from_agent_id"],
        json!(agent),
        "attribution is the inherited identity"
    );
    assert!(
        task_titles(project).contains(&title.to_owned()),
        "the task landed in {name}"
    );
}

/// The JSON value of a worker's successful tool call `id`.
fn worker_json(run: &WorkerRun, id: u64) -> Value {
    let response = &run.responses[&id];
    assert_eq!(
        response["result"]["isError"],
        json!(false),
        "call {id} failed: {response}; mcp stderr: {}",
        run.stderr
    );
    serde_json::from_str(tool_text(response)).expect("tool JSON")
}

fn worker_pair(mode: &str) {
    let a = git_project("proj-a");
    let b = git_project("proj-b");
    let title_a = format!("from-a-{mode}");
    let title_b = format!("from-b-{mode}");
    let (a_path, b_path) = (a.path().to_owned(), b.path().to_owned());
    let (mode_a, mode_b) = (mode.to_owned(), mode.to_owned());
    let (ta, tb) = (title_a.clone(), title_b.clone());
    let worker_a = spawn_bounded("worker A", move || {
        run_worker(&a_path, &mode_a, &ta, FORWARDING)
    });
    let worker_b = spawn_bounded("worker B", move || {
        run_worker(&b_path, &mode_b, &tb, FORWARDING)
    });
    let (run_a, agent_a) = worker_a.join(WORKER_JOIN_LIMIT);
    let (run_b, agent_b) = worker_b.join(WORKER_JOIN_LIMIT);
    assert_ne!(agent_a, agent_b);
    assert_worker_served_its_project(&run_a, &agent_a, a.path(), "proj-a", mode, &title_a);
    assert_worker_served_its_project(&run_b, &agent_b, b.path(), "proj-b", mode, &title_b);
    // A task made in B never creates a record in A, and the reverse.
    let a_titles = task_titles(a.path());
    let b_titles = task_titles(b.path());
    assert!(
        !a_titles.contains(&title_b),
        "A holds B's task: {a_titles:?}"
    );
    assert!(
        !b_titles.contains(&title_a),
        "B holds A's task: {b_titles:?}"
    );
}

#[test]
fn a_shared_workspace_worker_serves_its_own_project_with_its_inherited_identity() {
    worker_pair("shared");
}

#[test]
fn a_worktree_workspace_worker_serves_the_owning_project_with_its_inherited_identity() {
    worker_pair("worktree");
}

#[test]
fn an_isolated_workspace_worker_serves_the_owning_project_with_its_inherited_identity() {
    worker_pair("isolated");
}

#[test]
fn a_client_that_does_not_forward_the_environment_cannot_send_without_an_explicit_from() {
    let project = git_project("proj-a");
    let (run, _agent) = run_worker(
        project.path(),
        "shared",
        "lost-identity",
        Launch {
            mcp_args: "",
            forget_identity: true,
        },
    );
    assert!(
        run.instructions().contains("Default agent: unset"),
        "{}",
        run.instructions()
    );
    assert_eq!(
        error_code(&run.responses[&3]),
        Some(-32602),
        "{}",
        run.responses[&3]
    );
    assert!(
        task_titles(project.path()).contains(&"lost-identity".to_owned()),
        "the task call, which needs no identity, still worked"
    );
}

#[test]
fn a_registration_pinned_to_project_a_is_refused_for_a_worker_in_b_and_writes_nowhere() {
    // Why design D19 forbids a user-global entry with --project-root: the pin
    // wins over the worker's cwd. Before the daemon, B's worker silently
    // mutated A. Now the worker inherits B's daemon socket, and the daemon
    // refuses a bridge that names another project, so nothing is written.
    let a = git_project("proj-a");
    let b = git_project("proj-b");
    let pin = format!("--project-root {}", a.path().display());
    let (run, _agent) = run_worker(
        b.path(),
        "shared",
        "pinned-wrongly",
        Launch {
            mcp_args: &pin,
            forget_identity: false,
        },
    );
    let refusal = run.responses[&1]["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        refusal.contains("wrong_project"),
        "initialize must be refused as the wrong project: {}",
        run.responses[&1]
    );
    assert!(!task_titles(a.path()).contains(&"pinned-wrongly".to_owned()));
    assert!(!task_titles(b.path()).contains(&"pinned-wrongly".to_owned()));
}

// ── harness self-checks (the deadlines every fixture relies on) ───────────

#[test]
fn a_fixture_command_that_overruns_is_killed_with_every_descendant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("descendant.pid");
    let script = format!("sleep 300 & echo $! > '{}'; wait", pid_file.display());
    let limit = Duration::from_secs(3);
    let run = spawn_bounded("the overrunning command", move || {
        output_within_for_test(&script, limit)
    });
    assert!(
        mcp_common::wait_until(mcp_common::TIMEOUT, || pid_file.exists()
            && std::fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty())),
        "the command never started its descendant"
    );
    let descendant: u32 = std::fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid");
    assert!(mcp_common::StallingAdapters::alive(descendant));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run.join(Duration::from_secs(30))
    }));
    let panic = outcome.expect_err("an overrun must fail the test, not return");
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("did not finish within"), "{message}");
    assert!(
        mcp_common::wait_until(mcp_common::TIMEOUT, || {
            !mcp_common::StallingAdapters::alive(descendant)
        }),
        "the descendant of the overrunning command survived the timeout"
    );
}

fn output_within_for_test(script: &str, limit: Duration) -> std::process::Output {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", script]);
    mcp_common::output_within(&mut command, limit)
}

#[test]
fn a_fixture_command_that_finishes_returns_its_status_and_output() {
    let output = output_within_for_test("echo out; echo err >&2; exit 3", mcp_common::CLI_TIMEOUT);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"out\n");
    assert_eq!(output.stderr, b"err\n");
}

#[test]
fn a_rendezvous_is_released_with_an_error_when_a_peer_fails_before_arriving() {
    let rendezvous = Rendezvous::new(2);
    let waiting = {
        let rendezvous = std::sync::Arc::clone(&rendezvous);
        spawn_bounded("the waiting peer", move || {
            rendezvous.arrive(Duration::from_secs(60))
        })
    };
    let failing = {
        let rendezvous = std::sync::Arc::clone(&rendezvous);
        spawn_bounded("the failing peer", move || {
            let _participant = rendezvous.participant();
            panic!("the creator failed before reaching the rendezvous");
        })
    };
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        failing.join(Duration::from_secs(30))
    }));
    assert!(failure.is_err());
    // Released by the cancellation, long before its own 60 s deadline.
    let released = waiting.join(Duration::from_secs(30));
    assert!(
        released.is_err_and(|reason| reason.contains("cancelled")),
        "the waiting peer must be told the rendezvous was cancelled"
    );
}

#[test]
fn a_rendezvous_of_two_parties_releases_both_when_both_arrive() {
    let rendezvous = Rendezvous::new(2);
    let peer = {
        let rendezvous = std::sync::Arc::clone(&rendezvous);
        spawn_bounded("the peer", move || rendezvous.arrive(mcp_common::TIMEOUT))
    };
    assert_eq!(rendezvous.arrive(mcp_common::TIMEOUT), Ok(()));
    assert_eq!(peer.join(Duration::from_secs(30)), Ok(()));
}

#[test]
fn a_bounded_join_fails_instead_of_waiting_for_a_thread_that_never_returns() {
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let stuck = spawn_bounded("a stuck helper", move || {
        let _ = hold.recv();
    });
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stuck.join(Duration::from_millis(100))
    }));
    let panic = outcome.expect_err("the join must time out");
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        message.contains("a stuck helper did not finish"),
        "{message}"
    );
    drop(release);
}

// ── tools/list size per mode (design D20) ─────────────────────────────────

/// The serialized bytes of the `tools/list` result of a mode: the JSON-RPC
/// `result` object as the server wrote it, which is what a client must hold.
fn tools_list_bytes(args: &[&str]) -> (usize, usize) {
    let dir = initialized_project();
    let mut server = McpServer::spawn_with(dir.path(), args, &[]);
    server.initialize();
    let result = server.expect_result("tools/list", json!({}));
    let count = result["tools"].as_array().expect("tools").len();
    (
        serde_json::to_string(&result).expect("serialise").len(),
        count,
    )
}

#[test]
fn tools_list_sizes_are_reported_for_all_three_modes() {
    let (read_only, read_only_count) = tools_list_bytes(&["--read-only"]);
    let (default, default_count) = tools_list_bytes(&[]);
    let (destructive, destructive_count) = tools_list_bytes(&["--allow-destructive"]);
    // Printed for the report (run with --nocapture); the assertions are the
    // monotonic, count-consistent facts a size regression would break.
    println!(
        "tools/list bytes: read-only={read_only} ({read_only_count} tools) \
         default={default} ({default_count} tools) allow-destructive={destructive} ({destructive_count} tools)"
    );
    assert_eq!(
        (read_only_count, default_count, destructive_count),
        (18, 45, 50)
    );
    assert!(read_only < default && default < destructive);
}

// ── a negative call for every tool ────────────────────────────────────────

/// Entities every tool of the catalogue can be pointed at.
struct Catalogue {
    agent: String,
    spare_agent: String,
    task: String,
    spare_task: String,
    goal: String,
    spare_goal: String,
    team: String,
    spare_team: String,
}

/// An otherwise valid `arguments` object for each of the 50 tools, typed out
/// from the CLI (never read from the registry): every required field present
/// and well-typed, naming entities that exist. Panics for an unlisted tool, so
/// a tool added to the catalogue cannot slip past the loops that use this.
fn valid_arguments(tool: &str, c: &Catalogue) -> Value {
    match tool {
        "agent_add" => json!({"name": "added", "adapter": "shell", "command": "true"}),
        "agent_autonomous" => json!({"id": c.agent, "on": true}),
        "agent_disable" | "agent_enable" | "agent_show" => json!({"id": c.agent}),
        "agent_list" | "context_list" | "doctor" | "goal_list" | "msg_list" | "remote_list"
        | "remote_status" | "status" | "task_list" | "team_list" => json!({}),
        "agent_patch" | "task_patch" | "task_update" => {
            json!({"id": if tool == "agent_patch" { &c.agent } else { &c.task }, "file": "def.json"})
        }
        "agent_remove" => json!({"id": c.spare_agent}),
        "agent_update" => json!({"id": c.agent, "role_summary": "r"}),
        "config_get" => json!({"key": "project.name"}),
        "config_patch" => json!({"file": "def.json"}),
        "config_set" => json!({"key": "defaults.task.priority", "value": "2"}),
        "config_update" => json!({"file": "config.yml"}),
        "context_delete" | "context_get" => json!({"key": "k"}),
        "context_set" => json!({"key": "k2", "value": "v"}),
        "goal_add" => json!({"title": "added"}),
        "goal_delete" => json!({"id": c.spare_goal}),
        "goal_show" => json!({"id": c.goal}),
        "goal_status" => json!({"id": c.goal, "status": "abandoned"}),
        "goal_update" => json!({"id": c.goal, "title": "renamed"}),
        "logs" => json!({"agent": c.agent}),
        "msg_broadcast" => json!({"body": "hello", "from": c.spare_agent}),
        "msg_inbox" => json!({"agent_id": c.agent}),
        "msg_send" => json!({"to_agent_id": c.agent, "body": "hello", "from": c.spare_agent}),
        "task_add" => json!({"title": "added"}),
        "task_approve" | "task_reject" | "task_retry" | "task_show" => json!({"id": c.task}),
        "task_assign" => json!({"task_id": c.task, "agent_id": c.agent}),
        "task_cancel" => json!({"id": c.spare_task}),
        "team_add_task" => json!({"team_id": c.team, "task_id": c.task}),
        "team_create" => json!({"name": "added", "lead": c.agent}),
        "team_disband" => json!({"id": c.spare_team}),
        "team_join" | "team_leave" | "team_set_lead" => {
            json!({"team_id": c.team, "agent_id": c.spare_agent})
        }
        "team_show" => json!({"id": c.team}),
        other => panic!("no valid arguments are typed out for the tool `{other}`"),
    }
}

/// A destructive server holding one of everything, plus the files the
/// file-taking tools point at.
fn catalogue_fixture() -> (Fx, Catalogue) {
    let fx = Fx::destructive();
    let agent = fx.agent("first");
    let spare_agent = fx.agent("spare");
    let task = fx.task("t");
    let spare_task = fx.task("spare");
    let id_of = |value: Value| value["id"].as_str().expect("id").to_owned();
    let goal = id_of(fx.cli(&["goal", "add", "G"]));
    let spare_goal = id_of(fx.cli(&["goal", "add", "spare"]));
    let team = id_of(fx.cli(&["team", "create", "crew", "--lead", &agent]));
    let spare_team = id_of(fx.cli(&["team", "create", "spare", "--lead", &agent]));
    fx.cli_plain(&["context", "set", "k", "v"]);
    fx.doc("def.json", "{}");
    let config = std::fs::read_to_string(fx.root().join(".orchestry/config.yml")).expect("config");
    fx.doc("config.yml", &config);
    let catalogue = Catalogue {
        agent,
        spare_agent,
        task,
        spare_task,
        goal,
        spare_goal,
        team,
        spare_team,
    };
    (fx, catalogue)
}

fn all_fifty_tools() -> Vec<&'static str> {
    let tools: Vec<&str> = EXPECTED.iter().chain(DESTRUCTIVE).copied().collect();
    assert_eq!(tools.len(), 50);
    tools
}

#[test]
fn every_one_of_the_fifty_tools_refuses_an_undeclared_field_naming_it_and_changes_nothing() {
    const EXTRA: &str = "undeclared_argument_for_the_test";
    let (mut fx, catalogue) = catalogue_fixture();
    let before = snapshot(&fx.root().join(".orchestry"));
    for tool in all_fifty_tools() {
        // Everything the tool needs is present and valid; the undeclared field
        // is the only thing wrong, so only the field check can refuse it.
        let mut arguments = valid_arguments(tool, &catalogue);
        arguments[EXTRA] = json!(1);
        let response = fx.rpc_error(tool, arguments.clone());
        assert_eq!(error_code(&response), Some(-32602), "{tool}: {response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(EXTRA),
            "{tool} must refuse for the undeclared field: {response}"
        );
        assert!(
            !message.contains("missing field"),
            "{tool}: the arguments were meant to be complete: {response}"
        );
    }
    assert_eq!(snapshot(&fx.root().join(".orchestry")), before);
    assert!(
        fx.ok("status", json!({})).is_object(),
        "the session survives all of them"
    );
}

#[test]
fn the_typed_out_arguments_of_all_fifty_tools_pass_argument_validation() {
    // The control for the test above: without the undeclared field the same
    // arguments are accepted by argument validation (a tool-level error such
    // as "already a member" is fine - only a protocol error would mean the
    // arguments were malformed and the test above proved nothing).
    let (mut fx, catalogue) = catalogue_fixture();
    for tool in all_fifty_tools() {
        let arguments = valid_arguments(tool, &catalogue);
        let response = fx.call(tool, arguments.clone());
        assert!(
            response.get("error").is_none(),
            "{tool} {arguments}: the typed-out arguments are not valid: {response}"
        );
    }
}

#[test]
fn every_tool_with_required_arguments_refuses_their_absence_naming_the_field() {
    // Kept apart from the undeclared-field loop so each failure has one cause.
    let required: &[(&str, &str)] = &[
        ("agent_add", "name"),
        ("agent_autonomous", "id"),
        ("agent_disable", "id"),
        ("agent_enable", "id"),
        ("agent_patch", "id"),
        ("agent_remove", "id"),
        ("agent_show", "id"),
        ("agent_update", "id"),
        ("config_get", "key"),
        ("config_patch", "file"),
        ("config_set", "key"),
        ("config_update", "file"),
        ("context_delete", "key"),
        ("context_get", "key"),
        ("context_set", "key"),
        ("goal_add", "title"),
        ("goal_delete", "id"),
        ("goal_show", "id"),
        ("goal_status", "id"),
        ("goal_update", "id"),
        ("msg_broadcast", "body"),
        ("msg_send", "to_agent_id"),
        ("task_add", "title"),
        ("task_approve", "id"),
        ("task_assign", "task_id"),
        ("task_cancel", "id"),
        ("task_patch", "id"),
        ("task_reject", "id"),
        ("task_retry", "id"),
        ("task_show", "id"),
        ("task_update", "id"),
        ("team_add_task", "team_id"),
        ("team_create", "name"),
        ("team_disband", "id"),
        ("team_join", "team_id"),
        ("team_leave", "team_id"),
        ("team_set_lead", "team_id"),
        ("team_show", "id"),
    ];
    let (mut fx, _) = catalogue_fixture();
    let before = snapshot(&fx.root().join(".orchestry"));
    for (tool, field) in required {
        let response = fx.rpc_error(tool, json!({}));
        assert_eq!(error_code(&response), Some(-32602), "{tool}: {response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(&format!("missing field `{field}`")),
            "{tool}: {response}"
        );
    }
    assert_eq!(snapshot(&fx.root().join(".orchestry")), before);
    assert!(fx.ok("status", json!({})).is_object());
}

#[test]
fn context_delete_without_a_key_is_invalid_params() {
    let mut fx = Fx::destructive();
    assert_eq!(fx.rpc_code("context_delete", json!({})), -32602);
}

#[test]
fn context_set_without_a_value_is_invalid_params_and_stores_nothing() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("context_set", json!({"key": "k"})), -32602);
    assert_eq!(fx.cli(&["context", "list"]), json!([]));
}

#[test]
fn team_leave_with_an_unknown_team_is_a_tool_error() {
    let mut fx = Fx::new();
    let agent = fx.agent("a");
    fx.tool_error(
        "team_leave",
        json!({"team_id": "team_nope", "agent_id": agent}),
    );
}

#[test]
fn agent_disable_without_an_id_is_invalid_params() {
    let mut fx = Fx::new();
    assert_eq!(fx.rpc_code("agent_disable", json!({})), -32602);
}

#[test]
fn a_list_tool_given_an_argument_it_does_not_declare_is_invalid_params() {
    let mut fx = Fx::new();
    for tool in [
        "agent_list",
        "goal_list",
        "msg_list",
        "team_list",
        "remote_list",
    ] {
        let arguments = if tool == "goal_list" || tool == "msg_list" {
            json!({"unknown": 1})
        } else {
            json!({"status": "x"})
        };
        assert_eq!(fx.rpc_code(tool, arguments), -32602, "{tool}");
    }
}
