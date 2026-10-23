//! T1 / D2-1 (argv side): the frozen d8c2319 adapter transport oracle.
//!
//! `fixtures/token-eco/baseline-argv/<adapter>.json` holds, for every adapter
//! and every case below, the argv (element count, order, content), cwd, stdin
//! mode, **stdin bytes**, the environment overlay and (Claude) the **bytes of
//! the private system-prompt file** that the d8c2319 production adapters
//! produced. They are frozen: regenerate only from a disposable export of
//! d8c2319 (see `tests-t1.md`).
//!
//! This one source file compiles unchanged in both trees. With
//! `T1_CAPTURE_DIR=<dir>` it *writes* the captures (run it in the d8c2319
//! export); otherwise it *compares* the current adapters against the frozen
//! files. Only the named exceptions of design-v3 D2 may differ, and only as
//! the enumerated insertion:
//!
//! - Codex, explicit `low|medium|high`: `-c model_reasoning_effort="LEVEL"`;
//! - Antigravity, explicit `low|medium|high`: `--effort LEVEL`;
//! - everything else — every case with effort unset, and Claude/Pi/Grok,
//!   Cursor, Shell and OpenCode (unrecognised model) with effort set — is
//!   byte-identical.
//!
//! Claude's system-prompt file path is generated inside the adapter and cannot
//! be injected, so the path is masked as `<system-prompt-file>` (as
//! `adapter_argv.rs` does) and the file's *bytes* are asserted separately.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::ProcessManager;
use orch_domain::{AgentConfig, ReasoningEffort, WorkspaceMode};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use serde_json::{Value, json};
use support::{STDIN_EOF_MARKER, Script, ScriptedProcessManager, fixed_clock, params};

const ADAPTERS: [&str; 8] = [
    "claude",
    "codex",
    "cursor",
    "shell",
    "opencode",
    "pi",
    "grok",
    "antigravity",
];

const SYSTEM: &str = "# System\nYou are a careful agent — «unicode» ✓.\n\n## Rules\n- keep going\n";
const USER: &str = "Task: do the thing.\nLine two with `ticks` and \"quotes\".\n\ttabbed → arrow\n";

fn build(kind: &str, pm: Arc<dyn ProcessManager>) -> Box<dyn Adapter> {
    match kind {
        "claude" => Box::new(ClaudeAdapter::new(pm).with_clock(fixed_clock())),
        "codex" => Box::new(CodexAdapter::new(pm).with_clock(fixed_clock())),
        "cursor" => Box::new(CursorAdapter::new(pm).with_clock(fixed_clock())),
        "shell" => Box::new(ShellAdapter::new(pm).with_clock(fixed_clock())),
        "opencode" => Box::new(OpenCodeAdapter::new(pm).with_clock(fixed_clock())),
        "pi" => Box::new(PiAdapter::new(pm).with_clock(fixed_clock())),
        "grok" => Box::new(GrokAdapter::new(pm).with_clock(fixed_clock())),
        "antigravity" => Box::new(AntigravityAdapter::new(pm).with_clock(fixed_clock())),
        other => panic!("unknown adapter {other}"),
    }
}

