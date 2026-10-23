//! D5-1 / D5-7 — the old/new compatibility matrix, driven through the real
//! `orch` binaries against disposable stores.
//!
//! Columns: parse/list, dispatch, unrelated update, repair, downgrade (plus
//! diagnostics). Rows: Claude, Codex, OpenCode recognised, OpenCode
//! delegated/unknown, Antigravity, Pi, Grok, Cursor, Shell. Effort states:
//! unset, each legacy spelling, each new spelling.
//!
//! The *new* reader is this workspace's `orch`. The *old* reader is the
//! `d8c2319` binary named by `ORCH_T1_OLD_BIN` (built by
//! `fixtures/token-eco/matrix/run-matrix.sh`); without it the old-reader
//! cells print `SKIPPED` and the harness script, which sets
//! `ORCH_T1_REQUIRE_OLD=1`, is what makes their absence a failure.
//!
//! The expectations below are written from design-v3 D5 / §4 (vectors D5-1,
//! D5-2, D5-7), not derived from the implementation's own tables.

mod token_eco_support;

use token_eco_support::{Out, Reader, Sandbox, cli_of, new_reader, old_reader};

struct Row {
    key: &'static str,
    adapter: &'static str,
    model: Option<&'static str>,
}

const ROWS: [Row; 9] = [
    Row {
        key: "claude",
        adapter: "claude",
        model: Some("claude-sonnet-5-5"),
    },
    Row {
        key: "codex",
        adapter: "codex",
        model: Some("gpt-6.1-sol"),
    },
    Row {
        key: "opencode-recognized",
        adapter: "opencode",
        model: Some("openrouter/anthropic/claude-opus-5.5"),
    },
    Row {
        key: "opencode-delegated",
        adapter: "opencode",
        model: None,
    },
    Row {
        key: "antigravity",
        adapter: "antigravity",
        model: Some("gemini-4-argon"),
    },
    Row {
        key: "pi",
        adapter: "pi",
        model: Some("openai-codex/gpt-6.1-sol"),
    },
    Row {
        key: "grok",
        adapter: "grok",
        model: Some("grok-build"),
    },
    Row {
        key: "cursor",
        adapter: "cursor",
        model: Some("auto"),
    },
    Row {
        key: "shell",
        adapter: "shell",
        model: None,
    },
];

const LEGACY: [&str; 3] = ["low", "medium", "high"];
const NEW_ONLY: [&str; 6] = ["none", "off", "minimal", "xhigh", "max", "ultra"];

fn efforts() -> Vec<Option<&'static str>> {
    let mut all = vec![None];
    all.extend(LEGACY.iter().map(|e| Some(*e)));
    all.extend(NEW_ONLY.iter().map(|e| Some(*e)));
    all
}

fn is_legacy(effort: &str) -> bool {
    LEGACY.contains(&effort)
}

/// D5 table, written out: is `effort` an authorable request for this row?
fn authoring_ok(row: &Row, effort: &str) -> bool {
    let allowed: &[&str] = match row.key {
        "claude" | "opencode-recognized" => &["low", "medium", "high", "xhigh", "max"],
        "codex" => &["low", "medium", "high", "xhigh", "max", "ultra"],
        "antigravity" | "grok" => &["low", "medium", "high"],
        "pi" => &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        // OpenCode with no model, Cursor and Shell expose no generic effort.
        _ => &[],
    };
    allowed.contains(&effort)
}

/// Legacy spellings the old binary forwarded on these adapters and the new
/// binary still forwards byte-for-byte (lead ruling 1).
fn legacy_passthrough(row: &Row) -> bool {
    matches!(row.key, "claude" | "pi" | "grok")
}

fn cell(row: &Row, effort: Option<&str>) -> String {
    format!("{}/{}", row.key, effort.unwrap_or("unset"))
}

fn seeded(reader: &Reader, row: &Row, effort: Option<&str>) -> (Sandbox, String) {
    let sb = Sandbox::new(reader);
    let id = sb.seed_agent(reader, "Subject", row.adapter, row.model, effort);
    (sb, id)
}

fn agent_json(out: &Out, id: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(&out.stdout).ok()?;
    match value {
        serde_json::Value::Array(items) => items.into_iter().find(|a| a["id"] == id),
        other => Some(other),
    }
}

