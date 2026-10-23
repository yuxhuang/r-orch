//! T1 / D2-2: normal vs eco pairing at the adapter boundary.
//!
//! The same task + agent is rendered by the **production renderer**
//! (`orch_infra::template::render_prompt`) once as normal and once as eco and
//! each result is handed to every adapter with an identical resolved model and
//! effort. Mode is not an adapter input, so the argv may differ only in the
//! elements that *carry the prompt text*; every selector, model, flag and the
//! environment overlay must be equal, including each approved effort delta.

mod support;

use std::sync::Arc;

use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::ProcessManager;
use orch_core::template::TemplateEngine;
use orch_core::test_support::{make_agent, make_task};
use orch_domain::{AgentConfig, ReasoningEffort as E};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use orch_infra::template::{
    BuildPromptOptions, LiquidTemplateEngine, build_eco_prompt_context, build_prompt_context,
    render_prompt,
};
use support::{STDIN_EOF_MARKER, Script, ScriptedProcessManager, SpawnRecord, fixed_clock, params};

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

struct Rendered {
    system: String,
    user: String,
}

async fn render_both() -> (Rendered, Rendered) {
    let agent = make_agent();
    let helper = orch_domain::Agent {
        id: "agt_helper".into(),
        name: "Helper".into(),
        role: Some("Reviews the work.\n\nLonger prose that normal mode prints in full.".into()),
        ..make_agent()
    };
    let task = make_task();
    let config = orch_domain::default_config();
    let all = [agent.clone(), helper];
    let options = || BuildPromptOptions {
        all_agents: &all,
        ..Default::default()
    };
    let engine: &dyn TemplateEngine = &LiquidTemplateEngine::new();
    let normal_ctx = build_prompt_context(&task, &agent, 1, "/tmp/ws", &config, options());
    let eco_ctx = build_eco_prompt_context(&task, &agent, 1, "/tmp/ws", &config, options());
    let n = render_prompt(engine, None, &normal_ctx, None)
        .await
        .unwrap();
    let e = render_prompt(engine, None, &eco_ctx, None).await.unwrap();
    (
        Rendered {
            system: n.system_prompt.expect("normal has a system half"),
            user: n.prompt,
        },
        Rendered {
            system: e.system_prompt.expect("eco has a system half"),
            user: e.prompt,
        },
    )
}

async fn spawn(kind: &str, r: &Rendered, config: AgentConfig) -> (SpawnRecord, Option<String>) {
    let pm = ScriptedProcessManager::new(Script::new().capturing_stdin());
    let adapter = build(kind, Arc::clone(&pm) as Arc<dyn ProcessManager>);
    let mut p: ExecuteParams = params(&r.user, "/ws");
    p.system_prompt = Some(r.system.clone());
    p.config = AgentConfig {
        command: Some("npm test".into()),
        ..config
    };
    let handle = adapter.execute(p).expect("execute");
    let record = pm.spawn_record();
    let mut args = record.args.clone();
    // Claude: the system prompt travels as a private file; read it back.
    let mut file = None;
    if let Some(at) = args.iter().position(|a| a == "--system-prompt-file") {
        file = std::fs::read_to_string(&args[at + 1]).ok();
        args[at + 1] = "<system-prompt-file>".into();
    }
    let stdin = if format!("{:?}", record.stdin) == "Pipe" {
        pm.stdin_capture().await.map(|s| {
            s.strip_suffix(STDIN_EOF_MARKER)
                .map(str::to_owned)
                .unwrap_or(s)
        })
    } else {
        None
    };
    drop(handle);
    let mut record = record;
    record.args = args;
    // For the test's purposes the file contents are another stdin-like channel.
    (record, stdin.or(file))
}

fn carries(arg: &str, r: &Rendered) -> bool {
    let head = |s: &str| s.chars().take(60).collect::<String>();
    arg.contains(&head(&r.system)) || arg.contains(&head(&r.user))
}

