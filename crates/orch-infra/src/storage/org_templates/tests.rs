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
async fn full_root_update_repairs_missing_name_with_user_key() {
    assert_missing_root_identity_repaired("name", Some("a-team")).await;
}

#[tokio::test]
async fn full_root_update_repairs_missing_name_with_explicit_directory() {
    assert_missing_root_identity_repaired("name", None).await;
}

#[tokio::test]
async fn full_root_update_repairs_missing_schema_with_user_key() {
    assert_missing_root_identity_repaired("schema_version", Some("a-team")).await;
}

#[tokio::test]
async fn full_root_update_repairs_missing_schema_with_explicit_directory() {
    assert_missing_root_identity_repaired("schema_version", None).await;
}

async fn assert_missing_root_identity_repaired(field: &str, expected: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let path = root.join("org.yml");
    let replacement = std::fs::read(&path).unwrap();
    let mut old = serde_json::to_value(&bundle.org).unwrap();
    old.as_object_mut().unwrap().remove(field);
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let mut retained = RawSnapshot::capture(&root).unwrap().files;
    retained.remove("org.yml");
    let modified: Vec<_> = retained
        .keys()
        .map(|file| {
            std::fs::metadata(root.join(file))
                .unwrap()
                .modified()
                .unwrap()
        })
        .collect();

    mutate(
        &root,
        expected,
        Target::Org,
        Mutation::Replace(replacement.clone()),
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), replacement);
    let repaired = load_directory(&root, expected).await.unwrap();
    assert_eq!(repaired.org.name, "a-team");
    assert_eq!(repaired.org.schema_version, 1);
    let mut after = RawSnapshot::capture(&root).unwrap().files;
    after.remove("org.yml");
    assert_eq!(after, retained);
    for (file, before) in retained.keys().zip(modified) {
        assert_eq!(
            std::fs::metadata(root.join(file))
                .unwrap()
                .modified()
                .unwrap(),
            before,
            "{file} was rewritten"
        );
    }
}

#[tokio::test]
async fn missing_root_identity_does_not_allow_changing_present_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let path = root.join("org.yml");
    let replacement = std::fs::read(&path).unwrap();
    for (missing, present, value) in [
        ("name", "schema_version", serde_json::json!(2)),
        ("name", "schema_version", Value::Null),
        ("schema_version", "name", serde_json::json!("different")),
        ("schema_version", "name", Value::Null),
    ] {
        let mut old = serde_json::to_value(&bundle.org).unwrap();
        old.as_object_mut().unwrap().remove(missing);
        old[present] = value;
        let before = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &before).unwrap();
        let err = mutate(
            &root,
            None,
            Target::Org,
            Mutation::Replace(replacement.clone()),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("is immutable"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn missing_root_name_repair_still_requires_user_directory_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let path = root.join("org.yml");
    let mut old = serde_json::to_value(&bundle.org).unwrap();
    old.as_object_mut().unwrap().remove("name");
    let before = serde_json::to_vec(&old).unwrap();
    std::fs::write(&path, &before).unwrap();
    let mut replacement = bundle.org;
    replacement.name = "different".into();
    let err = mutate(
        &root,
        Some("a-team"),
        Target::Org,
        Mutation::Replace(serde_json::to_vec(&replacement).unwrap()),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("user directory key"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn missing_root_schema_repair_rejects_retained_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    let bundle = builtin("a-team").await.unwrap();
    init_bundle(&root, &bundle).await.unwrap();
    let path = root.join("org.yml");
    let replacement = std::fs::read(&path).unwrap();
    let mut old = serde_json::to_value(&bundle.org).unwrap();
    old.as_object_mut().unwrap().remove("schema_version");
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    std::fs::write(root.join("agents/backend-eng.yml"), "broken: [").unwrap();
    let before = RawSnapshot::capture(&root).unwrap().files;
    let err = mutate(&root, None, Target::Org, Mutation::Replace(replacement))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("agents/backend-eng.yml"));
    assert_eq!(RawSnapshot::capture(&root).unwrap().files, before);
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

#[test]
fn snapshot_ignores_dot_entries_before_inspecting_their_type() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("org.yml"), "name: team\n").unwrap();
    for member_dir in ["agents", "rules"] {
        let path = dir.path().join(member_dir);
        std::fs::create_dir_all(path.join(".worker.yml.0123abcd.tmp")).unwrap();
    }
    let snapshot = RawSnapshot::capture(dir.path()).unwrap();
    assert_eq!(
        snapshot
            .files
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["org.yml"]
    );
}

#[test]
fn snapshot_ignores_members_removed_after_directory_listing() {
    for (member_dir, name, ext) in [
        ("agents", "worker.yml", "yml"),
        ("rules", "shared.md", "md"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, "content").unwrap();
        let entry = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(capture_member(member_dir, ext, entry).unwrap().is_none());
    }
}

#[cfg(unix)]
fn write_root_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(
        root.join("org.yml"),
        "schema_version: 1\nname: victim\nlead: lead\n",
    )
    .unwrap();
    std::fs::write(
        root.join("agents/lead.yml"),
        "name: lead\nrole: Original instructions\n",
    )
    .unwrap();
}

