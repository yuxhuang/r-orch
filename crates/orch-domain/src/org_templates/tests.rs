use super::*;
use serde_json::json;

#[test]
fn identifiers_are_portable_and_never_normalized() {
    for name in [
        "../x", "a/b", "Con", "con", "com1", "a--b", "é", "-x", "a.b", "",
    ] {
        assert!(validate_id(name).is_err(), "{name}");
    }
    assert!(validate_id("a-team-2").is_ok());
}

#[test]
fn authoring_preserves_empty_and_omitted_and_rejects_null_and_unknown() {
    let base = json!({"name":"lead", "role":"Lead"});
    let absent: AgentDefinition = serde_json::from_value(base.clone()).unwrap();
    assert!(
        serde_json::to_value(absent)
            .unwrap()
            .get("skills")
            .is_none()
    );
    let mut empty = base.clone();
    empty["skills"] = json!([]);
    let present: AgentDefinition = serde_json::from_value(empty).unwrap();
    assert_eq!(serde_json::to_value(present).unwrap()["skills"], json!([]));
    for (key, value) in [("skills", json!(null)), ("typo", json!(null))] {
        let mut bad = base.clone();
        bad[key] = value;
        assert!(serde_json::from_value::<AgentDefinition>(bad).is_err());
    }
}

#[test]
fn rule_closure_is_ordered_cyclic_and_has_no_file_lookups() {
    let org: OrgDefinition = serde_json::from_value(
        json!({"schema_version":1,"name":"sample","lead":"lead","rules":["first","second"]}),
    )
    .unwrap();
    let agent: AgentDefinition = serde_json::from_value(json!({"name":"lead","role":"Rules (relative to your workspace root): .claude/agent-rules/second.md."})).unwrap();
    let bundle = Bundle {
        org,
        agents: vec![agent],
        rules: [
            ("first".into(), "First .claude/agent-rules/second.md".into()),
            (
                "second".into(),
                "Second .claude/agent-rules/first.md".into(),
            ),
        ]
        .into(),
    };
    bundle.validate().unwrap();
    let role = bundle.render_role(&bundle.agents[0]).unwrap();
    assert!(!role.contains(".claude/agent-rules/"));
    assert_eq!(role.matches("## Shared rule: first").count(), 1);
    assert!(
        role.find("## Shared rule: first").unwrap() < role.find("## Shared rule: second").unwrap()
    );
}

#[test]
fn shorthand_and_unknown_references_fail_even_in_unselected_rules() {
    let registry = vec!["seniority".into()];
    for text in [
        "seniority.md",
        "`seniority.md`.",
        ".claude/agent-rules/missing.md",
        "x/.claude/agent-rules/seniority.md",
    ] {
        assert!(rule_references(text, &registry).is_err(), "{text}");
    }
    assert!(rule_references("README.md", &registry).unwrap().is_empty());
}

fn bundle_with_role(role: &str) -> Bundle {
    Bundle {
        org: serde_json::from_value(json!({"schema_version":1,"name":"sample","lead":"lead"}))
            .unwrap(),
        agents: vec![serde_json::from_value(json!({"name":"lead","role":role})).unwrap()],
        rules: BTreeMap::new(),
    }
}

#[test]
fn empty_rule_registry_preserves_authored_role_bytes_including_header_and_whitespace() {
    let authored = " \nRules (relative to your workspace root): README.md.\r\nKeep café 🦀.\t\n\n";
    let mut bundle = bundle_with_role(authored);
    for name in ["sample", "a-team"] {
        bundle.org.name = name.into();
        bundle.validate().unwrap();
        assert_eq!(bundle.render_role(&bundle.agents[0]).unwrap(), authored);
    }
}

#[test]
fn registered_rules_selected_by_another_agent_do_not_rewrite_a_rule_free_role() {
    let authored = "Rules (relative to your workspace root): keep this header.  \n";
    let mut bundle = bundle_with_role(authored);
    bundle.org.rules = vec!["first".into()];
    bundle.rules.insert("first".into(), "First rule".into());
    bundle.agents.push(
        serde_json::from_value(json!({"name":"other","role":"Other","rules":["first"]})).unwrap(),
    );
    for name in ["sample", "a-team"] {
        bundle.org.name = name.into();
        bundle.validate().unwrap();
        assert_eq!(bundle.render_role(&bundle.agents[0]).unwrap(), authored);
    }
}

#[test]
fn explicit_rules_in_other_orgs_get_only_the_generic_explanation_once() {
    let mut bundle = bundle_with_role("Keep authored Pensieve instructions.");
    bundle.org.rules = vec!["first".into(), "unused".into()];
    bundle.rules = [
        ("first".into(), "First rule".into()),
        ("unused".into(), "Unused rule".into()),
    ]
    .into();
    bundle.agents[0].rules = vec!["first".into(), "first".into()];
    bundle.validate().unwrap();
    for name in ["sample", "a-team-copy"] {
        bundle.org.name = name.into();
        assert_eq!(
            bundle.render_role(&bundle.agents[0]).unwrap(),
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required.\n\nKeep authored Pensieve instructions.\n\n## Shared rule: first\n\nFirst rule"
        );
    }
}

