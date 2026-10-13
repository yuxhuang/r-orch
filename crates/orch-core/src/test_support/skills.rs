//! A skill loader backed by an in-memory library.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::error::Result;
use crate::skills::{SkillLoader, format_skills_block, is_mcp_skill, is_valid_skill_name};
use crate::test_support::faults::Faults;
use crate::test_support::recorder::Recorder;

/// Resolves skills from a map instead of the filesystem, but applies the real
/// rules: MCP names are skipped, invalid names resolve to nothing, a missing
/// skill is skipped rather than an error, and the output goes through
/// [`format_skills_block`].
#[derive(Debug, Clone, Default)]
pub struct FakeSkillLoader {
    library: Arc<Mutex<IndexMap<String, String>>>,
    pub calls: Recorder<Vec<String>>,
    pub faults: Faults,
}

impl FakeSkillLoader {
    /// An empty library: every `load_skills` resolves to `""`.
    pub fn new() -> Self {
        FakeSkillLoader::default()
    }

    pub fn with_skills<K, V>(skills: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let loader = FakeSkillLoader::new();
        for (name, content) in skills {
            loader.insert(name, content);
        }
        loader
    }

    pub fn insert(&self, name: impl Into<String>, content: impl Into<String>) {
        self.library
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.into(), content.into());
    }
}

#[async_trait]
impl SkillLoader for FakeSkillLoader {
    async fn load_skills(&self, skill_names: &[String]) -> Result<String> {
        self.calls.record(skill_names.to_vec());
        self.faults.check()?;

        let library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        let resolved: Vec<(String, String)> = skill_names
            .iter()
            .filter(|name| !is_mcp_skill(name) && is_valid_skill_name(name))
            .filter_map(|name| library.get(name).map(|c| (name.clone(), c.clone())))
            .collect();
        Ok(format_skills_block(&resolved))
    }

    async fn list_available(&self) -> Result<Vec<String>> {
        self.faults.check()?;
        let mut names: Vec<String> = self
            .library
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[tokio::test]
    async fn an_empty_library_resolves_to_nothing() {
        let loader = FakeSkillLoader::new();
        assert_eq!(loader.load_skills(&names(&["review"])).await.unwrap(), "");
        assert!(loader.list_available().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn resolves_in_the_requested_order_not_the_library_order() {
        let loader = FakeSkillLoader::with_skills([("review", "A"), ("careful", "B")]);
        assert_eq!(
            loader
                .load_skills(&names(&["careful", "review"]))
                .await
                .unwrap(),
            "## Skills\n\n### careful\n\nB\n\n### review\n\nA"
        );
    }

    #[tokio::test]
    async fn skips_mcp_names_silently() {
        let loader = FakeSkillLoader::with_skills([("review", "A")]);
        assert_eq!(
            loader
                .load_skills(&names(&["feature-dev:code-explorer", "review"]))
                .await
                .unwrap(),
            "## Skills\n\n### review\n\nA"
        );
        // Only MCP names → nothing at all.
        assert_eq!(
            loader
                .load_skills(&names(&["feature-dev:code-explorer"]))
                .await
                .unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn skips_missing_and_invalid_names_without_failing() {
        let loader = FakeSkillLoader::with_skills([("review", "A")]);
        assert_eq!(
            loader
                .load_skills(&names(&["nope", "../etc/passwd", "review"]))
                .await
                .unwrap(),
            "## Skills\n\n### review\n\nA"
        );
    }

    #[tokio::test]
    async fn lists_available_names_sorted() {
        let loader = FakeSkillLoader::with_skills([("zeta", "z"), ("alpha", "a")]);
        assert_eq!(
            loader.list_available().await.unwrap(),
            vec!["alpha".to_owned(), "zeta".to_owned()]
        );
    }

    #[tokio::test]
    async fn records_the_requested_names() {
        let loader = FakeSkillLoader::new();
        loader.load_skills(&names(&["review", "qa"])).await.unwrap();
        assert_eq!(loader.calls.first(), Some(names(&["review", "qa"])));
    }

    #[tokio::test]
    async fn faults_fail_the_load() {
        let loader = FakeSkillLoader::new();
        loader.faults.fail_once("library unreadable");
        assert!(loader.load_skills(&names(&["review"])).await.is_err());
        assert!(loader.load_skills(&names(&["review"])).await.is_ok());
    }

    #[tokio::test]
    async fn works_as_a_trait_object() {
        let loader: Arc<dyn SkillLoader> = Arc::new(FakeSkillLoader::with_skills([("qa", "Q")]));
        assert_eq!(
            loader.load_skills(&names(&["qa"])).await.unwrap(),
            "## Skills\n\n### qa\n\nQ"
        );
    }
}
