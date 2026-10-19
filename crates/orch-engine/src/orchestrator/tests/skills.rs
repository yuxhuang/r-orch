//! Library skills are appended to the **system** prompt, which is the half
//! that stays cacheable between runs. MCP skills are the Claude CLI's job, so
//! the loader resolves nothing for them — and the orchestrator still hands it
//! the full list, because deciding what to skip is the loader's call.

use orch_core::test_support::{FakeExecuteCall, make_agent, make_task};
use orch_domain::{Agent, OrchestratorConfig, PromptConfig, Task};

use super::harness::{Harness, test_config};

fn agent_with_skills(skills: Option<Vec<&str>>) -> Agent {
    let mut agent = make_agent();
    agent.config.skills = skills.map(|s| s.into_iter().map(str::to_owned).collect());
    agent
}

/// Dispatch one task and return the adapter call it produced.
async fn dispatch(h: &Harness) -> FakeExecuteCall {
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;
    h.adapter
        .execute_calls
        .last()
        .expect("the adapter was executed")
}

fn config_with_legacy_template(template: &str) -> OrchestratorConfig {
    OrchestratorConfig {
        prompt: Some(PromptConfig {
            template: Some(template.to_owned()),
            ..PromptConfig::default()
        }),
        ..test_config()
    }
}

// ── 1. library skills reach the system prompt ────────────────────────────

#[tokio::test]
async fn a_library_skill_is_appended_to_the_system_prompt() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["review"]))])
        .skills([(
            "review",
            "Always review code carefully before marking done.",
        )])
        .build();
    h.templates.set_output("rendered-system-prompt");

    let call = dispatch(&h).await;
    let system = call.system_prompt.expect("a system prompt");
    assert!(system.contains("## Skills"), "{system}");
    assert!(system.contains("### review"), "{system}");
    assert!(
        system.contains("Always review code carefully before marking done."),
        "{system}"
    );
}

#[tokio::test]
async fn the_loader_is_called_with_the_agents_skill_list_in_order() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["careful", "review"]))])
        .skills([("careful", "Be careful."), ("review", "Review thoroughly.")])
        .build();

    dispatch(&h).await;
    assert_eq!(
        h.skills.calls.calls(),
        vec![vec!["careful".to_owned(), "review".to_owned()]]
    );
}

#[tokio::test]
async fn the_skill_block_is_separated_from_the_base_prompt_by_a_blank_line() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["careful"]))])
        .skills([("careful", "Be careful.")])
        .build();
    h.templates.set_output("SYSTEM_BASE");

    let call = dispatch(&h).await;
    let system = call.system_prompt.expect("a system prompt");
    assert!(system.starts_with("SYSTEM_BASE\n\n## Skills"), "{system}");
}

#[tokio::test]
async fn several_library_skills_are_joined_in_the_agents_order() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["review", "careful"]))])
        .skills([("careful", "B"), ("review", "A")])
        .build();
    h.templates.set_output("BASE");

    let call = dispatch(&h).await;
    assert_eq!(
        call.system_prompt.unwrap(),
        "BASE\n\n## Skills\n\n### review\n\nA\n\n### careful\n\nB"
    );
}

// ── 2. MCP skills are skipped ────────────────────────────────────────────

#[tokio::test]
async fn an_mcp_only_skill_list_appends_nothing() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec![
            "feature-dev:code-explorer",
            "server:memory",
        ]))])
        .with_skill_loader()
        .build();
    h.templates.set_output("SYSTEM_BASE");

    let call = dispatch(&h).await;
    assert_eq!(call.system_prompt.as_deref(), Some("SYSTEM_BASE"));
}

#[tokio::test]
async fn the_loader_still_receives_the_mcp_names_and_resolves_nothing() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["feature-dev:code-explorer"]))])
        .with_skill_loader()
        .build();

    dispatch(&h).await;
    assert_eq!(
        h.skills.calls.calls(),
        vec![vec!["feature-dev:code-explorer".to_owned()]]
    );
}

// ── 3. mixed lists ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_mixed_list_contributes_only_its_library_skills() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec![
            "review",
            "feature-dev:code-explorer",
        ]))])
        .skills([("review", "Review thoroughly.")])
        .build();
    h.templates.set_output("BASE");

    let call = dispatch(&h).await;
    let system = call.system_prompt.expect("a system prompt");
    assert!(system.contains("### review"), "{system}");
    assert!(!system.contains("feature-dev"), "{system}");
}

#[tokio::test]
async fn a_mixed_list_is_passed_to_the_loader_whole() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec![
            "review",
            "feature-dev:code-explorer",
            "careful",
        ]))])
        .skills([("review", "A"), ("careful", "B")])
        .build();

    dispatch(&h).await;
    assert_eq!(
        h.skills.calls.calls(),
        vec![vec![
            "review".to_owned(),
            "feature-dev:code-explorer".to_owned(),
            "careful".to_owned(),
        ]]
    );
}