/// `(case name, params, effort)` — the effort is repeated so the comparator
/// does not have to dig it out of the config. Efforts are limited to the
/// three that exist at d8c2319 (this file must compile there).
fn cases() -> Vec<(String, ExecuteParams, Option<ReasoningEffort>)> {
    let shell = |mut c: AgentConfig| {
        c.command = Some("npm test".to_owned());
        c
    };
    let mut out = Vec::new();
    let mut add = |name: &str, user: &str, system: Option<&str>, config: AgentConfig| {
        let effort = config.effort;
        let mut p = params(user, "/ws");
        p.system_prompt = system.map(str::to_owned);
        p.config = shell(config);
        out.push((name.to_owned(), p, effort));
    };

    add("minimal", "the user prompt", None, AgentConfig::default());
    add(
        "full-effort-high",
        "the user prompt",
        Some("the system prompt"),
        AgentConfig {
            model: Some("a-model".into()),
            effort: Some(ReasoningEffort::High),
            max_turns: Some(7),
            system_prompt: Some("the agent prompt".into()),
            workspace_mode: Some(WorkspaceMode::Worktree),
            ..Default::default()
        },
    );
    add(
        "agent-system-prompt-only",
        "the user prompt",
        None,
        AgentConfig {
            system_prompt: Some("the agent prompt".into()),
            ..Default::default()
        },
    );
    add(
        "rich-prompts-no-effort",
        USER,
        Some(SYSTEM),
        AgentConfig {
            model: Some("a-model".into()),
            max_turns: Some(3),
            ..Default::default()
        },
    );
    add(
        "rich-prompts-no-effort-no-model",
        USER,
        Some(SYSTEM),
        AgentConfig::default(),
    );
    for (name, level) in [
        ("low", ReasoningEffort::Low),
        ("medium", ReasoningEffort::Medium),
        ("high", ReasoningEffort::High),
    ] {
        add(
            &format!("effort-{name}-model"),
            USER,
            Some(SYSTEM),
            AgentConfig {
                model: Some("a-model".into()),
                effort: Some(level),
                ..Default::default()
            },
        );
        add(
            &format!("effort-{name}-no-model"),
            USER,
            Some(SYSTEM),
            AgentConfig {
                effort: Some(level),
                ..Default::default()
            },
        );
    }
    // Native model suffixes the adapters must keep opaque.
    add(
        "opaque-suffixed-model-no-effort",
        USER,
        Some(SYSTEM),
        AgentConfig {
            model: Some("opaque/model:high#thinking".into()),
            ..Default::default()
        },
    );
    out
}

async fn capture(kind: &str, p: ExecuteParams) -> Value {
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin());
    let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
    let handle = adapter.execute(p).expect("execute");

    let record = pm.spawn_record();
    // Claude: the system prompt travels in a private file whose path is random.
    let mut args = record.args.clone();
    let mut system_file = Value::Null;
    if let Some(at) = args.iter().position(|a| a == "--system-prompt-file") {
        let path = args[at + 1].clone();
        system_file = Value::String(std::fs::read_to_string(&path).expect("system file"));
        args[at + 1] = "<system-prompt-file>".to_owned();
    }
    let stdin_mode = format!("{:?}", record.stdin);
    let stdin_bytes = if stdin_mode == "Pipe" {
        pm.stdin_capture().await.map(|s| {
            s.strip_suffix(STDIN_EOF_MARKER)
                .map(str::to_owned)
                .unwrap_or(s)
        })
    } else {
        None
    };
    drop(handle);
    json!({
        "command": record.command,
        "argv": args,
        "cwd": record.cwd.display().to_string(),
        "stdin_mode": stdin_mode,
        "stdin_bytes": stdin_bytes,
        "system_prompt_file": system_file,
        "env": record.env,
    })
}

async fn capture_adapter(kind: &str) -> Value {
    let mut map = serde_json::Map::new();
    for (name, p, _) in cases() {
        map.insert(name, capture(kind, p).await);
    }
    Value::Object(map)
}

fn frozen_path(kind: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/token-eco/baseline-argv")
        .join(format!("{kind}.json"))
}

/// Run only in a disposable d8c2319 export: `T1_CAPTURE_DIR=<dir> cargo test …`.
#[tokio::test]
async fn capture_frozen_baseline_when_asked() {
    let Ok(dir) = std::env::var("T1_CAPTURE_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    for kind in ADAPTERS {
        let value = capture_adapter(kind).await;
        let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{kind}.json")),
            text,
        )
        .unwrap();
    }
}

