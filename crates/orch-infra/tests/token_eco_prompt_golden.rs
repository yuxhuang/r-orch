//! Eco prompt goldens (`fixtures/prompts-eco/`). The normal goldens in
//! `fixtures/prompts/` stay immutable; these pin the eco layout byte for byte.
//!
//! Regenerate deliberately with `T1_BLESS_ECO=1 cargo test -p orch-infra
//! --test token_eco_prompt_golden` and review the diff by eye: every
//! obligation (CLI reference, own role, skills before snapshots, lookups,
//! concise hint, untrusted markers) must still be present.

#[path = "token_eco_cases/mod.rs"]
mod cases;

use std::fs;
use std::path::PathBuf;

use cases::Case;
use orch_core::template::GoalTaskRow;
use orch_domain::TaskStatus;
use orch_infra::template::{
    BuildPromptOptions, LiquidTemplateEngine, build_eco_prompt_context, render_prompt,
};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/prompts-eco")
}

fn case(name: &str) -> Case {
    cases::all_cases()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap()
}

fn with_source(mut c: Case) -> Case {
    c.retry.as_mut().unwrap().source_run_id = Some("run_src001".into());
    c
}

fn rows(n: usize) -> Vec<GoalTaskRow> {
    (0..n)
        .map(|i| GoalTaskRow {
            id: format!("tsk_{i:03}"),
            status: if i % 3 == 0 {
                TaskStatus::Done
            } else {
                TaskStatus::Todo
            },
            title: format!("Linked task number {i}"),
        })
        .collect()
}

/// `(golden name, case)`.
fn golden_cases() -> Vec<(&'static str, Case)> {
    let mut goal_preview = case("goal-with-tasks-and-progress");
    goal_preview.task.goal_id = Some("goal_x1".into());
    goal_preview.task.depends_on = vec!["tsk_041".into(), "tsk_002".into()];
    goal_preview.goal.as_mut().unwrap().task_rows = rows(45);

    let mut everything = with_source(case("everything-at-once"));
    everything.retry.as_mut().unwrap().previous_output = format!(
        "{}CLUE-IN-THE-MIDDLE{}",
        cases::big(9_000, "head"),
        cases::big(9_000, "tail")
    );
    everything.task.depends_on = vec!["tsk_001".into()];
    everything.goal.as_mut().unwrap().task_rows = rows(6);

    vec![
        ("minimal", case("minimal-default-templates")),
        (
            "roster-remote-unicode-liquid",
            case("roster-remote-caps-unicode"),
        ),
        ("roster-28-agents", case("roster-28-agents")),
        ("skills-before-snapshots", {
            let mut c = case("shared-context-goal-and-scope-relevance");
            c.skills = Some(cases::SKILLS_BLOCK.into());
            c
        }),
        (
            "retry-clipped-with-source-run",
            with_source(case("retry-attempt-2-large-output-full-in-normal")),
        ),
        ("retry-no-source-keeps-full-output", {
            let mut c = case("retry-attempt-2-large-output-full-in-normal");
            c.retry.as_mut().unwrap().previous_output = cases::big(9_000, "o");
            c
        }),
        ("feedback-and-messages", case("feedback-and-messages")),
        ("goal-preview-45-tasks", goal_preview),
        ("autonomous-goal", case("autonomous-goal")),
        ("everything", everything),
        (
            "hostile-text-in-every-field",
            case("hostile-text-in-every-field"),
        ),
    ]
}

async fn render(c: &Case) -> (String, String) {
    let ctx = build_eco_prompt_context(
        &c.task,
        &c.agent,
        c.attempt,
        &c.workspace,
        &c.config,
        BuildPromptOptions {
            all_agents: &c.agents,
            retry_context: c.retry.clone(),
            shared_context: c.shared.as_ref(),
            feedback: c.feedback.clone(),
            messages: &c.messages,
            goal: c.goal.clone(),
        },
    );
    let r = render_prompt(
        &LiquidTemplateEngine::new(),
        None,
        &ctx,
        c.skills.as_deref(),
    )
    .await
    .unwrap();
    (r.system_prompt.unwrap(), r.prompt)
}

#[tokio::test]
async fn eco_prompts_match_their_goldens_byte_for_byte() {
    let bless = std::env::var_os("T1_BLESS_ECO").is_some();
    fs::create_dir_all(dir()).unwrap();
    let mut failures = Vec::new();
    for (name, c) in golden_cases() {
        let (system, user) = render(&c).await;
        for (suffix, got) in [("system", &system), ("user", &user)] {
            let path = dir().join(format!("{name}.{suffix}.txt"));
            if bless {
                fs::write(&path, got).unwrap();
                continue;
            }
            match fs::read_to_string(&path) {
                Ok(want) if &want == got => {}
                Ok(_) => failures.push(format!("{name}.{suffix}: differs from golden")),
                Err(e) => failures.push(format!("{name}.{suffix}: {e}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_eco_golden_file_belongs_to_a_current_case() {
    if std::env::var_os("T1_BLESS_ECO").is_some() {
        return;
    }
    let mut on_disk: Vec<String> = fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut wanted: Vec<String> = golden_cases()
        .iter()
        .flat_map(|(n, _)| [format!("{n}.system.txt"), format!("{n}.user.txt")])
        .collect();
    wanted.sort();
    assert_eq!(on_disk, wanted);
}
