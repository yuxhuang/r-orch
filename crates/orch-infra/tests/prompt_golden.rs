//! V16 — the golden-prompt gate. Fixtures are generated from the TS engine by
//! `fixtures/prompts/gen/generate.mts`; Rust must reproduce them byte for byte.

use std::fs;
use std::path::PathBuf;

use indexmap::IndexMap;
use orch_core::template::{PromptContext, TemplateEngine};
use orch_infra::template::{
    ContextFilterInput, DEFAULT_SYSTEM_TEMPLATE, DEFAULT_USER_TEMPLATE, LiquidTemplateEngine,
    default_prompt_template, filter_relevant_context,
};
use serde::Deserialize;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/prompts")
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TemplateRef {
    Inline { inline: String },
    Named(String),
}

#[derive(Deserialize)]
struct RenderCase {
    name: String,
    template: TemplateRef,
    context: PromptContext,
    expected: String,
}

fn render_cases() -> Vec<RenderCase> {
    let mut cases: Vec<RenderCase> = fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && !p.file_name().unwrap().to_string_lossy().starts_with('_')
        })
        .map(|p| serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap())
        .collect();
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

#[tokio::test]
async fn every_golden_prompt_renders_byte_exact() {
    let engine = LiquidTemplateEngine::new();
    let cases = render_cases();
    assert!(cases.len() >= 80, "only {} fixtures found", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let template = match &case.template {
            TemplateRef::Inline { inline } => inline.clone(),
            TemplateRef::Named(n) if n == "system" => DEFAULT_SYSTEM_TEMPLATE.to_owned(),
            TemplateRef::Named(n) if n == "user" => DEFAULT_USER_TEMPLATE.to_owned(),
            TemplateRef::Named(n) if n == "legacy" => default_prompt_template(),
            TemplateRef::Named(n) => panic!("unknown template {n}"),
        };
        match engine.render(&template, &case.context).await {
            Ok(out) if out == case.expected => {}
            Ok(out) => failures.push(format!(
                "{}: output differs\n--- expected\n{:?}\n--- actual\n{:?}",
                case.name, case.expected, out
            )),
            Err(e) => failures.push(format!("{}: render error: {e}", case.name)),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} golden prompts differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}

#[derive(Deserialize)]
struct Templates {
    system: String,
    user: String,
    legacy: String,
}

#[test]
fn default_templates_match_the_ts_constants() {
    let t: Templates =
        serde_json::from_str(&fs::read_to_string(fixtures_dir().join("_templates.json")).unwrap())
            .unwrap();
    assert_eq!(DEFAULT_SYSTEM_TEMPLATE, t.system);
    assert_eq!(DEFAULT_USER_TEMPLATE, t.user);
    assert_eq!(default_prompt_template(), t.legacy);
}

#[derive(Deserialize)]
struct FilterFilter {
    #[serde(rename = "agentName")]
    agent_name: String,
    #[serde(rename = "agentRole")]
    agent_role: Option<String>,
    #[serde(rename = "goalId")]
    goal_id: Option<String>,
    #[serde(rename = "taskScope")]
    task_scope: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct FilterCase {
    name: String,
    entries: Vec<(String, String)>,
    filter: FilterFilter,
    expected: Vec<(String, String)>,
}

#[test]
fn filter_relevant_context_matches_ts() {
    let cases: Vec<FilterCase> =
        serde_json::from_str(&fs::read_to_string(fixtures_dir().join("_filter.json")).unwrap())
            .unwrap();
    assert!(cases.len() >= 20);
    for c in cases {
        let all: IndexMap<String, String> = c.entries.into_iter().collect();
        let got = filter_relevant_context(
            &all,
            &ContextFilterInput {
                agent_name: &c.filter.agent_name,
                agent_role: c.filter.agent_role.as_deref(),
                goal_id: c.filter.goal_id.as_deref(),
                task_scope: c.filter.task_scope.as_deref(),
            },
        );
        assert_eq!(got, c.expected, "filter case {}", c.name);
    }
}