#[cfg(unix)]
fn root_fixture_state(root: &Path) -> Vec<(Vec<u8>, std::time::SystemTime)> {
    ["", "agents", "org.yml", "agents/lead.yml"]
        .iter()
        .map(|name| {
            let path = root.join(name);
            let metadata = std::fs::metadata(&path).unwrap();
            let bytes = if metadata.is_file() {
                std::fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            (bytes, metadata.modified().unwrap())
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn selected_symlink_root_suffixes_reject_reads_without_changing_victim() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    write_root_fixture(&victim);
    let link = dir.path().join("root-link");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    let before = root_fixture_state(&victim);
    let mut results = Vec::new();
    for suffix in ["", "/", "/.", "//", "/./"] {
        let selected = PathBuf::from(format!("{}{suffix}", link.display()));
        let result = load_directory(&selected, None).await;
        results.push((
            suffix,
            result.is_err(),
            root_fixture_state(&victim) == before,
        ));
    }
    assert_eq!(
        results,
        vec![
            ("", true, true),
            ("/", true, true),
            ("/.", true, true),
            ("//", true, true),
            ("/./", true, true)
        ]
    );
    assert!(!victim.join(".template.lock").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn selected_symlink_root_suffixes_reject_mutations_without_changing_victim() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    write_root_fixture(&victim);
    let link = dir.path().join("root-link");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    let before = root_fixture_state(&victim);
    let mut results = Vec::new();
    for suffix in ["", "/", "/.", "//", "/./"] {
        let selected = PathBuf::from(format!("{}{suffix}", link.display()));
        let result = mutate(
            &selected,
            None,
            Target::Agent("lead".into()),
            Mutation::Replace(b"name: lead\nrole: Changed via symlink\n".to_vec()),
        )
        .await;
        results.push((
            suffix,
            result.is_err(),
            root_fixture_state(&victim) == before,
        ));
    }
    assert_eq!(
        results,
        vec![
            ("", true, true),
            ("/", true, true),
            ("/.", true, true),
            ("//", true, true),
            ("/./", true, true)
        ]
    );
    assert!(!victim.join(".template.lock").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn regular_root_suffixes_allow_reads_and_mutations_beneath_tmp_ancestor() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let root = dir.path().join("team");
    write_root_fixture(&root);
    for (index, suffix) in ["", "/", "/.", "//", "/./"].iter().enumerate() {
        let selected = PathBuf::from(format!("{}{suffix}", root.display()));
        assert_eq!(
            load_directory(&selected, None).await.unwrap().org.name,
            "victim"
        );
        let bytes = format!("name: lead\nrole: Revision {index}\n").into_bytes();
        mutate(
            &selected,
            None,
            Target::Agent("lead".into()),
            Mutation::Replace(bytes.clone()),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(root.join("agents/lead.yml")).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn root_guard_preserves_parent_aliases_and_parent_components() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("team");
    write_root_fixture(&root);
    std::fs::create_dir(root.join("child")).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(root.join("child"), &alias).unwrap();
    // Resolving alias/.. must reach team, not the lexical parent of alias.
    let selected = alias.join("..").join(".");
    assert_eq!(
        load_directory(&selected, None).await.unwrap().org.name,
        "victim"
    );
    mutate(
        &selected,
        None,
        Target::Agent("lead".into()),
        Mutation::Replace(b"name: lead\nrole: Via parent alias\n".to_vec()),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(root.join("agents/lead.yml")).unwrap(),
        b"name: lead\nrole: Via parent alias\n"
    );
}
