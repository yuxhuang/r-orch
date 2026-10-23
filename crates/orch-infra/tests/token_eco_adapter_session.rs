//! T1 / D7-1, D7-2 and the default `prepare_effort` identity.
//!
//! D7: every dispatch is a fresh process and a fresh CLI conversation. No
//! adapter may add a resume/continue/session selector, whatever stale provider
//! state, captured thread/session IDs or environment hints exist, and nothing
//! from one run (a nonce in a prompt, a thread id in its output) may reach the
//! next.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::ProcessManager;
use orch_domain::{AgentConfig, ReasoningEffort as E};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use support::{
    STDIN_EOF_MARKER, Script, ScriptedProcessManager, SpawnRecord, collect, fixed_clock, params,
};

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

/// Selectors that would continue earlier provider state. (`-c` is Codex's
/// config override and is checked separately for exactness.)
const FORBIDDEN: [&str; 17] = [
    "--resume",
    "-r",
    "--continue",
    "--session",
    "--session-id",
    "--sessions",
    "--last",
    "--thread",
    "--thread-id",
    "--conversation",
    "--fork-session",
    "--resume-session-at",
    "--chat",
    "--chat-id",
    "resume",
    "continue",
    "--no-session-persistence-off",
];

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

/// A workspace littered with stale provider session state.
fn stale_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".claude/projects/p/STALE-SESSION-1.jsonl", "{}"),
        (
            ".codex/sessions/2026/10/21/rollout-STALE-THREAD-2.jsonl",
            "{}",
        ),
        (".opencode/sessions/STALE-SESSION-3.json", "{}"),
        (".pi/sessions/STALE-SESSION-4.json", "{}"),
        (".grok/sessions/STALE-SESSION-5.json", "{}"),
        (".agy/conversations/STALE-CONVERSATION-6.json", "{}"),
        ("session.json", "{\"session_id\":\"STALE-SESSION-7\"}"),
    ] {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    dir
}

fn config_for(kind: &str) -> AgentConfig {
    AgentConfig {
        command: Some("npm test".into()),
        model: match kind {
            "claude" => Some("claude-opus-5-5".into()),
            "codex" => Some("gpt-6.1-sol".into()),
            _ => None,
        },
        ..Default::default()
    }
}

