//! Skill library port (the interface only).

use async_trait::async_trait;

use crate::error::Result;

#[async_trait]
pub trait SkillLoader: Send + Sync {
    /// Load and format library skill content for the given names.
    ///
    /// MCP skills (those containing `:`) are silently skipped — the Claude CLI
    /// handles them natively. Returns `## Skills\n\n` followed by one
    /// `### <name>\n\n<content>` section per resolved skill, joined by
    /// `\n\n`, or an empty string when nothing resolved.
    async fn load_skills(&self, skill_names: &[String]) -> Result<String>;

    /// Every available library skill name, sorted ascending.
    async fn list_available(&self) -> Result<Vec<String>>;
}

/// Valid library skill name: lowercase alphanumerics and hyphens only. This is
/// the path-traversal defence — a name that fails it is not looked up at all.
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether a skill name refers to an MCP skill, which the Claude CLI handles
/// natively and the loader skips.
pub fn is_mcp_skill(name: &str) -> bool {
    name.contains(':')
}

/// Assemble the skill block that gets appended to the system prompt.
///
/// `resolved` carries one `(name, content)` pair per skill that actually
/// loaded, **in `agent.config.skills` order**. Returns `""` for an empty
/// input, so the caller appends nothing.
///
/// Shared by the real loader and the test fake so there is exactly one
/// implementation of a contract format.
pub fn format_skills_block(resolved: &[(String, String)]) -> String {
    if resolved.is_empty() {
        return String::new();
    }
    let sections: Vec<String> = resolved
        .iter()
        .map(|(name, content)| format!("### {name}\n\n{content}"))
        .collect();
    format!("## Skills\n\n{}", sections.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_library_skill_names() {
        assert!(is_valid_skill_name("review"));
        assert!(is_valid_skill_name("plan-eng-review"));
        assert!(is_valid_skill_name("qa-only"));
        assert!(is_valid_skill_name("a1"));
    }

    #[test]
    fn mcp_skills_are_the_colon_separated_ones() {
        assert!(is_mcp_skill("feature-dev:code-explorer"));
        assert!(!is_mcp_skill("review"));
    }

    #[test]
    fn an_empty_resolution_formats_to_nothing() {
        assert_eq!(format_skills_block(&[]), "");
    }

    #[test]
    fn one_skill_formats_with_a_single_heading() {
        let resolved = vec![("review".to_owned(), "# Review\n\nsteps".to_owned())];
        assert_eq!(
            format_skills_block(&resolved),
            "## Skills\n\n### review\n\n# Review\n\nsteps"
        );
    }

    #[test]
    fn several_skills_are_joined_by_a_blank_line_in_order() {
        let resolved = vec![
            ("review".to_owned(), "A".to_owned()),
            ("careful".to_owned(), "B".to_owned()),
        ];
        assert_eq!(
            format_skills_block(&resolved),
            "## Skills\n\n### review\n\nA\n\n### careful\n\nB"
        );
    }

    #[test]
    fn content_is_injected_raw_including_frontmatter() {
        let resolved = vec![(
            "qa".to_owned(),
            "---\nname: qa\nversion: 1\n---\n\nbody".to_owned(),
        )];
        assert!(format_skills_block(&resolved).contains("---\nname: qa\nversion: 1\n---"));
    }

    #[test]
    fn rejects_anything_that_could_escape_the_library_directory() {
        assert!(!is_valid_skill_name(""));
        assert!(!is_valid_skill_name(".."));
        assert!(!is_valid_skill_name("../etc/passwd"));
        assert!(!is_valid_skill_name("a/b"));
        assert!(!is_valid_skill_name("Review"));
        assert!(!is_valid_skill_name("feature-dev:code-explorer"));
        assert!(!is_valid_skill_name("a_b"));
        assert!(!is_valid_skill_name("a.md"));
    }
}
