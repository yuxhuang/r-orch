//! D2-1 (production seam) — the **current** production render path
//! (`render_prompt`) must reproduce, for a normal-mode context, exactly the
//! bytes frozen from the pre-eco builder at `d8c2319`
//! (`fixtures/token-eco/baseline-prompts/`, captured by
//! `token_eco_prompt_baseline.rs`). Also pins that the eco builder leaves every
//! normal field untouched and that the normal builder ignores prompt mode.

#[path = "token_eco_cases/mod.rs"]
mod cases;

use std::fs;
use std::path::PathBuf;

use orch_infra::template::{
    BuildPromptOptions, LiquidTemplateEngine, build_eco_prompt_context, render_prompt,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Frozen {
    user: String,
    system: Option<String>,
}

fn frozen(name: &str) -> Frozen {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/token-eco/baseline-prompts")
        .join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
}

fn options(case: &cases::Case) -> BuildPromptOptions<'_> {
    BuildPromptOptions {
        all_agents: &case.agents,
        retry_context: case.retry.clone(),
        shared_context: case.shared.as_ref(),
        feedback: case.feedback.clone(),
        messages: &case.messages,
        goal: case.goal.clone(),
    }
}

#[tokio::test]
async fn render_prompt_reproduces_every_frozen_normal_prompt_byte_for_byte() {
    let engine = LiquidTemplateEngine::new();
    let mut differing = Vec::new();
    for case in cases::all_cases() {
        let expected = frozen(case.name);
        let context = cases::build_context(&case);
        assert!(
            context.eco.is_none(),
            "{}: normal context has eco",
            case.name
        );
        let got = render_prompt(
            &engine,
            Some(&case.prompt),
            &context,
            case.skills.as_deref(),
        )
        .await
        .unwrap();
        if got.prompt != expected.user || got.system_prompt != expected.system {
            differing.push(case.name);
        }
    }
    assert_eq!(differing, Vec::<&str>::new());
}

#[tokio::test]
async fn render_prompt_with_no_prompt_config_equals_the_default_config() {
    let engine = LiquidTemplateEngine::new();
    let case = &cases::all_cases()[0];
    let context = cases::build_context(case);
    let none = render_prompt(&engine, None, &context, None).await.unwrap();
    let default = render_prompt(&engine, Some(&Default::default()), &context, None)
        .await
        .unwrap();
    assert_eq!(none, default);
    assert_eq!(none.prompt, frozen(case.name).user);
}

#[test]
fn the_normal_context_builder_ignores_the_agents_prompt_mode() {
    use orch_domain::PromptMode;
    for case in cases::all_cases() {
        let plain = serde_json::to_value(cases::build_context(&case)).unwrap();
        for mode in [PromptMode::Normal, PromptMode::Eco] {
            let mut moded = cases::all_cases()
                .into_iter()
                .find(|c| c.name == case.name)
                .unwrap();
            moded.agent.config.prompt_mode = Some(mode);
            for a in &mut moded.agents {
                a.config.prompt_mode = Some(mode);
            }
            assert_eq!(
                serde_json::to_value(cases::build_context(&moded)).unwrap(),
                plain,
                "{} under {mode}",
                case.name
            );
        }
    }
}

#[test]
fn the_eco_context_builder_changes_nothing_but_the_eco_field() {
    for case in cases::all_cases() {
        let normal = cases::build_context(&case);
        let eco = build_eco_prompt_context(
            &case.task,
            &case.agent,
            case.attempt,
            &case.workspace,
            &case.config,
            options(&case),
        );
        assert!(eco.eco.is_some(), "{}", case.name);
        let mut stripped = eco;
        stripped.eco = None;
        assert_eq!(stripped, normal, "{}", case.name);
    }
}

#[test]
fn the_normal_builder_serialization_has_no_eco_key_so_templates_cannot_see_one() {
    for case in cases::all_cases() {
        let value = serde_json::to_value(cases::build_context(&case)).unwrap();
        assert!(value.get("eco").is_none(), "{}", case.name);
    }
}