#[test]
fn inferred_rules_include_the_ordered_transitive_cycle_once_without_a_team_context() {
    let mut bundle = bundle_with_role(
        "Rules (relative to your workspace root): `.claude/agent-rules/second.md`.",
    );
    bundle.org.rules = vec!["first".into(), "second".into(), "unused".into()];
    bundle.rules = [
        ("first".into(), "First .claude/agent-rules/second.md".into()),
        (
            "second".into(),
            "Second .claude/agent-rules/first.md".into(),
        ),
        ("unused".into(), "Unused rule".into()),
    ]
    .into();
    bundle.validate().unwrap();
    assert_eq!(
        bundle.render_role(&bundle.agents[0]).unwrap(),
        "Embedded shared-rule sections are authoritative; no rule-file lookup is required.\n\nRules (embedded in this role): `embedded rule «second»`.\n\n## Shared rule: first\n\nFirst embedded rule «second»\n\n## Shared rule: second\n\nSecond embedded rule «first»"
    );
}

fn a_team_bundle() -> Bundle {
    let mut bundle = bundle_with_role("placeholder");
    bundle.agents.clear();
    for (path, text) in builtin::A_TEAM {
        if *path == "org.yml" {
            bundle.org = serde_yaml_ng::from_str(text).unwrap();
        } else if path.starts_with("agents/") {
            bundle.agents.push(serde_yaml_ng::from_str(text).unwrap());
        } else if let Some(id) = path
            .strip_prefix("rules/")
            .and_then(|p| p.strip_suffix(".md"))
        {
            bundle.rules.insert(id.into(), (*text).into());
        }
    }
    bundle.validate().unwrap();
    bundle
}

const WORKSPACE_ROOT_CONTEXT: &str = " <workspace root> means the owning orch project root (the directory containing .orchestry), not the per-task checkout. Establish it from task/context before using destination placeholders. If unavailable, report it to the lead instead of guessing from cwd. Sibling ../pensieve destinations and named skills are deployment prerequisites; edit the template for other layouts.";

#[test]
fn renamed_a_team_members_using_workspace_root_keep_its_definition() {
    let mut bundle = a_team_bundle();
    bundle.org.name = "my-team".into();
    bundle.validate().unwrap();
    for name in ["orchestrator", "tech-writer"] {
        let agent = bundle.agents.iter().find(|a| a.name == name).unwrap();
        assert!(agent.role.contains("<workspace root>/../pensieve/"));
        let rendered = bundle.render_role(agent).unwrap();
        assert!(rendered.starts_with(&format!(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required.{WORKSPACE_ROOT_CONTEXT}\n\n"
        )), "{name}");
        assert!(rendered.contains("<workspace root>/../pensieve/"));
    }
}

#[test]
fn authored_workspace_root_placeholder_defines_context_with_selected_rules() {
    let mut bundle = bundle_with_role("Write <workspace root>/reports.");
    bundle.org.rules = vec!["first".into()];
    bundle.rules.insert("first".into(), "First rule".into());
    bundle.agents[0].rules = vec!["first".into()];
    bundle.validate().unwrap();
    assert_eq!(
        bundle.render_role(&bundle.agents[0]).unwrap(),
        format!(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required.{WORKSPACE_ROOT_CONTEXT}\n\nWrite <workspace root>/reports.\n\n## Shared rule: first\n\nFirst rule"
        )
    );
}

#[test]
fn selected_rule_workspace_root_placeholder_defines_context() {
    let mut bundle = bundle_with_role("Lead");
    bundle.org.rules = vec!["first".into()];
    bundle
        .rules
        .insert("first".into(), "Write <workspace root>/reports.".into());
    bundle.agents[0].rules = vec!["first".into()];
    bundle.validate().unwrap();
    assert_eq!(
        bundle.render_role(&bundle.agents[0]).unwrap(),
        format!(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required.{WORKSPACE_ROOT_CONTEXT}\n\nLead\n\n## Shared rule: first\n\nWrite <workspace root>/reports."
        )
    );
}

#[test]
fn transitively_selected_workspace_root_placeholder_defines_context() {
    let mut bundle = bundle_with_role("Read .claude/agent-rules/second.md.");
    bundle.org.rules = vec!["first".into(), "second".into()];
    bundle.rules = [
        (
            "first".into(),
            "Write <workspace root>/reports. Read .claude/agent-rules/second.md.".into(),
        ),
        ("second".into(), "Read .claude/agent-rules/first.md.".into()),
    ]
    .into();
    bundle.validate().unwrap();
    assert_eq!(
        bundle.render_role(&bundle.agents[0]).unwrap(),
        format!(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required.{WORKSPACE_ROOT_CONTEXT}\n\nRead embedded rule «second».\n\n## Shared rule: first\n\nWrite <workspace root>/reports. Read embedded rule «second».\n\n## Shared rule: second\n\nRead embedded rule «first»."
        )
    );
}

