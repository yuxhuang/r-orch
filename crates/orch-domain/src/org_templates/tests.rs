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