async fn spawn(
    kind: &str,
    workspace: &std::path::Path,
    prompt: &str,
    script: Script,
    env: &[(&str, &str)],
) -> (SpawnRecord, String, support::Collected) {
    let pm = ScriptedProcessManager::new(script.capturing_stdin());
    let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
    let mut p: ExecuteParams = params(prompt, workspace.to_str().unwrap());
    p.system_prompt = Some("system text".into());
    p.config = config_for(kind);
    for (k, v) in env {
        p.env.insert((*k).to_owned(), (*v).to_owned());
    }
    let handle = adapter.execute(p).expect("execute");
    let record = pm.spawn_record();
    let stdin = if format!("{:?}", record.stdin) == "Pipe" {
        pm.stdin_capture()
            .await
            .map(|s| {
                s.strip_suffix(STDIN_EOF_MARKER)
                    .map(str::to_owned)
                    .unwrap_or(s)
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let collected = collect(handle).await;
    (record, stdin, collected)
}

fn assert_no_continuation(kind: &str, rec: &SpawnRecord) {
    for arg in &rec.args {
        assert!(
            !FORBIDDEN.contains(&arg.as_str()),
            "{kind}: continuation selector {arg:?} in {:?}",
            rec.args
        );
        for stale in ["STALE-", "SESSION-ID", "THREAD-ID"] {
            assert!(!arg.contains(stale), "{kind}: stale id in argv: {arg:?}");
        }
    }
    // Codex: the subcommand is `exec`, never `exec resume`.
    if kind == "codex" {
        assert_eq!(rec.args[0], "exec");
        assert_ne!(rec.args.get(1).map(String::as_str), Some("resume"));
        // And with no effort configured there is no `-c` at all.
        assert!(!rec.args.iter().any(|a| a == "-c"), "{:?}", rec.args);
    }
}

#[tokio::test]
async fn no_adapter_adds_a_resume_continue_or_session_selector_despite_stale_provider_state() {
    let ws = stale_workspace();
    // Hints in the environment overlay must be passed through verbatim and
    // never turned into arguments.
    let env = [
        ("CLAUDE_SESSION_ID", "STALE-SESSION-ID-A"),
        ("CODEX_THREAD_ID", "STALE-THREAD-ID-B"),
        ("OPENCODE_SESSION", "STALE-SESSION-ID-C"),
    ];
    for kind in ADAPTERS {
        let (rec, stdin, _) = spawn(kind, ws.path(), "fresh prompt", Script::new(), &env).await;
        assert_no_continuation(kind, &rec);
        assert_eq!(
            rec.cwd,
            ws.path(),
            "{kind}: runs in the workspace it was given"
        );
        assert!(
            !stdin.contains("STALE-"),
            "{kind}: stale id on stdin: {stdin}"
        );
        // The overlay is exactly what the caller supplied: no added session env.
        let added: Vec<_> = rec
            .env
            .keys()
            .filter(|k| !env.iter().any(|(e, _)| e == k) && k.to_lowercase().contains("session"))
            .collect();
        assert!(
            added.is_empty(),
            "{kind}: adapter added session env {added:?}"
        );
    }
}

#[tokio::test]
async fn a_thread_or_session_id_in_one_runs_output_never_reaches_the_next_run() {
    let ws = stale_workspace();
    let script = || {
        Script::lines([
            r#"{"type":"thread.started","thread_id":"CAPTURED-THREAD-9"}"#,
            r#"{"type":"system","subtype":"init","session_id":"CAPTURED-SESSION-9"}"#,
            r#"{"type":"step_start","sessionID":"CAPTURED-SESSION-9"}"#,
            r#"{"type":"result","result":"NONCE-FROM-RUN-ONE","session_id":"CAPTURED-SESSION-9"}"#,
        ])
    };
    for kind in ADAPTERS {
        let (first, _, _) = spawn(kind, ws.path(), "first NONCE-FROM-RUN-ONE", script(), &[]).await;
        assert_no_continuation(kind, &first);

        let (second, stdin, _) = spawn(kind, ws.path(), "second prompt", Script::new(), &[]).await;
        assert_no_continuation(kind, &second);
        let everything = format!("{:?} {stdin} {:?}", second.args, second.env);
        assert!(
            !everything.contains("CAPTURED-"),
            "{kind}: id carried over: {everything}"
        );
        assert!(
            !everything.contains("NONCE-FROM-RUN-ONE"),
            "{kind}: nonce carried over"
        );
    }
}

#[tokio::test]
async fn one_adapter_instance_used_for_two_tasks_keeps_no_conversation_between_them() {
    let ws_a = stale_workspace();
    let ws_b = stale_workspace();
    for kind in ADAPTERS {
        let pm = ScriptedProcessManager::new(Script::new().capturing_stdin());
        let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
        for (ws, prompt) in [(&ws_a, "task A NONCE-A"), (&ws_b, "task B")] {
            let mut p = params(prompt, ws.path().to_str().unwrap());
            p.config = config_for(kind);
            let _ = adapter.execute(p).expect("execute");
        }
        let spawns = pm.spawns();
        assert_eq!(spawns.len(), 2, "{kind}");
        assert_eq!(
            spawns[0].args.len(),
            spawns[1].args.len(),
            "{kind}: argv shape changed between runs"
        );
        for rec in &spawns {
            assert_no_continuation(kind, rec);
        }
        // A workspace change is honoured, not pinned to the first run's.
        assert_eq!(spawns[0].cwd, ws_a.path(), "{kind}");
        assert_eq!(spawns[1].cwd, ws_b.path(), "{kind}");
        // A prompt that mentions NONCE-A is only ever in the first spawn.
        let second = format!("{:?} {:?}", spawns[1].args, spawns[1].env);
        assert!(
            !second.contains("NONCE-A"),
            "{kind}: previous task leaked: {second}"
        );
    }
}

#[tokio::test]
async fn concurrent_runs_on_one_adapter_each_carry_only_their_own_prompt() {
    for kind in [
        "claude",
        "codex",
        "opencode",
        "grok",
        "antigravity",
        "cursor",
    ] {
        let pm = ScriptedProcessManager::new(Script::new().capturing_stdin());
        let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
        let mut handles = Vec::new();
        for n in 0..4 {
            let mut p = params(&format!("CONCURRENT-PROMPT-{n}-END"), "/ws");
            p.config = config_for(kind);
            handles.push(adapter.execute(p).expect("execute"));
        }
        let spawns = pm.spawns();
        assert_eq!(spawns.len(), 4);
        for (n, rec) in spawns.iter().enumerate() {
            assert_no_continuation(kind, rec);
            let others: Vec<_> = (0..4).filter(|m| *m != n).collect();
            let joined = rec.args.join(" ");
            for m in others {
                assert!(
                    !joined.contains(&format!("CONCURRENT-PROMPT-{m}-END")),
                    "{kind}: spawn {n} carries prompt {m}"
                );
            }
        }
        drop(handles);
    }
}

// ── the default prepare_effort hook is identity and performs no I/O ─────────

#[tokio::test]
async fn the_default_prepare_effort_is_identity_and_spawns_nothing_for_every_non_opencode_adapter()
{
    // Even for an effort that execute would reject: the hook never validates.
    for kind in ADAPTERS.into_iter().filter(|k| *k != "opencode") {
        let pm = ScriptedProcessManager::silent();
        let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
        let mut p = params("prompt", "/ws");
        p.config = AgentConfig {
            model: Some("any/model:high#native".into()),
            effort: Some(E::Ultra),
            command: Some("npm test".into()),
            ..Default::default()
        };
        let prepared = adapter
            .prepare_effort(&p)
            .await
            .expect("identity never fails");
        assert_eq!(
            serde_json::to_value(&prepared.config).unwrap(),
            serde_json::to_value(&p.config).unwrap(),
            "{kind}"
        );
        assert!(prepared.diagnostics.is_empty(), "{kind}");
        assert!(
            pm.spawns().is_empty(),
            "{kind}: the default hook spawned a process"
        );
        assert!(pm.killed().is_empty());
    }
}

#[tokio::test]
async fn opencode_prepare_effort_spawns_nothing_when_effort_is_unset_or_statically_invalid() {
    let model = "openrouter/anthropic/claude-opus-5.5";
    for (config, label) in [
        (
            AgentConfig {
                model: Some(model.into()),
                ..Default::default()
            },
            "unset",
        ),
        // New value, unknown model: rejected from the table before any probe.
        (
            AgentConfig {
                model: Some("unlisted/model".into()),
                effort: Some(E::Max),
                ..Default::default()
            },
            "new value on unknown model",
        ),
        // Legacy value on a delegated model: ignored from the table, no probe.
        (
            AgentConfig {
                effort: Some(E::High),
                ..Default::default()
            },
            "legacy on delegated",
        ),
        // Conflicting suffix: static.
        (
            AgentConfig {
                model: Some(format!("{model}#high")),
                effort: Some(E::Xhigh),
                ..Default::default()
            },
            "suffix conflict",
        ),
    ] {
        let pm = ScriptedProcessManager::silent();
        let adapter = OpenCodeAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>);
        let mut p = params("prompt", "/ws");
        p.config = config;
        let _ = adapter.prepare_effort(&p).await;
        assert!(
            pm.spawns().is_empty(),
            "{label}: a static decision must not probe"
        );
    }
}