fn finish(column: &str, reader: &str, total: usize, failures: Vec<String>) {
    println!(
        "MATRIX column={column} reader={reader} cells={total} pass={} fail={}",
        total - failures.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {total} {column}/{reader} cells failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── parse / list ───────────────────────────────────────────────────────────

/// Every stored spelling parses and lists in the new reader, for every
/// adapter, whether or not the combination is valid; reading never writes.
#[test]
fn new_reader_lists_every_spelling_on_every_adapter_without_rewriting_bytes() {
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in efforts() {
            total += 1;
            let (sb, id) = seeded(&new, row, effort);
            let before = sb.agent_bytes();
            let list = sb.run(&new, &["--json", "agent", "list"], None);
            let status = sb.run(&new, &["--json", "agent", "status", &id], None);
            let c = cell(row, effort);
            let listed = agent_json(&list, &id);
            let shown = agent_json(&status, &id);
            let stored = |v: &Option<serde_json::Value>| {
                v.as_ref()
                    .and_then(|a| a["config"]["effort"].as_str().map(str::to_owned))
            };
            if !list.ok() || !status.ok() {
                failures.push(format!(
                    "{c}: list/status exit {}/{}: {}",
                    list.code, status.code, list.stderr
                ));
            } else if stored(&listed).as_deref() != effort || stored(&shown).as_deref() != effort {
                failures.push(format!(
                    "{c}: effort not round-tripped: list={:?} status={:?}",
                    stored(&listed),
                    stored(&shown)
                ));
            } else if sb.agent_bytes() != before {
                failures.push(format!("{c}: reading rewrote agent bytes"));
            }
        }
    }
    finish("parse-list", "new", total, failures);
}

/// Legacy scalars and unset effort parse in the old reader; the new
/// spellings are refused loudly (exit 1, the file named), never skipped.
#[test]
fn old_reader_lists_legacy_values_and_refuses_new_spellings_loudly() {
    let Some(old) = old_reader() else { return };
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in efforts() {
            total += 1;
            let (sb, id) = seeded(&new, row, effort);
            let before = sb.agent_bytes();
            let out = sb.run(&old, &["--json", "agent", "list"], None);
            let c = cell(row, effort);
            let new_spelling = effort.is_some_and(|e| !is_legacy(e));
            if new_spelling {
                if out.ok() || !out.stderr.contains(&format!("{id}.yml")) || !out.stdout.is_empty()
                {
                    failures.push(format!(
                        "{c}: old reader did not fail loudly: code={} stdout={:?} stderr={:?}",
                        out.code, out.stdout, out.stderr
                    ));
                }
            } else if !out.ok() || agent_json(&out, &id).is_none() {
                failures.push(format!(
                    "{c}: old reader could not list a legacy/unset store: code={} {}",
                    out.code, out.stderr
                ));
            }
            if sb.agent_bytes() != before {
                failures.push(format!("{c}: old reader rewrote agent bytes"));
            }
        }
    }
    finish("parse-list", "old", total, failures);
}

// ── unrelated update ───────────────────────────────────────────────────────

fn byte_preserved_rejection(
    sb: &Sandbox,
    before: &[(String, Vec<u8>)],
    out: &Out,
    id: &str,
    effort: &str,
    c: &str,
    failures: &mut Vec<String>,
) {
    if out.ok() {
        failures.push(format!(
            "{c}: unrelated update accepted an incompatible stored combination"
        ));
        return;
    }
    let text = out.all();
    for needle in [id, effort, "--effort default"] {
        if !text.contains(needle) {
            failures.push(format!("{c}: rejection lacks `{needle}`: {text}"));
        }
    }
    if sb.agent_bytes() != before {
        failures.push(format!(
            "{c}: rejected update changed definition/index bytes"
        ));
    }
}