#[test]
fn unselected_workspace_root_placeholder_keeps_only_generic_context() {
    let mut bundle = bundle_with_role("Lead");
    bundle.org.rules = vec!["first".into(), "unused".into()];
    bundle.rules = [
        ("first".into(), "First rule".into()),
        ("unused".into(), "Write <workspace root>/reports.".into()),
    ]
    .into();
    bundle.agents[0].rules = vec!["first".into()];
    bundle.validate().unwrap();
    assert_eq!(
        bundle.render_role(&bundle.agents[0]).unwrap(),
        "Embedded shared-rule sections are authoritative; no rule-file lookup is required.\n\nLead\n\n## Shared rule: first\n\nFirst rule"
    );
}

#[test]
fn empty_closure_workspace_root_placeholder_preserves_authored_bytes() {
    let authored = " \nRules (relative to your workspace root): <workspace root>/reports.\r\nKeep café 🦀.\t\n\n";
    for name in ["sample", "a-team", "my-team"] {
        let mut bundle = bundle_with_role(authored);
        bundle.org.name = name.into();
        bundle.validate().unwrap();
        assert_eq!(bundle.render_role(&bundle.agents[0]).unwrap(), authored);
        bundle.org.rules = vec!["first".into()];
        bundle
            .rules
            .insert("first".into(), "Write <workspace root>/reports.".into());
        bundle.agents.push(
            serde_json::from_value(json!({"name":"other","role":"Other","rules":["first"]}))
                .unwrap(),
        );
        bundle.validate().unwrap();
        assert_eq!(bundle.render_role(&bundle.agents[0]).unwrap(), authored);
    }
}

#[test]
fn a_team_members_with_rules_keep_their_frozen_ptt_role_bytes() {
    let bundle = a_team_bundle();
    // Frozen PTT preamble and fixture-only literal substitution form an oracle
    // independent of render_role and its token-rewriting helper. These members
    // declare their complete closure explicitly in the bundled assets.
    let rewrite_fixture = |text: &str| {
        let mut text = text.replace(
            "Rules (relative to your workspace root):",
            "Rules (embedded in this role):",
        );
        for id in &bundle.org.rules {
            text = text.replace(
                &format!(".claude/agent-rules/{id}.md"),
                &format!("embedded rule «{id}»"),
            );
        }
        text
    };
    let mut checked = 0;
    for agent in bundle.agents.iter().filter(|a| !a.rules.is_empty()) {
        let mut expected = String::from(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required. <workspace root> means the owning orch project root (the directory containing .orchestry), not the per-task checkout. Establish it from task/context before using destination placeholders. If unavailable, report it to the lead instead of guessing from cwd. Sibling ../pensieve destinations and named skills are deployment prerequisites; edit the template for other layouts.\n\n",
        );
        expected.push_str(&rewrite_fixture(&agent.role));
        for id in bundle
            .org
            .rules
            .iter()
            .filter(|id| agent.rules.contains(id))
        {
            expected.push_str(&format!("\n\n## Shared rule: {id}\n\n"));
            expected.push_str(&rewrite_fixture(&bundle.rules[id]));
        }
        assert_eq!(
            bundle.render_role(agent).unwrap(),
            expected,
            "{}",
            agent.name
        );
        checked += 1;
    }
    assert_eq!(checked, 25);
}

#[test]
fn both_a_team_backend_stubs_resolve_to_the_exact_authored_role() {
    let bundle = a_team_bundle();
    for name in ["backend-eng", "backend-eng-2"] {
        let agent = bundle.agents.iter().find(|a| a.name == name).unwrap();
        assert_eq!(bundle.render_role(agent).unwrap(), "Backend developer");
    }
}

#[test]
fn a_team_canonical_references_infer_the_same_roles_as_explicit_selections() {
    let bundle = a_team_bundle();
    for agent in &bundle.agents {
        let mut inferred = agent.clone();
        inferred.rules.clear();
        assert_eq!(
            bundle.render_role(&inferred).unwrap(),
            bundle.render_role(agent).unwrap(),
            "{}",
            agent.name
        );
    }
}

#[test]
fn legacy_preset_roles_keep_their_authored_catalogue_bytes() {
    for preset in &crate::ORG_TEMPLATES {
        for entry in preset.agents {
            let shop = crate::get_shop_template_by_key(entry.shop_key).unwrap();
            let mut bundle = bundle_with_role(shop.role);
            bundle.org.name = preset.key.into();
            bundle.validate().unwrap();
            assert_eq!(bundle.render_role(&bundle.agents[0]).unwrap(), shop.role);
        }
    }
}
