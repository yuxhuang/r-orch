use super::*;

async fn assert_parse_failure_reason(bytes: &[u8], reason: &str) {
    let path = Path::new("input.yml");
    let err = parse_document(path, bytes).await.unwrap_err();
    assert_eq!(err.path(), Some(path));
    assert_eq!(err.exit_code(), 1);
    assert!(std::error::Error::source(&err).is_some());
    let message = err.to_string();
    assert!(message.contains("input.yml"), "{message}");
    assert!(message.contains(reason), "{message}");
}

#[tokio::test]
async fn duplicate_yaml_keys_report_the_input_path_and_reason() {
    assert_parse_failure_reason(
        b"name: extra\nrole: x\nrole: y\n",
        "duplicate entry with key \"role\"",
    )
    .await;
}

#[tokio::test]
async fn duplicate_json_keys_report_the_input_path_and_reason() {
    assert_parse_failure_reason(
        br#"{"name":"extra","role":"x","role":"y"}"#,
        "duplicate entry with key \"role\"",
    )
    .await;
}

#[tokio::test]
async fn multiple_yaml_documents_report_the_input_path_and_reason() {
    assert_parse_failure_reason(
        b"name: extra\nrole: x\n---\nname: extra\nrole: y\n",
        "more than one document",
    )
    .await;
}

#[tokio::test]
async fn builtin_is_complete_self_contained_and_portable() {
    let bundle = builtin("a-team").await.unwrap();
    assert_eq!(bundle.agents.len(), 27);
    for agent in &bundle.agents {
        let role = bundle.render_role(agent).unwrap();
        assert!(!role.contains(".claude/agent-rules/"), "{}", agent.name);
        assert!(!role.contains("/Users/"));
        assert!(agent.model.is_none());
    }
    assert!(
        bundle
            .agents
            .iter()
            .find(|a| a.name == "sec-reviewer-codex")
            .unwrap()
            .role
            .contains("Default Codex model/effort")
    );
}