fn frozen(kind: &str) -> Value {
    let text = std::fs::read_to_string(frozen_path(kind))
        .unwrap_or_else(|e| panic!("frozen capture for {kind} missing: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// The single approved insertion for `(adapter, effort)`, `None` when the
/// argv must be byte-identical to d8c2319.
fn approved_insertion(kind: &str, effort: Option<ReasoningEffort>) -> Option<Vec<String>> {
    let level = effort?.as_str().to_owned();
    match kind {
        "codex" => Some(vec![
            "-c".to_owned(),
            format!("model_reasoning_effort=\"{level}\""),
        ]),
        "antigravity" => Some(vec!["--effort".to_owned(), level]),
        _ => None,
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

/// Remove exactly one occurrence of the `pair` (adjacent, in order).
fn without_pair(argv: &[String], pair: &[String]) -> Option<Vec<String>> {
    let hits: Vec<usize> = (0..argv.len().saturating_sub(1))
        .filter(|&i| argv[i] == pair[0] && argv[i + 1] == pair[1])
        .collect();
    if hits.len() != 1 {
        return None;
    }
    let mut out = argv.to_vec();
    out.drain(hits[0]..hits[0] + 2);
    Some(out)
}

#[tokio::test]
async fn every_adapter_matches_the_frozen_baseline_except_the_named_effort_insertions() {
    if std::env::var("T1_CAPTURE_DIR").is_ok() {
        return;
    }
    let mut checked = 0;
    for kind in ADAPTERS {
        let baseline = frozen(kind);
        for (name, p, effort) in cases() {
            let want = &baseline[&name];
            assert!(!want.is_null(), "{kind}/{name}: no frozen case");
            let got = capture(kind, p).await;

            for field in [
                "command",
                "cwd",
                "stdin_mode",
                "stdin_bytes",
                "system_prompt_file",
                "env",
            ] {
                assert_eq!(got[field], want[field], "{kind}/{name}: {field} drifted");
            }
            let got_argv = strings(&got["argv"]);
            let want_argv = strings(&want["argv"]);
            match approved_insertion(kind, effort) {
                None => assert_eq!(
                    got_argv, want_argv,
                    "{kind}/{name}: argv must be byte-identical to d8c2319"
                ),
                Some(pair) => {
                    assert_eq!(
                        without_pair(&got_argv, &pair).as_deref(),
                        Some(want_argv.as_slice()),
                        "{kind}/{name}: argv may differ from d8c2319 only by one {pair:?} \
                         insertion; got {got_argv:?}"
                    );
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, ADAPTERS.len() * cases().len());
}

#[test]
fn the_frozen_fixtures_cover_every_adapter_and_case() {
    if std::env::var("T1_CAPTURE_DIR").is_ok() {
        return;
    }
    let names: Vec<String> = cases().into_iter().map(|(n, _, _)| n).collect();
    for kind in ADAPTERS {
        let baseline = frozen(kind);
        let have: Vec<&String> = baseline.as_object().unwrap().keys().collect();
        assert_eq!(have.len(), names.len(), "{kind}: case count");
        for n in &names {
            assert!(baseline.get(n).is_some(), "{kind}: missing frozen case {n}");
        }
    }
}

#[test]
fn the_oracle_covers_stdin_bytes_and_the_claude_system_file() {
    if std::env::var("T1_CAPTURE_DIR").is_ok() {
        return;
    }
    // Guard the oracle itself: a capture with no stdin bytes or no file would
    // make the byte comparison vacuous.
    let claude = frozen("claude");
    let rich = &claude["rich-prompts-no-effort"];
    assert_eq!(rich["system_prompt_file"], SYSTEM);
    assert_eq!(rich["stdin_bytes"], USER);
    let codex = frozen("codex");
    assert!(
        codex["rich-prompts-no-effort"]["stdin_bytes"]
            .as_str()
            .is_some_and(|s| s.contains("Task: do the thing.")),
        "codex prompt travels on stdin"
    );
}
