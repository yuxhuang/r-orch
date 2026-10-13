//! Port of `test/unit/domain/agent-shop-skills.test.ts`: the shipped library
//! is internally consistent and satisfies every shop template.

use std::fs;

use orch_core::skills::{SkillLoader, is_mcp_skill};
use orch_domain::AGENT_SHOP_TEMPLATES;
use orch_infra::skills::{FileSkillLoader, resolve_library_dir};

const FORBIDDEN: [&str; 3] = ["gstack-telemetry", "gstack-update-check", "LAKE_INTRO"];
const MAX_SKILL_SIZE: usize = 100 * 1024;

#[tokio::test]
async fn every_non_mcp_template_skill_exists_in_the_library() {
    let available = FileSkillLoader::new().list_available().await.unwrap();
    assert!(!available.is_empty());
    let missing: Vec<String> = AGENT_SHOP_TEMPLATES
        .iter()
        .flat_map(|t| t.skills.iter().map(move |s| (t.key, *s)))
        .filter(|(_, s)| !is_mcp_skill(s) && !available.iter().any(|a| a == s))
        .map(|(k, s)| format!("template={k} skill={s}"))
        .collect();
    assert!(missing.is_empty(), "missing library skills: {missing:?}");
}

#[test]
fn every_template_has_between_one_and_four_skills() {
    for t in AGENT_SHOP_TEMPLATES.iter() {
        assert!(!t.skills.is_empty(), "{} has no skills", t.key);
        assert!(t.skills.len() <= 4, "{} has {} skills", t.key, t.skills.len());
    }
}

#[tokio::test]
async fn each_skill_loads_and_is_small_and_clean() {
    let loader = FileSkillLoader::new();
    let available = loader.list_available().await.unwrap();
    assert_eq!(available.len(), 26);
    let dir = resolve_library_dir();
    for name in &available {
        let block = loader.load_skills(std::slice::from_ref(name)).await.unwrap();
        assert!(block.len() > 10, "{name} loaded empty");
        let content = fs::read_to_string(dir.join(format!("{name}.md"))).unwrap();
        assert!(content.len() < MAX_SKILL_SIZE, "{name} exceeds 100 KB");
        for fragment in FORBIDDEN {
            assert!(!content.contains(fragment), "{name} contains {fragment}");
        }
    }
}