#[tokio::test]
async fn strict_documents_reject_hostile_shapes() {
    for bytes in [
        b"a: 1\na: 2\n".as_slice(),
        b"1: a",
        b"a: !tag x",
        b"a: .nan",
        b"a: 1\n---\nb: 2",
        b"{\"a\":1,\"a\":2}",
        b"a: null",
    ] {
        assert!(
            parse_document(std::path::Path::new("input.yml"), bytes)
                .await
                .is_err(),
            "{:?}",
            bytes
        );
    }
    let deep = format!("a: {}1{}", "[".repeat(65), "]".repeat(65));
    assert!(
        parse_document(std::path::Path::new("deep.yml"), deep.as_bytes())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invalid_candidates_never_publish_and_delete_can_repair() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    init_bundle(&root, &builtin("a-team").await.unwrap())
        .await
        .unwrap();
    let target = root.join("agents/backend-eng.yml");
    std::fs::write(&target, "not: [valid").unwrap();
    let before = std::fs::read(&target).unwrap();
    assert!(
        mutate(
            &root,
            None,
            Target::Agent("backend-eng".into()),
            Mutation::Replace(b"name: wrong\nrole: valid".to_vec())
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&target).unwrap(), before);
    mutate(
        &root,
        None,
        Target::Agent("backend-eng".into()),
        Mutation::Delete,
    )
    .await
    .unwrap();
    assert_eq!(load_directory(&root, None).await.unwrap().agents.len(), 26);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_and_yaml_members_are_never_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    init_bundle(&root, &builtin("a-team").await.unwrap())
        .await
        .unwrap();
    std::os::unix::fs::symlink(root.join("org.yml"), root.join("agents/escape.yml")).unwrap();
    assert!(load_directory(&root, None).await.is_err());
    std::fs::remove_file(root.join("agents/escape.yml")).unwrap();
    std::fs::write(
        root.join("agents/hidden.yaml"),
        "name: hidden\nrole: hidden",
    )
    .unwrap();
    assert!(
        load_directory(&root, None)
            .await
            .unwrap_err()
            .to_string()
            .contains(".yml")
    );
}

#[tokio::test]
async fn overlay_repairs_only_the_selected_bounded_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    init_bundle(&root, &builtin("a-team").await.unwrap())
        .await
        .unwrap();
    let path = root.join("agents/backend-eng.yml");
    std::fs::write(&path, "role: [broken").unwrap();
    let replacement = b"name: backend-eng\nrole: Backend developer\n".to_vec();
    mutate(
        &root,
        None,
        Target::Agent("backend-eng".into()),
        Mutation::Replace(replacement.clone()),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), replacement);
    std::fs::write(root.join("agents/backend-eng-2.yml"), "broken: [").unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(
        mutate(
            &root,
            None,
            Target::Agent("backend-eng".into()),
            Mutation::Replace(b"name: backend-eng\nrole: Changed".to_vec())
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn root_repair_preserves_parseable_identity_and_schema() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let root_bytes = std::fs::read(root.join("org.yml")).unwrap();
    std::fs::write(root.join("org.yml"), "broken: [").unwrap();
    mutate(
        &root,
        Some("a-team"),
        Target::Org,
        Mutation::Replace(root_bytes.clone()),
    )
    .await
    .unwrap();
    let mut wrong = bundle.org.clone();
    wrong.name = "different".into();
    assert!(
        mutate(
            &root,
            None,
            Target::Org,
            Mutation::Replace(serde_json::to_vec(&wrong).unwrap())
        )
        .await
        .is_err()
    );
    wrong = bundle.org;
    wrong.schema_version = 2;
    std::fs::write(root.join("org.yml"), serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(
        mutate(&root, None, Target::Org, Mutation::Replace(root_bytes))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn delete_refuses_lead_and_init_never_overwrites_an_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let before = std::fs::read(root.join("agents/orchestrator.yml")).unwrap();
    assert!(
        mutate(
            &root,
            None,
            Target::Agent("orchestrator".into()),
            Mutation::Delete
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read(root.join("agents/orchestrator.yml")).unwrap(),
        before
    );
    let empty = dir.path().join("existing");
    std::fs::create_dir(&empty).unwrap();
    assert!(init_bundle(&empty, &bundle).await.is_err());
    assert!(std::fs::read_dir(empty).unwrap().next().is_none());
}

#[tokio::test]
async fn snapshot_bounds_old_targets_even_when_deleting_or_replacing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    init_bundle(&root, &builtin("a-team").await.unwrap())
        .await
        .unwrap();
    let target = root.join("agents/backend-eng.yml");
    std::fs::write(&target, vec![b'x'; MAX_DOCUMENT_BYTES + 1]).unwrap();
    assert!(
        mutate(
            &root,
            None,
            Target::Agent("backend-eng".into()),
            Mutation::Delete
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::metadata(target).unwrap().len(),
        (MAX_DOCUMENT_BYTES + 1) as u64
    );
}

#[tokio::test]
async fn all_legacy_presets_have_owned_views_without_mutating_the_catalogue() {
    for legacy in &orch_domain::ORG_TEMPLATES {
        let view = builtin(legacy.key).await.unwrap();
        assert_eq!(view.agents.len(), legacy.agents.len());
    }
}

#[tokio::test(start_paused = true)]
async fn lock_wait_is_bounded_and_drop_releases_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    init_bundle(&root, &builtin("a-team").await.unwrap())
        .await
        .unwrap();
    let held = LockedTemplate::open(&root, None).await.unwrap();
    let error = LockedTemplate::open(&root, None)
        .await
        .err()
        .expect("contended lock");
    assert!(error.to_string().contains("5 s"));
    drop(held);
    assert!(LockedTemplate::open(&root, None).await.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn broken_optional_rule_directory_symlinks_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let mut bundle = builtin("a-team").await.unwrap();
    bundle.agents.retain(|a| a.name.starts_with("backend-eng"));
    bundle.org.lead = "backend-eng".into();
    bundle.org.rules.clear();
    bundle.rules.clear();
    init_bundle(&root, &bundle).await.unwrap();
    std::fs::remove_dir(root.join("rules")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("missing"), root.join("rules")).unwrap();
    assert!(load_directory(&root, None).await.is_err());
}

#[tokio::test]
async fn parseable_root_with_null_cannot_bypass_identity_or_schema_immutability() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let replacement = std::fs::read(root.join("org.yml")).unwrap();
    let mut old = serde_json::to_value(&bundle.org).unwrap();
    old["schema_version"] = serde_json::json!(2);
    old["description"] = serde_json::Value::Null;
    let old_bytes = serde_json::to_vec(&old).unwrap();
    std::fs::write(root.join("org.yml"), &old_bytes).unwrap();
    assert!(
        mutate(&root, None, Target::Org, Mutation::Replace(replacement))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(root.join("org.yml")).unwrap(), old_bytes);
}

#[tokio::test]
async fn legacy_owned_views_preserve_each_declared_approval_policy() {
    for legacy in &orch_domain::ORG_TEMPLATES {
        let bundle = builtin(legacy.key).await.unwrap();
        for entry in legacy.agents {
            let shop = orch_domain::get_shop_template_by_key(entry.shop_key).unwrap();
            let name = entry.name.to_ascii_lowercase().replace(' ', "-");
            assert_eq!(
                bundle
                    .agents
                    .iter()
                    .find(|a| a.name == name)
                    .unwrap()
                    .approval_policy,
                Some(shop.approval_policy)
            );
        }
    }
}

#[tokio::test]
async fn equivalent_replacement_preserves_authored_bytes_and_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    init_bundle(&root, &builtin("a-team").await.unwrap())
        .await
        .unwrap();
    let path = root.join("agents/backend-eng.yml");
    let bytes = b"# Keep this comment\nname: backend-eng\nrole: Backend developer\n";
    std::fs::write(&path, bytes).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    mutate(
        &root,
        None,
        Target::Agent("backend-eng".into()),
        Mutation::Replace(br#"{"name":"backend-eng","role":"Backend developer"}"#.to_vec()),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}