/// An unrelated edit (the role) of a valid combination succeeds; of an
/// incompatible stored one it fails, naming agent, value and the remedy, and
/// leaves definition and index bytes untouched. Unset effort always edits.
#[test]
fn new_reader_unrelated_update_accepts_valid_and_rejects_incompatible_with_bytes_preserved() {
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in efforts() {
            total += 1;
            let (sb, id) = seeded(&new, row, effort);
            let before = sb.agent_bytes();
            let out = sb.run(
                &new,
                &["agent", "patch", &id, "-f", "-"],
                Some(r#"{"role":"Unrelated edit."}"#),
            );
            let c = cell(row, effort);
            match effort {
                None => {
                    if !out.ok() {
                        failures.push(format!("{c}: unset effort edit failed: {}", out.all()));
                    }
                }
                // Shell silently drops a stored effort (baseline behaviour); the
                // design says a non-AI adapter's value should be rejected. See the
                // ignored `shell_agent_effort_is_rejected_...` test.
                Some(_) if row.key == "shell" => {
                    if !out.ok() {
                        failures.push(format!("{c}: shell edit failed: {}", out.all()));
                    }
                }
                Some(e) if authoring_ok(row, e) => {
                    if !out.ok() {
                        failures.push(format!("{c}: valid combination edit failed: {}", out.all()));
                    } else if !String::from_utf8_lossy(
                        &sb.agent_bytes()
                            .iter()
                            .find(|(n, _)| n == &format!("{id}.yml"))
                            .unwrap()
                            .1,
                    )
                    .contains(&format!("effort: {e}"))
                    {
                        failures.push(format!("{c}: edit lost or rewrote the stored effort"));
                    }
                }
                Some(e) => byte_preserved_rejection(&sb, &before, &out, &id, e, &c, &mut failures),
            }
        }
    }
    finish("unrelated-update", "new", total, failures);
}

