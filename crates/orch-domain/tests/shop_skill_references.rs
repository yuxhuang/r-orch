//! Reference integrity of the shop templates against the shipped skill
//! library on disk.
//!
//! Deferred during the first pass of WP-0 because `skills/library/` did not
//! exist yet; it does now, so the reference-integrity check is live. The
//! loader itself is WP-3's — this only needs the directory listing, so it
//! reads it directly rather than waiting for `SkillLoader`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use orch_domain::AGENT_SHOP_TEMPLATES;

/// `skills/library/` relative to this crate, which is where the binary ships
/// it from in a debug build.
fn library_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/library")
}

fn available_skills() -> BTreeSet<String> {
    let dir = library_dir();
    std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", dir.display()))
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|name| name.strip_suffix(".md").map(str::to_owned))
        .collect()
}

#[test]
fn the_library_ships_26_skills() {
    assert_eq!(available_skills().len(), 26);
}

#[test]
fn every_non_mcp_skill_referenced_by_a_template_exists_in_the_library() {
    let available = available_skills();
    let missing: Vec<(&str, &str)> = AGENT_SHOP_TEMPLATES
        .iter()
        .flat_map(|t| {
            t.skills
                .iter()
                .filter(|s| !s.contains(':'))
                .filter(|s| !available.contains(**s))
                .map(move |s| (t.key, *s))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "missing library skills: {}",
        missing
            .iter()
            .map(|(t, s)| format!("template={t} skill={s}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
fn the_agent_creator_role_only_advertises_skills_that_exist() {
    // The role prompt lists the library skills by name in a Markdown table. A
    // name drifting out of the library would make the bootstrap agent
    // recommend a skill that silently resolves to nothing.
    let available = available_skills();
    let advertised = [
        "review",
        "qa",
        "qa-only",
        "investigate",
        "plan-ceo-review",
        "plan-eng-review",
        "plan-design-review",
        "autoplan",
        "office-hours",
        "design-consultation",
        "design-review",
        "ship",
        "land-and-deploy",
        "canary",
        "document-release",
        "browse",
        "benchmark",
        "setup-deploy",
        "setup-browser-cookies",
        "careful",
        "freeze",
        "unfreeze",
        "guard",
        "codex",
        "upgrade",
        "retro",
    ];
    let missing: Vec<&str> = advertised
        .iter()
        .copied()
        .filter(|s| !available.contains(*s))
        .collect();
    assert!(missing.is_empty(), "advertised but absent: {missing:?}");
    // The table is the whole library, so the two sets coincide.
    assert_eq!(advertised.len(), available.len());
    for name in advertised {
        assert!(
            orch_domain::AGENT_CREATOR_ROLE.contains(name),
            "{name} is in the library but not advertised in the role prompt"
        );
    }
}

#[test]
fn every_library_skill_is_non_empty_and_under_100_kb() {
    for name in available_skills() {
        let path = library_dir().join(format!("{name}.md"));
        let content = std::fs::read_to_string(&path).expect("readable");
        assert!(content.len() > 10, "{name} is unexpectedly short");
        assert!(
            content.len() < 100 * 1024,
            "{name} exceeds 100 KB ({} bytes)",
            content.len()
        );
    }
}

#[test]
fn no_skill_file_contains_gstack_remnants() {
    const FORBIDDEN: [&str; 3] = ["gstack-telemetry", "gstack-update-check", "LAKE_INTRO"];
    for name in available_skills() {
        let content =
            std::fs::read_to_string(library_dir().join(format!("{name}.md"))).expect("readable");
        for fragment in FORBIDDEN {
            assert!(!content.contains(fragment), "{name} contains {fragment}");
        }
    }
}