/// `(adapter, model, efforts)` — efforts are the explicit requests exercised
/// for the adapter, the unset case being added to every row.
fn rows() -> Vec<(&'static str, Option<&'static str>, Vec<E>)> {
    vec![
        (
            "claude",
            Some("claude-opus-5-5"),
            vec![E::Low, E::High, E::Xhigh, E::Max],
        ),
        (
            "codex",
            Some("gpt-6-astra"),
            vec![E::Low, E::High, E::Xhigh, E::Max, E::Ultra],
        ),
        (
            "pi",
            Some("openai-codex/gpt-6.1-sol"),
            vec![E::Off, E::Minimal, E::High, E::Max],
        ),
        ("grok", Some("grok-build"), vec![E::Low, E::High]),
        (
            "antigravity",
            Some("gemini-4-argon"),
            vec![E::Low, E::Medium, E::High],
        ),
        (
            "opencode",
            Some("openrouter/anthropic/claude-opus-5.5"),
            vec![E::Low, E::Max],
        ),
        ("cursor", Some("auto"), vec![]),
        ("shell", None, vec![]),
    ]
}

#[tokio::test]
async fn normal_and_eco_prompts_differ_in_text_but_share_every_non_prompt_argv_element() {
    let (normal, eco) = render_both().await;
    assert_ne!(normal.system, eco.system, "the pair must actually differ");
    assert_ne!(normal.user, eco.user);

    let mut compared = 0;
    for (kind, model, efforts) in rows() {
        let mut cases: Vec<Option<E>> = vec![None];
        cases.extend(efforts.into_iter().map(Some));
        for effort in cases {
            let cfg = || AgentConfig {
                model: model.map(str::to_owned),
                effort,
                ..Default::default()
            };
            let (n, n_in) = spawn(kind, &normal, cfg()).await;
            let (e, e_in) = spawn(kind, &eco, cfg()).await;
            let ctx = format!("{kind} model={model:?} effort={effort:?}");

            assert_eq!(n.command, e.command, "{ctx}");
            assert_eq!(n.args.len(), e.args.len(), "{ctx}: argv length");
            for (i, (a, b)) in n.args.iter().zip(&e.args).enumerate() {
                if a != b {
                    assert!(
                        carries(a, &normal) && carries(b, &eco),
                        "{ctx}: argv[{i}] differs but is not prompt text: {a:?} vs {b:?}"
                    );
                }
            }
            assert_eq!(n.cwd, e.cwd, "{ctx}");
            assert_eq!(format!("{:?}", n.stdin), format!("{:?}", e.stdin), "{ctx}");
            assert_eq!(
                n.env.keys().collect::<Vec<_>>(),
                e.env.keys().collect::<Vec<_>>(),
                "{ctx}: env keys"
            );
            // The prompt text really did reach the child in each mode.
            let reached = |rec: &SpawnRecord, side: &Option<String>, r: &Rendered| {
                rec.args.iter().any(|a| carries(a, r))
                    || rec.env.values().any(|v| carries(v, r))
                    || side.as_deref().is_some_and(|s| carries(s, r))
            };
            assert!(reached(&n, &n_in, &normal), "{ctx}: normal prompt lost");
            assert!(reached(&e, &e_in, &eco), "{ctx}: eco prompt lost");
            compared += 1;
        }
    }
    // 8 adapters × (1 unset + explicit efforts).
    assert_eq!(compared, 5 + 6 + 5 + 3 + 4 + 3 + 1 + 1);
}

#[tokio::test]
async fn the_effort_deltas_are_identical_in_both_modes_and_exactly_the_enumerated_ones() {
    let (normal, eco) = render_both().await;
    for (kind, model, efforts) in rows() {
        for effort in efforts {
            let mut seen = Vec::new();
            for r in [&normal, &eco] {
                let (rec, _) = spawn(
                    kind,
                    r,
                    AgentConfig {
                        model: model.map(str::to_owned),
                        effort: Some(effort),
                        ..Default::default()
                    },
                )
                .await;
                let mut selectors = Vec::new();
                for (i, a) in rec.args.iter().enumerate() {
                    if matches!(a.as_str(), "--effort" | "--thinking") {
                        selectors.push(format!("{a} {}", rec.args[i + 1]));
                    } else if a.starts_with("model_reasoning_effort=") {
                        selectors.push(format!("{} {a}", rec.args[i - 1]));
                    } else if i > 0 && rec.args[i - 1] == "--model" && a.contains('#') {
                        selectors.push(format!("--model {a}"));
                    }
                }
                seen.push(selectors);
            }
            assert_eq!(
                seen[0], seen[1],
                "{kind} {effort}: mode changed the effort request"
            );
            assert_eq!(
                seen[0].len(),
                1,
                "{kind} {effort}: exactly one selector: {:?}",
                seen[0]
            );
        }
    }
}