// ── 4. no skills ─────────────────────────────────────────────────────────

#[tokio::test]
async fn an_empty_skill_list_leaves_the_system_prompt_untouched_and_calls_nothing() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec![]))])
        .with_skill_loader()
        .build();
    h.templates.set_output("SYSTEM_BASE");

    let call = dispatch(&h).await;
    assert_eq!(call.system_prompt.as_deref(), Some("SYSTEM_BASE"));
    // An empty list short-circuits before the loader is consulted.
    assert!(h.skills.calls.is_empty());
}

#[tokio::test]
async fn an_absent_skills_field_leaves_the_system_prompt_untouched() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(None)])
        .with_skill_loader()
        .build();
    h.templates.set_output("SYSTEM_BASE");

    let call = dispatch(&h).await;
    assert_eq!(call.system_prompt.as_deref(), Some("SYSTEM_BASE"));
    assert!(h.skills.calls.is_empty());
}

#[tokio::test]
async fn no_skill_loader_at_all_means_no_skill_lookup() {
    // `skill_loader` is optional in the deps, and the light container leaves
    // it out.
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["review"]))])
        .build();
    h.templates.set_output("SYSTEM_BASE");

    let call = dispatch(&h).await;
    assert_eq!(call.system_prompt.as_deref(), Some("SYSTEM_BASE"));
    assert!(h.skills.calls.is_empty());
}

// ── 5. legacy single-template mode ───────────────────────────────────────

#[tokio::test]
async fn the_legacy_combined_prompt_gets_the_skill_block_instead() {
    // With `prompt.template` set there is no system half to cache, so the
    // block has nowhere else to go.
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["review"]))])
        .skills([("review", "Review thoroughly.")])
        .config(config_with_legacy_template("legacy {{ task.title }}"))
        .build();
    h.templates.set_output("LEGACY_PROMPT");

    let call = dispatch(&h).await;
    assert_eq!(call.system_prompt, None);
    assert_eq!(
        call.prompt,
        "LEGACY_PROMPT\n\n## Skills\n\n### review\n\nReview thoroughly."
    );
}

#[tokio::test]
async fn the_legacy_prompt_keeps_its_own_content_before_the_block() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["careful"]))])
        .skills([("careful", "Be careful.")])
        .config(config_with_legacy_template("legacy"))
        .build();
    h.templates.set_output("ORIGINAL_LEGACY_CONTENT");

    let call = dispatch(&h).await;
    assert!(
        call.prompt
            .starts_with("ORIGINAL_LEGACY_CONTENT\n\n## Skills"),
        "{}",
        call.prompt
    );
}

#[tokio::test]
async fn an_all_mcp_list_appends_nothing_to_the_legacy_prompt_either() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(Some(vec!["feature-dev:code-explorer"]))])
        .with_skill_loader()
        .config(config_with_legacy_template("legacy"))
        .build();
    h.templates.set_output("LEGACY_PROMPT");

    let call = dispatch(&h).await;
    assert_eq!(call.prompt, "LEGACY_PROMPT");
    assert_eq!(call.system_prompt, None);
}

#[tokio::test]
async fn split_mode_renders_the_two_templates_separately() {
    // The default path: one render for the system half, one for the user half.
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_skills(None)])
        .build();

    let call = dispatch(&h).await;
    assert!(call.system_prompt.is_some());
    assert_eq!(h.templates.calls.len(), 2);
    let templates: Vec<String> = h
        .templates
        .calls
        .calls()
        .into_iter()
        .map(|call| call.template)
        .collect();
    assert_eq!(templates[0], orch_infra::template::DEFAULT_SYSTEM_TEMPLATE);
    assert_eq!(templates[1], orch_infra::template::DEFAULT_USER_TEMPLATE);
}

#[tokio::test]
async fn a_custom_split_template_pair_is_used_when_configured() {
    let mut config = test_config();
    config.prompt = Some(PromptConfig {
        template: None,
        system_template: Some("CUSTOM_SYSTEM".into()),
        user_template: Some("CUSTOM_USER".into()),
    });

    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_custom".into(),
            ..make_task()
        }])
        .agents([agent_with_skills(None)])
        .config(config)
        .build();

    dispatch(&h).await;
    let templates: Vec<String> = h
        .templates
        .calls
        .calls()
        .into_iter()
        .map(|call| call.template)
        .collect();
    assert_eq!(templates, vec!["CUSTOM_SYSTEM", "CUSTOM_USER"]);
}