/// The same holds for full-definition replacement (`update -f`): the original
/// file semantics, now with final-candidate validation.
#[test]
fn new_reader_full_replacement_of_an_incompatible_legacy_agent_is_rejected_and_preserves_bytes() {
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in LEGACY {
            if authoring_ok(row, effort) || row.key == "shell" {
                continue;
            }
            total += 1;
            let (sb, id) = seeded(&new, row, Some(effort));
            let before = sb.agent_bytes();
            let doc = format!(
                r#"{{"name":"Subject","adapter":"{}","role":"Replaced.","config":{{{}"effort":"{effort}"}}}}"#,
                row.adapter,
                row.model
                    .map(|m| format!(r#""model":"{m}","#))
                    .unwrap_or_default()
            );
            let out = sb.run(&new, &["agent", "update", &id, "-f", "-"], Some(&doc));
            byte_preserved_rejection(
                &sb,
                &before,
                &out,
                &id,
                effort,
                &cell(row, Some(effort)),
                &mut failures,
            );
        }
    }
    finish("unrelated-update-full-replace", "new", total, failures);
}

/// The old reader edits legacy stores freely (it never validated the
/// combination) — the behaviour the new reader's strictness is measured against.
#[test]
fn old_reader_unrelated_update_of_legacy_stores_succeeds_for_every_combination() {
    let Some(old) = old_reader() else { return };
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in efforts().into_iter().filter(|e| e.is_none_or(is_legacy)) {
            total += 1;
            let (sb, id) = seeded(&new, row, effort);
            let out = sb.run(
                &old,
                &["agent", "patch", &id, "-f", "-"],
                Some(r#"{"role":"Unrelated edit."}"#),
            );
            if !out.ok() {
                failures.push(format!(
                    "{}: old reader edit failed: {}",
                    cell(row, effort),
                    out.all()
                ));
            }
        }
    }
    finish("unrelated-update", "old", total, failures);
}

// ── repair ─────────────────────────────────────────────────────────────────

/// `--effort default` clears only the generic effort, whatever the stored
/// combination, leaving the model byte-for-byte; a rejected edit becomes
/// possible afterwards.
#[test]
fn effort_default_repairs_every_stored_combination_and_keeps_the_model() {
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in efforts().into_iter().flatten() {
            total += 1;
            let (sb, id) = seeded(&new, row, Some(effort));
            let c = cell(row, Some(effort));
            let out = sb.run(&new, &["agent", "update", &id, "--effort", "default"], None);
            if !out.ok() {
                failures.push(format!("{c}: repair failed: {}", out.all()));
                continue;
            }
            let show = sb.run(&new, &["--json", "agent", "status", &id], None);
            let json = agent_json(&show, &id).unwrap_or_default();
            if !json["config"]["effort"].is_null() {
                failures.push(format!("{c}: effort not cleared: {}", json["config"]));
            }
            if json["config"]["model"].as_str() != row.model {
                failures.push(format!(
                    "{c}: model changed by repair: {}",
                    json["config"]["model"]
                ));
            }
            let edit = sb.run(
                &new,
                &["agent", "patch", &id, "-f", "-"],
                Some(r#"{"role":"After repair."}"#),
            );
            if !edit.ok() {
                failures.push(format!(
                    "{c}: edit still rejected after repair: {}",
                    edit.all()
                ));
            }
        }
    }
    finish("repair", "new", total, failures);
}

/// Switching adapter and clearing the incompatible effort is one atomic
/// mutation (D5-6).
#[test]
fn switching_adapter_with_the_effort_cleared_in_one_patch_succeeds_and_without_it_fails_atomically()
{
    let new = new_reader();
    let (sb, id) = seeded(&new, &ROWS[0], Some("max")); // claude max
    let before = sb.agent_bytes();
    let keep = sb.run(
        &new,
        &["agent", "patch", &id, "-f", "-"],
        Some(r#"{"adapter":"grok","config":{"model":null}}"#),
    );
    assert!(!keep.ok(), "grok must not retain `max`: {}", keep.all());
    assert_eq!(sb.agent_bytes(), before, "rejected switch changed bytes");
    let clear = sb.run(
        &new,
        &["agent", "patch", &id, "-f", "-"],
        Some(r#"{"adapter":"grok","config":{"model":null,"effort":null}}"#),
    );
    assert!(clear.ok(), "atomic switch+clear failed: {}", clear.all());
    let json = agent_json(
        &sb.run(&new, &["--json", "agent", "status", &id], None),
        &id,
    )
    .unwrap();
    assert_eq!(json["adapter"], "grok");
    assert!(json["config"]["effort"].is_null());
}

// ── downgrade ──────────────────────────────────────────────────────────────

/// A new binary authors `max`; the old binary refuses to read the project
/// (loudly, bytes intact); clearing the effort with the new binary lets the
/// old binary read it again. `max`/`ultra` are never mapped to `high`.
#[test]
fn downgrade_of_new_spellings_fails_in_the_old_reader_and_repairs_by_clearing_effort() {
    let Some(old) = old_reader() else { return };
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for (adapter, model, effort) in [
        ("claude", "claude-sonnet-5-5", "max"),
        ("claude", "claude-sonnet-5-5", "xhigh"),
        ("codex", "gpt-6.1-sol", "ultra"),
        ("pi", "openai-codex/gpt-6.1-sol", "off"),
        ("pi", "openai-codex/gpt-6.1-sol", "minimal"),
    ] {
        total += 1;
        let c = format!("{adapter}/{effort}");
        let sb = Sandbox::new(&new);
        let add = sb.run(
            &new,
            &[
                "agent",
                "add",
                "D",
                "--adapter",
                adapter,
                "--model",
                model,
                "--effort",
                effort,
                "--quiet",
            ],
            None,
        );
        if !add.ok() {
            failures.push(format!(
                "{c}: new binary could not author the value: {}",
                add.all()
            ));
            continue;
        }
        let id = add.stdout.trim().to_owned();
        let before = sb.agent_bytes();
        let file = String::from_utf8(
            before
                .iter()
                .find(|(n, _)| n == &format!("{id}.yml"))
                .unwrap()
                .1
                .clone(),
        )
        .unwrap();
        if !file.contains(&format!("effort: {effort}")) {
            failures.push(format!("{c}: stored effort is not `{effort}`: {file}"));
        }
        for args in [vec!["agent", "list"], vec!["task", "list"], vec!["status"]] {
            let out = sb.run(&old, &args, None);
            if args[0] == "agent" && (out.ok() || !out.stderr.contains(&format!("{id}.yml"))) {
                failures.push(format!(
                    "{c}: old `{args:?}` did not fail naming the file: {} {}",
                    out.code, out.stderr
                ));
            }
        }
        if sb.agent_bytes() != before {
            failures.push(format!("{c}: old reader changed bytes while refusing"));
        }
        let repair = sb.run(&new, &["agent", "update", &id, "--effort", "default"], None);
        let after = sb.run(&old, &["--json", "agent", "list"], None);
        if !repair.ok() || !after.ok() || agent_json(&after, &id).is_none() {
            failures.push(format!(
                "{c}: repair did not restore old-reader access: {} / {}",
                repair.all(),
                after.all()
            ));
        }
        if sb
            .agent_bytes()
            .iter()
            .any(|(_, b)| String::from_utf8_lossy(b).contains("effort: high"))
        {
            failures.push(format!("{c}: a value was coerced to `high`"));
        }
    }
    finish("downgrade", "old-vs-new", total, failures);
}

// ── diagnostics ────────────────────────────────────────────────────────────

/// An incompatible legacy combination is still inspectable: the human view and
/// stderr carry the actionable warning, and JSON stdout stays pure JSON.
#[test]
fn incompatible_legacy_combinations_show_a_warning_while_json_stdout_stays_clean() {
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in &ROWS {
        for effort in LEGACY {
            if authoring_ok(row, effort) || legacy_passthrough(row) && row.key != "grok" {
                continue;
            }
            total += 1;
            let (sb, id) = seeded(&new, row, Some(effort));
            let c = cell(row, Some(effort));
            let human = sb.run(&new, &["agent", "status", &id], None);
            let json = sb.run(&new, &["--json", "agent", "status", &id], None);
            if !human.ok() || !json.ok() {
                failures.push(format!("{c}: status refused to inspect: {}", json.all()));
                continue;
            }
            if !human.stdout.contains("--effort default") {
                failures.push(format!("{c}: human status lacks the repair remedy"));
            }
            if serde_json::from_str::<serde_json::Value>(&json.stdout).is_err() {
                failures.push(format!(
                    "{c}: --json stdout is not pure JSON: {:?}",
                    json.stdout
                ));
            }
            if !json.stderr.contains("warning") {
                failures.push(format!(
                    "{c}: --json run printed no stderr warning: {:?}",
                    json.stderr
                ));
            }
        }
    }
    finish("diagnostics", "new", total, failures);
}

// ── dispatch ───────────────────────────────────────────────────────────────

/// Drop the approved effort selector from a recorded argv so the remainder can
/// be compared with the baseline's argv.
fn without_selector(row: &Row, argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let drop2 = match row.key {
            "codex" => {
                a == "-c"
                    && argv
                        .get(i + 1)
                        .is_some_and(|n| n.starts_with("model_reasoning_effort="))
            }
            "antigravity" => a == "--effort",
            "claude" | "grok" => a == "--effort",
            "pi" => a == "--thinking",
            _ => false,
        };
        if drop2 {
            i += 2;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

fn selector_of(row: &Row, argv: &[String]) -> Option<String> {
    argv.iter().enumerate().find_map(|(i, a)| match row.key {
        "codex" if a == "-c" => argv
            .get(i + 1)
            .filter(|n| n.starts_with("model_reasoning_effort="))
            .cloned(),
        "claude" | "grok" | "antigravity" if a == "--effort" => argv.get(i + 1).cloned(),
        "pi" if a == "--thinking" => argv.get(i + 1).cloned(),
        _ => None,
    })
}

fn expected_selector(row: &Row, effort: &str) -> String {
    match row.key {
        "codex" => format!("model_reasoning_effort=\"{effort}\""),
        _ => effort.to_owned(),
    }
}

/// Replace ids and the temp project path so argv from two sandboxes compare.
fn normalise(sb: &Sandbox, argv: &[String], ids: &[&str]) -> Vec<String> {
    let mut previous = "";
    argv.iter()
        .map(|a| {
            if previous == "--system-prompt-file" {
                previous = "";
                return "<system-prompt-file>".to_owned();
            }
            previous = if a == "--system-prompt-file" {
                "--system-prompt-file"
            } else {
                ""
            };
            let mut a = a.replace(sb.project.to_str().unwrap(), "<project>");
            for id in ids {
                a = a.replace(id, "<id>");
            }
            a
        })
        .collect()
}

struct Dispatched {
    out: Out,
    argv: Option<Vec<String>>,
    agent: String,
    task: String,
}

fn dispatch(reader: &Reader, row: &Row, effort: Option<&str>) -> (Sandbox, Dispatched) {
    let (sb, id) = seeded(&new_reader(), row, effort);
    let task = sb.run(
        reader,
        &["task", "add", "matrix task", "--assignee", &id, "--quiet"],
        None,
    );
    let task_id = task.stdout.trim().to_owned();
    let out = if task.ok() {
        sb.run(reader, &["run", &task_id], None)
    } else {
        task
    };
    let argv = if row.adapter == "shell" {
        None
    } else {
        sb.recorded(cli_of(row.adapter)).into_iter().last()
    };
    (
        sb,
        Dispatched {
            out,
            argv,
            agent: id,
            task: task_id,
        },
    )
}

/// Unset effort and legacy spellings: new and old binaries produce the same
/// child argv, except the three named exceptions (Codex explicit effort
/// config, Antigravity `--effort`, recognised-model OpenCode `#level`).
#[test]
fn dispatch_argv_of_unset_and_legacy_effort_matches_the_old_binary_except_the_named_exceptions() {
    let Some(old) = old_reader() else { return };
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in ROWS.iter().filter(|r| r.adapter != "shell") {
        for effort in efforts().into_iter().filter(|e| e.is_none_or(is_legacy)) {
            total += 1;
            let c = cell(row, effort);
            let (osb, o) = dispatch(&old, row, effort);
            let (nsb, n) = dispatch(&new, row, effort);
            let (Some(oa), Some(na)) = (o.argv.as_ref(), n.argv.as_ref()) else {
                failures.push(format!(
                    "{c}: no child recorded (old: {} {}; new: {} {})",
                    o.out.code, o.out.stderr, n.out.code, n.out.stderr
                ));
                continue;
            };
            let oa = normalise(&osb, oa, &[&o.agent, &o.task]);
            let na = normalise(&nsb, na, &[&n.agent, &n.task]);
            let oa_stripped = oa.clone();
            let ok = match (row.key, effort) {
                (_, None) => na == oa,
                ("claude" | "pi" | "grok", Some(_)) => na == oa,
                ("codex" | "antigravity", Some(e)) => {
                    selector_of(row, &na).as_deref() == Some(&expected_selector(row, e))
                        && without_selector(row, &na) == without_selector(row, &oa_stripped)
                }
                ("opencode-recognized", Some(e)) => {
                    let swapped: Vec<String> = oa
                        .iter()
                        .map(|a| {
                            if Some(a.as_str()) == row.model {
                                format!("{a}#{e}")
                            } else {
                                a.clone()
                            }
                        })
                        .collect();
                    na == swapped
                }
                // Delegated/unknown OpenCode, Cursor: legacy value ignored.
                _ => na == oa,
            };
            if !ok {
                failures.push(format!(
                    "{c}: argv differs beyond the named exception\n   old={oa:?}\n   new={na:?}"
                ));
            }
            if !n.out.ok() || !o.out.ok() {
                failures.push(format!(
                    "{c}: run failed old={} new={}: {}",
                    o.out.code,
                    n.out.code,
                    n.out.all()
                ));
            }
        }
    }
    finish("dispatch-legacy", "old-vs-new", total, failures);
}

/// New spellings: valid combinations forward the native selector exactly
/// once; invalid ones fail before any model child is spawned; an old binary
/// cannot even load the store.
#[test]
fn dispatch_of_new_spellings_forwards_valid_requests_once_and_spawns_no_child_for_invalid_ones() {
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in ROWS.iter().filter(|r| r.adapter != "shell") {
        for effort in NEW_ONLY {
            total += 1;
            let c = cell(row, Some(effort));
            let (sb, d) = dispatch(&new, row, Some(effort));
            if authoring_ok(row, effort) {
                let Some(argv) = d.argv else {
                    failures.push(format!(
                        "{c}: valid request spawned no child: {}",
                        d.out.all()
                    ));
                    continue;
                };
                let hits: Vec<_> = argv
                    .iter()
                    .filter(|a| a.as_str() == effort || a.contains("model_reasoning_effort="))
                    .collect();
                let ok = match row.key {
                    "opencode-recognized" => argv
                        .iter()
                        .any(|a| a == &format!("{}#{effort}", row.model.unwrap())),
                    _ => {
                        selector_of(row, &argv).as_deref() == Some(&expected_selector(row, effort))
                            && hits.len() == 1
                    }
                };
                if !ok || !d.out.ok() {
                    failures.push(format!(
                        "{c}: selector not forwarded exactly once (exit {}): {argv:?}",
                        d.out.code
                    ));
                }
            } else if d.argv.is_some() || d.out.ok() {
                failures.push(format!(
                    "{c}: invalid new request must fail before any model child: exit={} child={:?}",
                    d.out.code, d.argv
                ));
            } else if !sb.recorded(cli_of(row.adapter)).is_empty() {
                failures.push(format!("{c}: a model child was recorded"));
            }
        }
    }
    finish("dispatch-new", "new", total, failures);
}

/// Legacy values the new binary must not newly reject: the dispatch still
/// succeeds (with the value ignored) on Cursor, Shell and OpenCode without a
/// recognised model, and says so durably.
#[test]
fn legacy_effort_on_an_adapter_that_cannot_carry_it_is_ignored_with_a_visible_warning_not_rejected()
{
    let new = new_reader();
    let mut failures = Vec::new();
    let mut total = 0;
    for row in ROWS
        .iter()
        .filter(|r| matches!(r.key, "cursor" | "shell" | "opencode-delegated"))
    {
        for effort in LEGACY {
            total += 1;
            let c = cell(row, Some(effort));
            let (_sb, d) = dispatch(&new, row, Some(effort));
            if !d.out.ok() {
                failures.push(format!(
                    "{c}: legacy dispatch newly rejected: {}",
                    d.out.all()
                ));
            } else if !d.out.all().contains("legacy-ignored") {
                failures.push(format!(
                    "{c}: no legacy-ignored warning in output: {}",
                    d.out.all()
                ));
            }
        }
    }
    finish("dispatch-legacy-ignored", "new", total, failures);
}

/// An old binary cannot dispatch a store holding a new spelling; no child.
#[test]
fn old_binary_dispatch_of_a_new_spelling_fails_before_any_child() {
    let Some(old) = old_reader() else { return };
    let mut failures = Vec::new();
    let mut total = 0;
    for row in ROWS.iter().filter(|r| r.adapter != "shell") {
        for effort in NEW_ONLY {
            total += 1;
            let (sb, d) = dispatch(&old, row, Some(effort));
            if d.out.ok() || !sb.recorded(cli_of(row.adapter)).is_empty() {
                failures.push(format!(
                    "{}: old binary dispatched a new spelling: exit {}",
                    cell(row, Some(effort)),
                    d.out.code
                ));
            }
        }
    }
    finish("dispatch-new", "old", total, failures);
}

/// D5-7 / D5-2: a non-AI adapter has no generic effort. The new binary must
/// not silently accept and discard a requested value, nor silently discard a
/// stored one while rewriting the definition.
#[test]
#[ignore = "BUG: shell agents drop effort silently on add/patch/update (validate_candidate skips shell); lead ruling needed, see tests-t1.md"]
fn shell_agent_effort_is_rejected_on_authoring_and_on_unrelated_edit_with_bytes_preserved() {
    let new = new_reader();
    let (sb, id) = seeded(&new, &ROWS[8], Some("high"));
    let before = sb.agent_bytes();
    let edit = sb.run(
        &new,
        &["agent", "patch", &id, "-f", "-"],
        Some(r#"{"role":"Unrelated edit."}"#),
    );
    byte_preserved_rejection(
        &sb,
        &before,
        &edit,
        &id,
        "high",
        "shell/high",
        &mut Vec::new(),
    );
    assert!(
        !edit.ok(),
        "unrelated edit of a shell agent holding effort must be rejected"
    );
    let set = sb.run(
        &new,
        &["agent", "patch", &id, "-f", "-"],
        Some(r#"{"config":{"effort":"max"}}"#),
    );
    assert!(
        !set.ok(),
        "setting effort on a shell agent must be an error, not a silent no-op"
    );
}
